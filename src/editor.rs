//! Editor core: the model, undo, and the editing actions.

use std::collections::VecDeque;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::render::Style;
use crate::style::Role;
use crate::term::{MouseButton, MouseEvent, MouseKind, Picture, Pictures, Proto};

use crate::BufferState;
use crate::RowWrap;
use crate::buffer::{Buffer, DiskStamp, Pos};
use crate::config;
use crate::diffview::DiffAct;
pub use crate::keys::KeyOutcome;
use crate::lsp;
use crate::prompt::{Prompt, PromptKind, expand_tilde};
use crate::search_ctrl::ReplaceState;
use crate::syntax;
use crate::ui;
use crate::width;

/// The image id a plugin's picture of `path` is drawn under; the prefix keeps
/// it apart from a PNG view's.
fn picture_id(path: &Path) -> u32 {
    crate::term::graphics::image_id(&format!("plugin:{}", path.display()))
}

/// A picture's rows: [`Picture::lines`]'s placeholder cells as buffer rows.
fn picture_rows(picture: &Picture, proto: Proto) -> Vec<Vec<char>> {
    picture
        .lines(proto)
        .iter()
        .map(|l| l.plain().chars().collect())
        .collect()
}

/// A picture a plugin answered, on the glass. One at a time.
pub(crate) struct PictureView {
    path: PathBuf,
    picture: Picture,
}

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
    /// A TODO.md state change: one undo step, coalescing with nothing.
    Todo,
    /// A merge conflict resolved: one undo step, coalescing with nothing.
    Resolve,
    /// A plugin's `on_save` output replacing the buffer: one step only.
    Plugin,
    /// A formatter's whole-buffer rewrite on save (gofmt): one step only.
    Format,
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
    len_at_begin: usize, // buf.row_count() when the step began
}

/// How far one wheel notch scrolls, whatever the screen's height.
const WHEEL_LINES: isize = 3;

/// Which of nano's bars an editor draws; a host framing the pane in its own
/// chrome turns them off ([`Editor::set_chrome`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Chrome {
    pub title: bool,
    pub keys: bool,
}

impl Default for Chrome {
    fn default() -> Self {
        Chrome {
            title: true,
            keys: true,
        }
    }
}

impl Chrome {
    /// Neither bar: the text, and the status row under it.
    pub const BARE: Chrome = Chrome {
        title: false,
        keys: false,
    };

    /// The pane row the text starts on.
    pub fn top(&self) -> u16 {
        u16::from(self.title)
    }

    /// Rows that are not text: the bars that are on, and the status row.
    pub fn rows(&self) -> usize {
        usize::from(self.title) + 1 + if self.keys { 2 } else { 0 }
    }
}

/// What the title bar says, for a host that draws its own ([`Editor::frame_title`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrameTitle {
    /// The file's name as the bar shows it, `[i/n] ` first with several
    /// buffers.
    pub name: String,
    pub modified: bool,
    /// The buffer cannot be written (a tailed log, a view).
    pub read_only: bool,
}

/// A rectangle of terminal cells, the editor's own so editor logic does not
/// change with how it draws.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Area {
    pub x: u16,
    pub y: u16,
    pub w: u16,
    pub h: u16,
}

impl Area {
    pub const fn new(x: u16, y: u16, w: u16, h: u16) -> Self {
        Self { x, y, w, h }
    }

    pub const fn is_empty(&self) -> bool {
        self.w == 0 || self.h == 0
    }

    /// Whether the cell at column `x`, row `y` is inside.
    pub const fn contains(&self, x: u16, y: u16) -> bool {
        x >= self.x
            && y >= self.y
            && (x as u32) < self.x as u32 + self.w as u32
            && (y as u32) < self.y as u32 + self.h as u32
    }
}

pub struct Editor {
    pub buffers: Vec<BufferState>,
    pub cur: usize,
    pub cut: Vec<Vec<char>>,
    pub cut_line: bool,
    /// Search modifier toggles (M-C / M-R; runtime-only), case-insensitive and
    /// literal by default.
    pub search_case_sensitive: bool,
    pub search_regex: bool,
    pub prompt: Option<Prompt>,
    /// Live path suggestions for the open file-name prompt, reread only when
    /// its text changes.
    pub prompt_hints: Option<(PromptKind, String, Vec<String>)>,
    /// The keymaps in effect (see `commands.rs`); a host can rebind by editing
    /// them.
    pub keymaps: crate::commands::Keymaps,
    /// A host's own maps, stacked over `keymaps` (see [`Editor::push_keymap`]).
    pub(crate) host_keymaps: Vec<crate::keymap::Keymap>,
    /// A key sequence under way: a prefix pressed, or `describe-key` waiting.
    pub pending: crate::commands::Pending,
    /// `M-x`, while it is open.
    pub palette: Option<crate::help::Palette>,
    /// A help page (the overview, every key, a key's description).
    pub info: Option<crate::help::InfoView>,
    /// Commands run, most recent first: `M-x` offers them first.
    pub command_history: Vec<&'static str>,
    pub status: Option<Flash>,
    pub loc_until: Option<Instant>,
    pub quit: bool,
    pub pending_write: Option<PathBuf>,
    /// A save refused because its directory is missing; see
    /// [`Self::answer_mkdir`].
    pub(crate) pending_dir: Option<PathBuf>,
    pub quit_after_save: bool,
    /// M-W on a modified buffer answered 'y': close it once the save lands.
    pub close_after_save: bool,
    /// The list overlay (M-L buffers, M-? usages); modal while open.
    pub picker: Option<crate::picker::Picker>,
    /// The external-change diff over the text; modal while open.
    pub diff_view: Option<crate::diffview::DiffView>,
    /// Split rather than unified, for the next diff view: the last choice made
    /// with `s`.
    pub diff_split: bool,
    /// Whether the terminal takes inline pictures: a cell it cannot fill is
    /// drawn as garbage, so off until a host says otherwise.
    pub images: bool,
    /// Which picture protocol the terminal speaks, where a host knows more
    /// than [`Self::images`] says.
    pub(crate) proto: Proto,
    /// The picture a plugin answered, while it is on the glass.
    pub(crate) picture_view: Option<PictureView>,
    /// The pictures the terminal holds for [`Self::picture_view`]; separate
    /// from [`Self::images_held`] so the two cannot free each other's.
    pub(crate) pictures: Pictures,
    /// The WASM plugin, asked at two seams: `on_open` when text enters,
    /// `on_save` before a write.
    pub plugin: Option<crate::plugin::Plugin>,
    /// The builtins unpacked this session: they answer `provider_open` and
    /// never displace the configured plugin.
    pub builtins: Vec<crate::plugin::Plugin>,
    /// What the plugin said about the last read or write, appended to the
    /// message that reports it rather than flashed twice.
    pub(crate) plugin_note: Option<String>,
    /// `--edit`: open a `.log` as an ordinary file. `-f` forces a tail and
    /// wins where both are given.
    pub edit_override: bool,
    /// Bytes the host must write outside the frame: a view's kitty graphics
    /// commands.
    pub(crate) graphics_out: Vec<Vec<u8>>,
    /// Text queued for the terminal's clipboard (OSC 52); see
    /// [`Self::take_clipboard`].
    pub(crate) clipboard_out: Option<String>,
    /// The pictures the terminal is holding, so an unchanged frame sends
    /// nothing and a gone or moved one is dropped.
    pub(crate) images_held: std::collections::HashMap<u32, (u32, u32)>,
    /// Whether a left press armed a selection that has not dragged yet; a
    /// release without a drag drops the anchor.
    pub(crate) mouse_arm: bool,
    /// Where M-S sends the reader's place (see `send_ctrl.rs`).
    pub on_send: Option<crate::send_ctrl::OnSend>,
    pub replace: Option<ReplaceState>,
    pub replace_pos: Option<Pos>,
    pub replace_count: usize,
    pub text_w: usize,
    pub text_h: usize,
    /// Which of nano's bars this editor draws itself — see [`Chrome`].
    pub chrome: Chrome,
    /// The screen rectangle the editor occupies, as last given to
    /// [`Self::set_area`].
    pub(crate) area: Area,
    /// Rendered width of a tab (seeded from config, F1).
    pub tab_width: usize,
    /// Line-number gutter toggle (M-N; seeded from config, F1).
    pub show_line_numbers: bool,
    /// Soft line wrap (M-\; seeded from config). When on, `bs.scroll` counts
    /// visual rows and horizontal scrolling is off.
    pub wrap: bool,
    /// Startup config; the toggles above seed from it and stay runtime-only.
    pub config: config::Config,
    /// Prompt history per kind, session-global.
    pub search_hist: Vec<String>,
    pub exec_hist: Vec<String>,
    pub file_hist: Vec<String>,
    /// Index into the active history while cycling, plus the text that was
    /// being edited when cycling started.
    pub hist_idx: Option<usize>,
    pub hist_draft: String,
    /// Live completion popup (LSP); None when no candidates are showing.
    pub completion: Option<CompletionPopup>,
    /// Context each in-flight completion request was made for; a response that
    /// does not answer the current one is stale junk.
    pub(crate) completion_q: VecDeque<(usize, String)>,
    /// Until rust-analyzer has scanned a fresh crate it answers member
    /// completions with path junk; those are re-requested on this timer.
    pub(crate) completion_retry: Option<Instant>,
    pub(crate) completion_retries: u8,
    /// Where M-, returns to, one entry per definition jump (stacked).
    pub def_back: Vec<DefBack>,
    /// The startup update check: `Editor::new` does not start it; `run()`
    /// does.
    pub update: crate::update_ctrl::UpdateCheck,
    /// The loader filled its adoption budget: come straight back rather than
    /// sleep the idle wait between batches.
    pub(crate) load_saturated: bool,
    /// The style grid is behind the buffer; the next frame re-highlights.
    pub(crate) highlight_dirty: bool,
    /// Tree-sitter diagnostics are behind the buffer; a debounced parse will
    /// refresh them.
    pub(crate) diag_dirty: bool,
    /// When the last edit happened, for the diagnostics debounce.
    pub(crate) diag_last_edit: Instant,
}

/// Live completion popup: the candidates, the selected row, and the anchor that
/// places it.
pub struct CompletionPopup {
    pub items: Vec<lsp::CompletionItem>,
    pub sel: usize,
    pub row: usize,
    pub col: usize,
}

/// Case-insensitive subsequence test, to keep a server's fuzzy matches below
/// the exact-prefix ones.
fn fuzzy_match(hay: &str, needle: &str) -> bool {
    let mut rest = hay.chars();
    needle
        .chars()
        .all(|n| rest.any(|h| h.eq_ignore_ascii_case(&n)))
}

/// One entry of the M-, stack: the swapped-out buffer state, the buffer index,
/// or just the cursor.
pub struct DefBack {
    pub buf: Option<BufferState>,
    pub idx: Option<usize>,
    pub pos: Pos,
}

/// The word being completed before `col` on `row` and its start column; None
/// where completion does not apply, and the caller closes any popup then.
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
    pub fn new(buf: Buffer, config: config::Config) -> Self {
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
            prompt_hints: None,
            keymaps: crate::commands::Keymaps::standard(),
            host_keymaps: Vec::new(),
            pending: Default::default(),
            palette: None,
            info: None,
            command_history: Vec::new(),
            status: None,
            loc_until: None,
            quit: false,
            pending_write: None,
            pending_dir: None,
            quit_after_save: false,
            close_after_save: false,
            picker: None,
            diff_view: None,
            diff_split: false,
            images: false,
            proto: Proto::None,
            picture_view: None,
            pictures: Pictures::new(Proto::None),
            plugin: None,
            builtins: Vec::new(),
            plugin_note: None,
            edit_override: false,
            graphics_out: Vec::new(),
            clipboard_out: None,
            images_held: std::collections::HashMap::new(),
            mouse_arm: false,
            on_send: None,
            replace: None,
            replace_pos: None,
            replace_count: 0,
            text_w: 80,
            text_h: 24,
            chrome: Chrome::default(),
            area: Area::default(),
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
            update: crate::update_ctrl::UpdateCheck::default(),
            def_back: Vec::new(),
            load_saturated: false,
            highlight_dirty: true,
            diag_dirty: true,
            diag_last_edit: Instant::now(),
        };
        ed.lsp_sync();
        ed
    }

    /// The active buffer's state.
    pub(crate) fn bs(&self) -> &BufferState {
        &self.buffers[self.cur]
    }

    pub fn bs_mut(&mut self) -> &mut BufferState {
        &mut self.buffers[self.cur]
    }

    fn clamp_cursor(&mut self) {
        let bs = self.bs_mut();
        bs.cursor = bs.buf.clamp(bs.cursor);
    }

    /// Recompute the live path suggestions when the prompt's text changed.
    pub(crate) fn refresh_prompt_hints(&mut self) -> bool {
        let want = self.prompt.as_ref().filter(|p| {
            matches!(
                p.kind,
                PromptKind::WriteName
                    | PromptKind::ReadName
                    | PromptKind::OpenName
                    | PromptKind::BackupName
                    | PromptKind::DirName
            )
        });
        match want {
            None => self.prompt_hints.take().is_some(),
            Some(p) => {
                if let Some((k, t, _)) = &self.prompt_hints
                    && *k == p.kind
                    && *t == p.text
                {
                    return false;
                }
                let names = crate::prompt::complete_path(&p.text)
                    .map(|(_, n)| n)
                    .unwrap_or_default();
                self.prompt_hints = Some((p.kind, p.text.clone(), names));
                true
            }
        }
    }

    /// `msg`, with the plugin's note appended when a hook had something to say.
    pub(crate) fn flash_with_note(&mut self, msg: &str) {
        match self.plugin_note.take() {
            Some(note) => self.flash(&format!("{msg} — {note}")),
            None => self.flash(msg),
        }
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
            // Scroll counts visual rows; the cursor's visual row must stay in.
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
        let max_scroll = bs.buf.row_count().saturating_sub(text_h);
        bs.scroll = bs.scroll.min(max_scroll);
    }

    /// Keep the cursor's display column inside the horizontal window; with
    /// soft wrap on, lines wrap instead of scrolling sideways.
    pub(crate) fn adjust_scroll_x(&mut self) {
        if self.wrap {
            self.bs_mut().scroll_x = 0;
            return;
        }
        let gutter = self.gutter_w();
        let view_w = self.text_w.saturating_sub(gutter).max(1);
        let tab_width = self.tab_width;
        let row = self
            .bs()
            .cursor
            .row
            .min(self.bs().buf.row_count().saturating_sub(1));
        // A simple row (fresh wrap table): display col == char index.
        let simple = self.row_is_simple(row) == Some(true);
        let bs = self.bs_mut();
        let line = bs.buf.row(row);
        let disp = if simple {
            bs.cursor.col.min(line.len())
        } else {
            ui::display_col(line, bs.cursor.col, tab_width)
        };
        let line_w = if simple {
            line.len()
        } else {
            ui::display_width(line, tab_width)
        };
        bs.scroll_x = bs.scroll_x.min(disp).max((disp + 1).saturating_sub(view_w));
        bs.scroll_x = bs.scroll_x.min(line_w);
        // The left edge must not fall inside a wide character; a tab is
        // fine — the renderer clips it into leading spaces.
        if bs.scroll_x > 0 {
            let at = ui::char_at_display(line, bs.scroll_x, tab_width);
            if line
                .get(at)
                .is_some_and(|c| crate::width::char_width(*c) > 1)
            {
                bs.scroll_x = ui::display_col(line, at, tab_width);
            }
        }
    }

    // ---------- soft wrap (M-\) ----------

    /// Width of the text viewport in display cols (gutter excluded).
    pub(crate) fn view_w(&self) -> usize {
        self.text_w.saturating_sub(self.gutter_w()).max(1)
    }

    /// The gutter's width, one place so the draw, the mouse mapping and
    /// `view_w` agree; measured from the buffer's line span, not its rows.
    pub(crate) fn gutter_w(&self) -> usize {
        // A view wears no line numbers: a rendering's row is not a file line.
        if self.show_line_numbers && !self.bs().synthetic {
            ui::gutter_width(self.bs().line_span())
        } else {
            0
        }
    }

    /// Rebuild the visual-row prefix table when the buffer or the wrap width
    /// changed; an edit that names its one changed row re-scans only that row.
    pub(crate) fn ensure_wrap_prefix(&mut self) {
        if !self.wrap {
            return;
        }
        let vw = self.view_w();
        let key = (self.bs().edit_gen, vw);
        let fresh = self.bs().wrap_key == key
            && self.bs().wrap_prefix.len() == self.bs().buf.row_count() + 1
            && self.bs().wrap_rows.len() == self.bs().buf.row_count();
        if fresh {
            return;
        }
        let tw = self.tab_width.max(1);
        let n = self.bs().buf.row_count();
        // An append: rows `[0, k)` are untouched and only `[k, n)` are new —
        // without it a load re-measures every row per batch.
        if let Some(k) = self.bs().wrap_extend_from {
            let k = k.min(n).min(self.bs().wrap_rows.len());
            let usable = k > 0
                && self.bs().wrap_rows.len() <= n
                && self.bs().wrap_prefix.len() == self.bs().wrap_rows.len() + 1;
            if usable {
                let bs = self.bs_mut();
                bs.wrap_rows.truncate(k);
                bs.wrap_prefix.truncate(k + 1);
                for r in k..n {
                    let row_wrap = Self::row_wrap_of_line(bs.buf.row(r), tw, vw);
                    let segs = match &row_wrap.segs {
                        Some(s) => s.len(),
                        None => bs.buf.row(r).len().div_ceil(vw).max(1),
                    };
                    bs.wrap_prefix.push(bs.wrap_prefix[r] + segs);
                    bs.wrap_rows.push(row_wrap);
                }
                bs.wrap_key = key;
                bs.wrap_lines = n;
                bs.wrap_extend_from = None;
                return;
            }
        }
        // The cheap path: same rows, one of them edited.
        if self.bs().wrap_lines == n
            && self.bs().wrap_rows.len() == n
            && self.bs().wrap_prefix.len() == n + 1
            && let Some(row) = self.bs().wrap_dirty_row
            && row < n
        {
            let row_wrap = Self::row_wrap_of_line(self.bs().buf.row(row), tw, vw);
            let bs = self.bs_mut();
            bs.wrap_rows[row] = row_wrap;
            // Re-sum from the edited row to the end: pure arithmetic.
            for i in row..n {
                let segs = match &bs.wrap_rows[i].segs {
                    Some(s) => s.len(),
                    None => bs.buf.row(i).len().div_ceil(vw).max(1),
                };
                bs.wrap_prefix[i + 1] = bs.wrap_prefix[i] + segs;
            }
            bs.wrap_key = key;
            bs.wrap_dirty_row = None;
            bs.wrap_extend_from = None;
            return;
        }
        // The full rebuild: rows moved, or the edit could not name its row.
        let mut prefix = Vec::with_capacity(n + 1);
        let mut rows = Vec::with_capacity(n);
        prefix.push(0);
        for line in self.bs().buf.rows() {
            let row_wrap = Self::row_wrap_of_line(line, tw, vw);
            let segs = match &row_wrap.segs {
                Some(s) => s.len(),
                None => line.len().div_ceil(vw).max(1),
            };
            prefix.push(*prefix.last().unwrap() + segs);
            rows.push(row_wrap);
        }
        let bs = self.bs_mut();
        bs.wrap_prefix = prefix;
        bs.wrap_rows = rows;
        bs.wrap_key = key;
        bs.wrap_lines = n;
        bs.wrap_dirty_row = None;
        bs.wrap_extend_from = None;
    }

    /// One row's wrap geometry, shared by both rebuild paths.
    fn row_wrap_of_line(line: &[char], tab_width: usize, view_w: usize) -> RowWrap {
        // A huge row is measured affinely unless its head is non-ordinary.
        let bounded = if line.len() > Self::LONG_ROW {
            width::is_simple_prefix(line, Self::LONG_ROW)
        } else {
            width::is_simple(line)
        };
        if bounded {
            // Every character one column: segments are affine.
            return RowWrap {
                segs: None,
                w: line.len(),
            };
        }
        RowWrap {
            segs: Some(width::segments(line, tab_width, view_w)),
            w: width::width(line, tab_width),
        }
    }

    /// The wrap geometry of row `r`, or None when the table is stale.
    fn row_wrap(&self, r: usize) -> Option<&RowWrap> {
        let bs = self.bs();
        if bs.wrap_key != (bs.edit_gen, self.view_w()) || bs.wrap_rows.len() != bs.buf.row_count() {
            return None;
        }
        bs.wrap_rows.get(r)
    }

    /// `Some(true)` when row `r` is simple (display col == char index), None
    /// when the table is stale.
    pub(crate) fn row_is_simple(&self, r: usize) -> Option<bool> {
        self.row_wrap(r).map(|rw| rw.segs.is_none())
    }

    /// How many visual rows buffer row `r` occupies.
    pub(crate) fn seg_count(&self, r: usize) -> usize {
        let len = self.line_len(r);
        if let Some(segs) = self.row_wrap(r).and_then(|rw| rw.segs.as_ref()) {
            return segs.len().max(1);
        }
        len.div_ceil(self.view_w()).max(1)
    }

    /// Char range `[lo, hi)` of segment `seg` of buffer row `r`.
    pub(crate) fn seg_chars(&self, r: usize, seg: usize) -> (usize, usize) {
        let len = self.line_len(r);
        if let Some(segs) = self.row_wrap(r).and_then(|rw| rw.segs.as_ref()) {
            let lo = segs.get(seg).map(|s| s.0).unwrap_or(len);
            let hi = segs.get(seg + 1).map(|s| s.0).unwrap_or(len);
            return (lo.min(len), hi.min(len));
        }
        let vw = self.view_w();
        let lo = (seg * vw).min(len);
        (lo, (lo + vw).min(len))
    }

    /// Display range `[d0, d1)` segment `seg` of buffer row `r` paints into.
    pub(crate) fn seg_disp(&self, r: usize, seg: usize) -> (usize, usize) {
        if let Some(rw) = self.row_wrap(r) {
            if let Some(segs) = &rw.segs {
                let d0 = segs.get(seg).map(|s| s.1).unwrap_or(rw.w);
                let d1 = segs.get(seg + 1).map(|s| s.1).unwrap_or(rw.w);
                return (d0, d1);
            }
            return (
                (seg * self.view_w()).min(rw.w),
                rw.w.min((seg + 1) * self.view_w()),
            );
        }
        let len = self.line_len(r);
        let vw = self.view_w();
        ((seg * vw).min(len), ((seg + 1) * vw).min(len))
    }

    /// Which segment of buffer row `r` holds char col `col`.
    pub(crate) fn seg_of_col(&self, r: usize, col: usize) -> usize {
        let len = self.line_len(r);
        let col = col.min(len);
        if let Some(segs) = self.row_wrap(r).and_then(|rw| rw.segs.as_ref()) {
            // Starts are sorted, so the segment is the last one at or before `col`.
            return match segs.binary_search_by_key(&col, |s| s.0) {
                Ok(i) => i,
                Err(0) => 0,
                Err(i) => i - 1,
            };
        }
        (col / self.view_w()).min(self.seg_count(r).saturating_sub(1))
    }

    /// Display offset of char col `col` inside its own segment.
    pub(crate) fn disp_in_seg(&self, r: usize, col: usize) -> usize {
        let (lo, _) = self.seg_chars(r, self.seg_of_col(r, col));
        if self.row_is_simple(r) == Some(true) {
            // Char index == display col: the offset is from the segment's start.
            return col.min(self.line_len(r)).saturating_sub(lo);
        }
        let line = self.bs().buf.row(r);
        ui::display_col(line, col, self.tab_width)
            .saturating_sub(self.seg_disp(r, self.seg_of_col(r, col)).0)
    }

    /// Char col at display offset `within` into segment `seg`, clamped to it.
    pub(crate) fn col_in_seg(&self, r: usize, seg: usize, within: usize) -> usize {
        let (lo, hi) = self.seg_chars(r, seg);
        if self.row_is_simple(r) == Some(true) {
            return (lo + within).min(hi);
        }
        let (d0, _) = self.seg_disp(r, seg);
        let line = self.bs().buf.row(r);
        ui::char_at_display(line, d0 + within, self.tab_width).clamp(lo, hi)
    }

    fn line_len(&self, r: usize) -> usize {
        self.bs().buf.row_opt(r).map(Vec::len).unwrap_or(0)
    }

    /// Visual row containing position `p` (wrap on only).
    pub(crate) fn visual_pos(&self, p: Pos) -> usize {
        let bs = self.bs();
        let r = p.row.min(bs.buf.row_count().saturating_sub(1));
        bs.wrap_prefix.get(r).copied().unwrap_or(0) + self.seg_of_col(r, p.col)
    }

    /// (buffer row, wrap segment) of visual row `v`, clamped to the buffer.
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
        // Clamped to the table too: a stale caller gets a sane row, not one past the end.
        let r = r
            .min(bs.buf.row_count().saturating_sub(1))
            .min(bs.wrap_prefix.len().saturating_sub(2));
        (r, v - bs.wrap_prefix[r])
    }

    /// Wait until the buffer has `p.row`, then put the cursor there and centre
    /// the line; clamping to what has arrived would open the file elsewhere.
    pub(crate) fn apply_startup_pos(&mut self) {
        let Some(p) = self.bs().goto else {
            return;
        };
        // Not yet: a row a million lines in arrives minutes after the first
        // frame, and waiting is the whole point.
        if p.row >= self.bs().buf.row_count() {
            return;
        }
        let bs = self.bs_mut();
        bs.cursor = bs.buf.clamp(p);
        // Centred, which needs the wrap table: a long line above spans several rows.
        self.ensure_wrap_prefix();
        let text_h = self.text_h;
        let cv = self.visual_pos(self.bs().cursor);
        let total = self.bs().wrap_prefix.last().copied().unwrap_or(0);
        let half = text_h / 2;
        let max_scroll = total.saturating_sub(text_h);
        let bs = self.bs_mut();
        bs.scroll = cv.saturating_sub(half).min(max_scroll);
        bs.goto = None;
        self.adjust_scroll_x();
    }

    /// Rows either side of the viewport a large buffer's highlight covers: a
    /// construct that opens outside the window is mis-coloured at its edges.
    pub(crate) const HIGHLIGHT_MARGIN: usize = 200;

    /// Characters either side of the viewport, for a row too long to hand over
    /// whole.
    pub(crate) const COL_MARGIN: usize = 4_000;

    /// A longer row is windowed by columns: past any hand-written line, under
    /// what a minifier produces.
    pub(crate) const LONG_ROW: usize = 8_000;

    /// The buffer rows the viewport shows, with the wrap segments of the first
    /// and last, before any margin: coverage is about the viewport.
    fn viewport_core(&self, text_h: usize) -> (usize, usize, usize, usize) {
        let bs = self.bs();
        // Visual row → buffer row, when the wrap table is fresh.
        if self.wrap && !bs.wrap_prefix.is_empty() {
            let (r0, s0) = self.buf_row_of_visual(bs.scroll);
            let (r1, s1) = self.buf_row_of_visual(bs.scroll + text_h.max(1));
            (r0, r1, s0, s1)
        } else {
            // The last visible row can be one past a short buffer; clamping
            // keeps the highlighter off a row that does not exist.
            let last = bs.buf.row_count().saturating_sub(1);
            (bs.scroll.min(last), (bs.scroll + text_h).min(last), 0, 0)
        }
    }

    /// The window a highlight should cover: the visible rows plus a margin, and
    /// the visible columns plus one for a row too long to hand over whole.
    fn highlight_window(&self, text_h: usize) -> syntax::Window {
        let bs = self.bs();
        let last_row = bs.buf.row_count().saturating_sub(1);
        let (first_vis, last_vis, first_seg, last_seg) = self.viewport_core(text_h);
        // Clamped here, where the indices are used.
        let last_row_now = bs.buf.row_count().saturating_sub(1);
        let first_vis = first_vis.min(last_row_now);
        let last_vis = last_vis.min(last_row_now);
        let first = first_vis.saturating_sub(Self::HIGHLIGHT_MARGIN);
        // A tail clamps the window to what is settled, so `style_at` answers
        // nothing for rows the file is still writing.
        let last = (last_vis + Self::HIGHLIGHT_MARGIN)
            .min(last_row)
            .min(bs.settled_last_row());
        let (first, last) = (first.min(last), last);
        // Column bounds matter only on the window's first and last row.
        let from = if bs.buf.row(first_vis).len() > Self::LONG_ROW {
            let (lo, _) = self.seg_chars(first_vis, first_seg);
            lo.saturating_sub(Self::COL_MARGIN)
        } else {
            0
        };
        let to = if bs.buf.row(last_vis).len() > Self::LONG_ROW {
            let (_, hi) = self.seg_chars(last_vis, last_seg);
            hi.saturating_add(Self::COL_MARGIN)
        } else {
            usize::MAX
        };
        syntax::Window {
            rows: (first, last),
            cols: (from, to),
        }
    }

    /// Whether the style grid still covers what the viewport shows.
    fn highlight_covers_viewport(&self) -> bool {
        let Some(have) = self.bs().hl.styled_window() else {
            // No window means the last refresh covered the whole document.
            return true;
        };
        let (r0, r1, _, _) = self.viewport_core(self.text_h);
        let last_row = self.bs().buf.row_count().saturating_sub(1);
        // Clamped to what is settled, exactly as `highlight_window` is.
        let settled = self.bs().settled_last_row();
        let (r0, r1) = (r0.min(last_row).min(settled), r1.min(last_row).min(settled));
        if r0 < have.rows.0 || r1 > have.rows.1 {
            return false;
        }
        // Columns too, for a row not handed over whole — what horizontal
        // scrolling moves through.
        let want = self.highlight_window(self.text_h);
        want.cols.0 >= have.cols.0 && want.cols.1 <= have.cols.1
    }

    /// Whether the style grid is behind the buffer; once per frame keeps typing
    /// cheap.
    pub(crate) fn ensure_highlight(&mut self) {
        // The wrap table first: the coverage check reads the viewport in visual
        // rows, and a stale table answers about the wrong rows.
        self.ensure_wrap_prefix();
        if !self.highlight_dirty && self.highlight_covers_viewport() {
            return;
        }
        self.highlight_dirty = false;
        self.highlight_now();
    }

    /// Refresh tree-sitter diagnostics at most once per pause, 300 ms after the
    /// last edit: a whole-document parse does not belong on a keystroke.
    pub(crate) fn diag_flush(&mut self, now: Instant) -> bool {
        if !self.diag_dirty || now < self.diag_last_edit + Duration::from_millis(300) {
            return false;
        }
        self.diag_dirty = false;
        let before = self.bs().syntax_diags.len() + self.bs().todo_diags.len();
        // The todo check rides the same debounce: nothing per pause, a liability
        // per keystroke.
        let todo = self.todo_check();
        // `refresh_syntax_diags` takes `&mut`, so this borrow is mutable
        // though only the diagnostics change.
        let bs = self.bs_mut();
        if bs.hl.parse_for_diagnostics(&bs.buf) {
            refresh_syntax_diags(bs);
        }
        bs.todo_diags = todo
            .into_iter()
            .map(|(line, col, end_col, message)| lsp::Diagnostic {
                line,
                col,
                end_col,
                message,
                severity: 1,
            })
            .collect();
        bs.syntax_diags.len() + bs.todo_diags.len() != before
    }

    /// Re-highlight for the current viewport — always a window, which covers the
    /// whole buffer when it is small enough.
    pub(crate) fn highlight_now(&mut self) {
        // Before the window is measured: an edit that added or removed a row
        // leaves the table short.
        self.ensure_wrap_prefix();
        let text_h = self.text_h;
        let win = self.highlight_window(text_h);
        let bs = self.bs_mut();
        bs.hl.refresh_window(&bs.buf, win);
        // Diagnostics need the whole document, so `diag_flush` does them on a
        // pause.
    }

    /// An edit whose one changed row is known: only that row is re-measured. A
    /// mistake here is slow, not wrong — the full rebuild is the fallback.
    pub(crate) fn edit_invalidate_row(&mut self, row: usize) {
        self.edit_invalidate_impl(Some(row));
    }

    /// An edit that touched other rows too: the wrap table rebuilds in full.
    pub(crate) fn edit_invalidate(&mut self) {
        self.edit_invalidate_impl(None);
    }

    fn edit_invalidate_impl(&mut self, row: Option<usize>) {
        {
            let bs = self.bs_mut();
            // An edit during a load: stop the rest from landing underneath.
            if let Some(job) = bs.load.as_ref() {
                job.cancel();
            }
            bs.load = None;
            bs.buf.modified = true;
            bs.search_matches = None;
            bs.search.current = 0;
            bs.edit_gen = bs.edit_gen.wrapping_add(1);
            bs.lsp_dirty = true;
            bs.wrap_dirty_row = row;
        }
        // Not highlighted here: the frame does it once, so a burst of keystrokes
        // between frames costs one highlight.
        self.highlight_dirty = true;
        self.diag_dirty = true;
        self.diag_last_edit = Instant::now();
    }

    /// The union of tree-sitter and LSP diagnostics, row-major.
    pub fn all_diags(&self) -> Vec<lsp::Diagnostic> {
        let bs = self.bs();
        let mut out = bs.syntax_diags.clone();
        out.extend(bs.todo_diags.iter().cloned());
        out.extend(bs.lsp_diags.iter().cloned());
        out.sort_by_key(|d| (d.line, d.col, d.end_col));
        out
    }

    /// The whitespace a Tab inserts and an auto-indent puts on a new line.
    pub fn indent_unit(&self) -> String {
        self.config.indent_unit(&self.bs().buf)
    }

    /// Tab: insert the indent unit. Spaces go to the next unit boundary, so a
    /// four-space buffer keeps its columns mid-indent.
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
    // Region-based: a step records the affected rows plus the row-count delta,
    // and a run of the same single-row action coalesces into one.

    const UNDO_LIMIT: usize = 500;

    // Row span [first, last_exclusive) of the current selection, if non-empty.
    pub(crate) fn sel_span(&self) -> Option<(usize, usize)> {
        let mark = self.bs().mark?;
        let (a, b) = normalize(mark, self.bs().cursor);
        (a != b).then_some((a.row, b.row))
    }

    // Empty region allowed: first_row == last_row_exclusive, a pure insertion.
    pub(crate) fn begin_action(
        &mut self,
        kind: ActionKind,
        first_row: usize,
        last_row_exclusive: usize,
    ) {
        let bs = self.bs_mut();
        bs.redo.clear();
        let len = bs.buf.row_count();
        let first = first_row.min(len);
        let last = last_row_exclusive.min(len);
        let before = if first < last {
            bs.buf.rows_range(first, last)
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

    // Coalescing: runs of Insert/Backspace/Delete on one row, repeated ^K cuts.
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
        let delta = self.bs().buf.row_count() as isize - len_at_begin as isize;
        let count = (before_len as isize + delta).max(0) as usize;
        let end = (after_start + count).min(self.bs().buf.row_count());
        if after_start >= end {
            Vec::new()
        } else {
            self.bs().buf.rows_range(after_start, end)
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

    /// Make this document a view: read, navigate, search, but do not edit or
    /// write it. The refusal is at the keystroke, not at the save.
    pub fn set_read_only(&mut self, on: bool) {
        self.bs_mut().read_only = on;
    }

    /// Refuse `what` because this buffer is read-only; true when it refused. One
    /// gate for every mutation — a mode that guards nine of ten ways to change a
    /// buffer is worse than none.
    pub(crate) fn refuse_read_only(&mut self, what: &str) -> bool {
        if !self.bs().read_only {
            return false;
        }
        self.flash(&format!("Read-only buffer — {what} refused"));
        true
    }

    pub(crate) fn undo(&mut self) {
        if self.refuse_read_only("undo") {
            return;
        }
        if let Some(step) = self.bs_mut().undo.pop_back() {
            let bs = self.bs_mut();
            let start = step.after_start.min(bs.buf.row_count());
            let end = (step.after_start + step.after.len())
                .min(bs.buf.row_count())
                .max(start);
            bs.buf.splice_rows(start, end, step.before.iter().cloned());
            bs.cursor = step.cur_before;
            bs.last_kind = None;
            self.edit_invalidate();
            self.clamp_cursor();
            self.push_redo(step);
        }
    }

    pub(crate) fn redo(&mut self) {
        if self.refuse_read_only("redo") {
            return;
        }
        if let Some(step) = self.bs_mut().redo.pop_back() {
            let bs = self.bs_mut();
            let start = step.start.min(bs.buf.row_count());
            let end = (step.start + step.before.len())
                .min(bs.buf.row_count())
                .max(start);
            bs.buf.splice_rows(start, end, step.after.iter().cloned());
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

    /// Insert text at the cursor as one undo step, without triggering completion.
    fn insert_str_plain(&mut self, s: &str) {
        if self.refuse_read_only("typing") {
            return;
        }
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

    /// Ask the language server for completions where the context warrants it,
    /// keeping the popup alive across requests; close it outside that context.
    pub(crate) fn maybe_request_completion(&mut self) {
        let cur = self.bs().cursor;
        let Some((prefix, start)) = completion_prefix(&self.bs().buf.rows_vec(), cur.row, cur.col)
        else {
            self.completion = None;
            self.completion_q.clear();
            return;
        };
        // Push the exact text first: the didChange debounce would complete
        // against a stale document.
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
        let line = bs.buf.row_opt(cur.row).cloned().unwrap_or_default();
        l.request_completion(&uri, cur.row as u64, lsp::utf16_col(&line, cur.col) as u64);
        self.completion_q.push_back((cur.row, prefix));
        if self.completion_q.len() > 8 {
            self.completion_q.drain(0..self.completion_q.len() - 8);
        }
        // Anchor an existing popup where it opened; moving it per keystroke
        // reads as flicker.
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

    /// Apply a completion response: sort by sortText, prefix matches first then
    /// fuzzy ones, and only when it answers the request for the current context.
    /// An empty prefix keeps the server's order.
    pub(crate) fn complete_response(&mut self, items: Vec<lsp::CompletionItem>) {
        let Some((req_row, req_prefix)) = self.completion_q.pop_front() else {
            return;
        };
        let cur = self.bs().cursor;
        let Some((prefix, _)) = completion_prefix(&self.bs().buf.rows_vec(), cur.row, cur.col)
        else {
            self.completion = None;
            self.completion_q.clear();
            return;
        };
        if req_row != cur.row || req_prefix != prefix {
            return;
        }
        // A `.` context answered with path items means the crate scan is not
        // done; re-request shortly instead of flashing fallback junk.
        let dot_ctx = cur.col > 0 && self.bs().buf.row(cur.row).get(cur.col - 1) == Some(&'.');
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
        // The server's JSON order is not its ranked order; sortText is.
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

    /// Fire a scheduled completion re-request once its timer elapses, if the
    /// popup is still open on the cursor's row.
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
        if completion_prefix(&self.bs().buf.rows_vec(), cur.row, cur.col).is_none() {
            self.completion_retries = 0;
            self.completion = None;
            return;
        }
        self.maybe_request_completion();
    }

    /// Insert the selected completion: append the part past the typed prefix, or
    /// replace the prefix when the item's text diverges from it.
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
        let Some((prefix, _)) = completion_prefix(&self.bs().buf.rows_vec(), cur.row, cur.col)
        else {
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

    /// M-.: ask for the definition of the symbol under the cursor and jump there.
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
        let line = bs.buf.row_opt(cur.row).cloned().unwrap_or_default();
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
            // Named even for a same-file jump: M-, may come after a switch.
            self.def_back.push(DefBack {
                buf: None,
                idx: Some(self.cur),
                pos: from,
            });
        } else if let Some(i) = self.find_buffer(&target_path) {
            // Already open: a second copy would split its edits across two
            // buffers.
            self.def_back.push(DefBack {
                buf: None,
                idx: Some(self.cur),
                pos: from,
            });
            self.cur = i;
            self.highlight_dirty = true;
            self.load_now();
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
            self.highlight_dirty = true;
            self.lsp_sync();
        }
        let row = loc.line as usize;
        let line = self.bs().buf.row_opt(row).cloned().unwrap_or_default();
        let col = lsp::utf16_to_char(&line, loc.character as usize);
        let bs = self.bs_mut();
        bs.cursor = bs.buf.clamp(Pos { row, col });
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
        // Another buffer (or the same one swapped back) is on screen.
        self.highlight_dirty = true;
        let pos = entry.pos;
        let bs = self.bs_mut();
        bs.cursor = bs.buf.clamp(pos);
        self.adjust_scroll(self.text_h);
        self.adjust_scroll_x();
    }

    // ---------- mouse ----------

    /// Give the editor its place on the screen: the whole terminal for the
    /// binary, a pane for a host.
    pub fn set_area(&mut self, area: Area) -> bool {
        if area == self.area {
            return false;
        }
        self.area = area;
        self.text_w = area.w as usize;
        self.text_h = (area.h as usize).saturating_sub(self.chrome.rows());
        // A picture's box follows the frame, as a PNG's view does in
        // `refresh_diff_view`.
        self.recut_picture();
        true
    }

    /// Draw with or without nano's own bars; the status row stays either way.
    pub fn set_chrome(&mut self, chrome: Chrome) {
        self.chrome = chrome;
        if !self.area.is_empty() {
            self.text_h = (self.area.h as usize).saturating_sub(chrome.rows());
        }
    }

    /// What the title bar would say, for a host drawing its own.
    pub fn frame_title(&self) -> FrameTitle {
        let bs = self.bs();
        FrameTitle {
            name: self.title_text(),
            modified: bs.buf.modified,
            read_only: bs.read_only,
        }
    }

    /// The function bar's entries, for a host drawing its own: `(key, label)` in
    /// its order.
    pub fn key_hints(&self) -> Vec<(String, String)> {
        self.bar_items()
    }

    /// Map a pane cell to a buffer position: only clicks inside the text area
    /// land; the gutter or past EOL goes to the line start/end.
    fn mouse_pos(&self, pane_row: u16, pane_col: u16) -> Option<Pos> {
        let top = self.chrome.top();
        if self.prompt.is_some() || pane_row < top || (pane_row - top) as usize >= self.text_h {
            return None;
        }
        // From here `pane_row` counts as though the title row were there.
        let pane_row = pane_row + 1 - top;
        let bs = self.bs();
        // With wrap on, the pane row is a visual row.
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
        if row >= bs.buf.row_count() {
            return None;
        }
        let g = self.gutter_w();
        let x = pane_col as usize;
        if x < g {
            return Some(Pos { row, col: 0 });
        }
        let disp = if self.wrap {
            let (d0, _) = self.seg_disp(row, seg);
            d0 + (x - g)
        } else {
            bs.scroll_x + (x - g)
        };
        let line = bs.buf.row(row);
        Some(Pos {
            row,
            col: ui::char_at_display(line, disp, self.tab_width),
        })
    }

    /// Left click: the cursor, and on the cursor itself the mark (nano's rule).
    /// Drag: extend. Release without a drag: drop the anchor. Wheel: scroll.
    pub fn handle_mouse(&mut self, m: MouseEvent) -> bool {
        let mut m = m;
        if !self.area.is_empty() {
            if !self.area.contains(m.x, m.y) {
                return false;
            }
            m.x -= self.area.x;
            m.y -= self.area.y;
        }
        // The list overlay is keyboard-only; the text under it is not there to click.
        if self.picker.is_some() {
            return false;
        }
        // A view over the text takes the wheel and nothing else: a click has
        // nothing to point at, and must not move the cursor underneath.
        if self.diff_view.is_some() {
            return match m.kind {
                MouseKind::WheelUp => {
                    self.diff_act(DiffAct::Scroll(-WHEEL_LINES));
                    true
                }
                MouseKind::WheelDown => {
                    self.diff_act(DiffAct::Scroll(WHEEL_LINES));
                    true
                }
                _ => false,
            };
        }
        self.ensure_wrap_prefix();
        match m.kind {
            MouseKind::WheelUp => self.wheel(-WHEEL_LINES as i64),
            MouseKind::WheelDown => self.wheel(WHEEL_LINES as i64),
            MouseKind::Press(MouseButton::Left) => match self.mouse_pos(m.y, m.x) {
                Some(p) => {
                    let bs = self.bs_mut();
                    let at = bs.buf.clamp(p);
                    let at_cursor = at == bs.cursor;
                    // nano's click: on the cursor itself the mark toggles (the
                    // gesture that starts a selection); elsewhere it places the cursor.
                    if at_cursor {
                        bs.mark = match bs.mark {
                            None => Some(bs.cursor),
                            Some(_) => None,
                        };
                    } else {
                        bs.cursor = at;
                        bs.mark = Some(at);
                    }
                    self.mouse_arm = !at_cursor;
                    self.completion_close();
                    true
                }
                None => false,
            },
            MouseKind::Release(MouseButton::Left) => {
                // A press that never dragged was a click, and a click places the
                // cursor: its anchor goes. A drag's selection is kept.
                let armed = std::mem::take(&mut self.mouse_arm);
                let bs = self.bs_mut();
                let dropped = armed && bs.mark == Some(bs.cursor);
                if dropped {
                    bs.mark = None;
                }
                // A mouse selection goes to the system clipboard: the drag
                // selected here, so the terminal's own copy has nothing.
                if !dropped {
                    self.copy_to_clipboard();
                }
                dropped
            }
            MouseKind::Drag(MouseButton::Left) => match self.mouse_pos(m.y, m.x) {
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

    /// Wheel: scroll the viewport without moving the edit point; the cursor is
    /// pinned to the edge it would cross, and only when the scroll pushes it out.
    fn wheel(&mut self, delta: i64) -> bool {
        let text_h = self.text_h;
        if self.wrap {
            // Scroll in visual rows; the cursor is pinned at that edge's row start/end.
            self.ensure_wrap_prefix();
            let total = self.bs().wrap_prefix.last().copied().unwrap_or(0);
            let max_scroll = total.saturating_sub(text_h);
            let scroll = (self.bs().scroll as i64 + delta).clamp(0, max_scroll as i64) as usize;
            let cv = self.visual_pos(self.bs().cursor);
            let pin = if cv < scroll {
                let (r, seg) = self.buf_row_of_visual(scroll);
                let (lo, _) = self.seg_chars(r, seg);
                Some(Pos { row: r, col: lo })
            } else if text_h > 0 && cv >= scroll + text_h {
                let (r, seg) = self.buf_row_of_visual(scroll + text_h - 1);
                let (_, hi) = self.seg_chars(r, seg);
                Some(Pos { row: r, col: hi })
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
        let max_scroll = bs.buf.row_count().saturating_sub(text_h);
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
        if self.refuse_read_only("typing") {
            return;
        }
        let (first, last) = match self.sel_span() {
            Some((a, b)) => (a, b + 1),
            None => (self.bs().cursor.row, self.bs().cursor.row + 1),
        };
        self.begin_action(ActionKind::Insert, first, last);
        self.delete_selection_if_any();
        let bs = self.bs_mut();
        bs.buf.insert_char(bs.cursor.row, bs.cursor.col, ch);
        bs.cursor.col += 1;
        let row = self.bs().cursor.row;
        self.finish_step();
        // Typing: the row is known and none was added, so one row is re-measured.
        self.edit_invalidate_row(row);
        self.maybe_request_completion();
    }

    pub(crate) fn newline(&mut self) {
        if self.refuse_read_only("a new line") {
            return;
        }
        let (first, last) = match self.sel_span() {
            Some((a, b)) => (a, b + 1),
            None => (self.bs().cursor.row, self.bs().cursor.row + 1),
        };
        self.begin_action(ActionKind::Newline, first, last);
        self.delete_selection_if_any();
        let row = self.bs().cursor.row;
        let col = self.bs().cursor.col;
        // With auto_indent, carry the leading whitespace onto the new row; an
        // opening brace puts the cursor one unit deeper.
        let indent: Vec<char> = if self.config.auto_indent {
            let mut ind: Vec<char> = self
                .bs()
                .buf
                .row(row)
                .iter()
                .take(col)
                .take_while(|c| c.is_whitespace())
                .copied()
                .collect();
            let opens = self.bs().buf.row(row)[..col]
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
            self.bs_mut().buf.row_mut(row + 1).splice(0..0, indent);
            self.bs_mut().cursor = Pos {
                row: row + 1,
                col: n,
            };
        }
        self.finish_step();
        self.edit_invalidate();
    }

    pub(crate) fn backspace(&mut self) {
        if self.refuse_read_only("backspace") {
            return;
        }
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
        let mut same_row = false;
        if c.col > 0 {
            same_row = true;
            // Soft tabs: on leading whitespace, backspace eats a whole indent unit.
            let unit = self.indent_unit();
            let line = self.bs().buf.row(c.row).clone();
            let on_indent =
                unit != "\t" && c.col <= line.len() && line[..c.col].iter().all(|ch| *ch == ' ');
            let n = if on_indent {
                ((c.col - 1) % unit.len()) + 1
            } else {
                1
            };
            let bs = self.bs_mut();
            for _ in 0..n {
                // backspace removes a row it empties; stop before the index goes stale.
                if bs.cursor.col == 0 || bs.buf.row_opt(c.row).is_none() {
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
        if same_row {
            self.edit_invalidate_row(c.row);
        } else {
            // Joined two rows: the whole table below shifts.
            self.edit_invalidate();
        }
        if had_popup {
            self.maybe_request_completion();
        }
    }

    pub(crate) fn delete_at(&mut self) {
        if self.refuse_read_only("deletion") {
            return;
        }
        let c = self.bs().cursor;
        let sel = self.sel_span();
        let can_delete =
            c.col < self.bs().buf.line_len(c.row) || c.row + 1 < self.bs().buf.row_count();
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
        let same_row = c.col < self.bs().buf.line_len(c.row);
        self.bs_mut().buf.delete_at(c.row, c.col);
        self.clamp_cursor();
        self.finish_step();
        if same_row {
            self.edit_invalidate_row(c.row);
        } else {
            self.edit_invalidate();
        }
    }

    // ---------- cut / paste ----------

    pub(crate) fn cut(&mut self) {
        if self.refuse_read_only("cut") {
            return;
        }
        // What was cut goes to the clipboard too, read before the selection goes.
        let clip = self.selection_text().or_else(|| {
            // A whole line, which is `^K`'s own unit, with its newline.
            let c = self.bs().cursor;
            let line: String = self.bs().buf.row(c.row).iter().collect();
            Some(format!("{line}\n"))
        });
        self.clipboard_out = clip;
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
                let line = bs.buf.remove_row(c.row);
                let empty = bs.buf.rows_is_empty();
                (line, empty)
            };
            if empty {
                self.bs_mut().buf.push_row(Vec::new());
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
                let frag: Vec<char> = bs.buf.row_mut(c.row).drain(c.col..).collect();
                let gone = bs.buf.row(c.row).is_empty() && bs.buf.row_count() > 1;
                (frag, gone)
            };
            if gone {
                self.bs_mut().buf.remove_row(c.row);
            }
            self.cut = vec![frag];
            self.cut_line = false;
            self.clamp_cursor();
        }
        self.finish_step();
        self.edit_invalidate();
    }

    pub(crate) fn paste(&mut self) {
        if self.refuse_read_only("paste") {
            return;
        }
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

    /// Bracketed paste. \r is stripped, newlines split into rows; the whole
    /// paste (including selection replacement) is one undo step.
    pub fn paste_text(&mut self, s: &str) {
        if self.refuse_read_only("paste") {
            return;
        }
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

    /// The marked region as text, rows joined with newlines: the form a reader
    /// pastes elsewhere. A whole-line selection keeps its trailing newline.
    pub fn selection_text(&self) -> Option<String> {
        let mark = self.bs().mark?;
        let (a, b) = normalize(mark, self.bs().cursor);
        if a == b {
            return None;
        }
        let mut out = self
            .bs()
            .buf
            .copy_range(a, b)
            .iter()
            .map(|row| row.iter().collect::<String>())
            .collect::<Vec<String>>()
            .join("\n");
        // `copy_range` trims both ends, so a line-wise selection is missing its
        // newline and pasting it would join it to what follows.
        if a.row < b.row && (a.col == 0 || b.col == 0) {
            out.push('\n');
        }
        Some(out)
    }

    /// Put the marked region on the terminal's clipboard (OSC 52). Queued rather
    /// than written: the terminal is the host's, drained with [`Self::take_clipboard`].
    pub(crate) fn copy_to_clipboard(&mut self) {
        if let Some(text) = self.selection_text() {
            self.clipboard_out = Some(text);
        }
    }

    /// Take the text queued for the terminal's clipboard, if any; a host writes
    /// it with `term::Terminal::copy`.
    pub fn take_clipboard(&mut self) -> Option<String> {
        self.clipboard_out.take()
    }

    /// M-6: copy the current line (or marked region) without deleting it; the
    /// mark stays and the region also goes to the system clipboard.
    pub(crate) fn copy(&mut self) {
        if let Some(mark) = self.bs().mark {
            let (a, b) = normalize(mark, self.bs().cursor);
            if a != b {
                let rows = self.bs_mut().buf.copy_range(a, b);
                self.cut = rows;
                self.cut_line = a.col == 0 && b.row > a.row;
                self.copy_to_clipboard();
                return;
            }
        }
        let c = self.bs().cursor;
        let line = self.bs().buf.row(c.row).clone();
        self.cut = vec![line];
        self.cut_line = true;
        // A whole line, with its newline: what `^K` would have taken.
        self.clipboard_out = Some(format!("{}\n", self.cut[0].iter().collect::<String>()));
    }

    pub(crate) fn delete_char_cut(&mut self) {
        if self.refuse_read_only("deletion") {
            return;
        }
        let c = self.bs().cursor;
        if c.col < self.bs().buf.line_len(c.row) {
            self.begin_action(ActionKind::Delete, c.row, c.row + 1);
            let ch = {
                let bs = self.bs_mut();
                let ch = bs.buf.row(c.row)[c.col];
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
        } else if c.row + 1 < self.bs().buf.row_count() {
            let bs = self.bs_mut();
            bs.cursor.row += 1;
            bs.cursor.col = 0;
        }
    }

    /// Move the cursor to visual row `v`, keeping the column inside it.
    fn goto_visual_row(&mut self, v: usize) {
        let c = self.bs().cursor;
        let within = self.disp_in_seg(c.row, c.col);
        let (r, seg) = self.buf_row_of_visual(v);
        let col = self.col_in_seg(r, seg, within);
        self.bs_mut().cursor = Pos { row: r, col };
    }

    pub(crate) fn move_up(&mut self) {
        if self.wrap {
            // One visual row up: a wrapped line is traversed segment by segment.
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
        if self.bs().cursor.row + 1 < self.bs().buf.row_count() {
            let bs = self.bs_mut();
            bs.cursor.row += 1;
            bs.cursor.col = bs.cursor.col.min(bs.buf.line_len(bs.cursor.row));
        }
    }

    pub(crate) fn move_home(&mut self) {
        if self.wrap {
            // Start of the visual row, not of the buffer row.
            let cv = self.visual_pos(self.bs().cursor);
            let (r, seg) = self.buf_row_of_visual(cv);
            let (lo, _) = self.seg_chars(r, seg);
            self.bs_mut().cursor.col = lo;
            return;
        }
        self.bs_mut().cursor.col = 0;
    }

    pub(crate) fn move_end(&mut self) {
        if self.wrap {
            // End of the visual row, not of the buffer row.
            let cv = self.visual_pos(self.bs().cursor);
            let (r, seg) = self.buf_row_of_visual(cv);
            let (_, hi) = self.seg_chars(r, seg);
            self.bs_mut().cursor.col = hi;
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
        bs.cursor.row = (bs.cursor.row + step).min(bs.buf.row_count().saturating_sub(1));
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
            if self.bs().buf.row(r)[c - 1].is_whitespace() {
                c -= 1;
                continue;
            }
            break;
        }
        c -= 1;
        if is_word(self.bs().buf.row(r)[c]) {
            while c > 0 && is_word(self.bs().buf.row(r)[c - 1]) {
                c -= 1;
            }
        }
        self.bs_mut().cursor = Pos { row: r, col: c };
    }

    // Next Word (nano's ^Right): move forward one word.
    pub(crate) fn next_word(&mut self) {
        let is_word = |ch: char| ch.is_alphanumeric() || ch == '_';
        let mut r = self.bs().cursor.row;
        let mut c = self.bs().cursor.col;
        let nlines = self.bs().buf.row_count();
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
            if self.bs().buf.row(r)[c].is_whitespace() {
                c += 1;
                continue;
            }
            break;
        }
        let len = self.bs().buf.line_len(r);
        if is_word(self.bs().buf.row(r)[c]) {
            while c < len && is_word(self.bs().buf.row(r)[c]) {
                c += 1;
            }
        } else {
            c += 1;
        }
        self.bs_mut().cursor = Pos { row: r, col: c };
    }

    // To Bracket (nano's M-]): jump to the matching bracket.
    pub(crate) fn match_bracket(&mut self) {
        let pairs: [(char, char); 3] = [('(', ')'), ('{', '}'), ('[', ']')];
        let start = self.bs().cursor;
        // Copy the at-cursor chars out so the search below can borrow the
        // buffer freely.
        let window: Vec<char> = {
            let line = self.bs().buf.row(start.row);
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
        let start_ch = self.bs().buf.row(row)[col];
        let nlines = self.bs().buf.row_count() as i64;
        let mut depth: i32 = 0;
        let mut r = row as i64;
        let mut c = col as i64;
        loop {
            if r < 0 || r >= nlines {
                return None;
            }
            let line = self.bs().buf.row(r as usize);
            let llen = line.len() as i64;
            if c < 0 {
                r -= 1;
                if r < 0 {
                    return None;
                }
                c = self.bs().buf.row(r as usize).len() as i64 - 1;
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
        let n = n.clamp(1, self.bs().buf.row_count());
        self.bs_mut().cursor = Pos { row: n - 1, col: 0 };
        self.loc_until = Some(Instant::now() + Duration::from_secs(2));
    }

    // ---------- the mode ----------

    /// The mode in effect as a person reads it: the one they chose, or what the
    /// ladder detected. Bare, so the prompt can open on it.
    pub fn mode_name(&self) -> String {
        match self.bs().hl.mode {
            syntax::Mode::Plain => "plain".to_string(),
            syntax::Mode::Lang(l) => l.name().to_string(),
            syntax::Mode::Detected => syntax::language_of(&self.bs().buf)
                .map(|l| l.name().to_string())
                .unwrap_or_else(|| "plain".to_string()),
        }
    }

    /// Ask for a mode, opening on the one in effect: an editor that guesses
    /// must be able to be told it guessed wrong.
    pub fn start_setmode(&mut self) {
        let text = self.mode_name();
        let cursor = text.chars().count();
        self.prompt = Some(Prompt {
            kind: PromptKind::SetMode,
            text,
            cursor,
        });
    }

    /// Apply what was typed: a language's name, `plain` to stop the
    /// guessing, `auto` (or empty) to put the detection back.
    pub fn do_setmode(&mut self, text: &str) {
        let t = text.trim();
        let mode =
            if t.is_empty() || t.eq_ignore_ascii_case("auto") || t.eq_ignore_ascii_case("detect") {
                syntax::Mode::Detected
            } else if matches!(t.to_ascii_lowercase().as_str(), "plain" | "text" | "none") {
                syntax::Mode::Plain
            } else if let Some(l) = syntax::Lang::from_name(t) {
                syntax::Mode::Lang(l)
            } else {
                self.flash(&format!("No mode called {t:?}"));
                return;
            };
        self.set_mode(mode);
    }

    /// Set the mode and invalidate what it decided: the colour grid and the
    /// diagnostics were the old mode's.
    pub fn set_mode(&mut self, mode: syntax::Mode) {
        self.bs_mut().hl.set_mode(mode);
        self.highlight_dirty = true;
        self.diag_dirty = true;
        let auto = matches!(mode, syntax::Mode::Detected);
        let said = self.mode_name();
        self.flash(&format!(
            "Mode: {said}{}",
            if auto { " (auto)" } else { "" }
        ));
    }

    // ---------- write / read / backup ----------

    pub(crate) fn start_write(&mut self) {
        if self.refuse_read_only("writing") {
            return;
        }
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
        if self.refuse_read_only("writing") {
            return;
        }
        if name.trim().is_empty() {
            return;
        }
        let path = PathBuf::from(expand_tilde(&name));
        // Writing back to the buffer's own file is a save, not an overwrite.
        let own = self
            .bs()
            .buf
            .name
            .as_deref()
            .is_some_and(|n| canonical(n) == canonical(&path));
        if path.exists() && !own {
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

    /// Save to `path`, unless it is this buffer's own file and something else
    /// wrote it since it was read: then ask (y, n, d) — see `answer_external`.
    pub(crate) fn save_to(&mut self, path: PathBuf) {
        if self.refuse_read_only("writing") {
            return;
        }
        if self.changed_on_disk(&path) {
            self.pending_write = Some(path);
            self.prompt = Some(Prompt {
                kind: PromptKind::ConfirmExternal,
                text: String::new(),
                cursor: 0,
            });
            return;
        }
        self.write_file(path);
    }

    /// Whether `path` is the buffer's own file and no longer looks the way it did
    /// when it was read or last written; checked at save time only.
    fn changed_on_disk(&self, path: &Path) -> bool {
        let buf = &self.bs().buf;
        let (Some(stamp), Some(name)) = (buf.disk, buf.name.as_deref()) else {
            return false;
        };
        canonical(name) == canonical(path) && DiskStamp::of(path) != Some(stamp)
    }

    /// The answer to "File changed on disk since it was read".
    pub(crate) fn answer_external(&mut self, c: char) {
        match c {
            'y' | 'Y' => {
                if let Some(path) = self.pending_write.take() {
                    self.write_file(path);
                }
            }
            'n' | 'N' => self.cancel_external_save(),
            'd' | 'D' => self.show_external_diff(),
            _ => self.reask_external(),
        }
    }

    /// Not saving after all: the quit or close the save was for is off too.
    pub(crate) fn cancel_external_save(&mut self) {
        self.pending_write = None;
        self.quit_after_save = false;
        self.close_after_save = false;
        self.flash("Not saved");
    }

    pub(crate) fn reask_external(&mut self) {
        self.prompt = Some(Prompt {
            kind: PromptKind::ConfirmExternal,
            text: String::new(),
            cursor: 0,
        });
    }

    /// `d`: what saving would change in the file on disk now, as a diff view;
    /// Esc comes back to the question, y and n answer it from there.
    fn show_external_diff(&mut self) {
        let Some(path) = self.pending_write.clone() else {
            return;
        };
        let mine = match self.bs().buf.file_bytes() {
            Ok(b) => b,
            Err(e) => {
                self.flash(&format!("Cannot encode the buffer: {e}"));
                self.reask_external();
                return;
            }
        };
        // A file that is gone is diffed as empty: every line shows as added.
        let disk = fs::read(&path).unwrap_or_default();
        if disk == mine {
            self.flash("No differences: only the file's timestamp changed");
            self.reask_external();
            return;
        }
        // Both sides read the way the buffer reads its file, so an encoding
        // difference is not every line changed.
        let enc = self.bs().buf.encoding;
        let decode = |b: &[u8]| {
            crate::encoding::decode(b, enc)
                .unwrap_or_else(|_| String::from_utf8_lossy(b).into_owned())
        };
        let (disk, mine) = (decode(&disk), decode(&mine));
        if disk.lines().eq(mine.lines()) {
            self.flash("No differences in the text: only line endings or encoding");
            self.reask_external();
            return;
        }
        self.open_diff_view(&path, disk, mine);
    }

    /// Answer to "the directory does not exist": `y` makes the directories and
    /// retries the write; `n` drops the save and keeps the buffer modified.
    pub(crate) fn answer_mkdir(&mut self, c: char) {
        match c {
            'y' | 'Y' => {
                let Some(dir) = self.pending_dir.clone() else {
                    return;
                };
                match fs::create_dir_all(&dir) {
                    Ok(()) => {
                        if let Some(path) = self.pending_write.take() {
                            self.pending_dir = None;
                            self.write_file(path);
                        }
                    }
                    Err(e) => {
                        // The quit or close this save was for is off too, as for
                        // any failed write.
                        self.pending_write = None;
                        self.pending_dir = None;
                        self.quit_after_save = false;
                        self.close_after_save = false;
                        self.flash(&format!("Cannot create {}: {e}", dir.display()));
                    }
                }
            }
            'n' | 'N' => {
                let dir = self.pending_dir.take();
                self.pending_write = None;
                self.quit_after_save = false;
                self.close_after_save = false;
                match dir {
                    Some(d) => self.flash(&format!("Not saved: {} does not exist", d.display())),
                    None => self.flash("Not saved"),
                }
            }
            _ => {}
        }
    }

    /// gofmt on save, for a Go file: formatted before the write, as one undo
    /// step. Silent when nothing changed, or when gofmt is missing.
    fn format_for_save(&mut self) {
        let is_go = self
            .bs()
            .buf
            .name
            .as_deref()
            .and_then(Path::extension)
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("go"));
        if !is_go {
            return;
        }
        let bin = crate::lsp::find_bin("gofmt");
        if bin == "gofmt" {
            return;
        }
        let before = self.bs().buf.text();
        match crate::exec_ctrl::run_filter(&bin, &[], &before, FORMAT_TIMEOUT) {
            Ok(formatted) if formatted != before => {
                let mut rows: Vec<Vec<char>> =
                    formatted.lines().map(|l| l.chars().collect()).collect();
                if rows.is_empty() {
                    rows.push(Vec::new());
                }
                let n = self.bs().buf.row_count();
                self.begin_action(ActionKind::Format, 0, n);
                self.bs_mut().buf.splice_rows(0, n, rows);
                let last = self.bs().buf.row_count().saturating_sub(1);
                let c = self.bs().cursor;
                self.bs_mut().cursor = Pos {
                    row: c.row.min(last),
                    col: c.col,
                };
                self.finish_step();
                self.edit_invalidate();
                self.flash("gofmt: formatted on save (M-U undoes)");
            }
            Ok(_) => {}
            Err(e) => {
                let line = e.lines().next().unwrap_or("").to_string();
                self.flash(&format!("Saved as written: gofmt {line}"));
            }
        }
    }

    fn write_file(&mut self, path: PathBuf) {
        // A save into a missing directory is offered rather than failed: the
        // reader typed the path on purpose.
        if let Some(dir) = path
            .parent()
            .filter(|d| !d.as_os_str().is_empty() && !d.is_dir())
            .map(Path::to_path_buf)
        {
            self.pending_write = Some(path);
            self.pending_dir = Some(dir);
            self.prompt = Some(Prompt {
                kind: PromptKind::ConfirmMkdir,
                text: String::new(),
                cursor: 0,
            });
            return;
        }
        // gofmt after the questions, so a save the reader backed out of changes
        // nothing.
        self.format_for_save();
        // The plugin's `on_save`: its answer is what is written and what the
        // buffer becomes.
        self.run_save_hook();
        // The buffer's own encoding: a file read as cp1252 is written back so.
        let encoded = match self.bs().buf.file_bytes() {
            Ok(b) => b,
            Err(e) => {
                self.flash(&format!("Cannot save: {e}"));
                return;
            }
        };
        let bytes = encoded.len();
        match fs::write(&path, encoded) {
            Ok(()) => {
                {
                    let bs = self.bs_mut();
                    bs.buf.disk = DiskStamp::of(&path);
                    bs.buf.name = Some(path);
                    bs.buf.modified = false;
                    // The file's name changed, so its language may have too.
                    bs.hl.refresh(&bs.buf);
                    refresh_syntax_diags(bs);
                }
                // lsp_sync below may kill the client; flush first.
                self.lsp_flush(Instant::now());
                self.lsp_sync();
                self.flash_with_note(&format!("Wrote {bytes} bytes"));
                if self.close_after_save {
                    self.close_after_save = false;
                    self.remove_current_buffer();
                }
                if self.quit_after_save {
                    self.quit_after_save = false;
                    // Re-attempt the quit: further modified buffers get the prompt.
                    self.try_quit();
                }
            }
            Err(e) => {
                // The quit or close this save was for is off: still unsaved.
                self.quit_after_save = false;
                self.close_after_save = false;
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

    // F8: open a file — a new buffer with config.multibuffer, else in place.
    pub(crate) fn start_open(&mut self) {
        self.prompt = Some(Prompt {
            kind: PromptKind::OpenName,
            text: String::new(),
            cursor: 0,
        });
    }

    /// Open `path` as a folder listing — the dired: read-only, `RET` enters what
    /// the cursor is on, and unsaved edits are never thrown away for it.
    pub fn open_dir(&mut self, path: &Path) -> bool {
        let rows = match crate::dirview::rows(path) {
            Ok(rows) => rows,
            Err(e) => {
                self.flash(&format!("Cannot read {}: {e}", path.display()));
                return false;
            }
        };
        let text = format!("{}\n", rows.join("\n"));
        self.install_view(path, &text, true);
        self.flash_with_note(&format!(
            "{}\u{2009}entr{}",
            rows.len(),
            if rows.len() == 1 { "y" } else { "ies" }
        ));
        true
    }

    /// A read-only buffer holding a view of `path` — a folder listing or a
    /// provider's text; `dir_mode` decides what `RET`/`-`/`r` mean.
    fn install_view(&mut self, path: &Path, text: &str, dir_mode: bool) {
        // A view replaces a view: a picture the terminal held goes with its bytes.
        self.close_picture();
        let buf = Buffer::from_text(
            text,
            Some(path.to_path_buf()),
            crate::encoding::Encoding::Utf8,
        );
        let mut state = BufferState::new(buf);
        state.read_only = true;
        state.synthetic = true;
        state.dir = dir_mode.then(|| path.to_path_buf());
        let kept = !self.config.multibuffer && self.bs().buf.modified;
        let scratch = {
            let b = &self.bs().buf;
            b.name.is_none() && !b.modified && b.row_count() <= 1 && b.text().is_empty()
        };
        if (self.config.multibuffer && !scratch) || kept {
            self.buffers.push(state);
            self.cur = self.buffers.len() - 1;
        } else {
            self.buffers[self.cur] = state;
        }
        self.highlight_dirty = true;
        self.lsp_sync();
        self.adjust_scroll(self.text_h);
        self.adjust_scroll_x();
    }

    /// Open a path the way its providers say, else the way [`Self::start_named`] does.
    pub fn open_named(&mut self, path: &Path) -> std::io::Result<()> {
        if self.ask_providers(path) {
            return Ok(());
        }
        self.start_named(path)
    }

    /// Ask the providers whether this path opens as something else; one door for
    /// the command line, `M->`, F8 and a host's jump.
    pub fn ask_providers(&mut self, path: &Path) -> bool {
        let cols = self.text_w as u32;
        let rows = self.text_h as u32;
        // A provider that draws is asked first: a picture wins, the text answer is
        // the fallback.
        let mut drawn: Option<(PathBuf, crate::plugin::Picture)> = None;
        for plugin in self.plugin.iter().chain(self.builtins.iter()) {
            if !plugin.has("provider_picture") {
                continue;
            }
            match plugin.provider_picture(&path.to_string_lossy(), cols, rows) {
                Ok(Some(answer)) => {
                    drawn = Some((path.to_path_buf(), answer));
                    break;
                }
                Ok(None) => {}
                Err(e) => {
                    self.plugin_note = Some(format!(
                        "plugin {}: {e} — opened the usual way",
                        plugin.path().display()
                    ));
                }
            }
        }
        // Drawn only if the terminal can carry it; a decline falls through to the
        // text hook.
        if let Some((at, answer)) = drawn
            && self.open_provider_picture(&at, &answer)
        {
            return true;
        }
        let mut claim = None;
        for plugin in self.plugin.iter().chain(self.builtins.iter()) {
            if !plugin.has("provider_open") {
                continue;
            }
            match plugin.provider_open(&path.to_string_lossy(), cols, rows) {
                Ok(Some(text)) => {
                    claim = Some(text);
                    break;
                }
                Ok(None) => {}
                Err(e) => {
                    self.plugin_note = Some(format!(
                        "plugin {}: {e} — opened the usual way",
                        plugin.path().display()
                    ));
                }
            }
        }
        if let Some(text) = claim {
            self.open_provider(path, text);
            return true;
        }
        false
    }

    /// A path the plugin claims: `provider_open`'s text is what opens, a folder
    /// keeping the listing keys.
    fn open_provider(&mut self, path: &Path, text: String) {
        let dir_mode = path.is_dir();
        self.install_view(path, &text, dir_mode);
        self.flash_with_note(&format!("{} (view)", path.display()));
        if !dir_mode {
            self.preview_answered(&text);
        }
    }

    /// A provider's answer that is markdown opens as prose: a virtual document
    /// need not exist, so its path may carry no extension.
    pub(crate) fn preview_answered(&mut self, text: &str) {
        let named = syntax::detect(self.bs().buf.name.as_deref(), None);
        let markdown = match named {
            Some(lang) => lang == syntax::Lang::Markdown,
            None => crate::diffview::looks_like_markdown(text),
        };
        if markdown && self.diff_view.is_none() {
            let name = self.buffer_name(self.cur);
            self.open_markdown_view(name, text.to_string());
        }
    }

    /// The manual (`M-x manual`, `C-g m`): rano's own docs as one document.
    pub fn open_manual(&mut self) {
        self.open_manual_from(&crate::builtin::scripts_dir());
    }

    /// Unpack and load every builtin, so one can claim a file never configured.
    pub fn load_builtins_from(&mut self, scripts: &Path) {
        for builtin in crate::builtin::ALL {
            let Ok(at) = crate::builtin::unpack(scripts, builtin) else {
                continue;
            };
            if self
                .plugin
                .iter()
                .chain(self.builtins.iter())
                .any(|p| p.path() == at)
            {
                continue;
            }
            let mut names: Vec<String> = builtin.caps.iter().map(|c| (*c).to_string()).collect();
            names.extend(self.config.capabilities.iter().cloned());
            let caps = crate::plugin::Capabilities::from_names(&names);
            match crate::plugin::Plugin::load_with(&at, caps) {
                Ok(p) => self.builtins.push(p),
                Err(e) => self.plugin_note = Some(format!("{}: {e}", at.display())),
            }
        }
    }

    /// [`Self::load_builtins_from`] in the reader's own scripts folder.
    pub fn load_builtins(&mut self) {
        self.load_builtins_from(&crate::builtin::scripts_dir());
    }

    /// [`Self::open_manual`] with the scripts folder given, so tests unpack their
    /// own.
    pub fn open_manual_from(&mut self, scripts: &Path) {
        let builtin = &crate::builtin::MANUAL;
        let at = match crate::builtin::unpack(scripts, builtin) {
            Ok(at) => at,
            Err(e) => {
                self.flash(&format!("Manual not unpacked: {e}"));
                return;
            }
        };
        if let Some(note) = crate::builtin::newer_note(scripts, builtin) {
            self.flash(&note);
        }
        let loaded = self
            .plugin
            .iter()
            .chain(self.builtins.iter())
            .any(|p| p.path() == at);
        if !loaded {
            // A builtin is granted what the config grants a hand-installed plugin.
            let caps = crate::plugin::Capabilities::from_names(&self.config.capabilities);
            match crate::plugin::Plugin::load_with(&at, caps) {
                Ok(p) => self.builtins.push(p),
                Err(e) => {
                    self.flash(&format!("Manual {} not loaded: {e}", at.display()));
                    return;
                }
            }
        }
        self.open_file("rano:manual");
        // Asked for again with the manual open: the rendered form is re-opened.
        if self.bs().buf.name.as_deref() == Some(Path::new("rano:manual")) {
            let text = self.bs().buf.text();
            self.preview_answered(&text);
        }
    }

    // ---------- a plugin's picture ----------

    /// Told the terminal's picture protocol, once; [`Self::images`] follows it,
    /// and what the terminal held is dropped: the old ids mean nothing now.
    pub fn set_pictures(&mut self, proto: Proto) {
        self.proto = proto;
        self.images = proto.speaks();
        self.sync_picture();
    }

    /// The protocol a picture is drawn with: the host's, else kitty when only
    /// [`Self::images`] was set, else [`Proto::None`].
    pub(crate) fn picture_proto(&self) -> Proto {
        if self.proto.speaks() {
            self.proto
        } else if self.images {
            Proto::Kitty
        } else {
            Proto::None
        }
    }

    /// A path a plugin answered with pixels, drawn as the read-only view it is;
    /// refused, not drawn, where the terminal cannot carry it.
    pub fn open_provider_picture(&mut self, path: &Path, answer: &crate::plugin::Picture) -> bool {
        let proto = self.picture_proto();
        let pic = match Picture::of_rgb(
            picture_id(path),
            answer.width,
            answer.height,
            &answer.rgb,
            self.text_w,
        ) {
            Ok(p) => p,
            Err(e) => {
                self.flash(&format!("{}: {e}", path.display()));
                return true;
            }
        };
        if pic.refusal(proto).is_some() {
            // Not the end of the path: the plugin may answer a text hook, and a
            // terminal without pictures is exactly who that is for.
            return false;
        }
        // The rows are the picture: the box's placeholders.
        let text = pic
            .lines(proto)
            .iter()
            .map(|l| l.plain())
            .collect::<Vec<String>>()
            .join("\n");
        self.close_picture();
        self.install_view(path, &text, false);
        self.picture_view = Some(PictureView {
            path: path.to_path_buf(),
            picture: pic,
        });
        self.sync_picture();
        self.flash_with_note(&format!("{} (picture)", path.display()));
        true
    }

    /// What the terminal must be told about the picture, over a set of its own
    /// so the two kinds of view cannot free each other's images.
    fn sync_picture(&mut self) {
        let mut out = self.pictures.set_proto(self.picture_proto());
        let want: Vec<Picture> = match &self.picture_view {
            Some(v) if self.picture_is_current(v) => vec![v.picture.clone()],
            _ => Vec::new(),
        };
        out.extend(self.pictures.sync(&want));
        self.graphics_out.extend(out);
    }

    /// Whether the picture view is the buffer on the glass.
    fn picture_is_current(&self, v: &PictureView) -> bool {
        self.bs().buf.name.as_deref() == Some(v.path.as_path())
    }

    /// The picture is off the glass: the terminal's copy goes, since nothing else
    /// can free it.
    fn close_picture(&mut self) {
        if self.picture_view.take().is_some() {
            self.sync_picture();
        }
    }

    /// A resize re-cuts the box and the placement; the pixels are the terminal's
    /// already.
    fn recut_picture(&mut self) {
        let proto = self.picture_proto();
        let Some(mut v) = self.picture_view.take() else {
            return;
        };
        let moved =
            self.bs().buf.name.as_deref() == Some(v.path.as_path()) && v.picture.refit(self.text_w);
        if moved {
            self.bs_mut().buf.set_rows(picture_rows(&v.picture, proto));
            let last = self.bs().buf.row_count().saturating_sub(1);
            let c = self.bs().cursor;
            self.bs_mut().cursor = Pos {
                row: c.row.min(last),
                col: c.col,
            };
            self.highlight_dirty = true;
            self.adjust_scroll(self.text_h);
            self.adjust_scroll_x();
        }
        self.picture_view = Some(v);
        if moved {
            self.sync_picture();
        }
    }

    /// The image id, when `row` is a picture view's placeholder row: hashed from
    /// the view's name, so the renderer needs no table of terminal state.
    fn picture_row_id(&self, row: usize) -> Option<u32> {
        let bs = self.bs();
        if !bs.synthetic || !bs.read_only {
            return None;
        }
        let name = bs.buf.name.as_deref()?;
        crate::term::graphics::is_placeholder_row(bs.buf.row_opt(row)?).then(|| picture_id(name))
    }

    /// M-g: ask for a folder, defaulting to this buffer's own.
    pub(crate) fn start_dir(&mut self) {
        let from = self
            .bs()
            .dir
            .clone()
            .or_else(|| {
                self.bs()
                    .buf
                    .name
                    .as_deref()
                    .and_then(Path::parent)
                    .map(|p| p.to_path_buf())
            })
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("."));
        let text = from.display().to_string();
        self.prompt = Some(Prompt {
            kind: PromptKind::DirName,
            cursor: text.chars().count(),
            text,
        });
    }

    /// The answer to the folder prompt: a folder opens as a listing, a file as a
    /// file.
    pub(crate) fn do_dir(&mut self, name: String) {
        let path = PathBuf::from(expand_tilde(name.trim()));
        if path.is_dir() {
            self.open_dir(&path);
        } else if path.is_file() {
            self.open_file(&path.display().to_string());
        } else {
            self.flash(&format!("No such folder: {}", path.display()));
        }
    }

    /// `RET` in a listing: enter what the cursor is on.
    pub(crate) fn dir_enter(&mut self) {
        let Some(dir) = self.bs().dir.clone() else {
            return;
        };
        let row = self.bs().cursor.row;
        let line: String = self
            .bs()
            .buf
            .row_opt(row)
            .map(|r| r.iter().collect())
            .unwrap_or_default();
        let Some(path) = crate::dirview::path_at(&dir, &line) else {
            self.flash("Nothing here to enter");
            return;
        };
        if path.is_dir() {
            self.open_dir(&path);
        } else {
            self.open_file(&path.display().to_string());
        }
    }

    /// `-` in a listing: the folder above.
    pub(crate) fn dir_parent(&mut self) {
        let Some(dir) = self.bs().dir.clone() else {
            return;
        };
        match dir.parent() {
            Some(p) if p != dir => {
                self.open_dir(p);
            }
            _ => self.flash("Already at the top"),
        }
    }

    /// `r` in a listing: read the folder again, keeping the cursor on the same
    /// entry.
    pub(crate) fn dir_refresh(&mut self) {
        let Some(dir) = self.bs().dir.clone() else {
            return;
        };
        let row = self.bs().cursor.row;
        let here: String = self
            .bs()
            .buf
            .row_opt(row)
            .map(|r| r.iter().collect())
            .unwrap_or_default();
        let name = here.strip_suffix('/').unwrap_or(&here).to_string();
        if !self.open_dir(&dir) {
            return;
        }
        if let Some(i) = crate::dirview::row_of(&dir, &name) {
            self.bs_mut().cursor = Pos { row: i, col: 0 };
            self.adjust_scroll(self.text_h);
        }
    }

    /// Load the plugin named by the config (or `RANO_PLUGIN`); one that cannot
    /// load is reported and left off — a hook is never a precondition.
    pub fn set_plugin(&mut self, path: &Path) {
        let caps = crate::plugin::Capabilities::from_names(&self.config.capabilities);
        match crate::plugin::Plugin::load_with(path, caps) {
            Ok(p) => {
                let said = p.summary();
                self.plugin = Some(p);
                self.flash(&format!("Plugin: {said}"));
            }
            Err(e) => {
                self.plugin = None;
                self.flash(&format!("Plugin {} not loaded: {e}", path.display()));
            }
        }
    }

    /// The plugin from inline bytes — the seam the tests use.
    pub fn set_plugin_bytes(&mut self, wasm: &[u8], from: PathBuf) {
        self.set_plugin_bytes_with(wasm, from, crate::plugin::Capabilities::default())
    }

    /// [`Self::set_plugin_bytes`] under granted capabilities.
    pub fn set_plugin_bytes_with(
        &mut self,
        wasm: &[u8],
        from: PathBuf,
        caps: crate::plugin::Capabilities,
    ) {
        match crate::plugin::Plugin::from_bytes_with(wasm, from.clone(), caps) {
            Ok(p) => {
                let said = p.summary();
                self.plugin = Some(p);
                self.flash(&format!("Plugin: {said}"));
            }
            Err(e) => {
                self.plugin = None;
                self.flash(&format!("Plugin {} not loaded: {e}", from.display()));
            }
        }
    }

    /// A file, read through the open hook — the one place a file's text enters
    /// rano. Skipped for a binary; a failing hook says so and leaves the text.
    pub(crate) fn read_file(&mut self, path: &Path) -> std::io::Result<Buffer> {
        let mut buf = Buffer::from_file(path)?;
        if buf.binary {
            return Ok(buf);
        }
        let Some(plugin) = self.plugin.as_ref().filter(|p| p.has("on_open")) else {
            return Ok(buf);
        };
        let text = buf.text();
        match plugin.open(&text) {
            Ok(Some(new)) => {
                if new != text {
                    buf.set_rows(new.lines().map(|l| l.chars().collect()).collect());
                    self.plugin_note = Some("plugin: transformed on open".to_string());
                }
            }
            Ok(None) => {}
            Err(e) => {
                self.plugin_note = Some(format!(
                    "plugin {}: {e} — opened unchanged",
                    plugin.path().display()
                ));
            }
        }
        Ok(buf)
    }

    /// The save hook, after `format_for_save` and before the encode: its answer
    /// replaces the buffer as one undo step, and a failing hook says so.
    fn run_save_hook(&mut self) {
        let Some(plugin) = self.plugin.as_ref().filter(|p| p.has("on_save")) else {
            return;
        };
        let before = self.bs().buf.text();
        match plugin.save(&before) {
            Ok(Some(new)) if new != before => {
                let mut rows: Vec<Vec<char>> = new.lines().map(|l| l.chars().collect()).collect();
                if rows.is_empty() {
                    rows.push(Vec::new());
                }
                let n = self.bs().buf.row_count();
                self.begin_action(ActionKind::Plugin, 0, n);
                self.bs_mut().buf.splice_rows(0, n, rows);
                let last = self.bs().buf.row_count().saturating_sub(1);
                let c = self.bs().cursor;
                self.bs_mut().cursor = Pos {
                    row: c.row.min(last),
                    col: c.col,
                };
                self.finish_step();
                self.edit_invalidate();
                self.plugin_note = Some("plugin: transformed on save".to_string());
            }
            Ok(_) => {}
            Err(e) => {
                self.plugin_note = Some(format!(
                    "plugin {}: {e} — saved as written",
                    plugin.path().display()
                ));
            }
        }
    }

    pub(crate) fn do_read(&mut self, name: String) {
        if self.refuse_read_only("reading a file in") {
            return;
        }
        if name.trim().is_empty() {
            return;
        }
        let read = self.read_file(Path::new(&expand_tilde(&name)));
        match read {
            Ok(mut read) => {
                // From `Buffer::from_file`, so `^R` gets the same encoding
                // detection as startup.
                let lines = read.take_rows();
                let count = lines.len();
                self.bs_mut().buf.crlf = read.crlf;
                self.bs_mut().buf.encoding = read.encoding;
                assert!(!lines.is_empty(), "from_file always yields one row");
                if self.bs().buf.row_count() == 1 && self.bs().buf.row(0).is_empty() {
                    self.begin_action(ActionKind::ReadFile, 0, 1);
                    self.bs_mut().buf.set_rows(lines);
                    self.bs_mut().cursor = Pos { row: 0, col: 0 };
                } else {
                    let c = self.bs().cursor;
                    // empty before-region: pure insertion at the cursor row
                    self.begin_action(ActionKind::ReadFile, c.row, c.row);
                    self.bs_mut().buf.insert_lines_at(c.row, lines);
                }
                self.finish_step();
                self.edit_invalidate();
                self.flash_with_note(&format!("Read {} lines", count));
            }
            Err(e) => {
                self.flash(&format!("Error: {}", e));
            }
        }
    }

    /// F8: open `path` — a new buffer with config.multibuffer, else in place.
    /// False when the read failed, and the caller keeps the prompt open.
    pub(crate) fn open_file(&mut self, path: &str) -> bool {
        let path = path.trim();
        if path.is_empty() {
            return true;
        }
        // A view was of the current buffer: opening a file gives that one up.
        self.close_view();
        self.close_picture();
        let path = PathBuf::from(expand_tilde(path));
        // Re-opening the current file single-buffer still re-reads it: that is
        // how a revert is done there.
        if let Some(i) = self.find_buffer(&path)
            && (self.config.multibuffer || i != self.cur)
        {
            self.set_current(i);
            return true;
        }
        // A provider hook asks first; the config's plugin before the builtins, so
        // none shadows it.
        if self.ask_providers(&path) {
            return true;
        }
        // A folder opens as a listing: typing a directory should show what is in it.
        if path.is_dir() {
            return self.open_dir(&path);
        }
        // A `.log` opens in log mode, a load that reads it on the next tick, so a
        // huge log is never read whole into a buffer.
        let log = self.log_mode_for(&path);
        let (buf, count) = if log {
            let mut b = Buffer::new();
            b.name = Some(path.clone());
            b.disk = crate::buffer::DiskStamp::of(&path);
            (b, 0)
        } else {
            match self.read_file(&path) {
                Ok(b) => {
                    let n = b.row_count();
                    (b, n)
                }
                Err(e) => {
                    self.flash(&format!("Error: {e}"));
                    return false;
                }
            }
        };
        // Without multibuffer the new file replaces the current buffer, but never
        // one with unsaved edits: that one stays and the file gets its own.
        let kept = !self.config.multibuffer && self.bs().buf.modified;
        // An untouched scratch is replaced either way: keeping it left a `[1/2]`
        // nobody asked for.
        let scratch = {
            let b = &self.bs().buf;
            b.name.is_none() && !b.modified && b.row_count() <= 1 && b.text().is_empty()
        };
        let mut state = BufferState::new(buf);
        if log {
            state.pending_load = Some(path.clone());
        }
        if (self.config.multibuffer && !scratch) || kept {
            self.buffers.push(state);
            self.cur = self.buffers.len() - 1;
        } else {
            self.buffers[self.cur] = state;
        }
        // A fresh highlighter has no window, which reads as "covers everything".
        self.highlight_dirty = true;
        self.lsp_sync();
        self.adjust_scroll(self.text_h);
        self.adjust_scroll_x();
        // A log says nothing: its rows have not been read, and the scan counts them.
        if !log {
            if kept {
                self.flash_with_note(&format!(
                    "Read {count} lines into a new buffer: the other has unsaved edits (M-< goes back)"
                ));
            } else {
                self.flash_with_note(&format!("Read {} lines", count));
            }
        }
        // A picture is its own reading: the text just loaded is a lossy decode.
        self.preview_if_picture();
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

    /// ^X: quit once every buffer has been dealt with. The first modified buffer
    /// at or after this one (wrapping) becomes current and gets the save prompt.
    pub(crate) fn try_quit(&mut self) {
        // ^C is the interrupt, and while a file is still arriving that is what it
        // should do: stop reading, keeping the rows already adopted.
        self.cancel_load();
        if let Some(i) = self.first_modified_from(self.cur) {
            if i != self.cur {
                self.cur = i;
                self.highlight_dirty = true;
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
                // Discard these changes, then move on to the next modified buffer.
                self.bs_mut().buf.modified = false;
                self.try_quit();
            }
            _ => {}
        }
    }

    // ---------- buffers ----------

    /// The open buffer showing `path`, canonicalised so all spellings agree.
    pub(crate) fn find_buffer(&self, path: &Path) -> Option<usize> {
        let want = canonical(path);
        self.buffers
            .iter()
            .position(|bs| bs.buf.name.as_deref().is_some_and(|n| canonical(n) == want))
    }

    /// Make buffer `i` current and say so.
    pub(crate) fn set_current(&mut self, i: usize) {
        if i >= self.buffers.len() {
            return;
        }
        self.cur = i;
        self.highlight_dirty = true;
        self.completion_close();
        self.adjust_scroll(self.text_h);
        self.adjust_scroll_x();
        let name = self.buffer_name(i);
        self.flash(&format!("Buffer: {name}"));
        // The view that was up belonged to the buffer that was current.
        self.close_view();
        // A picture the terminal holds is one this buffer draws: leaving frees it.
        self.sync_picture();
        self.preview_if_picture();
    }

    /// What the status line and the buffer list call buffer `i`.
    pub(crate) fn buffer_name(&self, i: usize) -> String {
        self.buffers[i]
            .buf
            .name
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "scratch".to_string())
    }

    /// M-> / M-<: switch to the next/previous buffer, wrapping.
    pub(crate) fn switch_buffer(&mut self, dir: isize) {
        let n = self.buffers.len();
        if n < 2 {
            return;
        }
        let i = (self.cur as isize + dir).rem_euclid(n as isize) as usize;
        self.set_current(i);
    }

    /// M-W: close the current buffer; a modified one asks first. The last one
    /// stays — ^X is how to leave.
    pub(crate) fn close_buffer(&mut self) {
        if self.buffers.len() < 2 {
            self.flash("Last buffer; ^X to exit");
            return;
        }
        if self.bs().buf.modified {
            self.prompt = Some(Prompt {
                kind: PromptKind::ConfirmClose,
                text: String::new(),
                cursor: 0,
            });
            return;
        }
        self.remove_current_buffer();
    }

    pub(crate) fn answer_close(&mut self, c: char) {
        match c {
            'y' | 'Y' => {
                self.close_after_save = true;
                match self.bs().buf.name.clone() {
                    Some(p) => self.save_to(p),
                    None => {
                        self.prompt = Some(Prompt {
                            kind: PromptKind::WriteName,
                            text: String::new(),
                            cursor: 0,
                        });
                    }
                }
            }
            'n' | 'N' => self.remove_current_buffer(),
            _ => {}
        }
    }

    /// Drop the current buffer and fix up everything that indexes `buffers`: the
    /// M-, stack loses entries into it and later indices shift down.
    pub(crate) fn remove_current_buffer(&mut self) {
        // The view belonged to this buffer: it goes with it.
        self.close_view();
        self.close_picture();
        if self.buffers.len() < 2 {
            return;
        }
        let k = self.cur;
        let name = self.buffer_name(k);
        self.cancel_load();
        self.completion_close();
        let gone = self.buffers.remove(k);
        drop(gone);
        self.def_back.retain(|e| e.idx != Some(k));
        for e in &mut self.def_back {
            if let Some(i) = e.idx.as_mut()
                && *i > k
            {
                *i -= 1;
            }
        }
        // The buffer that followed takes its place (nano's order).
        self.cur = k.min(self.buffers.len() - 1);
        self.highlight_dirty = true;
        self.adjust_scroll(self.text_h);
        self.adjust_scroll_x();
        self.flash(&format!("Closed {name}"));
    }

    // ---------- justify / sort ----------

    pub(crate) fn justify(&mut self) {
        if self.refuse_read_only("justify") {
            return;
        }
        let width = self.text_w.saturating_sub(2).max(20);
        let c = self.bs().cursor.row;
        let mut top = c;
        while top > 0 && !self.bs().buf.row(top - 1).is_empty() {
            top -= 1;
        }
        let mut bot = c;
        while bot + 1 < self.bs().buf.row_count() && !self.bs().buf.row(bot + 1).is_empty() {
            bot += 1;
        }
        let mut words: Vec<String> = Vec::new();
        for r in top..=bot {
            let s: String = self.bs().buf.row(r).iter().collect();
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
            .splice_rows(top, bot + 1, new_lines.iter().cloned());
        self.bs_mut().cursor = Pos { row: top, col: 0 };
        self.bs_mut().mark = None;
        self.finish_step();
        self.edit_invalidate();
    }

    pub(crate) fn sort_lines(&mut self) {
        if self.refuse_read_only("sort") {
            return;
        }
        let (top, bot) = match self.bs().mark {
            Some(mark) => {
                let (a, b) = normalize(mark, self.bs().cursor);
                (a.row, b.row)
            }
            None => (0, self.bs().buf.row_count().saturating_sub(1)),
        };
        self.begin_action(ActionKind::Sort, top, bot + 1);
        self.bs_mut().buf.sort_rows_by(top, bot + 1, |a, b| {
            let key = |l: &Vec<char>| l.iter().collect::<String>().to_lowercase();
            key(a).cmp(&key(b))
        });
        self.bs_mut().mark = None;
        self.finish_step();
        self.edit_invalidate();
    }

    /// Style for the character at `p`; `diags` is this row's slice, since
    /// `all_diags` must not run per character.
    pub fn char_style_with(&self, p: Pos, diags: &[lsp::Diagnostic]) -> Style {
        // A picture view's cells are placeholders: the terminal reads the image
        // from the cell's colours, so this comes before any selection styling.
        if let Some(id) = self.picture_row_id(p.row) {
            return crate::term::graphics::placeholder_style(id);
        }
        // Current match: black on yellow (nano's default). Others are not highlighted.
        if let Some((a, b)) = self.current_match_range()
            && a <= p
            && p < b
        {
            return Style::of(Role::Match);
        }
        if let Some(mark) = self.bs().mark {
            let (a, b) = normalize(mark, self.bs().cursor);
            if a <= p && p < b {
                // Explicit colours, not reverse: reverse comes out white-on-white
                // on some terminals.
                return Style::of(Role::Selection);
            }
        }
        // Diagnostics: underline in severity colour. LSP cols are UTF-16 units
        // and can drift on astral chars.
        for d in diags {
            if d.line == p.row && d.col <= p.col && p.col < d.end_col {
                return Style::of(diag_role(d.severity)).underline();
            }
        }
        // Syntax highlighting (tree-sitter), below search/selection priority.
        if let Some(s) = self.bs().hl.style_at(p) {
            return s;
        }
        Style::new()
    }
}

/// The role a diagnostic's severity paints with (LSP: 1 error, 2 warning, else
/// lesser).
pub(crate) fn diag_role(severity: u64) -> Role {
    match severity {
        1 => Role::DiagError,
        2 => Role::DiagWarning,
        _ => Role::DiagNote,
    }
}

pub(crate) fn normalize(a: Pos, b: Pos) -> (Pos, Pos) {
    if a <= b { (a, b) } else { (b, a) }
}

pub(crate) fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

/// `path` made absolute and symlink-free when it exists, so a not-yet-saved
/// buffer still compares equal to itself.
pub(crate) fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| {
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            std::env::current_dir()
                .map(|d| d.join(path))
                .unwrap_or_else(|_| path.to_path_buf())
        }
    })
}

pub(crate) fn refresh_syntax_diags(bs: &mut BufferState) {
    bs.syntax_diags = bs
        .hl
        .syntax_errors(&bs.buf.rows_vec())
        .into_iter()
        .map(|(line, col, end_col, message)| lsp::Diagnostic {
            line,
            col,
            end_col,
            // A merge conflict is work to finish, not broken code: a warning.
            severity: if message.starts_with("merge conflict") {
                2
            } else {
                1
            },
            message,
        })
        .collect();
    widen_zero_width(&mut bs.syntax_diags, &bs.buf.rows_vec());
}

/// Zero-width diagnostics (rust-analyzer reports syntax errors as insertion
/// points) can never be underlined; widen each to one visible column.
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

/// A save must never hang on a subprocess, so a formatter gets slack, not a
/// budget.
const FORMAT_TIMEOUT: Duration = Duration::from_secs(5);

impl Editor {
    pub fn status_text(&self) -> Option<String> {
        // A load in flight is the most important thing to say: without it the
        // screen reads as frozen.
        if let Some(s) = self.loading_text() {
            return Some(s);
        }
        if let Some(f) = &self.status
            && f.until > Instant::now()
        {
            return Some(f.text.clone());
        }
        // After the flash: it is a number arriving late, not a state of the screen.
        if let Some(s) = self.counting_text() {
            return Some(s);
        }
        if let Some(job) = &self.bs().exec_job {
            return Some(format!("Running: {}", job.cmd));
        }
        if let Some(s) = self.lsp_status() {
            return Some(s);
        }
        if let Some(t) = self.loc_until
            && t > Instant::now()
        {
            return Some(format!(
                "Line {}, Col {}",
                self.bs().cursor.row + 1,
                self.bs().cursor.col + 1
            ));
        }
        None
    }

    /// Expire an overdue status flash or cursor-position display; true when
    /// anything visible was cleared.
    pub(crate) fn tick_status(&mut self) -> bool {
        let mut dirty = false;
        if let Some(f) = &self.status
            && f.until <= Instant::now()
        {
            self.status = None;
            dirty = true;
        }
        if self.loc_until.is_some_and(|t| t <= Instant::now()) {
            self.loc_until = None;
            dirty = true;
        }
        dirty
    }

    /// One buffer per extra command-line file, named now and read when it first
    /// becomes current.
    #[cfg(test)]
    pub(crate) fn add_deferred_buffers(&mut self, paths: &[PathBuf]) {
        let at: Vec<(PathBuf, Option<Pos>)> = paths.iter().map(|p| (p.clone(), None)).collect();
        self.add_deferred_buffers_at(&at);
    }

    /// [`Self::add_deferred_buffers`], each with the position its cursor goes to
    /// once it is read.
    pub fn add_deferred_buffers_at(&mut self, files: &[(PathBuf, Option<Pos>)]) {
        for (p, at) in files {
            if self.find_buffer(p).is_some() {
                continue;
            }
            let mut buf = Buffer::new();
            buf.name = Some(p.clone());
            let mut bs = BufferState::new(buf);
            if p.exists() {
                bs.pending_load = Some(p.clone());
            }
            bs.goto = *at;
            self.buffers.push(bs);
        }
    }

    // ---------- styling ----------

    /// The file name for the title bar, `[i/n] ` when several buffers are open.
    pub(crate) fn title_text(&self) -> String {
        let name = self
            .bs()
            .buf
            .name
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        if self.buffers.len() > 1 {
            format!("[{}/{}] {}", self.cur + 1, self.buffers.len(), name)
        } else {
            name
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view_ed(text: &str) -> Editor {
        let mut buf = Buffer::new();
        buf.set_rows(text.lines().map(|l| l.chars().collect()).collect());
        if buf.rows_is_empty() {
            buf.push_row(Vec::new());
        }
        let mut ed = Editor::new(buf, config::Config::default());
        ed.text_w = 120;
        ed.text_h = 20;
        ed.images = true;
        ed
    }

    /// A plugin's picture answer: the pixels and the size they call themselves.
    fn plugin_pic(w: u32, h: u32) -> crate::plugin::Picture {
        crate::plugin::Picture {
            width: w,
            height: h,
            rgb: vec![0x20u8; (w * h * 3) as usize],
        }
    }

    fn graphics(ed: &mut Editor) -> Vec<String> {
        ed.take_graphics()
            .iter()
            .map(|b| String::from_utf8_lossy(b).into_owned())
            .collect()
    }

    /// A plugin's picture is a view, the same shape a converted PNG's view has.
    #[test]
    fn a_provider_picture_is_the_rows_the_terminal_fills_and_the_bytes_that_fill_them() {
        let mut ed = view_ed("the buffer this picture replaced");
        let path = PathBuf::from("/tmp/part.stl");
        let (w, h) = (64u32, 48u32);
        assert!(ed.open_provider_picture(&path, &plugin_pic(w, h)));

        // A view, not a document: read-only like a tail, and named after the path it claims.
        assert!(ed.bs().read_only && ed.bs().synthetic);
        assert_eq!(ed.bs().buf.name.as_deref(), Some(path.as_path()));
        let cells = crate::term::graphics::image_cells(
            Some(w),
            Some(h),
            crate::term::graphics::image_box(120),
        );
        let rows: Vec<Vec<char>> = ed.bs().buf.rows_vec();
        assert_eq!(rows.len(), cells.1 as usize, "one row per row of the box");
        assert!(
            crate::term::graphics::is_placeholder_row(&rows[0]),
            "{:?}",
            rows[0]
        );
        assert_eq!(
            crate::width::width(&rows[0], ed.tab_width),
            cells.0 as usize
        );

        // The cell's colour names the image: the id the upload went out under.
        let id = crate::term::graphics::image_id(&format!("plugin:{}", path.display()));
        let style = ed.char_style_with(Pos { row: 0, col: 0 }, &[]);
        assert_eq!(style, crate::term::graphics::placeholder_style(id));
        // A selection over a placeholder does not repaint it: a cell the terminal
        // no longer knows is a hole in the picture.
        ed.bs_mut().mark = Some(Pos { row: 0, col: 0 });
        ed.bs_mut().cursor = Pos { row: 0, col: 3 };
        assert_eq!(
            ed.char_style_with(Pos { row: 0, col: 1 }, &[]),
            crate::term::graphics::placeholder_style(id)
        );
        ed.bs_mut().mark = None;

        // Through a frame, the row is drawn in that colour: plain cells are a
        // picture not drawn.
        let mut frame = crate::render::Buffer::empty(crate::render::Rect::new(0, 0, 120, 24));
        crate::ui::draw(&mut frame, &ed);
        let colour = format!(
            "38;2;{};{};{};58;5;1m",
            (id >> 16) & 0xff,
            (id >> 8) & 0xff,
            id & 0xff
        );
        let emitted = frame.emit(crate::render::Palette::Colour);
        assert!(
            emitted[1].contains(&colour),
            "the first text row: {:?}",
            emitted[1]
        );
        assert!(emitted[1].contains('\u{10EEEE}'), "{:?}", emitted[1]);

        // The terminal is told: the raw-RGB upload, then the placement the rows name.
        let out = graphics(&mut ed);
        assert_eq!(out.len(), 2, "{out:?}");
        assert!(
            out[0].starts_with(&format!("\x1b_Ga=t,f=24,i={id},s=64,v=48,q=2,m=")),
            "{:?}",
            &out[0][..40.min(out[0].len())]
        );
        assert!(out[1].contains("a=p,U=1") && out[1].contains(&format!("c={},", cells.0)));
        // Nothing changed since, so nothing is sent again.
        assert!(graphics(&mut ed).is_empty());
    }

    /// A terminal without a picture protocol draws nothing, and false lets the
    /// caller try the text hook.
    #[test]
    fn a_terminal_without_pictures_declines_a_provider_picture_instead_of_drawing_it() {
        let mut ed = view_ed("still here");
        ed.images = false;
        let path = PathBuf::from("/tmp/part.stl");
        assert!(
            !ed.open_provider_picture(&path, &plugin_pic(8, 8)),
            "a refused picture must let the text answer be tried"
        );
        // Nothing was opened, nothing was written, and the buffer is the one that was there.
        assert_eq!(
            ed.bs().buf.text(),
            "still here\n",
            "the text that was there"
        );
        assert!(!ed.bs().read_only);
        assert!(graphics(&mut ed).is_empty());
        // The same answer from a host that named the protocol rather than only
        // `images`.
        let mut ed = view_ed("still here");
        ed.set_pictures(Proto::None);
        assert!(!ed.open_provider_picture(&path, &plugin_pic(8, 8)));
        assert!(
            ed.status.is_none(),
            "declining says nothing: the text answer, or the ordinary open, speaks for itself"
        );
        assert_eq!(
            ed.bs().buf.text(),
            "still here\n",
            "the text that was there"
        );
        assert!(graphics(&mut ed).is_empty());
    }

    /// A payload whose size and pixels disagree is refused by name.
    #[test]
    fn a_payload_that_disagrees_with_its_pixels_is_refused_and_nothing_is_drawn() {
        let mut ed = view_ed("still here");
        let path = PathBuf::from("/tmp/part.stl");
        let mut short = plugin_pic(16, 16);
        short.rgb.pop();
        assert!(ed.open_provider_picture(&path, &short));
        let said = &ed.status.as_ref().expect("one line saying why").text;
        assert!(said.contains("16×16") && said.contains("767"), "{said}");
        assert_eq!(
            ed.bs().buf.text(),
            "still here\n",
            "the text that was there"
        );
        assert!(ed.picture_view.is_none());
        assert!(graphics(&mut ed).is_empty());
    }

    /// A resize re-cuts the box and the terminal gets the placement alone: it
    /// has the pixels already.
    #[test]
    fn a_resize_re_cuts_a_providers_picture_and_sends_only_the_placement() {
        let mut ed = view_ed("still here");
        let path = PathBuf::from("/tmp/part.stl");
        assert!(ed.open_provider_picture(&path, &plugin_pic(64, 48)));
        let _ = graphics(&mut ed);
        let before = ed.bs().buf.row(0).len();
        assert!(ed.set_area(Area::new(0, 0, 40, 24)));
        let out = graphics(&mut ed);
        assert_eq!(out.len(), 1, "the placement only: {out:?}");
        assert!(
            out[0].contains("a=p,U=1") && out[0].contains("c=20,"),
            "{out:?}"
        );
        assert_ne!(ed.bs().buf.row(0).len(), before, "the rows were re-cut");
        assert!(graphics(&mut ed).is_empty(), "and then quiet again");
        // Leaving the view frees the terminal's copy, with its data.
        let mut ed = view_ed("still here");
        assert!(ed.open_provider_picture(&path, &plugin_pic(64, 48)));
        let _ = graphics(&mut ed);
        ed.close_picture();
        let out = graphics(&mut ed);
        assert_eq!(out.len(), 1, "{out:?}");
        assert!(out[0].contains("a=d,d=I"), "{out:?}");
    }
}
