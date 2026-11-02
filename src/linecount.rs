//! Counting a file's lines without reading them into anything: one pass over
//! the bytes before a tail begins, counting newlines — nothing decoded, held,
//! or allocated per row. A separate job from the loader, which would answer by
//! building every row; a scan from a byte offset because a future line index
//! wants the same walk.

use std::fs::File;
use std::io::{self, Read};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};

/// Read granularity: one page-cache-friendly read, as [`crate::loader::CHUNK`].
const CHUNK: usize = 64 * 1024;

/// Bytes between progress reports: one message per half megabyte.
const PROGRESS_EVERY: u64 = 8 * CHUNK as u64;

/// What one [`LineCountJob::poll`] found.
#[derive(Debug, PartialEq, Eq)]
pub enum Counted {
    Nothing,
    Progress(u64),
    /// The scan finished: this many newlines in the bytes it was given.
    Done(u64),
    /// The scan ended without an answer — cancelled, or its owner went away.
    /// Deliberately not a partial count.
    Stopped,
    /// The file could not be read. The string is for a status line.
    Failed(String),
}

enum Msg {
    Progress(u64),
    Done(u64),
    Failed(String),
}

/// Newlines counted in a file's prefix, on another thread.
pub struct LineCountJob {
    rx: Receiver<Msg>,
    cancel: Arc<AtomicBool>,
    done: bool,
    /// Newlines counted so far; reaches the answer when `poll` reports `Done`.
    pub counted: u64,
}

impl LineCountJob {
    /// Count the newlines in `path[0..upto)` on a worker. The open happens
    /// here, on the caller's thread, so an unreadable file fails now. `upto` of
    /// 0 is legitimate; past the end, the count of what is there.
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

    /// Take whatever the worker has said; never blocks.
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
                // Cancelled, or the sender dropped: the partial count is not
                // the answer.
                Err(TryRecvError::Disconnected) => {
                    self.done = true;
                    return Counted::Stopped;
                }
            }
        }
        out
    }

    /// Stop the scan at the next chunk boundary; idempotent.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

/// The worker: one pass over `[0, upto)`, one buffer for the whole run, so its
/// footprint is the same on a 10 KiB file and a 2 GiB one.
fn count_prefix(mut file: File, upto: u64, tx: &mpsc::Sender<Msg>, cancel: &AtomicBool) {
    let mut buf = vec![0u8; CHUNK];
    let mut total: u64 = 0;
    let mut read: u64 = 0;
    let mut since_progress: u64 = 0;
    while read < upto {
        if cancel.load(Ordering::Relaxed) {
            return;
        }
        // Never past `upto`: reading the rest would be work nobody asked for.
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

    /// A scratch file that removes itself; the counter keeps parallel tests apart.
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

    /// No trailing newline: the last row has no newline of its own; the caller
    /// adds 1.
    #[test]
    fn a_file_without_a_trailing_newline_counts_its_rows() {
        assert_eq!(counted(b"one\ntwo\nthree", 13), 2);
    }

    #[test]
    fn an_empty_file_is_zero() {
        assert_eq!(counted(b"", 0), 0);
    }

    /// Only the prefix: the rows after it are already in the buffer.
    #[test]
    fn only_the_prefix_is_counted() {
        let body = b"a\nb\nc\nd\ne\n";
        assert_eq!(counted(body, 0), 0);
        assert_eq!(counted(body, 2), 1, "one newline in [0,2)");
        assert_eq!(counted(body, 4), 2);
        assert_eq!(counted(body, body.len() as u64), 5);
    }

    /// A tail offset is one past a newline, so the count is exactly the rows
    /// above the tail.
    #[test]
    fn a_prefix_ending_in_a_newline_counts_whole_rows() {
        let body = b"a\nb\nc\n";
        assert_eq!(counted(body, 4), 2);
    }

    #[test]
    fn crlf_newlines_count_the_same() {
        assert_eq!(counted(b"one\r\ntwo\r\n", 10), 2);
    }

    /// `upto` past the end answers for what is there.
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

    /// A cancelled scan answers `Stopped` or the whole count (a race), never a
    /// partial one.
    #[test]
    fn a_cancelled_scan_answers_stopped_or_a_whole_count_and_never_a_partial_one() {
        let body = b"a line of about forty bytes, give or take\n".repeat(40_000);
        let t = Temp::new("cancel.txt", &body);
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
        match outcome {
            Counted::Stopped => {}
            Counted::Done(n) => assert_eq!(n, 40_000, "a whole count, not a partial one"),
            other => panic!("a cancelled scan answered {other:?}"),
        }
    }

    /// Pins the behaviour — a 200k-row log counted in one pass — not the
    /// memory: the footprint is structural (one buffer, once).
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
        // 6 MiB through the page cache; the point is the order, not the number.
        assert!(dt.as_secs() < 5, "counting 6 MiB took {dt:?}");
    }
}
