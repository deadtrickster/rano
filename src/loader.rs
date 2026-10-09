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
use std::io::{self, Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::Duration;

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

/// How many rows a tail (`-f`/`--follow`) opens with.
///
/// Several screens at any realistic terminal height — `Editor::HIGHLIGHT_MARGIN`
/// is 200 rows for the same reason — and deliberately fewer than one [`BATCH`],
/// so the whole tail lands in the first poll rather than arriving in lumps.
///
/// This is the number of rows the view STARTS with, not the number it keeps:
/// TODO.md §20.4's budget is what makes a tail nobody is looking at cheap, and
/// that is a later increment than the open.
pub const TAIL_ROWS: usize = 200;

/// The first window [`tail_offset`] reads, widened 4x until it holds
/// [`TAIL_ROWS`] rows.
///
/// One [`CHUNK`]: page-cache friendly, and more than enough for the common log
/// shape, so the widening loop does not run at all. Its COST is what matters —
/// a 2 GiB log opens for this much reading and not the file's.
pub const TAIL_WINDOW: u64 = CHUNK as u64;

/// A buffer opened at the tail of a file: `-f`/`--follow`, and later a `.log`
/// name (TODO.md §20.7).
///
/// It exists to remember the one number the rest of the mode is defined
/// against, §20.1's idea 2: **the size at open**. Below it, the file was there
/// before we were, so a row can be numbered, styled and searched like any other
/// file's. At or above it, the rows are still arriving, and are neither styled
/// nor counted until their newline lands — which is what makes "settled" a fact
/// about the file rather than a guess about the user's patience.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tail {
    /// The file's length when it was opened.
    ///
    /// Read by no one yet: increment E (settle-before-styling) is its first
    /// reader, and increment D (following the growth) is its second.
    #[allow(dead_code)]
    pub size_at_open: u64,
    /// **The boundary styling stops at**, in buffer rows: everything below this
    /// index arrived after the tail was read, so it is a row the file is still
    /// writing and must not be coloured (TODO.md §20.1's idea 4, §20.3 E).
    ///
    /// A ROW COUNT rather than a byte offset, and that is not a shortcut: the
    /// buffer holds `Vec<char>` rows and has no per-row byte index, so "rows at
    /// or after byte `size_at_open`" is not a question it can answer — but
    /// "rows appended after the tail read" is exactly the same set, and the
    /// moment it becomes known is [`crate::loader::Adopted::CaughtUp`].
    ///
    /// `None` until that moment, which is honest rather than empty: while the
    /// initial read is still arriving the boundary is unknown, and the rows in
    /// hand are precisely the ones that were in the file before us.
    pub rows_at_open: Option<usize>,
}

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
    ///
    /// Sent once by a reading job and never by a following one: a follower has
    /// no end, and this would be a message about a file that has one.
    Done { crlf: bool },
    /// **Everything that exists has been read** — sent once by a follower when
    /// it first reaches the end, and never by a plain reader (for which that is
    /// `Done`). The rows in the buffer are settled at this moment; what arrives
    /// after it is the file growing.
    CaughtUp { crlf: bool },
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
    /// Rows were appended and the follower has read everything that exists; the
    /// job is NOT over — more will arrive, which is what following means.
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
    /// Set when `CaughtUp` has been seen: this job is a follower that has read
    /// everything the file had, and is waiting for it to grow.
    ///
    /// It is what tells "a read in flight" (a frozen-looking screen) apart from
    /// "a follower with nothing to do" (an ordinary, finished view), which are
    /// the same `Some(job)` in [`crate::BufferState::load`].
    caught_up: bool,
    /// Rows handed out so far, for the status line.
    pub rows_read: usize,
    /// The encoding the reader decided, once it has. `None` until the first
    /// chunk has been sniffed.
    pub encoding: Option<crate::encoding::Encoding>,
}

impl LoadJob {
    /// Start reading `path` from its beginning.
    pub fn spawn(path: PathBuf) -> io::Result<Self> {
        Self::spawn_from(path, 0)
    }

    /// Start reading `path` from byte `from` onward — the tail path, paired
    /// with [`tail_offset`].
    ///
    /// The seek happens HERE, on the caller's thread and before the worker
    /// exists, for the same reason the open does: a position that could not be
    /// taken is the caller's problem to report now, not a status line after a
    /// frame has been drawn.
    ///
    /// `from` is also what tells the reader whether the bytes it sees first are
    /// the file's start, which is a real difference and not a formality: at 0 a
    /// byte-order mark is a mark and is skipped, and at a tail offset the same
    /// three bytes are content.
    pub fn spawn_from(path: PathBuf, from: u64) -> io::Result<Self> {
        Self::open(path, from, false)
    }

    /// Start reading `path` from `from` **and keep reading it as it grows** —
    /// `-f`/`--follow` (TODO.md §20.3 D).
    ///
    /// The reader is the same one, with two differences that are the whole of
    /// following: at the end of the file it waits and looks again instead of
    /// finishing, and it says [`Adopted::CaughtUp`] once when it first gets
    /// there rather than [`Adopted::Finished`].
    pub fn spawn_follow_from(path: PathBuf, from: u64) -> io::Result<Self> {
        Self::open(path, from, true)
    }

    fn open(path: PathBuf, from: u64, follow: bool) -> io::Result<Self> {
        // Open here, on the caller's thread: a missing or unreadable file is
        // the caller's problem to report *now*, not a message that arrives
        // after a frame has already been drawn.
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

    /// Whether this job is a follower that has read everything the file had.
    ///
    /// A follower with nothing to do is a finished view, not a read in flight:
    /// it must not hold the frame at [`crate::host`]'s loading cadence, and it
    /// must not keep saying "Reading…" over the status line for ever.
    pub fn caught_up(&self) -> bool {
        self.caught_up
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
                Ok(LoadMsg::CaughtUp { crlf }) => {
                    // NOT done: everything so far has been read, and the point
                    // of a follower is that there is more to come.
                    self.caught_up = true;
                    let took = out.len() - before;
                    self.rows_read += took;
                    return Adopted::CaughtUp { rows: took, crlf };
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

/// Where the tail of a file begins: the byte offset of the first of its last
/// `rows` rows.
///
/// **Scans backwards from the end, so the cost is the tail and not the file.**
/// That is the whole point for a log: a 2 GiB file opens showing its last
/// screenful for a read of a few hundred KiB, where reading from the start to
/// find it costs the whole file. `bench_cold_open`'s 37 µs for a first
/// screenful is the precedent — this is the same idea applied to the other end.
///
/// The window widens by 4x until it holds more than `rows` newlines or reaches
/// the start of the file, so a file of one enormous row (a stack trace with no
/// breaks, an unterminated line) is correct rather than special — it just reads
/// more to get there, and reports 0 because that row's start IS the file's.
///
/// Returns `size` when there is nothing to show, so a caller can read from it
/// and get nothing.
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

        // Row starts in this window, absolute. A start is the file's beginning,
        // or one past a newline — and never past EOF, because a trailing newline
        // does not begin a row.
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
            // row of the window. A partial first row is skipped by construction,
            // because we only ever return a position one past a newline.
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
///
/// Split from the offset so the two can be tested apart: this is the read, and it
/// is deliberately a plain forward read from a byte the caller already has.
/// [`crate::logtail::LogTail::last_rows`] is its caller — the pane that draws the
/// end of a file it never holds.
pub fn read_from(file: &mut File, from: u64) -> io::Result<Vec<u8>> {
    file.seek(SeekFrom::Start(from))?;
    let mut out = Vec::new();
    file.read_to_end(&mut out)?;
    Ok(out)
}

/// How long a follower waits at the end of the file before looking again.
///
/// A poll rather than an inotify watch, deliberately: one syscall per tenth of
/// a second costs nothing next to a frame, and it does not need a descriptor, a
/// platform, or a story for the case where the file is replaced rather than
/// appended to. `tail -f` looks cleverer and is the same mechanism underneath —
/// including the one consequence worth naming: **a file that is replaced rather
/// than appended to is not noticed**, because the descriptor is still the old
/// one. (`tail -F` follows the NAME; that is a different feature and is not
/// this one.)
const FOLLOW_SLEEP: Duration = Duration::from_millis(100);

/// Send `batch` as a `Rows` message, leaving it empty. Returns whether the loop
/// it was sent to is still there.
///
/// A separate function because there are three moments to send: the batch is
/// full, and — for a follower — the end of the file was reached, which is the
/// one a plain read does not have.
fn send_batch(tx: &mpsc::Sender<LoadMsg>, batch: &mut Vec<Vec<char>>) -> bool {
    if batch.is_empty() {
        return true;
    }
    tx.send(LoadMsg::Rows(std::mem::take(batch))).is_ok()
}

/// The reader thread: one pass, decoding rows as their newlines arrive.
///
/// It never re-reads and never holds the whole file: `pending` is one partial
/// row, and a row is sent as soon as it is complete.
///
/// `from` is where the file was positioned before the thread started, and it is
/// the only difference between reading a file and reading its tail: the loop is
/// the same, and so is every message it sends.
///
/// `follow` is the second difference, and it is two lines of it: at the end of
/// the file a follower waits and looks again instead of finishing, and it says
/// `CaughtUp` once instead of `Done`. **`pending` is deliberately NOT flushed in
/// follow mode** — a row the file has not finished writing is not a row, and
/// emitting it would put half a stack trace on screen as if it were whole
/// (TODO.md §20.5) and give §20.3 E a row to style that is about to change.
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
    // The first chunk decides the encoding, from the same 64 KiB prefix the
    // ladder is measured at (TODO.md §13.5: 8 µs against 46 ms for a
    // whole-file scan, agreeing with the whole-file answer on every shape).
    let mut encoding: Option<Encoding> = None;
    let mut first = true;
    let mut first_chunk_bom = false;
    let mut said_caught_up = false;
    loop {
        if cancel.load(Ordering::Relaxed) {
            return;
        }
        let n = match file.read(&mut buf) {
            Ok(0) => {
                if !follow {
                    break;
                }
                // **Flush the whole rows FIRST** — before the message that says
                // this is all of them, and before waiting.
                //
                // A follower never reaches the end of `read_all`, and the end of
                // the file is what flushes the last partial batch for a plain
                // read. Without this, a file that grew by less than
                // [`BATCH`] rows would show NOTHING at all: the rows would sit
                // in `batch` for ever. (Written here because the first version
                // of this had exactly that bug, and a followed file simply
                // stayed empty.)
                if !send_batch(tx, &mut batch) {
                    return;
                }
                // Everything that exists has been read. Say so ONCE — this is
                // "the file so far", not a per-growth event — and then wait for
                // it to grow, holding `pending` (see this function's note on why
                // a partial row is not emitted).
                //
                // Once is enough because in follow mode every row that reaches
                // the buffer ended in a newline, so unlike a plain read there is
                // no unsettled tail in it: the file has no end and every row in
                // hand is one the file finished writing.
                if !said_caught_up {
                    said_caught_up = true;
                    // `crlf` as of this moment. A file that switches convention
                    // AFTER it is caught up does not update it — and does not
                    // need to: a tail is read-only, so the flag never reaches a
                    // write (TODO.md §20.6).
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
            // Only the file's own beginning can be a byte-order mark. A tail
            // read starts mid-file, where those same three bytes (EF BB BF) are
            // a character somebody wrote, so they must not be eaten.
            first_chunk_bom = from == 0;
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
        // Offset of the first of the last `rows` rows.
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

    /// **The property: the backward scan agrees with reading the file forwards.**
    /// Driven over the shapes where the off-by-ones live — trailing newline or
    /// not, fewer rows than asked, exactly as many, a lone enormous row, CRLF —
    /// and over a range of `rows`, against a reference that reads everything.
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

    /// And the bytes from that offset are the last `rows` rows, exactly — the
    /// offset is only useful if reading from it gives what was asked for.
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

    /// **The offset and the reader meet.** `spawn_from` at a tail offset gives
    /// the file's last rows and NOT the whole file — which is the entire point
    /// of the pair, and the thing a wrong seek or a wrong offset would hide as
    /// a plausible-looking tail.
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

    /// **The cost is the tail, not the file.** A file far larger than the window
    /// still answers immediately, because the scan goes backwards from the end
    /// and never looks at the beginning.
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
        // Generous: this is a 12 MB file and the scan reads 64 KiB of it. The
        // point is that it is not proportional to the file, so a bound well
        // under a full read is the assertion.
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
                Adopted::NeedsEager(e) => panic!("unexpected eager path: {}", e.name()),
                Adopted::Rows(n) => assert_eq!(n, rows.len() - before, "count matches"),
                Adopted::Finished { rows: n, crlf: c } => {
                    assert_eq!(n, rows.len() - before, "count matches");
                    crlf = c;
                    break;
                }
                // A `spawn_from` job ends rather than catching up; this helper is
                // for those, and a follower would hang it rather than reach here.
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

    /// **A follower appends what arrives and holds back a half-written row.**
    ///
    /// Driven against a real file that grows under it, because the two things
    /// that can be wrong here are facts about a file rather than about types: a
    /// partial row shown as if it were whole (§20.5's half a stack trace), and a
    /// row lost between the first read and the first append.
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

        // **A row the file has not finished writing is not a row.** "thr" has
        // no newline after it, so it must NOT appear — the whole point of
        // holding `pending` back rather than flushing it at the end.
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

        // Its newline arrives and the row does, with nothing dropped.
        f.write_all(b"ee\n").expect("write");
        f.flush().expect("flush");
        let deadline = Instant::now() + Duration::from_secs(5);
        while rows.len() < 3 {
            assert!(Instant::now() < deadline, "the growth never arrived");
            let _ = job.poll(&mut rows);
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(text(&rows), ["one", "two", "three"]);

        // A burst of several rows arrives in order, which is the reason the
        // reader appends rather than re-reads.
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

    /// A follower that is caught up is not a read in flight: `loading` says so,
    /// so the loop waits at its idle cadence and the status line stops saying
    /// "Reading…" over everything else (§20.3 D).
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
        // And it stays a follower: polling after the catch-up is not an end.
        assert_eq!(job.poll(&mut rows), Adopted::Nothing);
        job.cancel();
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

    /// **A byte-order mark in the middle of a file is content.** Only the
    /// file's own first bytes can be a mark — a tail read starts wherever
    /// `tail_offset` says, and if that happens to be a row beginning with
    /// EF BB BF, those bytes are a character somebody wrote. Eating them would
    /// silently corrupt one line out of the middle of a log, which is exactly
    /// the kind of bug a tail is prone to.
    #[test]
    fn a_tail_read_does_not_eat_a_mid_file_byte_order_mark() {
        let mut body = b"first\n".to_vec();
        body.extend_from_slice("\u{feff}".as_bytes());
        body.extend_from_slice(b"second\n");
        let t = Temp::new("midbom.txt", &body);
        // Byte 6 is the row that begins with the mark.
        let mut job = LoadJob::spawn_from(t.path().to_path_buf(), 6).expect("spawn_from");
        let (rows, _, _) = drain(&mut job);
        assert_eq!(text(&rows), ["\u{feff}second"], "the mark was eaten");
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
