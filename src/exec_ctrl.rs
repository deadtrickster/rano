//! Execute (^T, D7) and filter (M-|, F6): run external commands and splice
//! their output back as undoable edits, on top of exec::ExecJob.

use std::io::Write as _;
use std::process::{Command, Stdio};

use crate::buffer::Pos;
use crate::editor::{ActionKind, Editor, normalize, plural};
use crate::exec;
use crate::prompt::{Prompt, PromptKind};

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
        let input: String = self.bs().buf.lines[a.row..=b.row]
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
        self.bs_mut().buf.lines.splice(a.row..=b.row, lines);
        self.bs_mut().cursor = Pos { row: a.row, col: 0 };
        self.bs_mut().mark = None;
        self.finish_step();
        self.edit_invalidate();
        self.flash(&format!("Filtered {} line{}", n, plural(n)));
    }
}
