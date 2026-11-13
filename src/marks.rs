//! Marks: line-addressed things a host puts beside the text — a citation, a note, a
//! suggested replacement, a change already on disk. They ride the diagnostic list, so the
//! gutter and `M-d` reach them; `M-d` on a hunk draws it, and `a`/`r` decide it here,
//! because only the editor holds the buffer the decision has to land in.

use std::path::Path;

use crate::buffer::Pos;
use crate::editor::{ActionKind, Editor};
use crate::lsp;
use crate::review::Change;

/// What a mark is. `Cite` and `Note` are remarks about lines; `Suggest` and `Edit` are
/// hunks — one not applied yet, one already on disk — and only those can be decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MarkKind {
    #[default]
    Cite,
    Note,
    Suggest,
    Edit,
}

impl MarkKind {
    pub fn name(self) -> &'static str {
        match self {
            MarkKind::Cite => "cite",
            MarkKind::Note => "note",
            MarkKind::Suggest => "suggest",
            MarkKind::Edit => "edit",
        }
    }

    /// Whether this kind is a hunk: accept/reject move text for it.
    pub fn hunk(self) -> bool {
        matches!(self, MarkKind::Suggest | MarkKind::Edit)
    }
}

/// How much a mark wants to be noticed. Never the LSP's severity number, so a host is
/// not asked to spell one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Severity {
    #[default]
    Info,
    Ok,
    Warn,
    Error,
}

/// What the operator decided about a hunk, drained by [`Editor::take_mark_outcomes`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Accepted,
    Rejected,
    Reverted,
}

/// One mark: 1-based inclusive lines, the text those lines had when the mark was made,
/// and for a hunk the change to take or undo.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Mark {
    /// First line, 1-based.
    pub line: usize,
    /// Last line, 1-based and inclusive.
    pub end_line: usize,
    /// A char range on one line; `None` is the whole of `line..=end_line`.
    pub col: Option<(usize, usize)>,
    pub kind: MarkKind,
    pub severity: Severity,
    /// One line, shown on the status row while the cursor is on the mark.
    pub text: String,
    /// The lines the mark expects to find; a mismatch makes it stale.
    pub anchor: Vec<String>,
    /// The hunk: `Suggest` and `Edit` carry one.
    pub change: Option<Change>,
    /// Ours to hand back in an outcome, the host's to name.
    pub id: String,
    /// Who set it, stamped by [`Editor::set_marks`]; a host replaces its own by this.
    pub source: String,
}

impl Mark {
    /// A citation: the lines, and the text they had when the reply landed.
    pub fn cite(id: &str, line: usize, end_line: usize, anchor: Vec<String>) -> Mark {
        Mark {
            line,
            end_line,
            anchor,
            id: id.to_string(),
            kind: MarkKind::Cite,
            ..Mark::default()
        }
    }

    /// A remark about lines, with the words shown at them.
    pub fn note(
        id: &str,
        line: usize,
        end_line: usize,
        anchor: Vec<String>,
        severity: Severity,
        text: &str,
    ) -> Mark {
        Mark {
            text: text.to_string(),
            severity,
            ..Mark::cite(id, line, end_line, anchor)
        }
        .of_kind(MarkKind::Note)
    }

    /// A replacement for the lines, not applied until it is accepted.
    pub fn suggest(
        id: &str,
        line: usize,
        end_line: usize,
        anchor: Vec<String>,
        body: &str,
    ) -> Mark {
        let before = anchor.join("\n");
        Mark {
            change: Some(Change {
                before,
                after: body.to_string(),
                before_start: line,
                after_start: line,
            }),
            ..Mark::cite(id, line, end_line, anchor)
        }
        .of_kind(MarkKind::Suggest)
    }

    /// An edit already on disk: `change.after` is what the lines hold now.
    pub fn edit(id: &str, change: Change) -> Mark {
        let line = change.after_start.max(1);
        let anchor: Vec<String> = change.after.lines().map(str::to_string).collect();
        // A pure deletion has no lines to anchor on: an empty range at its own line.
        let end_line = if anchor.is_empty() {
            line - 1
        } else {
            line + anchor.len() - 1
        };
        Mark {
            change: Some(change),
            ..Mark::cite(id, line, end_line, anchor)
        }
        .of_kind(MarkKind::Edit)
    }

    fn of_kind(mut self, kind: MarkKind) -> Mark {
        self.kind = kind;
        self
    }

    /// What a suggestion puts in place of its lines.
    pub fn after(&self) -> Option<String> {
        self.change.as_ref().map(|c| c.after.clone())
    }

    /// What an edit replaced, to put back when it is reverted.
    pub fn before(&self) -> Option<String> {
        self.change.as_ref().map(|c| c.before.clone())
    }

    /// Whether `row` (0-based) is inside the mark.
    pub fn covers(&self, row: usize) -> bool {
        let first = self.line.saturating_sub(1);
        first <= row && row < self.line.max(self.end_line)
    }
}

/// What `a`, `r`, `A` and `R` ask of the mark under the cursor, and what the wider
/// scopes the palette offers ask of a reply's or every file's hunks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MarkAct {
    /// `a`: take this hunk.
    Accept,
    /// `r`: drop this hunk, leaving the text as it is.
    Reject,
    /// `A`: take every hunk in the file.
    AcceptAll,
    /// `R`: drop or revert every hunk in the file.
    RejectAll,
    /// Take every hunk the reply under the cursor set, in every buffer.
    AcceptReply,
    /// Drop or revert every hunk that reply set.
    RejectReply,
    /// Take every hunk in every open buffer.
    AcceptEvery,
    /// Drop or revert every hunk in every open buffer.
    RejectEvery,
}

/// How far a decision reaches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Scope {
    /// The hunk under the cursor.
    Hunk,
    /// Every hunk in the current buffer.
    File,
    /// Every hunk one reply or call set, wherever it is open.
    Reply,
    /// Every hunk there is.
    Every,
}

impl MarkAct {
    /// Whether this is an accept, and how far it reaches.
    fn parts(self) -> (bool, Scope) {
        match self {
            MarkAct::Accept => (true, Scope::Hunk),
            MarkAct::Reject => (false, Scope::Hunk),
            MarkAct::AcceptAll => (true, Scope::File),
            MarkAct::RejectAll => (false, Scope::File),
            MarkAct::AcceptReply => (true, Scope::Reply),
            MarkAct::RejectReply => (false, Scope::Reply),
            MarkAct::AcceptEvery => (true, Scope::Every),
            MarkAct::RejectEvery => (false, Scope::Every),
        }
    }
}

/// The diagnostic severity a mark draws with: its own, and a hint when it has gone
/// stale, so a hunk that can no longer be taken reads as the weakest thing on the row.
fn diag_severity(m: &Mark, stale: bool) -> u64 {
    if stale {
        return 4;
    }
    match m.severity {
        Severity::Error => 1,
        Severity::Warn => 2,
        Severity::Info | Severity::Ok => 3,
    }
}

/// Does the buffer still hold the lines the mark was made against? An empty anchor is
/// never stale: a mark with nothing to compare against is trusted.
pub(crate) fn anchored(bs: &crate::BufferState, m: &Mark) -> bool {
    if m.anchor.is_empty() {
        return true;
    }
    let first = m.line.saturating_sub(1);
    let end = m.end_line.max(m.line);
    if m.line == 0 || end > bs.buf.row_count() || m.anchor.len() != end - first {
        return false;
    }
    bs.buf
        .rows_range(first, end)
        .iter()
        .zip(&m.anchor)
        .all(|(row, want)| row.iter().copied().eq(want.chars()))
}

impl Editor {
    /// Set `source`'s marks on the file at `path`, replacing the ones it set before.
    /// A file that is not open is the host's to keep and set when it opens.
    pub fn set_marks(&mut self, path: &Path, source: &str, marks: Vec<Mark>) {
        let Some(i) = self.find_buffer(path) else {
            return;
        };
        let bs = &mut self.buffers[i];
        bs.marks.retain(|m| m.source != source);
        bs.marks.extend(marks.into_iter().map(|mut m| {
            m.source = source.to_string();
            m
        }));
        // Row order, so the gutter, `M-d` and a file-wide `A` read them the same way.
        bs.marks.sort_by_key(|m| (m.line, m.end_line));
        if i == self.cur {
            self.highlight_dirty = true;
        }
    }

    /// Whether a buffer with this file is open, which is what decides whether a host's marks can
    /// land now or have to be kept for the pass that opens it.
    pub fn holds_file(&self, path: &Path) -> bool {
        self.find_buffer(path).is_some()
    }

    /// Drop the marks `source` set, in every buffer, or with `None` every mark there is.
    /// A closed buffer has already dropped its own.
    pub fn clear_marks(&mut self, source: Option<&str>) {
        for bs in &mut self.buffers {
            bs.marks.retain(|m| match source {
                Some(s) => m.source != s,
                None => false,
            });
        }
    }

    /// The current buffer's marks, row-major. Staleness is not stored: ask
    /// [`Editor::mark_stale`] about one, since the answer changes with every edit.
    pub fn marks(&self) -> &[Mark] {
        &self.bs().marks
    }

    /// Whether the mark with this id no longer fits the lines it was made against: an
    /// edit moved or changed them, so its text and its hunk are not about this file.
    pub fn mark_stale(&self, id: &str) -> bool {
        self.bs()
            .marks
            .iter()
            .find(|m| m.id == id)
            .is_some_and(|m| !anchored(self.bs(), m))
    }

    /// What the operator decided since the last prompt, and forget it: the host sends
    /// each one on and does not say it twice.
    pub fn take_mark_outcomes(&mut self) -> Vec<(String, Outcome)> {
        std::mem::take(&mut self.mark_outcomes)
    }

    /// The mark under the cursor that carries a hunk, whole lines first.
    pub(crate) fn hunk_at_cursor(&self) -> Option<Mark> {
        let row = self.bs().cursor.row;
        self.bs()
            .marks
            .iter()
            .find(|m| m.kind.hunk() && m.covers(row))
            .cloned()
    }

    /// The mark under the cursor, hunk or not: what the status row and `M-d` speak of.
    pub(crate) fn mark_at_cursor(&self) -> Option<&Mark> {
        let row = self.bs().cursor.row;
        self.bs().marks.iter().find(|m| m.covers(row))
    }

    /// Draw the hunk under the cursor, as a host review is drawn. False when the cursor
    /// is on no hunk, so a caller can say so.
    pub(crate) fn show_mark_hunk(&mut self) -> bool {
        let Some(m) = self.hunk_at_cursor() else {
            return false;
        };
        let Some(change) = m.change.clone() else {
            return false;
        };
        let name = self.buffer_name(self.cur);
        self.open_view(crate::diffview::Source::Review { name, change });
        self.mark_view = Some(m.id);
        true
    }

    /// `a`, `r`, `A` and `R` from the review view, and the palette's wider scopes. A
    /// hunk whose anchor no longer holds is refused, not guessed at: the lines it meant
    /// are gone.
    pub(crate) fn mark_act(&mut self, act: MarkAct) {
        let (take, scope) = act.parts();
        let targets = match scope {
            Scope::Hunk => self
                .hunk_at_cursor()
                .map(|m| (self.cur, m))
                .into_iter()
                .collect(),
            _ => self.hunks_in(scope),
        };
        if targets.is_empty() {
            self.flash(match scope {
                Scope::Hunk => "No suggestion or edit under the cursor",
                Scope::Reply => "Nothing from that reply is open here",
                Scope::Every => "No suggestions or edits anywhere",
                Scope::File => "No suggestions or edits in this file",
            });
            return;
        }
        if self.refuse_read_only("a mark") {
            return;
        }
        let here = self.cur;
        let mut accepted = 0usize;
        let mut rejected = 0usize;
        let mut kept = 0usize;
        let mut reverted = 0usize;
        let mut stale = 0usize;
        for (i, m) in &targets {
            // A hunk in another buffer is written there, under that buffer's own undo
            // step, and the cursor comes back to where the operator was.
            self.cur = *i;
            if !anchored(self.bs(), m) {
                stale += 1;
                continue;
            }
            let (text, outcome) = match (m.kind, take) {
                (MarkKind::Suggest, true) => (m.after(), Outcome::Accepted),
                (MarkKind::Suggest, false) => (None, Outcome::Rejected),
                (MarkKind::Edit, true) => (None, Outcome::Accepted),
                (MarkKind::Edit, false) => (m.before(), Outcome::Reverted),
                _ => continue,
            };
            if let Some(text) = text {
                self.write_hunk(m, &text);
            }
            self.settle(&m.id, outcome);
            match (m.kind, take) {
                (MarkKind::Suggest, true) => accepted += 1,
                (MarkKind::Suggest, false) => rejected += 1,
                (MarkKind::Edit, true) => kept += 1,
                _ => reverted += 1,
            }
        }
        self.cur = here;
        self.highlight_dirty = true;
        let mut said: Vec<String> = Vec::new();
        for (n, verb, noun) in [
            (accepted, "Accepted", "suggestion"),
            (rejected, "Rejected", "suggestion"),
            (kept, "Kept", "edit"),
            (reverted, "Reverted", "edit"),
        ] {
            if n > 0 {
                said.push(format!("{verb} {n} {noun}{}", crate::editor::plural(n)));
            }
        }
        let mut msg = if said.is_empty() {
            "Not applied".to_string()
        } else {
            said.join(", ")
        };
        // A suggestion taken or an edit put back is text the operator may want back.
        if accepted + reverted > 0 {
            msg.push_str(" — M-U undoes the text");
        }
        if stale > 0 {
            msg.push_str(&format!(
                "{}{stale} hunk{} went stale, {} lines changed",
                if said.is_empty() { ": " } else { "; " },
                crate::editor::plural(stale),
                if stale == 1 { "its" } else { "their" },
            ));
        }
        self.flash(&msg);
        self.close_view();
    }

    /// Every hunk a scope reaches, as (buffer, mark), bottom-up in each buffer: a hunk
    /// that grows or shrinks the file leaves the ones above it where they are.
    fn hunks_in(&self, scope: Scope) -> Vec<(usize, Mark)> {
        let reply = match scope {
            Scope::Reply => match self.mark_at_cursor() {
                Some(m) => Some(m.source.clone()),
                None => return Vec::new(),
            },
            _ => None,
        };
        let which: Vec<usize> = match scope {
            Scope::File => vec![self.cur],
            _ => (0..self.buffers.len()).collect(),
        };
        let mut out: Vec<(usize, Mark)> = Vec::new();
        for i in which {
            let bs = &self.buffers[i];
            out.extend(
                bs.marks
                    .iter()
                    .filter(|m| m.kind.hunk() && reply.as_deref().is_none_or(|r| m.source == r))
                    .map(|m| (i, m.clone())),
            );
        }
        out.sort_by_key(|(i, m)| (*i, std::cmp::Reverse((m.line, m.end_line))));
        out
    }

    /// Put `text` where the mark's lines are, as one undo step, and keep the cursor on
    /// the first row of what changed.
    fn write_hunk(&mut self, m: &Mark, text: &str) {
        let row_count = self.bs().buf.row_count();
        let first = m.line.saturating_sub(1).min(row_count);
        // `end_line < line` is an empty range: an edit that only inserted, put back.
        let last = if m.end_line < m.line {
            first
        } else {
            m.end_line.clamp(first, row_count)
        };
        let rows: Vec<Vec<char>> = match m.col {
            // A char range on one line: what is outside it survives the replacement.
            Some((c0, c1)) if m.line == m.end_line && first < row_count => {
                let row = self.bs().buf.row(first).clone();
                let head: String = row[..c0.min(row.len())].iter().collect();
                let tail: String = row[c1.min(row.len()).max(c0)..].iter().collect();
                format!("{head}{text}{tail}")
                    .split('\n')
                    .map(|l| l.chars().collect())
                    .collect()
            }
            _ if text.is_empty() => Vec::new(),
            _ => text.split('\n').map(|l| l.chars().collect()).collect(),
        };
        self.begin_action(ActionKind::Resolve, first, last);
        self.bs_mut().buf.splice_rows(first, last, rows);
        // A buffer is never zero rows, and a hunk may be the whole file.
        if self.bs().buf.row_count() == 0 {
            self.bs_mut().buf.push_row(Vec::new());
        }
        let top = first.min(self.bs().buf.row_count() - 1);
        let bs = self.bs_mut();
        bs.cursor = Pos { row: top, col: 0 };
        bs.mark = None;
        self.finish_step();
        self.edit_invalidate();
    }

    /// Drop a decided mark and remember what was decided, once.
    fn settle(&mut self, id: &str, outcome: Outcome) {
        for bs in &mut self.buffers {
            bs.marks.retain(|m| m.id != id);
        }
        if self.mark_view.as_deref() == Some(id) {
            self.mark_view = None;
        }
        self.mark_outcomes.push((id.to_string(), outcome));
    }

    /// After an edit: a citation or a note about lines that no longer say what they said
    /// is dropped. A hunk is kept — it is work — and becomes stale instead.
    pub(crate) fn marks_after_edit(&mut self) {
        let bs = self.bs_mut();
        if bs.marks.is_empty() {
            return;
        }
        // Read off the buffer before the filter takes its mutable borrow.
        let keep: Vec<bool> = bs
            .marks
            .iter()
            .map(|m| m.kind.hunk() || anchored(bs, m))
            .collect();
        let mut i = 0;
        bs.marks.retain(|_| {
            let k = keep[i];
            i += 1;
            k
        });
    }

    /// The marks as diagnostics, one per row they cover, so the gutter signs every line
    /// of a range and `M-d` lands on its first. A stale hunk says so in its text.
    pub(crate) fn mark_diags(&self) -> Vec<lsp::Diagnostic> {
        let bs = self.bs();
        let mut out = Vec::new();
        for m in &bs.marks {
            let stale = !anchored(bs, m);
            if stale && !m.kind.hunk() {
                continue;
            }
            let message = if stale {
                format!("{} (stale — the lines changed)", m.text)
            } else if m.text.is_empty() {
                m.kind.name().to_string()
            } else {
                m.text.clone()
            };
            let first = m.line.saturating_sub(1);
            let end = m.end_line.max(m.line).min(bs.buf.row_count());
            for row in first..end {
                let len = bs.buf.row(row).len();
                let (col, end_col) = match m.col {
                    Some(c) if row == first => {
                        let c0 = c.0.min(len);
                        (c0, c.1.min(len).max(c0))
                    }
                    _ => (0, len),
                };
                out.push(lsp::Diagnostic {
                    line: row,
                    col,
                    end_col,
                    message: message.clone(),
                    severity: diag_severity(m, stale),
                });
            }
        }
        out
    }

    /// The mark at the cursor, as the status row says it: its kind, its text, and that it
    /// can no longer be taken.
    pub(crate) fn mark_status(&self) -> Option<String> {
        let m = self.mark_at_cursor()?;
        let stale = !anchored(self.bs(), m);
        let mut s = if m.text.is_empty() {
            m.kind.name().to_string()
        } else {
            format!("{}: {}", m.kind.name(), m.text)
        };
        if stale {
            s.push_str(" (stale)");
        } else if m.kind.hunk() {
            s.push_str(" — M-D: the hunk, a: take it, r: drop it");
        }
        Some(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::buffer::Buffer;
    use crate::config;
    use crate::term::{KeyCode, KeyEvent, Mods};
    use std::path::PathBuf;

    fn path() -> PathBuf {
        PathBuf::from("/tmp/rano_marks_test/article.md")
    }

    fn opened(text: &str) -> Editor {
        let mut buf = Buffer::new();
        buf.set_rows(text.lines().map(|l| l.chars().collect()).collect());
        buf.name = Some(path());
        let mut ed = Editor::new(buf, config::Config::default());
        ed.text_w = 80;
        ed.text_h = 24;
        ed
    }

    fn lines(ed: &Editor) -> Vec<String> {
        ed.bs().buf.rows().map(|l| l.iter().collect()).collect()
    }

    fn keys(ed: &mut Editor, seq: &str) {
        for c in seq.chars() {
            ed.handle_key(KeyEvent::new(KeyCode::Char(c), Mods::NONE));
        }
    }

    fn alt(ed: &mut Editor, c: char) {
        ed.handle_key(KeyEvent::new(KeyCode::Char(c), Mods::ALT));
    }

    const TEXT: &str = "one\ntwo\nthree\nfour\nfive";

    #[test]
    fn a_suggestion_is_accepted_as_one_undo_step() {
        let mut ed = opened(TEXT);
        let anchor = vec!["two".to_string(), "three".to_string()];
        ed.set_marks(
            &path(),
            "reply:1",
            vec![Mark::suggest(
                "m1",
                2,
                3,
                anchor,
                "TWO\nTHREE\nTHREE AND A HALF",
            )],
        );
        assert_eq!(ed.marks().len(), 1);
        assert!(!ed.mark_stale("m1"));

        // M-D puts the cursor on the hunk and draws it.
        alt(&mut ed, 'd');
        assert!(ed.diff_view.is_some(), "the hunk is drawn");
        assert_eq!(ed.bs().cursor.row, 1);

        keys(&mut ed, "a");
        assert_eq!(
            lines(&ed),
            vec!["one", "TWO", "THREE", "THREE AND A HALF", "four", "five"]
        );
        assert!(
            ed.mark_stale("m1") || ed.marks().is_empty(),
            "the hunk is gone"
        );
        assert!(ed.bs().buf.modified, "unsaved, as a review is");
        assert!(ed.diff_view.is_none(), "the decided hunk closes its view");

        // One undo step: a single M-U puts all three lines back.
        alt(&mut ed, 'u');
        assert_eq!(lines(&ed), vec!["one", "two", "three", "four", "five"]);
        assert_eq!(
            ed.take_mark_outcomes(),
            vec![("m1".to_string(), Outcome::Accepted)]
        );
        assert!(ed.take_mark_outcomes().is_empty(), "said once, not twice");
    }

    #[test]
    fn an_edited_anchor_makes_a_hunk_stale_and_unacceptable() {
        let mut ed = opened(TEXT);
        ed.set_marks(
            &path(),
            "reply:1",
            vec![
                Mark::suggest("keep", 2, 2, vec!["two".to_string()], "TWO"),
                Mark::suggest("gone", 4, 4, vec!["four".to_string()], "FOUR"),
            ],
        );
        // An edit inside the second hunk's line: its anchor no longer holds.
        ed.bs_mut().cursor = Pos { row: 3, col: 0 };
        keys(&mut ed, "x");
        assert!(!ed.mark_stale("keep"), "an untouched hunk stays live");
        assert!(ed.mark_stale("gone"), "the edited one is stale");

        // M-D goes to it, and says so rather than offering to take it.
        ed.bs_mut().cursor = Pos { row: 0, col: 0 };
        alt(&mut ed, 'd');
        alt(&mut ed, 'd');
        assert_eq!(ed.bs().cursor.row, 3);
        assert!(
            ed.status_text().unwrap_or_default().contains("stale"),
            "{:?}",
            ed.status_text()
        );

        let before = lines(&ed);
        keys(&mut ed, "a");
        assert_eq!(lines(&ed), before, "a stale hunk is not applied");
        assert_eq!(ed.marks().len(), 2, "and it is still there to be read");
        assert!(ed.take_mark_outcomes().is_empty(), "nothing was decided");
    }

    #[test]
    fn a_note_reaches_the_gutter_and_says_its_words_at_the_cursor() {
        let mut ed = opened(TEXT);
        ed.set_marks(
            &path(),
            "reply:1",
            vec![Mark::note(
                "n1",
                3,
                3,
                vec!["three".to_string()],
                Severity::Warn,
                "this claim has no source",
            )],
        );
        let diags = ed.all_diags();
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].line, 2, "0-based row, as a diagnostic counts");
        assert_eq!(diags[0].severity, 2, "a warning colours the gutter");

        ed.jump_next_diag();
        assert_eq!(ed.bs().cursor.row, 2);
        let status = ed.status_text().unwrap_or_default();
        assert!(status.contains("this claim has no source"), "{status}");
        assert!(ed.diff_view.is_none(), "a remark is not drawn as a diff");
    }

    #[test]
    fn an_edit_drops_the_citations_about_the_lines_it_changed() {
        let mut ed = opened(TEXT);
        ed.set_marks(
            &path(),
            "reply:1",
            vec![
                Mark::cite("c1", 1, 1, vec!["one".to_string()]),
                Mark::note(
                    "c2",
                    5,
                    5,
                    vec!["five".to_string()],
                    Severity::Info,
                    "read this",
                ),
            ],
        );
        ed.bs_mut().cursor = Pos { row: 0, col: 0 };
        keys(&mut ed, "z");
        let ids: Vec<&str> = ed.marks().iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, vec!["c2"], "only the untouched citation is left");
    }

    #[test]
    fn source_is_the_owner_of_a_set_and_clearing_one_leaves_the_others() {
        let mut ed = opened(TEXT);
        ed.set_marks(&path(), "reply:1", vec![Mark::cite("a", 1, 1, vec![])]);
        ed.set_marks(&path(), "call:9", vec![Mark::cite("b", 2, 2, vec![])]);
        ed.set_marks(&path(), "reply:1", vec![Mark::cite("c", 3, 3, vec![])]);
        let ids: Vec<&str> = ed.marks().iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, vec!["b", "c"], "a source replaces only its own");
        ed.clear_marks(Some("call:9"));
        assert_eq!(ed.marks().len(), 1);
        ed.clear_marks(None);
        assert!(ed.marks().is_empty());
    }

    #[test]
    fn every_hunk_in_the_file_is_taken_from_the_bottom_up() {
        let mut ed = opened(TEXT);
        ed.set_marks(
            &path(),
            "reply:1",
            vec![
                Mark::suggest("s1", 2, 2, vec!["two".to_string()], "TWO\nTWO AGAIN"),
                Mark::suggest("s2", 4, 4, vec!["four".to_string()], "FOUR"),
            ],
        );
        ed.mark_act(MarkAct::AcceptAll);
        assert_eq!(
            lines(&ed),
            vec!["one", "TWO", "TWO AGAIN", "three", "FOUR", "five"]
        );
        assert!(ed.marks().is_empty());
        assert_eq!(
            ed.take_mark_outcomes(),
            vec![
                ("s2".to_string(), Outcome::Accepted),
                ("s1".to_string(), Outcome::Accepted)
            ]
        );
    }

    #[test]
    fn a_replys_hunks_are_the_ones_that_reply_set() {
        let mut ed = opened(TEXT);
        ed.set_marks(
            &path(),
            "reply:1",
            vec![Mark::suggest("s1", 2, 2, vec!["two".to_string()], "TWO")],
        );
        ed.set_marks(
            &path(),
            "reply:2",
            vec![Mark::suggest("s2", 4, 4, vec!["four".to_string()], "FOUR")],
        );
        // The cursor is on the second reply's hunk: that is the reply meant.
        ed.bs_mut().cursor = Pos { row: 3, col: 0 };
        ed.mark_act(MarkAct::AcceptReply);
        assert_eq!(lines(&ed), vec!["one", "two", "three", "FOUR", "five"]);
        let ids: Vec<&str> = ed.marks().iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, vec!["s1"], "the other reply's hunk is left alone");
        assert_eq!(
            ed.take_mark_outcomes(),
            vec![("s2".to_string(), Outcome::Accepted)]
        );
    }

    #[test]
    fn every_hunk_in_every_open_file_is_taken() {
        let mut ed = opened(TEXT);
        let other = PathBuf::from("/tmp/rano_marks_test/other.md");
        let mut buf = Buffer::new();
        buf.set_rows("alpha\nbeta".lines().map(|l| l.chars().collect()).collect());
        buf.name = Some(other.clone());
        ed.buffers.push(crate::BufferState::new(buf));
        ed.set_marks(
            &path(),
            "reply:1",
            vec![Mark::suggest("s1", 2, 2, vec!["two".to_string()], "TWO")],
        );
        ed.set_marks(
            &other,
            "reply:1",
            vec![Mark::suggest("s2", 2, 2, vec!["beta".to_string()], "BETA")],
        );
        ed.mark_act(MarkAct::AcceptEvery);
        assert_eq!(lines(&ed), vec!["one", "TWO", "three", "four", "five"]);
        let there: Vec<String> = ed.buffers[1]
            .buf
            .rows()
            .map(|l| l.iter().collect())
            .collect();
        assert_eq!(there, vec!["alpha", "BETA"], "the other file changed too");
        assert_eq!(ed.cur, 0, "the cursor comes back where it was");
        assert!(ed.buffers.iter().all(|b| b.marks.is_empty()));
        let mut ids: Vec<String> = ed
            .take_mark_outcomes()
            .into_iter()
            .map(|(i, _)| i)
            .collect();
        ids.sort();
        assert_eq!(ids, vec!["s1", "s2"]);
    }

    #[test]
    fn a_hunk_on_a_file_that_is_not_open_is_the_hosts_to_keep() {
        let mut ed = opened(TEXT);
        ed.set_marks(
            &PathBuf::from("/tmp/rano_marks_test/elsewhere.md"),
            "reply:1",
            vec![Mark::cite("x", 1, 1, vec![])],
        );
        assert!(ed.marks().is_empty());
    }

    #[test]
    fn an_edit_hunk_reverts_to_before_and_keeps_stays() {
        let mut ed = opened(TEXT);
        let change = Change {
            before: "four".to_string(),
            after: "FOUR".to_string(),
            before_start: 4,
            after_start: 4,
        };
        let edited = ed.bs().buf.text().replace("four", "FOUR");
        ed.bs_mut()
            .buf
            .set_rows(edited.lines().map(|l| l.chars().collect()).collect());
        ed.set_marks(&path(), "call:1", vec![Mark::edit("e1", change)]);
        assert!(!ed.mark_stale("e1"), "the anchor is the after text");

        ed.bs_mut().cursor = Pos { row: 3, col: 0 };
        ed.mark_act(MarkAct::Reject);
        assert_eq!(lines(&ed)[3], "four", "revert put before back");
        alt(&mut ed, 'u');
        assert_eq!(lines(&ed)[3], "FOUR", "one undo step, as an accept is");
        assert_eq!(
            ed.take_mark_outcomes(),
            vec![("e1".to_string(), Outcome::Reverted)]
        );
    }
}
