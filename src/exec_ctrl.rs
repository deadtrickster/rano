//! Execute (^T, D7) and filter (M-|, F6): run external commands and splice
//! their output back as undoable edits, on top of exec::ExecJob.

use crate::buffer::Pos;
use crate::editor::{ActionKind, Editor, normalize, plural};
use crate::exec;
use crate::prompt::{Prompt, PromptKind};
use std::io::Write as _;
use std::process::{Command, Stdio};

impl Editor {
    // ---------- exec ----------

    pub(crate) fn start_exec(&mut self) {
        self.prompt = Some(Prompt {
            kind: PromptKind::Exec,
            text: String::new(),
            cursor: 0,
        });
    }

    pub(crate) fn do_exec(&mut self, cmd: &str) {
        if cmd.trim().is_empty() {
            return;
        }
        match exec::spawn_job(cmd, self.bs().cursor.row + 1) {
            Ok(job) => {
                self.bs_mut().exec_job = Some(job);
                self.flash(&format!("Running: {}", cmd));
            }
            Err(e) => {
                self.flash(&format!("Error: {}", e));
            }
        }
    }

    /// Poll the active exec job. On completion, insert the stdout below the
    /// spawn row as ONE undo step, or flash the failure (no undo step, no
    /// edit invalidation). Returns whether state changed, for a later
    /// dirty-draw pass.
    pub(crate) fn exec_poll(&mut self) -> bool {
        let Some(job) = self.bs_mut().exec_job.as_mut() else {
            return false;
        };
        let Some((code, stdout, stderr)) = job.try_finish() else {
            return false;
        };
        let job = self.bs_mut().exec_job.take().expect("job held above");
        let cmd = job.cmd;
        if code == Some(0) {
            if !stdout.trim().is_empty() {
                let mut lines: Vec<Vec<char>> =
                    stdout.split('\n').map(|l| l.chars().collect()).collect();
                if stdout.ends_with('\n') {
                    lines.pop();
                }
                let row = job.insert_row;
                // empty before-region: pure insertion after the spawn row
                self.begin_action(ActionKind::Exec, row, row);
                self.bs_mut().buf.insert_lines_at(row, lines);
                self.bs_mut().cursor = Pos { row, col: 0 };
                self.finish_step();
                self.edit_invalidate();
            }
            self.flash(&format!("Ran: {}", cmd));
        } else {
            let mut msg = format!("Exit code {}", code.unwrap_or(-1));
            if let Some(line) = stderr.trim().lines().next()
                && !line.is_empty()
            {
                msg.push_str(": ");
                msg.push_str(line);
            }
            self.flash(&msg);
        }
        true
    }

    // ---------- filter ----------

    pub(crate) fn start_filter(&mut self) {
        if self.sel_span().is_none() {
            self.flash("No selection");
            return;
        }
        self.prompt = Some(Prompt {
            kind: PromptKind::FilterCmd,
            text: String::new(),
            cursor: 0,
        });
    }

    /// M-|: pipe the selected whole rows through `sh -c cmd` and splice the
    /// output back. Failure (spawn or non-zero exit) touches nothing.
    pub(crate) fn do_filter(&mut self, cmd: &str) {
        let cmd = cmd.trim();
        if cmd.is_empty() {
            return;
        }
        let mark = self.bs().mark.expect("start_filter guarantees mark");
        let (a, b) = normalize(mark, self.bs().cursor);
        let input: String = self
            .bs()
            .buf
            .rows_range(a.row, b.row + 1)
            .iter()
            .map(|l| l.iter().collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        // Writing inline before reading stdout deadlocks past the 64K pipe
        // buffer, so the stdin writer runs on its own thread.
        let child = Command::new("sh")
            .arg("-c")
            .arg(cmd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn();
        let mut child = match child {
            Ok(c) => c,
            Err(e) => {
                self.flash(&format!("Error: {}", e));
                return;
            }
        };
        let mut stdin = child.stdin.take().expect("piped stdin");
        std::thread::spawn(move || {
            let _ = stdin.write_all(input.as_bytes());
        });
        let out = match child.wait_with_output() {
            Ok(o) => o,
            Err(e) => {
                self.flash(&format!("Error: {}", e));
                return;
            }
        };
        let code = out.status.code().unwrap_or(-1);
        if code != 0 {
            let err = String::from_utf8_lossy(&out.stderr);
            let mut msg = format!("Exit code {}", code);
            if let Some(line) = err.trim().lines().next()
                && !line.is_empty()
            {
                msg.push_str(": ");
                msg.push_str(line);
            }
            self.flash(&msg);
            return;
        }
        let text = String::from_utf8_lossy(&out.stdout);
        let mut lines: Vec<Vec<char>> = text.split('\n').map(|l| l.chars().collect()).collect();
        if text.ends_with('\n') {
            lines.pop();
        }
        if lines.is_empty() {
            lines.push(Vec::new());
        }
        let n = lines.len();
        self.begin_action(ActionKind::Filter, a.row, b.row + 1);
        self.bs_mut().buf.splice_rows(a.row, b.row + 1, lines);
        self.bs_mut().cursor = Pos { row: a.row, col: 0 };
        self.bs_mut().mark = None;
        self.finish_step();
        self.edit_invalidate();
        self.flash(&format!("Filtered {} line{}", n, plural(n)));
    }
}

/// **Run a program over text, bounded.** `input` goes to its stdin, its stdout comes back,
/// and a wall clock bounds the whole thing — this sits on the SAVE path (gofmt on a Go
/// buffer), where a child that never answers would be an editor that never saves.
///
/// A different shape from [`Editor::do_filter`], which is the interactive `M-|`: that one
/// goes through `sh -c` on its own thread and splices into a marked region of the buffer;
/// this one runs a program directly with an argv and hands its output back to the caller.
///
/// Both pipes are drained by their own threads: writing first and reading afterwards
/// deadlocks on a 64 KiB pipe, which is the same trap `do_filter` records one paragraph up.
pub(crate) fn run_filter(
    program: &str,
    args: &[&str],
    input: &str,
    timeout: std::time::Duration,
) -> Result<String, String> {
    use std::io::Read as _;
    use std::sync::mpsc;

    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("{program}: {e}"))?;
    let drain = |pipe: Option<Box<dyn std::io::Read + Send>>| {
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(mut p) = pipe {
                let _ = p.read_to_end(&mut buf);
            }
            let _ = tx.send(buf);
        });
        rx
    };
    let out_rx = drain(
        child
            .stdout
            .take()
            .map(|p| Box::new(p) as Box<dyn std::io::Read + Send>),
    );
    let err_rx = drain(
        child
            .stderr
            .take()
            .map(|p| Box::new(p) as Box<dyn std::io::Read + Send>),
    );
    let text = input.to_string();
    if let Some(mut stdin) = child.stdin.take() {
        std::thread::spawn(move || {
            let _ = stdin.write_all(text.as_bytes());
        });
    }
    let deadline = std::time::Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) => {}
            Err(e) => break Err(format!("{program}: {e}")),
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            break Err(format!("timed out after {timeout:?}"));
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    };
    let out = String::from_utf8_lossy(&out_rx.recv().unwrap_or_default()).into_owned();
    let err = String::from_utf8_lossy(&err_rx.recv().unwrap_or_default()).into_owned();
    let status = status?;
    if !status.success() {
        let first = err.trim().lines().next().unwrap_or("").to_string();
        return Err(if first.is_empty() {
            format!("exit {}", status.code().unwrap_or(-1))
        } else {
            format!("exit {}: {first}", status.code().unwrap_or(-1))
        });
    }
    Ok(out)
}
