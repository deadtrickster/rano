mod bindings;
mod buffer;
mod config;
mod editor;
mod exec;
mod exec_ctrl;
mod keys;
mod lsp;
mod lsp_ctrl;
mod prompt;
mod search;
mod search_ctrl;
mod syntax;
mod ui;

use std::collections::VecDeque;
use std::io;
use std::path::Path;
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
    pub(crate) wrap_key: (u64, usize),
    /// Bumped on every edit; part of the wrap_prefix freshness key.
    pub(crate) edit_gen: u64,
    pub(crate) undo: VecDeque<UndoStep>,
    redo: VecDeque<UndoStep>,
    pending: Option<UndoStep>,
    last_kind: Option<ActionKind>,
}

impl BufferState {
    pub(crate) fn new(buf: Buffer) -> Self {
        let mut buf = buf;
        if buf.lines.is_empty() {
            buf.lines.push(Vec::new());
        }
        let mut bs = Self {
            buf,
            hl: syntax::Highlighter::new(),
            lsp: None,
            lsp_diags: Vec::new(),
            syntax_diags: Vec::new(),
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
            wrap_key: (0, 0),
            edit_gen: 0,
            undo: VecDeque::new(),
            redo: VecDeque::new(),
            pending: None,
            last_kind: None,
        };
        bs.hl.refresh(&bs.buf);
        editor::refresh_syntax_diags(&mut bs);
        bs
    }
}

impl Editor {
    pub fn status_text(&self) -> Option<String> {
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

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("usage: rano [file]");
        return;
    }
    if args.iter().any(|a| a == "--version" || a == "-V") {
        println!("rano {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    let file = args.first().cloned();
    let mut buf = Buffer::new();
    let mut read_lines: Option<usize> = None;
    if let Some(f) = &file {
        let p = Path::new(f);
        if p.exists() {
            match Buffer::from_file(p) {
                Ok(b) => {
                    read_lines = Some(b.lines.len());
                    buf = b;
                }
                Err(e) => {
                    eprintln!("rano: cannot read {}: {}", f, e);
                    std::process::exit(1);
                }
            }
        } else {
            buf.name = Some(p.to_path_buf());
        }
    }

    let cfg = config::load();
    if let Err(e) = run(buf, read_lines, cfg) {
        eprintln!("rano: {}", e);
        std::process::exit(1);
    }
}

fn run(buf: Buffer, read_lines: Option<usize>, cfg: config::Config) -> io::Result<()> {
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
    if let Some(n) = read_lines
        && n > 0
    {
        ed.flash(&format!("Read {} line{}", n, if n == 1 { "" } else { "s" }));
    }
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
        ed.adjust_scroll(ed.text_h);
        ed.adjust_scroll_x();
        if dirty {
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
        if event::poll(Duration::from_millis(200))? {
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
        dirty |= ed.tick_status();
        dirty |= ed.lsp_poll();
        dirty |= ed.lsp_flush(Instant::now());
        ed.completion_retry_poll();
        dirty |= ed.exec_poll();
    };
    disable_raw_mode()?;
    execute!(
        io::stdout(),
        DisableBracketedPaste,
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    result
}

#[cfg(test)]
mod ed_tests {
    use super::*;
    use crate::BufferState;
    use crate::buffer::Pos;
    use crate::editor::Flash;
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
        buf.lines = text.lines().map(|l| l.chars().collect()).collect();
        if buf.lines.is_empty() {
            buf.lines.push(Vec::new());
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
        ed.bs()
            .buf
            .lines
            .iter()
            .map(|l| l.iter().collect())
            .collect()
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
        assert!(ed.def_back[0].buf.is_none() && ed.def_back[0].idx.is_none());
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
                .lines
                .iter()
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
        buf.lines = vec!["fn main() {".chars().collect()];
        let mut ed = Editor::new(buf, config::Config::default());
        ed.edit_invalidate();
        assert!(
            !ed.bs().syntax_diags.is_empty(),
            "unclosed fn block must yield a tree-sitter diagnostic"
        );
        ed.bs_mut().buf.lines = vec!["fn main() {}".chars().collect()];
        ed.edit_invalidate();
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
        buf.lines = vec!["fn broken(".chars().collect()];
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
        buf.lines = vec!["a\tb".chars().collect()];
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
        buf.lines = vec!["    foo".chars().collect()];
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
        b.lines = text.lines().map(|l| l.chars().collect()).collect();
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
}
