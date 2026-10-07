//! A diff drawn over the text by the library's renderers, split or unified
//! (`s` toggles, remembered for the session). Three things open it:
//!
//! - **the "File changed on disk" question** (`d`): what saving would do to
//!   the file. It answers to the question — `y` and `n` answer from here, and
//!   leaving comes back to it;
//! - **M-P on a diff or patch buffer**: the patch drawn file by file, hunk by
//!   hunk, at each file's own line numbers ([`rano::patch`]);
//! - **M-P on a buffer with merge conflicts**: ours against theirs, side by
//!   side ([`rano::conflict`]).
//!
//! The sources are kept and the lines re-rendered only when the width or the
//! view changes — a diff and a highlight of two whole files is not per-frame
//! work. The two M-P views are snapshots of the buffer when they opened.

use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use rano::diff::DiffConfig;
use rano::sidediff::{EditView, edit_view, render_edit_view};
use rano::style::Palette;
use ratatui::text::Line;

use crate::editor::Editor;
use crate::syntax::Lang;

/// What a diff view shows.
pub enum Source {
    /// The file on disk against the buffer, from the save question.
    Save {
        path: PathBuf,
        disk: String,
        mine: String,
    },
    /// A diff or patch buffer's text.
    Patch { name: String, text: String },
    /// A buffer with conflict markers.
    Conflict { name: String, text: String },
}

pub struct DiffView {
    pub source: Source,
    pub split: bool,
    /// First rendered line shown.
    pub top: usize,
    /// The width `lines` were rendered for.
    width: usize,
    pub lines: Vec<Line<'static>>,
}

impl DiffView {
    fn new(source: Source, split: bool, width: usize) -> Self {
        let mut v = DiffView {
            source,
            split,
            top: 0,
            width: 0,
            lines: Vec::new(),
        };
        v.render(width);
        v
    }

    fn render(&mut self, width: usize) {
        let cfg = DiffConfig {
            width,
            palette: Palette::Colour,
            context: 3,
            line_numbers: true,
            intra_line: true,
            // The view scrolls, so nothing is dropped.
            max_rows: usize::MAX,
        };
        let view = edit_view(self.split);
        self.lines = match &self.source {
            Source::Save { path, disk, mine } => {
                let shown = path.display().to_string();
                render_edit_view(&shown, disk, mine, 1, 1, &cfg, view)
            }
            Source::Patch { text, .. } => {
                rano::patch::render(&rano::patch::parse(text), &cfg, view)
            }
            Source::Conflict { name, text } => {
                rano::conflict::render(name, text, &cfg, view).unwrap_or_default()
            }
        };
        self.width = width;
        self.top = self.top.min(self.lines.len().saturating_sub(1));
    }

    pub fn view(&self) -> EditView {
        edit_view(self.split)
    }

    /// The reversed row above the lines: what this is, the view, the keys.
    pub fn header(&self) -> String {
        let view = match self.view() {
            EditView::Split => "split",
            EditView::Unified => "unified",
        };
        match &self.source {
            Source::Save { path, .. } => format!(
                " Saving would change {} ({view})   s: split/unified  y: save anyway  n: don't  Esc: back",
                path.display()
            ),
            Source::Patch { name, .. } => {
                format!(" Patch {name} ({view})   s: split/unified  Esc: back to the text")
            }
            Source::Conflict { name, .. } => {
                format!(" Conflicts in {name} ({view})   s: split/unified  Esc: back to the text")
            }
        }
    }

    fn answers_save(&self) -> bool {
        matches!(self.source, Source::Save { .. })
    }
}

/// Rows of the text area the diff lines get: all of it but the header.
pub(crate) fn body_rows(text_h: usize) -> usize {
    text_h.saturating_sub(1).max(1)
}

impl Editor {
    /// Open the diff of `disk` (the file now) against `mine` (the buffer as a
    /// save would write it), from the save question.
    pub(crate) fn open_diff_view(&mut self, path: &std::path::Path, disk: String, mine: String) {
        self.open_view(Source::Save {
            path: path.to_path_buf(),
            disk,
            mine,
        });
    }

    fn open_view(&mut self, source: Source) {
        self.completion_close();
        self.diff_view = Some(DiffView::new(source, self.diff_split, self.text_w));
    }

    /// M-P: the current buffer rendered — a patch file hunk by hunk, a file
    /// with merge conflicts ours against theirs. Anything else says why not.
    pub(crate) fn toggle_rendered_view(&mut self) {
        let name = self.buffer_name(self.cur);
        let text = self.bs().buf.text();
        let named = self.bs().buf.name.as_deref();
        let first = self
            .bs()
            .buf
            .row_opt(0)
            .map(|l| l.iter().collect::<String>());
        // Conflicts first: a patch file with markers in it is mid-merge too,
        // and the merge is what needs reading.
        if rano::conflict::has_conflicts(&text) {
            self.open_view(Source::Conflict { name, text });
        } else if crate::syntax::detect(named, first.as_deref()) == Some(Lang::Diff)
            || looks_like_a_patch(&text)
        {
            self.open_view(Source::Patch { name, text });
        } else {
            self.flash("Nothing to render: not a diff or patch, and no merge conflicts");
        }
    }

    /// Re-render after a resize. Returns whether anything changed.
    pub(crate) fn refresh_diff_view(&mut self) -> bool {
        let w = self.text_w;
        match self.diff_view.as_mut() {
            Some(v) if v.width != w => {
                v.render(w);
                true
            }
            _ => false,
        }
    }

    pub(crate) fn handle_diff_key(&mut self, mut v: DiffView, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let page = body_rows(self.text_h);
        let last = v.lines.len().saturating_sub(page);
        // Leaving: back to the save question, or back to the text.
        let leave = |ed: &mut Editor, v: &DiffView| {
            if v.answers_save() {
                ed.reask_external();
            }
        };
        match key.code {
            KeyCode::Char(c @ ('y' | 'Y' | 'n' | 'N')) if !ctrl && !alt && v.answers_save() => {
                self.answer_external(c);
                return;
            }
            KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') if !ctrl && !alt => {
                leave(self, &v);
                return;
            }
            KeyCode::Char('d') if !ctrl && !alt && v.answers_save() => {
                leave(self, &v);
                return;
            }
            // The key that opened it closes it.
            KeyCode::Char('p' | 'P') if alt && !v.answers_save() => return,
            KeyCode::Char('g' | 'c') if ctrl => {
                leave(self, &v);
                return;
            }
            // s: split ↔ unified, remembered for the next diff.
            KeyCode::Char('s') | KeyCode::Tab if !ctrl && !alt => {
                v.split = !v.split;
                self.diff_split = v.split;
                v.top = 0;
                v.render(self.text_w);
            }
            KeyCode::Up => v.top = v.top.saturating_sub(1),
            KeyCode::Char('p') if ctrl => v.top = v.top.saturating_sub(1),
            KeyCode::Down => v.top += 1,
            KeyCode::Char('n') if ctrl => v.top += 1,
            KeyCode::PageUp => v.top = v.top.saturating_sub(page),
            KeyCode::PageDown | KeyCode::Char(' ') => v.top += page,
            KeyCode::Home => v.top = 0,
            KeyCode::End => v.top = last,
            _ => {}
        }
        // Never so far down that the last page is short.
        let last = v.lines.len().saturating_sub(page);
        v.top = v.top.min(last);
        self.diff_view = Some(v);
    }
}

/// A buffer with no diff name can still be one: `git diff > out` into an
/// unnamed buffer, or a file without the extension. It is one when it has a
/// file header and a hunk.
fn looks_like_a_patch(text: &str) -> bool {
    !rano::patch::parse(text).files.is_empty() && text.lines().any(|l| l.starts_with("@@ -"))
}
