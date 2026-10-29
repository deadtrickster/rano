//! One open document's state ([`BufferState`]) and the wrap geometry it
//! caches ([`RowWrap`]).

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

/// Where a buffer row's soft-wrap segments begin, for a row that is not simple (see
/// [`crate::width`]): `(char index, display column)` per segment. The renderer, the
/// cursor and the mouse read this instead of recomputing `seg * view_w`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct RowWrap {
    /// `None` → a simple row: display col == char index, segments affine.
    pub(crate) segs: Option<Vec<(usize, usize)>>,
    /// Rendered width of the row in display columns.
    pub(crate) w: usize,
}

/// Everything that belongs to ONE open document: text, view state, undo
/// history and its LSP session.
pub struct BufferState {
    pub buf: Buffer,
    pub hl: syntax::Highlighter,
    pub lsp: Option<lsp::LspClient>,
    pub lsp_diags: Vec<lsp::Diagnostic>,
    /// Syntax errors from the tree-sitter parse (recomputed per edit), so
    /// deliberate mistakes are visible even without a language server.
    pub syntax_diags: Vec<lsp::Diagnostic>,
    /// TODO.md markers the format cannot express — an unknown state such as
    /// `[/]` — in the same shape as `syntax_diags`, so the gutter, the severity
    /// colours and M-D work on them unchanged. Only for a buffer named `TODO.md`.
    pub todo_diags: Vec<lsp::Diagnostic>,
    /// didChange pending since the last flush.
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
    /// Single active async ^T job: spawn, poll, insert stdout.
    pub exec_job: Option<exec::ExecJob>,
    /// A view, not a document: every way to change this buffer is refused with a
    /// line saying so, and so is the save, because an edit that cannot be saved
    /// is work the user loses. Nav and search still work.
    pub read_only: bool,
    /// The rows are a rendering, not a file's lines: a folder listing, a
    /// provider's view, a picture. No gutter, no language server, and the status
    /// line says a row rather than a line; a send carries the view's target.
    pub synthetic: bool,
    /// This buffer is a folder listing (the dired), and this is the directory it
    /// shows. `RET` enters what the cursor is on, `-` goes up, `r` reads it again.
    pub dir: Option<PathBuf>,
    /// This buffer is a tail of a file (`-f`/`--follow`), and this is the size at
    /// open: the line between a row that can be numbered, styled and searched
    /// like a file's and one that is still arriving. `None` is an ordinary file.
    pub(crate) tail: Option<crate::loader::Tail>,
    /// How many complete rows the file has BEFORE this buffer's first row: `Some(0)`
    /// for an ordinary document, `Some(n)` once a tail's scan has answered. `None`
    /// while it runs, because `Ln 1` for a screenful from a huge log is a guess.
    pub(crate) lines_before: Option<u64>,
    /// The scan that will fill [`Self::lines_before`] in, while it runs.
    pub(crate) line_count: Option<crate::linecount::LineCountJob>,
    /// Horizontal scroll of the text window, in DISPLAY cols.
    /// Always 0 while soft wrap is on (lines wrap instead of scrolling).
    pub scroll_x: usize,
    /// Soft-wrap visual-row table (M-\): `wrap_prefix[r]` is the visual row
    /// where buffer row `r` begins (len = lines.len() + 1, strictly increasing).
    /// Valid only while `wrap_key` matches the buffer's (edit_gen, view_w).
    pub wrap_prefix: Vec<usize>,
    /// Per-row wrap geometry, built in the same pass and with the same freshness
    /// key as `wrap_prefix`; see [`RowWrap`] for what a simple row is.
    pub(crate) wrap_rows: Vec<RowWrap>,
    pub(crate) wrap_key: (u64, usize),
    /// Row count `wrap_rows` was built for: a change means rows were inserted or
    /// removed, which shifts every entry below and forces a full rebuild.
    pub(crate) wrap_lines: usize,
    /// The one row whose content changed since the table was built, when an edit
    /// can name it. `None` rebuilds in full, which is correct for every
    /// multi-row edit; the hot paths name their row and pay O(row) instead.
    pub(crate) wrap_dirty_row: Option<usize>,
    /// An append changed the buffer: the first `k` rows are untouched, so the
    /// table can be EXTENDED from `k` rather than rebuilt. Set by the loader,
    /// which only ever appends; `None` means unknown, rebuild.
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
    /// `--line`/`--column`, `file:line:col`, or [`Editor::open_at`]. Applied late,
    /// because the size, the wrap table and, with the loader, the row must be there first.
    pub goto: Option<Pos>,
    /// The change a host opened this buffer to review
    /// ([`crate::editor::Editor::open_review`]), kept so `M-P` can show it again: a
    /// snapshot of the change as the host saw it made, not a diff against the buffer.
    pub review: Option<crate::review::Change>,
    pub(crate) undo: VecDeque<UndoStep>,
    pub(crate) redo: VecDeque<UndoStep>,
    pub(crate) pending: Option<UndoStep>,
    pub(crate) last_kind: Option<ActionKind>,
}

impl BufferState {
    /// The number row `row` has in the file, 1-based — or `None` when nobody has
    /// counted the file's lines yet. The one place a line number is derived, so a
    /// gutter and a status line agree about a tail.
    pub fn line_number(&self, row: usize) -> Option<u64> {
        self.lines_before.map(|before| before + row as u64 + 1)
    }

    /// The largest line number this buffer could show, for the gutter's width: the
    /// file's true last line when the count is known, the buffer's own row count
    /// when it is not. Erring narrow is deliberate; a wide gutter never gives back.
    pub fn line_span(&self) -> usize {
        let rows = self.buf.row_count();
        match self.lines_before {
            Some(before) => rows.saturating_add(usize::try_from(before).unwrap_or(usize::MAX)),
            None => rows,
        }
    }

    /// The last buffer row that may be STYLED: rows past it are ones the file was
    /// still writing when the tail was read. `usize::MAX` for an ordinary buffer and
    /// for a tail whose initial read has not finished, when the rows in hand are settled.
    pub fn settled_last_row(&self) -> usize {
        match self.tail.and_then(|t| t.rows_at_open) {
            Some(rows) => rows.saturating_sub(1),
            None => usize::MAX,
        }
    }

    /// Whether the styling boundary is known — i.e. this is a tail and its
    /// initial read has finished. A settled tail does not re-highlight when the
    /// file grows: nothing it may colour has changed.
    pub fn tail_settled(&self) -> bool {
        self.tail.and_then(|t| t.rows_at_open).is_some()
    }

    pub(crate) fn new(buf: Buffer) -> Self {
        let mut buf = buf;
        if buf.rows_is_empty() {
            buf.push_row(Vec::new());
        }
        // Read before `buf` moves into the state below.
        let binary = buf.binary;
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
            // A binary is a view, the rule a tailed file follows: there is nothing
            // here rano can write back without corrupting it, so refusing at the
            // keystroke is more honest than refusing at the save.
            read_only: binary,
            synthetic: false,
            dir: None,
            tail: None,
            lines_before: Some(0),
            line_count: None,
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
        // No highlight here: on a 200 MB file that would parse the whole document
        // before the first frame. The run loop highlights the viewport instead.
        bs
    }
}
