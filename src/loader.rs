//! Reading a file without blocking the event loop.
//!
//! The freeze this removes: before it, `main()` read and decoded the whole file
//! *before* the terminal was even initialised, so `rano huge.log` was a dead
//! screen — no frame, no chrome, no event loop, so not even `^C` — for 575 ms at
//! 184 MB and about six seconds at 2 GB. Measured: `bench_cold_open`.
//!
//! Three things about the shape are deliberate, and each is a bug avoided
//! rather than a detail chosen (see TODO.md §14.6):
//!
//! 1. **It hands out chunks, not a buffer.** The first screenful of a 193 MB
//!    file costs one 64 KiB read — **37 µs** — because the top of a file does
//!    not need the file. A single `read_to_string` cannot do that, and cannot be
//!    abandoned either.
//!
//! 2. **`poll` never waits.** `try_recv`, never `recv`: a receiver that blocks
//!    moves the stall from the read to the wait and leaves the loop just as
//!    dead. This is the same contract `lsp_poll` and `exec_poll` already keep.
//!
//! 3. **Adoption is budgeted.** `while let Ok(chunk) = rx.try_recv()` is
//!    unbounded, so a worker faster than the loop turns adoption itself into the
//!    stall it was meant to fix. One `poll` takes at most [`ADOPT_BUDGET`] rows.
//!
//! Cancellation is one `AtomicBool` checked between reads, so a 193 MB read is
//! abandoned within a chunk (**~5 ms**) rather than at the end — set by `^C`,
//! by an edit, and by opening another file.

use std::fs::File;
use std::io::{self, Read};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};

/// Read granularity. One page-cache-friendly read, and enough rows to fill a
/// viewport several times over in the common log case.
const CHUNK: usize = 64 * 1024;

/// Rows per message. Small enough that the loop interleaves with the worker,
/// large enough that the channel is not the cost.
///
/// A message never carries more than this, which is what makes the adoption
/// budget below a true bound rather than "usually".
const BATCH: usize = 512;

/// Rows one [`LoadJob::poll`] may adopt at most — **exactly**, because it is a
/// whole number of batches.
///
/// On an mpsc channel a batch that does not fit cannot be put back, so a budget
/// that was not a multiple of [`BATCH`] would be exceeded by up to one batch.
/// Making it a multiple turns "stop when the budget is reached" into a hard
/// bound, and the loop between adoptions is what stops a fast disk from making
/// the loader the stall.
pub const ADOPT_BUDGET: usize = 8 * BATCH;

/// One message from the reader thread.
#[derive(Debug)]
pub enum LoadMsg {
    /// The encoding the reader decided, sent before any row so the buffer knows
    /// how to save the file back. Only the byte-splittable encodings reach here.
    Encoding(crate::encoding::Encoding),
    /// The file is not one this reader can stream — UTF-16, whose rows cannot be
    /// found without decoding — so its owner should read it whole instead.
    NeedsEager(crate::encoding::Encoding),
    /// Rows in file order. Every row is complete: the reader holds a partial
    /// row back until its newline arrives, so a chunk boundary can never
    /// split one.
    Rows(Vec<Vec<char>>),
    /// The end of the file. Carries whether it used CRLF, decided by the same
    /// pass that split the rows (a second scan would be a second full read).
    Done { crlf: bool },
    /// The read failed: unreadable, or not UTF-8. A message rather than an
    /// `eprintln!`, so it can become a status line instead of vanishing into a
    /// thread nobody is reading.
    Failed(String),
}

/// What one [`LoadJob::poll`] did.
#[derive(Debug, PartialEq, Eq)]
pub enum Adopted {
    /// Nothing was ready (or the job is over).
    Nothing,
    /// `n` rows were appended; still reading.
    Rows(usize),
    /// Rows were appended and the read finished.
    Finished { rows: usize, crlf: bool },
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
    /// Rows handed out so far, for the status line.
    pub rows_read: usize,
    /// The encoding the reader decided, once it has. `None` until the first
    /// chunk has been sniffed.
    pub encoding: Option<crate::encoding::Encoding>,
}

impl LoadJob {
    /// Start reading `path`. The returned job is polled by the event loop; it
    /// never blocks the caller, including `spawn`.
    pub fn spawn(path: PathBuf) -> io::Result<Self> {
        // Open here, on the caller's thread: a missing or unreadable file is
        // the caller's problem to report *now*, not a message that arrives
        // after a frame has already been drawn.
        let file = File::open(&path)?;
        let (tx, rx) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let c = Arc::clone(&cancel);
        std::thread::spawn(move || read_all(file, &tx, &c));
        Ok(Self {
            rx,
            cancel,
            done: false,
            rows_read: 0,
            encoding: None,
        })
    }

    /// Append at most [`ADOPT_BUDGET`] rows to `out`. **Never blocks.**
    ///
    /// Returns as soon as the channel is empty, so the loop can draw; the
    /// budget is what stops a fast worker from starving the frame. Rows go into
    /// the caller's `Vec` rather than into the return value because the caller
    /// already has one — the buffer's own rows — and copying them twice to say
    /// "here are your rows" would be the only allocation this adds.
    ///
    /// A `Finished` with zero rows is the normal ending for a file whose last
    /// batch landed in an earlier poll; `out` may be left untouched.
    pub fn poll(&mut self, out: &mut Vec<Vec<char>>) -> Adopted {
        if self.done {
            return Adopted::Nothing;
        }
        let before = out.len();
        // The bound is exact because every `Rows` message carries at most
        // `BATCH` rows and the budget is a whole number of batches: eight
        // messages reach it and the loop stops. See `ADOPT_BUDGET`.
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
                Ok(LoadMsg::Failed(e)) => {
                    self.done = true;
                    return Adopted::Failed(e);
                }
                // Disconnected without a Done: the worker stopped early (it was
                // cancelled, or the channel dropped). Treat it as the end.
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

    /// Stop the reader at the next chunk boundary. Idempotent, and safe to call
    /// from the event loop while the worker is mid-read.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

/// One row's bytes as characters, in the file's encoding.
///
/// A whole row at a time is what makes a multi-byte character split across two
/// chunk reads a non-event: the bytes accumulate until the newline, so the
/// sequence is never cut.
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

/// The reader thread: one pass, decoding rows as their newlines arrive.
///
/// It never re-reads and never holds the whole file: `pending` is one partial
/// row, and a row is sent as soon as it is complete.
fn read_all(mut file: File, tx: &mpsc::Sender<LoadMsg>, cancel: &AtomicBool) {
    use crate::encoding::{self, Encoding, Scope};
    let mut buf = vec![0u8; CHUNK];
    let mut pending: Vec<u8> = Vec::new();
    let mut batch: Vec<Vec<char>> = Vec::with_capacity(BATCH);
    let mut crlf = false;
    // The first chunk decides the encoding, from the same 64 KiB prefix the
    // ladder is measured at (TODO.md §13.5: 8 µs against 46 ms for a
    // whole-file scan, agreeing with the whole-file answer on every shape).
    let mut encoding: Option<Encoding> = None;
    let mut first = true;
    let mut first_chunk_bom = false;
    loop {
        if cancel.load(Ordering::Relaxed) {
            return;
        }
        let n = match file.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) => {
                let _ = tx.send(LoadMsg::Failed(format!("{e}")));
                return;
            }
        };
        if first {
            first = false;
            // `Prefix` unless this read got the whole file: a sequence cut by
            // the read is the reader's doing, not the file's.
            let whole = (n as u64) == file.metadata().map(|m| m.len()).unwrap_or(0);
            let scope = if whole { Scope::Whole } else { Scope::Prefix };
            let enc = encoding::detect(&buf[..n], scope);
            if !enc.rows_split_on_byte_newlines() {
                // UTF-16: no byte-level newline to split on, so this reader
                // cannot do it. The owner decodes the file whole instead —
                // which is what any file whose rows cannot be found without
                // decoding needs, at any size.
                let _ = tx.send(LoadMsg::NeedsEager(enc));
                return;
            }
            if tx.send(LoadMsg::Encoding(enc)).is_err() {
                return; // the loop is gone; nothing to deliver to
            }
            encoding = Some(enc);
            first_chunk_bom = true;
        }
        let chunk = &buf[..n];
        let mut start = if first_chunk_bom {
            // The BOM is at the very start of the file and is not content: it
            // is skipped here rather than decoded, or it would be U+FEFF as the
            // buffer's first character — invisible, but a real column that
            // Home, click positioning and `^`-anchored regexes all see.
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
            match decode_row(&pending, encoding) {
                Ok(row) => batch.push(row),
                Err(e) => {
                    let _ = tx.send(LoadMsg::Failed(e));
                    return;
                }
            }
            pending.clear();
            start = i + 1;
            if batch.len() >= BATCH {
                if tx.send(LoadMsg::Rows(std::mem::take(&mut batch))).is_err() {
                    return; // the loop is gone; nothing to deliver to
                }
                batch = Vec::with_capacity(BATCH);
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
        match decode_row(&pending, encoding) {
            Ok(row) => batch.push(row),
            Err(e) => {
                let _ = tx.send(LoadMsg::Failed(e));
                return;
            }
        }
    }
    if !batch.is_empty() {
        let _ = tx.send(LoadMsg::Rows(batch));
    }
    let _ = tx.send(LoadMsg::Done { crlf });
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
                Adopted::NeedsEager(e) => panic!("unexpected eager path: {}", e.name()),
                Adopted::Rows(n) => assert_eq!(n, rows.len() - before, "count matches"),
                Adopted::Finished { rows: n, crlf: c } => {
                    assert_eq!(n, rows.len() - before, "count matches");
                    crlf = c;
                    break;
                }
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
        // The case the lazy prototype got wrong (a phantom empty final row).
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
        // `Buffer`'s own invariant: there is always at least one row.
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
        // A minified file: 200 KB on one line, against a 64 KiB read. A chunk
        // boundary must not split it, and must not produce a phantom row.
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
        // A 3-byte character straddling the 64 KiB read: decoding per CHUNK
        // would fail here, which is why the reader accumulates bytes and
        // decodes whole rows.
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
        // A file with more rows than one poll may take: several polls must run,
        // each bounded, and no row may be lost or duplicated.
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
        // The property that makes the loop live. A poll on a job whose worker
        // has not produced anything yet must return immediately, not wait.
        let body: String = (0..200_000).map(|i| format!("row {i}\n")).collect();
        let t = Temp::new("nonblock.txt", body.as_bytes());
        let mut job = LoadJob::spawn(t.path().to_path_buf()).expect("spawn");
        // Immediately, before the worker can plausibly have finished.
        let mut sink: Vec<Vec<char>> = Vec::new();
        let t0 = Instant::now();
        for _ in 0..100 {
            let _ = job.poll(&mut sink);
        }
        let elapsed = t0.elapsed();
        // 100 polls of a job that may still be reading: if `poll` blocked on an
        // empty channel this would take as long as the read.
        assert!(
            elapsed < Duration::from_millis(50),
            "100 polls took {elapsed:?} — poll is waiting on the worker"
        );
        job.cancel();
    }

    #[test]
    fn cancel_stops_the_reader() {
        // A big enough file that the worker cannot have finished instantly.
        let body: String = (0..2_000_000)
            .map(|i| format!("row {i} padding\n"))
            .collect();
        let t = Temp::new("cancel.txt", body.as_bytes());
        let job = LoadJob::spawn(t.path().to_path_buf()).expect("spawn");
        let t0 = Instant::now();
        job.cancel();
        // The worker checks the flag between reads, so the bound is one chunk.
        // Poll until it reports done, or fail.
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

    #[test]
    fn a_missing_file_fails_at_spawn_not_later() {
        // The caller has to be able to report this before a frame is drawn —
        // and it is the one error that should not become a status line after
        // the fact, because there is nothing to show.
        let missing = std::env::temp_dir().join("rano_load_there_is_no_such_file");
        let _ = std::fs::remove_file(&missing);
        assert!(LoadJob::spawn(missing).is_err());
    }

    #[test]
    fn latin1_bytes_are_decoded_as_cp1252_not_refused() {
        // What changed: a file that is not valid UTF-8 used to be refused
        // outright — "stream did not contain valid UTF-8" and no way in. It is
        // now the last rung of the ladder, so the file opens and its accented
        // letters are right. The byte 0xE9 is 'e-acute', not an error.
        let t = Temp::new("latin1.bin", b"caf\xe9 latin-1\n");
        let mut job = LoadJob::spawn(t.path().to_path_buf()).expect("spawn");
        let (rows, _, _) = drain(&mut job);
        assert_eq!(text(&rows), ["caf\u{e9} latin-1"]);
        assert_eq!(job.encoding, Some(crate::encoding::Encoding::Cp1252));
    }

    #[test]
    fn the_encoding_is_reported_before_any_row() {
        // The buffer needs to know how to save before it has any rows: a file
        // opened and saved immediately must not be rewritten as UTF-8.
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
        // Its rows cannot be found without decoding — U+000A is two bytes — so
        // the streaming reader declines rather than emitting wrong rows. The
        // owner reads it whole instead, which is what such a file needs at any
        // size.
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
