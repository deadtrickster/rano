//! Editor core: the Editor model, the undo machinery and the editing
//! actions (input, cut/paste, undo/redo, movement, files, buffers,
//! goto, justify/sort, styling glue).

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use ratatui::style::{Color, Modifier, Style};

use crate::BufferState;
use crate::buffer::{Buffer, Pos};
use crate::config;
use crate::prompt::{Prompt, PromptKind, expand_tilde};
use crate::search_ctrl::ReplaceState;
use crate::ui;

#[derive(Debug, Clone)]
pub struct Flash {
    pub text: String,
    pub until: Instant,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ActionKind {
    Insert,
    Newline,
    Backspace,
    Delete,
    Cut,
    Paste,
    Replace,
    Justify,
    Sort,
    Filter,
    Exec,
    ReadFile,
}

#[derive(Debug)]
pub(crate) struct UndoStep {
    pub kind: ActionKind,
    pub start: usize,           // row index, PRE-edit coords
    pub before: Vec<Vec<char>>, // pre-edit rows [start, start+len) (may be empty → pure insertion)
    pub after_start: usize,     // row index, POST-edit coords
    pub after: Vec<Vec<char>>,  // post-edit rows (may be empty → pure deletion)
    pub cur_before: Pos,
    pub cur_after: Pos,
    len_at_begin: usize, // buf.lines.len() when the step began
}

pub struct Editor {
    /// Open buffers (F8 groundwork); exactly one until multi-buffer lands.
    pub buffers: Vec<BufferState>,
    /// Index into `buffers` of the active buffer.
    pub cur: usize,
    pub cut: Vec<Vec<char>>,
    pub cut_line: bool,
    /// Search modifier toggles (M-C / M-R in the search prompt; runtime-only,
    /// like show_line_numbers). Defaults are case-insensitive, literal.
    pub search_case_sensitive: bool,
    pub search_regex: bool,
    pub prompt: Option<Prompt>,
    pub help: bool,
    pub status: Option<Flash>,
    pub loc_until: Option<Instant>,
    pub quit: bool,
    pub pending_write: Option<PathBuf>,
    pub quit_after_save: bool,
    pub replace: Option<ReplaceState>,
    pub replace_pos: Option<Pos>,
    pub replace_count: usize,
    pub text_w: usize,
    pub text_h: usize,
    /// Rendered width of a tab (seeded from config, F1).
    pub tab_width: usize,
    /// Line-number gutter toggle (M-N; seeded from config, F1).
    pub show_line_numbers: bool,
    /// Startup config (F1). tab_width/show_line_numbers seed from it; the
    /// M-N toggle stays runtime-only (no config write-back). `multibuffer`
    /// is unused until F8.
    pub config: config::Config,
    /// Prompt history per kind (E4b). Session-global: survives buffer
    /// switches and prompt close/reopen.
    pub search_hist: Vec<String>,
    pub exec_hist: Vec<String>,
    pub file_hist: Vec<String>,
    /// Index into the active history while cycling (None = not browsing),
    /// plus the text being edited when cycling started.
    pub hist_idx: Option<usize>,
    pub hist_draft: String,
}

impl Editor {
    pub(crate) fn new(buf: Buffer, config: config::Config) -> Self {
        let tab_width = config.tab_width;
        let show_line_numbers = config.line_numbers;
        let mut ed = Self {
            buffers: vec![BufferState::new(buf)],
            cur: 0,
            cut: Vec::new(),
            cut_line: false,
            search_case_sensitive: false,
            search_regex: false,
            prompt: None,
            help: false,
            status: None,
            loc_until: None,
            quit: false,
            pending_write: None,
            quit_after_save: false,
            replace: None,
            replace_pos: None,
            replace_count: 0,
            text_w: 80,
            text_h: 24,
            tab_width,
            show_line_numbers,
            config,
            search_hist: Vec::new(),
            exec_hist: Vec::new(),
            file_hist: Vec::new(),
            hist_idx: None,
            hist_draft: String::new(),
        };
        ed.lsp_sync();
        ed
    }

    /// The active buffer's state (F8 groundwork: exactly one buffer today).
    pub(crate) fn bs(&self) -> &BufferState {
        &self.buffers[self.cur]
    }

    pub(crate) fn bs_mut(&mut self) -> &mut BufferState {
        &mut self.buffers[self.cur]
    }

    fn clamp_cursor(&mut self) {
        let bs = self.bs_mut();
        bs.cursor = bs.buf.clamp(bs.cursor);
    }

    pub(crate) fn flash(&mut self, msg: &str) {
        self.status = Some(Flash {
            text: msg.to_string(),
            until: Instant::now() + Duration::from_secs(3),
        });
    }

    pub(crate) fn adjust_scroll(&mut self, text_h: usize) {
        if text_h == 0 {
            return;
        }
        let bs = self.bs_mut();
        if bs.cursor.row < bs.scroll {
            bs.scroll = bs.cursor.row;
        }
        if bs.cursor.row >= bs.scroll + text_h {
            bs.scroll = bs.cursor.row - text_h + 1;
        }
        let max_scroll = bs.buf.lines.len().saturating_sub(text_h);
        bs.scroll = bs.scroll.min(max_scroll);
    }

    /// E3: keep the cursor's display column inside the horizontal window.
    /// scroll_x and the viewport width are both in display cols; the
    /// viewport excludes the gutter when line numbers are shown (F4).
    pub(crate) fn adjust_scroll_x(&mut self) {
        let gutter = if self.show_line_numbers {
            ui::gutter_width(self.bs().buf.lines.len())
        } else {
            0
        };
        let view_w = self.text_w.saturating_sub(gutter);
        if view_w == 0 {
            return;
        }
        let tab_width = self.tab_width;
        let bs = self.bs_mut();
        let row = bs.cursor.row.min(bs.buf.lines.len().saturating_sub(1));
        let line = &bs.buf.lines[row];
        let disp = ui::display_col(line, bs.cursor.col, tab_width);
        if disp < bs.scroll_x {
            bs.scroll_x = disp;
        }
        if disp >= bs.scroll_x + view_w {
            bs.scroll_x = disp + 1 - view_w;
        }
        bs.scroll_x = bs.scroll_x.min(ui::display_width(line, tab_width));
    }

    pub(crate) fn edit_invalidate(&mut self) {
        let bs = self.bs_mut();
        bs.buf.modified = true;
        bs.search_matches = None;
        bs.search.current = 0;
        bs.hl.refresh(&bs.buf);
        bs.lsp_dirty = true;
    }

    // ---------- undo / redo ----------
    //
    // Region-based: each step records only the affected rows (before/after)
    // plus the row-count delta, not the whole buffer. A run of the same kind
    // of single-row action (a typed word, a run of backspaces) coalesces into
    // one step; repeated ^K cuts merge with pre-edit-row extension.

    const UNDO_LIMIT: usize = 500;

    // Row span [first, last_exclusive) of the current selection, if non-empty.
    pub(crate) fn sel_span(&self) -> Option<(usize, usize)> {
        let mark = self.bs().mark?;
        let (a, b) = normalize(mark, self.bs().cursor);
        (a != b).then_some((a.row, b.row))
    }

    // Empty region allowed: first_row == last_row_exclusive (pure insertion
    // point); before == [] is tolerated.
    pub(crate) fn begin_action(
        &mut self,
        kind: ActionKind,
        first_row: usize,
        last_row_exclusive: usize,
    ) {
        let bs = self.bs_mut();
        bs.redo.clear();
        let len = bs.buf.lines.len();
        let first = first_row.min(len);
        let last = last_row_exclusive.min(len);
        let before = if first < last {
            bs.buf.lines[first..last].to_vec()
        } else {
            Vec::new()
        };
        bs.pending = Some(UndoStep {
            kind,
            start: first,
            before,
            after_start: first,
            after: Vec::new(),
            cur_before: bs.cursor,
            cur_after: bs.cursor,
            len_at_begin: len,
        });
    }

    pub(crate) fn finish_step(&mut self) {
        let Some(mut step) = self.bs_mut().pending.take() else {
            return;
        };
        step.cur_after = self.bs().cursor;
        let kind = step.kind;
        let coalesce = self.bs().last_kind == Some(kind) && self.coalesce_kind(&step);
        if coalesce {
            let mut prev = self.bs_mut().undo.pop_back().unwrap();
            self.apply_coalesce(&mut prev, &step);
            self.bs_mut().undo.push_back(prev);
        } else {
            step.after = self.current_after(step.after_start, step.before.len(), step.len_at_begin);
            self.push_undo(step);
        }
        self.bs_mut().last_kind = Some(kind);
    }

    // Coalescing rules for merging `step` into `undo.back()`: runs of
    // Insert/Backspace/Delete on the same single row, and repeated ^K cuts.
    fn coalesce_kind(&self, step: &UndoStep) -> bool {
        let Some(prev) = self.bs().undo.back() else {
            return false;
        };
        match step.kind {
            ActionKind::Insert | ActionKind::Backspace | ActionKind::Delete => {
                prev.before.len() == 1
                    && prev.after.len() == 1
                    && step.before.len() == 1
                    && prev.start == step.start
            }
            ActionKind::Cut => {
                if step.cur_before.row != prev.cur_after.row || prev.start != prev.after_start {
                    return false;
                }
                let covered_end = prev.after_start + prev.after.len();
                let hi = step.start + step.before.len();
                if step.start >= covered_end {
                    step.start == covered_end // cut of the next uncovered row
                } else {
                    // fully inside the rows the step already covers
                    step.start >= prev.after_start && hi <= covered_end
                }
            }
            _ => false,
        }
    }

    fn apply_coalesce(&mut self, prev: &mut UndoStep, step: &UndoStep) {
        if step.kind == ActionKind::Cut {
            let covered_end = prev.after_start + prev.after.len();
            if step.start >= covered_end {
                // the cut removes a NEW pre-edit row: extend before with it
                prev.before.extend(step.before.iter().cloned());
            }
            // partial→full cut of an already-included row extends nothing
        }
        prev.after = self.current_after(prev.after_start, prev.before.len(), prev.len_at_begin);
        prev.cur_after = step.cur_after;
    }

    // Post-edit rows of a region: before.len() + (lines.len() - len_at_begin)
    // rows starting at after_start, clamped to the buffer.
    fn current_after(
        &self,
        after_start: usize,
        before_len: usize,
        len_at_begin: usize,
    ) -> Vec<Vec<char>> {
        let delta = self.bs().buf.lines.len() as isize - len_at_begin as isize;
        let count = (before_len as isize + delta).max(0) as usize;
        let end = (after_start + count).min(self.bs().buf.lines.len());
        if after_start >= end {
            Vec::new()
        } else {
            self.bs().buf.lines[after_start..end].to_vec()
        }
    }

    fn push_undo(&mut self, step: UndoStep) {
        let bs = self.bs_mut();
        bs.undo.push_back(step);
        if bs.undo.len() > Self::UNDO_LIMIT {
            bs.undo.pop_front();
        }
    }

    fn push_redo(&mut self, step: UndoStep) {
        let bs = self.bs_mut();
        bs.redo.push_back(step);
        if bs.redo.len() > Self::UNDO_LIMIT {
            bs.redo.pop_front();
        }
    }

    pub(crate) fn undo(&mut self) {
        if let Some(step) = self.bs_mut().undo.pop_back() {
            let bs = self.bs_mut();
            let start = step.after_start.min(bs.buf.lines.len());
            let end = (step.after_start + step.after.len())
                .min(bs.buf.lines.len())
                .max(start);
            bs.buf.lines.splice(start..end, step.before.iter().cloned());
            bs.cursor = step.cur_before;
            bs.last_kind = None;
            self.edit_invalidate();
            self.clamp_cursor();
            self.push_redo(step);
        }
    }

    pub(crate) fn redo(&mut self) {
        if let Some(step) = self.bs_mut().redo.pop_back() {
            let bs = self.bs_mut();
            let start = step.start.min(bs.buf.lines.len());
            let end = (step.start + step.before.len())
                .min(bs.buf.lines.len())
                .max(start);
            bs.buf.lines.splice(start..end, step.after.iter().cloned());
            bs.cursor = step.cur_after;
            bs.last_kind = None;
            self.edit_invalidate();
            self.clamp_cursor();
            self.push_undo(step);
        }
    }

    fn delete_selection_if_any(&mut self) -> bool {
        if let Some(mark) = self.bs().mark {
            let (a, b) = normalize(mark, self.bs().cursor);
            if a != b {
                {
                    let bs = self.bs_mut();
                    let _ = bs.buf.cut_range(a, b);
                    bs.cursor = a;
                    bs.mark = None;
                }
                self.edit_invalidate();
                return true;
            }
        }
        false
    }

    // ---------- character input ----------

    pub(crate) fn insert_char(&mut self, ch: char) {
        let (first, last) = match self.sel_span() {
            Some((a, b)) => (a, b + 1),
            None => (self.bs().cursor.row, self.bs().cursor.row + 1),
        };
        self.begin_action(ActionKind::Insert, first, last);
        self.delete_selection_if_any();
        let bs = self.bs_mut();
        bs.buf.insert_char(bs.cursor.row, bs.cursor.col, ch);
        bs.cursor.col += 1;
        self.finish_step();
        self.edit_invalidate();
    }

    pub(crate) fn newline(&mut self) {
        let (first, last) = match self.sel_span() {
            Some((a, b)) => (a, b + 1),
            None => (self.bs().cursor.row, self.bs().cursor.row + 1),
        };
        self.begin_action(ActionKind::Newline, first, last);
        self.delete_selection_if_any();
        let row = self.bs().cursor.row;
        let col = self.bs().cursor.col;
        // F3: with auto_indent, carry the current line's leading whitespace
        // (up to the cursor) onto the new row.
        let indent: Vec<char> = if self.config.auto_indent {
            self.bs().buf.lines[row]
                .iter()
                .take(col)
                .take_while(|c| c.is_whitespace())
                .copied()
                .collect()
        } else {
            Vec::new()
        };
        self.bs_mut().buf.newline(row, col);
        if indent.is_empty() {
            self.bs_mut().cursor = Pos {
                row: row + 1,
                col: 0,
            };
        } else {
            let n = indent.len();
            self.bs_mut().buf.lines[row + 1].splice(0..0, indent);
            self.bs_mut().cursor = Pos {
                row: row + 1,
                col: n,
            };
        }
        self.finish_step();
        self.edit_invalidate();
    }

    pub(crate) fn backspace(&mut self) {
        let c = self.bs().cursor;
        let sel = self.sel_span();
        if sel.is_none() && !(c.col > 0 || c.row > 0) {
            return;
        }
        let (first, last) = match sel {
            Some((a, b)) => (a, b + 1),
            None if c.col > 0 => (c.row, c.row + 1),
            None => (c.row - 1, c.row + 1), // join with the previous row
        };
        self.begin_action(ActionKind::Backspace, first, last);
        if self.delete_selection_if_any() {
            self.finish_step();
            return;
        }
        if c.col > 0 {
            let bs = self.bs_mut();
            bs.buf.backspace(c.row, c.col);
            bs.cursor.col -= 1;
        } else if c.row > 0 {
            let bs = self.bs_mut();
            let prev_len = bs.buf.line_len(c.row - 1);
            bs.buf.backspace(c.row, 0);
            bs.cursor = Pos {
                row: c.row - 1,
                col: prev_len,
            };
        }
        self.finish_step();
        self.edit_invalidate();
    }

    pub(crate) fn delete_at(&mut self) {
        let c = self.bs().cursor;
        let sel = self.sel_span();
        let can_delete =
            c.col < self.bs().buf.line_len(c.row) || c.row + 1 < self.bs().buf.lines.len();
        if sel.is_none() && !can_delete {
            return;
        }
        let (first, last) = match sel {
            Some((a, b)) => (a, b + 1),
            None if c.col < self.bs().buf.line_len(c.row) => (c.row, c.row + 1),
            None => (c.row, c.row + 2), // join with the next row
        };
        self.begin_action(ActionKind::Delete, first, last);
        if self.delete_selection_if_any() {
            self.finish_step();
            return;
        }
        self.bs_mut().buf.delete_at(c.row, c.col);
        self.clamp_cursor();
        self.finish_step();
        self.edit_invalidate();
    }

    // ---------- cut / paste ----------

    pub(crate) fn cut(&mut self) {
        let (first, last) = match self.bs().mark {
            Some(mark) => {
                let (a, b) = normalize(mark, self.bs().cursor);
                if a != b {
                    (a.row, b.row + 1)
                } else {
                    (self.bs().cursor.row, self.bs().cursor.row + 1)
                }
            }
            None => (self.bs().cursor.row, self.bs().cursor.row + 1),
        };
        self.begin_action(ActionKind::Cut, first, last);
        if let Some(mark) = self.bs().mark {
            let (a, b) = normalize(mark, self.bs().cursor);
            if a != b {
                let rows = {
                    let bs = self.bs_mut();
                    let rows = bs.buf.cut_range(a, b);
                    bs.cursor = a;
                    rows
                };
                self.cut = rows;
                self.cut_line = a.col == 0 && b.row > a.row;
                self.bs_mut().mark = None;
                self.finish_step();
                self.edit_invalidate();
                return;
            }
            self.bs_mut().mark = None;
        }
        let c = self.bs().cursor;
        let l = self.bs().buf.line_len(c.row);
        if c.col == l {
            let (line, empty) = {
                let bs = self.bs_mut();
                let line = bs.buf.lines.remove(c.row);
                let empty = bs.buf.lines.is_empty();
                (line, empty)
            };
            if empty {
                self.bs_mut().buf.lines.push(Vec::new());
            }
            if self.cut_line {
                self.cut.push(line);
            } else {
                self.cut = vec![line];
                self.cut_line = true;
            }
            self.clamp_cursor();
        } else {
            let (frag, gone) = {
                let bs = self.bs_mut();
                let frag: Vec<char> = bs.buf.lines[c.row].drain(c.col..).collect();
                let gone = bs.buf.lines[c.row].is_empty() && bs.buf.lines.len() > 1;
                (frag, gone)
            };
            if gone {
                self.bs_mut().buf.lines.remove(c.row);
            }
            self.cut = vec![frag];
            self.cut_line = false;
            self.clamp_cursor();
        }
        self.finish_step();
        self.edit_invalidate();
    }

    pub(crate) fn paste(&mut self) {
        if self.cut.is_empty() {
            return;
        }
        let (first, last) = match self.sel_span() {
            Some((a, b)) => (a, b + 1),
            None => (self.bs().cursor.row, self.bs().cursor.row + 1),
        };
        self.begin_action(ActionKind::Paste, first, last);
        self.delete_selection_if_any();
        let c = self.bs().cursor;
        if self.cut_line {
            let orig_col = c.col;
            let cut = self.cut.clone();
            let bs = self.bs_mut();
            bs.buf.insert_lines_at(c.row, cut);
            bs.cursor = Pos {
                row: c.row,
                col: orig_col.min(bs.buf.line_len(c.row)),
            };
        } else {
            let first = self.cut[0].clone();
            let rest = self.cut[1..].to_vec();
            let bs = self.bs_mut();
            bs.buf.merge_inline(c.row, c.col, &first);
            if !rest.is_empty() {
                bs.buf.insert_lines_at(c.row + 1, rest);
            }
            bs.cursor.col += first.len();
        }
        self.finish_step();
        self.edit_invalidate();
    }

    /// E1: bracketed paste. \r is stripped, newlines split into rows; the
    /// whole paste (including selection replacement) is ONE undo step.
    pub(crate) fn paste_text(&mut self, s: &str) {
        let frags: Vec<Vec<char>> = s
            .replace('\r', "")
            .split('\n')
            .map(|l| l.chars().collect())
            .collect();
        if frags.iter().all(|f| f.is_empty()) {
            return;
        }
        let (first, last) = match self.sel_span() {
            Some((a, b)) => (a, b + 1),
            None => (self.bs().cursor.row, self.bs().cursor.row + 1),
        };
        self.begin_action(ActionKind::Paste, first, last);
        self.delete_selection_if_any();
        let c = self.bs().cursor;
        let head = &frags[0];
        let rest = &frags[1..];
        let bs = self.bs_mut();
        bs.buf.merge_inline(c.row, c.col, head);
        if !rest.is_empty() {
            bs.buf.insert_lines_at(c.row + 1, rest.to_vec());
        }
        bs.cursor = if rest.is_empty() {
            Pos {
                row: c.row,
                col: c.col + head.len(),
            }
        } else {
            Pos {
                row: c.row + rest.len(),
                col: rest[rest.len() - 1].len(),
            }
        };
        self.finish_step();
        self.edit_invalidate();
    }

    /// M-6: copy the current line (or marked region) to the cutbuffer
    /// without deleting it. The mark stays active.
    pub(crate) fn copy(&mut self) {
        if let Some(mark) = self.bs().mark {
            let (a, b) = normalize(mark, self.bs().cursor);
            if a != b {
                let rows = self.bs_mut().buf.copy_range(a, b);
                self.cut = rows;
                self.cut_line = a.col == 0 && b.row > a.row;
                return;
            }
        }
        let c = self.bs().cursor;
        let line = self.bs().buf.lines[c.row].clone();
        self.cut = vec![line];
        self.cut_line = true;
    }

    pub(crate) fn delete_char_cut(&mut self) {
        let c = self.bs().cursor;
        if c.col < self.bs().buf.line_len(c.row) {
            self.begin_action(ActionKind::Delete, c.row, c.row + 1);
            let ch = {
                let bs = self.bs_mut();
                let ch = bs.buf.lines[c.row][c.col];
                bs.buf.delete_at(c.row, c.col);
                ch
            };
            self.cut = vec![vec![ch]];
            self.cut_line = false;
            self.clamp_cursor();
            self.finish_step();
            self.edit_invalidate();
        }
    }

    // ---------- movement ----------

    pub(crate) fn move_left(&mut self) {
        let c = self.bs().cursor;
        if c.col > 0 {
            self.bs_mut().cursor.col -= 1;
        } else if c.row > 0 {
            let bs = self.bs_mut();
            bs.cursor.row -= 1;
            bs.cursor.col = bs.buf.line_len(bs.cursor.row);
        }
        self.clamp_cursor();
    }

    pub(crate) fn move_right(&mut self) {
        let c = self.bs().cursor;
        if c.col < self.bs().buf.line_len(c.row) {
            self.bs_mut().cursor.col += 1;
        } else if c.row + 1 < self.bs().buf.lines.len() {
            let bs = self.bs_mut();
            bs.cursor.row += 1;
            bs.cursor.col = 0;
        }
    }

    pub(crate) fn move_up(&mut self) {
        if self.bs().cursor.row > 0 {
            let bs = self.bs_mut();
            bs.cursor.row -= 1;
            bs.cursor.col = bs.cursor.col.min(bs.buf.line_len(bs.cursor.row));
        }
    }

    pub(crate) fn move_down(&mut self) {
        if self.bs().cursor.row + 1 < self.bs().buf.lines.len() {
            let bs = self.bs_mut();
            bs.cursor.row += 1;
            bs.cursor.col = bs.cursor.col.min(bs.buf.line_len(bs.cursor.row));
        }
    }

    pub(crate) fn move_home(&mut self) {
        self.bs_mut().cursor.col = 0;
    }

    pub(crate) fn move_end(&mut self) {
        let bs = self.bs_mut();
        bs.cursor.col = bs.buf.line_len(bs.cursor.row);
    }

    pub(crate) fn page_up(&mut self, text_h: usize) {
        let step = text_h.saturating_sub(1).max(1);
        let bs = self.bs_mut();
        bs.cursor.row = bs.cursor.row.saturating_sub(step);
        bs.cursor.col = bs.cursor.col.min(bs.buf.line_len(bs.cursor.row));
    }

    pub(crate) fn page_down(&mut self, text_h: usize) {
        let step = text_h.saturating_sub(1).max(1);
        let bs = self.bs_mut();
        bs.cursor.row = (bs.cursor.row + step).min(bs.buf.lines.len().saturating_sub(1));
        bs.cursor.col = bs.cursor.col.min(bs.buf.line_len(bs.cursor.row));
    }

    pub(crate) fn prev_word(&mut self) {
        let is_word = |ch: char| ch.is_alphanumeric() || ch == '_';
        let mut r = self.bs().cursor.row;
        let mut c = self.bs().cursor.col;
        loop {
            if c == 0 {
                if r == 0 {
                    self.bs_mut().cursor = Pos { row: 0, col: 0 };
                    return;
                }
                r -= 1;
                c = self.bs().buf.line_len(r);
                continue;
            }
            if self.bs().buf.lines[r][c - 1].is_whitespace() {
                c -= 1;
                continue;
            }
            break;
        }
        c -= 1;
        if is_word(self.bs().buf.lines[r][c]) {
            while c > 0 && is_word(self.bs().buf.lines[r][c - 1]) {
                c -= 1;
            }
        }
        self.bs_mut().cursor = Pos { row: r, col: c };
    }

    pub(crate) fn next_line(&mut self) {
        self.move_down();
    }

    pub(crate) fn prev_line(&mut self) {
        self.move_up();
    }

    // Next Word (nano's ^Right / M-N): move forward one word.
    pub(crate) fn next_word(&mut self) {
        let is_word = |ch: char| ch.is_alphanumeric() || ch == '_';
        let mut r = self.bs().cursor.row;
        let mut c = self.bs().cursor.col;
        let nlines = self.bs().buf.lines.len();
        loop {
            let len = self.bs().buf.line_len(r);
            if c >= len {
                if r + 1 >= nlines {
                    return; // already at end of buffer
                }
                r += 1;
                c = 0;
                continue;
            }
            if self.bs().buf.lines[r][c].is_whitespace() {
                c += 1;
                continue;
            }
            break;
        }
        let len = self.bs().buf.line_len(r);
        if is_word(self.bs().buf.lines[r][c]) {
            while c < len && is_word(self.bs().buf.lines[r][c]) {
                c += 1;
            }
        } else {
            c += 1;
        }
        self.bs_mut().cursor = Pos { row: r, col: c };
    }

    // To Bracket (nano's M-]): jump to the bracket matching the one at/near
    // the cursor.
    pub(crate) fn match_bracket(&mut self) {
        let pairs: [(char, char); 3] = [('(', ')'), ('{', '}'), ('[', ']')];
        let start = self.bs().cursor;
        // Copy the at-cursor chars out of the buffer so the search below can
        // borrow it mutably-free while we still test them.
        let window: Vec<char> = {
            let line = &self.bs().buf.lines[start.row];
            (0..5)
                .map(|o| start.col.saturating_add(o))
                .take_while(|&i| i < line.len())
                .map(|i| line[i])
                .collect()
        };
        for (offset, ch) in window.into_iter().enumerate() {
            let idx = start.col.saturating_add(offset);
            for &(open, close) in &pairs {
                let (target, dir) = if ch == open {
                    (close, 1)
                } else if ch == close {
                    (open, -1)
                } else {
                    continue;
                };
                if let Some(pos) = self.find_matching(start.row, idx, target, dir) {
                    self.bs_mut().cursor = pos;
                    return;
                }
            }
        }
        self.flash("No matching bracket");
    }

    /// Find the position of `target` matching the bracket at (row, col),
    /// scanning the whole buffer. `dir` is 1 (forward) or -1 (backward).
    fn find_matching(&self, row: usize, col: usize, target: char, dir: i64) -> Option<Pos> {
        let start_ch = self.bs().buf.lines[row][col];
        let nlines = self.bs().buf.lines.len() as i64;
        let mut depth: i32 = 0;
        let mut r = row as i64;
        let mut c = col as i64;
        loop {
            if r < 0 || r >= nlines {
                return None;
            }
            let line = &self.bs().buf.lines[r as usize];
            let llen = line.len() as i64;
            if c < 0 {
                r -= 1;
                if r < 0 {
                    return None;
                }
                c = self.bs().buf.lines[r as usize].len() as i64 - 1;
                continue;
            }
            if c >= llen {
                r += 1;
                if r >= nlines {
                    return None;
                }
                c = 0;
                continue;
            }
            let ch = line[c as usize];
            if ch == start_ch {
                depth += 1;
            } else if ch == target {
                depth -= 1;
                if depth == 0 {
                    return Some(Pos {
                        row: r as usize,
                        col: c as usize,
                    });
                }
            }
            c += dir;
        }
    }

    // ---------- mark ----------

    pub(crate) fn toggle_mark(&mut self) {
        let bs = self.bs_mut();
        bs.mark = match bs.mark {
            None => Some(bs.cursor),
            Some(_) => None,
        };
    }

    // ---------- goto ----------

    pub(crate) fn start_goto(&mut self) {
        self.prompt = Some(Prompt {
            kind: PromptKind::GoTo,
            text: String::new(),
            cursor: 0,
        });
    }

    pub(crate) fn do_goto(&mut self, text: &str) {
        let digits: String = text.chars().filter(|c| c.is_ascii_digit()).collect();
        let n: usize = digits.parse().unwrap_or(1);
        let n = n.clamp(1, self.bs().buf.lines.len());
        self.bs_mut().cursor = Pos { row: n - 1, col: 0 };
        self.loc_until = Some(Instant::now() + Duration::from_secs(2));
    }

    // ---------- write / read / backup ----------

    pub(crate) fn start_write(&mut self) {
        let name = self
            .bs()
            .buf
            .name
            .clone()
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        self.prompt = Some(Prompt {
            kind: PromptKind::WriteName,
            text: name.clone(),
            cursor: name.chars().count(),
        });
    }

    pub(crate) fn do_write(&mut self, name: String) {
        if name.trim().is_empty() {
            return;
        }
        let path = PathBuf::from(expand_tilde(&name));
        if path.exists() {
            self.pending_write = Some(path);
            self.prompt = Some(Prompt {
                kind: PromptKind::ConfirmOverwrite,
                text: String::new(),
                cursor: 0,
            });
        } else {
            self.save_to(path);
        }
    }

    pub(crate) fn save_to(&mut self, path: PathBuf) {
        let text = self.bs().buf.file_text();
        let bytes = text.len();
        match fs::write(&path, text) {
            Ok(()) => {
                {
                    let bs = self.bs_mut();
                    bs.buf.name = Some(path);
                    bs.buf.modified = false;
                    bs.hl.refresh(&bs.buf);
                }
                // lsp_sync below may kill the client; send any pending
                // change while it is still alive.
                self.lsp_flush(Instant::now());
                self.lsp_sync();
                self.flash(&format!("Wrote {} bytes", bytes));
                if self.quit_after_save {
                    self.quit_after_save = false;
                    // Re-attempt the quit: further modified buffers get the
                    // save prompt, otherwise this quits.
                    self.try_quit();
                }
            }
            Err(e) => {
                self.flash(&format!("Error: {}", e));
            }
        }
    }

    pub(crate) fn start_read(&mut self) {
        self.prompt = Some(Prompt {
            kind: PromptKind::ReadName,
            text: String::new(),
            cursor: 0,
        });
    }

    // F8: open a file — a new buffer when config.multibuffer, else the
    // current buffer is replaced in place.
    pub(crate) fn start_open(&mut self) {
        self.prompt = Some(Prompt {
            kind: PromptKind::OpenName,
            text: String::new(),
            cursor: 0,
        });
    }

    pub(crate) fn do_read(&mut self, name: String) {
        if name.trim().is_empty() {
            return;
        }
        match fs::read_to_string(expand_tilde(&name)) {
            Ok(text) => {
                let mut lines: Vec<Vec<char>> = text.lines().map(|l| l.chars().collect()).collect();
                if lines.is_empty() {
                    lines.push(Vec::new());
                }
                let count = lines.len();
                if self.bs().buf.lines.len() == 1 && self.bs().buf.lines[0].is_empty() {
                    self.begin_action(ActionKind::ReadFile, 0, 1);
                    self.bs_mut().buf.lines = lines;
                    self.bs_mut().cursor = Pos { row: 0, col: 0 };
                } else {
                    let c = self.bs().cursor;
                    // empty before-region: pure insertion at the cursor row
                    self.begin_action(ActionKind::ReadFile, c.row, c.row);
                    self.bs_mut().buf.insert_lines_at(c.row, lines);
                }
                self.finish_step();
                self.edit_invalidate();
                self.flash(&format!("Read {} lines", count));
            }
            Err(e) => {
                self.flash(&format!("Error: {}", e));
            }
        }
    }

    /// F8: open `path` (same read path as startup: Buffer::from_file). With
    /// config.multibuffer the file becomes a NEW buffer that becomes
    /// current; otherwise it replaces the current buffer in place. Returns
    /// false when the read failed — the caller keeps the prompt open.
    pub(crate) fn open_file(&mut self, path: &str) -> bool {
        let path = path.trim();
        if path.is_empty() {
            return true;
        }
        let buf = match Buffer::from_file(Path::new(&expand_tilde(path))) {
            Ok(b) => b,
            Err(e) => {
                self.flash(&format!("Error: {}", e));
                return false;
            }
        };
        let count = buf.lines.len();
        if self.config.multibuffer {
            self.buffers.push(BufferState::new(buf));
            self.cur = self.buffers.len() - 1;
        } else {
            self.buffers[self.cur] = BufferState::new(buf);
        }
        self.lsp_sync();
        self.adjust_scroll(self.text_h);
        self.adjust_scroll_x();
        self.flash(&format!("Read {} lines", count));
        true
    }

    pub(crate) fn start_backup(&mut self) {
        let name = self.bs().buf.name.clone();
        match name {
            Some(p) => {
                let def = format!("{}~", p.display());
                self.prompt = Some(Prompt {
                    kind: PromptKind::BackupName,
                    text: def.clone(),
                    cursor: def.chars().count(),
                });
            }
            None => {
                self.flash("No file name");
            }
        }
    }

    pub(crate) fn do_backup(&mut self, name: String) {
        let Some(src) = self.bs().buf.name.as_ref() else {
            return;
        };
        let dest = expand_tilde(&name);
        match fs::copy(src, &dest) {
            Ok(_) => self.flash(&format!("Backup written to {dest}")),
            Err(e) => self.flash(&format!("Error: {}", e)),
        }
    }

    // ---------- quit ----------

    /// ^X: quit once every buffer has been dealt with. The first modified
    /// buffer at-or-after the current one (wrapping) becomes current and
    /// gets the save prompt; with none modified the editor quits.
    pub(crate) fn try_quit(&mut self) {
        if let Some(i) = self.first_modified_from(self.cur) {
            if i != self.cur {
                self.cur = i;
                self.adjust_scroll(self.text_h);
                self.adjust_scroll_x();
            }
            self.prompt = Some(Prompt {
                kind: PromptKind::ConfirmSave,
                text: String::new(),
                cursor: 0,
            });
            return;
        }
        self.quit = true;
    }

    // Index of the first modified buffer starting at `start`, wrapping.
    fn first_modified_from(&self, start: usize) -> Option<usize> {
        let n = self.buffers.len();
        (0..n)
            .map(|k| (start + k) % n)
            .find(|&i| self.buffers[i].buf.modified)
    }

    pub(crate) fn answer_save(&mut self, c: char) {
        match c {
            'y' | 'Y' => match self.bs().buf.name.clone() {
                Some(p) => {
                    self.quit_after_save = true;
                    self.save_to(p);
                }
                None => {
                    self.quit_after_save = true;
                    self.prompt = Some(Prompt {
                        kind: PromptKind::WriteName,
                        text: String::new(),
                        cursor: 0,
                    });
                }
            },
            'n' | 'N' => {
                // Discard this buffer's changes, then move on to the next
                // modified buffer (or quit when none is left).
                self.bs_mut().buf.modified = false;
                self.try_quit();
            }
            _ => {}
        }
    }

    // ---------- buffers ----------

    /// M-> / M-<: switch to the next/previous buffer, wrapping. All view and
    /// edit state lives in BufferState, so switching only re-points `cur`;
    /// each buffer keeps its own LSP session (lsp_poll services whichever
    /// buffer is current).
    pub(crate) fn switch_buffer(&mut self, dir: isize) {
        let n = self.buffers.len();
        if n < 2 {
            return;
        }
        self.cur = (self.cur as isize + dir).rem_euclid(n as isize) as usize;
        self.adjust_scroll(self.text_h);
        self.adjust_scroll_x();
        let name = self
            .bs()
            .buf
            .name
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "scratch".to_string());
        self.flash(&format!("Buffer: {name}"));
    }

    // ---------- justify / sort ----------

    pub(crate) fn justify(&mut self) {
        // text_w is the full viewport since D6 unified it with draw; the -2
        // keeps the historical wrap width (it used to be subtracted at the
        // size-plumbing site).
        let width = self.text_w.saturating_sub(2).max(20);
        let c = self.bs().cursor.row;
        let mut top = c;
        while top > 0 && !self.bs().buf.lines[top - 1].is_empty() {
            top -= 1;
        }
        let mut bot = c;
        while bot + 1 < self.bs().buf.lines.len() && !self.bs().buf.lines[bot + 1].is_empty() {
            bot += 1;
        }
        let mut words: Vec<String> = Vec::new();
        for r in top..=bot {
            let s: String = self.bs().buf.lines[r].iter().collect();
            words.extend(s.split_whitespace().map(|w| w.to_string()));
        }
        let mut new_lines: Vec<Vec<char>> = Vec::new();
        let mut cur: Vec<char> = Vec::new();
        for w in &words {
            if cur.is_empty() {
                cur.extend(w.chars());
            } else if cur.len() + 1 + w.chars().count() <= width {
                cur.push(' ');
                cur.extend(w.chars());
            } else {
                new_lines.push(std::mem::take(&mut cur));
                cur.extend(w.chars());
            }
        }
        if !cur.is_empty() {
            new_lines.push(cur);
        }
        if new_lines.is_empty() {
            new_lines.push(Vec::new());
        }
        self.begin_action(ActionKind::Justify, top, bot + 1);
        self.bs_mut()
            .buf
            .lines
            .splice(top..=bot, new_lines.iter().cloned());
        self.bs_mut().cursor = Pos { row: top, col: 0 };
        self.bs_mut().mark = None;
        self.finish_step();
        self.edit_invalidate();
    }

    pub(crate) fn sort_lines(&mut self) {
        let (top, bot) = match self.bs().mark {
            Some(mark) => {
                let (a, b) = normalize(mark, self.bs().cursor);
                (a.row, b.row)
            }
            None => (0, self.bs().buf.lines.len().saturating_sub(1)),
        };
        self.begin_action(ActionKind::Sort, top, bot + 1);
        let region: &mut [Vec<char>] = &mut self.bs_mut().buf.lines[top..=bot];
        region.sort_by_cached_key(|l| l.iter().collect::<String>().to_lowercase());
        self.bs_mut().mark = None;
        self.finish_step();
        self.edit_invalidate();
    }

    pub fn char_style(&self, p: Pos) -> Style {
        // Current search match: black on yellow (nano's default). Other
        // matches are not highlighted.
        if let Some((a, b)) = self.current_match_range()
            && a <= p
            && p < b
        {
            return Style::default().fg(Color::Black).bg(Color::Yellow);
        }
        if let Some(mark) = self.bs().mark {
            let (a, b) = normalize(mark, self.bs().cursor);
            if a <= p && p < b {
                // Explicit colors (not REVERSED) so the selection is visible
                // on terminals whose reverse-video comes out white-on-white.
                return Style::default().fg(Color::White).bg(Color::DarkGray);
            }
        }
        // Diagnostics: underline in severity color. Search match and
        // selection return above, so they still win. LSP cols are UTF-16
        // units and can drift on astral chars (accepted limitation).
        for d in &self.bs().lsp_diags {
            if d.line == p.row && d.col <= p.col && p.col < d.end_col {
                let c = match d.severity {
                    1 => Color::Red,
                    2 => Color::Yellow,
                    _ => Color::Blue,
                };
                return Style::default().fg(c).add_modifier(Modifier::UNDERLINED);
            }
        }
        // Syntax highlighting (tree-sitter), below search/selection priority.
        if let Some(s) = self.bs().hl.style_at(p) {
            return s;
        }
        Style::default()
    }
}

pub(crate) fn normalize(a: Pos, b: Pos) -> (Pos, Pos) {
    if a <= b { (a, b) } else { (b, a) }
}

pub(crate) fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}
