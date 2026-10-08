//! A diff drawn over the text by the library's renderers, split or unified
//! (`s` toggles, remembered for the session). Three things open it:
//!
//! - **the "File changed on disk" question** (`d`): what saving would do to
//!   the file. It answers to the question — `y` and `n` answer from here, and
//!   leaving comes back to it;
//! - **M-P on a diff or patch buffer**: the patch drawn file by file, hunk by
//!   hunk, at each file's own line numbers ([`crate::patch`]);
//! - **M-P on a buffer with merge conflicts**: ours against theirs, side by
//!   side ([`crate::conflict`]);
//! - **a host's review** ([`Editor::open_review`]): one change to the file,
//!   as the host saw it made, at the file's own line numbers — and M-P on that
//!   buffer shows it again ([`crate::review`]).
//!
//! The sources are kept and the lines re-rendered only when the width or the
//! view changes — a diff and a highlight of two whole files is not per-frame
//! work. The two M-P views are snapshots of the buffer when they opened.

use std::path::PathBuf;

use crate::conflict::{Compare, Take};
use crate::diff::DiffConfig;
use crate::render::Line;
use crate::sidediff::{EditView, edit_view, render_edit_view};
use crate::style::Palette;

use crate::buffer::Pos;
use crate::editor::{ActionKind, Editor};
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
    /// A buffer with conflict markers, and where the reader is in it.
    Conflict {
        name: String,
        text: String,
        /// The conflict the keys act on (0-based, file order).
        current: usize,
        compare: Compare,
        /// Each conflict's header row in `lines`, from the last render.
        sections: Vec<usize>,
    },
    /// A change a host asked the reader to review.
    Review {
        name: String,
        change: crate::review::Change,
    },
}

pub struct DiffView {
    pub source: Source,
    pub split: bool,
    /// First rendered line shown.
    pub top: usize,
    /// The width `lines` were rendered for.
    width: usize,
    pub lines: Vec<Line>,
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
        self.lines = match &mut self.source {
            Source::Save { path, disk, mine } => {
                let shown = path.display().to_string();
                render_edit_view(&shown, disk, mine, 1, 1, &cfg, view)
            }
            Source::Patch { text, .. } => {
                crate::patch::render(&crate::patch::parse(text), &cfg, view)
            }
            Source::Conflict {
                name,
                text,
                current,
                compare,
                sections,
            } => match crate::conflict::render_view(name, text, &cfg, view, *compare, *current) {
                Some(v) => {
                    *sections = v.sections;
                    v.lines
                }
                None => {
                    sections.clear();
                    Vec::new()
                }
            },
            Source::Review { name, change } => render_edit_view(
                name,
                &change.before,
                &change.after,
                change.before_start.max(1),
                change.after_start.max(1),
                &cfg,
                view,
            ),
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
            Source::Review { name, .. } => format!(
                " Review {name} ({view})   s: split/unified  M-s: send your place  Esc: to the change in the text"
            ),
            Source::Conflict {
                current,
                sections,
                compare,
                ..
            } => {
                let cmp = match compare {
                    Compare::OursTheirs => "ours/theirs",
                    Compare::BaseOurs => "base/ours",
                    Compare::BaseTheirs => "base/theirs",
                };
                format!(
                    " Conflict {}/{} ({view}, {cmp})   n/p: next/prev  o/t: take ours/theirs  b/B: both  c: compare  s: split  Esc: back",
                    current + 1,
                    sections.len()
                )
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

    pub(crate) fn open_view(&mut self, source: Source) {
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
        if crate::conflict::has_conflicts(&text) {
            self.open_view(Source::Conflict {
                name,
                text,
                current: 0,
                compare: Compare::OursTheirs,
                sections: Vec::new(),
            });
        } else if crate::syntax::detect(named, first.as_deref()) == Some(Lang::Diff)
            || looks_like_a_patch(&text)
        {
            self.open_view(Source::Patch { name, text });
        } else if !self.show_review() {
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

    /// One action in the diff view, from a command (see `commands.rs`, whose
    /// keymaps say which key does what in each kind of view).
    pub(crate) fn diff_act(&mut self, act: DiffAct) {
        let Some(mut v) = self.diff_view.take() else {
            return;
        };
        let page = body_rows(self.text_h);
        let last = v.lines.len().saturating_sub(page);
        match act {
            DiffAct::Conflict(a) => return self.conflict_act(v, a),
            DiffAct::Answer(c) => {
                if v.answers_save() {
                    self.answer_external(c);
                } else {
                    self.diff_view = Some(v);
                }
                return;
            }
            // Leaving: back to the save question, or back to the text.
            DiffAct::Close => {
                if v.answers_save() {
                    self.reask_external();
                }
                return;
            }
            // Split ↔ unified, remembered for the next diff.
            DiffAct::ToggleSplit => {
                v.split = !v.split;
                self.diff_split = v.split;
                v.top = 0;
                v.render(self.text_w);
            }
            DiffAct::Scroll(d) => v.top = (v.top as isize + d).max(0) as usize,
            DiffAct::Page(d) => v.top = (v.top as isize + d * page as isize).max(0) as usize,
            DiffAct::Top => v.top = 0,
            DiffAct::Bottom => v.top = last,
        }
        // Never so far down that the last page is short.
        v.top = v.top.min(last);
        self.diff_view = Some(v);
    }

    /// Which kind of diff is open, for choosing its keymap.
    pub(crate) fn diff_kind(&self) -> Option<DiffKind> {
        self.diff_view.as_ref().map(|v| match v.source {
            Source::Save { .. } => DiffKind::Save,
            Source::Patch { .. } => DiffKind::Patch,
            Source::Conflict { .. } => DiffKind::Conflict,
            Source::Review { .. } => DiffKind::Review,
        })
    }
}

/// What a command does in a diff view.
#[derive(Debug, Clone, Copy)]
pub(crate) enum DiffAct {
    ToggleSplit,
    Scroll(isize),
    Page(isize),
    Top,
    Bottom,
    Close,
    /// `y` / `n` to the save question the view was opened from.
    Answer(char),
    Conflict(ConflictAct),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DiffKind {
    Save,
    Patch,
    Conflict,
    Review,
}

/// What a command does in the conflict view, beyond scrolling.
#[derive(Debug, Clone, Copy)]
pub(crate) enum ConflictAct {
    /// Make conflict `current + delta` current and scroll to it.
    Move(isize),
    /// The next comparison (ours/theirs → base/ours → base/theirs).
    Compare,
    Take(Take),
}

impl Editor {
    fn conflict_act(&mut self, mut v: DiffView, act: ConflictAct) {
        let width = self.text_w;
        match act {
            ConflictAct::Move(d) => {
                if let Source::Conflict {
                    current, sections, ..
                } = &mut v.source
                {
                    let last = sections.len().saturating_sub(1) as isize;
                    *current = (*current as isize + d).clamp(0, last) as usize;
                }
                v.render(width);
                v.jump_to_current();
            }
            ConflictAct::Compare => {
                if let Source::Conflict { compare, .. } = &mut v.source {
                    *compare = compare.next();
                }
                v.render(width);
                v.jump_to_current();
            }
            ConflictAct::Take(take) => return self.resolve_conflict(v, take),
        }
        self.clamp_top(&mut v);
        self.diff_view = Some(v);
    }

    fn clamp_top(&self, v: &mut DiffView) {
        let last = v.lines.len().saturating_sub(body_rows(self.text_h));
        v.top = v.top.min(last);
    }

    /// Replace the current conflict's marker block with the side taken, as one
    /// undo step, then show what is left — or close when nothing is.
    fn resolve_conflict(&mut self, mut v: DiffView, take: Take) {
        let Source::Conflict { current, .. } = &v.source else {
            return;
        };
        let k = *current;
        // The buffer itself, not the view's snapshot: the view is modal, so the
        // two agree, and the edit must be computed against what it edits.
        let text = self.bs().buf.text();
        let Some((start, end, lines)) = crate::conflict::resolution(&text, k, take) else {
            self.flash("No base recorded for this conflict");
            self.diff_view = Some(v);
            return;
        };
        let mut rows: Vec<Vec<char>> = lines.iter().map(|l| l.chars().collect()).collect();
        // A buffer is never zero rows.
        if rows.is_empty() && self.bs().buf.row_count() == end - start {
            rows.push(Vec::new());
        }
        self.begin_action(ActionKind::Resolve, start, end);
        self.bs_mut().buf.splice_rows(start, end, rows);
        let last_row = self.bs().buf.row_count().saturating_sub(1);
        self.bs_mut().cursor = Pos {
            row: start.min(last_row),
            col: 0,
        };
        self.bs_mut().mark = None;
        self.finish_step();
        self.edit_invalidate();
        let taken = match take {
            Take::Ours => "ours",
            Take::Theirs => "theirs",
            Take::OursThenTheirs => "both, ours first",
            Take::TheirsThenOurs => "both, theirs first",
            Take::Base => "the base",
        };
        let text = self.bs().buf.text();
        let left = crate::conflict::parse(&text)
            .iter()
            .filter(|s| matches!(s, crate::conflict::Segment::Conflict(_)))
            .count();
        if left == 0 {
            self.adjust_scroll(self.text_h);
            self.adjust_scroll_x();
            self.flash(&format!(
                "Took {taken}. All conflicts resolved: review, then save"
            ));
            return;
        }
        if let Source::Conflict {
            text: snap,
            current,
            ..
        } = &mut v.source
        {
            *snap = text;
            // The same index is now the conflict that followed.
            *current = k.min(left - 1);
        }
        v.render(self.text_w);
        v.jump_to_current();
        self.clamp_top(&mut v);
        let plural = if left == 1 { "" } else { "s" };
        self.flash(&format!(
            "Took {taken}; {left} conflict{plural} left. M-U undoes"
        ));
        self.diff_view = Some(v);
    }
}

impl DiffView {
    /// Scroll so the current conflict's header is the first row shown.
    fn jump_to_current(&mut self) {
        if let Source::Conflict {
            current, sections, ..
        } = &self.source
            && let Some(&row) = sections.get(*current)
        {
            self.top = row;
        }
    }
}

/// A buffer with no diff name can still be one: `git diff > out` into an
/// unnamed buffer, or a file without the extension. It is one when it has a
/// file header and a hunk.
fn looks_like_a_patch(text: &str) -> bool {
    !crate::patch::parse(text).files.is_empty() && text.lines().any(|l| l.starts_with("@@ -"))
}
