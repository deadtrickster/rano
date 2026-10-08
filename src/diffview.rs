//! A view drawn over the text by the library's renderers — a diff, split or
//! unified (`s` toggles, remembered for the session), or a file rendered as
//! something other than its own text. Six things open it:
//!
//! - **the "File changed on disk" question** (`d`): what saving would do to
//!   the file. It answers to the question — `y` and `n` answer from here, and
//!   leaving comes back to it;
//! - **M-P on a diff or patch buffer**: the patch drawn file by file, hunk by
//!   hunk, at each file's own line numbers ([`crate::patch`]);
//! - **M-P on a buffer with merge conflicts**: ours against theirs, side by
//!   side ([`crate::conflict`]);
//! - **M-P on a markdown buffer**: the file as prose ([`crate::markdown`]), the
//!   text untouched underneath it — and the pictures it names (`![alt](path)`)
//!   drawn in it, as the terminal's own rows ([`crate::term::graphics`]);
//! - **M-P on a PNG**: the file as the picture it is, in the terminal, at a box
//!   that follows the window;
//! - **a host's review** ([`Editor::open_review`]): one change to the file,
//!   as the host saw it made, at the file's own line numbers — and M-P on that
//!   buffer shows it again ([`crate::review`]).
//!
//! The sources are kept and the lines re-rendered only when the width or the
//! view changes — a diff and a highlight of two whole files is not per-frame
//! work. The M-P views are snapshots of the buffer when they opened, and a picture
//! is the exception that is cut again at the box a new width gives it.
//!
//! **A picture is the one view whose rows are not the whole of it**: the rows are
//! placeholder cells the terminal fills from its own copy of the bytes, so the
//! editor queues those bytes for the host ([`Editor::take_graphics`]) and keeps the
//! set the terminal is holding up to date ([`Editor::sync_image`]).

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
    /// A markdown buffer's text, drawn as the prose it documents.
    Markdown {
        name: String,
        text: String,
        /// **The pictures the document names**, read when the view opened: an image
        /// reference's target is not in the rendered rows, so it is taken from the
        /// source and the file is read here rather than while a frame is drawn.
        /// Empty for a document that names none, or a terminal that takes no
        /// pictures.
        images: Vec<Img>,
    },
    /// **A picture**: a PNG ([`crate::image`]) drawn as the placeholder cells the
    /// terminal fills from its own copy of it ([`crate::term::graphics`]).
    Picture {
        name: String,
        /// The image id, hashed from the file's name: the placeholders and the
        /// upload have to name the same image, and one file is one picture.
        id: u32,
        /// The PNG's bytes, base64 — what the upload sends — and the picture's own
        /// pixel size, which decides the cell box at any width (the bytes are never
        /// re-read: they are the same picture whatever box it is drawn in).
        png: String,
        intrinsic: (u32, u32),
        /// The cell box the placeholders draw, re-cut when the width moves.
        cells: (u32, u32),
    },
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

/// **A picture inside a rendered document**: the reference it was named by and the
/// bytes it is, with the cell box it fills.
///
/// The box is the renderer's to set (it depends on the width the view draws at) and
/// the rest is read once, when the view opens: `alt` and `target` are kept because
/// the row the picture goes under is found by the reference it shows, and a document
/// can name the same file twice.
#[derive(Debug, Clone)]
pub struct Img {
    /// The image id, hashed from the resolved path: two references to one file are
    /// one picture in the terminal, drawn in two places.
    pub id: u32,
    pub alt: String,
    pub target: String,
    /// The PNG's bytes, base64.
    pub png: String,
    pub intrinsic: (u32, u32),
    pub cells: (u32, u32),
}

impl Source {
    /// **Whether `s` has two panels to toggle here.** The four diffs do; a
    /// rendered document is one column, and says so rather than re-rendering
    /// itself under a key that promised a change (see `DiffAct::ToggleSplit`).
    /// Positive rather than "not a document", so a source added later has to
    /// ask for the split deliberately.
    fn has_a_split(&self) -> bool {
        matches!(
            self,
            Source::Save { .. }
                | Source::Patch { .. }
                | Source::Conflict { .. }
                | Source::Review { .. }
        )
    }
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
            // The document's own renderer, the one a model's reply is drawn
            // with: blocks to rows, at the width the view draws them, with the
            // pictures it names spliced in under the lines that name them.
            Source::Markdown { text, images, .. } => {
                let mut rows = crate::markdown::render_blocks(
                    &crate::markdown::lex(text),
                    width,
                    &crate::markdown::RenderOptions::default(),
                );
                splice_pictures(&mut rows, images, width);
                rows
            }
            // Rows of placeholder cells: the terminal fills them from its own
            // copy of the picture. Nothing is read here — the bytes came off the
            // disk when the view opened — but a resize moves the box, and the
            // cells are what the placeholders and the placement agree on.
            Source::Picture {
                id,
                intrinsic,
                cells,
                ..
            } => {
                *cells = cells_for(*intrinsic, width);
                crate::term::graphics::image_lines(*id, cells.0, cells.1)
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
            Source::Markdown { name, .. } => {
                format!(" Markdown {name}   Esc: back to the text")
            }
            Source::Picture { name, cells, .. } => format!(
                " Picture {name} ({}×{} cells)   Esc: back to the text",
                cells.0, cells.1
            ),
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

/// The cells a picture of `intrinsic` pixels fills at the width a view draws at:
/// the one call the placeholders and the placement both go through.
fn cells_for(intrinsic: (u32, u32), width: usize) -> (u32, u32) {
    crate::term::graphics::image_cells(
        Some(intrinsic.0),
        Some(intrinsic.1),
        crate::term::graphics::image_box(width),
    )
}

/// **The pictures of a rendered document, spliced in as rows**: each goes under the
/// first row that shows its reference ([`picture_anchor`]), and each picture's box is
/// re-cut for this width. Ported from letibot's `transcript::assistant`, which draws a
/// reply's pictures the same way — an inserted row of placeholder cells, because to
/// the row-diffing painter they are text.
fn splice_pictures(rows: &mut Vec<Line>, images: &mut [Img], width: usize) {
    let mut from = 0;
    for img in images.iter_mut() {
        img.cells = cells_for(img.intrinsic, width);
        let at = {
            let plain: Vec<String> = rows.iter().map(Line::plain).collect();
            picture_anchor(&plain, from, &img.alt, &img.target).unwrap_or(rows.len())
        };
        let picture = crate::term::graphics::image_lines(img.id, img.cells.0, img.cells.1);
        from = at + picture.len();
        rows.splice(at..at, picture);
    }
}

/// **Where a document's picture goes**: after the first rendered row, at or past
/// `from`, that shows its reference — the markdown itself when it is drawn literally
/// (a fence), else the alt text, which is what the renderer leaves of an image, else
/// the bare target.
///
/// Ported from letibot's `ui::render::picture_anchor`, with one order changed and for
/// the reason that function's own comment gives: the operator's reply named the path in
/// a sentence before the `![…]` line, and the picture hung under the sentence. Searching
/// the Target first does not fix that — the rendered row shows the *alt text*, never the
/// path — so the alt text is looked for before it, and the path is the last resort (it
/// is what an image with no alt text has left to be found by, and it is right in a fence
/// that draws the reference literally). The frame rule is letibot's: a reference drawn
/// inside a code fence (`│` rows, closed by `└`) puts the picture under the frame, not
/// inside it.
fn picture_anchor(rows: &[String], from: usize, alt: &str, target: &str) -> Option<usize> {
    let reference = format!("]({target}");
    let on = |needle: &str| {
        rows.iter()
            .enumerate()
            .skip(from)
            .find(|(_, l)| l.contains(needle))
    };
    on(&reference)
        .or_else(|| (!alt.is_empty()).then(|| on(alt)).flatten())
        .or_else(|| on(target))
        .map(|(i, _)| {
            let mut at = i + 1;
            while at < rows.len() && rows[at].trim_start().starts_with('│') {
                at += 1;
            }
            if at < rows.len() && rows[at].trim_start().starts_with('└') {
                at += 1;
            }
            at
        })
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
        // A picture's upload and placement follow from what is now open, and a
        // picture that was open before it is dropped here.
        self.sync_image();
    }

    /// **M-P on a picture file**: read it and open the view that draws it, or say why
    /// not. What stops it is said rather than drawn — a picture is the one view whose
    /// failure the reader cannot see for themselves.
    fn open_picture(&mut self, name: String) {
        if !self.images {
            self.flash(
                "This terminal does not take inline pictures: RANO_TERM_FEATURES=images forces them",
            );
            return;
        }
        let Some(path) = self.bs().buf.name.clone() else {
            self.flash("This buffer has no file to read a picture from");
            return;
        };
        let id = crate::term::graphics::image_id(&name);
        // The box a conversion may fill, before anything about the picture is known.
        let max_px = crate::image::box_px(self.text_w);
        match crate::image::read(&path.to_string_lossy(), None, max_px) {
            Ok(pic) => self.open_view(Source::Picture {
                cells: cells_for((pic.width, pic.height), self.text_w),
                intrinsic: (pic.width, pic.height),
                png: crate::term::terminal::base64(&pic.png),
                id,
                name,
            }),
            Err(e) => self.flash(&e),
        }
    }

    /// **The pictures a markdown document names**, read beside it: `![alt](path)`,
    /// resolved against the directory the file is in. A reference that cannot be read
    /// is skipped rather than flashed — the document is still the document, and the
    /// alt text is what the renderer already drew in its place.
    fn document_images(&self, text: &str) -> Vec<Img> {
        if !self.images {
            return Vec::new();
        }
        let dir = self
            .bs()
            .buf
            .name
            .as_deref()
            .and_then(std::path::Path::parent);
        let max_px = crate::image::box_px(self.text_w);
        // **One budget for the document**, not one per picture: a page naming twenty
        // photographs is not twenty waits. The spend is on conversions only — a PNG is read
        // as it is and never counts against it — and a reference left unread draws nothing,
        // which is what a reference that cannot be read does anyway.
        let started = std::time::Instant::now();
        let mut out = Vec::new();
        for (alt, target) in crate::markdown::images(text) {
            let converts = !crate::image::is_png(Some(std::path::Path::new(&target)));
            if converts && started.elapsed() > crate::image::CONVERT_TIMEOUT {
                continue;
            }
            let Ok(pic) = crate::image::read(&target, dir, max_px) else {
                continue;
            };
            let path = crate::image::resolve(&target, dir);
            out.push(Img {
                id: crate::term::graphics::image_id(&path.to_string_lossy()),
                cells: (0, 0),
                alt,
                target,
                png: crate::term::terminal::base64(&pic.png),
                intrinsic: (pic.width, pic.height),
            });
        }
        out
    }

    /// **What the terminal must be told about the open view's pictures**: the upload
    /// of one it has not been sent, the placement of one whose box moved, and the drop
    /// of one no longer drawn. Nothing when nothing changed, so it is cheap enough to
    /// ask after every render.
    ///
    /// The bytes are queued rather than written — this is a library, and the terminal
    /// belongs to the host ([`Self::take_graphics`]).
    fn sync_image(&mut self) {
        let want = match &self.diff_view {
            Some(v) => v.images(),
            None => Vec::new(),
        };
        // A picture the view no longer draws at all goes first — with its bytes, which
        // nothing else can free once the rows that named it are gone. A picture whose
        // *box* moved is kept: the upload under the same id replaces the data, and the
        // placement drops the placements that came before it.
        let mut stale: Vec<u32> = self
            .images_held
            .keys()
            .copied()
            .filter(|id| !want.iter().any(|(w, _, _)| w == id))
            .collect();
        stale.sort_unstable();
        for id in stale {
            self.images_held.remove(&id);
            self.graphics_out
                .push(crate::term::graphics::image_delete(id));
        }
        for (id, cells, png) in want {
            if self.images_held.get(&id) == Some(&cells) {
                continue;
            }
            self.images_held.insert(id, cells);
            self.graphics_out
                .push(crate::term::graphics::image_upload(id, png));
            self.graphics_out
                .push(crate::term::graphics::image_place(id, cells.0, cells.1));
        }
    }

    /// **Drain the bytes queued for the terminal**: the kitty graphics commands a
    /// picture needs (see [`Self::images`]), as many as there are, in the order they
    /// must be written. A host whose terminal takes pictures writes them out before the
    /// frame that draws the placeholder rows.
    pub fn take_graphics(&mut self) -> Vec<Vec<u8>> {
        std::mem::take(&mut self.graphics_out)
    }

    /// M-P: the current buffer rendered — a patch file hunk by hunk, a markdown file
    /// as prose with the pictures it names, a picture file as itself, a file with merge
    /// conflicts ours against theirs. Anything else says why not.
    pub(crate) fn toggle_rendered_view(&mut self) {
        let name = self.buffer_name(self.cur);
        let text = self.bs().buf.text();
        let named = self.bs().buf.name.as_deref();
        let first = self
            .bs()
            .buf
            .row_opt(0)
            .map(|l| l.iter().collect::<String>());
        // **A picture file is asked about first**, by its name and not its bytes: what is
        // in the buffer is a lossy decode of those bytes — text that is not the file — so
        // neither the conflict nor the patch question can be asked of it sensibly. A PNG
        // is drawn as it is; anything else a converter turns into one (§19.3), and a
        // format with no converter for it says so by name.
        if crate::image::kind(named).is_some() {
            return self.open_picture(name);
        }
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
        } else if crate::syntax::detect(named, first.as_deref()) == Some(Lang::Markdown) {
            let images = self.document_images(&text);
            self.open_view(Source::Markdown { name, text, images });
        } else if !self.show_review() {
            self.flash(
                "Nothing to render: not a diff or patch, not markdown or a picture, and no merge conflicts",
            );
        }
    }

    /// Re-render after a resize. Returns whether anything changed.
    pub(crate) fn refresh_diff_view(&mut self) -> bool {
        let w = self.text_w;
        match self.diff_view.as_mut() {
            Some(v) if v.width != w => {
                v.render(w);
                // The placeholders moved with the box, and the terminal has to be told
                // where they are now (and given the picture again, cut for the new box).
                self.sync_image();
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
                // Nothing is open now, so a picture the terminal holds is one nobody
                // draws: it goes, with its bytes.
                self.sync_image();
                if v.answers_save() {
                    self.reask_external();
                }
                return;
            }
            // Split ↔ unified, remembered for the next diff. A view with one
            // column has none to toggle, and says so rather than re-rendering
            // the same rows.
            DiffAct::ToggleSplit => {
                if !v.source.has_a_split() {
                    self.flash("This view is one column: there is no split");
                    self.diff_view = Some(v);
                    return;
                }
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
            Source::Markdown { .. } => DiffKind::Markdown,
            Source::Picture { .. } => DiffKind::Picture,
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
    Markdown,
    Picture,
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
    /// **The pictures this view's rows are placeholders for**, as `(id, cells, png)`:
    /// what the terminal has to hold before the frame that draws them. Empty for every
    /// view whose rows are their own text.
    fn images(&self) -> Vec<(u32, (u32, u32), &str)> {
        match &self.source {
            Source::Picture { id, cells, png, .. } => vec![(*id, *cells, png.as_str())],
            Source::Markdown { images, .. } => images
                .iter()
                .map(|i| (i.id, i.cells, i.png.as_str()))
                .collect(),
            _ => Vec::new(),
        }
    }

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
