//! Async command execution, backing ^T / F6 Execute: `sh -c` with piped stdout
//! and stderr and a reader thread per pipe, so the loop only ever polls.

use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

/// How long the pipes get after the child exits before what arrived is taken as all of
/// it: a grandchild can hold a pipe open for minutes.
const READER_GRACE: Duration = Duration::from_millis(200);

/// One pipe being drained by a reader thread: the bytes so far and whether it has closed.
struct Pipe {
    rx: Receiver<Vec<u8>>,
    bytes: Vec<u8>,
    closed: bool,
}

impl Pipe {
    /// Take whatever has arrived, never dropping a chunk that lands mid-poll; `closed`
    /// once the reader thread has gone.
    fn poll(&mut self) {
        loop {
            match self.rx.try_recv() {
                Ok(chunk) => self.bytes.extend_from_slice(&chunk),
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.closed = true;
                    break;
                }
            }
        }
    }

    fn take_text(&mut self) -> String {
        String::from_utf8_lossy(&std::mem::take(&mut self.bytes)).into_owned()
    }
}

pub struct ExecJob {
    pub child: Child,
    /// stdout and stderr, drained as they arrive so a held pipe is not a hang.
    out: Pipe,
    err: Pipe,
    pub cmd: String,
    /// Row index (in the spawning buffer) where the output is inserted.
    pub insert_row: usize,
    /// When the child was first seen to have exited; the reader grace runs from here.
    exited: Option<Instant>,
}

/// Spawn `sh -c cmd`. Stdin is null; stdout and stderr each get a thread that drains
/// its pipe and sends the bytes on as it reads them.
pub fn spawn_job(cmd: &str, insert_row: usize) -> std::io::Result<ExecJob> {
    let mut child = Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let out = child.stdout.take();
    let err = child.stderr.take();
    let out = Pipe {
        rx: spawn_reader(out),
        bytes: Vec::new(),
        closed: false,
    };
    let err = Pipe {
        rx: spawn_reader(err),
        bytes: Vec::new(),
        closed: false,
    };
    Ok(ExecJob {
        child,
        out,
        err,
        cmd: cmd.to_string(),
        insert_row,
        exited: None,
    })
}

fn spawn_reader<R: Read + Send + 'static>(pipe: Option<R>) -> Receiver<Vec<u8>> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        if let Some(mut p) = pipe {
            while let Ok(n) = p.read(&mut buf) {
                if n == 0 || tx.send(buf[..n].to_vec()).is_err() {
                    break;
                }
            }
        }
    });
    rx
}

impl ExecJob {
    /// Poll the job: None while the child runs, else (exit code, stdout, stderr).
    /// Never blocks: a pipe still held open after the child exits gets `READER_GRACE`.
    pub fn try_finish(&mut self) -> Option<(Option<i32>, String, String)> {
        let status = self.child.try_wait().ok().flatten()?;
        let exited = *self.exited.get_or_insert_with(Instant::now);
        self.out.poll();
        self.err.poll();
        if !(self.out.closed && self.err.closed) && exited.elapsed() < READER_GRACE {
            return None;
        }
        let out = self.out.take_text();
        let err = self.err.take_text();
        Some((status.code(), out, err))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn wait_finish(job: &mut ExecJob) -> (Option<i32>, String, String) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(r) = job.try_finish() {
                return r;
            }
            assert!(Instant::now() < deadline, "job did not finish in time");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn spawn_captures_stdout() {
        let mut job = spawn_job("printf hi", 0).expect("spawn printf");
        let (code, out, _err) = wait_finish(&mut job);
        assert_eq!(code, Some(0));
        assert_eq!(out, "hi");
        assert_eq!(job.cmd, "printf hi");
        assert_eq!(job.insert_row, 0);
    }

    #[test]
    fn spawn_captures_stderr() {
        let mut job = spawn_job("echo boom 1>&2", 0).expect("spawn echo");
        let (code, _out, err) = wait_finish(&mut job);
        assert_eq!(code, Some(0));
        assert!(err.contains("boom"), "stderr was: {:?}", err);
    }

    #[test]
    fn try_finish_none_while_running() {
        let mut job = spawn_job("sleep 0.2", 0).expect("spawn sleep");
        assert!(job.try_finish().is_none());
        let (code, _out, _err) = wait_finish(&mut job);
        assert_eq!(code, Some(0));
    }

    #[test]
    fn failing_command_reports_code() {
        let mut job = spawn_job("false", 0).expect("spawn false");
        let (code, out, _err) = wait_finish(&mut job);
        assert_eq!(code, Some(1));
        assert!(out.is_empty());
    }

    #[test]
    fn many_chunks_all_arrive() {
        // The reader sends a message per read: a poll that drops one loses output.
        let mut job = spawn_job("seq 1 200000", 0).expect("spawn seq");
        let (code, out, _err) = wait_finish(&mut job);
        assert_eq!(code, Some(0));
        assert_eq!(out.lines().count(), 200_000, "every line arrived");
        assert!(out.ends_with("200000\n"), "the last line is there");
    }

    #[test]
    fn a_grandchild_holding_the_pipe_does_not_hold_the_finish() {
        // `sleep 3` keeps the pipes open after `sh` exits; the grace ends the job.
        let mut job = spawn_job("sleep 3 & echo hi", 0).expect("spawn");
        let started = Instant::now();
        let (code, out, _err) = wait_finish(&mut job);
        assert_eq!(code, Some(0));
        assert_eq!(out.trim(), "hi");
        // The claim is that the finish does not wait for the pipe holder, and the pipe holder sleeps
        // three seconds — so two is a bound on the wrong shape and not on a loaded machine's spawn
        // and read. At one second this went red for a busier host, not for a job that waited.
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "took {:?}",
            started.elapsed()
        );
    }
}
