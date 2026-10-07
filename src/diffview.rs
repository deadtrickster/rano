//! The external-change diff: what saving would do to a file something else
//! wrote since it was read, drawn over the text by the library's renderers
//! ([`rano::sidediff`] / [`rano::diff`]), split or unified.
//!
//! It is opened from the "File changed on disk" question and answers to it: `y`
//! and `n` answer from here, and leaving comes back to the question. The two
//! texts are kept and the lines re-rendered only when the width or the view
//! changes — a diff and a highlight of two whole files is not per-frame work.

use std::path::{Path, PathBuf};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use rano::diff::DiffConfig;
use rano::sidediff::{EditView, edit_view, render_edit_view};
use rano::style::Palette;
use ratatui::text::Line;

use crate::editor::Editor;

pub struct DiffView {
    pub path: PathBuf,
    /// The file as it is on disk now.
    disk: String,
    /// The buffer as a save would write it.
    mine: String,
    pub split: bool,
    /// First rendered line shown.
    pub top: usize,
    /// The width `lines` were rendered for.
    width: usize,
    pub lines: Vec<Line<'static>>,
}

impl DiffView {
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
        let shown = self.path.display().to_string();
        self.lines = render_edit_view(
            &shown,
            &self.disk,
            &self.mine,
            1,
            1,
            &cfg,
            edit_view(self.split),
        );
        self.width = width;
        self.top = self.top.min(self.lines.len().saturating_sub(1));
    }

    pub fn view(&self) -> EditView {
        edit_view(self.split)
    }
}

/// Rows of the text area the diff lines get: all of it but the header.
pub(crate) fn body_rows(text_h: usize) -> usize {
    text_h.saturating_sub(1).max(1)
}

impl Editor {
    /// Open the diff of `disk` (the file now) against `mine` (the buffer as a
    /// save would write it). The split/unified choice is the last one made in
    /// this session.
    pub(crate) fn open_diff_view(&mut self, path: &Path, disk: String, mine: String) {
        self.completion_close();
        let mut v = DiffView {
            path: path.to_path_buf(),
            disk,
            mine,
            split: self.diff_split,
            top: 0,
            width: 0,
            lines: Vec::new(),
        };
        v.render(self.text_w);
        self.diff_view = Some(v);
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
        match key.code {
            KeyCode::Char(c @ ('y' | 'Y' | 'n' | 'N')) if !ctrl && !alt => {
                self.answer_external(c);
                return;
            }
            KeyCode::Esc | KeyCode::Enter | KeyCode::Char('d' | 'q') if !ctrl && !alt => {
                self.reask_external();
                return;
            }
            KeyCode::Char('g' | 'c') if ctrl => {
                self.reask_external();
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
        v.top = v.top.min(last);
        self.diff_view = Some(v);
    }
}
