//! Talking to a host: being told where to go ([`Editor::open_at`]), and telling
//! it where the reader is ([`Editor::send_position`], M-S).
//!
//! Both are in-process calls, the shape embedding needs: a host that owns an
//! `Editor` calls `open_at` and sets `on_send`. The standalone binary is its own
//! host — `file:line:col` on the command line goes through the same positioning,
//! and `on_send` runs the configured `send_command` ([`command_sender`]).

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use rano::send::{Point, Selection, SendEvent};

use crate::buffer::Pos;
use crate::editor::{Editor, canonical, normalize};

/// What a host does with a [`SendEvent`]: `Ok` carries what to tell the reader
/// (shown in the status line), `Err` why it did not work.
pub type OnSend = Box<dyn FnMut(&SendEvent) -> Result<String, String>>;

impl Editor {
    /// Open `path` — or switch to it when a buffer already shows it — and put
    /// the cursor on `line` (1-based), at `column` (1-based, the line's start
    /// when `None`), centred. A file still arriving is positioned when that
    /// row lands, in its own buffer whatever is current by then.
    ///
    /// A new file gets a buffer of its own even without `multibuffer` when the
    /// current one has unsaved edits, so a host's jump can never discard them.
    pub fn open_at(
        &mut self,
        path: &Path,
        line: usize,
        column: Option<usize>,
    ) -> Result<(), String> {
        let at = Pos {
            row: line.saturating_sub(1),
            col: column.unwrap_or(1).saturating_sub(1),
        };
        if let Some(i) = self.find_buffer(path) {
            self.set_current(i);
        } else {
            let keep = self.bs().buf.modified && !self.config.multibuffer;
            if keep {
                self.config.multibuffer = true;
            }
            let opened = self.open_file(&path.display().to_string());
            if keep {
                self.config.multibuffer = false;
            }
            if !opened {
                return Err(self.status_text().unwrap_or_else(|| "cannot open".into()));
            }
        }
        self.bs_mut().goto = Some(at);
        // Now if the row is there; otherwise the loop applies it on arrival.
        self.apply_startup_pos();
        Ok(())
    }

    /// The reader's place: the file (absolute), the cursor, and the marked
    /// region with its text when a mark is set and the region is not empty.
    pub fn send_event(&self) -> SendEvent {
        let bs = self.bs();
        let point = |p: Pos| Point {
            line: p.row + 1,
            column: p.col + 1,
        };
        let selection = bs.mark.and_then(|m| {
            let (a, b) = normalize(m, bs.cursor);
            (a != b).then(|| Selection {
                start: point(a),
                end: point(b),
                text: bs
                    .buf
                    .copy_range(a, b)
                    .iter()
                    .map(|r| r.iter().collect::<String>())
                    .collect::<Vec<_>>()
                    .join("\n"),
            })
        });
        SendEvent {
            path: bs.buf.name.as_deref().map(canonical),
            cursor: point(bs.cursor),
            selection,
            modified: bs.buf.modified,
        }
    }

    /// M-S: hand the reader's place to the host.
    pub(crate) fn send_position(&mut self) {
        let event = self.send_event();
        let Some(send) = self.on_send.as_mut() else {
            self.flash("Nowhere to send: set send_command in the config");
            return;
        };
        match send(&event) {
            Ok(msg) => self.flash(&msg),
            Err(e) => self.flash(&format!("Send failed: {e}")),
        }
    }
}

/// `on_send` for the standalone binary: run `cmd` with `sh -c`, the event as
/// JSON on its stdin and `RANO_FILE` / `RANO_LINE` / `RANO_COLUMN` in its
/// environment (`RANO_FILE` empty for an unnamed buffer).
///
/// The command is not waited for — the editor stays live while it runs — so
/// what is reported is that it started. Its output goes nowhere: the terminal
/// belongs to the editor.
pub fn command_sender(cmd: String) -> OnSend {
    Box::new(move |e: &SendEvent| {
        let mut child = Command::new("sh")
            .arg("-c")
            .arg(&cmd)
            .env(
                "RANO_FILE",
                e.path
                    .as_deref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default(),
            )
            .env("RANO_LINE", e.cursor.line.to_string())
            .env("RANO_COLUMN", e.cursor.column.to_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|err| format!("{cmd}: {err}"))?;
        let json = e.to_json();
        let mut stdin = child.stdin.take().expect("piped");
        // Off the main thread, and reaped there: a command that reads slowly
        // (or not at all) must not hold up a keystroke.
        std::thread::spawn(move || {
            let _ = stdin.write_all(json.as_bytes());
            let _ = stdin.write_all(b"\n");
            drop(stdin);
            let _ = child.wait();
        });
        let what = match &e.selection {
            Some(s) => format!(
                "{}:{}–{}:{}",
                s.start.line, s.start.column, s.end.line, s.end.column
            ),
            None => format!("line {}, column {}", e.cursor.line, e.cursor.column),
        };
        Ok(format!("Sent {what}"))
    })
}
