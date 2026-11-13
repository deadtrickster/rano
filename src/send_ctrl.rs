//! Talking to a host: being told where to go ([`Editor::open_at`]), and telling it
//! where the reader is ([`Editor::send_position`], M-S). Both are in-process calls.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use crate::send::{Point, Selection, SendEvent};

use crate::buffer::Pos;
use crate::editor::{Editor, canonical, normalize};

/// What a host does with a [`SendEvent`]: `Ok` carries what to tell the reader.
pub type OnSend = Box<dyn FnMut(&SendEvent) -> Result<String, String>>;

impl Editor {
    /// Open `path` — or switch to it when a buffer already shows it — and put the
    /// cursor on `line` (1-based), at `column`, centred. A file still arriving is
    /// positioned when that row lands.
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
            // `open_file` keeps a buffer with unsaved edits and opens the file
            // beside it, so a host's jump never discards work.
            if !self.open_file(&path.display().to_string()) {
                return Err(self.status_text().unwrap_or_else(|| "cannot open".into()));
            }
        }
        self.bs_mut().goto = Some(at);
        // Now if the row is there; otherwise the loop applies it on arrival.
        self.apply_startup_pos();
        Ok(())
    }

    /// The reader's place: the file (absolute), the cursor, and the marked region
    /// with its text. A view does not send a line of text — its rows are a rendering.
    pub fn send_event(&self) -> SendEvent {
        let bs = self.bs();
        let point = |p: Pos| Point {
            line: p.row + 1,
            column: p.col + 1,
        };
        if bs.synthetic {
            let text = |row: usize| {
                bs.buf
                    .row_opt(row)
                    .map(|r| r.iter().collect::<String>())
                    .unwrap_or_default()
            };
            // A listing's row names an entry; a provider's row names nothing, so the
            // view's own path is the target.
            let entry = bs
                .dir
                .as_ref()
                .and_then(|dir| crate::dirview::path_at(dir, &text(bs.cursor.row)));
            let selection = bs.mark.and_then(|m| {
                let (a, b) = normalize(m, bs.cursor);
                (a != b).then(|| Selection {
                    // The view's own rows; for a listing, the entry names.
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
            return SendEvent {
                path: entry.or_else(|| bs.buf.name.as_deref().map(canonical)),
                // A view has no line of the target in it: line 1 is where a host
                // opening the target lands.
                cursor: Point { line: 1, column: 1 },
                selection,
                modified: bs.buf.modified,
                ask: None,
            };
        }
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
            ask: None,
        }
    }

    /// M-S: hand the reader's place to the host.
    pub(crate) fn send_position(&mut self) {
        let event = self.send_event();
        let Some(send) = self.on_send.as_mut() else {
            self.flash(
                "Nowhere to send: no send_command in the config, and no host answering sends. \
                 Add `send_command = \"your-tool\"` - it gets the event on stdin and \
                 RANO_FILE/RANO_LINE/RANO_COLUMN - or ask from letibot's editor pane, which \
                 answers sends itself.",
            );
            return;
        };
        match send(&event) {
            Ok(msg) => self.flash(&msg),
            Err(e) => self.flash(&format!("Send failed: {e}")),
        }
    }
}

/// `on_send` for the standalone binary: run `cmd` with `sh -c`, the event as JSON
/// on stdin and `RANO_FILE`/`RANO_LINE`/`RANO_COLUMN` in its environment.
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
        // Off the main thread, and reaped there: a command that reads slowly must
        // not hold up a keystroke.
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

/// `name:line` or `name:line:col`, when `arg` is not itself an existing path.
/// Splits from the right, so a name with colons of its own keeps them.
pub fn split_position(arg: &str) -> Option<(String, usize, Option<usize>)> {
    if Path::new(arg).exists() {
        return None;
    }
    let num = |s: &str| s.parse::<usize>().ok().filter(|&n| n > 0);
    let (head, last) = arg.rsplit_once(':')?;
    let last = num(last)?;
    if let Some((name, line)) = head.rsplit_once(':')
        && let Some(line) = num(line)
        && !name.is_empty()
    {
        return Some((name.to_string(), line, Some(last)));
    }
    (!head.is_empty()).then(|| (head.to_string(), last, None))
}
