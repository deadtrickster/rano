//! Counting a file's lines without reading them into anything.
//!
//! A tail starts at row 0 of *what we read* (TODO.md §20.1's idea 3), so its
//! row 1 is a screenful near the end of a file whose size nobody has measured.
//! The number that fixes it is one integer: **how many newlines there are in
//! the bytes before the tail begins**. One pass over those bytes gets it — the
//! same pass at the same ~2 GB/s §20.9 measured, so about a second for a 2 GiB
//! log — and nothing is decoded, nothing is held, and nothing is allocated per
//! row.
//!
//! That last part is the whole reason this is a separate job rather than a
//! second [`crate::loader::LoadJob`]. The loader would answer the same question
//! by delivering every row of the file as `Vec<char>` — four bytes per
//! character, one heap allocation per row, which is exactly the per-row cost
//! §20.4's budget exists to refuse. A tail that is not being looked at must not
//! pay 8× a 2 GiB file to learn how many lines it has.
//!
//! It is also the beginning of the sparse line index §16.3 wants, which is why
//! it is written as a scan over bytes from a byte offset rather than as a
//! "count the lines" helper: the next caller wants the same walk with the row
//! *starts* kept.

use std::fs::File;
use std::io::{self, Read};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};

/// Read granularity. One page-cache-friendly read, as [`crate::loader::CHUNK`]
/// is, and for the same reason.
const CHUNK: usize = 64 * 1024;

/// How many bytes between progress reports. Eight of the 64 KiB chunks, so the
/// channel carries one message per half megabyte: often enough that a status
/// line counting up looks alive, rare enough that the sending is not the cost.
const PROGRESS_EVERY: u64 = 8 * CHUNK as u64;

/// What one [`LineCountJob::poll`] found.
#[derive(Debug, PartialEq, Eq)]
pub enum Counted {
    /// Nothing has arrived since the last poll.
    Nothing,
    /// The count so far; the scan is still going.
    Progress(u64),
    /// The scan finished: this many newlines in the bytes it was given.
    Done(u64),
    /// The scan ended without an answer — it was cancelled, or its owner went
    /// away. **Deliberately not a count**: a partial count is not the file's
    /// number of lines, and a caller that installed it would be stating a
    /// wrong number as a fact.
    Stopped,
    /// The file could not be read. The string is for a status line.
    Failed(String),
}

/// One message from the counting thread.
enum Msg {
    Progress(u64),
    Done(u64),
    Failed(String),
}

/// Newlines counted in a file's prefix, on another thread.
pub struct LineCountJob {
    rx: Receiver<Msg>,
    cancel: Arc<AtomicBool>,
    /// Set when `Done`, `Stopped` or `Failed` has been seen, so `poll` stops
    /// asking.
    done: bool,
    /// Newlines counted so far, as of the last `poll`. Starts at 0 and reaches
    /// the answer when `poll` reports `Done`.
    pub counted: u64,
}

impl LineCountJob {
    /// Count the newlines in `path[0..upto)` on a worker.
    ///
    /// The open happens here, on the caller's thread, so an unreadable file
    /// fails now rather than turning into a job that silently never answers.
    /// `upto == 0` is a legitimate request with the answer zero — a tail that
    /// began at the file's start — and is not a case the caller has to avoid.
    ///
    /// `upto` past the end of the file is answered with the count of what is
    /// there, which is what a reader of a file being written wants.
    pub fn spawn(path: PathBuf, upto: u64) -> io::Result<Self> {
        let file = File::open(&path)?;
        let (tx, rx) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let c = Arc::clone(&cancel);
        std::thread::spawn(move || count_prefix(file, upto, &tx, &c));
        Ok(Self {
            rx,
            cancel,
            done: false,
            counted: 0,
        })
    }

    /// Take whatever the worker has said. **Never blocks.**
    ///
    /// `Progress` is returned rather than swallowed so its caller can redraw
    /// the counting number; `self.counted` already holds it either way.
    pub fn poll(&mut self) -> Counted {
        if self.done {
            return Counted::Nothing;
        }
        let mut out = Counted::Nothing;
        loop {
            match self.rx.try_recv() {
                Ok(Msg::Progress(n)) => {
                    self.counted = n;
                    out = Counted::Progress(n);
                }
                Ok(Msg::Done(n)) => {
                    self.counted = n;
                    self.done = true;
                    return Counted::Done(n);
                }
                Ok(Msg::Failed(e)) => {
                    self.done = true;
                    return Counted::Failed(e);
                }
                Err(TryRecvError::Empty) => break,
                // The worker went away without answering. It was cancelled, or
                // its sender was dropped; either way there is no number, and
                // the partial one is not the answer.
                Err(TryRecvError::Disconnected) => {
                    self.done = true;
                    return Counted::Stopped;
                }
            }
        }
        out
    }

    /// Stop the scan at the next chunk boundary. Idempotent, and safe to call
    /// while the worker is mid-read.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

/// The worker: one pass over `[0, upto)`, counting newlines.
///
/// It never seeks, never re-reads, and holds one [`CHUNK`] buffer for the whole
/// run — so its footprint is the same on a 10 KiB file and a 2 GiB one, which
/// is the property `linecount_allocates_nothing_per_row` pins.
///
/// A cancellation sends nothing at all: the caller cancelled because it is not
/// waiting any more, and a message would only race with whatever replaced it.
fn count_prefix(mut file: File, upto: u64, tx: &mpsc::Sender<Msg>, cancel: &AtomicBool) {
    let mut buf = vec![0u8; CHUNK];
    let mut total: u64 = 0;
    let mut read: u64 = 0;
    let mut since_progress: u64 = 0;
    while read < upto {
        if cancel.load(Ordering::Relaxed) {
            return;
        }
        // Never past `upto`: the whole point is to scan a prefix, and reading
        // the rest would be work whose answer nobody asked for.
        let want = ((upto - read) as usize).min(CHUNK);
        let n = match file.read(&mut buf[..want]) {
            // A short file answers with what it has.
            Ok(0) => break,
            Ok(n) => n,
            Err(e) => {
                let _ = tx.send(Msg::Failed(format!("{e}")));
                return;
            }
        };
        total += buf[..n].iter().filter(|b| **b == b'\n').count() as u64;
        read += n as u64;
        since_progress += n as u64;
        if since_progress >= PROGRESS_EVERY {
            since_progress = 0;
            if tx.send(Msg::Progress(total)).is_err() {
                return; // the loop is gone; nothing to deliver to
            }
        }
    }
    let _ = tx.send(Msg::Done(total));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::time::{Duration, Instant};

    /// A scratch file that removes itself.
    ///
    /// The name carries a counter: these tests run beside each other in one
    /// process, and two of them writing `rano_linecount_c.txt` at once is a
    /// fixture one test reads as the other's.
    struct Temp(PathBuf);
    impl Temp {
        fn new(name: &str, contents: &[u8]) -> Self {
            static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            let n = N.fetch_add(1, Ordering::Relaxed);
            let p = std::env::temp_dir().join(format!("rano_linecount_{n}_{name}"));
            std::fs::write(&p, contents).expect("write fixture");
            Temp(p)
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

    /// Poll until the scan ends, with a deadline. Returns the final outcome.
    fn drain(job: &mut LineCountJob) -> Counted {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            assert!(Instant::now() < deadline, "the count did not finish");
            match job.poll() {
                Counted::Nothing | Counted::Progress(_) => {
                    std::thread::sleep(Duration::from_millis(1))
                }
                other => return other,
            }
        }
    }

    fn counted(contents: &[u8], upto: u64) -> u64 {
        let t = Temp::new("c.txt", contents);
        let mut job = LineCountJob::spawn(t.path().to_path_buf(), upto).expect("spawn");
        match drain(&mut job) {
            Counted::Done(n) => n,
            other => panic!("expected a count, got {other:?}"),
        }
    }

    #[test]
    fn it_counts_the_newlines() {
        assert_eq!(counted(b"one\ntwo\nthree\n", 14), 3);
    }

    /// No trailing newline: the last row is still a row, but it has no newline
    /// of its own. This is the off-by-one worth pinning, because the caller
    /// adds 1 to get a 1-based line number.
    #[test]
    fn a_file_without_a_trailing_newline_counts_its_rows() {
        assert_eq!(counted(b"one\ntwo\nthree", 13), 2);
    }

    #[test]
    fn an_empty_file_is_zero() {
        assert_eq!(counted(b"", 0), 0);
    }

    /// **Only the prefix is scanned.** The caller asks for the bytes before the
    /// tail, and the rows after it are already in the buffer — scanning them
    /// would be reading the file twice.
    #[test]
    fn only_the_prefix_is_counted() {
        let body = b"a\nb\nc\nd\ne\n";
        assert_eq!(counted(body, 0), 0);
        assert_eq!(counted(body, 2), 1, "one newline in [0,2)");
        assert_eq!(counted(body, 4), 2);
        assert_eq!(counted(body, body.len() as u64), 5);
    }

    /// A tail offset is always one past a newline, so the bytes it is given end
    /// with one — and the count is then exactly the rows above the tail.
    #[test]
    fn a_prefix_ending_in_a_newline_counts_whole_rows() {
        let body = b"a\nb\nc\n";
        // Offset 4 is the start of the third row; two rows are above it.
        assert_eq!(counted(body, 4), 2);
    }

    #[test]
    fn crlf_newlines_count_the_same() {
        assert_eq!(counted(b"one\r\ntwo\r\n", 10), 2);
    }

    /// `upto` past the end is not an error: it answers for what is there, which
    /// is what a reader of a file somebody is still writing needs.
    #[test]
    fn a_prefix_longer_than_the_file_answers_for_the_file() {
        assert_eq!(counted(b"a\nb\n", 1_000_000), 2);
    }

    #[test]
    fn a_missing_file_fails_at_spawn_not_later() {
        let missing = std::env::temp_dir().join("rano_linecount_there_is_no_such_file");
        let _ = std::fs::remove_file(&missing);
        assert!(LineCountJob::spawn(missing, 10).is_err());
    }

    /// A cancelled scan reports `Stopped`, never a partial count: a partial
    /// count installed as the answer would be a wrong line number stated as a
    /// fact, which is worse than no number at all.
    #[test]
    fn a_cancelled_scan_answers_stopped_and_not_a_partial_count() {
        let body = b"a line of about forty bytes, give or take\n".repeat(40_000);
        let t = Temp::new("cancel.txt", &body);
        // The fixture really has a count to report, so `Stopped` below is a
        // distinction and not an accident of it being empty.
        assert_eq!(body.iter().filter(|b| **b == b'\n').count(), 40_000);
        let mut job =
            LineCountJob::spawn(t.path().to_path_buf(), body.len() as u64).expect("spawn");
        job.cancel();
        let deadline = Instant::now() + Duration::from_secs(10);
        let outcome = loop {
            assert!(Instant::now() < deadline, "the cancelled scan never ended");
            match job.poll() {
                Counted::Nothing | Counted::Progress(_) => {
                    std::thread::sleep(Duration::from_millis(1))
                }
                other => break other,
            }
        };
        assert_eq!(outcome, Counted::Stopped);
    }

    /// **The footprint does not depend on the file.**
    ///
    /// What this test pins is the behaviour: a log of 200 000 rows is counted
    /// correctly in one forward pass. What it does NOT pin is the memory, and
    /// saying otherwise would be worse than saying nothing — the property is
    /// *structural* (`count_prefix` allocates one [`CHUNK`] buffer, once, for
    /// the whole run), and a memory assertion in a test binary would have to
    /// measure the whole process, which every test running beside this one
    /// also allocates into. An 8-byte-per-character implementation of this
    /// would fail the count above on no machine, so the shape is the argument
    /// and the comment is where it is written down.
    #[test]
    fn a_large_file_is_counted_in_one_pass() {
        let body = b"2026-10-09T12:00:00 INFO a row of a log\n".repeat(200_000);
        assert!(body.len() > 6 * 1024 * 1024, "the fixture must be big");
        let t = Temp::new("big.txt", &body);
        let mut job =
            LineCountJob::spawn(t.path().to_path_buf(), body.len() as u64).expect("spawn");
        let t0 = Instant::now();
        let outcome = drain(&mut job);
        let dt = t0.elapsed();
        assert_eq!(outcome, Counted::Done(200_000));
        // Generous, and stated as what it is: this is 6 MiB read through the
        // page cache, and the point is the order — seconds, not minutes, for a
        // file whose rows are never built.
        assert!(dt.as_secs() < 5, "counting 6 MiB took {dt:?}");
    }
}
