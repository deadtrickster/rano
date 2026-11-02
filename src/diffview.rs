//! A view drawn over the text by the library's renderers: the save question's
//! diff, M-P's patch, conflict, markdown and picture views, or a host's
//! review. Lines re-render only when the width or the view changes.

use std::path::PathBuf;

use crate::conflict::{Compare, Take};
use crate::diff::DiffConfig;
use crate::render::Line;
use crate::sidediff::{EditView, edit_view, render_edit_view};
use crate::style::Palette;

use crate::buffer::Pos;
use crate::editor::{ActionKind, Editor};
use crate::syntax::Lang;

pub enum Source {
    Save {
        path: PathBuf,
        disk: String,
        mine: String,
    },
    /// A diff or patch buffer's text.
    Patch { name: String, text: String },
    Markdown {
        name: String,
        text: String,
        /// Read when the view opened, not while a frame is drawn: a
        /// reference's target is not in the rendered rows.
        images: Vec<Img>,
    },
    /// A PNG drawn as placeholder cells the terminal fills from its own copy.
    Picture {
        name: String,
        /// Hashed from the file's name: placeholders and upload must name the same image.
        id: u32,
        /// Base64 bytes, never re-read — the same picture whatever box it is drawn in.
        png: String,
        intrinsic: (u32, u32),
        /// The cell box the placeholders draw, re-cut when the width moves.
        cells: (u32, u32),
    },
    Conflict {
        name: String,
        text: String,
        /// The conflict the keys act on (0-based, file order).
        current: usize,
        compare: Compare,
        /// Each conflict's header row in `lines`, from the last render.
        sections: Vec<usize>,
    },
    Review {
        name: String,
        change: crate::review::Change,
    },
}

/// A picture named by a rendered document; `alt` and `target` find the row it goes under.
#[derive(Debug, Clone)]
pub struct Img {
    pub id: u32,
    pub alt: String,
    pub target: String,
    pub png: String,
    pub intrinsic: (u32, u32),
    pub cells: (u32, u32),
}

impl Source {
    /// Positive, so a source added later has to ask for the split deliberately.
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
            // The document's own renderer, pictures spliced under the lines that name them.
            Source::Markdown { text, images, .. } => {
                let mut rows = crate::markdown::render_blocks(
                    &crate::markdown::lex(text),
                    width,
                    &crate::markdown::RenderOptions::default(),
                );
                splice_pictures(&mut rows, images, width);
                rows
            }
            // Placeholder rows the terminal fills from its own copy; nothing is read here.
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

/// The cells a picture of `intrinsic` pixels fills at this width — the one call the placeholders and the placement share.
fn cells_for(intrinsic: (u32, u32), width: usize) -> (u32, u32) {
    crate::term::graphics::image_cells(
        Some(intrinsic.0),
        Some(intrinsic.1),
        crate::term::graphics::image_box(width),
    )
}

/// Splice each picture in under the first row that shows its reference, as
/// placeholder rows the row-diffing painter treats as text.
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

/// Where a picture goes: after the first row at or past `from` that shows its
/// reference — literal markdown (a fence), alt text, or bare target, in that
/// order. One inside a code frame hangs under the frame, not in it.
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
    /// The file on disk now against the buffer as a save would write it.
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
        // Uploads and placements follow from what is now open; a picture open before is dropped.
        self.sync_image();
    }

    /// A view belongs to the buffer it was opened on: making another current closes it.
    pub(crate) fn close_view(&mut self) {
        if self.diff_view.take().is_some() {
            self.sync_image();
        }
    }

    /// A picture file opens with its view up: a PNG's text is a lossy decode,
    /// and there is no byte view. A terminal that takes no pictures is not flashed — M-P says why.
    pub(crate) fn preview_if_picture(&mut self) -> bool {
        if !self.images || self.bs().buf.name.is_none() {
            return false;
        }
        if crate::image::kind(self.bs().buf.name.as_deref()).is_none() {
            return false;
        }
        let name = self.buffer_name(self.cur);
        self.open_picture(name);
        self.diff_view.is_some()
    }

    /// Failures are said rather than drawn: a picture's failure is one the reader cannot see.
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

    /// The document's `![alt](path)` references, resolved against its directory; an unreadable one is skipped, not flashed.
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
        // One conversion budget for the whole document, not one per picture; a reference past it draws nothing.
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

    /// Bring the terminal's pictures in line with the open view: upload one it lacks,
    /// re-place one whose box moved, drop one no longer drawn — nothing when nothing changed.
    fn sync_image(&mut self) {
        let want = match &self.diff_view {
            Some(v) => v.images(),
            None => Vec::new(),
        };
        // A picture no longer drawn goes first, with its bytes; a moved box only needs the same id uploaded again.
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

    /// Drain the graphics commands queued for the terminal, in write order; the host owns the terminal.
    pub fn take_graphics(&mut self) -> Vec<Vec<u8>> {
        std::mem::take(&mut self.graphics_out)
    }

    /// M-P: render the current buffer — patch, markdown, picture or conflicts — or say why not.
    pub(crate) fn toggle_rendered_view(&mut self) {
        let name = self.buffer_name(self.cur);
        let text = self.bs().buf.text();
        let named = self.bs().buf.name.as_deref();
        let first = self
            .bs()
            .buf
            .row_opt(0)
            .map(|l| l.iter().collect::<String>());
        // A picture file is asked about by name, not bytes: the buffer holds a
        // lossy decode the conflict and patch questions cannot use.
        if crate::image::kind(named).is_some() {
            return self.open_picture(name);
        }
        // Conflicts first: a patch with markers in it is mid-merge too.
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
        } else if crate::syntax::detect(named, first.as_deref()) == Some(Lang::Markdown)
            // The text is asked about too, not just the name: an answer named
            // after a non-markdown file opens with a heading `detect` passes over.
            || looks_like_markdown(&text)
        {
            self.open_markdown_view(name, text);
        } else if !self.show_review() {
            self.flash(
                "Nothing to render: not a diff or patch, not markdown or a picture, and no merge conflicts",
            );
        }
    }

    /// M-P's answer and a provider's answer, one place so the two cannot render differently.
    pub(crate) fn open_markdown_view(&mut self, name: String, text: String) {
        let images = self.document_images(&text);
        self.open_view(Source::Markdown { name, text, images });
    }

    pub(crate) fn refresh_diff_view(&mut self) -> bool {
        let w = self.text_w;
        match self.diff_view.as_mut() {
            Some(v) if v.width != w => {
                v.render(w);
                self.sync_image();
                true
            }
            _ => false,
        }
    }

    /// One action in the diff view, from a command (keymaps in `commands.rs`).
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
            DiffAct::Close => {
                if v.answers_save() {
                    self.sync_image();
                    self.reask_external();
                    return;
                }
                if matches!(v.source, Source::Picture { .. }) {
                    // Esc leaves the file, not the view; the view goes back up
                    // so the last buffer keeps the picture, not PNG bytes.
                    self.diff_view = Some(v);
                    self.close_buffer();
                    return;
                }
                self.sync_image();
                return;
            }
            // Split ↔ unified, remembered for the next diff; one column has none to toggle.
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

#[derive(Debug, Clone, Copy)]
pub(crate) enum ConflictAct {
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
    /// The `(id, cells, png)` of the pictures these rows are placeholders for; empty when the rows are their own text.
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

/// No diff name can still be a patch (`git diff > out`): a file header and a hunk make it one.
fn looks_like_a_patch(text: &str) -> bool {
    !crate::patch::parse(text).files.is_empty() && text.lines().any(|l| l.starts_with("@@ -"))
}

/// No name to go by (`rano:manual`): markdown only when a heading opens it and
/// another block marker follows — a heading alone would make a folder listing prose.
pub(crate) fn looks_like_markdown(text: &str) -> bool {
    let mut heading = false;
    for line in text.lines() {
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        let atx = is_atx_heading(t);
        if !heading {
            if !atx {
                return false;
            }
            heading = true;
            continue;
        }
        if atx
            || t.starts_with("- ")
            || t.starts_with("* ")
            || t.starts_with("```")
            || t.starts_with('|')
            || t.contains("](")
        {
            return true;
        }
    }
    false
}

/// `# Title`: one to six `#`, then a space — CommonMark's ATX heading.
fn is_atx_heading(t: &str) -> bool {
    let hashes = t.chars().take_while(|c| *c == '#').count();
    (1..=6).contains(&hashes) && t[hashes..].starts_with(' ')
}
