//! Async command execution (D7), backing ^T / F6 Execute. The command runs
//! under `sh -c` with piped stdout/stderr and a reader thread per pipe, so
//! the editor's event loop only ever polls — never blocks on the child.

use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};

pub struct ExecJob {
    pub child: Child,
    /// Full stdout, sent by the reader thread.
    pub rx: Receiver<String>,
    /// Full stderr, sent by the reader thread.
    pub err_rx: Receiver<String>,
    pub cmd: String,
    /// Row index (in the spawning buffer) where the output is inserted.
    pub insert_row: usize,
}

/// Spawn `sh -c cmd`. Stdin is null; stdout and stderr are piped and drained
/// to completion by one thread each, which then sends the whole text (even
/// when empty) — exactly one message per channel, always.
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
    let rx = spawn_reader(out);
    let err_rx = spawn_reader(err);
    Ok(ExecJob {
        child,
        rx,
        err_rx,
        cmd: cmd.to_string(),
        insert_row,
    })
}

fn spawn_reader<R: Read + Send + 'static>(pipe: Option<R>) -> Receiver<String> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(mut p) = pipe {
            let _ = p.read_to_end(&mut buf);
        }
        let _ = tx.send(String::from_utf8_lossy(&buf).into_owned());
    });
    rx
}

impl ExecJob {
    /// Poll the job. None while the child is still running; once it has
    /// exited, (exit code, stdout, stderr). The `recv`s cannot block
    /// forever: the child's exit closes the pipes, each reader hits EOF and
    /// sends exactly once. (A stray grandchild inheriting a pipe can delay
    /// delivery, exactly as `wait_with_output` would.)
    pub fn try_finish(&mut self) -> Option<(Option<i32>, String, String)> {
        let status = self.child.try_wait().ok().flatten()?;
        let out = self.rx.recv().unwrap_or_default();
        let err = self.err_rx.recv().unwrap_or_default();
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
}
