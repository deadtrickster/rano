//! Reading a file without blocking the event loop. Chunks, not a buffer, so
//! a first screenful costs one 64 KiB read; `poll` never waits; adoption is
//! budgeted, or a fast worker becomes the stall it was meant to fix.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::Duration;

/// Read granularity: one page-cache-friendly read.
const CHUNK: usize = 64 * 1024;

/// Rows per message — a hard cap, which is what makes the adoption budget
/// exact.
const BATCH: usize = 512;

/// Rows one [`LoadJob::poll`] may adopt at most: a whole number of batches
/// (a batch that does not fit cannot be put back on the channel).
pub const ADOPT_BUDGET: usize = 8 * BATCH;

/// Rows a tail opens with: several screens, under one [`BATCH`], so the tail
/// lands in the first poll.
pub const TAIL_ROWS: usize = 200;

/// The first window [`tail_offset`] reads, widened 4x until it holds
/// [`TAIL_ROWS`] rows.
pub const TAIL_WINDOW: u64 = CHUNK as u64;

/// A buffer opened at a file's tail, remembering the size at open: below it a
/// row can be numbered, styled and searched; at or above it rows are arriving.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tail {
    /// The file's length when it was opened.
    #[allow(dead_code)]
    pub size_at_open: u64,
    /// The boundary styling stops at, in buffer rows; `None` until
    /// `Adopted::CaughtUp`.
    pub rows_at_open: Option<usize>,
}

/// One message from the reader thread.
#[derive(Debug)]
pub enum LoadMsg {
    /// Not text (a NUL in the first bytes); nothing read into rows — 64 KiB
    /// of mojibake is worse than saying so.
    Binary,
    /// The encoding, decided before any row; only the byte-splittable
    /// encodings reach here.
    Encoding(crate::encoding::Encoding),
    /// Not streamable (UTF-16): its owner should read it whole instead.
    NeedsEager(crate::encoding::Encoding),
    /// Rows in file order, each complete: a partial row is held back until its
    /// newline arrives.
    Rows(Vec<Vec<char>>),
    /// The end of the file and its CRLF decision. Sent by a reading job,
    /// never by a follower.
    Done { crlf: bool },
    /// A follower has read everything that exists; what arrives after is
    /// growth.
    CaughtUp { crlf: bool },
    /// The read failed — a message, so it becomes a status line rather than
    /// vanishing into a thread nobody reads.
    Failed(String),
}

/// What one [`LoadJob::poll`] did.
#[derive(Debug, PartialEq, Eq)]
pub enum Adopted {
    /// Nothing was ready (or the job is over).
    Nothing,
    /// The file is not text — see [`LoadMsg::Binary`]. The job is over.
    Binary,
    /// `n` rows were appended; still reading.
    Rows(usize),
    /// Rows were appended and the read finished.
    Finished { rows: usize, crlf: bool },
    /// Rows appended and the follower has read everything; the job is NOT
    /// over.
    CaughtUp { rows: usize, crlf: bool },
    /// The read failed; the string is for the status line.
    Failed(String),
    /// The reader decided the encoding before any row arrived.
    Encoding(crate::encoding::Encoding),
    /// The file cannot be streamed (UTF-16) and its owner should read it whole.
    NeedsEager(crate::encoding::Encoding),
}

/// A file being read on another thread.
pub struct LoadJob {
    rx: Receiver<LoadMsg>,
    cancel: Arc<AtomicBool>,
    /// Set when `Done` or `Failed` has been seen, so `poll` stops asking.
    done: bool,
    /// Set when `CaughtUp` has been seen: a follower waiting for growth — a
    /// finished view, not a read in flight.
    caught_up: bool,
    /// Rows handed out so far, for the status line.
    pub rows_read: usize,
    /// The encoding the reader decided, once it has.
    pub encoding: Option<crate::encoding::Encoding>,
}

impl LoadJob {
    /// Start reading `path` from its beginning.
    pub fn spawn(path: PathBuf) -> io::Result<Self> {
        Self::spawn_from(path, 0)
    }

    /// Start reading `path` from byte `from` — the tail path. The seek happens
    /// on the caller's thread, so a bad position is reported now.
    pub fn spawn_from(path: PathBuf, from: u64) -> io::Result<Self> {
        Self::open(path, from, false)
    }

    /// Start reading `path` from `from` and keep reading as it grows —
    /// `-f`/`--follow`.
    pub fn spawn_follow_from(path: PathBuf, from: u64) -> io::Result<Self> {
        Self::open(path, from, true)
    }

    fn open(path: PathBuf, from: u64, follow: bool) -> io::Result<Self> {
        // Open here, on the caller's thread: an unreadable file is reported
        // now, not by a message after a frame has been drawn.
        let mut file = File::open(&path)?;
        if from > 0 {
            file.seek(SeekFrom::Start(from))?;
        }
        let (tx, rx) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let c = Arc::clone(&cancel);
        std::thread::spawn(move || read_all(file, from, follow, &tx, &c));
        Ok(Self {
            rx,
            cancel,
            done: false,
            caught_up: false,
            rows_read: 0,
            encoding: None,
        })
    }

    /// Whether this is a follower that has read everything — a finished view,
    /// not a load.
    pub fn caught_up(&self) -> bool {
        self.caught_up
    }

    /// Append at most [`ADOPT_BUDGET`] rows to `out`, never blocking. A
    /// `Finished` with zero rows is normal.
    pub fn poll(&mut self, out: &mut Vec<Vec<char>>) -> Adopted {
        if self.done {
            return Adopted::Nothing;
        }
        let before = out.len();
        // The bound is exact: every `Rows` carries at most `BATCH` rows.
        while out.len() - before < ADOPT_BUDGET {
            match self.rx.try_recv() {
                Ok(LoadMsg::Encoding(enc)) => {
                    self.encoding = Some(enc);
                    return Adopted::Encoding(enc);
                }
                Ok(LoadMsg::NeedsEager(enc)) => {
                    self.done = true;
                    return Adopted::NeedsEager(enc);
                }
                Ok(LoadMsg::Binary) => {
                    self.done = true;
                    return Adopted::Binary;
                }
                Ok(LoadMsg::Rows(mut rows)) => {
                    debug_assert!(
                        rows.len() <= BATCH,
                        "a batch larger than BATCH would break the budget bound"
                    );
                    out.append(&mut rows);
                }
                Ok(LoadMsg::Done { crlf }) => {
                    self.done = true;
                    let took = out.len() - before;
                    self.rows_read += took;
                    return Adopted::Finished { rows: took, crlf };
                }
                Ok(LoadMsg::CaughtUp { crlf }) => {
                    // NOT done: the point of a follower is that more comes.
                    self.caught_up = true;
                    let took = out.len() - before;
                    self.rows_read += took;
                    return Adopted::CaughtUp { rows: took, crlf };
                }
                Ok(LoadMsg::Failed(e)) => {
                    self.done = true;
                    return Adopted::Failed(e);
                }
                // Disconnected without a Done: cancelled, or the channel
                // dropped. Treat it as the end.
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.done = true;
                    let took = out.len() - before;
                    self.rows_read += took;
                    return Adopted::Finished {
                        rows: took,
                        crlf: false,
                    };
                }
            }
        }
        let took = out.len() - before;
        self.rows_read += took;
        if took == 0 {
            Adopted::Nothing
        } else {
            Adopted::Rows(took)
        }
    }

    /// Stop the reader at the next chunk boundary; idempotent.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

/// One row's bytes as characters, in the file's encoding — whole rows at a
/// time, so a character split across chunk reads just accumulates.
fn decode_row(
    bytes: &[u8],
    encoding: Option<crate::encoding::Encoding>,
) -> Result<Vec<char>, String> {
    use crate::encoding::{self, Encoding};
    match encoding.unwrap_or(Encoding::Utf8) {
        Encoding::Cp1252 => Ok(encoding::decode(bytes, Encoding::Cp1252)?.chars().collect()),
        _ => match std::str::from_utf8(bytes) {
            Ok(s) => Ok(s.chars().collect()),
            Err(e) => Err(format!("not UTF-8 at byte {}", e.valid_up_to())),
        },
    }
}

/// Where the tail begins: the byte offset of the first of the last `rows`
/// rows, found by scanning backwards, so the cost is the tail and not the
/// file.
pub fn tail_offset(file: &mut File, size: u64, rows: usize, window: u64) -> io::Result<u64> {
    if size == 0 || rows == 0 {
        return Ok(size);
    }
    let mut window = window.max(1);
    loop {
        let win_start = size.saturating_sub(window);
        file.seek(SeekFrom::Start(win_start))?;
        let mut buf = vec![0u8; (size - win_start) as usize];
        file.read_exact(&mut buf)?;

        // A row start is the file's beginning or one past a newline — and never
        // past EOF, because a trailing newline does not begin a row.
        let mut starts: usize = 0;
        for (i, b) in buf.iter().enumerate() {
            if *b == b'\n' && win_start + i as u64 + 1 < size {
                starts += 1;
            }
        }
        let total_starts = starts + if win_start == 0 { 1 } else { 0 };

        if total_starts > rows || win_start == 0 {
            // Walk back to the `rows`-th row start from the end.
            let mut remaining = rows;
            for i in (0..buf.len()).rev() {
                if buf[i] != b'\n' {
                    continue;
                }
                let abs = win_start + i as u64 + 1;
                if abs >= size {
                    continue; // a trailing newline begins nothing
                }
                if remaining == 1 {
                    return Ok(abs);
                }
                remaining -= 1;
            }
            // Fewer rows in the window than asked for: start at the first full
            // row of the window.
            if win_start == 0 {
                return Ok(0);
            }
            return Ok(buf
                .iter()
                .position(|b| *b == b'\n')
                .map(|i| win_start + i as u64 + 1)
                .unwrap_or(size));
        }
        window = window.saturating_mul(4);
    }
}

/// The rows of `[from, size)` — the tail a caller reads after [`tail_offset`].
pub fn read_from(file: &mut File, from: u64) -> io::Result<Vec<u8>> {
    file.seek(SeekFrom::Start(from))?;
    let mut out = Vec::new();
    file.read_to_end(&mut out)?;
    Ok(out)
}

/// How long a follower waits at the end before looking again: a poll, not an
/// inotify watch. A file replaced rather than appended is not noticed.
const FOLLOW_SLEEP: Duration = Duration::from_millis(100);

/// Send `batch` as a `Rows` message, leaving it empty; whether the loop is
/// still there.
fn send_batch(tx: &mpsc::Sender<LoadMsg>, batch: &mut Vec<Vec<char>>) -> bool {
    if batch.is_empty() {
        return true;
    }
    tx.send(LoadMsg::Rows(std::mem::take(batch))).is_ok()
}

/// The reader thread: one pass, decoding rows as their newlines arrive. In
/// follow mode it waits at the end, says `CaughtUp` once, and does not flush
/// `pending` — a half-written row is not a row.
fn read_all(
    mut file: File,
    from: u64,
    follow: bool,
    tx: &mpsc::Sender<LoadMsg>,
    cancel: &AtomicBool,
) {
    use crate::encoding::{self, Encoding, Scope};
    let mut buf = vec![0u8; CHUNK];
    let mut pending: Vec<u8> = Vec::new();
    let mut batch: Vec<Vec<char>> = Vec::with_capacity(BATCH);
    let mut crlf = false;
    // The first chunk decides the encoding, from the same 64 KiB prefix.
    let mut encoding: Option<Encoding> = None;
    let mut first = true;
    let mut first_chunk_bom = false;
    let mut said_caught_up = false;
    let mut docker = false;
    let mut unwrap = crate::dockerlog::Unwrap::default();
    loop {
        if cancel.load(Ordering::Relaxed) {
            return;
        }
        let n = match file.read(&mut buf) {
            Ok(0) => {
                if !follow {
                    break;
                }
                // Flush whole rows BEFORE the message that says this is all of
                // them.
                if !send_batch(tx, &mut batch) {
                    return;
                }
                // Say so ONCE, then wait for growth, holding `pending` back.
                if !said_caught_up {
                    said_caught_up = true;
                    // `crlf` as of this moment; a tail is read-only, so the flag
                    // never reaches a write.
                    if tx.send(LoadMsg::CaughtUp { crlf }).is_err() {
                        return;
                    }
                }
                std::thread::sleep(FOLLOW_SLEEP);
                continue;
            }
            Ok(n) => n,
            Err(e) => {
                let _ = tx.send(LoadMsg::Failed(format!("{e}")));
                return;
            }
        };
        if first {
            first = false;
            // `Prefix` unless this read got the whole file.
            let whole = (n as u64) == file.metadata().map(|m| m.len()).unwrap_or(0);
            let scope = if whole { Scope::Whole } else { Scope::Prefix };
            let enc = encoding::detect(&buf[..n], scope);
            if !enc.rows_split_on_byte_newlines() {
                // UTF-16: no byte-level newline to split on.
                let _ = tx.send(LoadMsg::NeedsEager(enc));
                return;
            }
            if tx.send(LoadMsg::Encoding(enc)).is_err() {
                return; // the loop is gone; nothing to deliver to
            }
            // The probe runs AFTER the encoding decision: UTF-16 is text whose
            // bytes are half NULs, and the eager path above took it.
            if encoding::is_binary(&buf[..n]) {
                let _ = tx.send(LoadMsg::Binary);
                return;
            }
            encoding = Some(enc);
            // A docker log is unwrapped as it is read: what reaches a row is
            // the container's text.
            docker = crate::dockerlog::looks_like(&buf[..n]);
            // Only the file's own beginning can be a byte-order mark; at an
            // offset those same bytes are a character somebody wrote.
            first_chunk_bom = from == 0;
        }
        let chunk = &buf[..n];
        let mut start = if first_chunk_bom {
            // Skipped here rather than decoded, or it would be U+FEFF as the
            // buffer's first character — an invisible but real column.
            first_chunk_bom = false;
            encoding.map(|e| e.bom().len()).unwrap_or(0).min(n)
        } else {
            0
        };
        // Split on newlines; everything before the last one is complete rows.
        for (i, b) in chunk.iter().enumerate() {
            if *b != b'\n' {
                continue;
            }
            pending.extend_from_slice(&chunk[start..i]);
            if pending.last() == Some(&b'\r') {
                pending.pop();
                crlf = true;
            }
            if docker {
                docker_rows(&mut unwrap, &pending, &mut batch);
            } else {
                match decode_row(&pending, encoding) {
                    Ok(row) => batch.push(row),
                    Err(e) => {
                        let _ = tx.send(LoadMsg::Failed(e));
                        return;
                    }
                }
            }
            pending.clear();
            start = i + 1;
            if batch.len() >= BATCH && !send_batch(tx, &mut batch) {
                return; // the loop is gone; nothing to deliver to
            }
        }
        // The tail of the chunk is a partial row (or the start of one).
        pending.extend_from_slice(&chunk[start..]);
    }
    // A file not ending in a newline still has a final row.
    if !pending.is_empty() {
        if pending.last() == Some(&b'\r') {
            pending.pop();
            crlf = true;
        }
        if docker {
            docker_rows(&mut unwrap, &pending, &mut batch);
        } else {
            match decode_row(&pending, encoding) {
                Ok(row) => batch.push(row),
                Err(e) => {
                    let _ = tx.send(LoadMsg::Failed(e));
                    return;
                }
            }
        }
    }
    // A docker line still open at the end is a line the container wrote without a newline.
    if docker && let Some(line) = unwrap.flush() {
        batch.push(line.chars().collect());
    }
    if !batch.is_empty() {
        let _ = tx.send(LoadMsg::Rows(batch));
    }
    let _ = tx.send(LoadMsg::Done { crlf });
}

/// One record into rows: its `log` is a chunk of a line, so only complete
/// lines become rows; a stray line is shown as it is.
fn docker_rows(unwrap: &mut crate::dockerlog::Unwrap, line: &[u8], batch: &mut Vec<Vec<char>>) {
    let mut done: Vec<String> = Vec::new();
    match crate::dockerlog::record(line) {
        Some(rec) => unwrap.feed(&rec.text, &mut done),
        None => done.push(String::from_utf8_lossy(line).into_owned()),
    }
    batch.extend(done.into_iter().map(|l| l.chars().collect()));
}

#[cfg(test)]
mod docker_tests {
    use super::*;
    use std::time::Instant;

    /// A file of its own per test, named for the test so two cannot collide.
    struct Temp(std::path::PathBuf);
    impl Temp {
        fn new(name: &str, body: &[u8]) -> Self {
            let p = std::env::temp_dir().join(format!("rano_docker_{name}"));
            std::fs::write(&p, body).expect("write");
            Temp(p)
        }
        fn path(&self) -> &std::path::Path {
            &self.0
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    /// A docker log read through the loader: the container's text, and a line
    /// spanning three records back to one row.
    #[test]
    fn a_docker_log_reads_as_the_containers_text() {
        let long = "x".repeat(16384);
        let mut body = String::new();
        // The newline arrives in the LAST chunk of the long line, which is
        // what makes the next record a line of its own.
        for part in [long.as_str(), long.as_str(), "tail\\n"] {
            body.push_str(&format!(
                "{{\"log\":\"{part}\",\"stream\":\"stdout\",\"time\":\"t\"}}\n"
            ));
        }
        body.push_str("{\"log\":\"one\\n\",\"stream\":\"stdout\",\"time\":\"t\"}\n");
        body.push_str("{\"log\":\"bad\\n\",\"stream\":\"stderr\",\"time\":\"t\"}\n");
        let t = Temp::new("docker-json.log", body.as_bytes());
        let mut job = LoadJob::spawn(t.path().to_path_buf()).expect("spawn");
        let mut rows: Vec<Vec<char>> = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            assert!(Instant::now() < deadline, "the loader did not finish");
            match job.poll(&mut rows) {
                Adopted::Finished { .. } => break,
                Adopted::Encoding(_) => {}
                Adopted::Nothing => std::thread::sleep(Duration::from_millis(1)),
                other => panic!("unexpected: {other:?}"),
            }
        }
        let text: Vec<String> = rows.iter().map(|r| r.iter().collect()).collect();
        assert_eq!(text.len(), 3, "{text:?}");
        assert_eq!(text[0].len(), 16384 * 2 + 4, "one line from three records");
        assert_eq!(text[1], "one");
        assert_eq!(text[2], "bad");
    }

    /// Detection is the first record; a plain log is untouched.
    #[test]
    fn an_ordinary_log_is_not_unwrapped() {
        let t = Temp::new("plain.log", b"2026-10-30 INFO one\nINFO two\n");
        let mut job = LoadJob::spawn(t.path().to_path_buf()).expect("spawn");
        let mut rows: Vec<Vec<char>> = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            assert!(Instant::now() < deadline, "the loader did not finish");
            match job.poll(&mut rows) {
                Adopted::Finished { .. } => break,
                Adopted::Encoding(_) => {}
                Adopted::Nothing => std::thread::sleep(Duration::from_millis(1)),
                other => panic!("unexpected: {other:?}"),
            }
        }
        let text: Vec<String> = rows.iter().map(|r| r.iter().collect()).collect();
        assert_eq!(text, vec!["2026-10-30 INFO one", "INFO two"]);
    }
}

#[cfg(test)]
mod tail_tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    struct Temp(PathBuf);
    impl Temp {
        fn new(name: &str, body: &[u8]) -> Self {
            let p = std::env::temp_dir().join(format!("rano_tail_{name}"));
            fs::write(&p, body).expect("write");
            Temp(p)
        }
        fn path(&self) -> PathBuf {
            self.0.clone()
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.0);
        }
    }

    /// The obvious reading: every row, in order.
    fn rows_of(body: &[u8]) -> Vec<&[u8]> {
        let mut out: Vec<&[u8]> = Vec::new();
        let mut start = 0usize;
        for (i, b) in body.iter().enumerate() {
            if *b == b'\n' {
                out.push(&body[start..i]);
                start = i + 1;
            }
        }
        if start < body.len() {
            out.push(&body[start..]);
        }
        out
    }

    /// The start byte of the last `rows` rows, by reading the whole file.
    fn reference(body: &[u8], rows: usize) -> u64 {
        if body.is_empty() || rows == 0 {
            return body.len() as u64;
        }
        let all = rows_of(body);
        if all.len() <= rows {
            return 0;
        }
        let want = all.len() - rows;
        let mut seen = 0usize;
        let mut start = 0usize;
        for (i, b) in body.iter().enumerate() {
            if *b == b'\n' {
                seen += 1;
                if seen == want {
                    start = i + 1;
                    break;
                }
            }
        }
        start as u64
    }

    /// The backward scan agrees with a forward read, over the shapes where the
    /// off-by-ones live (trailing newline, fewer rows, a huge row, CRLF).
    #[test]
    fn the_tail_offset_matches_a_forward_read() {
        let bodies: Vec<(&str, Vec<u8>)> = vec![
            ("empty", b"".to_vec()),
            ("one", b"a".to_vec()),
            ("one_nl", b"a\n".to_vec()),
            ("two", b"a\nb".to_vec()),
            ("two_nl", b"a\nb\n".to_vec()),
            ("ten", b"a\nb\nc\nd\ne\nf\ng\nh\ni\nj".to_vec()),
            ("ten_nl", b"a\nb\nc\nd\ne\nf\ng\nh\ni\nj\n".to_vec()),
            ("blanks", b"\n\n\n\n".to_vec()),
            ("crlf", b"a\r\nb\r\nc\r\n".to_vec()),
            ("huge_row", {
                let mut v = vec![b'x'; 5_000];
                v.push(b'\n');
                v.extend_from_slice(b"short\n");
                v
            }),
            ("trailing_blanks", b"a\n\n\n".to_vec()),
            (
                "no_nl_many",
                (0..500)
                    .map(|i| format!("row {i}\n"))
                    .collect::<String>()
                    .into_bytes(),
            ),
        ];
        for (label, body) in bodies {
            let t = Temp::new(&format!("{label}.txt"), &body);
            let size = body.len() as u64;
            for rows in [1usize, 2, 3, 7, 64, 1000] {
                // A window small enough that widening is exercised, and one big
                // enough that it is not.
                for window in [16u64, 64, 4096] {
                    let mut f = File::open(t.path()).expect("open");
                    let got = tail_offset(&mut f, size, rows, window).expect("tail_offset");
                    let want = reference(&body, rows);
                    assert_eq!(
                        got, want,
                        "{label}: rows={rows} window={window} — got {got}, reference {want}"
                    );
                }
            }
        }
    }

    /// The bytes from that offset are the last `rows` rows, exactly.
    #[test]
    fn reading_from_the_offset_gives_the_last_rows() {
        let body: Vec<u8> = (0..300)
            .map(|i| format!("line {i}\n"))
            .collect::<String>()
            .into_bytes();
        let t = Temp::new("readback.txt", &body);
        let size = body.len() as u64;
        for rows in [1usize, 5, 50] {
            let mut f = File::open(t.path()).expect("open");
            let off = tail_offset(&mut f, size, rows, 128).expect("tail_offset");
            let mut g = File::open(t.path()).expect("open");
            let bytes = read_from(&mut g, off).expect("read");
            let got = rows_of(&bytes);
            assert_eq!(got.len(), rows, "rows={rows}");
            let want = rows_of(&body);
            assert_eq!(
                got,
                &want[want.len() - rows..],
                "rows={rows}: the bytes from the offset are not the last rows"
            );
        }
    }

    /// `spawn_from` at a tail offset gives the last rows, NOT the file.
    #[test]
    fn spawn_from_reads_the_last_rows_and_not_the_file() {
        let body: Vec<u8> = (0..300)
            .map(|i| format!("line {i}\n"))
            .collect::<String>()
            .into_bytes();
        let t = Temp::new("spawn_from.txt", &body);
        let size = body.len() as u64;
        let mut f = File::open(t.path()).expect("open");
        let off = tail_offset(&mut f, size, 5, 128).expect("tail_offset");
        let mut job = LoadJob::spawn_from(t.path(), off).expect("spawn_from");
        let mut rows: Vec<Vec<char>> = Vec::new();
        let mut done = false;
        for _ in 0..500 {
            match job.poll(&mut rows) {
                Adopted::Finished { .. } => {
                    done = true;
                    break;
                }
                Adopted::Failed(e) => panic!("the reader failed: {e}"),
                _ => std::thread::sleep(std::time::Duration::from_millis(2)),
            }
        }
        assert!(done, "the reader never finished");
        let got: Vec<String> = rows.iter().map(|r| r.iter().collect()).collect();
        assert_eq!(got.len(), 5, "the whole file arrived: {got:?}");
        assert_eq!(got[0], "line 295");
        assert_eq!(got[4], "line 299");
    }

    /// The cost is the tail, not the file: the scan goes backwards from the
    /// end.
    #[test]
    fn a_large_file_costs_the_window_not_the_file() {
        let body: Vec<u8> = (0..400_000)
            .map(|i| format!("2026-10-09T12:00:00 INFO row {i}\n"))
            .collect::<String>()
            .into_bytes();
        let t = Temp::new("large.txt", &body);
        let size = body.len() as u64;
        assert!(
            size > 10 * 1024 * 1024,
            "the fixture must be big: {size} bytes"
        );
        let mut f = File::open(t.path()).expect("open");
        let t0 = std::time::Instant::now();
        let off = tail_offset(&mut f, size, 40, 64 * 1024).expect("tail_offset");
        let dt = t0.elapsed();
        // The last 40 rows of `row 399xxx` lines.
        assert!(
            off > size - 64 * 1024,
            "the offset should be in the tail: {off}"
        );
        let want = reference(&body, 40);
        assert_eq!(off, want);
        // The point: the bound is not proportional to the file (64 KiB of a
        // 12 MB file).
        assert!(
            dt.as_millis() < 200,
            "tail_offset took {dt:?} on a {size}-byte file — that is not a tail scan"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::time::{Duration, Instant};

    /// A scratch file that removes itself, so the tests leave nothing behind.
    struct Temp(PathBuf);

    impl Temp {
        fn new(name: &str, contents: &[u8]) -> Self {
            let p = std::env::temp_dir().join(format!("rano_load_{name}"));
            std::fs::write(&p, contents).expect("write fixture");
            Self(p)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    /// Poll until the job finishes, with a deadline. Returns every row.
    fn drain(job: &mut LoadJob) -> (Vec<Vec<char>>, bool, usize) {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut rows: Vec<Vec<char>> = Vec::new();
        let mut polls = 0usize;
        let crlf;
        loop {
            assert!(Instant::now() < deadline, "loader did not finish");
            polls += 1;
            let before = rows.len();
            match job.poll(&mut rows) {
                Adopted::Nothing => std::thread::sleep(Duration::from_millis(1)),
                Adopted::Encoding(_) => {}
                Adopted::Binary => panic!("a text fixture was called binary"),
                Adopted::NeedsEager(e) => panic!("unexpected eager path: {}", e.name()),
                Adopted::Rows(n) => assert_eq!(n, rows.len() - before, "count matches"),
                Adopted::Finished { rows: n, crlf: c } => {
                    assert_eq!(n, rows.len() - before, "count matches");
                    crlf = c;
                    break;
                }
                // A `spawn_from` job ends rather than catching up; a follower
                // would hang this helper.
                Adopted::CaughtUp { .. } => panic!("a follower has no end to drain"),
                Adopted::Failed(e) => panic!("load failed: {e}"),
            }
            assert!(
                rows.len() - before <= ADOPT_BUDGET,
                "poll adopted more than the budget"
            );
        }
        (rows, crlf, polls)
    }

    fn text(rows: &[Vec<char>]) -> Vec<String> {
        rows.iter().map(|r| r.iter().collect()).collect()
    }

    #[test]
    fn reads_a_file_into_rows() {
        let t = Temp::new("plain.txt", b"one\ntwo\nthree\n");
        let mut job = LoadJob::spawn(t.path().to_path_buf()).expect("spawn");
        let (rows, crlf, _) = drain(&mut job);
        assert_eq!(text(&rows), ["one", "two", "three"]);
        assert!(!crlf);
    }

    #[test]
    fn a_file_without_a_trailing_newline_has_a_final_row() {
        let t = Temp::new("notrail.txt", b"a\nb");
        let mut job = LoadJob::spawn(t.path().to_path_buf()).expect("spawn");
        let (rows, _, _) = drain(&mut job);
        assert_eq!(text(&rows), ["a", "b"], "no trailing empty row");
    }

    #[test]
    fn a_trailing_newline_does_not_add_an_empty_row() {
        let t = Temp::new("trail.txt", b"a\nb\n");
        let mut job = LoadJob::spawn(t.path().to_path_buf()).expect("spawn");
        let (rows, _, _) = drain(&mut job);
        assert_eq!(text(&rows), ["a", "b"]);
    }

    #[test]
    fn an_empty_file_is_one_empty_row() {
        let t = Temp::new("empty.txt", b"");
        let mut job = LoadJob::spawn(t.path().to_path_buf()).expect("spawn");
        let (rows, _, _) = drain(&mut job);
        assert!(
            rows.is_empty(),
            "the loader reports no rows; the caller seeds one"
        );
    }

    #[test]
    fn crlf_is_reported_and_stripped() {
        let t = Temp::new("crlf.txt", b"a\r\nb\r\n");
        let mut job = LoadJob::spawn(t.path().to_path_buf()).expect("spawn");
        let (rows, crlf, _) = drain(&mut job);
        assert_eq!(text(&rows), ["a", "b"], "the \\r is not part of the row");
        assert!(crlf, "the CRLF decision comes back from the same pass");
    }

    #[test]
    fn a_row_longer_than_a_chunk_is_one_row() {
        // A minified file: 200 KB on one line against a 64 KiB read — a chunk
        // boundary must not split it.
        let big = "x".repeat(200_000);
        let body = format!("head\n{big}\ntail\n");
        let t = Temp::new("longrow.txt", body.as_bytes());
        let mut job = LoadJob::spawn(t.path().to_path_buf()).expect("spawn");
        let (rows, _, _) = drain(&mut job);
        let got = text(&rows);
        assert_eq!(got.len(), 3);
        assert_eq!(got[0], "head");
        assert_eq!(got[1].len(), 200_000, "the long row arrived whole");
        assert_eq!(got[2], "tail");
    }

    #[test]
    fn a_multibyte_character_split_across_a_chunk_boundary_survives() {
        // A 3-byte character straddling the 64 KiB read: decoding per chunk
        // would fail here — the reader decodes whole rows.
        let pad = "a".repeat(CHUNK - 1);
        let body = format!("{pad}\u{4e2d}\nnext\n");
        let t = Temp::new("split.txt", body.as_bytes());
        let mut job = LoadJob::spawn(t.path().to_path_buf()).expect("spawn");
        let (rows, _, _) = drain(&mut job);
        let got = text(&rows);
        assert_eq!(got.len(), 2, "{got:?}");
        assert!(
            got[0].ends_with('\u{4e2d}'),
            "the character survived intact"
        );
        assert_eq!(got[0].chars().count(), CHUNK - 1 + 1);
        assert_eq!(got[1], "next");
    }

    #[test]
    fn adoption_is_budgeted_across_polls() {
        // Several bounded polls must run, and no row lost or duplicated.
        let body: String = (0..(ADOPT_BUDGET * 3))
            .map(|i| format!("row {i}\n"))
            .collect();
        let t = Temp::new("many.lines", body.as_bytes());
        let mut job = LoadJob::spawn(t.path().to_path_buf()).expect("spawn");
        let (rows, _, polls) = drain(&mut job);
        assert_eq!(rows.len(), ADOPT_BUDGET * 3);
        assert_eq!(text(&rows)[0], "row 0");
        assert_eq!(
            text(&rows)[ADOPT_BUDGET * 3 - 1],
            format!("row {}", ADOPT_BUDGET * 3 - 1)
        );
        assert!(
            polls > 1,
            "the budget forced more than one poll (got {polls})"
        );
    }

    #[test]
    fn poll_never_blocks() {
        // A poll on a job whose worker has produced nothing must return
        // immediately.
        let body: String = (0..200_000).map(|i| format!("row {i}\n")).collect();
        let t = Temp::new("nonblock.txt", body.as_bytes());
        let mut job = LoadJob::spawn(t.path().to_path_buf()).expect("spawn");
        let mut sink: Vec<Vec<char>> = Vec::new();
        let t0 = Instant::now();
        for _ in 0..100 {
            let _ = job.poll(&mut sink);
        }
        let elapsed = t0.elapsed();
        // If `poll` blocked on an empty channel this would take as long as the
        // read.
        assert!(
            elapsed < Duration::from_millis(50),
            "100 polls took {elapsed:?} — poll is waiting on the worker"
        );
        job.cancel();
    }

    #[test]
    fn cancel_stops_the_reader() {
        let body: String = (0..2_000_000)
            .map(|i| format!("row {i} padding\n"))
            .collect();
        let t = Temp::new("cancel.txt", body.as_bytes());
        let job = LoadJob::spawn(t.path().to_path_buf()).expect("spawn");
        let t0 = Instant::now();
        job.cancel();
        // The worker checks the flag between reads; poll until it reports done.
        let mut job = job;
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut sink: Vec<Vec<char>> = Vec::new();
        loop {
            assert!(Instant::now() < deadline, "cancel did not take effect");
            match job.poll(&mut sink) {
                Adopted::Finished { .. } | Adopted::Failed(_) => break,
                Adopted::Nothing => std::thread::sleep(Duration::from_millis(1)),
                _ => {}
            }
        }
        assert!(
            t0.elapsed() < Duration::from_secs(5),
            "cancellation took {:?}",
            t0.elapsed()
        );
    }

    /// A follower appends what arrives and holds back a half-written row,
    /// driven against a real file that grows under it.
    #[test]
    fn a_follower_appends_growth_and_holds_back_a_partial_row() {
        use std::io::Write;
        let t = Temp::new("follow.txt", b"one\ntwo\n");
        let mut job = LoadJob::spawn_follow_from(t.path().to_path_buf(), 0).expect("spawn");
        let mut rows: Vec<Vec<char>> = Vec::new();

        // The first pass reads everything there is and says so.
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            assert!(Instant::now() < deadline, "the follower never caught up");
            if let Adopted::CaughtUp { .. } = job.poll(&mut rows) {
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(text(&rows), ["one", "two"]);

        // "thr" has no newline after it, so it must NOT appear as a row.
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(t.path())
            .expect("append");
        f.write_all(b"thr").expect("write");
        f.flush().expect("flush");
        let quiet = Instant::now() + Duration::from_millis(400);
        while Instant::now() < quiet {
            let _ = job.poll(&mut rows);
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(text(&rows), ["one", "two"], "a partial row was emitted");

        f.write_all(b"ee\n").expect("write");
        f.flush().expect("flush");
        let deadline = Instant::now() + Duration::from_secs(5);
        while rows.len() < 3 {
            assert!(Instant::now() < deadline, "the growth never arrived");
            let _ = job.poll(&mut rows);
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(text(&rows), ["one", "two", "three"]);

        f.write_all(b"four\nfive\n").expect("write");
        f.flush().expect("flush");
        let deadline = Instant::now() + Duration::from_secs(5);
        while rows.len() < 5 {
            assert!(Instant::now() < deadline, "the burst never arrived");
            let _ = job.poll(&mut rows);
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(text(&rows), ["one", "two", "three", "four", "five"]);

        job.cancel();
    }

    /// A caught-up follower is not a read in flight.
    #[test]
    fn a_caught_up_follower_is_not_loading() {
        let t = Temp::new("caught_up.txt", b"a\nb\n");
        let mut job = LoadJob::spawn_follow_from(t.path().to_path_buf(), 0).expect("spawn");
        assert!(!job.caught_up(), "not before it has read anything");
        let mut rows: Vec<Vec<char>> = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !job.caught_up() {
            assert!(Instant::now() < deadline, "never caught up");
            let _ = job.poll(&mut rows);
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(text(&rows), ["a", "b"]);
        assert!(job.caught_up());
        // Polling after the catch-up is not an end.
        assert_eq!(job.poll(&mut rows), Adopted::Nothing);
        job.cancel();
    }

    #[test]
    fn a_missing_file_fails_at_spawn_not_later() {
        // The one error that must not become a status line after the fact —
        // there is nothing to show.
        let missing = std::env::temp_dir().join("rano_load_there_is_no_such_file");
        let _ = std::fs::remove_file(&missing);
        assert!(LoadJob::spawn(missing).is_err());
    }

    #[test]
    fn latin1_bytes_are_decoded_as_cp1252_not_refused() {
        // Not valid UTF-8 is the last rung of the ladder, not an error.
        let t = Temp::new("latin1.bin", b"caf\xe9 latin-1\n");
        let mut job = LoadJob::spawn(t.path().to_path_buf()).expect("spawn");
        let (rows, _, _) = drain(&mut job);
        assert_eq!(text(&rows), ["caf\u{e9} latin-1"]);
        assert_eq!(job.encoding, Some(crate::encoding::Encoding::Cp1252));
    }

    /// A mid-file byte-order mark is content: eating one a tail read happens
    /// to land on would corrupt a line out of the middle of a log.
    #[test]
    fn a_tail_read_does_not_eat_a_mid_file_byte_order_mark() {
        let mut body = b"first\n".to_vec();
        body.extend_from_slice("\u{feff}".as_bytes());
        body.extend_from_slice(b"second\n");
        let t = Temp::new("midbom.txt", &body);
        let mut job = LoadJob::spawn_from(t.path().to_path_buf(), 6).expect("spawn_from");
        let (rows, _, _) = drain(&mut job);
        assert_eq!(text(&rows), ["\u{feff}second"], "the mark was eaten");
    }

    #[test]
    fn the_encoding_is_reported_before_any_row() {
        // A file opened and saved immediately must not be rewritten as UTF-8.
        for (name, body, want) in [
            (
                "enc_plain.txt",
                b"plain\n".to_vec(),
                crate::encoding::Encoding::Utf8,
            ),
            (
                "enc_bom.txt",
                [&[0xEF, 0xBB, 0xBF][..], b"plain\n"].concat(),
                crate::encoding::Encoding::Utf8Bom,
            ),
            (
                "enc_latin.bin",
                b"caf\xe9\n".to_vec(),
                crate::encoding::Encoding::Cp1252,
            ),
        ] {
            let t = Temp::new(name, &body);
            let mut job = LoadJob::spawn(t.path().to_path_buf()).expect("spawn");
            let mut sink = Vec::new();
            let deadline = Instant::now() + Duration::from_secs(5);
            while job.encoding.is_none() {
                assert!(Instant::now() < deadline, "{name}: never reported");
                job.poll(&mut sink);
                std::thread::sleep(Duration::from_millis(1));
            }
            assert_eq!(job.encoding, Some(want), "{name}");
        }
    }

    #[test]
    fn utf16_is_declined_for_the_eager_path() {
        // Its rows cannot be found without decoding, so the streaming reader
        // declines.
        let text = "one\ntwo\n";
        let bytes = crate::encoding::encode(text, crate::encoding::Encoding::Utf16Le).unwrap();
        let t = Temp::new("u16.txt", &bytes);
        let mut job = LoadJob::spawn(t.path().to_path_buf()).expect("spawn");
        let mut sink = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            assert!(Instant::now() < deadline, "never declined");
            match job.poll(&mut sink) {
                Adopted::NeedsEager(e) => {
                    assert_eq!(e, crate::encoding::Encoding::Utf16Le);
                    break;
                }
                Adopted::Finished { .. } => panic!("UTF-16 was streamed as if it were UTF-8"),
                _ => std::thread::sleep(Duration::from_millis(1)),
            }
        }
    }
}
