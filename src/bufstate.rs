//! One open document's state ([`BufferState`]) and the wrap geometry it
//! caches ([`RowWrap`]).
//!
//! These lived at the root of the binary's `main.rs`; they moved into the
//! library with the editor, and stay re-exported at the crate root because
//! the controllers name them `crate::BufferState` / `crate::RowWrap`.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Instant;

use crate::buffer::{Buffer, Pos};
use crate::editor::{ActionKind, UndoStep};
use crate::exec;
use crate::lsp;
use crate::search_ctrl::SearchState;
use crate::syntax;

/// Where a buffer row's soft-wrap segments begin, for a row that is not
/// *simple* (see [`crate::width`]): `(char index, display column)` per
/// segment. The renderer, the cursor and the mouse all read this instead of
/// recomputing `seg * view_w`, which is only true while every character is
/// one column wide.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct RowWrap {
    /// `None` → a simple row: display col == char index, segments affine.
    pub(crate) segs: Option<Vec<(usize, usize)>>,
    /// Rendered width of the row in display columns.
    pub(crate) w: usize,
}

/// Everything that belongs to ONE open document: text, view state, undo
/// history and its LSP session. F8 will hold several of these in
/// `Editor::buffers`; today there is exactly one.
pub struct BufferState {
    pub buf: Buffer,
    pub hl: syntax::Highlighter,
    pub lsp: Option<lsp::LspClient>,
    pub lsp_diags: Vec<lsp::Diagnostic>,
    /// Syntax errors from the tree-sitter parse (recomputed per edit), so
    /// deliberate mistakes are visible even without a language server.
    pub syntax_diags: Vec<lsp::Diagnostic>,
    /// TODO.md markers the format cannot express — an unknown state such as
    /// `[/]` — in the same shape as `syntax_diags`, so the gutter, the
    /// severity colours and M-D all work on them unchanged. Only for a buffer
    /// named `TODO.md`: `[-]` in somebody else's markdown is theirs to write.
    pub todo_diags: Vec<lsp::Diagnostic>,
    /// didChange pending since the last flush (D4 debounce).
    pub lsp_dirty: bool,
    pub lsp_last_send: Instant,
    /// Handshake in flight: the tag is the `buf.name` display string the
    /// spawn was started FOR; a mismatch on adoption means it went stale.
    pub lsp_starting: Option<(String, mpsc::Receiver<Result<lsp::LspClient, String>>)>,
    pub cursor: Pos,
    pub scroll: usize,
    pub mark: Option<Pos>,
    pub search: SearchState,
    /// Matches of the running search (Matcher::find_all output: pos + length
    /// in chars, row-major). Consumed by the match highlight; None once an
    /// edit invalidates it.
    pub search_matches: Option<Vec<(Pos, usize)>>,
    /// Single active async ^T job (D7): spawn, poll, insert stdout.
    pub exec_job: Option<exec::ExecJob>,
    /// **A view, not a document.** Every way to change this buffer is refused
    /// with a line saying so, and so is the save — a tailed file is one that
    /// something else is writing, so an edit that cannot be saved is work the
    /// user loses, and the loss is silent until they try (TODO.md §20.6).
    ///
    /// Nav, search and everything that only moves still work: that is the
    /// point of opening a file you will not edit.
    pub read_only: bool,
    /// Horizontal scroll of the text window, in DISPLAY cols (E3/F2).
    /// Always 0 while soft wrap is on (lines wrap instead of scrolling).
    pub scroll_x: usize,
    /// Soft-wrap visual-row table (M-\): `wrap_prefix[r]` is the visual row
    /// where buffer row `r` begins (len = lines.len() + 1, strictly
    /// increasing — every row occupies at least one visual row). Valid only
    /// while `wrap_key` matches the buffer's (edit_gen, view_w).
    pub wrap_prefix: Vec<usize>,
    /// Per-row wrap geometry, built in the same pass as `wrap_prefix` (and
    /// with the same freshness key). `None` segments means the row is
    /// *simple* — every character one column and no tab — so display col ==
    /// char index and its segments are affine: segment `s` is chars
    /// `[s*vw, (s+1)*vw)`. A row with a tab, a wide character (CJK, emoji) or
    /// a combining mark stores where each segment begins, as
    /// `(char index, display col)`, because neither relation is affine any
    /// more. See [`crate::width`].
    pub(crate) wrap_rows: Vec<RowWrap>,
    pub(crate) wrap_key: (u64, usize),
    /// Row count `wrap_rows` was built for. A change here means rows were
    /// inserted or removed, which shifts every entry below — the wrap table's
    /// cheap update path requires this to be unchanged.
    pub(crate) wrap_lines: usize,
    /// The one row whose content changed since the table was built, when an
    /// edit can name it. `None` means "unknown", and the table rebuilds in
    /// full — which is correct for every multi-row edit (sort, replace-all,
    /// a paste of many lines) and costs O(document), so the hot paths (typing)
    /// name their row and pay O(row) instead.
    pub(crate) wrap_dirty_row: Option<usize>,
    /// An append changed the buffer: the first `k` rows are untouched and the
    /// table can be EXTENDED from `k` instead of rebuilt. Set by the loader,
    /// which only ever appends; `None` means "unknown, rebuild". See
    /// `Editor::ensure_wrap_prefix`.
    pub(crate) wrap_extend_from: Option<usize>,
    /// Bumped on every edit; part of the wrap_prefix freshness key.
    pub(crate) edit_gen: u64,
    /// A file still arriving from disk; `None` once it has (or a normal,
    /// fully-loaded buffer). See `load_ctrl.rs`.
    pub(crate) load: Option<crate::loader::LoadJob>,
    /// A file named on the command line but not read yet: it starts loading
    /// the first time this buffer becomes current, so `rano *.rs` costs one
    /// read and one language server up front rather than one per file.
    pub(crate) pending_load: Option<PathBuf>,
    /// Where to put the cursor once this buffer has that row, and centre it:
    /// `--line`/`--column`, `file:line:col`, or [`Editor::open_at`].
    ///
    /// Applied late on purpose, because three things have to be true first: the
    /// terminal's size (to know what "centre" means), the wrap table (a visual
    /// row is not a buffer row when wrap is on), and — with the async loader —
    /// the arrival of the row itself. A huge file delivers rows in batches, so a
    /// position a million rows in is applied when it lands rather than clamped
    /// to whatever had arrived. Per buffer, so a position waiting for its rows
    /// cannot land in another buffer that became current meanwhile.
    pub goto: Option<Pos>,
    /// **The change a host opened this buffer to review** ([`Editor::open_review`]), kept
    /// so `M-P` can show it again after the reader has gone back to the text. A snapshot
    /// of the change as the host saw it made, not a diff against the buffer now.
    ///
    /// [`Editor::open_review`]: crate::editor::Editor::open_review
    pub review: Option<crate::review::Change>,
    pub(crate) undo: VecDeque<UndoStep>,
    pub(crate) redo: VecDeque<UndoStep>,
    pub(crate) pending: Option<UndoStep>,
    pub(crate) last_kind: Option<ActionKind>,
}

impl BufferState {
    pub(crate) fn new(buf: Buffer) -> Self {
        let mut buf = buf;
        if buf.rows_is_empty() {
            buf.push_row(Vec::new());
        }
        let bs = Self {
            buf,
            hl: syntax::Highlighter::new(),
            lsp: None,
            lsp_diags: Vec::new(),
            syntax_diags: Vec::new(),
            todo_diags: Vec::new(),
            lsp_dirty: false,
            lsp_last_send: Instant::now(),
            lsp_starting: None,
            cursor: Pos { row: 0, col: 0 },
            scroll: 0,
            mark: None,
            search: SearchState {
                query: String::new(),
                current: 0,
                backwards: false,
            },
            search_matches: None,
            exec_job: None,
            read_only: false,
            scroll_x: 0,
            wrap_prefix: Vec::new(),
            wrap_rows: Vec::new(),
            wrap_key: (0, 0),
            wrap_lines: 0,
            wrap_dirty_row: None,
            wrap_extend_from: None,
            edit_gen: 0,
            load: None,
            pending_load: None,
            goto: None,
            review: None,
            undo: VecDeque::new(),
            redo: VecDeque::new(),
            pending: None,
            last_kind: None,
        };
        // No highlight here. On a 200 MB file a refresh now would be the whole
        // document parsed before the first frame; the run loop highlights the
        // viewport instead, and it is the same call the editor makes on every
        // keystroke. See `Editor::ensure_highlight`.
        bs
    }
}
