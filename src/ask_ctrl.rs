//! `M-k`: ask about the selection. The target rule lives here — the mark, else the
//! paragraph around the cursor in prose and its line in code — and one callback
//! carries the ask to the host.

use crate::ask::{Answer, AskEvent};
use crate::buffer::{Buffer, Pos};
use crate::editor::Editor;
use crate::send::{Point, Selection, SendEvent};
use crate::syntax::Lang;

impl Editor {
    /// Bind `M-k` from `config.ask`: a key per preset, then ask… and clear marks. A
    /// host that replaces the presets after `Editor::new` calls this again.
    pub fn bind_ask(&mut self) {
        self.keymaps.bind_ask(&self.config.ask);
    }

    /// **`M-k <key>`: ask a preset about the selection**, or about the paragraph or
    /// line the cursor is on. One event out; the answer comes back as chat, notes or
    /// a suggestion, so nothing ask-specific travels back.
    pub fn ask(&mut self, preset: &str) {
        let Some(p) = self.config.ask.iter().find(|p| p.id == preset).cloned() else {
            self.flash(&format!("No ask preset {preset}"));
            return;
        };
        let event = AskEvent {
            send: self.asked_place(),
            preset: p.id,
            answer: p.answer,
            prompt: p.prompt,
        };
        self.deliver_ask(event);
    }

    /// `M-k k`: the question is typed, so there is no prompt of its own, and the
    /// place stays attached. Standalone, rano's own chat takes the keyboard, since
    /// that is the only composer it has.
    pub fn ask_free(&mut self) {
        let event = AskEvent {
            send: self.asked_place(),
            preset: "ask".to_string(),
            answer: Answer::Chat,
            prompt: String::new(),
        };
        if self.on_ask.is_some() {
            self.deliver_ask(event);
            return;
        }
        self.enter_sidebar();
        self.flash("Type the question in the chat");
    }

    /// `M-k c`: clear every mark in this buffer. Marks are package R3's
    /// (`rano::marks`, `Editor::clear_marks`); this call is the one its
    /// `clear_marks(None)` replaces, so nothing here decides what a mark is.
    pub(crate) fn ask_clear_marks(&mut self) {
        self.flash("No marks to clear");
    }

    /// The place an ask is about: the mark's region, else the target rule.
    fn asked_place(&self) -> SendEvent {
        let mut send = self.send_event();
        // A view has no line of a file to point at, and its own event already names
        // the entry under the cursor.
        if send.selection.is_none() && !self.bs().synthetic {
            send.selection = Some(self.ask_target());
        }
        send
    }

    /// The range with no mark set: a paragraph for prose, a line for code.
    fn ask_target(&self) -> Selection {
        let bs = self.bs();
        let row = bs.cursor.row;
        let prose = matches!(
            crate::syntax::language_of(&bs.buf),
            None | Some(Lang::Markdown) | Some(Lang::MarkdownInline)
        );
        let (first, last) = if prose {
            paragraph_rows(&bs.buf, row)
        } else {
            (row, row)
        };
        let end = Pos {
            row: last,
            col: bs.buf.line_len(last),
        };
        let text = bs
            .buf
            .copy_range(Pos { row: first, col: 0 }, end)
            .iter()
            .map(|r| r.iter().collect::<String>())
            .collect::<Vec<_>>()
            .join("\n");
        let point = |line: usize, col: usize| Point { line, column: col };
        Selection {
            start: point(first + 1, 1),
            end: point(last + 1, end.col + 1),
            text,
        }
    }

    /// Hand the ask to the host. `on_ask` is the door a pane answers; a host with only
    /// `on_send` still gets the place and the prompt, which `send_command` can read.
    fn deliver_ask(&mut self, event: AskEvent) {
        if let Some(ask) = self.on_ask.as_mut() {
            match ask(&event) {
                Ok(msg) => self.flash(&msg),
                Err(e) => self.flash(&format!("Ask failed: {e}")),
            }
            return;
        }
        if let Some(send) = self.on_send.as_mut() {
            let mut place = event.send.clone();
            place.ask = (!event.prompt.is_empty()).then(|| event.prompt.clone());
            match send(&place) {
                Ok(msg) => self.flash(&msg),
                Err(e) => self.flash(&format!("Ask failed: {e}")),
            }
            return;
        }
        self.flash(
            "Nowhere to ask: no host here answers asks, and no send_command in the config. \
             Ask from letibot's editor pane, which answers them itself.",
        );
    }

    /// What a keymap entry is called: a command's title, or an ask preset's label.
    pub(crate) fn entry_title(&self, name: &str) -> Option<String> {
        match crate::ask::preset_of(name) {
            Some(id) => Some(
                self.config
                    .ask
                    .iter()
                    .find(|p| p.id == id)
                    .map_or_else(|| id.to_string(), |p| p.label.clone()),
            ),
            None => crate::commands::command(name).map(|c| c.title.to_string()),
        }
    }
}

/// The rows of the paragraph `row` sits in: outward while the rows hold text. A cursor
/// on a blank row has no paragraph, so its own row is all it gets.
fn paragraph_rows(buf: &Buffer, row: usize) -> (usize, usize) {
    let blank = |r: usize| {
        buf.row_opt(r)
            .is_none_or(|l| l.iter().all(|c| c.is_whitespace()))
    };
    if blank(row) {
        return (row, row);
    }
    let mut first = row;
    while first > 0 && !blank(first - 1) {
        first -= 1;
    }
    let mut last = row;
    while !blank(last + 1) {
        last += 1;
    }
    (first, last)
}
