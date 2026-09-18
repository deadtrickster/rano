//! Editor core: the Editor model, the undo machinery and the editing
//! actions (input, cut/paste, undo/redo, movement, files, buffers,
//! goto, justify/sort, styling glue).

use std::collections::VecDeque;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crossterm::event::{MouseButton, MouseEvent};
use ratatui::style::{Color, Modifier, Style};

use crate::BufferState;
use crate::buffer::{Buffer, Pos};
use crate::config;
use crate::lsp;
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
    /// Soft line wrap toggle (M-\; seeded from config, F1). When on, long
    /// lines wrap at the viewport edge, `bs.scroll` counts VISUAL rows, and
    /// horizontal scrolling is disabled (scroll_x stays 0).
    pub wrap: bool,
    /// Startup config (F1). tab_width/show_line_numbers/wrap seed from it;
    /// the M-N / M-\ toggles stay runtime-only (no config write-back).
    /// `multibuffer` is unused until F8.
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
    /// Live completion popup (LSP textDocument/completion); None when no
    /// candidates are showing.
    pub completion: Option<CompletionPopup>,
    /// Context each in-flight completion request was made for, in request
    /// order: (row, typed prefix). A response is applied only when it
    /// answers the request for the *current* context — servers can lag
    /// several keystrokes behind, and applying their older answers is what
    /// made the popup show unrelated items.
    pub(crate) completion_q: VecDeque<(usize, String)>,
    /// rust-analyzer answers member completions with a path-completion
    /// fallback until it has finished scanning a freshly opened crate
    /// (~15 s on rano itself). Responses carrying that fallback signature
    /// are dropped and re-requested on this timer instead of flashing junk.
    pub(crate) completion_retry: Option<Instant>,
    pub(crate) completion_retries: u8,
    /// Where M-, returns to, one entry per definition jump (stacked).
    pub def_back: Vec<DefBack>,
}

/// Live completion popup state: the filtered candidate list, the selected
/// row, and the anchor (buffer row + word-start col) used to place the
/// popup on screen and to reject responses for a stale cursor position.
pub struct CompletionPopup {
    pub items: Vec<lsp::CompletionItem>,
    pub sel: usize,
    pub row: usize,
    pub col: usize,
}

/// Case-insensitive subsequence test: does every char of `needle` appear
/// in `hay` in order? Used to keep a server's fuzzy matches (e.g. `__is_long`
/// for `is`) below the exact-prefix ones instead of dropping or promoting
/// them.
fn fuzzy_match(hay: &str, needle: &str) -> bool {
    let mut rest = hay.chars();
    needle
        .chars()
        .all(|n| rest.any(|h| h.eq_ignore_ascii_case(&n)))
}

/// One entry of the jump-to-definition back stack: where to return to when
/// M-, unwinds a jump. Cross-file jumps in single-buffer mode swap the whole
/// buffer state out (edits survive); multibuffer mode remembers the buffer
/// index; same-file jumps only need the cursor.
pub struct DefBack {
    pub buf: Option<BufferState>,
    pub idx: Option<usize>,
    pub pos: Pos,
}

/// The word being completed just before `col` on `row`, plus its start
/// column. Fires for identifiers (`foo|`) and with an empty prefix after a
/// member-access trigger (`foo.|`, `foo::|`). None elsewhere (spaces,
/// punctuation, empty lines) — the caller closes any open popup then.
pub(crate) fn completion_prefix(
    lines: &[Vec<char>],
    row: usize,
    col: usize,
) -> Option<(String, usize)> {
    let line = lines.get(row)?;
    let col = col.min(line.len());
    let prev = if col == 0 { None } else { Some(line[col - 1]) };
    match prev {
        Some(c) if c.is_alphanumeric() || c == '_' => {
            let start = line[..col]
                .iter()
                .rposition(|c| !(c.is_alphanumeric() || *c == '_'))
                .map(|i| i + 1)
                .unwrap_or(0);
            Some((line[start..col].iter().collect(), start))
        }
        Some('.') => Some((String::new(), col)),
        Some(':') if col >= 2 && line[col - 2] == ':' => Some((String::new(), col)),
        _ => None,
    }
}

impl Editor {
    pub(crate) fn new(buf: Buffer, config: config::Config) -> Self {
        let tab_width = config.tab_width;
        let show_line_numbers = config.line_numbers;
        let wrap = config.wrap;
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
            wrap,
            config,
            search_hist: Vec::new(),
            exec_hist: Vec::new(),
            file_hist: Vec::new(),
            hist_idx: None,
            hist_draft: String::new(),
            completion: None,
            completion_q: VecDeque::new(),
            completion_retry: None,
            completion_retries: 0,
            def_back: Vec::new(),
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
        if self.wrap {
            // M-\: scroll counts VISUAL rows; the cursor's visual row must
            // stay inside [scroll, scroll + text_h).
            self.ensure_wrap_prefix();
            let bs = self.bs();
            let total = bs.wrap_prefix.last().copied().unwrap_or(0);
            let max_scroll = total.saturating_sub(text_h);
            let cv = self.visual_pos(bs.cursor);
            let mut scroll = bs.scroll.min(max_scroll);
            if cv < scroll {
                scroll = cv;
            }
            if cv >= scroll + text_h {
                scroll = cv - text_h + 1;
            }
            self.bs_mut().scroll = scroll.min(max_scroll);
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
    /// M-\: with soft wrap on, lines wrap instead of scrolling sideways.
    pub(crate) fn adjust_scroll_x(&mut self) {
        if self.wrap {
            self.bs_mut().scroll_x = 0;
            return;
        }
        let gutter = if self.show_line_numbers {
            ui::gutter_width(self.bs().buf.lines.len())
        } else {
            0
        };
        let view_w = self.text_w.saturating_sub(gutter).max(1);
        let tab_width = self.tab_width;
        let bs = self.bs_mut();
        let row = bs.cursor.row.min(bs.buf.lines.len().saturating_sub(1));
        let line = &bs.buf.lines[row];
        let disp = ui::display_col(line, bs.cursor.col, tab_width);
        bs.scroll_x = bs.scroll_x.min(disp).max((disp + 1).saturating_sub(view_w));
        bs.scroll_x = bs.scroll_x.min(ui::display_width(line, tab_width));
    }

    // ---------- soft wrap (M-\) ----------

    /// Width of the text viewport in display cols (gutter excluded).
    pub(crate) fn view_w(&self) -> usize {
        let g = if self.show_line_numbers {
            ui::gutter_width(self.bs().buf.lines.len())
        } else {
            0
        };
        self.text_w.saturating_sub(g).max(1)
    }

    /// Rebuild the visual-row prefix table when the buffer or the wrap
    /// width changed since the last build. No-op when wrap is off.
    pub(crate) fn ensure_wrap_prefix(&mut self) {
        if !self.wrap {
            return;
        }
        let vw = self.view_w();
        let key = (self.bs().edit_gen, vw);
        let fresh = self.bs().wrap_key == key
            && self.bs().wrap_prefix.len() == self.bs().buf.lines.len() + 1;
        if fresh {
            return;
        }
        let mut prefix = Vec::with_capacity(self.bs().buf.lines.len() + 1);
        prefix.push(0);
        for line in &self.bs().buf.lines {
            // A tab's width depends on the ABSOLUTE display col, which does
            // not reset at wrap boundaries, so the full-line width is
            // exactly the sum of the segment widths.
            let w = ui::display_width(line, self.tab_width);
            prefix.push(*prefix.last().unwrap() + w.div_ceil(vw).max(1));
        }
        let bs = self.bs_mut();
        bs.wrap_prefix = prefix;
        bs.wrap_key = key;
    }

    /// Visual row containing position `p` (wrap on only).
    pub(crate) fn visual_pos(&self, p: Pos) -> usize {
        let bs = self.bs();
        let r = p.row.min(bs.buf.lines.len().saturating_sub(1));
        let line = &bs.buf.lines[r];
        let vw = self.view_w();
        bs.wrap_prefix.get(r).copied().unwrap_or(0)
            + ui::display_col(line, p.col, self.tab_width) / vw
    }

    /// (buffer row, wrap segment) of visual row `v`, clamped to the buffer
    /// (wrap on only).
    pub(crate) fn buf_row_of_visual(&self, v: usize) -> (usize, usize) {
        let bs = self.bs();
        if bs.wrap_prefix.is_empty() {
            return (0, 0);
        }
        let total = bs.wrap_prefix.last().copied().unwrap_or(0);
        let v = v.min(total.saturating_sub(1));
        // wrap_prefix is strictly increasing, so the search is exact.
        let r = match bs.wrap_prefix.binary_search(&v) {
            Ok(i) => i,
            Err(i) => i.saturating_sub(1),
        };
        let r = r.min(bs.buf.lines.len().saturating_sub(1));
        (r, v - bs.wrap_prefix[r])
    }

    /// Char col of display col `d` on buffer row `r` (clamped to EOL).
    fn col_at_disp(&self, r: usize, d: usize) -> usize {
        ui::char_at_display(&self.bs().buf.lines[r], d, self.tab_width)
    }

    pub(crate) fn edit_invalidate(&mut self) {
        let bs = self.bs_mut();
        bs.buf.modified = true;
        bs.search_matches = None;
        bs.search.current = 0;
        bs.hl.refresh(&bs.buf);
        refresh_syntax_diags(bs);
        bs.lsp_dirty = true;
        // Every edit path funnels through here, so this one bump is enough
        // to invalidate the soft-wrap visual-row table (M-\).
        bs.edit_gen = bs.edit_gen.wrapping_add(1);
    }

    /// The union of tree-sitter and LSP diagnostics, row-major — what the
    /// underline, the gutter and M-D all work off.
    pub fn all_diags(&self) -> Vec<lsp::Diagnostic> {
        let bs = self.bs();
        let mut out = bs.syntax_diags.clone();
        out.extend(bs.lsp_diags.iter().cloned());
        out.sort_by_key(|d| (d.line, d.col, d.end_col));
        out
    }

    /// Indentation unit of the current buffer: a tab char if any line
    /// indents with tabs, otherwise the common leading-space width (the GCD,
    /// so mixed 4/8 comes out as 4). Falls back to a tab for flat buffers.
    pub fn indent_unit(&self) -> String {
        let lines = &self.bs().buf.lines;
        if lines.iter().any(|l| l.first() == Some(&'\t')) {
            return "\t".to_string();
        }
        let mut counts: Vec<usize> = lines
            .iter()
            .filter_map(|l| {
                let n = l.iter().take_while(|c| **c == ' ').count();
                (n > 0).then_some(n)
            })
            .collect();
        if counts.is_empty() {
            return "\t".to_string();
        }
        counts.sort();
        let mut unit = counts[0];
        for n in &counts {
            unit = gcd(unit, *n);
        }
        if unit == 0 || unit > 8 {
            return "\t".to_string();
        }
        " ".repeat(unit)
    }

    /// Tab: insert the buffer's indent unit — spaces up to the next unit
    /// boundary for space-indented files (so rano matches its own 4-space
    /// source), a literal tab for tab-indented ones.
    pub fn indent_line(&mut self) {
        let unit = self.indent_unit();
        if unit == "\t" {
            self.insert_char('\t');
            return;
        }
        let col = self.bs().cursor.col;
        let unit_len = unit.len();
        let n = unit_len - (col % unit_len);
        for _ in 0..n {
            self.insert_char(' ');
        }
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

    /// Insert text at the cursor as one undo step, without triggering
    /// completion (used by completion_accept).
    fn insert_str_plain(&mut self, s: &str) {
        let (first, last) = match self.sel_span() {
            Some((a, b)) => (a, b + 1),
            None => (self.bs().cursor.row, self.bs().cursor.row + 1),
        };
        self.begin_action(ActionKind::Insert, first, last);
        self.delete_selection_if_any();
        for ch in s.chars() {
            let bs = self.bs_mut();
            bs.buf.insert_char(bs.cursor.row, bs.cursor.col, ch);
            bs.cursor.col += 1;
        }
        self.finish_step();
        self.edit_invalidate();
    }

    // ---------- completion (LSP) ----------

    /// Ask the language server for completions at the cursor when the
    /// context warrants it (an identifier, or after `.` / `::`), and keep
    /// the popup alive across requests so responses don't flicker it away.
    /// Closes the popup when the cursor leaves completion context.
    pub(crate) fn maybe_request_completion(&mut self) {
        let cur = self.bs().cursor;
        let Some((prefix, start)) = completion_prefix(&self.bs().buf.lines, cur.row, cur.col)
        else {
            self.completion = None;
            self.completion_q.clear();
            return;
        };
        // Push the exact text to the server first: the 300 ms didChange
        // debounce would otherwise complete against a stale document.
        let bs = self.bs_mut();
        let Some(l) = bs.lsp.as_mut() else {
            return;
        };
        if bs.lsp_dirty {
            l.change(&bs.buf.text());
            bs.lsp_dirty = false;
            bs.lsp_last_send = Instant::now();
        }
        let uri = l.doc_uri.clone().unwrap_or_default();
        let line = bs.buf.lines.get(cur.row).cloned().unwrap_or_default();
        l.request_completion(&uri, cur.row as u64, lsp::utf16_col(&line, cur.col) as u64);
        self.completion_q.push_back((cur.row, prefix));
        if self.completion_q.len() > 8 {
            self.completion_q.drain(0..self.completion_q.len() - 8);
        }
        // Keep an existing popup anchored where it first opened: moving it
        // with every keystroke reads as flicker.
        match &mut self.completion {
            Some(p) if p.row == cur.row => {}
            _ => {
                self.completion = Some(CompletionPopup {
                    items: Vec::new(),
                    sel: 0,
                    row: cur.row,
                    col: start,
                });
            }
        }
    }

    /// Apply a completion response. Servers send their raw ranked list
    /// (rust-analyzer marks big member lists `isIncomplete` and leaves the
    /// filtering to the client), so the list is filtered here: items whose
    /// label starts with the typed prefix first — in server order — then
    /// fuzzy (subsequence) matches, everything else dropped. An empty
    /// prefix (right after `.` / `::`) keeps the server's order untouched.
    /// The response is applied only when it answers the request for the
    /// current (row, prefix) — answers for older typing are dropped.
    pub(crate) fn complete_response(&mut self, items: Vec<lsp::CompletionItem>) {
        let Some((req_row, req_prefix)) = self.completion_q.pop_front() else {
            return;
        };
        let cur = self.bs().cursor;
        let Some((prefix, _)) = completion_prefix(&self.bs().buf.lines, cur.row, cur.col) else {
            self.completion = None;
            self.completion_q.clear();
            return;
        };
        if req_row != cur.row || req_prefix != prefix {
            return;
        }
        // A `.` context answered with path items (self:: / crate:: /
        // super::) means the server has not finished scanning the crate
        // yet — member lists never contain those. Park the popup and
        // re-request shortly instead of flashing fallback junk.
        let dot_ctx = cur.col > 0 && self.bs().buf.lines[cur.row].get(cur.col - 1) == Some(&'.');
        if dot_ctx
            && self.completion_retries < 40
            && items.iter().any(|it| {
                it.label.starts_with("self::")
                    || it.label.starts_with("crate::")
                    || it.label.starts_with("super::")
            })
        {
            self.completion_retry = Some(Instant::now() + Duration::from_millis(700));
            self.completion_retries += 1;
            return;
        }
        self.completion_retries = 0;
        self.completion_retry = None;
        let Some(p) = &mut self.completion else {
            self.completion_q.clear();
            return;
        };
        if p.row != cur.row {
            self.completion = None;
            self.completion_q.clear();
            return;
        }
        // The server's JSON order is not its ranked order: sortText is.
        let mut items = items;
        items.sort_by(|a, b| a.sort.cmp(&b.sort));
        let mut exact = Vec::new();
        let mut fuzzy = Vec::new();
        for it in items {
            if prefix.is_empty() {
                exact.push(it);
            } else {
                let hay = if it.filter.is_empty() {
                    it.label.as_str()
                } else {
                    it.filter.as_str()
                };
                if hay.starts_with(&prefix) {
                    exact.push(it);
                } else if fuzzy_match(hay, &prefix) {
                    fuzzy.push(it);
                }
            }
        }
        if exact.is_empty() && fuzzy.is_empty() {
            self.completion = None;
            return;
        }
        exact.append(&mut fuzzy);
        p.items = exact;
        p.sel = 0;
    }

    pub(crate) fn completion_up(&mut self) {
        if let Some(p) = &mut self.completion
            && !p.items.is_empty()
        {
            p.sel = (p.sel + p.items.len() - 1) % p.items.len();
        }
    }

    pub(crate) fn completion_down(&mut self) {
        if let Some(p) = &mut self.completion
            && !p.items.is_empty()
        {
            p.sel = (p.sel + 1) % p.items.len();
        }
    }

    pub(crate) fn completion_close(&mut self) {
        self.completion = None;
        self.completion_q.clear();
        self.completion_retry = None;
        self.completion_retries = 0;
    }

    /// Fire a scheduled completion re-request once its timer elapses. The
    /// popup must still be open on the cursor's row with a completable
    /// prefix, otherwise the wait is dropped.
    pub(crate) fn completion_retry_poll(&mut self) {
        let Some(t) = self.completion_retry else {
            return;
        };
        if Instant::now() < t {
            return;
        }
        self.completion_retry = None;
        let cur = self.bs().cursor;
        if !matches!(&self.completion, Some(p) if p.row == cur.row) {
            self.completion_retries = 0;
            return;
        }
        if completion_prefix(&self.bs().buf.lines, cur.row, cur.col).is_none() {
            self.completion_retries = 0;
            self.completion = None;
            return;
        }
        self.maybe_request_completion();
    }

    /// Insert the selected completion: append the part past the typed
    /// prefix, or replace the prefix when the item's text diverges from it.
    pub(crate) fn completion_accept(&mut self) {
        let Some(p) = self.completion.take() else {
            return;
        };
        if p.items.is_empty() {
            return;
        }
        let item = p.items[p.sel].clone();
        let cur = self.bs().cursor;
        if cur.row != p.row {
            return;
        }
        let Some((prefix, _)) = completion_prefix(&self.bs().buf.lines, cur.row, cur.col) else {
            return;
        };
        match item.insert.strip_prefix(prefix.as_str()) {
            Some("") => {}
            Some(rest) => self.insert_str_plain(rest),
            None => {
                for _ in 0..prefix.chars().count() {
                    self.backspace();
                }
                self.insert_str_plain(&item.insert);
            }
        }
    }

    // ---------- jump to definition (M-. / M-,) ----------

    /// M-.: ask the language server for the definition of the symbol under
    /// the cursor and jump there — within the file, or into another file
    /// (pushing the current position onto the M-, back stack).
    pub(crate) fn jump_definition(&mut self) {
        let cur = self.bs().cursor;
        let bs = self.bs_mut();
        let Some(l) = bs.lsp.as_mut() else {
            self.completion_close();
            self.flash("No LSP server");
            return;
        };
        // The position must match the server's view of the document.
        if bs.lsp_dirty {
            l.change(&bs.buf.text());
            bs.lsp_dirty = false;
            bs.lsp_last_send = Instant::now();
        }
        let uri = l.doc_uri.clone().unwrap_or_default();
        let line = bs.buf.lines.get(cur.row).cloned().unwrap_or_default();
        let loc = match l.definition(
            &uri,
            cur.row as u64,
            lsp::utf16_col(&line, cur.col) as u64,
            Duration::from_secs(3),
        ) {
            Ok(x) => x,
            Err(e) => {
                self.flash(&e);
                return;
            }
        };
        let Some(loc) = loc else {
            self.flash("No definition found");
            return;
        };
        self.goto_location(loc, cur);
    }

    /// Move to a definition target, pushing a back-stack entry first.
    pub(crate) fn goto_location(&mut self, loc: lsp::DefLocation, from: Pos) {
        let target_path = lsp::uri_to_path(&loc.uri);
        let cur_name = self.bs().buf.name.clone();
        let same_file = match &cur_name {
            Some(p) => lsp::path_to_uri(p) == loc.uri,
            None => false,
        };
        if same_file {
            self.def_back.push(DefBack {
                buf: None,
                idx: None,
                pos: from,
            });
        } else {
            let buf = match Buffer::from_file(&target_path) {
                Ok(b) => b,
                Err(e) => {
                    self.flash(&format!("Error: {}", e));
                    return;
                }
            };
            if self.config.multibuffer {
                // The origin buffer stays alive in self.buffers.
                self.def_back.push(DefBack {
                    buf: None,
                    idx: Some(self.cur),
                    pos: from,
                });
                self.buffers.push(BufferState::new(buf));
                self.cur = self.buffers.len() - 1;
            } else {
                // Swap the current buffer out whole (edits survive on M-,).
                let old = std::mem::replace(&mut self.buffers[self.cur], BufferState::new(buf));
                self.def_back.push(DefBack {
                    buf: Some(old),
                    idx: None,
                    pos: from,
                });
            }
            self.lsp_sync();
        }
        let row = loc.line as usize;
        let line = self.bs().buf.lines.get(row).cloned().unwrap_or_default();
        let col = lsp::utf16_to_char(&line, loc.character as usize);
        self.bs_mut().cursor = Pos { row, col };
        self.completion_close();
        self.adjust_scroll(self.text_h);
        self.adjust_scroll_x();
    }

    /// M-,: unwind one definition jump (stacked — press repeatedly).
    pub(crate) fn jump_back(&mut self) {
        let Some(entry) = self.def_back.pop() else {
            self.flash("No jump to return to");
            return;
        };
        if let Some(bs) = entry.buf {
            self.buffers[self.cur] = bs;
            self.lsp_sync();
        } else if let Some(i) = entry.idx
            && i < self.buffers.len()
        {
            self.cur = i;
        }
        let pos = entry.pos;
        let bs = self.bs_mut();
        bs.cursor = bs.buf.clamp(pos);
        self.adjust_scroll(self.text_h);
        self.adjust_scroll_x();
    }

    // ---------- mouse ----------

    /// Map a pane cell to a buffer position: only clicks inside the text
    /// area land; the title, status and function bars are ignored, and a
    /// click on the gutter or past EOL goes to the line start / line end.
    fn mouse_pos(&self, pane_row: u16, pane_col: u16) -> Option<Pos> {
        if self.prompt.is_some() || pane_row == 0 || pane_row as usize > self.text_h {
            return None;
        }
        let bs = self.bs();
        // M-\: the pane row is a VISUAL row; map it to (buffer row, wrap
        // segment). With wrap off, seg is 0 and this is the old arithmetic.
        let (row, seg) = if self.wrap {
            let v = pane_row as usize - 1 + bs.scroll;
            let total = bs.wrap_prefix.last().copied().unwrap_or(0);
            if v >= total {
                return None;
            }
            self.buf_row_of_visual(v)
        } else {
            (pane_row as usize - 1 + bs.scroll, 0)
        };
        if row >= bs.buf.lines.len() {
            return None;
        }
        let g = if self.show_line_numbers {
            ui::gutter_width(bs.buf.lines.len())
        } else {
            0
        };
        let x = pane_col as usize;
        if x < g {
            return Some(Pos { row, col: 0 });
        }
        let disp = x - g + seg * self.view_w() + bs.scroll_x;
        let line = &bs.buf.lines[row];
        Some(Pos {
            row,
            col: ui::char_at_display(line, disp, self.tab_width),
        })
    }

    /// Left click: cursor + fresh selection anchor. Left drag: extend.
    /// Wheel: scroll the viewport a few lines. Everything else is ignored.
    pub(crate) fn handle_mouse(&mut self, m: MouseEvent) -> bool {
        self.ensure_wrap_prefix();
        use crossterm::event::MouseEventKind as K;
        match m.kind {
            K::ScrollUp => self.wheel(-3),
            K::ScrollDown => self.wheel(3),
            K::Down(MouseButton::Left) => match self.mouse_pos(m.row, m.column) {
                Some(p) => {
                    let bs = self.bs_mut();
                    bs.cursor = bs.buf.clamp(p);
                    bs.mark = Some(bs.cursor);
                    self.completion_close();
                    true
                }
                None => false,
            },
            K::Drag(MouseButton::Left) => match self.mouse_pos(m.row, m.column) {
                Some(p) => {
                    let bs = self.bs_mut();
                    bs.cursor = bs.buf.clamp(p);
                    true
                }
                None => false,
            },
            _ => false,
        }
    }

    /// Wheel: scroll the viewport without moving the edit point; the cursor
    /// is pulled along only when the scroll would push it out of the view
    /// (nano-style — it pins to the edge it would cross, so it stays on a
    /// real line and the view follows it from there).
    fn wheel(&mut self, delta: i64) -> bool {
        let text_h = self.text_h;
        if self.wrap {
            // M-\: scroll in visual rows; the cursor is pinned to the edge
            // it would cross, at the start/end of that edge's visual row.
            self.ensure_wrap_prefix();
            let total = self.bs().wrap_prefix.last().copied().unwrap_or(0);
            let max_scroll = total.saturating_sub(text_h);
            let scroll = (self.bs().scroll as i64 + delta).clamp(0, max_scroll as i64) as usize;
            let cv = self.visual_pos(self.bs().cursor);
            let vw = self.view_w();
            let pin = if cv < scroll {
                let (r, seg) = self.buf_row_of_visual(scroll);
                Some(Pos {
                    row: r,
                    col: self.col_at_disp(r, seg * vw),
                })
            } else if text_h > 0 && cv >= scroll + text_h {
                let (r, seg) = self.buf_row_of_visual(scroll + text_h - 1);
                Some(Pos {
                    row: r,
                    col: self.col_at_disp(r, (seg + 1) * vw),
                })
            } else {
                None
            };
            let bs = self.bs_mut();
            bs.scroll = scroll;
            if let Some(p) = pin {
                bs.cursor = bs.buf.clamp(p);
            }
            return true;
        }
        let bs = self.bs_mut();
        let max_scroll = bs.buf.lines.len().saturating_sub(text_h);
        let scroll = (bs.scroll as i64 + delta).clamp(0, max_scroll as i64) as usize;
        bs.scroll = scroll;
        if bs.cursor.row < scroll {
            bs.cursor.row = scroll;
        } else if text_h > 0 && bs.cursor.row >= scroll + text_h {
            bs.cursor.row = scroll + text_h - 1;
        }
        bs.cursor = bs.buf.clamp(bs.cursor);
        true
    }

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
        self.maybe_request_completion();
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
        // (up to the cursor) onto the new row. Electric: an opening brace at
        // the end of the line puts the cursor one indent unit deeper.
        let indent: Vec<char> = if self.config.auto_indent {
            let mut ind: Vec<char> = self.bs().buf.lines[row]
                .iter()
                .take(col)
                .take_while(|c| c.is_whitespace())
                .copied()
                .collect();
            let opens = self.bs().buf.lines[row][..col]
                .iter()
                .rev()
                .find(|c| !c.is_whitespace())
                == Some(&'{');
            if opens {
                ind.extend(self.indent_unit().chars());
            }
            ind
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
        let had_popup = self.completion.is_some();
        if sel.is_none() && !(c.col > 0 || c.row > 0) {
            if had_popup {
                self.maybe_request_completion();
            }
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
            if had_popup {
                self.maybe_request_completion();
            }
            return;
        }
        if c.col > 0 {
            // Soft tabs: on leading whitespace, backspace eats a whole
            // indent unit down to the previous boundary, not one space.
            let unit = self.indent_unit();
            let line = self.bs().buf.lines[c.row].clone();
            let on_indent =
                unit != "\t" && c.col <= line.len() && line[..c.col].iter().all(|ch| *ch == ' ');
            let n = if on_indent {
                ((c.col - 1) % unit.len()) + 1
            } else {
                1
            };
            let bs = self.bs_mut();
            for _ in 0..n {
                // buf.backspace removes a row it empties; stop before the
                // row index goes stale and let clamp_cursor settle it.
                if bs.cursor.col == 0 || bs.buf.lines.get(c.row).is_none() {
                    break;
                }
                bs.buf.backspace(c.row, bs.cursor.col);
                bs.cursor.col -= 1;
            }
            self.clamp_cursor();
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
        if had_popup {
            self.maybe_request_completion();
        }
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

    /// Move the cursor to visual row `v`, keeping the current column WITHIN
    /// the visual row (clamped to the target row's length). Wrap on only.
    fn goto_visual_row(&mut self, v: usize) {
        let c = self.bs().cursor;
        let disp = ui::display_col(&self.bs().buf.lines[c.row], c.col, self.tab_width);
        let vw = self.view_w();
        let (r, seg) = self.buf_row_of_visual(v);
        let col = self.col_at_disp(r, seg * vw + disp % vw);
        self.bs_mut().cursor = Pos { row: r, col };
    }

    pub(crate) fn move_up(&mut self) {
        if self.wrap {
            // One VISUAL row up: a wrapped line is traversed segment by
            // segment before the cursor reaches the previous buffer row.
            let cv = self.visual_pos(self.bs().cursor);
            if cv > 0 {
                self.goto_visual_row(cv - 1);
            }
            return;
        }
        if self.bs().cursor.row > 0 {
            let bs = self.bs_mut();
            bs.cursor.row -= 1;
            bs.cursor.col = bs.cursor.col.min(bs.buf.line_len(bs.cursor.row));
        }
    }

    pub(crate) fn move_down(&mut self) {
        if self.wrap {
            let bs = self.bs();
            let total = bs.wrap_prefix.last().copied().unwrap_or(0);
            let cv = self.visual_pos(bs.cursor);
            if cv + 1 < total {
                self.goto_visual_row(cv + 1);
            }
            return;
        }
        if self.bs().cursor.row + 1 < self.bs().buf.lines.len() {
            let bs = self.bs_mut();
            bs.cursor.row += 1;
            bs.cursor.col = bs.cursor.col.min(bs.buf.line_len(bs.cursor.row));
        }
    }

    pub(crate) fn move_home(&mut self) {
        if self.wrap {
            // Start of the VISUAL row, not of the buffer row.
            let cv = self.visual_pos(self.bs().cursor);
            let (r, seg) = self.buf_row_of_visual(cv);
            self.bs_mut().cursor.col = self.col_at_disp(r, seg * self.view_w());
            return;
        }
        self.bs_mut().cursor.col = 0;
    }

    pub(crate) fn move_end(&mut self) {
        if self.wrap {
            // End of the VISUAL row, not of the buffer row.
            let cv = self.visual_pos(self.bs().cursor);
            let (r, seg) = self.buf_row_of_visual(cv);
            self.bs_mut().cursor.col = self.col_at_disp(r, (seg + 1) * self.view_w());
            return;
        }
        let bs = self.bs_mut();
        bs.cursor.col = bs.buf.line_len(bs.cursor.row);
    }

    pub(crate) fn page_up(&mut self, text_h: usize) {
        let step = text_h.saturating_sub(1).max(1);
        if self.wrap {
            let cv = self.visual_pos(self.bs().cursor);
            self.goto_visual_row(cv.saturating_sub(step));
            return;
        }
        let bs = self.bs_mut();
        bs.cursor.row = bs.cursor.row.saturating_sub(step);
        bs.cursor.col = bs.cursor.col.min(bs.buf.line_len(bs.cursor.row));
    }

    pub(crate) fn page_down(&mut self, text_h: usize) {
        let step = text_h.saturating_sub(1).max(1);
        if self.wrap {
            let total = self.bs().wrap_prefix.last().copied().unwrap_or(0);
            let cv = self.visual_pos(self.bs().cursor);
            self.goto_visual_row((cv + step).min(total.saturating_sub(1)));
            return;
        }
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
                    refresh_syntax_diags(bs);
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

    /// Style for the character at `p`, given the frame's merged diagnostics
    /// (see `ui::draw`, which computes them once per frame — `all_diags`
    /// clones and sorts, so it must not run per character).
    pub fn char_style_with(&self, p: Pos, diags: &[lsp::Diagnostic]) -> Style {
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
        for d in diags {
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

/// Rebuild `syntax_diags` from the fresh tree-sitter parse. Called after
/// every re-highlight (open, read, edit) so mistakes are visible even with
/// no language server.
pub(crate) fn refresh_syntax_diags(bs: &mut BufferState) {
    bs.syntax_diags = bs
        .hl
        .syntax_errors(&bs.buf.lines)
        .into_iter()
        .map(|(line, col, end_col, message)| lsp::Diagnostic {
            line,
            col,
            end_col,
            message,
            severity: 1,
        })
        .collect();
    widen_zero_width(&mut bs.syntax_diags, &bs.buf.lines);
}

/// rust-analyzer reports many syntax errors as zero-width insertion points,
/// which rano's `col <= p.col < end_col` underline can never match. Widen
/// those to one visible column: the char under the insertion point, or the
/// last char of the line when the point is at EOL.
pub(crate) fn widen_zero_width(diags: &mut [lsp::Diagnostic], lines: &[Vec<char>]) {
    for d in diags {
        if d.end_col > d.col {
            continue;
        }
        let line_len = lines.get(d.line).map(Vec::len).unwrap_or(0);
        if d.col >= line_len && line_len > 0 {
            d.col = line_len - 1;
        }
        d.end_col = d.col + 1;
    }
}

fn gcd(mut a: usize, mut b: usize) -> usize {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}
