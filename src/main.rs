mod bindings;
mod buffer;
mod config;
mod diffview;
mod editor;
mod encoding;
mod exec;
mod exec_ctrl;
mod export;
mod keys;
mod load_ctrl;
mod loader;
mod lsp;
mod lsp_ctrl;
mod picker;
mod prompt;
// The chunked row store (§16.3). Declared here now because the binary's
// `Buffer` is built on it — the library-only note in `lib.rs` said this would
// happen when the migration landed.
//
// `allow(dead_code)` because the binary uses only the first half of this file.
// `Rows` is stage A and has a caller; `RowStore` is stage B — the lazy decode,
// which saves 835 MB on a 184 MB file and is the half that can silently blank a
// row, so it waits for an API that cannot lie about a missing row. The library
// exports both (`pub mod rows`), where nothing here is dead.
#[allow(dead_code)]
mod rows;
mod search;
mod search_ctrl;
mod syntax;
mod todo_ctrl;
mod ui;
mod update;
mod update_ctrl;
mod width;

#[cfg(test)]
mod bench;

use std::collections::VecDeque;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use buffer::Buffer;
use buffer::Pos;
use crossterm::event::{
    self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    Event, KeyEventKind,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use editor::Editor;
use editor::{ActionKind, UndoStep};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use search_ctrl::SearchState;

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
    pub(crate) undo: VecDeque<UndoStep>,
    redo: VecDeque<UndoStep>,
    pending: Option<UndoStep>,
    last_kind: Option<ActionKind>,
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

impl Editor {
    pub fn status_text(&self) -> Option<String> {
        // A load in flight is the most important thing to say: without it the
        // screen is a frame with nothing in it, which reads as frozen.
        if let Some(s) = self.loading_text() {
            return Some(s);
        }
        if let Some(f) = &self.status
            && f.until > Instant::now()
        {
            return Some(f.text.clone());
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

    /// Expire an overdue status flash / cursor-position display. Returns
    /// whether anything visible was cleared (dirty-draw, D6).
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

    /// One buffer per extra command-line file, after the first. Each is
    /// named at once (title, buffer list, language) but read only when it
    /// first becomes current — see `start_pending_load`. A file named twice
    /// gets one buffer; a file that does not exist yet is a new buffer that
    /// will be saved under that name, as for the first file.
    pub(crate) fn add_deferred_buffers(&mut self, paths: &[PathBuf]) {
        for p in paths {
            if self.find_buffer(p).is_some() {
                continue;
            }
            let mut buf = Buffer::new();
            buf.name = Some(p.clone());
            let mut bs = BufferState::new(buf);
            if p.exists() {
                bs.pending_load = Some(p.clone());
            }
            self.buffers.push(bs);
        }
    }

    // ---------- styling ----------

    /// The file name for the title bar, prefixed "[i/n] " when several
    /// buffers are open. Scratch buffers show an empty name.
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

/// The command line, parsed.
///
/// Hand-rolled rather than a parser crate: files, a handful of flags, and no
/// subcommands. What it must get right is that a flag's VALUE is
/// not mistaken for the file — `rano --line 42 main.rs` — which the positional
/// `args.first()` it replaced did get wrong.
#[derive(Debug, Default, PartialEq, Eq)]
struct Args {
    /// In command-line order; each opens as its own buffer, the first one
    /// current. `--line`/`--column` apply to the first.
    files: Vec<String>,
    /// 1-based, as a person reads them and as `Ln` in the status bar counts.
    /// Converted to 0-based before the editor sees them.
    line: Option<usize>,
    col: Option<usize>,
    export: Option<export::Format>,
    help: bool,
    version: bool,
}

/// Split `--opt=value` into `("--opt", Some("value"))`.
fn split_eq(a: &str) -> (&str, Option<&str>) {
    match a.split_once('=') {
        Some((k, v)) => (k, Some(v)),
        None => (a, None),
    }
}

fn parse_num(flag: &str, v: &str) -> Result<usize, String> {
    v.parse::<usize>()
        .map_err(|_| format!("{flag} needs a number, got {v:?}"))
}

fn parse_args(args: &[String]) -> Result<Args, String> {
    let mut out = Args::default();
    let mut i = 0usize;
    while i < args.len() {
        let (key, inline) = split_eq(&args[i]);
        // A flag that takes a value reads it from `=` or the next word.
        let mut value = |name: &str| -> Result<String, String> {
            if let Some(v) = inline {
                return Ok(v.to_string());
            }
            i += 1;
            args.get(i)
                .cloned()
                .ok_or_else(|| format!("{name} needs a value"))
        };
        match key {
            "-h" | "--help" => out.help = true,
            "-V" | "--version" => out.version = true,
            "-l" | "--line" => out.line = Some(parse_num("--line", &value("--line")?)?),
            "-c" | "--col" | "--column" => {
                out.col = Some(parse_num("--column", &value("--column")?)?)
            }
            "--export" => {
                let v = value("--export")?;
                out.export = Some(export::Format::parse(&v).ok_or_else(|| {
                    format!(
                        "unknown export format {v:?} (expected {})",
                        export::Format::NAMES
                    )
                })?);
            }
            other if other.len() > 1 && other.starts_with('-') => {
                return Err(format!("unknown option {other}"));
            }
            other => out.files.push(other.to_string()),
        }
        i += 1;
    }
    if out.export.is_some() && out.files.len() > 1 {
        return Err(format!(
            "--export takes one file, got {} ({})",
            out.files.len(),
            out.files.join(", ")
        ));
    }
    Ok(out)
}

impl Args {
    /// The first file: the one that opens current, and the one `--line`,
    /// `--column` and `--export` act on.
    fn file(&self) -> Option<&str> {
        self.files.first().map(String::as_str)
    }
}

const USAGE: &str = "\
usage: rano [options] [file...]

  -l, --line N      put the cursor on line N (1-based) and centre it
  -c, --column N    put the cursor on column N (1-based)
                    (both apply to the first file)
  -h, --help        this
  -V, --version     the version

exporting (no terminal needed):
  --export FORMAT [file]   write the file to stdout and exit
                           formats: html, ansi, markdown, text";

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let args = match parse_args(&argv) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("rano: {e}");
            eprintln!("{USAGE}");
            std::process::exit(2);
        }
    };
    if args.help {
        println!("{USAGE}");
        return;
    }
    if args.version {
        println!("rano {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    // `--export`: read, colourise, write, exit. No terminal is touched, which is
    // what makes it usable in a pipe and as a way to measure the highlighter
    // without the UI in the way.
    if let Some(fmt) = args.export {
        if let Err(e) = export_to_stdout(args.file().map(str::to_string), fmt) {
            eprintln!("rano: {e}");
            std::process::exit(1);
        }
        return;
    }
    // The file is NOT read here. Reading it before the terminal is set up is
    // what made `rano huge.log` a dead screen — no frame, no event loop, so not
    // even ^C — for 575 ms at 184 MB and about six seconds at 2 GB. The run
    // loop starts a loader instead and adopts rows as they arrive; see
    // `load_ctrl.rs` and `loader.rs`.
    let file = args.file().map(str::to_string);
    let mut buf = Buffer::new();
    let mut load: Option<PathBuf> = None;
    if let Some(f) = &file {
        let p = Path::new(f);
        // The name is set either way, so language detection, the title bar and
        // the gutter are right from the first frame.
        buf.name = Some(p.to_path_buf());
        if p.exists() {
            load = Some(p.to_path_buf());
        }
    }

    // 1-based on the command line, 0-based internally; a missing column is 0.
    let pos = args.line.map(|l| crate::buffer::Pos {
        row: l.saturating_sub(1),
        col: args.col.unwrap_or(1).saturating_sub(1),
    });
    let mut cfg = config::load();
    // The other files become buffers of their own, read when first visited.
    // Naming several files is asking for several buffers, so the session is
    // multibuffer whatever the config says: F8 and jumps then add buffers
    // rather than replacing the one in view.
    let rest: Vec<PathBuf> = args.files.iter().skip(1).map(PathBuf::from).collect();
    if !rest.is_empty() {
        cfg.multibuffer = true;
    }
    if let Err(e) = run(buf, load, rest, pos, cfg) {
        eprintln!("rano: {}", e);
        std::process::exit(1);
    }
}

/// Read a file (or stdin), colourise it, and write it out. No terminal is
/// touched: this is the path that makes the highlighter measurable on its own,
/// and what `rano --export html big.rs > big.html` uses.
fn export_to_stdout(path: Option<String>, fmt: export::Format) -> io::Result<()> {
    let buf = match &path {
        Some(p) => match Buffer::from_file(Path::new(p)) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("rano: cannot read {p}: {e}");
                std::process::exit(1);
            }
        },
        None => {
            // No file: read stdin, which is what a pipe wants.
            let mut text = String::new();
            io::Read::read_to_string(&mut io::stdin(), &mut text)?;
            let mut b = Buffer::new();
            b.set_rows(text.lines().map(|l| l.chars().collect()).collect());
            if b.rows_is_empty() {
                b.push_row(Vec::new());
            }
            b
        }
    };
    // A name is what `detect` works from, and it also supplies the markdown
    // fence tag and the HTML title, so it is set before highlighting.
    let mut buf = buf;
    let title = match &path {
        Some(p) => p.clone(),
        None => String::from("<stdin>"),
    };
    buf.name = Some(std::path::PathBuf::from(&title));
    // The whole document, not a window: colouring everything is the point of
    // an export, and there is no viewport to bound it by.
    let mut hl = syntax::Highlighter::new();
    hl.refresh(&buf);
    let style_of = |p: Pos| hl.style_at(p);
    let out = export::render(&buf.rows_vec(), 8, &title, fmt, &style_of);
    // Written, not `print!`ed: a closed pipe is the normal end of
    // `rano --export ansi f.rs | head`, not a panic. `println!` aborts the
    // process with a broken-pipe message when the reader goes away.
    let mut stdout = io::stdout().lock();
    if let Err(e) = stdout.write_all(out.as_bytes()) {
        if e.kind() == io::ErrorKind::BrokenPipe {
            return Ok(());
        }
        return Err(e);
    }
    stdout.flush()?;
    Ok(())
}

fn run(
    buf: Buffer,
    load: Option<PathBuf>,
    rest: Vec<PathBuf>,
    pos: Option<crate::buffer::Pos>,
    cfg: config::Config,
) -> io::Result<()> {
    // Panic guard: restore the terminal, then report the panic normally.
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen, DisableMouseCapture);
        prev(info);
    }));
    enable_raw_mode()?;
    execute!(io::stdout(), EnterAlternateScreen, EnableMouseCapture)?;
    execute!(io::stdout(), EnableBracketedPaste)?;
    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::new(backend)?;
    let mut ed = Editor::new(buf, cfg);
    // One request at startup, off the main thread. `run` rather than
    // `Editor::new` because it is a property of RUNNING, not of being — no test
    // makes a network call by constructing an editor.
    ed.start_update_check();
    // 0-based, once, here: `--line 1` is the first row, and the editor's own
    // coordinates are 0-based throughout. `--column` alone leaves the row at 0.
    ed.startup_pos = pos;
    if let Some(path) = load
        && let Err(e) = ed.start_load(&path)
    {
        // The one load failure reported before a frame is drawn: there is
        // nothing on screen yet to attach a status line to.
        disable_raw_mode()?;
        execute!(io::stdout(), LeaveAlternateScreen, DisableMouseCapture)?;
        eprintln!("rano: cannot read {}: {}", path.display(), e);
        std::process::exit(1);
    }
    ed.add_deferred_buffers(&rest);
    // D6 dirty-draw: redraw only when something changed. Any handled key,
    // paste or resize dirties (coarse); the pollers below report their own
    // state changes. text_w is the FULL viewport width now (draw renders
    // full-width lines); justify keeps its old wrap width via a -2 there.
    let mut dirty = true;
    let mut last_size = (0u16, 0u16);
    let result = loop {
        let size = terminal.size()?;
        if (size.width, size.height) != last_size {
            last_size = (size.width, size.height);
            ed.text_w = size.width as usize;
            ed.text_h = (size.height as usize).saturating_sub(4);
            dirty = true;
        }
        // Before the scroll: `--line` CENTRES the target, and centring sets
        // the scroll. Running `adjust_scroll` first would then pull the view
        // back to the nearest edge, which is the opposite of centring. It is
        // also why this waits for the row — see `apply_startup_pos`.
        // A file from the command line whose buffer just became current.
        dirty |= ed.start_pending_load();
        dirty |= ed.refresh_prompt_hints();
        dirty |= ed.refresh_diff_view();
        ed.apply_startup_pos();
        ed.adjust_scroll(ed.text_h);
        ed.adjust_scroll_x();
        if dirty {
            // The frame's highlight, after the scroll the window is measured
            // against. Lazy on purpose: a burst of keystrokes between two
            // frames costs one highlight here rather than one per key, and a
            // huge file's first highlight is this window rather than the whole
            // document.
            ed.ensure_highlight();
            // Hide the physical cursor while the frame paints: the backend
            // moves it across the cells it writes, and on a fast scroll that
            // sweep shows as a ghost cursor blinking at painted cells (often
            // a line start, column 0, mid-screen). draw() re-shows it at the
            // positioned spot, or leaves it hidden when ui::draw skips
            // positioning (edit point outside the viewport).
            terminal.hide_cursor()?;
            terminal.draw(|f| ui::draw(f, &ed))?;
            dirty = false;
        }
        if ed.quit {
            break Ok(());
        }
        // While a file is arriving the loop must come back promptly to adopt
        // it, or the text appears in 200 ms lumps and the load looks like a
        // stutter rather than a stream. 8 ms is a frame at 120 Hz; otherwise the
        // idle wait is the long one, because a keystroke is what ends it.
        //
        // And when the loader is SATURATED — it filled the adoption budget, so
        // more was ready — wait not at all: the budget is what keeps one
        // iteration short, not what paces the load. Without this the loop slept
        // 8 ms per 4096 rows and a 2.6M-line file took ten seconds instead of
        // one, with the disk idle in between.
        let wait = if ed.load_saturated {
            0
        } else if ed.loading() {
            8
        } else {
            200
        };
        if event::poll(Duration::from_millis(wait))? {
            match event::read()? {
                Event::Key(k) if matches!(k.kind, KeyEventKind::Press | KeyEventKind::Repeat) => {
                    ed.handle_key(k);
                    dirty = true;
                }
                Event::Paste(t) => {
                    ed.paste_text(&t);
                    dirty = true;
                }
                Event::Resize(..) => dirty = true,
                Event::Mouse(m) => dirty |= ed.handle_mouse(m),
                _ => {}
            }
        }
        // The load, beside the other pollers: bounded per iteration, never
        // waiting, and it reports its own state changes.
        dirty |= ed.load_poll();
        dirty |= ed.diag_flush(Instant::now());
        dirty |= ed.tick_status();
        dirty |= ed.lsp_poll();
        dirty |= ed.lsp_flush(Instant::now());
        ed.completion_retry_poll();
        dirty |= ed.exec_poll();
        dirty |= ed.update_poll();
    };
    disable_raw_mode()?;
    execute!(
        io::stdout(),
        DisableBracketedPaste,
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    // The update offer is announced HERE, on the way out, and that is deliberate.
    // Installing replaces the running executable, so it takes effect on the next
    // start — and a notice that had to be caught before the screen was torn down
    // would be a notice nobody reads. Printed after the terminal is restored, so
    // it lands in the shell's scrollback and survives.
    if let Some(u) = &ed.update.found
        && ed.update_installable_on_exit()
    {
        println!("{}", u.message());
    }
    result
}

#[cfg(test)]
mod ed_tests {
    use super::*;
    use crate::BufferState;
    use crate::buffer::Pos;
    use crate::editor::{DefBack, Flash};
    use crate::prompt::{PromptKind, complete_path, expand_tilde};
    use crossterm::event::{
        KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
    };
    use ratatui::style::{Color, Modifier, Style};
    use std::fs;
    use std::path::PathBuf;
    use std::sync::mpsc;

    fn test_ed(text: &str) -> Editor {
        let mut buf = Buffer::new();
        buf.set_rows(text.lines().map(|l| l.chars().collect()).collect());
        if buf.rows_is_empty() {
            buf.push_row(Vec::new());
        }
        let mut ed = Editor::new(buf, config::Config::default());
        ed.text_w = 80;
        ed.text_h = 24;
        ed
    }

    fn press(ed: &mut Editor, code: KeyCode, mods: KeyModifiers) {
        ed.handle_key(KeyEvent::new(code, mods));
    }

    fn me(kind: MouseEventKind, row: u16, col: u16) -> MouseEvent {
        MouseEvent {
            kind,
            column: col,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    fn lines(ed: &Editor) -> Vec<String> {
        ed.bs().buf.rows().map(|l| l.iter().collect()).collect()
    }

    struct TempDir(PathBuf);
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn temp_dir(tag: &str) -> TempDir {
        let d = std::env::temp_dir().join(format!(
            "rano_ed_{}_{}_{:?}",
            tag,
            std::process::id(),
            std::time::Instant::now()
        ));
        fs::create_dir_all(&d).unwrap();
        TempDir(d)
    }

    // B2 — move_left at BOL must land on the END of the previous row.

    #[test]
    fn move_left_bol_clamps_to_prev_row_end() {
        let mut ed = test_ed("ab\ncdef");
        ed.bs_mut().cursor = Pos { row: 1, col: 0 };
        press(&mut ed, KeyCode::Left, KeyModifiers::NONE);
        assert_eq!(ed.bs().cursor, Pos { row: 0, col: 2 });
        press(&mut ed, KeyCode::Backspace, KeyModifiers::NONE);
        assert_eq!(lines(&ed), vec!["a", "cdef"]);
    }

    // B1 — prompt editing is char-indexed; multibyte text must not panic.

    #[test]
    fn prompt_multibyte_backspace_no_panic() {
        let mut ed = test_ed("");
        press(&mut ed, KeyCode::Char('f'), KeyModifiers::CONTROL);
        assert!(ed.prompt.is_some());
        press(&mut ed, KeyCode::Char('é'), KeyModifiers::NONE);
        press(&mut ed, KeyCode::Char('a'), KeyModifiers::NONE);
        press(&mut ed, KeyCode::Backspace, KeyModifiers::NONE);
        let p = ed.prompt.as_ref().unwrap();
        assert_eq!(p.text, "é");
        assert_eq!(p.cursor, 1);
    }

    #[test]
    fn prompt_multibyte_insert_no_panic() {
        let mut ed = test_ed("");
        press(&mut ed, KeyCode::Char('f'), KeyModifiers::CONTROL);
        press(&mut ed, KeyCode::Char('é'), KeyModifiers::NONE);
        press(&mut ed, KeyCode::Char('x'), KeyModifiers::NONE);
        let p = ed.prompt.as_ref().unwrap();
        assert_eq!(p.text, "éx");
        assert_eq!(p.cursor, 2);
    }

    #[test]
    fn prompt_mid_string_multibyte_edit() {
        let mut ed = test_ed("");
        press(&mut ed, KeyCode::Char('f'), KeyModifiers::CONTROL);
        press(&mut ed, KeyCode::Char('é'), KeyModifiers::NONE);
        press(&mut ed, KeyCode::Char('a'), KeyModifiers::NONE);
        press(&mut ed, KeyCode::Left, KeyModifiers::NONE);
        press(&mut ed, KeyCode::Char('x'), KeyModifiers::NONE);
        let p = ed.prompt.as_ref().unwrap();
        assert_eq!(p.text, "éxa");
        assert_eq!(p.cursor, 2);
    }

    #[test]
    fn prompt_ascii_edit_still_works() {
        let mut ed = test_ed("");
        press(&mut ed, KeyCode::Char('f'), KeyModifiers::CONTROL);
        for c in ['a', 'b', 'c'] {
            press(&mut ed, KeyCode::Char(c), KeyModifiers::NONE);
        }
        press(&mut ed, KeyCode::Backspace, KeyModifiers::NONE);
        press(&mut ed, KeyCode::Left, KeyModifiers::NONE);
        press(&mut ed, KeyCode::Char('x'), KeyModifiers::NONE);
        let p = ed.prompt.as_ref().unwrap();
        assert_eq!(p.text, "axb");
        assert_eq!(p.cursor, 2);
        assert!(ed.prompt.is_some());
    }

    // C3 — ^C cursor position flashes for 2 s, then expires.

    #[test]
    fn show_loc_expires() {
        let mut ed = test_ed("hi");
        press(&mut ed, KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(ed.status_text(), Some("Line 1, Col 1".to_string()));
        ed.loc_until = Some(Instant::now() - Duration::from_secs(1));
        ed.tick_status();
        assert_eq!(ed.status_text(), None);
    }

    #[test]
    fn goto_still_shows_loc() {
        let mut ed = test_ed("a\nb\nc");
        press(&mut ed, KeyCode::Char('7'), KeyModifiers::CONTROL);
        press(&mut ed, KeyCode::Char('3'), KeyModifiers::NONE);
        press(&mut ed, KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(ed.bs().cursor, Pos { row: 2, col: 0 });
        let st = ed.status_text().unwrap();
        assert!(st.contains("Line 3"), "status: {st}");
    }

    #[test]
    fn prompt_reopen_multibyte_query_cursor_ok() {
        // ^F re-opens the prompt only when the last search had no matches,
        // so search for something absent; the seeded cursor must be a CHAR
        // index (byte length would start past EOL for "éé").
        let mut ed = test_ed("abc");
        press(&mut ed, KeyCode::Char('f'), KeyModifiers::CONTROL);
        for c in "éé".chars() {
            press(&mut ed, KeyCode::Char(c), KeyModifiers::NONE);
        }
        press(&mut ed, KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(ed.bs().search.query, "éé");
        press(&mut ed, KeyCode::Char('f'), KeyModifiers::CONTROL);
        let p = ed.prompt.as_ref().unwrap();
        assert_eq!(p.text, "éé");
        assert_eq!(p.cursor, 2);
    }

    // D2 — sort: case-insensitive, region-scoped when marked.

    #[test]
    fn sort_lines_case_insensitive_and_region() {
        let mut ed = test_ed("b\nA\nc\na\nB");
        press(&mut ed, KeyCode::Char('a'), KeyModifiers::ALT);
        press(&mut ed, KeyCode::Down, KeyModifiers::NONE);
        press(&mut ed, KeyCode::Down, KeyModifiers::NONE);
        press(&mut ed, KeyCode::F(9), KeyModifiers::NONE);
        assert_eq!(lines(&ed), vec!["A", "b", "c", "a", "B"]);
        assert!(ed.bs().mark.is_none());
        let mut ed = test_ed("B\na\nA\nb");
        press(&mut ed, KeyCode::F(9), KeyModifiers::NONE);
        assert_eq!(lines(&ed), vec!["a", "A", "B", "b"]);
    }

    // C1 — save_to preserves the buffer's original CRLF line endings.

    #[test]
    fn save_preserves_crlf() {
        let d = temp_dir("crlf");
        let src = d.0.join("in.txt");
        fs::write(&src, "a\r\nb\r\n").unwrap();
        let buf = Buffer::from_file(&src).unwrap();
        assert!(buf.crlf);
        let mut ed = Editor::new(buf, config::Config::default());
        let out = d.0.join("out.txt");
        ed.save_to(out.clone());
        let bytes = fs::read(&out).unwrap();
        assert_eq!(bytes, b"a\r\nb\r\n");
        assert!(!ed.bs().buf.modified);
        assert_eq!(ed.bs().buf.name, Some(out));
    }

    // Saving over a file something else changed since it was read.

    /// A file read into an editor, then rewritten on disk behind its back.
    fn externally_changed(tag: &str) -> (TempDir, PathBuf, Editor) {
        let d = temp_dir(tag);
        let f = d.0.join("f.txt");
        fs::write(&f, "one\ntwo\n").unwrap();
        let mut ed = Editor::new(Buffer::from_file(&f).unwrap(), config::Config::default());
        press_text(&mut ed, "X");
        fs::write(&f, "one\ntwo\nthree from elsewhere\n").unwrap();
        (d, f, ed)
    }

    fn prompt_kind(ed: &Editor) -> Option<PromptKind> {
        ed.prompt.as_ref().map(|p| p.kind)
    }

    #[test]
    fn save_of_an_unchanged_file_does_not_ask() {
        let d = temp_dir("ext_same");
        let f = d.0.join("f.txt");
        fs::write(&f, "one\n").unwrap();
        let mut ed = Editor::new(Buffer::from_file(&f).unwrap(), config::Config::default());
        press_text(&mut ed, "X");
        // ^O on the buffer's own file is a save, not "File exists".
        ed.do_write(f.display().to_string());
        assert_eq!(prompt_kind(&ed), None);
        assert_eq!(fs::read_to_string(&f).unwrap(), "Xone\n");
    }

    #[test]
    fn save_asks_when_the_file_changed_on_disk() {
        let (_d, f, mut ed) = externally_changed("ext_ask");
        ed.save_to(f.clone());
        assert_eq!(prompt_kind(&ed), Some(PromptKind::ConfirmExternal));
        assert_eq!(
            fs::read_to_string(&f).unwrap(),
            "one\ntwo\nthree from elsewhere\n"
        );
        // y writes, and the next save is quiet: the stamp is the new file's.
        press(&mut ed, KeyCode::Char('y'), KeyModifiers::NONE);
        assert_eq!(fs::read_to_string(&f).unwrap(), "Xone\ntwo\n");
        press_text(&mut ed, "Y");
        ed.save_to(f.clone());
        assert_eq!(prompt_kind(&ed), None);
        assert_eq!(fs::read_to_string(&f).unwrap(), "XYone\ntwo\n");
    }

    #[test]
    fn no_to_the_external_change_keeps_the_file_and_disarms_quit() {
        let (_d, f, mut ed) = externally_changed("ext_no");
        press(&mut ed, KeyCode::Char('x'), KeyModifiers::CONTROL);
        press(&mut ed, KeyCode::Char('y'), KeyModifiers::NONE);
        assert_eq!(prompt_kind(&ed), Some(PromptKind::ConfirmExternal));
        press(&mut ed, KeyCode::Char('n'), KeyModifiers::NONE);
        assert!(!ed.quit);
        assert!(!ed.quit_after_save);
        assert!(ed.bs().buf.modified);
        assert_eq!(
            fs::read_to_string(&f).unwrap(),
            "one\ntwo\nthree from elsewhere\n"
        );
    }

    #[test]
    fn d_shows_the_diff_and_esc_comes_back_to_the_question() {
        let (_d, f, mut ed) = externally_changed("ext_diff");
        ed.text_w = 100;
        ed.text_h = 20;
        ed.save_to(f.clone());
        press(&mut ed, KeyCode::Char('d'), KeyModifiers::NONE);
        let text = |ed: &Editor| -> Vec<String> {
            ed.diff_view
                .as_ref()
                .expect("diff view")
                .lines
                .iter()
                .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
                .collect()
        };
        // Unified first (the session's default): the file's name, the hunk, and
        // each line numbered in its own file.
        let rows = text(&ed);
        assert!(rows[0].ends_with("f.txt"), "{rows:?}");
        assert!(rows[1].starts_with("@@"), "{rows:?}");
        assert!(rows.contains(&"1 -one".to_string()), "{rows:?}");
        assert!(rows.contains(&"1 +Xone".to_string()), "{rows:?}");
        assert!(
            rows.contains(&"3 -three from elsewhere".to_string()),
            "{rows:?}"
        );
        // s: two panels, the disk's line on the left and the buffer's on the right.
        press(&mut ed, KeyCode::Char('s'), KeyModifiers::NONE);
        assert!(ed.diff_split, "the choice is remembered");
        let rows = text(&ed);
        let pair = rows.iter().find(|r| r.contains("Xone")).expect("the pair");
        let (left, right) = pair.split_once('│').expect("two panels");
        assert!(left.contains("- one"), "{pair:?}");
        assert!(right.contains("+ Xone"), "{pair:?}");
        // A resize re-renders at the new width.
        ed.text_w = 60;
        assert!(ed.refresh_diff_view());
        assert!(!ed.refresh_diff_view());
        // Drawn over the text, with the header naming the view.
        let backend = ratatui::backend::TestBackend::new(60, 12);
        let mut term = ratatui::Terminal::new(backend).unwrap();
        ed.text_h = 8;
        term.draw(|fr| ui::draw(fr, &ed)).unwrap();
        let buf = term.backend().buffer().clone();
        let row =
            |y: u16| -> String { (0..60).map(|x| buf[(x, y)].symbol().to_string()).collect() };
        assert!(row(1).contains("Saving would change"), "{}", row(1));
        assert!((2..9).any(|y| row(y).contains("Xone")));
        press(&mut ed, KeyCode::Esc, KeyModifiers::NONE);
        assert!(ed.diff_view.is_none());
        assert_eq!(prompt_kind(&ed), Some(PromptKind::ConfirmExternal));
        // y from the diff itself answers the question.
        press(&mut ed, KeyCode::Char('d'), KeyModifiers::NONE);
        assert!(ed.diff_view.is_some());
        press(&mut ed, KeyCode::Char('y'), KeyModifiers::NONE);
        assert!(ed.diff_view.is_none());
        assert_eq!(fs::read_to_string(&f).unwrap(), "Xone\ntwo\n");
    }

    fn diff_view_text(ed: &Editor) -> Vec<String> {
        ed.diff_view
            .as_ref()
            .expect("diff view")
            .lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect()
    }

    #[test]
    fn m_p_previews_a_patch_buffer_and_closes_again() {
        let mut ed = test_ed(
            "diff --git a/src/a.rs b/src/a.rs\n--- a/src/a.rs\n+++ b/src/a.rs\n@@ -7,3 +7,3 @@\n fn a() {\n-    old();\n+    new();\n }",
        );
        ed.bs_mut().buf.name = Some(PathBuf::from("/tmp/rano_preview.patch"));
        ed.text_w = 100;
        ed.text_h = 20;
        press(&mut ed, KeyCode::Char('p'), KeyModifiers::ALT);
        let rows = diff_view_text(&ed);
        assert!(rows.contains(&"src/a.rs".to_string()), "{rows:?}");
        assert!(rows.contains(&"8 -    old();".to_string()), "{rows:?}");
        assert!(rows.contains(&"8 +    new();".to_string()), "{rows:?}");
        assert!(ed.diff_view.as_ref().unwrap().header().contains("Patch"));
        // y and n are not answers here; M-P closes and leaves no prompt.
        press(&mut ed, KeyCode::Char('y'), KeyModifiers::NONE);
        assert!(ed.diff_view.is_some());
        press(&mut ed, KeyCode::Char('p'), KeyModifiers::ALT);
        assert!(ed.diff_view.is_none());
        assert!(ed.prompt.is_none());
        // The text is untouched: the preview is a view, not an edit.
        assert_eq!(lines(&ed)[0], "diff --git a/src/a.rs b/src/a.rs");
    }

    #[test]
    fn m_p_shows_merge_conflicts_ours_against_theirs() {
        let mut ed = test_ed(
            "fn main() {\n<<<<<<< HEAD\n    let total = 1;\n=======\n    let sum = 1;\n>>>>>>> feature\n}",
        );
        ed.bs_mut().buf.name = Some(PathBuf::from("/tmp/rano_conflict.rs"));
        ed.text_w = 100;
        ed.text_h = 20;
        ed.diff_split = true;
        press(&mut ed, KeyCode::Char('p'), KeyModifiers::ALT);
        let rows = diff_view_text(&ed);
        assert!(rows[0].starts_with("1 conflict"), "{rows:?}");
        let pair = rows
            .iter()
            .find(|r| r.contains("let total"))
            .expect("{rows:?}");
        let (left, right) = pair.split_once('│').expect("two panels");
        assert!(
            left.contains("let total") && right.contains("let sum"),
            "{pair:?}"
        );
        assert!(
            ed.diff_view
                .as_ref()
                .unwrap()
                .header()
                .contains("Conflict 1/1")
        );
        press(&mut ed, KeyCode::Esc, KeyModifiers::NONE);
        assert!(ed.diff_view.is_none());
    }

    const CONFLICTED: &str = "fn main() {\n<<<<<<< HEAD\n    let total = 1;\n=======\n    let sum = 1;\n>>>>>>> feature\n    mid();\n<<<<<<< HEAD\n    one();\n||||||| base\n    zero();\n=======\n    two();\n>>>>>>> feature\n}";

    fn conflict_ed() -> Editor {
        let mut ed = test_ed(CONFLICTED);
        ed.bs_mut().buf.name = Some(PathBuf::from("/tmp/rano_resolve.rs"));
        ed.text_w = 120;
        ed.text_h = 30;
        press(&mut ed, KeyCode::Char('p'), KeyModifiers::ALT);
        assert!(ed.diff_view.is_some());
        ed
    }

    #[test]
    fn the_conflict_view_moves_between_conflicts_and_compares_with_the_base() {
        let mut ed = conflict_ed();
        // A short screen, so moving to a conflict has to scroll.
        ed.text_h = 6;
        assert!(
            ed.diff_view
                .as_ref()
                .unwrap()
                .header()
                .contains("Conflict 1/2")
        );
        press(&mut ed, KeyCode::Char('n'), KeyModifiers::NONE);
        let v = ed.diff_view.as_ref().unwrap();
        assert!(v.header().contains("Conflict 2/2"), "{}", v.header());
        // Scrolled to it: the first row shown is its header, marked current.
        let first: String = v.lines[v.top]
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert!(first.starts_with("▶ Conflict 2 of 2"), "{first:?}");
        // Past the last one it stays.
        press(&mut ed, KeyCode::Char('n'), KeyModifiers::NONE);
        assert!(
            ed.diff_view
                .as_ref()
                .unwrap()
                .header()
                .contains("Conflict 2/2")
        );
        // c: base against ours shows the base's line.
        press(&mut ed, KeyCode::Char('c'), KeyModifiers::NONE);
        let rows = diff_view_text(&ed);
        assert!(
            ed.diff_view
                .as_ref()
                .unwrap()
                .header()
                .contains("base/ours")
        );
        assert!(rows.iter().any(|r| r.contains("zero()")), "{rows:?}");
        press(&mut ed, KeyCode::Char('p'), KeyModifiers::NONE);
        assert!(
            ed.diff_view
                .as_ref()
                .unwrap()
                .header()
                .contains("Conflict 1/2")
        );
    }

    #[test]
    fn taking_a_side_resolves_one_conflict_as_one_undo_step() {
        let mut ed = conflict_ed();
        press(&mut ed, KeyCode::Char('t'), KeyModifiers::NONE);
        assert_eq!(
            lines(&ed)[..3],
            ["fn main() {", "    let sum = 1;", "    mid();"],
            "{:?}",
            lines(&ed)
        );
        // One left, and it is now current.
        let v = ed.diff_view.as_ref().expect("still open");
        assert!(v.header().contains("Conflict 1/1"), "{}", v.header());
        assert!(ed.status_text().unwrap().contains("1 conflict left"));
        // b: both, ours first — the last one, so the view closes.
        press(&mut ed, KeyCode::Char('b'), KeyModifiers::NONE);
        assert!(ed.diff_view.is_none());
        assert!(ed.status_text().unwrap().contains("All conflicts resolved"));
        assert_eq!(
            lines(&ed),
            vec![
                "fn main() {",
                "    let sum = 1;",
                "    mid();",
                "    one();",
                "    two();",
                "}"
            ]
        );
        // Each take is one undo step.
        press(&mut ed, KeyCode::Char('u'), KeyModifiers::ALT);
        assert!(lines(&ed).contains(&"<<<<<<< HEAD".to_string()));
        assert!(lines(&ed).contains(&"    let sum = 1;".to_string()));
        press(&mut ed, KeyCode::Char('u'), KeyModifiers::ALT);
        assert_eq!(lines(&ed).join("\n"), CONFLICTED);
    }

    #[test]
    fn m_p_on_plain_text_says_there_is_nothing_to_render() {
        let mut ed = test_ed("just text");
        press(&mut ed, KeyCode::Char('p'), KeyModifiers::ALT);
        assert!(ed.diff_view.is_none());
        assert!(ed.status_text().unwrap().starts_with("Nothing to render"));
    }

    #[test]
    fn a_timestamp_only_change_says_so_in_the_diff() {
        let d = temp_dir("ext_touch");
        let f = d.0.join("f.txt");
        fs::write(&f, "one\n").unwrap();
        let mut ed = Editor::new(Buffer::from_file(&f).unwrap(), config::Config::default());
        let later = std::time::SystemTime::now() + Duration::from_secs(60);
        fs::File::options()
            .write(true)
            .open(&f)
            .unwrap()
            .set_modified(later)
            .unwrap();
        ed.save_to(f.clone());
        assert_eq!(prompt_kind(&ed), Some(PromptKind::ConfirmExternal));
        press(&mut ed, KeyCode::Char('d'), KeyModifiers::NONE);
        assert!(ed.diff_view.is_none());
        assert_eq!(prompt_kind(&ed), Some(PromptKind::ConfirmExternal));
        assert!(
            ed.status_text()
                .unwrap()
                .contains("only the file's timestamp")
        );
    }

    // D3 — undo/redo protective tests (green on the snapshot impl; they must
    // stay green through the region-based refactor).

    fn press_text(ed: &mut Editor, s: &str) {
        for c in s.chars() {
            press(ed, KeyCode::Char(c), KeyModifiers::NONE);
        }
    }

    fn replace_via_prompt(ed: &mut Editor, find: &str, with: &str, answer: char) {
        press(ed, KeyCode::Char('4'), KeyModifiers::CONTROL);
        press_text(ed, find);
        press(ed, KeyCode::Enter, KeyModifiers::NONE);
        press_text(ed, with);
        press(ed, KeyCode::Enter, KeyModifiers::NONE);
        press(ed, KeyCode::Char(answer), KeyModifiers::NONE);
    }

    #[test]
    fn undo_types_coalesce() {
        let mut ed = test_ed("");
        press_text(&mut ed, "abc");
        assert_eq!(lines(&ed), vec!["abc"]);
        press(&mut ed, KeyCode::Char('u'), KeyModifiers::ALT);
        assert_eq!(lines(&ed), vec![""]);
    }

    #[test]
    fn undo_backspace_run() {
        let mut ed = test_ed("ab");
        press(&mut ed, KeyCode::End, KeyModifiers::NONE);
        press(&mut ed, KeyCode::Backspace, KeyModifiers::NONE);
        press(&mut ed, KeyCode::Backspace, KeyModifiers::NONE);
        assert_eq!(lines(&ed), vec![""]);
        press(&mut ed, KeyCode::Char('u'), KeyModifiers::ALT);
        assert_eq!(lines(&ed), vec!["ab"]);
    }

    #[test]
    fn undo_redo_roundtrip() {
        let mut ed = test_ed("ab");
        press(&mut ed, KeyCode::End, KeyModifiers::NONE);
        press(&mut ed, KeyCode::Char('c'), KeyModifiers::NONE);
        press(&mut ed, KeyCode::Char('u'), KeyModifiers::ALT);
        assert_eq!(lines(&ed), vec!["ab"]);
        assert_eq!(ed.bs().cursor, Pos { row: 0, col: 2 });
        press(&mut ed, KeyCode::Char('e'), KeyModifiers::ALT);
        assert_eq!(lines(&ed), vec!["abc"]);
        assert_eq!(ed.bs().cursor, Pos { row: 0, col: 3 });
    }

    #[test]
    fn undo_repeated_cut_coalesce() {
        let mut ed = test_ed("1\n2\n3\n4");
        for _ in 0..3 {
            press(&mut ed, KeyCode::Char('k'), KeyModifiers::CONTROL);
        }
        assert_eq!(lines(&ed), vec!["4"]);
        press(&mut ed, KeyCode::Char('u'), KeyModifiers::ALT);
        assert_eq!(lines(&ed), vec!["1", "2", "3", "4"]);
    }

    #[test]
    fn undo_partial_then_full_cut() {
        let mut ed = test_ed("hello");
        press(&mut ed, KeyCode::Right, KeyModifiers::NONE);
        press(&mut ed, KeyCode::Right, KeyModifiers::NONE);
        press(&mut ed, KeyCode::Char('k'), KeyModifiers::CONTROL);
        assert_eq!(lines(&ed), vec!["he"]);
        press(&mut ed, KeyCode::Char('k'), KeyModifiers::CONTROL);
        assert_eq!(lines(&ed), vec![""]);
        press(&mut ed, KeyCode::Char('u'), KeyModifiers::ALT);
        assert_eq!(lines(&ed), vec!["hello"]);
    }

    #[test]
    fn undo_paste_roundtrip() {
        let mut ed = test_ed("ab\ncd");
        press(&mut ed, KeyCode::Char('k'), KeyModifiers::CONTROL);
        press(&mut ed, KeyCode::Char('u'), KeyModifiers::CONTROL);
        assert_eq!(lines(&ed), vec!["abcd"]);
        press(&mut ed, KeyCode::Char('u'), KeyModifiers::ALT);
        assert_eq!(lines(&ed), vec!["cd"]);
        assert_eq!(ed.bs().cursor, Pos { row: 0, col: 0 });
        press(&mut ed, KeyCode::Char('e'), KeyModifiers::ALT);
        assert_eq!(lines(&ed), vec!["abcd"]);
        assert_eq!(ed.bs().cursor, Pos { row: 0, col: 2 });
    }

    #[test]
    fn undo_replace_all_one_step() {
        let mut ed = test_ed("aa\naa");
        replace_via_prompt(&mut ed, "aa", "x", 'a');
        assert_eq!(lines(&ed), vec!["x", "x"]);
        press(&mut ed, KeyCode::Char('u'), KeyModifiers::ALT);
        assert_eq!(lines(&ed), vec!["aa", "aa"]);
    }

    #[test]
    fn undo_limit_trims() {
        let mut ed = test_ed("a");
        for _ in 0..600 {
            press(&mut ed, KeyCode::Enter, KeyModifiers::NONE);
        }
        // CURRENT snapshot impl coalesces a run of Enters into one step (1
        // step here); D3 makes Newline never coalesce (500 steps). The plan's
        // contract "500 steps max" holds for both.
        assert!(ed.bs().undo.len() <= 500);
    }

    #[test]
    fn undo_selection_overwrite() {
        let mut ed = test_ed("abcd");
        press(&mut ed, KeyCode::Right, KeyModifiers::NONE);
        press(&mut ed, KeyCode::Char('a'), KeyModifiers::ALT);
        press(&mut ed, KeyCode::Right, KeyModifiers::NONE);
        press(&mut ed, KeyCode::Right, KeyModifiers::NONE);
        press(&mut ed, KeyCode::Char('X'), KeyModifiers::NONE);
        assert_eq!(lines(&ed), vec!["aXd"]);
        press(&mut ed, KeyCode::Char('u'), KeyModifiers::ALT);
        assert_eq!(lines(&ed), vec!["abcd"]);
        assert!(ed.bs().mark.is_none());
    }

    #[test]
    fn undo_delete_selection() {
        let mut ed = test_ed("abcd");
        press(&mut ed, KeyCode::Right, KeyModifiers::NONE);
        press(&mut ed, KeyCode::Char('a'), KeyModifiers::ALT);
        press(&mut ed, KeyCode::Right, KeyModifiers::NONE);
        press(&mut ed, KeyCode::Right, KeyModifiers::NONE);
        press(&mut ed, KeyCode::Backspace, KeyModifiers::NONE);
        assert_eq!(lines(&ed), vec!["ad"]);
        press(&mut ed, KeyCode::Char('u'), KeyModifiers::ALT);
        assert_eq!(lines(&ed), vec!["abcd"]);
    }

    #[test]
    fn undo_newline_join() {
        let mut ed = test_ed("ab");
        press(&mut ed, KeyCode::End, KeyModifiers::NONE);
        press(&mut ed, KeyCode::Enter, KeyModifiers::NONE);
        press(&mut ed, KeyCode::Backspace, KeyModifiers::NONE);
        assert_eq!(lines(&ed), vec!["ab"]);
        press(&mut ed, KeyCode::Char('u'), KeyModifiers::ALT);
        assert_eq!(lines(&ed), vec!["ab", ""]);
        press(&mut ed, KeyCode::Char('u'), KeyModifiers::ALT);
        assert_eq!(lines(&ed), vec!["ab"]);
    }

    #[test]
    fn undo_read_empty_replace() {
        let d = temp_dir("read_undo");
        let src = d.0.join("in.txt");
        fs::write(&src, "x\ny\n").unwrap();
        let mut ed = test_ed("");
        press(&mut ed, KeyCode::Char('r'), KeyModifiers::CONTROL);
        press_text(&mut ed, &src.display().to_string());
        press(&mut ed, KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(lines(&ed), vec!["x", "y"]);
        press(&mut ed, KeyCode::Char('u'), KeyModifiers::ALT);
        assert_eq!(lines(&ed), vec![""]);
    }

    #[test]
    fn undo_redo_after_new_edit() {
        let mut ed = test_ed("ab");
        press(&mut ed, KeyCode::End, KeyModifiers::NONE);
        press(&mut ed, KeyCode::Char('c'), KeyModifiers::NONE);
        press(&mut ed, KeyCode::Char('u'), KeyModifiers::ALT);
        press(&mut ed, KeyCode::Char('x'), KeyModifiers::NONE);
        assert_eq!(lines(&ed), vec!["abx"]);
        press(&mut ed, KeyCode::Char('e'), KeyModifiers::ALT);
        assert_eq!(lines(&ed), vec!["abx"]);
    }

    // D1 — replace-all is one pass from the cursor, non-overlapping.

    #[test]
    fn replace_all_from_cursor_and_no_overlap() {
        let mut ed = test_ed("aaa");
        replace_via_prompt(&mut ed, "aa", "b", 'a');
        assert_eq!(lines(&ed), vec!["ba"]);
        assert_eq!(ed.replace_count, 1);
    }

    #[test]
    fn replace_all_multiline() {
        let mut ed = test_ed("aa\naa\naa");
        replace_via_prompt(&mut ed, "aa", "x", 'a');
        assert_eq!(ed.replace_count, 3);
        assert_eq!(ed.bs().cursor, Pos { row: 2, col: 1 });
        assert_eq!(lines(&ed), vec!["x", "x", "x"]);
    }

    #[test]
    fn replace_all_drift() {
        let mut ed = test_ed("aaaa");
        replace_via_prompt(&mut ed, "aa", "bbb", 'a');
        assert_eq!(lines(&ed), vec!["bbbbbb"]);
        assert_eq!(ed.replace_count, 2);
    }

    // D4 — didChange is debounced: the flag clears only after the 300 ms
    // window, even when no client is attached (scratch buffer → lsp None).

    #[test]
    fn lsp_flush_debounce() {
        let mut ed = test_ed("");
        let now = Instant::now();
        ed.insert_char('x');
        assert!(ed.bs().lsp_dirty);
        ed.lsp_flush(now + Duration::from_millis(100));
        assert!(ed.bs().lsp_dirty, "inside the window the flag must survive");
        ed.lsp_flush(now + Duration::from_millis(400));
        assert!(!ed.bs().lsp_dirty, "no client attached: flag still cleared");
        assert_eq!(ed.bs().lsp_last_send, now + Duration::from_millis(400));
    }

    // D5 — a failed async handshake is adopted on the next poll and flashes;
    // a handshake started for another file stays silent.

    #[test]
    fn lsp_adopt_err_flashes() {
        let mut ed = test_ed("");
        let (tx, rx) = mpsc::channel::<Result<lsp::LspClient, String>>();
        tx.send(Err("boom".to_string())).unwrap();
        ed.bs_mut().lsp_starting = Some(("".to_string(), rx)); // scratch buffer → tag ""
        ed.lsp_poll();
        assert!(ed.bs().lsp.is_none());
        assert!(ed.bs().lsp_starting.is_none());
        let st = ed.status_text().unwrap();
        assert!(st.contains("boom"), "status: {st}");
    }

    #[test]
    fn lsp_adopt_stale_silent() {
        let mut ed = test_ed("");
        let (tx, rx) = mpsc::channel::<Result<lsp::LspClient, String>>();
        tx.send(Err("stale boom".to_string())).unwrap();
        ed.bs_mut().lsp_starting = Some(("/tmp/other.rs".to_string(), rx));
        ed.lsp_poll();
        assert!(ed.bs().lsp.is_none());
        assert!(ed.bs().lsp_starting.is_none());
        assert!(ed.status.is_none(), "stale failure must be silent");
    }

    // E5 — diagnostics are underlined in severity color under the cursor;
    // search matches and the selection still win.

    // ---------- completion (LSP) ----------

    use crate::editor::{CompletionPopup, completion_prefix};

    fn citem(label: &str, kind: u64) -> lsp::CompletionItem {
        lsp::CompletionItem {
            label: label.to_string(),
            kind,
            insert: label.to_string(),
            sort: label.to_string(),
            filter: label.to_string(),
        }
    }

    fn popup(items: Vec<lsp::CompletionItem>, row: usize, col: usize) -> CompletionPopup {
        CompletionPopup {
            items,
            sel: 0,
            row,
            col,
        }
    }

    #[test]
    fn completion_prefix_word_dot_and_colons() {
        let l: Vec<Vec<char>> = vec![
            "foo.bar".chars().collect(),
            "x::".chars().collect(),
            " a ".chars().collect(),
        ];
        assert_eq!(completion_prefix(&l, 0, 7), Some(("bar".to_string(), 4)));
        assert_eq!(completion_prefix(&l, 0, 4), Some((String::new(), 4)));
        assert_eq!(completion_prefix(&l, 1, 3), Some((String::new(), 3)));
        assert_eq!(completion_prefix(&l, 2, 2), Some(("a".to_string(), 1)));
        assert_eq!(completion_prefix(&l, 2, 3), None);
        assert_eq!(completion_prefix(&l, 2, 0), None);
    }

    #[test]
    fn completion_response_prefix_first_then_fuzzy() {
        // Exact prefix matches keep the server's order and come first;
        // subsequence matches follow; non-matches are dropped.
        let mut ed = test_ed("x\nis\n");
        ed.bs_mut().cursor = Pos { row: 1, col: 2 };
        ed.completion_q.push_back((1, "is".to_string()));
        ed.completion = Some(popup(Vec::new(), 1, 0));
        // sortText carries the server's ranking: exact matches first here.
        let it = |label: &str, sort: &str| {
            let mut c = citem(label, 6);
            c.sort = sort.to_string();
            c
        };
        ed.complete_response(vec![
            it("into_raw_parts", "3"),
            it("is_empty", "1"),
            it("__is_long", "4"),
            it("as_bytes", "5"),
            it("is_ascii", "2"),
        ]);
        let p = ed.completion.as_ref().unwrap();
        let labels: Vec<_> = p.items.iter().map(|i| i.label.as_str()).collect();
        // "into_raw_parts" survives as a subsequence match (i…s), but only
        // after the exact-prefix items; "as_bytes" has no `i` and is dropped.
        assert_eq!(
            labels,
            vec!["is_empty", "is_ascii", "into_raw_parts", "__is_long"]
        );
        assert_eq!(p.sel, 0);
        assert!(ed.completion_q.is_empty(), "queue entry consumed");
        // Empty prefix (right after `.`): the server's list is kept as-is.
        let mut ed2 = test_ed("x.\n");
        ed2.bs_mut().cursor = Pos { row: 0, col: 2 };
        ed2.completion_q.push_back((0, String::new()));
        ed2.completion = Some(popup(Vec::new(), 0, 0));
        ed2.complete_response(vec![citem("into_raw_parts", 6), citem("zz", 6)]);
        assert_eq!(ed2.completion.as_ref().unwrap().items.len(), 2);
    }

    #[test]
    fn completion_fallback_junk_parks_popup_and_retries() {
        // A `.`-context answered with path-fallback items (rust-analyzer
        // still scanning a freshly opened crate) is not applied: the popup
        // stays empty and a re-request is scheduled. The same items in a
        // word context ("cra") are legitimate and applied as-is.
        let mut ed = test_ed("text.\n");
        ed.bs_mut().cursor = Pos { row: 0, col: 5 };
        ed.completion_q.push_back((0, String::new()));
        ed.completion = Some(popup(Vec::new(), 0, 5));
        ed.complete_response(vec![citem("crate::", 0), citem("text", 6)]);
        assert!(
            ed.completion.as_ref().unwrap().items.is_empty(),
            "fallback junk parked"
        );
        assert!(ed.completion_retry.is_some(), "re-request scheduled");
        assert_eq!(ed.completion_retries, 1);
        // Timer elapsed: the poll consumes it and keeps the popup open
        // (no LSP attached here, so the re-request itself is a no-op).
        ed.completion_retry = Some(Instant::now() - Duration::from_millis(1));
        ed.completion_retry_poll();
        assert!(ed.completion.is_some());
        assert!(ed.completion_retry.is_none(), "timer consumed");
        // Word context: path items are legitimate.
        let mut ed2 = test_ed("cra\n");
        ed2.bs_mut().cursor = Pos { row: 0, col: 3 };
        ed2.completion_q.push_back((0, "cra".to_string()));
        ed2.completion = Some(popup(Vec::new(), 0, 0));
        ed2.complete_response(vec![citem("crate::", 0)]);
        assert_eq!(ed2.completion.as_ref().unwrap().items.len(), 1);
        // A good dot-context response resets the retry state.
        let mut ed3 = test_ed("text.\n");
        ed3.bs_mut().cursor = Pos { row: 0, col: 5 };
        ed3.completion_q.push_back((0, String::new()));
        ed3.completion = Some(popup(Vec::new(), 0, 5));
        ed3.completion_retries = 3;
        ed3.complete_response(vec![citem("is_empty", 1)]);
        assert_eq!(ed3.completion.as_ref().unwrap().items.len(), 1);
        assert!(ed3.completion_retry.is_none());
        assert_eq!(ed3.completion_retries, 0);
    }

    #[test]
    fn completion_response_stale_or_missing_popup() {
        // Response answering an older keystroke (different prefix): dropped,
        // popup untouched.
        let mut ed = test_ed("ab\n");
        ed.bs_mut().cursor = Pos { row: 0, col: 2 };
        ed.completion_q.push_back((0, "a".to_string()));
        ed.completion = Some(popup(vec![citem("ab", 6)], 0, 0));
        ed.complete_response(vec![citem("abc", 6)]);
        let p = ed.completion.as_ref().unwrap();
        assert_eq!(p.items.len(), 1, "stale response must not replace items");
        // Response for a different row than the cursor: popup closed.
        let mut ed2 = test_ed("ab\n");
        ed2.completion_q.push_back((0, "ab".to_string()));
        ed2.completion = Some(popup(Vec::new(), 5, 0));
        ed2.complete_response(vec![citem("ab", 6)]);
        assert!(ed2.completion.is_none());
        // Matched context but no popup open at all: ignored, queue cleared.
        let mut ed3 = test_ed("ab\n");
        ed3.bs_mut().cursor = Pos { row: 0, col: 2 };
        ed3.completion_q.push_back((0, "ab".to_string()));
        ed3.complete_response(vec![citem("ab", 6)]);
        assert!(ed3.completion.is_none());
        assert!(ed3.completion_q.is_empty());
        // No request in flight: response ignored.
        let mut ed4 = test_ed("ab\n");
        ed4.completion = Some(popup(Vec::new(), 0, 0));
        ed4.complete_response(vec![citem("ab", 6)]);
        assert!(ed4.completion_q.is_empty());
    }

    #[test]
    fn completion_response_no_matches_closes() {
        let mut ed = test_ed("zz\n");
        ed.bs_mut().cursor = Pos { row: 0, col: 2 };
        ed.completion_q.push_back((0, "zz".to_string()));
        ed.completion = Some(popup(Vec::new(), 0, 0));
        ed.complete_response(vec![]);
        assert!(ed.completion.is_none());
    }

    #[test]
    fn completion_accept_inserts_remainder() {
        let mut ed = test_ed("pri\n");
        ed.bs_mut().cursor = Pos { row: 0, col: 3 };
        ed.completion = Some(popup(vec![citem("println!", 3)], 0, 0));
        ed.completion_accept();
        assert_eq!(lines(&ed), vec!["println!"]);
        assert_eq!(ed.bs().cursor, Pos { row: 0, col: 8 });
        assert!(ed.completion.is_none());
    }

    #[test]
    fn completion_accept_replaces_divergent_prefix() {
        let mut ed = test_ed("pri\n");
        ed.bs_mut().cursor = Pos { row: 0, col: 3 };
        ed.completion = Some(popup(vec![citem("puts", 6)], 0, 0));
        ed.completion_accept();
        assert_eq!(lines(&ed), vec!["puts"]);
        assert_eq!(ed.bs().cursor, Pos { row: 0, col: 4 });
    }

    #[test]
    fn completion_nav_wraps_and_esc_closes() {
        let mut ed = test_ed("pri\n");
        ed.bs_mut().cursor = Pos { row: 0, col: 3 };
        ed.completion = Some(popup(vec![citem("print!", 3), citem("println!", 3)], 0, 0));
        press(&mut ed, KeyCode::Down, KeyModifiers::NONE);
        assert_eq!(ed.completion.as_ref().unwrap().sel, 1);
        assert_eq!(ed.bs().cursor.col, 3, "cursor must not move");
        press(&mut ed, KeyCode::Up, KeyModifiers::NONE);
        assert_eq!(ed.completion.as_ref().unwrap().sel, 0);
        press(&mut ed, KeyCode::Char('n'), KeyModifiers::CONTROL);
        assert_eq!(ed.completion.as_ref().unwrap().sel, 1);
        press(&mut ed, KeyCode::Esc, KeyModifiers::NONE);
        assert!(ed.completion.is_none());
    }

    #[test]
    fn completion_enter_accepts_not_newline() {
        let mut ed = test_ed("pri\n");
        ed.bs_mut().cursor = Pos { row: 0, col: 3 };
        ed.completion = Some(popup(vec![citem("print!", 3)], 0, 0));
        press(&mut ed, KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(lines(&ed), vec!["print!"]);
        assert_eq!(ed.bs().cursor.col, 6);
    }

    #[test]
    fn completion_typing_backspace_and_space_lifecycle() {
        // No LSP attached: the popup can't (re)open, but typing inside the
        // word and backspacing keep it alive; leaving the word closes it.
        let mut ed = test_ed("pr x\n");
        ed.bs_mut().cursor = Pos { row: 0, col: 2 };
        ed.completion = Some(popup(vec![citem("pr", 6)], 0, 0));
        press_text(&mut ed, "i");
        assert!(ed.completion.is_some(), "identifier char keeps popup");
        press(&mut ed, KeyCode::Backspace, KeyModifiers::NONE);
        assert!(ed.completion.is_some(), "backspace inside word keeps popup");
        press(&mut ed, KeyCode::Char(' '), KeyModifiers::NONE);
        assert!(ed.completion.is_none(), "space closes popup");
    }

    // ---------- jump to definition (M-. / M-,) ----------

    fn named_ed(text: &str, path: &str) -> Editor {
        let mut ed = test_ed(text);
        ed.bs_mut().buf.name = Some(std::path::PathBuf::from(path));
        ed
    }

    #[test]
    fn jump_definition_without_lsp_flashes() {
        let mut ed = test_ed("fn main() {}\n");
        press(&mut ed, KeyCode::Char('.'), KeyModifiers::ALT);
        assert_eq!(ed.status_text(), Some("No LSP server".to_string()));
        press(&mut ed, KeyCode::Char(','), KeyModifiers::ALT);
        assert_eq!(ed.status_text(), Some("No jump to return to".to_string()));
    }

    #[test]
    fn goto_location_same_file_pushes_stack_and_moves() {
        let p = "/tmp/rano_jump_same.rs";
        let mut ed = named_ed("fn a() {}\nfn b() {}\n", p);
        ed.bs_mut().cursor = Pos { row: 1, col: 3 };
        let loc = lsp::DefLocation {
            uri: lsp::path_to_uri(std::path::Path::new(p)),
            line: 0,
            character: 3,
        };
        ed.goto_location(loc, Pos { row: 1, col: 3 });
        assert_eq!(ed.bs().cursor, Pos { row: 0, col: 3 });
        assert_eq!(ed.def_back.len(), 1);
        assert!(ed.def_back[0].buf.is_none());
        assert_eq!(ed.def_back[0].idx, Some(0), "the origin buffer is recorded");
        // M-, returns to the origin row.
        press(&mut ed, KeyCode::Char(','), KeyModifiers::ALT);
        assert_eq!(ed.bs().cursor, Pos { row: 1, col: 3 });
        assert!(ed.def_back.is_empty());
    }

    #[test]
    fn goto_location_cross_file_swaps_and_back() {
        let target = std::env::temp_dir().join("rano_jump_target.rs");
        std::fs::write(&target, "fn target_fn() {}\n").unwrap();
        let mut ed = named_ed("fn a() {}\n", "/tmp/rano_jump_src.rs");
        ed.bs_mut().cursor = Pos { row: 0, col: 1 };
        let loc = lsp::DefLocation {
            uri: lsp::path_to_uri(&target),
            line: 0,
            character: 3,
        };
        ed.goto_location(loc, Pos { row: 0, col: 1 });
        assert_eq!(lines(&ed), vec!["fn target_fn() {}"]);
        assert_eq!(ed.bs().cursor, Pos { row: 0, col: 3 });
        // The origin buffer (with its edits) waits on the stack.
        let back = ed.def_back.pop().unwrap();
        let bs = back.buf.expect("single-buffer swap stores the state");
        assert_eq!(
            bs.buf
                .rows()
                .map(|l| l.iter().collect::<String>())
                .collect::<Vec<_>>(),
            vec!["fn a() {}"]
        );
        assert_eq!(back.pos, Pos { row: 0, col: 1 });
        std::fs::remove_file(&target).ok();
    }

    #[test]
    fn goto_location_multibuffer_keeps_origin_and_back_switches() {
        let target = std::env::temp_dir().join("rano_jump_mb.rs");
        std::fs::write(&target, "fn t() {}\n").unwrap();
        let mut ed = named_ed("fn a() {}\n", "/tmp/rano_jump_mb_src.rs");
        ed.config.multibuffer = true;
        let loc = lsp::DefLocation {
            uri: lsp::path_to_uri(&target),
            line: 0,
            character: 3,
        };
        ed.goto_location(loc, Pos { row: 0, col: 6 });
        assert_eq!(ed.cur, 1, "target opened as a new buffer");
        assert_eq!(lines(&ed), vec!["fn t() {}"]);
        press(&mut ed, KeyCode::Char(','), KeyModifiers::ALT);
        assert_eq!(ed.cur, 0);
        assert_eq!(lines(&ed), vec!["fn a() {}"]);
        assert_eq!(ed.bs().cursor, Pos { row: 0, col: 6 });
        std::fs::remove_file(&target).ok();
    }

    fn diag(line: usize, col: usize, end_col: usize, severity: u64) -> lsp::Diagnostic {
        lsp::Diagnostic {
            line,
            col,
            end_col,
            message: "m".to_string(),
            severity,
        }
    }

    #[test]
    fn diag_underline_style() {
        let mut ed = test_ed("fn main() {}\n");
        ed.bs_mut().lsp_diags = vec![diag(0, 0, 2, 1)];
        let s = ed.char_style_with(Pos { row: 0, col: 0 }, &ed.all_diags());
        assert_eq!(s.fg, Some(Color::Red));
        assert!(s.add_modifier.contains(Modifier::UNDERLINED));
        let s = ed.char_style_with(Pos { row: 0, col: 3 }, &ed.all_diags());
        assert_eq!(s.fg, None);
        assert!(!s.add_modifier.contains(Modifier::UNDERLINED));
        ed.bs_mut().lsp_diags = vec![diag(0, 0, 2, 2)];
        assert_eq!(
            ed.char_style_with(Pos { row: 0, col: 1 }, &ed.all_diags())
                .fg,
            Some(Color::Yellow)
        );
        ed.bs_mut().lsp_diags = vec![diag(0, 0, 2, 3)];
        assert_eq!(
            ed.char_style_with(Pos { row: 0, col: 1 }, &ed.all_diags())
                .fg,
            Some(Color::Blue)
        );
    }

    #[test]
    fn diag_style_priority() {
        let mut ed = test_ed("fn main() {}\n");
        ed.bs_mut().lsp_diags = vec![diag(0, 0, 5, 1)];
        ed.bs_mut().mark = Some(Pos { row: 0, col: 0 });
        ed.bs_mut().cursor = Pos { row: 0, col: 3 };
        assert_eq!(
            ed.char_style_with(Pos { row: 0, col: 1 }, &ed.all_diags()),
            Style::default().fg(Color::White).bg(Color::DarkGray)
        );
        ed.bs_mut().mark = None;
        ed.bs_mut().search.query = "fn".to_string();
        ed.bs_mut().search_matches = Some(vec![(Pos { row: 0, col: 0 }, 2)]);
        ed.bs_mut().search.current = 0;
        assert_eq!(
            ed.char_style_with(Pos { row: 0, col: 1 }, &ed.all_diags()),
            Style::default().fg(Color::Black).bg(Color::Yellow)
        );
    }

    // E5 — M-D walks the diagnostics top-down, wrapping past the last one.

    // Indentation: Tab follows the buffer's own indent style instead of
    // always inserting a literal tab char.

    #[test]
    fn tab_matches_space_indent() {
        let mut ed = test_ed("    a\n\n");
        ed.bs_mut().cursor = Pos { row: 1, col: 0 };
        press(&mut ed, KeyCode::Tab, KeyModifiers::NONE);
        assert_eq!(lines(&ed), vec!["    a", "    "]);
        assert_eq!(ed.bs().cursor.col, 4);
    }

    #[test]
    fn tab_aligns_to_next_unit_mid_line() {
        let mut ed = test_ed("    a\n  x");
        ed.bs_mut().cursor = Pos { row: 1, col: 2 };
        press(&mut ed, KeyCode::Tab, KeyModifiers::NONE);
        assert_eq!(lines(&ed), vec!["    a", "    x"]);
    }

    #[test]
    fn tab_uses_tab_char_when_file_does() {
        let mut ed = test_ed("\ta\n\tb\n\n");
        ed.bs_mut().cursor = Pos { row: 2, col: 0 };
        press(&mut ed, KeyCode::Tab, KeyModifiers::NONE);
        assert_eq!(lines(&ed), vec!["\ta", "\tb", "\t"]);
    }

    #[test]
    fn backspace_deletes_indent_run() {
        let mut ed = test_ed("    a\n    x");
        ed.bs_mut().cursor = Pos { row: 1, col: 4 };
        press(&mut ed, KeyCode::Backspace, KeyModifiers::NONE);
        assert_eq!(ed.bs().cursor.col, 0);
        assert_eq!(lines(&ed), vec!["    a", "x"]);
    }

    #[test]
    fn backspace_mid_text_still_one_char() {
        let mut ed = test_ed("    ab");
        ed.bs_mut().cursor = Pos { row: 0, col: 6 };
        press(&mut ed, KeyCode::Backspace, KeyModifiers::NONE);
        assert_eq!(lines(&ed), vec!["    a"]);
    }

    // Syntax errors surface without a language server (tree-sitter ERROR
    // nodes become diagnostics), and zero-width LSP ranges become visible.

    #[test]
    fn syntax_error_diag_without_lsp() {
        let mut buf = Buffer::new();
        buf.name = Some(std::path::PathBuf::from("t.rs"));
        buf.set_rows(vec!["fn main() {".chars().collect()]);
        let mut ed = Editor::new(buf, config::Config::default());
        ed.edit_invalidate();
        // Diagnostics are a WHOLE-document parse now, debounced the way the
        // LSP's `didChange` is — so the test does what the run loop does: the
        // frame's highlight (colour), then a flush past the debounce.
        ed.ensure_highlight();
        let after_the_pause = std::time::Instant::now() + std::time::Duration::from_millis(400);
        ed.diag_flush(after_the_pause);
        assert!(
            !ed.bs().syntax_diags.is_empty(),
            "unclosed fn block must yield a tree-sitter diagnostic"
        );
        ed.bs_mut()
            .buf
            .set_rows(vec!["fn main() {}".chars().collect()]);
        ed.edit_invalidate();
        ed.ensure_highlight();
        ed.diag_flush(after_the_pause);
        assert!(ed.bs().syntax_diags.is_empty(), "clean parse has no diags");
    }

    #[test]
    fn zero_width_diag_widened_to_one_column() {
        let lines = vec!["abcdefghij".chars().collect::<Vec<char>>()];
        let mut d = diag(0, 5, 5, 1);
        editor::widen_zero_width(std::slice::from_mut(&mut d), &lines);
        assert_eq!((d.col, d.end_col), (5, 6));
        let mut d = diag(0, 10, 10, 1); // insertion point at EOL
        editor::widen_zero_width(std::slice::from_mut(&mut d), &lines);
        assert_eq!((d.col, d.end_col), (9, 10));
    }

    #[test]
    fn jump_next_diag_includes_syntax_diags() {
        let mut buf = Buffer::new();
        buf.name = Some(std::path::PathBuf::from("t.rs"));
        buf.set_rows(vec!["fn broken(".chars().collect()]);
        let mut ed = Editor::new(buf, config::Config::default());
        ed.edit_invalidate();
        ed.jump_next_diag();
        assert_eq!(ed.bs().cursor.row, 0, "jumps to the tree-sitter error");
    }

    #[test]
    fn jump_next_diag_wraps() {
        let mut ed = test_ed("a\nb\nc\nd\ne");
        ed.bs_mut().lsp_diags = vec![diag(3, 0, 1, 1), diag(1, 0, 1, 2)];
        ed.bs_mut().cursor = Pos { row: 0, col: 0 };
        ed.jump_next_diag();
        assert_eq!(ed.bs().cursor, Pos { row: 1, col: 0 });
        ed.jump_next_diag();
        assert_eq!(ed.bs().cursor, Pos { row: 3, col: 0 });
        ed.jump_next_diag();
        assert_eq!(ed.bs().cursor, Pos { row: 1, col: 0 }, "wrap past the last");
        assert!(ed.bs().mark.is_none());
    }

    #[test]
    fn jump_next_diag_empty_flashes() {
        let mut ed = test_ed("a\nb");
        ed.bs_mut().cursor = Pos { row: 1, col: 0 };
        press(&mut ed, KeyCode::Char('d'), KeyModifiers::ALT);
        let st = ed.status_text().unwrap();
        assert!(st.contains("No diagnostics"), "status: {st}");
        assert_eq!(ed.bs().cursor, Pos { row: 1, col: 0 });
    }

    // D7 — exec is async: output lands below the spawn row as ONE undo
    // step; a failed command records no step and never edit_invalidates.

    fn poll_until(ed: &mut Editor, done: impl Fn(&Editor) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !done(ed) {
            assert!(Instant::now() < deadline, "job did not finish in time");
            std::thread::sleep(Duration::from_millis(10));
            ed.exec_poll();
        }
    }

    #[test]
    fn exec_async_inserts_output_one_undo() {
        let mut ed = test_ed("");
        ed.do_exec("printf hi");
        assert!(ed.bs().exec_job.is_some());
        poll_until(&mut ed, |ed| ed.bs().exec_job.is_none());
        assert_eq!(lines(&ed), vec!["", "hi"]);
        assert_eq!(ed.bs().cursor, Pos { row: 1, col: 0 });
        let st = ed.status_text().unwrap();
        assert!(st.contains("Ran: printf hi"), "status: {st}");
        press(&mut ed, KeyCode::Char('u'), KeyModifiers::ALT);
        assert_eq!(lines(&ed), vec![""]);
    }

    #[test]
    fn exec_failure_no_undo_step() {
        let mut ed = test_ed("");
        ed.do_exec("false");
        poll_until(&mut ed, |ed| ed.bs().exec_job.is_none());
        let st = ed.status_text().unwrap();
        assert!(st.contains("Exit code"), "status: {st}");
        assert_eq!(lines(&ed), vec![""]);
        assert!(ed.bs().undo.is_empty(), "failure must record no undo step");
        assert!(!ed.bs().buf.modified, "failure must not edit_invalidate");
        assert!(!ed.bs().lsp_dirty, "failure must not edit_invalidate");
    }

    #[test]
    fn status_shows_running_command() {
        let mut ed = test_ed("");
        ed.do_exec("sleep 0.3");
        ed.status = None; // skip the spawn flash; exercise the exec_job arm
        let st = ed.status_text().unwrap();
        assert!(st.contains("Running: sleep 0.3"), "status: {st}");
        poll_until(&mut ed, |ed| ed.bs().exec_job.is_none());
    }

    // E1 — bracketed paste: one undo step, \r stripped, cursor at the end.

    #[test]
    fn paste_text_single_and_multiline() {
        let mut ed = test_ed("");
        ed.paste_text("ab\ncd");
        assert_eq!(lines(&ed), vec!["ab", "cd"]);
        assert_eq!(ed.bs().cursor, Pos { row: 1, col: 2 });
        let mut ed = test_ed("x");
        ed.paste_text("q");
        assert_eq!(lines(&ed), vec!["qx"]);
        assert_eq!(ed.bs().cursor, Pos { row: 0, col: 1 });
    }

    #[test]
    fn paste_text_one_undo() {
        let mut ed = test_ed("");
        ed.paste_text("ab\ncd");
        press(&mut ed, KeyCode::Char('u'), KeyModifiers::ALT);
        assert_eq!(lines(&ed), vec![""]);
    }

    #[test]
    fn paste_text_strips_cr() {
        let mut ed = test_ed("");
        ed.paste_text("x\r\ny");
        assert_eq!(lines(&ed), vec!["x", "y"]);
    }

    // F6 — M-| filters the selected rows through an external command.

    #[test]
    fn filter_region_uppercases() {
        let mut ed = test_ed("hello\nworld");
        ed.bs_mut().cursor = Pos { row: 0, col: 5 };
        ed.bs_mut().mark = Some(Pos { row: 0, col: 0 });
        press(&mut ed, KeyCode::Char('|'), KeyModifiers::ALT);
        assert!(ed.prompt.is_some());
        press_text(&mut ed, "tr a-z A-Z");
        press(&mut ed, KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(lines(&ed), vec!["HELLO", "world"]);
        assert_eq!(ed.bs().cursor, Pos { row: 0, col: 0 });
        assert!(ed.bs().mark.is_none());
        press(&mut ed, KeyCode::Char('u'), KeyModifiers::ALT);
        assert_eq!(lines(&ed), vec!["hello", "world"]);
    }

    #[test]
    fn filter_no_selection_flashes() {
        let mut ed = test_ed("hello");
        press(&mut ed, KeyCode::Char('|'), KeyModifiers::ALT);
        let st = ed.status_text().unwrap();
        assert!(st.contains("No selection"), "status: {st}");
        assert!(ed.prompt.is_none());
    }

    #[test]
    fn filter_failure_no_undo() {
        let mut ed = test_ed("hello\nworld");
        ed.bs_mut().cursor = Pos { row: 0, col: 5 };
        ed.bs_mut().mark = Some(Pos { row: 0, col: 0 });
        press(&mut ed, KeyCode::Char('|'), KeyModifiers::ALT);
        press_text(&mut ed, "false");
        press(&mut ed, KeyCode::Enter, KeyModifiers::NONE);
        let st = ed.status_text().unwrap();
        assert!(st.contains("Exit code"), "status: {st}");
        assert_eq!(lines(&ed), vec!["hello", "world"]);
        assert_eq!(ed.bs().mark, Some(Pos { row: 0, col: 0 }), "mark unchanged");
        assert!(ed.bs().undo.is_empty(), "failure must record no undo step");
    }

    // E5 — M-D is next-diagnostic; word motions moved to Alt+Left/Right.

    #[test]
    fn alt_d_jumps_diag_alt_arrows_words() {
        let mut ed = test_ed("ab cd ef");
        ed.bs_mut().lsp_diags = vec![diag(0, 3, 5, 1)];
        press(&mut ed, KeyCode::Char('d'), KeyModifiers::ALT);
        assert_eq!(ed.bs().cursor, Pos { row: 0, col: 3 });
        let mut ed = test_ed("ab cd");
        press(&mut ed, KeyCode::Right, KeyModifiers::ALT);
        assert_eq!(ed.bs().cursor, Pos { row: 0, col: 2 });
        press(&mut ed, KeyCode::Left, KeyModifiers::ALT);
        assert_eq!(ed.bs().cursor, Pos { row: 0, col: 0 });
    }

    // E3 — horizontal scroll follows the cursor in display cols.

    #[test]
    fn scroll_x_follows_cursor() {
        let mut ed = test_ed(&"a".repeat(40));
        ed.wrap = false; // horizontal scrolling needs wrap off
        ed.show_line_numbers = false;
        ed.text_w = 10;
        press(&mut ed, KeyCode::End, KeyModifiers::NONE);
        ed.adjust_scroll_x();
        assert_eq!(ed.bs().scroll_x, 31);
        press(&mut ed, KeyCode::Home, KeyModifiers::NONE);
        ed.adjust_scroll_x();
        assert_eq!(ed.bs().scroll_x, 0);
    }

    #[test]
    fn scroll_x_with_tabs() {
        // cursor col 3 → display col 9; 9 >= 0 + 4 → scroll_x = 9 + 1 - 4
        let mut ed = test_ed("a\tb");
        ed.wrap = false; // horizontal scrolling needs wrap off
        ed.show_line_numbers = false;
        ed.text_w = 4;
        press(&mut ed, KeyCode::End, KeyModifiers::NONE);
        ed.adjust_scroll_x();
        assert_eq!(ed.bs().scroll_x, 6);
    }

    // F4 — M-N toggles the line-number gutter.

    #[test]
    fn m_n_toggles_line_numbers() {
        let mut ed = test_ed("hi");
        assert!(ed.show_line_numbers, "line numbers default to on");
        press(&mut ed, KeyCode::Char('n'), KeyModifiers::ALT);
        assert!(!ed.show_line_numbers);
        press(&mut ed, KeyCode::Char('n'), KeyModifiers::ALT);
        assert!(ed.show_line_numbers);
    }

    // M-\ — soft line wrap: scroll, motion and mouse work in VISUAL rows.

    #[test]
    fn m_backslash_toggles_wrap() {
        let mut ed = test_ed("hi");
        assert!(ed.wrap, "soft wrap defaults to on (nano)");
        press(&mut ed, KeyCode::Char('\\'), KeyModifiers::ALT);
        assert!(!ed.wrap);
        press(&mut ed, KeyCode::Char('\\'), KeyModifiers::ALT);
        assert!(ed.wrap);
    }

    #[test]
    fn wrap_table_and_motion_handle_wide_characters() {
        // 5 CJK characters = 10 display columns in a 4-column view: three
        // visual rows of 2, 2 and 1 characters. Nothing may split a glyph,
        // and the cursor's column must be measured in cells, not characters.
        let mut ed = test_ed("中文字语言\nx");
        ed.show_line_numbers = false;
        ed.text_w = 4;
        ed.ensure_wrap_prefix();
        assert_eq!(
            ed.bs().wrap_prefix,
            vec![0, 3, 4],
            "10 columns / 4 per row = 3 visual rows"
        );
        assert_eq!(ed.seg_count(0), 3);
        // Segments begin at characters 0, 2 and 4 — and at display columns
        // 0, 4 and 8, which is what the renderer paints from.
        assert_eq!(ed.seg_chars(0, 0), (0, 2));
        assert_eq!(ed.seg_chars(0, 1), (2, 4));
        assert_eq!(ed.seg_chars(0, 2), (4, 5));
        assert_eq!(ed.seg_disp(0, 0), (0, 4));
        assert_eq!(ed.seg_disp(0, 1), (4, 8));
        assert_eq!(ed.seg_disp(0, 2), (8, 10));
        // A char col maps to its visual row and to its offset inside it.
        assert_eq!(ed.visual_pos(Pos { row: 0, col: 0 }), 0);
        assert_eq!(ed.visual_pos(Pos { row: 0, col: 1 }), 0);
        assert_eq!(ed.visual_pos(Pos { row: 0, col: 2 }), 1);
        assert_eq!(ed.visual_pos(Pos { row: 0, col: 4 }), 2);
        assert_eq!(ed.disp_in_seg(0, 0), 0);
        assert_eq!(ed.disp_in_seg(0, 1), 2, "one glyph in, on row 0");
        assert_eq!(ed.disp_in_seg(0, 4), 0, "start of the third row");
        // End goes to the end of the VISUAL row — a cluster boundary, never
        // the middle of a glyph — and Down carries that offset across.
        ed.bs_mut().cursor = Pos { row: 0, col: 0 };
        press(&mut ed, KeyCode::End, KeyModifiers::NONE);
        assert_eq!(ed.bs().cursor, Pos { row: 0, col: 2 });
        press(&mut ed, KeyCode::Home, KeyModifiers::NONE);
        ed.bs_mut().cursor = Pos { row: 0, col: 1 };
        press(&mut ed, KeyCode::Down, KeyModifiers::NONE);
        assert_eq!(
            ed.bs().cursor,
            Pos { row: 0, col: 3 },
            "one glyph in on the row below"
        );
    }

    #[test]
    fn mouse_pos_maps_wide_columns_to_the_clicked_character() {
        let mut ed = test_ed("中文字语言\nx");
        ed.show_line_numbers = false;
        ed.text_w = 4;
        ed.ensure_wrap_prefix();
        // Pane column 1 is the left cell of 中; column 3 is inside 文.
        assert!(ed.handle_mouse(me(MouseEventKind::Down(MouseButton::Left), 1, 1)));
        assert_eq!(ed.bs().cursor, Pos { row: 0, col: 0 });
        assert!(ed.handle_mouse(me(MouseEventKind::Down(MouseButton::Left), 1, 3)));
        assert_eq!(ed.bs().cursor, Pos { row: 0, col: 1 });
        // Pane row 2 is the second visual row: its column 3 is inside 语.
        assert!(ed.handle_mouse(me(MouseEventKind::Down(MouseButton::Left), 2, 3)));
        assert_eq!(ed.bs().cursor, Pos { row: 0, col: 3 });
        // Past the last cell of the final row → end of line.
        assert!(ed.handle_mouse(me(MouseEventKind::Down(MouseButton::Left), 3, 4)));
        assert_eq!(ed.bs().cursor, Pos { row: 0, col: 5 });
    }

    #[test]
    fn wheel_scrolls_wide_rows_by_visual_row() {
        // 5 CJK characters in a 4-column view is 3 visual rows; a wheel
        // notch must move three of them, not three characters.
        let mut ed = test_ed("中文字语言\nx");
        ed.show_line_numbers = false;
        ed.text_w = 4;
        ed.text_h = 1; // otherwise the sheet fits and there is nowhere to scroll
        ed.ensure_wrap_prefix();
        assert!(ed.handle_mouse(me(MouseEventKind::ScrollDown, 0, 0)));
        assert_eq!(ed.bs().scroll, 3, "one buffer row is three visual rows");
        // The cursor is pinned onto the edge it would have crossed.
        assert_eq!(ed.bs().cursor, Pos { row: 1, col: 0 });
    }

    #[test]
    fn wrap_prefix_counts_visual_rows() {
        // view_w 10: row 0 (25 cols) → 3 visual rows, row 1 (5) → 1,
        // row 2 (15) → 2.
        let mut ed = test_ed(&format!(
            "{}\n{}\n{}",
            "a".repeat(25),
            "b".repeat(5),
            "c".repeat(15)
        ));
        ed.show_line_numbers = false;
        ed.text_w = 10;
        ed.ensure_wrap_prefix();
        assert_eq!(ed.bs().wrap_prefix, vec![0, 3, 4, 6]);
        assert_eq!(ed.visual_pos(Pos { row: 0, col: 0 }), 0);
        assert_eq!(ed.visual_pos(Pos { row: 0, col: 9 }), 0);
        assert_eq!(ed.visual_pos(Pos { row: 0, col: 10 }), 1);
        assert_eq!(ed.visual_pos(Pos { row: 2, col: 14 }), 5);
        assert_eq!(ed.buf_row_of_visual(2), (0, 2));
        assert_eq!(ed.buf_row_of_visual(3), (1, 0));
        assert_eq!(ed.buf_row_of_visual(5), (2, 1));
        assert_eq!(
            ed.buf_row_of_visual(99),
            (2, 1),
            "clamped to the last visual row"
        );
    }

    #[test]
    fn wrap_prefix_rebuilds_on_edit() {
        let mut ed = test_ed("aaaa");
        ed.show_line_numbers = false;
        ed.text_w = 5;
        ed.ensure_wrap_prefix();
        assert_eq!(ed.bs().wrap_prefix, vec![0, 1]);
        ed.bs_mut().cursor = Pos { row: 0, col: 4 };
        ed.insert_char('b'); // "aaaab" — 5 cols, still one visual row
        ed.insert_char('c'); // "aaaabc" — 6 cols, two visual rows
        ed.ensure_wrap_prefix();
        assert_eq!(ed.bs().wrap_prefix, vec![0, 2]);
    }

    // ---------- the command line ----------

    fn argv(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_flag_value_is_not_mistaken_for_the_file() {
        // The bug the positional `args.first()` had: `--line 42 main.rs` made
        // "42" the file.
        let a = parse_args(&argv(&["--line", "42", "main.rs"])).expect("parse");
        assert_eq!(a.file(), Some("main.rs"));
        assert_eq!(a.line, Some(42));
        assert_eq!(a.col, None);
        // And in the other order.
        let a = parse_args(&argv(&["main.rs", "--line", "42"])).expect("parse");
        assert_eq!(a.file(), Some("main.rs"));
        assert_eq!(a.line, Some(42));
    }

    #[test]
    fn the_value_forms_are_accepted() {
        let a = parse_args(&argv(&["--line=42", "--col=7", "f.rs"])).expect("equals form");
        assert_eq!((a.line, a.col, a.file()), (Some(42), Some(7), Some("f.rs")));
        let a = parse_args(&argv(&["-l", "42", "-c", "7", "f.rs"])).expect("short form");
        assert_eq!((a.line, a.col), (Some(42), Some(7)));
        let a = parse_args(&argv(&["--column", "3", "f.rs"])).expect("long column");
        assert_eq!(
            (a.line, a.col),
            (None, Some(3)),
            "a column alone leaves the row at 0"
        );
    }

    #[test]
    fn bad_arguments_are_refused_rather_than_guessed() {
        // Each of these is a mistake the user wants told about, not silently
        // reinterpreted.
        for (args, why) in [
            (argv(&["--line"]), "a flag with no value"),
            (argv(&["--line", "abc"]), "a non-number"),
            (argv(&["--line", "-1"]), "a negative line"),
            (argv(&["--nope", "f.rs"]), "an unknown option"),
            (
                argv(&["--export", "html", "a.rs", "b.rs"]),
                "two files to export",
            ),
            (argv(&["--export", "pdf", "f.rs"]), "an unknown format"),
        ] {
            assert!(parse_args(&args).is_err(), "{why}: {args:?} was accepted");
        }
    }

    #[test]
    fn help_and_version_win_over_a_file() {
        // `rano --help file.rs` prints usage rather than opening the file.
        let a = parse_args(&argv(&["--help", "file.rs"])).expect("parse");
        assert!(a.help && a.file().is_some());
        let a = parse_args(&argv(&["-V"])).expect("parse");
        assert!(a.version);
    }

    // ---------- opening at a position ----------

    fn ed_with_lines(n: usize) -> Editor {
        let text: String = (1..=n).map(|i| format!("line {i}\n")).collect();
        test_ed(&text)
    }

    #[test]
    fn a_startup_position_is_centred() {
        // 500 lines in a 20-row viewport. The target is a line with room above
        // AND below it, which is what makes centring mean anything — with the
        // file's end in view the clamp wins instead, which
        // `centring_clamps_at_both_ends` pins.
        let mut ed = ed_with_lines(500);
        ed.text_h = 20;
        ed.show_line_numbers = false;
        ed.text_w = 40;
        ed.startup_pos = Some(Pos { row: 249, col: 0 });
        ed.ensure_wrap_prefix();
        ed.apply_startup_pos();
        assert_eq!(ed.bs().cursor, Pos { row: 249, col: 0 });
        assert_eq!(ed.startup_pos, None, "applied once");
        // Centred: text_h/2 = 10 rows above, so the target is on pane row 11.
        assert_eq!(ed.bs().scroll, 239, "249 - text_h/2");
        let vis = ed.visual_pos(Pos { row: 249, col: 0 });
        assert_eq!(
            vis - ed.bs().scroll,
            10,
            "the target's offset into the viewport"
        );
    }

    #[test]
    fn centring_clamps_at_both_ends() {
        // Near the start there is nothing above to show, so the target is not
        // centred — it is as centred as the file allows, which is the top.
        let mut ed = ed_with_lines(100);
        ed.text_h = 20;
        ed.startup_pos = Some(Pos { row: 1, col: 0 });
        ed.ensure_wrap_prefix();
        ed.apply_startup_pos();
        assert_eq!(ed.bs().scroll, 0, "no blank space above the first line");
        // And near the end, the last screenful.
        let mut ed = ed_with_lines(100);
        ed.text_h = 20;
        ed.startup_pos = Some(Pos { row: 99, col: 0 });
        ed.ensure_wrap_prefix();
        ed.apply_startup_pos();
        assert_eq!(ed.bs().scroll, 80, "100 rows - 20 visible");
    }

    #[test]
    fn centring_counts_visual_rows_when_wrapped() {
        // One long line above the target occupies several visual rows, and the
        // scroll is counted in those — so a buffer row and a visual row differ
        // and the centring has to use the visual one.
        // A long line first, then enough short ones that the target can
        // actually be centred (there has to be something below it).
        let tail: String = (0..40).map(|i| format!("tail {i}\n")).collect();
        let mut ed = test_ed(&format!("{}\ntarget\n{tail}", "x".repeat(120)));
        ed.text_h = 10;
        ed.text_w = 20;
        ed.show_line_numbers = false;
        ed.ensure_wrap_prefix();
        let seg = ed.seg_count(0);
        assert!(seg >= 6, "the long line wraps into {seg} rows");
        ed.startup_pos = Some(Pos { row: 20, col: 0 });
        ed.apply_startup_pos();
        let vis = ed.visual_pos(Pos { row: 20, col: 0 });
        // The target's BUFFER row is 20, but its VISUAL row is much further down
        // because the long line above it occupies `seg` rows. Centring uses the
        // visual one — using the buffer row would put the view `seg` rows off.
        assert_eq!(
            vis,
            seg + 19,
            "the long line's segments plus the rows between"
        );
        assert_eq!(ed.bs().scroll, vis - 5, "centred in VISUAL rows");
    }

    #[test]
    fn a_position_waits_for_its_row_to_arrive() {
        // With the loader a row a million lines in arrives long after the first
        // frame. Clamping to what had arrived would open the file at the wrong
        // place and look like the feature was broken, so it waits.
        let mut ed = ed_with_lines(10);
        ed.text_h = 10;
        ed.startup_pos = Some(Pos {
            row: 999_999,
            col: 0,
        });
        ed.apply_startup_pos();
        assert_eq!(ed.bs().cursor, Pos { row: 0, col: 0 }, "not moved");
        assert_eq!(
            ed.startup_pos,
            Some(Pos {
                row: 999_999,
                col: 0
            }),
            "still pending"
        );
        // Now the row arrives.
        ed.bs_mut()
            .buf
            .extend_rows((0..1_000_000).map(|i| format!("r{i}").chars().collect()));
        ed.ensure_wrap_prefix();
        ed.apply_startup_pos();
        assert_eq!(ed.bs().cursor.row, 999_999);
        assert_eq!(ed.startup_pos, None);
        assert_eq!(
            ed.bs().scroll,
            999_999 - 5,
            "centred once the row was there"
        );
    }

    #[test]
    fn a_column_is_clamped_to_the_line() {
        let mut ed = ed_with_lines(10);
        ed.text_h = 10;
        ed.startup_pos = Some(Pos { row: 2, col: 999 });
        ed.ensure_wrap_prefix();
        ed.apply_startup_pos();
        assert_eq!(
            ed.bs().cursor,
            Pos { row: 2, col: 6 },
            "\"line 3\" is 6 chars"
        );
    }

    #[test]
    fn no_position_leaves_the_editor_alone() {
        let mut ed = ed_with_lines(50);
        ed.text_h = 10;
        ed.apply_startup_pos();
        assert_eq!(ed.bs().cursor, Pos { row: 0, col: 0 });
        assert_eq!(ed.bs().scroll, 0);
    }

    #[test]
    fn the_extended_wrap_table_agrees_with_a_full_rebuild() {
        // The loader APPENDS rows, and the table is extended from the join
        // rather than rebuilt. A wrong prefix is wrong scrolling, so this drives
        // both paths over the same sequence of appends and compares — the same
        // form as the single-row test above, for the same reason.
        let mut ed = test_ed("seed");
        ed.show_line_numbers = false;
        ed.text_w = 12;
        ed.ensure_wrap_prefix();

        for batch in 0..8 {
            let was = ed.bs().buf.row_count();
            let added: Vec<Vec<char>> = (0..7)
                .map(|i| {
                    format!("b{batch}r{i}{}", "x".repeat(i * 5))
                        .chars()
                        .collect()
                })
                .collect();
            ed.bs_mut().buf.extend_rows(added);
            // What the loader does: only the first batch overlaps the seed.
            ed.bs_mut().wrap_extend_from = Some(if was <= 1 { 0 } else { was });
            ed.bs_mut().edit_gen = ed.bs().edit_gen.wrapping_add(1);
            ed.ensure_wrap_prefix();
            let extended_prefix = ed.bs().wrap_prefix.clone();
            let extended_rows = ed.bs().wrap_rows.clone();

            // Now the same buffer, forced down the full-rebuild path.
            ed.bs_mut().wrap_extend_from = None;
            ed.bs_mut().wrap_lines = 0;
            ed.bs_mut().wrap_dirty_row = None;
            ed.ensure_wrap_prefix();
            assert_eq!(
                ed.bs().wrap_prefix,
                extended_prefix,
                "batch {batch}: the extended table disagrees with a full rebuild"
            );
            assert_eq!(
                ed.bs().wrap_rows,
                extended_rows,
                "batch {batch}: row geometry differs"
            );
            assert_eq!(
                ed.bs().wrap_prefix.len(),
                ed.bs().buf.row_count() + 1,
                "batch {batch}: prefix length"
            );
        }
    }

    #[test]
    fn the_incremental_wrap_table_agrees_with_a_full_rebuild() {
        // Typing re-measures ONE row and re-sums the rest arithmetically; a
        // multi-row edit rebuilds everything. The two must not disagree, so
        // this drives both and compares — the fast path is only ever a
        // shorter way to compute the same table.
        let text: String = (0..40)
            .map(|i| format!("{}{}\n", i, "x".repeat(i * 3)))
            .collect();
        let mut ed = test_ed(&text);
        ed.show_line_numbers = false;
        ed.text_w = 12;
        ed.ensure_wrap_prefix();

        for (row, extra) in [(0usize, 30usize), (17, 40), (39, 60)] {
            ed.bs_mut().cursor = Pos { row, col: 0 };
            for _ in 0..extra {
                ed.insert_char('y');
            }
            ed.ensure_wrap_prefix();
            let incremental = ed.bs().wrap_prefix.clone();
            let rows = ed.bs().wrap_rows.clone();
            // The same buffer, forced down the full-rebuild path.
            ed.bs_mut().wrap_dirty_row = None;
            ed.bs_mut().wrap_lines = 0;
            ed.ensure_wrap_prefix();
            assert_eq!(
                ed.bs().wrap_prefix,
                incremental,
                "row {row}: the incremental table disagrees with a full rebuild"
            );
            assert_eq!(ed.bs().wrap_rows, rows, "row {row}: row geometry differs");
        }
    }

    #[test]
    fn a_row_that_stops_wrapping_is_still_measured_once() {
        // The dirty-row path must notice a row that shrinks back under the
        // viewport, not just one that grows past it.
        let mut ed = test_ed(&format!("{}\nshort", "a".repeat(30)));
        ed.show_line_numbers = false;
        ed.text_w = 10;
        ed.ensure_wrap_prefix();
        assert_eq!(
            ed.bs().wrap_prefix,
            vec![0, 3, 4],
            "30 cols → 3 visual rows"
        );
        ed.bs_mut().cursor = Pos { row: 0, col: 30 };
        for _ in 0..25 {
            press(&mut ed, KeyCode::Backspace, KeyModifiers::NONE);
        }
        ed.ensure_wrap_prefix();
        assert_eq!(ed.bs().wrap_prefix, vec![0, 1, 2], "5 cols → 1 visual row");
    }

    /// **Scrolling must re-highlight.** The style grid is a WINDOW over the
    /// viewport, and until now nothing recomputed it when the view scrolled:
    /// `highlight_dirty` is set by edits and loads and by no scroll path, so
    /// `ensure_highlight` returned early for ever after the first frame and every
    /// newly revealed row was drawn plain.
    ///
    /// Found by comparing the operator's window (rano's own `TODO.md`, scrolled
    /// to line 229) against a fresh open of the same file: everything above
    /// ~row 260 was coloured, everything below was plain, and the boundary was
    /// `first viewport + text_h + HIGHLIGHT_MARGIN`.
    #[test]
    fn scrolling_far_re_highlights_the_new_viewport() {
        let text: String = (0..600)
            .map(|i| format!("## Section {i}\n\n- [ ] task {i}\n\n"))
            .collect();
        let mut ed = named_ed(&text, "TODO.md");
        ed.show_line_numbers = false;
        ed.text_w = 60;
        ed.text_h = 20;
        ed.ensure_wrap_prefix();
        // The frame at the top of the file.
        ed.ensure_highlight();
        // Row 4 is `## Section 1` — the `##` is a keyword, so an answer here is
        // the grid actually being built. (`row 5` is the blank line after it,
        // where `None` is the correct answer and asserting on it would have been
        // a test that could not fail.)
        assert!(
            ed.bs().hl.style_at(Pos { row: 4, col: 0 }).is_some(),
            "the first viewport is highlighted"
        );

        // Scroll a long way WITHOUT touching the buffer, which is what PgDn and
        // the wheel do — no edit, so `highlight_dirty` stays false.
        let far = 1500;
        ed.bs_mut().scroll = far;
        ed.bs_mut().cursor = Pos { row: far, col: 0 };
        ed.ensure_wrap_prefix();
        ed.ensure_highlight();

        // The window must cover the viewport. This is the assertion that fails
        // without the fix: the window would still be the one built for the top.
        let win = ed
            .bs()
            .hl
            .styled_window()
            .expect("a window, not a whole-document refresh");
        assert!(
            win.rows.0 <= far && far + ed.text_h <= win.rows.1 + 1,
            "the highlight window {win:?} does not cover the viewport at \
             {far}..{} — those rows would be drawn plain",
            far + ed.text_h
        );
    }

    #[test]
    fn an_edit_highlights_on_the_frame_not_per_key() {
        // The open cost, and the reason a burst of typing costs one highlight
        // rather than one per keystroke. A big buffer is highlighted by
        // viewport window and only when a frame asks for it, so nothing is
        // parsed at open and nothing is parsed by the edit itself.
        let text: String = (0..60_000)
            .map(|i| format!("pub fn f{i}(x: usize) -> usize {{ x + {i} }}\n"))
            .collect();
        assert!(text.len() > 2 << 20, "{} bytes", text.len());
        let mut ed = test_ed(&text);
        // A name, so the highlighter has a language at all (`detect` works
        // from the name and the shebang).
        ed.bs_mut().buf.name = Some(std::path::PathBuf::from("big.rs"));
        ed.show_line_numbers = false;
        ed.text_w = 100;
        ed.text_h = 40;

        // Opening parses nothing: the grid is empty until a frame asks.
        assert_eq!(
            ed.bs().hl.styled_window(),
            None,
            "open must not highlight a whole big file"
        );
        assert_eq!(ed.bs().hl.style_at(Pos { row: 5, col: 0 }), None);

        // The frame's call: a viewport window, not the document.
        ed.ensure_highlight();
        let win = ed
            .bs()
            .hl
            .styled_window()
            .expect("a window, not the whole buffer");
        let rows = win.rows.1 - win.rows.0;
        assert!(
            rows < 500,
            "the window must be the viewport plus a margin, got {rows} rows of {}",
            ed.bs().buf.row_count()
        );
        assert!(ed.bs().hl.style_at(Pos { row: 5, col: 0 }).is_some());

        // An edit marks the grid stale and does NOT re-highlight; the next
        // frame does, once, however many keys arrived in between.
        ed.bs_mut().cursor = Pos { row: 3, col: 0 };
        for _ in 0..5 {
            ed.insert_char('y');
        }
        // Five keystrokes, one highlight — and it happens when asked.
        ed.ensure_highlight();
        assert!(ed.bs().hl.styled_window().is_some());
        // Idempotent: asking twice is free and changes nothing.
        let before = ed.bs().hl.styled_window();
        ed.ensure_highlight();
        assert_eq!(ed.bs().hl.styled_window(), before);
    }

    #[test]
    fn wrap_disables_horizontal_scroll() {
        let mut ed = test_ed(&"a".repeat(40));
        ed.show_line_numbers = false;
        ed.text_w = 10;
        ed.bs_mut().cursor = Pos { row: 0, col: 40 };
        ed.adjust_scroll_x();
        assert_eq!(
            ed.bs().scroll_x,
            0,
            "wrap on: lines wrap, no sideways scroll"
        );
        ed.wrap = false;
        ed.adjust_scroll_x();
        assert_eq!(ed.bs().scroll_x, 31);
    }

    #[test]
    fn adjust_scroll_keeps_cursor_visible_with_wrap() {
        // 10 rows × 30 cols, view 10 wide → 3 visual rows per row, 30 total.
        let text: String = (0..10)
            .map(|i| format!("{}{}\n", i, "a".repeat(29)))
            .collect();
        let mut ed = test_ed(&text);
        ed.show_line_numbers = false;
        ed.text_w = 10;
        ed.text_h = 6;
        ed.bs_mut().cursor = Pos { row: 5, col: 29 }; // visual row 5*3 + 2 = 17
        ed.adjust_scroll(ed.text_h);
        // max_scroll = 30 - 6 = 24; scroll = 17 - 6 + 1 = 12
        assert_eq!(ed.bs().scroll, 12);
    }

    #[test]
    fn wheel_scrolls_visual_rows_with_wrap() {
        let text: String = (0..10)
            .map(|i| format!("{}{}\n", i, "a".repeat(29)))
            .collect();
        let mut ed = test_ed(&text);
        ed.show_line_numbers = false;
        ed.text_w = 10;
        ed.text_h = 6;
        // 30 visual rows; the cursor (visual 0) is pinned to the top edge.
        assert!(ed.handle_mouse(me(MouseEventKind::ScrollDown, 0, 0)));
        assert_eq!(ed.bs().scroll, 3);
        assert_eq!(
            ed.bs().cursor,
            Pos { row: 1, col: 0 },
            "pinned to the viewport top"
        );
        assert!(ed.handle_mouse(me(MouseEventKind::ScrollDown, 0, 0)));
        assert_eq!(ed.bs().scroll, 6);
        assert_eq!(ed.bs().cursor, Pos { row: 2, col: 0 });
    }

    #[test]
    fn move_up_down_cross_wrap_segments() {
        let mut ed = test_ed(&format!("{}\n{}", "a".repeat(25), "b".repeat(5)));
        ed.show_line_numbers = false;
        ed.text_w = 10;
        // row 0 occupies visual rows 0..3; the cursor starts on row 1 (visual 3).
        ed.bs_mut().cursor = Pos { row: 1, col: 2 };
        press(&mut ed, KeyCode::Up, KeyModifiers::NONE);
        assert_eq!(
            ed.bs().cursor,
            Pos { row: 0, col: 22 },
            "up: same col on the visual row above"
        );
        press(&mut ed, KeyCode::Up, KeyModifiers::NONE);
        assert_eq!(ed.bs().cursor, Pos { row: 0, col: 12 });
        press(&mut ed, KeyCode::Up, KeyModifiers::NONE);
        assert_eq!(ed.bs().cursor, Pos { row: 0, col: 2 });
        press(&mut ed, KeyCode::Up, KeyModifiers::NONE);
        assert_eq!(
            ed.bs().cursor,
            Pos { row: 0, col: 2 },
            "top of the buffer: no move"
        );
        press(&mut ed, KeyCode::Down, KeyModifiers::NONE);
        assert_eq!(ed.bs().cursor, Pos { row: 0, col: 12 });
        press(&mut ed, KeyCode::Down, KeyModifiers::NONE);
        assert_eq!(ed.bs().cursor, Pos { row: 0, col: 22 });
        press(&mut ed, KeyCode::Down, KeyModifiers::NONE);
        assert_eq!(
            ed.bs().cursor,
            Pos { row: 1, col: 2 },
            "down: crosses into the next buffer row"
        );
    }

    #[test]
    fn home_end_use_visual_rows_with_wrap() {
        let mut ed = test_ed(&"a".repeat(25));
        ed.show_line_numbers = false;
        ed.text_w = 10;
        ed.bs_mut().cursor = Pos { row: 0, col: 15 }; // visual row 1
        press(&mut ed, KeyCode::Home, KeyModifiers::NONE);
        assert_eq!(
            ed.bs().cursor,
            Pos { row: 0, col: 10 },
            "Home = start of the visual row"
        );
        press(&mut ed, KeyCode::End, KeyModifiers::NONE);
        assert_eq!(
            ed.bs().cursor,
            Pos { row: 0, col: 20 },
            "End = end of the visual row"
        );
    }

    #[test]
    fn page_keys_move_visual_rows_with_wrap() {
        let text: String = (0..10)
            .map(|i| format!("{}{}\n", i, "a".repeat(29)))
            .collect();
        let mut ed = test_ed(&text);
        ed.show_line_numbers = false;
        ed.text_w = 10;
        ed.text_h = 6;
        ed.bs_mut().cursor = Pos { row: 5, col: 0 }; // visual row 15
        press(&mut ed, KeyCode::PageUp, KeyModifiers::NONE);
        // step 5 → visual row 10 = row 3, segment 1, col 0 → char col 10
        assert_eq!(ed.bs().cursor, Pos { row: 3, col: 10 });
        press(&mut ed, KeyCode::PageDown, KeyModifiers::NONE);
        // back to visual row 15 = row 5, segment 0
        assert_eq!(ed.bs().cursor, Pos { row: 5, col: 0 });
    }

    #[test]
    fn mouse_pos_maps_visual_rows_with_wrap() {
        let mut ed = test_ed(&format!("{}\n{}", "a".repeat(25), "b".repeat(5)));
        ed.show_line_numbers = false;
        ed.text_w = 10;
        ed.text_h = 10;
        // pane row 2 = visual row 1 = row 0, segment 1; col 3 → display col 13.
        assert!(ed.handle_mouse(me(MouseEventKind::Down(MouseButton::Left), 2, 3)));
        assert_eq!(ed.bs().cursor, Pos { row: 0, col: 13 });
        // pane row 4 = visual row 3 = row 1, segment 0.
        assert!(ed.handle_mouse(me(MouseEventKind::Down(MouseButton::Left), 4, 2)));
        assert_eq!(ed.bs().cursor, Pos { row: 1, col: 2 });
    }

    // D6 — tick_status reports whether it cleared visible state.

    #[test]
    fn tick_status_reports_clear() {
        let mut ed = test_ed("hi");
        ed.status = Some(Flash {
            text: "x".to_string(),
            until: Instant::now() - Duration::from_secs(1),
        });
        assert!(ed.tick_status());
        assert!(ed.status.is_none());
        assert!(!ed.tick_status());
    }

    // F1 — Editor::new takes a Config; tab_width/line_numbers seed from it.

    #[test]
    fn config_tab_width_used() {
        let mut buf = Buffer::new();
        buf.set_rows(vec!["a\tb".chars().collect()]);
        let cfg = config::Config {
            tab_width: 4,
            ..config::Config::default()
        };
        let mut ed = Editor::new(buf, cfg);
        assert_eq!(ed.tab_width, 4);
        ed.wrap = false; // horizontal scrolling needs wrap off
        ed.show_line_numbers = false;
        ed.text_w = 4;
        ed.bs_mut().cursor = Pos { row: 0, col: 3 };
        ed.adjust_scroll_x();
        // "a\tb" at tw 4 is 5 display cols; 5 >= 0 + 4 → scroll_x = 5 + 1 - 4
        assert_eq!(ed.bs().scroll_x, 2);
    }

    #[test]
    fn config_line_numbers_seeded() {
        let cfg = config::Config {
            line_numbers: true,
            ..config::Config::default()
        };
        let ed = Editor::new(Buffer::new(), cfg);
        assert!(ed.show_line_numbers);
        assert_eq!(ed.config.tab_width, 8);
    }

    // F3 — auto-indent carries the current line's leading whitespace onto
    // the new row (only when config.auto_indent is on).

    #[test]
    fn auto_indent_copies_indent() {
        let mut buf = Buffer::new();
        buf.set_rows(vec!["    foo".chars().collect()]);
        let cfg = config::Config {
            auto_indent: true,
            ..config::Config::default()
        };
        let mut ed = Editor::new(buf, cfg);
        ed.bs_mut().cursor = Pos { row: 0, col: 7 };
        press(&mut ed, KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(lines(&ed), vec!["    foo", "    "]);
        assert_eq!(ed.bs().cursor, Pos { row: 1, col: 4 });
    }

    /// **The one-pass scan must agree with the obvious one.** The early exit at
    /// GCD 1 is exact (a GCD cannot rise), but "exact" is a claim, so it is
    /// checked against a reference over documents chosen to reach every branch:
    /// no indentation at all, a tab anywhere, a single width, a 1 that appears
    /// only at the END (the case the early exit has to get right), a mixed 4/8,
    /// and a unit past the 8-space fallback.
    #[test]
    fn indent_unit_matches_the_reference_scan() {
        /// The literal reading: collect every count, sort, fold the GCD.
        fn reference(text: &str) -> String {
            let lines: Vec<&str> = text.split('\n').collect();
            if lines.iter().any(|l| l.starts_with('\t')) {
                return "\t".to_string();
            }
            let mut counts: Vec<usize> = lines
                .iter()
                .filter_map(|l| {
                    let n = l.len() - l.trim_start_matches(' ').len();
                    (n > 0).then_some(n)
                })
                .collect();
            if counts.is_empty() {
                return "\t".to_string();
            }
            counts.sort();
            let mut unit = counts[0];
            for n in &counts {
                let (mut a, mut b) = (unit, *n);
                while b != 0 {
                    let t = b;
                    b = a % b;
                    a = t;
                }
                unit = a;
            }
            if unit == 0 || unit > 8 {
                return "\t".to_string();
            }
            " ".repeat(unit)
        }

        for (label, text) in [
            ("flat", "a\nb\nc"),
            ("tab", "a\n\tb"),
            ("tab late", &format!("{}x\n\tlate", "a\n".repeat(500))),
            ("four", "    a\n    b\n        c"),
            ("eight", "        a\n        b"),
            ("mixed 4/8", "    a\n        b\n    c"),
            ("two", "  a\n    b\n  c"),
            ("one early", " a\n  b\n    c"),
            ("one last", &format!("{}\n a", "    row\n".repeat(200))),
            ("nine", "         a\n         b"),
            ("blank lines", "\n\n    a\n\n"),
            ("single", "    only"),
        ] {
            let ed = test_ed(text);
            assert_eq!(
                ed.indent_unit(),
                reference(text),
                "{label}: one-pass disagrees with the reference"
            );
        }
    }

    #[test]
    fn auto_indent_off_by_config() {
        let mut ed = test_ed("    foo");
        ed.config.auto_indent = false;
        ed.bs_mut().cursor = Pos { row: 0, col: 7 };
        press(&mut ed, KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(lines(&ed), vec!["    foo", ""]);
        assert_eq!(ed.bs().cursor, Pos { row: 1, col: 0 });
    }

    #[test]
    fn auto_indent_on_by_default_and_electric_after_brace() {
        // Default config: Enter carries the indent...
        let mut ed = test_ed("    foo");
        ed.bs_mut().cursor = Pos { row: 0, col: 7 };
        press(&mut ed, KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(lines(&ed), vec!["    foo", "    "]);
        assert_eq!(ed.bs().cursor, Pos { row: 1, col: 4 });
        // ...and an opening brace indents one unit deeper.
        let mut ed = test_ed("    if x {");
        ed.bs_mut().cursor = Pos { row: 0, col: 10 };
        press(&mut ed, KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(lines(&ed), vec!["    if x {", "        "]);
        assert_eq!(ed.bs().cursor, Pos { row: 1, col: 8 });
    }

    #[test]
    fn mouse_click_drag_and_wheel() {
        let mut ed = test_ed("hello\nworld\n3\n4\n5\n6\n");
        ed.show_line_numbers = true;
        ed.text_w = 40;
        ed.text_h = 10;
        // Click on "world" (pane row 2, col 2; gutter is 3 wide).
        assert!(ed.handle_mouse(me(MouseEventKind::Down(MouseButton::Left), 2, 5)));
        assert_eq!(ed.bs().cursor, Pos { row: 1, col: 2 });
        assert_eq!(ed.bs().mark, Some(Pos { row: 1, col: 2 }));
        // Drag extends the selection (pane col 4 → disp 1 → char col 1).
        assert!(ed.handle_mouse(me(MouseEventKind::Drag(MouseButton::Left), 2, 4)));
        assert_eq!(ed.bs().cursor, Pos { row: 1, col: 1 });
        assert_eq!(ed.bs().mark, Some(Pos { row: 1, col: 2 }));
        // Title row and status/bar rows are ignored.
        assert!(!ed.handle_mouse(me(MouseEventKind::Down(MouseButton::Left), 0, 3)));
        assert!(!ed.handle_mouse(me(MouseEventKind::Down(MouseButton::Left), 22, 3)));
    }

    // Wheel — viewport scrolls without moving the edit point; the cursor is
    // pulled along only when the scroll would push it out of the view.
    #[test]
    fn mouse_wheel_scrolls_view_not_cursor() {
        let text: String = (1..=30).map(|i| format!("L{i}\n")).collect();
        let mut ed = test_ed(&text);
        ed.text_h = 10; // max_scroll = 30 - 10 = 20
        ed.bs_mut().cursor = Pos { row: 4, col: 1 };
        // Wheel down: the viewport moves, the edit point stays.
        assert!(ed.handle_mouse(me(MouseEventKind::ScrollDown, 0, 0)));
        assert_eq!(ed.bs().scroll, 3);
        assert_eq!(ed.bs().cursor.row, 4, "wheel must not move the cursor");
        // Scrolling past the cursor pins it to the top edge of the view.
        assert!(ed.handle_mouse(me(MouseEventKind::ScrollDown, 0, 0)));
        assert_eq!(ed.bs().scroll, 6);
        assert_eq!(ed.bs().cursor.row, 6, "cursor pinned to the viewport top");
        // Wheel up: the viewport moves back, the cursor stays.
        assert!(ed.handle_mouse(me(MouseEventKind::ScrollUp, 0, 0)));
        assert_eq!(ed.bs().scroll, 3);
        assert_eq!(ed.bs().cursor.row, 6);
        // Cursor on the bottom row of the view: scrolling up pins it there.
        ed.bs_mut().cursor = Pos { row: 12, col: 1 };
        assert!(ed.handle_mouse(me(MouseEventKind::ScrollUp, 0, 0)));
        assert_eq!(ed.bs().scroll, 0);
        assert_eq!(
            ed.bs().cursor.row,
            9,
            "cursor pinned to the viewport bottom"
        );
        // Clamped at the top of the file.
        assert!(ed.handle_mouse(me(MouseEventKind::ScrollUp, 0, 0)));
        assert_eq!(ed.bs().scroll, 0);
        assert_eq!(ed.bs().cursor.row, 9);
        // Clamped at the end of the file.
        ed.bs_mut().scroll = 20;
        ed.bs_mut().cursor = Pos { row: 25, col: 1 };
        assert!(ed.handle_mouse(me(MouseEventKind::ScrollDown, 0, 0)));
        assert_eq!(ed.bs().scroll, 20, "scroll clamped at end of file");
        assert_eq!(ed.bs().cursor.row, 25);
    }

    // E4a — expand_tilde: only a leading ~ (exactly "~" or "~/") expands.

    #[test]
    fn expand_tilde_home() {
        let home = std::env::var("HOME").unwrap_or_default();
        assert_eq!(expand_tilde("~"), home);
        assert_eq!(expand_tilde("~/x"), format!("{home}/x"));
    }

    #[test]
    fn expand_tilde_leaves_paths() {
        assert_eq!(expand_tilde("/a"), "/a");
        assert_eq!(expand_tilde("x~"), "x~");
        assert_eq!(expand_tilde("~user/x"), "~user/x");
        assert_eq!(expand_tilde(""), "");
    }

    // E4b — prompt history: Up cycles back, Down forward, past the newest
    // entry restores the text captured when cycling began.

    #[test]
    fn search_history_cycles() {
        let mut ed = test_ed("bar");
        press(&mut ed, KeyCode::Char('f'), KeyModifiers::CONTROL);
        press_text(&mut ed, "foo");
        press(&mut ed, KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(ed.search_hist, vec!["foo".to_string()]);
        // no matches → ^F re-opens the prompt seeded with the old query
        press(&mut ed, KeyCode::Char('f'), KeyModifiers::CONTROL);
        for _ in 0..3 {
            press(&mut ed, KeyCode::Backspace, KeyModifiers::NONE);
        }
        press(&mut ed, KeyCode::Up, KeyModifiers::NONE);
        let p = ed.prompt.as_ref().unwrap();
        assert_eq!(p.text, "foo");
        assert_eq!(p.cursor, 3);
        press(&mut ed, KeyCode::Down, KeyModifiers::NONE);
        let p = ed.prompt.as_ref().unwrap();
        assert_eq!(p.text, "", "past the newest entry restores the draft");
        press(&mut ed, KeyCode::Up, KeyModifiers::NONE);
        press(&mut ed, KeyCode::Up, KeyModifiers::NONE);
        let p = ed.prompt.as_ref().unwrap();
        assert_eq!(p.text, "foo", "stays on the oldest entry");
        assert!(ed.prompt.is_some());
    }

    #[test]
    fn prompt_history_dedupes_consecutive() {
        let mut ed = test_ed("");
        for _ in 0..2 {
            press(&mut ed, KeyCode::Char('t'), KeyModifiers::CONTROL);
            press_text(&mut ed, "true");
            press(&mut ed, KeyCode::Enter, KeyModifiers::NONE);
        }
        assert_eq!(ed.exec_hist, vec!["true".to_string()]);
    }

    #[test]
    fn prompt_history_skips_empty() {
        let mut ed = test_ed("");
        press(&mut ed, KeyCode::Char('o'), KeyModifiers::CONTROL);
        press(&mut ed, KeyCode::Enter, KeyModifiers::NONE);
        assert!(ed.file_hist.is_empty());
    }

    // E4c — Tab completes file paths in the path prompts.

    #[test]
    fn complete_path_fixture() {
        let d = temp_dir("cmpl");
        fs::write(d.0.join("alpha.txt"), "").unwrap();
        fs::create_dir_all(d.0.join("alphabet")).unwrap();
        let base = format!("{}/", d.0.display());
        let (c, opts) = complete_path(&format!("{base}alpha.txt")).unwrap();
        assert_eq!(c, format!("{base}alpha.txt"));
        assert_eq!(opts, vec!["alpha.txt".to_string()]);
        let (c, opts) = complete_path(&format!("{base}alphabet")).unwrap();
        assert_eq!(
            c,
            format!("{base}alphabet/"),
            "unique dir gains a trailing /"
        );
        assert_eq!(opts, vec!["alphabet/".to_string()]);
        let (c, opts) = complete_path(&format!("{base}alp")).unwrap();
        assert_eq!(c, format!("{base}alpha"));
        assert_eq!(opts, vec!["alpha.txt".to_string(), "alphabet/".to_string()]);
        let (c, opts) = complete_path(&format!("{base}a")).unwrap();
        assert_eq!(c, format!("{base}alpha"));
        assert_eq!(opts.len(), 2);
        assert!(complete_path(&format!("{base}zzz")).is_none());
    }

    #[test]
    fn tab_completes_in_prompt() {
        let d = temp_dir("cmpl_prompt");
        fs::write(d.0.join("alpha.txt"), "").unwrap();
        fs::create_dir_all(d.0.join("alphabet")).unwrap();
        let mut ed = test_ed("");
        press(&mut ed, KeyCode::Char('o'), KeyModifiers::CONTROL);
        press_text(&mut ed, &format!("{}/alp", d.0.display()));
        press(&mut ed, KeyCode::Tab, KeyModifiers::NONE);
        let p = ed.prompt.as_ref().unwrap();
        assert_eq!(p.text, format!("{}/alpha", d.0.display()));
        assert!(ed.prompt.is_some());
    }

    // E4d — M-b / M-f / Ctrl+Left / Ctrl+Right are word motion inside
    // prompts (they must not type letters or move one char).

    #[test]
    fn prompt_word_motion() {
        let mut ed = test_ed("");
        press(&mut ed, KeyCode::Char('t'), KeyModifiers::CONTROL);
        press_text(&mut ed, "foo bar_baz");
        press(&mut ed, KeyCode::Home, KeyModifiers::NONE);
        press(&mut ed, KeyCode::Char('f'), KeyModifiers::ALT);
        assert_eq!(ed.prompt.as_ref().unwrap().cursor, 4);
        press(&mut ed, KeyCode::Char('f'), KeyModifiers::ALT);
        assert_eq!(ed.prompt.as_ref().unwrap().cursor, 11);
        press(&mut ed, KeyCode::Char('b'), KeyModifiers::ALT);
        assert_eq!(ed.prompt.as_ref().unwrap().cursor, 4);
        press(&mut ed, KeyCode::Char('b'), KeyModifiers::ALT);
        assert_eq!(ed.prompt.as_ref().unwrap().cursor, 0);
        assert!(ed.prompt.is_some());
    }

    #[test]
    fn prompt_ctrl_arrow_word_motion() {
        let mut ed = test_ed("");
        press(&mut ed, KeyCode::Char('t'), KeyModifiers::CONTROL);
        press_text(&mut ed, "foo bar_baz");
        press(&mut ed, KeyCode::Home, KeyModifiers::NONE);
        press(&mut ed, KeyCode::Right, KeyModifiers::CONTROL);
        assert_eq!(ed.prompt.as_ref().unwrap().cursor, 4);
        press(&mut ed, KeyCode::Left, KeyModifiers::CONTROL);
        assert_eq!(ed.prompt.as_ref().unwrap().cursor, 0);
        assert!(ed.prompt.is_some());
    }

    // F5 — search wiring: Matcher semantics (regex + case toggle), invalid
    // regex flashes, M-C / M-R toggles in the search prompt only.

    #[test]
    fn regex_search_matches_line_starts() {
        let mut ed = test_ed("ba\naba\nbar");
        ed.search_regex = true;
        press(&mut ed, KeyCode::Char('f'), KeyModifiers::CONTROL);
        press_text(&mut ed, "^ba");
        press(&mut ed, KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(ed.bs().cursor, Pos { row: 0, col: 0 });
        press(&mut ed, KeyCode::Char('f'), KeyModifiers::ALT);
        assert_eq!(
            ed.bs().cursor,
            Pos { row: 2, col: 0 },
            "aba must be skipped"
        );
    }

    #[test]
    fn case_toggle_search() {
        let mut ed = test_ed("foo\nFOO");
        press(&mut ed, KeyCode::Char('f'), KeyModifiers::CONTROL);
        press_text(&mut ed, "FOO");
        press(&mut ed, KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(
            ed.bs().cursor,
            Pos { row: 0, col: 0 },
            "default is case-insensitive"
        );
        press(&mut ed, KeyCode::Char('f'), KeyModifiers::ALT);
        assert_eq!(ed.bs().cursor, Pos { row: 1, col: 0 });
        ed.bs_mut().search_matches = None; // let ^F re-open the prompt
        press(&mut ed, KeyCode::Char('f'), KeyModifiers::CONTROL);
        press(&mut ed, KeyCode::Char('c'), KeyModifiers::ALT);
        assert!(ed.search_case_sensitive);
        press(&mut ed, KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(ed.bs().cursor, Pos { row: 1, col: 0 });
        assert_eq!(
            ed.bs().search_matches.as_ref().unwrap().len(),
            1,
            "case-sensitive search sees only FOO"
        );
    }

    #[test]
    fn invalid_regex_flashes() {
        let mut ed = test_ed("abc");
        ed.search_regex = true;
        press(&mut ed, KeyCode::Char('f'), KeyModifiers::CONTROL);
        press_text(&mut ed, "[");
        press(&mut ed, KeyCode::Enter, KeyModifiers::NONE);
        let st = ed.status_text().unwrap();
        assert!(st.contains("regex"), "status: {st}");
        assert_eq!(
            ed.bs().cursor,
            Pos { row: 0, col: 0 },
            "no jump on invalid regex"
        );
        assert!(ed.bs().search_matches.is_none());
    }

    #[test]
    fn m_c_m_r_toggle_in_prompt() {
        let mut ed = test_ed("");
        press(&mut ed, KeyCode::Char('f'), KeyModifiers::CONTROL);
        press_text(&mut ed, "q");
        press(&mut ed, KeyCode::Char('c'), KeyModifiers::ALT);
        assert!(ed.search_case_sensitive);
        assert!(
            ed.status_text().unwrap().contains("Case sensitive: on"),
            "status: {}",
            ed.status_text().unwrap()
        );
        press(&mut ed, KeyCode::Char('c'), KeyModifiers::ALT);
        assert!(!ed.search_case_sensitive);
        press(&mut ed, KeyCode::Char('r'), KeyModifiers::ALT);
        assert!(ed.search_regex);
        assert!(ed.status_text().unwrap().contains("Regex: on"));
        press(&mut ed, KeyCode::Char('r'), KeyModifiers::ALT);
        assert!(!ed.search_regex);
        let p = ed.prompt.as_ref().unwrap();
        assert_eq!(p.text, "q", "toggle leaves prompt text unchanged");
        assert_eq!(p.cursor, 1);
        // Toggles must not leak into other prompt kinds: M-C types 'c' there.
        press(&mut ed, KeyCode::Enter, KeyModifiers::NONE);
        press(&mut ed, KeyCode::Char('o'), KeyModifiers::CONTROL);
        press(&mut ed, KeyCode::Char('c'), KeyModifiers::ALT);
        assert!(!ed.search_case_sensitive);
        let p = ed.prompt.as_ref().unwrap();
        assert!(p.text.contains('c'), "M-C must type in a WriteName prompt");
    }

    // F8 — multi-buffer: open pushes/replaces, M-</M-> switch with per-buffer
    // state, title index, and ^X cycling over modified buffers.

    fn buf_with(text: &str) -> Buffer {
        let mut b = Buffer::new();
        b.set_rows(text.lines().map(|l| l.chars().collect()).collect());
        b
    }

    #[test]
    fn open_file_multibuffer_pushes() {
        let d = temp_dir("open_multi");
        let f1 = d.0.join("a.txt");
        let f2 = d.0.join("b.txt");
        fs::write(&f1, "one\ntwo").unwrap();
        fs::write(&f2, "x\ny\nz").unwrap();
        let cfg = config::Config {
            multibuffer: true,
            ..config::Config::default()
        };
        let mut ed = Editor::new(buf_with("seed"), cfg);
        press(&mut ed, KeyCode::F(8), KeyModifiers::NONE);
        assert!(ed.prompt.is_some());
        press_text(&mut ed, &f2.display().to_string());
        press(&mut ed, KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(ed.buffers.len(), 2);
        assert_eq!(ed.cur, 1);
        assert_eq!(lines(&ed), vec!["x", "y", "z"]);
        assert_eq!(ed.bs().buf.name, Some(f2));
    }

    #[test]
    fn open_file_replaces_when_disabled() {
        let d = temp_dir("open_single");
        let f1 = d.0.join("a.txt");
        fs::write(&f1, "x\ny\nz").unwrap();
        let mut ed = test_ed("seed");
        press(&mut ed, KeyCode::F(8), KeyModifiers::NONE);
        press_text(&mut ed, &f1.display().to_string());
        press(&mut ed, KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(ed.buffers.len(), 1);
        assert_eq!(ed.cur, 0);
        assert_eq!(lines(&ed), vec!["x", "y", "z"]);
        assert_eq!(ed.bs().buf.name, Some(f1));
    }

    #[test]
    fn open_missing_file_flashes() {
        let mut ed = test_ed("seed");
        press(&mut ed, KeyCode::F(8), KeyModifiers::NONE);
        press_text(&mut ed, "/no/such/file/rano_open_test.txt");
        press(&mut ed, KeyCode::Enter, KeyModifiers::NONE);
        let st = ed.status_text().unwrap();
        assert!(st.contains("Error:"), "status: {st}");
        assert!(ed.prompt.is_some(), "the open prompt stays open on error");
        assert_eq!(lines(&ed), vec!["seed"]);
    }

    #[test]
    fn switch_buffers_wrap() {
        let mut ed = test_ed("a");
        ed.buffers.push(BufferState::new(buf_with("b")));
        press(&mut ed, KeyCode::Char('>'), KeyModifiers::ALT);
        assert_eq!(ed.cur, 1);
        let st = ed.status_text().unwrap();
        assert!(st.contains("Buffer:"), "status: {st}");
        press(&mut ed, KeyCode::Char('>'), KeyModifiers::ALT);
        assert_eq!(ed.cur, 0, "wraps past the last");
        press(&mut ed, KeyCode::Char('<'), KeyModifiers::ALT);
        assert_eq!(ed.cur, 1, "wraps back past the first");
    }

    #[test]
    fn per_buffer_state_isolated() {
        let mut ed = test_ed("a\nb");
        ed.buffers.push(BufferState::new(buf_with("x\ny")));
        ed.cur = 1;
        press(&mut ed, KeyCode::Down, KeyModifiers::NONE);
        press(&mut ed, KeyCode::Right, KeyModifiers::NONE);
        assert_eq!(ed.bs().cursor, Pos { row: 1, col: 1 });
        press(&mut ed, KeyCode::Char('>'), KeyModifiers::ALT);
        assert_eq!(
            ed.bs().cursor,
            Pos { row: 0, col: 0 },
            "buffer 1 keeps its own cursor"
        );
        press(&mut ed, KeyCode::Char('<'), KeyModifiers::ALT);
        assert_eq!(
            ed.bs().cursor,
            Pos { row: 1, col: 1 },
            "buffer 2's cursor preserved across the switch"
        );
        // Undo stacks are per buffer: each M-U hits only the current one.
        press_text(&mut ed, "Z");
        press(&mut ed, KeyCode::Char('>'), KeyModifiers::ALT);
        press_text(&mut ed, "Q");
        press(&mut ed, KeyCode::Char('u'), KeyModifiers::ALT);
        assert_eq!(lines(&ed), vec!["a", "b"], "buffer 1 undoes only its edit");
        press(&mut ed, KeyCode::Char('<'), KeyModifiers::ALT);
        press(&mut ed, KeyCode::Char('u'), KeyModifiers::ALT);
        assert_eq!(lines(&ed), vec!["x", "y"], "buffer 2 undoes only its edit");
    }

    #[test]
    fn title_shows_index_when_multibuffer() {
        let mut ed = test_ed("a");
        ed.bs_mut().buf.name = Some(PathBuf::from("/tmp/a.txt"));
        assert_eq!(ed.title_text(), "/tmp/a.txt");
        let mut b2 = buf_with("b");
        b2.name = Some(PathBuf::from("/tmp/b.txt"));
        ed.buffers.push(BufferState::new(b2));
        assert_eq!(ed.title_text(), "[1/2] /tmp/a.txt");
        press(&mut ed, KeyCode::Char('>'), KeyModifiers::ALT);
        assert_eq!(ed.title_text(), "[2/2] /tmp/b.txt");
    }

    #[test]
    fn quit_cycles_modified_buffers() {
        let mut ed = test_ed("a");
        ed.buffers.push(BufferState::new(buf_with("b")));
        ed.buffers[0].buf.modified = true;
        ed.buffers[1].buf.modified = true;
        press(&mut ed, KeyCode::Char('x'), KeyModifiers::CONTROL);
        assert!(matches!(
            ed.prompt.as_ref().map(|p| p.kind),
            Some(PromptKind::ConfirmSave)
        ));
        assert_eq!(ed.cur, 0);
        press(&mut ed, KeyCode::Char('n'), KeyModifiers::NONE);
        assert!(!ed.buffers[0].buf.modified, "'n' discards this buffer");
        assert_eq!(ed.cur, 1, "next modified buffer becomes current");
        assert!(matches!(
            ed.prompt.as_ref().map(|p| p.kind),
            Some(PromptKind::ConfirmSave)
        ));
        assert!(!ed.quit);
        press(&mut ed, KeyCode::Char('n'), KeyModifiers::NONE);
        assert!(!ed.buffers[1].buf.modified);
        assert!(ed.quit, "all modified buffers dealt with → quit");
    }

    #[test]
    fn try_quit_other_modified() {
        let mut ed = test_ed("a");
        ed.buffers.push(BufferState::new(buf_with("b")));
        ed.buffers[1].buf.modified = true;
        press(&mut ed, KeyCode::Char('x'), KeyModifiers::CONTROL);
        assert_eq!(ed.cur, 1, "the modified buffer becomes current");
        assert!(matches!(
            ed.prompt.as_ref().map(|p| p.kind),
            Some(PromptKind::ConfirmSave)
        ));
        assert!(!ed.quit);
        press(&mut ed, KeyCode::Esc, KeyModifiers::NONE);
        assert!(!ed.quit, "cancel abandons the quit");
        assert!(ed.prompt.is_none());
    }

    #[test]
    fn quit_save_cycles_to_next_modified() {
        let d = temp_dir("quit_cycle_save");
        let f1 = d.0.join("a.txt");
        let f2 = d.0.join("b.txt");
        let mut ed = test_ed("a");
        ed.bs_mut().buf.name = Some(f1.clone());
        ed.bs_mut().buf.modified = true;
        let mut b2 = buf_with("b");
        b2.name = Some(f2.clone());
        b2.modified = true;
        ed.buffers.push(BufferState::new(b2));
        press(&mut ed, KeyCode::Char('x'), KeyModifiers::CONTROL);
        press(&mut ed, KeyCode::Char('y'), KeyModifiers::NONE);
        assert!(f1.exists());
        assert!(!ed.buffers[0].buf.modified);
        assert_eq!(ed.cur, 1, "saved → next modified buffer prompted");
        assert!(matches!(
            ed.prompt.as_ref().map(|p| p.kind),
            Some(PromptKind::ConfirmSave)
        ));
        press(&mut ed, KeyCode::Char('y'), KeyModifiers::NONE);
        assert!(f2.exists());
        assert!(ed.quit, "last buffer saved → quit");
    }

    // ---------- buffers: close, CLI files, reuse by jumps ----------

    fn named_buffers(names: &[&str]) -> Editor {
        let mut ed = test_ed("0");
        ed.config.multibuffer = true;
        ed.bs_mut().buf.name = Some(PathBuf::from(names[0]));
        for (i, n) in names.iter().enumerate().skip(1) {
            let mut b = buf_with(&i.to_string());
            b.name = Some(PathBuf::from(n));
            ed.buffers.push(BufferState::new(b));
        }
        ed
    }

    #[test]
    fn close_buffer_removes_it_and_lands_on_the_next() {
        let mut ed = named_buffers(&["/tmp/rano_c0", "/tmp/rano_c1", "/tmp/rano_c2"]);
        ed.cur = 1;
        press(&mut ed, KeyCode::Char('w'), KeyModifiers::ALT);
        assert_eq!(ed.buffers.len(), 2);
        assert_eq!(ed.cur, 1);
        assert_eq!(
            lines(&ed),
            vec!["2"],
            "the following buffer takes its place"
        );
        press(&mut ed, KeyCode::Char('w'), KeyModifiers::ALT);
        assert_eq!(ed.cur, 0, "closing the last one lands on the new last");
        press(&mut ed, KeyCode::Char('w'), KeyModifiers::ALT);
        assert_eq!(ed.buffers.len(), 1, "the last buffer stays");
        assert!(ed.status_text().unwrap().contains("^X"));
    }

    #[test]
    fn close_modified_buffer_asks_and_n_discards() {
        let mut ed = named_buffers(&["/tmp/rano_m0", "/tmp/rano_m1"]);
        ed.buffers[0].buf.modified = true;
        press(&mut ed, KeyCode::Char('w'), KeyModifiers::ALT);
        assert!(matches!(
            ed.prompt.as_ref().map(|p| p.kind),
            Some(PromptKind::ConfirmClose)
        ));
        press(&mut ed, KeyCode::Esc, KeyModifiers::NONE);
        assert_eq!(ed.buffers.len(), 2, "cancel keeps it");
        press(&mut ed, KeyCode::Char('w'), KeyModifiers::ALT);
        press(&mut ed, KeyCode::Char('n'), KeyModifiers::NONE);
        assert_eq!(ed.buffers.len(), 1);
        assert_eq!(lines(&ed), vec!["1"]);
    }

    #[test]
    fn close_modified_buffer_y_saves_then_closes() {
        let d = temp_dir("close_save");
        let f = d.0.join("a.txt");
        let mut ed = named_buffers(&[f.to_str().unwrap(), "/tmp/rano_cs1"]);
        ed.buffers[0].buf.modified = true;
        press(&mut ed, KeyCode::Char('w'), KeyModifiers::ALT);
        press(&mut ed, KeyCode::Char('y'), KeyModifiers::NONE);
        assert_eq!(fs::read_to_string(&f).unwrap().trim_end(), "0");
        assert_eq!(ed.buffers.len(), 1);
        assert!(!ed.close_after_save);
    }

    #[test]
    fn cancelling_the_file_name_of_a_close_forgets_the_close() {
        let mut ed = test_ed("scratch");
        ed.buffers.push(BufferState::new(buf_with("other")));
        ed.bs_mut().buf.modified = true;
        press(&mut ed, KeyCode::Char('w'), KeyModifiers::ALT);
        press(&mut ed, KeyCode::Char('y'), KeyModifiers::NONE);
        assert!(matches!(
            ed.prompt.as_ref().map(|p| p.kind),
            Some(PromptKind::WriteName)
        ));
        press(&mut ed, KeyCode::Esc, KeyModifiers::NONE);
        assert!(!ed.close_after_save, "a later ^O must not close the buffer");
        assert_eq!(ed.buffers.len(), 2);
    }

    #[test]
    fn closing_a_buffer_fixes_the_jump_back_stack() {
        let mut ed = named_buffers(&["/tmp/rano_j0", "/tmp/rano_j1", "/tmp/rano_j2"]);
        ed.def_back.push(DefBack {
            buf: None,
            idx: Some(1),
            pos: Pos { row: 0, col: 0 },
        });
        ed.def_back.push(DefBack {
            buf: None,
            idx: Some(2),
            pos: Pos { row: 0, col: 1 },
        });
        ed.cur = 1;
        press(&mut ed, KeyCode::Char('w'), KeyModifiers::ALT);
        assert_eq!(
            ed.def_back.len(),
            1,
            "entries into the closed buffer are gone"
        );
        assert_eq!(ed.def_back[0].idx, Some(1), "later indices shift down");
        ed.cur = 0;
        press(&mut ed, KeyCode::Char(','), KeyModifiers::ALT);
        assert_eq!(lines(&ed), vec!["2"]);
    }

    #[test]
    fn several_files_parse_in_order() {
        let a = parse_args(&argv(&["a.rs", "-l", "3", "b.rs", "c.rs"])).expect("parse");
        assert_eq!(a.files, vec!["a.rs", "b.rs", "c.rs"]);
        assert_eq!(a.file(), Some("a.rs"));
        assert_eq!(a.line, Some(3));
    }

    #[test]
    fn extra_files_are_named_now_and_read_when_visited() {
        let d = temp_dir("deferred");
        let b = d.0.join("b.txt");
        fs::write(&b, "bee\n").unwrap();
        let missing = d.0.join("new.txt");
        let mut ed = test_ed("a");
        ed.bs_mut().buf.name = Some(d.0.join("a.txt"));
        ed.add_deferred_buffers(&[b.clone(), missing.clone(), b.clone()]);
        assert_eq!(ed.buffers.len(), 3, "a file named twice gets one buffer");
        assert_eq!(ed.buffers[1].pending_load.as_ref(), Some(&b));
        assert!(
            ed.buffers[2].pending_load.is_none(),
            "a new file has nothing to read"
        );
        assert!(!ed.start_pending_load(), "buffer 0 has nothing pending");
        press(&mut ed, KeyCode::Char('>'), KeyModifiers::ALT);
        assert!(ed.start_pending_load());
        assert!(ed.bs().pending_load.is_none());
        for _ in 0..200 {
            ed.load_poll();
            if !ed.loading() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_eq!(lines(&ed), vec!["bee"]);
    }

    #[test]
    fn open_file_switches_to_an_already_open_buffer() {
        let d = temp_dir("open_dup");
        let f = d.0.join("a.txt");
        fs::write(&f, "disk").unwrap();
        let mut ed = named_buffers(&[f.to_str().unwrap(), "/tmp/rano_od1"]);
        ed.cur = 1;
        assert!(ed.open_file(f.to_str().unwrap()));
        assert_eq!(ed.buffers.len(), 2, "no second copy");
        assert_eq!(ed.cur, 0);
        assert_eq!(lines(&ed), vec!["0"], "the open buffer, not the disk text");
    }

    #[test]
    fn definition_into_an_open_buffer_reuses_it() {
        let d = temp_dir("def_reuse");
        let src = d.0.join("src.rs");
        let tgt = d.0.join("tgt.rs");
        fs::write(&tgt, "fn t() {}\n").unwrap();
        let mut ed = named_buffers(&[src.to_str().unwrap(), tgt.to_str().unwrap()]);
        // An unsaved edit in the target buffer must be what the jump lands in.
        ed.buffers[1]
            .buf
            .set_rows(vec!["fn t() { edited }".chars().collect()]);
        let loc = lsp::DefLocation {
            uri: lsp::path_to_uri(&tgt),
            line: 0,
            character: 3,
        };
        ed.goto_location(loc, Pos { row: 0, col: 0 });
        assert_eq!(ed.buffers.len(), 2);
        assert_eq!(ed.cur, 1);
        assert_eq!(lines(&ed), vec!["fn t() { edited }"]);
        assert_eq!(ed.bs().cursor, Pos { row: 0, col: 3 });
        press(&mut ed, KeyCode::Char(','), KeyModifiers::ALT);
        assert_eq!(ed.cur, 0);
    }

    #[test]
    fn definition_into_a_deferred_buffer_reads_it_first() {
        let d = temp_dir("def_deferred");
        let tgt = d.0.join("tgt.rs");
        fs::write(&tgt, "// one\n// two\nfn t() {}\n").unwrap();
        let mut ed = named_ed("fn a() {}\n", "/tmp/rano_def_deferred_src.rs");
        ed.config.multibuffer = true;
        ed.add_deferred_buffers(std::slice::from_ref(&tgt));
        let loc = lsp::DefLocation {
            uri: lsp::path_to_uri(&tgt),
            line: 2,
            character: 3,
        };
        ed.goto_location(loc, Pos { row: 0, col: 0 });
        assert_eq!(ed.cur, 1);
        assert!(ed.bs().pending_load.is_none());
        assert_eq!(ed.bs().cursor, Pos { row: 2, col: 3 });
        assert_eq!(lines(&ed).len(), 3);
    }

    #[test]
    fn a_buffer_made_current_is_highlighted() {
        let dir = std::env::temp_dir().join("rano_hl_switch_fixture");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let a = dir.join("a.rs");
        let b = dir.join("b.rs");
        std::fs::write(&a, "fn a() {}\n").unwrap();
        std::fs::write(&b, "fn b() {}\n").unwrap();
        let mut ed = test_ed("");
        ed.config.multibuffer = true;
        ed.text_h = 10;
        let colored = |ed: &Editor| ed.bs().hl.style_at(Pos { row: 0, col: 0 }).is_some();
        // F8 into a new buffer.
        assert!(ed.open_file(a.to_str().unwrap()));
        ed.ensure_highlight();
        assert!(colored(&ed), "F8 opened a.rs uncoloured");
        assert!(ed.open_file(b.to_str().unwrap()));
        ed.ensure_highlight();
        assert!(colored(&ed), "F8 opened b.rs uncoloured");
        // Back to a.rs through the buffer switch, after its grid was dropped.
        let ia = ed.find_buffer(&a).unwrap();
        ed.buffers[ia].hl = Default::default();
        ed.set_current(ia);
        ed.ensure_highlight();
        assert!(colored(&ed), "switching to a.rs left it uncoloured");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn open_prompt_shows_live_path_hints() {
        let dir = std::env::temp_dir().join("rano_hints_fixture");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("alpha.rs"), "").unwrap();
        std::fs::write(dir.join("beta.rs"), "").unwrap();
        let mut ed = test_ed("hidden text");
        let base = format!("{}/", dir.display());
        ed.prompt = Some(crate::prompt::Prompt {
            kind: PromptKind::OpenName,
            cursor: base.chars().count(),
            text: base.clone(),
        });
        assert!(ed.refresh_prompt_hints());
        assert!(!ed.refresh_prompt_hints());
        let backend = ratatui::backend::TestBackend::new(60, 12);
        let mut term = ratatui::Terminal::new(backend).unwrap();
        term.draw(|f| ui::draw(f, &ed)).unwrap();
        let buf = term.backend().buffer().clone();
        let row =
            |y: u16| -> String { (0..60).map(|x| buf[(x, y)].symbol().to_string()).collect() };
        // status row is 12 - 3 = 9; the hints sit on the row above it.
        assert!(row(8).contains("alpha.rs  beta.rs  sub/"), "{}", row(8));
        // Typing narrows them.
        ed.prompt.as_mut().unwrap().text = format!("{base}b");
        assert!(ed.refresh_prompt_hints());
        assert_eq!(
            ed.prompt_hints.as_ref().unwrap().2,
            vec!["beta.rs".to_string()]
        );
        ed.prompt = None;
        assert!(ed.refresh_prompt_hints());
        assert!(ed.prompt_hints.is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn hint_rows_cap_and_count_the_rest() {
        let names: Vec<String> = (0..20).map(|i| format!("file{i:02}.rs")).collect();
        let rows = ui::hint_rows(&names, 30, 2);
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|r| r.chars().count() == 30));
        assert!(rows[1].trim_end().ends_with("(+16)"), "{:?}", rows);
    }

    #[test]
    fn picker_draws_over_the_text() {
        let mut ed = test_ed("hidden text");
        ed.bs_mut().buf.name = Some(PathBuf::from("/tmp/rano_draw_a"));
        let mut b = buf_with("x");
        b.name = Some(PathBuf::from("/tmp/rano_draw_b"));
        ed.buffers.push(BufferState::new(b));
        ed.open_buffer_list();
        let backend = ratatui::backend::TestBackend::new(60, 12);
        let mut term = ratatui::Terminal::new(backend).unwrap();
        term.draw(|f| ui::draw(f, &ed)).unwrap();
        let buf = term.backend().buffer().clone();
        let row =
            |y: u16| -> String { (0..60).map(|x| buf[(x, y)].symbol().to_string()).collect() };
        assert!(row(1).contains("Buffers (2)"), "{}", row(1));
        assert!(row(2).contains("rano_draw_a"), "{}", row(2));
        assert!(row(3).contains("rano_draw_b"), "{}", row(3));
        assert!(!(1..9).any(|y| row(y).contains("hidden text")));
    }
}
