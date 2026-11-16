//! A terminal at the bottom of the frame, with tabs: each tab is a pty and a `vt100` parser, and
//! the panel draws the screen the shell answers with.
//!
//! The panel is **not** a plugin's job: the ABI has no process or terminal capability, so this is
//! native code, deliberately small. A tab is a pty plus a `vt100::Parser`; the host opens one per
//! tab, forwards keys to it, and reads its screen. Nothing here waits on a wire: the pty's parent
//! end is non-blocking, and a quiet shell costs a frame nothing.

use std::os::fd::RawFd;
use std::path::Path;

use crate::render::{Line, Span};
use crate::style::Role;

/// How many bytes a tab drains per tick before handing the terminal back.
const FEED_BUDGET: usize = 256 * 1024;

/// One shell: the pty it writes to, its process, and the emulator that makes a screen of it.
pub struct Tab {
    parser: vt100::Parser,
    master: RawFd,
    pid: libc::pid_t,
    /// The tab's label: the shell's name, and whether it has exited.
    pub title: String,
    /// The shell's exit status, once its end of the pty is gone and waitpid has reaped it.
    /// A dead shell takes no more keys.
    pub exited: Option<i32>,
}

impl Tab {
    /// Open a shell in `cwd`, its pty `cols`×`rows`. The parent's end is non-blocking and
    /// close-on-exec, and the spawn is a `Command`: the argv, the cwd and the environment are
    /// built before the fork, not in the child.
    pub fn open(cwd: &Path, cols: u16, rows: u16, scrollback: usize) -> std::io::Result<Tab> {
        let (master, slave) = openpty(cols, rows)?;
        unsafe {
            libc::fcntl(master, libc::F_SETFL, libc::O_NONBLOCK);
            // A child of the editor must not inherit the shell's pty, and the shell itself must
            // not inherit the parent end. Set before the spawn, and checked.
            if libc::fcntl(master, libc::F_SETFD, libc::FD_CLOEXEC) == -1 {
                let e = std::io::Error::last_os_error();
                libc::close(master);
                libc::close(slave);
                return Err(e);
            }
        }
        // The slave is the Command's Stdio: it is closed when that Command is dropped, on either
        // path. Closing it here too would be a double close, and a reused fd number is fatal.
        match spawn_shell(cwd, slave) {
            Ok(pid) => Ok(Tab {
                parser: new_parser(cols, rows, scrollback),
                master,
                pid,
                title: "sh".to_string(),
                exited: None,
            }),
            Err(e) => {
                // A failed spawn leaves the master to close; the slave went with the Command.
                unsafe {
                    libc::close(master);
                }
                Err(e)
            }
        }
    }

    /// Whatever the shell wrote since the last pass, into the emulator.
    pub fn feed(&mut self) -> bool {
        self.feed_budget(FEED_BUDGET)
    }

    /// A per-tick budget: a shell that never stops writing (`yes`) must not freeze the editor.
    /// Each read is capped at what is left of the budget, so the rest is still in the pty for the
    /// next tick and the panel keeps answering keys.
    fn feed_budget(&mut self, mut budget: usize) -> bool {
        let mut buf = [0u8; 4096];
        let mut dirty = false;
        loop {
            if budget == 0 {
                break;
            }
            let want = buf.len().min(budget);
            let n = unsafe { libc::read(self.master, buf.as_mut_ptr() as *mut _, want) };
            if n == 0
                || (n == -1 && std::io::Error::last_os_error().raw_os_error() == Some(libc::EIO))
            {
                // The shell's end is gone: EOF on some platforms, EIO on Linux. Reap it and put
                // the status in the tab's title rather than leaving a silent dead pane.
                self.notice_exit();
                break;
            }
            if n < 0 {
                break;
            }
            dirty = true;
            budget = budget.saturating_sub(n as usize);
            self.parser.process(&buf[..n as usize]);
        }
        dirty
    }

    /// The shell's exit, if waitpid can collect it now: the title carries the status, and the
    /// tab stops taking input.
    fn notice_exit(&mut self) {
        if self.exited.is_some() {
            return;
        }
        let mut status = 0;
        let r = unsafe { libc::waitpid(self.pid, &mut status, libc::WNOHANG) };
        if r == self.pid {
            let code = if libc::WIFEXITED(status) {
                libc::WEXITSTATUS(status)
            } else if libc::WIFSIGNALED(status) {
                128 + libc::WTERMSIG(status)
            } else {
                1
            };
            self.exited = Some(code);
            self.title = format!("sh (exited {code})");
        }
    }

    /// A key, as the shell's bytes. Every key translates or is swallowed: a chord this cannot
    /// spell must not fall through to the editor, which would cut or indent the file behind the
    /// reader's back.
    pub fn send(&mut self, bytes: &[u8]) {
        if self.exited.is_some() {
            // Nothing is written to a dead shell: the keystrokes are not its input any more.
            return;
        }
        // A key sent to the shell snaps the view to the bottom: what the operator types is an
        // answer to the newest output, and the scrollback is for reading, not for typing into.
        // A key sent to the shell snaps the view to the bottom: what the operator types is an
        // answer to the newest output, and the scrollback is for reading, not for typing into.
        self.parser.screen_mut().set_scrollback(0);
        let _ = unsafe { libc::write(self.master, bytes.as_ptr() as *const _, bytes.len()) };
    }

    /// A paste is the shell's input, not the editor's: bracketed when the program asked for
    /// bracketed paste, so readline and editors can tell it apart from typing.
    pub fn paste(&mut self, s: &str) {
        self.parser.screen_mut().set_scrollback(0);
        let mut out = Vec::new();
        if self.parser.screen().bracketed_paste() {
            out.extend_from_slice(b"\x1b[200~");
        }
        out.extend_from_slice(s.as_bytes());
        if self.parser.screen().bracketed_paste() {
            out.extend_from_slice(b"\x1b[201~");
        }
        self.send(&out);
    }

    /// The terminal's answer to a key, in the mode the program asked for: arrows are SS3 in
    /// application cursor mode (DECCKM), and a paste is bracketed when the program asked.
    pub fn key_bytes(&self, key: &crate::term::KeyEvent) -> Vec<u8> {
        use crate::term::KeyCode;
        let ctrl = key.mods.ctrl();
        let alt = key.mods.alt();
        let plain = !ctrl && !alt;
        let code = key.code;
        let ch = match code {
            KeyCode::Char(c) => c,
            _ => '\0',
        };
        if ctrl && ch.is_ascii_alphabetic() {
            // C-a..C-z are the control bytes 0x01..0x1a, which is what a shell's readline wants.
            return vec![(ch.to_ascii_lowercase() as u8) - b'a' + 1];
        }
        if alt && ch != '\0' {
            let mut out = vec![0x1b];
            let mut b = [0u8; 4];
            out.extend_from_slice(ch.encode_utf8(&mut b).as_bytes());
            return out;
        }
        match code {
            KeyCode::Char(c) if plain => {
                let mut b = [0u8; 4];
                c.encode_utf8(&mut b).as_bytes().to_vec()
            }
            KeyCode::Enter => vec![b'\n'],
            KeyCode::Backspace => vec![0x7f],
            KeyCode::Tab => vec![b'\t'],
            KeyCode::Delete => b"\x1b[3~".to_vec(),
            KeyCode::Insert => b"\x1b[2~".to_vec(),
            KeyCode::PageUp => b"\x1b[5~".to_vec(),
            KeyCode::PageDown => b"\x1b[6~".to_vec(),
            KeyCode::Up => self.arrow('A'),
            KeyCode::Down => self.arrow('B'),
            KeyCode::Right => self.arrow('C'),
            KeyCode::Left => self.arrow('D'),
            KeyCode::Home => b"\x1b[H".to_vec(),
            KeyCode::End => b"\x1b[F".to_vec(),
            KeyCode::F(n) => match n {
                1 => b"\x1bOP".to_vec(),
                2 => b"\x1bOQ".to_vec(),
                3 => b"\x1bOR".to_vec(),
                4 => b"\x1bOS".to_vec(),
                5 => b"\x1b[15~".to_vec(),
                6 => b"\x1b[17~".to_vec(),
                7 => b"\x1b[18~".to_vec(),
                8 => b"\x1b[19~".to_vec(),
                9 => b"\x1b[20~".to_vec(),
                10 => b"\x1b[21~".to_vec(),
                11 => b"\x1b[23~".to_vec(),
                12 => b"\x1b[24~".to_vec(),
                _ => Vec::new(),
            },
            _ => Vec::new(),
        }
    }

    /// An arrow, in the mode the program set: application cursor mode sends SS3, the default CSI.
    fn arrow(&self, which: char) -> Vec<u8> {
        if self.parser.screen().application_cursor() {
            vec![0x1b, b'O', which as u8]
        } else {
            vec![0x1b, b'[', which as u8]
        }
    }

    /// The strip's rectangle is the shell's terminal: the pty is told with TIOCSWINSZ, so
    /// `stty size` and full-screen programs agree with what is drawn, and the emulator's screen
    /// is the same size. A no-op when nothing changed, so a frame costs no ioctl.
    pub fn resize(&mut self, cols: u16, rows: u16) {
        let cols = cols.max(1);
        let rows = rows.max(1);
        let (have_rows, have_cols) = self.parser.screen().size();
        if have_cols.get() == cols.max(1) && have_rows.get() == rows.max(1) {
            return;
        }
        let size = libc::winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        unsafe {
            libc::ioctl(self.master, libc::TIOCSWINSZ as _, &size as *const _);
        }
        self.parser.set_size(nz(rows), nz(cols));
    }

    /// How far back the view is scrolled, in lines: the wheel's effect, readable by the host.
    /// Where the program's cursor is, in screen cells: `None` when the program hid it, or when
    /// the view is scrolled back, where the cursor is not on the glass.
    pub fn cursor_pos(&self) -> Option<(u16, u16)> {
        let screen = self.parser.screen();
        if screen.hide_cursor() || screen.scrollback() > 0 {
            return None;
        }
        Some(screen.cursor_position())
    }

    /// Is the program on the alternate screen? Then it owns the page keys: `less` scrolls its own
    /// document, and the scrollback rano keeps is not on the glass.
    pub fn on_program_screen(&self) -> bool {
        self.parser.screen().alternate_screen()
    }

    /// The rows the shell has scrolled off, and the rows on the glass: the scrollbar's total.
    pub fn history_len(&self) -> usize {
        self.parser.screen().scrollback_len()
    }

    pub fn screen_rows(&self) -> usize {
        self.parser.screen().size().0.get() as usize
    }

    /// Scroll so that the history's row `top` is the first visible one: what a click on the
    /// track names.
    pub fn scroll_to_top_index(&mut self, top: usize) {
        let history = self.history_len();
        self.parser
            .screen_mut()
            .set_scrollback(history.saturating_sub(top));
    }

    pub fn scroll_pos(&self) -> usize {
        self.parser.screen().scrollback()
    }

    /// Scroll back by `n` lines, capped at what the scrollback holds.
    pub fn scroll_up(&mut self, n: usize) {
        let cur = self.parser.screen().scrollback();
        // The parser clamps to what it kept, so a wheel at the top simply stops.
        self.parser.screen_mut().set_scrollback(cur + n);
    }

    /// Scroll forward by `n` lines, back to the live screen at zero.
    pub fn scroll_down(&mut self, n: usize) {
        let cur = self.parser.screen().scrollback();
        self.parser
            .screen_mut()
            .set_scrollback(cur.saturating_sub(n));
    }

    /// The screen, in the panel's rows.
    pub fn lines(&self) -> Vec<Line> {
        self.lines_for(false)
    }

    /// The screen, with the painted caret only where the strip does NOT have the keyboard: a
    /// focused strip's caret is the real one, which the frame places.
    pub fn lines_for(&self, focused: bool) -> Vec<Line> {
        let screen = self.parser.screen();
        // `size()` answers (rows, cols), as `Parser::new` takes them.
        let (rows, cols) = screen.size();
        let (rows, cols) = (rows.get() as usize, cols.get() as usize);
        (0..rows)
            .map(|y| row_line(screen, y, cols, focused))
            .collect()
    }
}

impl Drop for Tab {
    fn drop(&mut self) {
        unsafe {
            libc::close(self.master);
            libc::kill(self.pid, libc::SIGHUP);
            // A short grace, then SIGKILL, and the reap happens on a detached thread: closing a
            // tab must never block the editor on a child that ignores SIGHUP, and a closed tab
            // must not stay a zombie for the life of the process.
            let mut status = 0;
            let mut reaped = false;
            for _ in 0..20 {
                let r = libc::waitpid(self.pid, &mut status, libc::WNOHANG);
                if r == self.pid || r == -1 {
                    reaped = true;
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            if !reaped {
                libc::kill(self.pid, libc::SIGKILL);
                let pid = self.pid;
                std::thread::spawn(move || {
                    let mut status = 0;
                    libc::waitpid(pid, &mut status, 0);
                });
            }
        }
    }
}

/// The bottom terminal: a row of tabs, the active one drawn, and which of them has the keyboard.
pub struct BottomTerm {
    pub open: bool,
    pub tabs: Vec<Tab>,
    pub active: usize,
    /// The strip's height in rows, remembered across toggles: a terminal the operator has resized
    /// stays that size.
    pub height: usize,
    /// The rule above the strip is being dragged: the drag is the terminal's wherever the
    /// pointer goes, exactly as a held panel border is the panel's.
    pub dragging: bool,
}

impl BottomTerm {
    pub fn new() -> BottomTerm {
        BottomTerm {
            open: false,
            tabs: Vec::new(),
            active: 0,
            height: 12,
            dragging: false,
        }
    }

    /// Open a tab if there are none, and show it. The first tab opens in the editor's folder.
    pub fn toggle(
        &mut self,
        cwd: &Path,
        cols: u16,
        rows: u16,
        scrollback: usize,
    ) -> std::io::Result<()> {
        self.open = !self.open;
        if self.open && self.tabs.is_empty() {
            self.tabs.push(Tab::open(cwd, cols, rows, scrollback)?);
        }
        Ok(())
    }

    pub fn new_tab(
        &mut self,
        cwd: &Path,
        cols: u16,
        rows: u16,
        scrollback: usize,
    ) -> std::io::Result<()> {
        self.tabs.push(Tab::open(cwd, cols, rows, scrollback)?);
        self.active = self.tabs.len() - 1;
        Ok(())
    }

    /// The strip is resizable by key too: `M-+` grows it, `M--` shrinks it, and the choice sticks.
    pub fn grow(&mut self) {
        self.height = (self.height + 1).min(40);
    }

    pub fn shrink(&mut self) {
        self.height = self.height.saturating_sub(1).max(4);
    }

    pub fn next(&mut self) {
        if !self.tabs.is_empty() {
            self.active = (self.active + 1) % self.tabs.len();
        }
    }

    pub fn prev(&mut self) {
        if !self.tabs.is_empty() {
            self.active = (self.active + self.tabs.len() - 1) % self.tabs.len();
        }
    }

    pub fn close_active(&mut self) {
        if self.tabs.is_empty() {
            return;
        }
        self.tabs.remove(self.active);
        if self.tabs.is_empty() {
            self.open = false;
        }
        self.active = self.active.min(self.tabs.len().saturating_sub(1));
    }

    /// Feed every tab: a quiet shell is a cheap frame. `true` when anything arrived, which is
    /// what makes the next draw redraw — a terminal whose output only arrives on a keypress is a
    /// terminal that looks hung on `ls`.
    pub fn feed(&mut self) -> bool {
        let mut dirty = false;
        for t in &mut self.tabs {
            dirty |= t.feed();
        }
        dirty
    }

    /// The active tab's screen, or the empty state with the tab strip.
    pub fn lines(&self, w: usize, cwd: &Path) -> Vec<Line> {
        self.lines_for(w, cwd, false)
    }

    /// The pty masters of the LIVE tabs: a shell's output must wake the host's poll the way a
    /// keystroke does, or every echo waits out the idle interval instead of being read at once.
    /// A tab whose shell has exited is not polled: its master would answer HUP forever, and
    /// the loop would never sleep.
    pub fn master_fds(&self) -> Vec<std::os::unix::io::RawFd> {
        self.tabs
            .iter()
            .filter(|t| t.exited.is_none())
            .map(|t| t.master)
            .collect()
    }

    /// The strip's rows: the SHELL's rows only, the tabs living on the rule above them (see
    /// [`BottomTerm::tab_rule`]). With the active tab's caret painted in the theme's faint
    /// role reversed unless the strip has the keyboard (then the frame places the real one).

    pub fn lines_for(&self, w: usize, cwd: &Path, focused: bool) -> Vec<Line> {
        let mut out = Vec::new();
        if self.tabs.is_empty() {
            out.push(Line::new(vec![Span::role(
                format!("no terminal open — M-n opens one in {}", cwd.display()),
                Role::Faint,
            )]));
            return out.into_iter().map(|l| pad_to_width(l, w)).collect();
        }
        out.extend(
            self.tabs[self.active]
                .lines_for(focused)
                .into_iter()
                .map(|l| crate::render::text::truncate(&l, w)),
        );
        // Every row is the pane's width: `Buffer::set_line` writes only a line's spans, so a
        // short row painted over a longer one would leave the old row's tail behind. The parser
        // is not to blame — scrollback rows are stored trimmed, so padding belongs here.
        out.into_iter().map(|l| pad_to_width(l, w)).collect()
    }

    /// The strip's rule WITH ITS TABS: `── 1 sh ── 2 sh (exited 0) ─────`, the tabs separated
    /// by rule, the row padded with rule to the width, the active tab Strong (the accent while
    /// the strip has the keyboard) and the others faint, and the scroll counter — when the
    /// view is off the bottom — at the right end. Drawing the tabs ON the rule costs no row
    /// of the strip: an 11-row strip keeps its 11 rows of shell.
    pub fn tab_rule(&self, w: usize, focused: bool) -> Line {
        let rule_role = if focused {
            Role::UserAccent
        } else {
            Role::Faint
        };
        if self.tabs.is_empty() {
            let dashes = "─".repeat(w);
            return Line::new(vec![Span::role(dashes, rule_role)]);
        }
        let scrolled = self.tabs[self.active].parser.screen().scrollback();
        let more = if scrolled > 0 {
            format!("↑{scrolled} ")
        } else {
            String::new()
        };
        let mut spans: Vec<Span> = Vec::new();
        let mut used = 0usize;
        // The rule carries the strip's focus (the accent while the shell types); the active
        // tab is marked by weight upon it.
        let active_role = Role::Strong;
        for (i, tab) in self.tabs.iter().enumerate() {
            let label = format!("{} {}", i + 1, tab.title);
            let sep = if i == 0 { "── " } else { " ── " };
            let need = sep.chars().count() + label.chars().count();
            // The counter has the right end reserved; a tab that would run into it is
            // dropped, never cut — a tab half-drawn is a tab that lies.
            if used + need + more.chars().count() + 1 > w {
                break;
            }
            spans.push(Span::role(sep.to_string(), rule_role));
            spans.push(Span::role(
                label,
                if i == self.active {
                    active_role
                } else {
                    Role::Faint
                },
            ));
            used += need;
        }
        // The rule to the width, then the counter at the right end.
        let pad = w.saturating_sub(used + more.chars().count() + 1);
        spans.push(Span::role(" ".to_string() + &"─".repeat(pad), rule_role));
        if !more.is_empty() {
            spans.push(Span::role(more, Role::Strong));
        }
        Line::new(spans)
    }
}

impl Default for BottomTerm {
    fn default() -> Self {
        Self::new()
    }
}

/// A row padded with blanks to the pane's width: painting it over a longer row clears the tail.
fn pad_to_width(l: Line, w: usize) -> Line {
    let have = l.plain().chars().count();
    if have >= w {
        return l;
    }
    let mut spans = l.spans;
    spans.push(Span::raw(" ".repeat(w - have)));
    Line::new(spans)
}

/// The pty pair, sized to the panel. `openpty` is libc's, since that is the only C this needs.
fn openpty(cols: u16, rows: u16) -> std::io::Result<(RawFd, RawFd)> {
    let mut master: libc::c_int = 0;
    let mut slave: libc::c_int = 0;
    // **`*mut winsize`, because libc 0.2.189 unified it across platforms**: macOS's own
    // declaration is `const struct winsize *`, Linux's is not, and the crate chose the
    // mutable form for both. `&mut` is what compiles on every target this builds for.
    let mut size = libc::winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    let rc = unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut size,
        )
    };
    if rc != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok((master, slave))
}

/// The shell to open: `$SHELL` if it names one, `/bin/sh` otherwise.
fn shell() -> String {
    std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string())
}

/// The shell on the slave end of the pty, in the editor's folder. `Command` builds the argv and
/// the environment before the fork; the child's own work is only async-signal-safe calls.
fn spawn_shell(cwd: &Path, slave: RawFd) -> std::io::Result<libc::pid_t> {
    use std::os::fd::FromRawFd;
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};
    let mut cmd = Command::new(shell());
    cmd.current_dir(cwd)
        // What the parser implements well: 256 colours, the alternate screen, cursor addressing.
        .env("TERM", "xterm-256color");
    unsafe {
        // Each Stdio owns its own descriptor, so nothing is closed twice.
        cmd.stdin(Stdio::from_raw_fd(slave))
            .stdout(Stdio::from_raw_fd(libc::dup(slave)))
            .stderr(Stdio::from_raw_fd(libc::dup(slave)));
        cmd.pre_exec(|| {
            libc::setsid();
            // The pty is the child's controlling terminal: without it the shell says it has no
            // tty and turns job control off, a warning on every open.
            libc::ioctl(0, libc::TIOCSCTTY as _, 0);
            // SIGPIPE back to the default: the editor ignores it, and a pipeline like
            // `yes | head -1` only ends because the writer dies on it.
            libc::signal(libc::SIGPIPE, libc::SIG_DFL);
            Ok(())
        });
    }
    let child = cmd.spawn()?;
    Ok(child.id() as libc::pid_t)
}

/// The parser a tab is born with: `rows`×`cols`, keeping `scrollback` lines of history.
fn new_parser(cols: u16, rows: u16, scrollback: usize) -> vt100::Parser {
    vt100::Parser::new(nz(rows), nz(cols), scrollback)
}

/// A terminal dimension as the parser wants it: never zero.
fn nz(v: u16) -> std::num::NonZeroU16 {
    std::num::NonZeroU16::new(v.max(1)).expect("max(1) is not zero")
}

/// One screen row as a [`Line`], styles and all: the cells are grouped into runs of one style,
/// and the row is the parser's full width (a short row painted over a longer one must clear it).
fn row_line(screen: &vt100::Screen, y: usize, cols: usize, focused: bool) -> Line {
    let mut spans: Vec<Span> = Vec::new();
    let mut text = String::new();
    let mut style = crate::render::Style::new();
    let cursor = (!screen.hide_cursor()).then(|| screen.cursor_position());
    let painted = !focused;
    for x in 0..cols {
        let Some(cell) = screen.cell(y as u16, x as u16) else {
            break;
        };
        let mut st = crate::render::Style::new();
        if let Some(rgb) = colour_rgb(cell.fgcolor()) {
            st = st.raw(crate::render::Raw {
                fg_rgb: Some(rgb),
                ..crate::render::Raw::default()
            });
        }
        if cell.bold() {
            st = st.bold();
        }
        if cell.dim() {
            st = st.dim();
        }
        if cell.italic() {
            st = st.italic();
        }
        if cell.underline() {
            st = st.underline();
        }
        if cell.inverse() {
            st = st.reverse();
        }
        if painted && cursor == Some((y as u16, x as u16)) {
            // An unfocused strip's caret is the theme's faint role REVERSED, replacing the
            // cell's own style: the real cursor is in the focused pane. The theme is the
            // mechanism — the base table's faint is `dim`, which some terminals ignore, and a
            // theme file can name `faint` as a colour; either way the caret reads as the
            // theme's own idea of unimportant, never as a bright block over a reverse cell.
            st = crate::render::Style::of(crate::style::Role::Faint).reverse();
        }
        if st != style {
            if !text.is_empty() {
                spans.push(Span::styled(std::mem::take(&mut text), style.clone()));
            }
            style = st;
        }
        let contents = cell.contents();
        text.push_str(if contents.is_empty() { " " } else { contents });
    }
    if !text.is_empty() {
        spans.push(Span::styled(text, style));
    }
    if spans.is_empty() {
        spans.push(Span::raw(" ".to_string()));
    }
    Line::new(spans)
}

/// A vt100 colour as RGB: the sixteen the theme names, then the 6×6×6 cube and the grey ramp, so
/// a program asking for 256 colours keeps the one it chose.
fn colour_rgb(c: vt100::Color) -> Option<[u8; 3]> {
    match c {
        vt100::Color::Default => None,
        vt100::Color::Rgb(r, g, b) => Some([r, g, b]),
        vt100::Color::Idx(i) => Some(index_rgb(i)),
    }
}

fn index_rgb(i: u8) -> [u8; 3] {
    let n = i as usize;
    if n < 16 {
        return rgb(n);
    }
    if n < 232 {
        let n = n - 16;
        const STEPS: [u8; 6] = [0, 95, 135, 175, 215, 255];
        return [STEPS[n / 36], STEPS[(n / 6) % 6], STEPS[n % 6]];
    }
    let v = 8 + 10 * (i - 232);
    [v, v, v]
}

/// The sixteen colours SGR names, as RGB: exact colours, since a palette's nearest is not the
/// same colour the shell chose.
fn rgb(n: usize) -> [u8; 3] {
    const C: &[[u8; 3]] = &[
        [0, 0, 0],
        [205, 49, 49],
        [49, 205, 49],
        [205, 205, 49],
        [49, 49, 205],
        [205, 49, 205],
        [49, 205, 205],
        [229, 229, 229],
        [127, 127, 127],
        [255, 85, 85],
        [85, 255, 85],
        [255, 255, 85],
        [85, 85, 255],
        [255, 85, 255],
        [85, 255, 255],
        [255, 255, 255],
    ];
    C[n.min(15)]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tab on a pty the test drives: the test writes the slave, the tab reads the master.
    /// A real `sleep` is the child so Drop's SIGHUP and reap have someone to answer to.
    fn pipe_tab() -> (Tab, RawFd) {
        let (master, slave) = openpty(40, 6).expect("a pty");
        unsafe {
            libc::fcntl(master, libc::F_SETFL, libc::O_NONBLOCK);
        }
        let child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("sleep");
        let pid = child.id() as libc::pid_t;
        std::mem::forget(child);
        (
            Tab {
                parser: new_parser(40, 6, 200),
                master,
                pid,
                title: "t".to_string(),
                exited: None,
            },
            slave,
        )
    }

    /// Feed until the screen answers `pred`: a pty delivers a write in however many reads it
    /// likes, so a single feed is not a guarantee.
    fn feed_until(t: &mut Tab, pred: impl Fn(&Tab) -> bool) -> bool {
        // A loaded machine is slower than any small bound: the merge gate's suite has needed
        // more than six seconds for a shell to print its prompt. Thirty, returning the moment
        // the answer arrives.
        for _ in 0..3000 {
            t.feed();
            if pred(t) {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        false
    }

    fn write_all(fd: RawFd, bytes: &[u8]) {
        let n = unsafe { libc::write(fd, bytes.as_ptr() as *const _, bytes.len()) };
        assert_eq!(n, bytes.len() as isize, "the pty took the whole chunk");
    }

    /// A shell that floods must not be drained in one pass: more than the budget, in odd-sized
    /// chunks, leaves data in the pty that the next call reads.
    #[test]
    fn a_flooding_shell_leaves_data_for_the_next_tick() {
        let (mut t, slave) = pipe_tab();
        // 301 + 517 + 433 = 1251 bytes, against a budget of 1000.
        for n in [301usize, 517, 433] {
            write_all(slave, &vec![b'x'; n]);
        }
        assert!(t.feed_budget(1000), "the first pass read something");
        assert!(
            t.feed_budget(1000),
            "data was left for the next call: the first pass stopped at its budget"
        );
        assert!(!t.feed_budget(1000), "now the pty is drained");
        unsafe {
            libc::close(slave);
        }
    }

    /// Closing a tab kills even a shell that ignores SIGHUP, returns promptly, and reaps the
    /// pid: no zombie left for the life of the editor.
    #[test]
    fn drop_kills_a_shell_that_ignores_sighup_and_reaps_it() {
        let (master, slave) = openpty(40, 6).expect("a pty");
        // An ignored signal survives exec, so this `sleep` really does ignore SIGHUP.
        let child = std::process::Command::new("sh")
            .args(["-c", "trap '' HUP; exec sleep 5"])
            .spawn()
            .expect("a shell");
        let pid = child.id() as libc::pid_t;
        std::mem::forget(child);
        let t = Tab {
            parser: new_parser(40, 6, 200),
            master,
            pid,
            title: "t".to_string(),
            exited: None,
        };
        std::thread::sleep(std::time::Duration::from_millis(200));
        assert_eq!(unsafe { libc::kill(pid, 0) }, 0, "the child is alive");
        unsafe {
            libc::kill(pid, libc::SIGHUP);
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
        assert_eq!(unsafe { libc::kill(pid, 0) }, 0, "the child ignores SIGHUP");
        let start = std::time::Instant::now();
        drop(t);
        let elapsed = start.elapsed();
        assert!(
            elapsed < std::time::Duration::from_millis(500),
            "Drop returned in {elapsed:?}, not parked behind a child that ignores SIGHUP"
        );
        let mut gone = false;
        for _ in 0..3000 {
            // A zombie still answers kill(pid, 0); ESRCH means the pid was reaped.
            let r = unsafe { libc::kill(pid, 0) };
            if r == -1 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
                gone = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(gone, "the pid is reaped, not left a zombie");
        unsafe {
            libc::close(slave);
        }
    }

    /// A child of the editor does not inherit the shell's pty: the master is close-on-exec, and
    /// a process spawned after the tab opened does not have the fd.
    #[test]
    fn a_child_of_the_editor_does_not_inherit_the_pty() {
        let t = Tab::open(Path::new("/tmp"), 40, 6, 200).expect("a pty");
        let flags = unsafe { libc::fcntl(t.master, libc::F_GETFD) };
        assert!(
            flags >= 0 && flags & libc::FD_CLOEXEC != 0,
            "FD_CLOEXEC is set"
        );
        let status = std::process::Command::new("sh")
            .arg("-c")
            .arg(format!("[ ! -e /proc/self/fd/{} ]", t.master))
            .status()
            .expect("a child");
        assert!(status.success(), "fd {} is closed in the child", t.master);
    }

    /// `terminal_scrollback = 5` in config.toml is the history limit: five lines are kept, and
    /// the oldest are gone — not the compiled-in 200.
    #[test]
    fn the_config_sets_the_scrollback() {
        let dir = std::env::temp_dir().join(format!("rano-scrollback-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("a temp dir");
        let path = dir.join("config.toml");
        std::fs::write(&path, "terminal_scrollback = 5\n").expect("a config");
        let cfg = crate::config::load_from(&path);
        assert_eq!(cfg.terminal_scrollback, 5, "parsed from config.toml");
        let mut t = Tab::open(Path::new("/tmp"), 40, 6, cfg.terminal_scrollback).expect("a pty");
        for i in 0..20 {
            t.parser.process(format!("line{i}\r\n").as_bytes());
        }
        // The parser keeps five and clamps a scroll past them: the oldest are gone.
        t.scroll_up(100);
        assert_eq!(t.scroll_pos(), 5, "five lines kept");
        assert!(
            !t.lines().iter().any(|l| l.plain().contains("line0")),
            "the oldest lines scrolled out of the kept history"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A paste is the pty's input, and it is bracketed exactly when the program asked (2004).
    #[test]
    fn a_paste_reaches_the_pty_bracketed_when_asked() {
        let (mut t, slave) = pipe_tab();
        // Raw mode on the slave: a canonical line discipline would hold the paste until a
        // newline, and the test would be reading its own timeout.
        unsafe {
            let mut tio: libc::termios = std::mem::zeroed();
            libc::tcgetattr(slave, &mut tio);
            libc::cfmakeraw(&mut tio);
            libc::tcsetattr(slave, libc::TCSANOW, &tio);
        }
        let mut buf = [0u8; 64];
        t.paste("ab");
        let n = unsafe { libc::read(slave, buf.as_mut_ptr() as *mut _, buf.len()) };
        assert_eq!(&buf[..n as usize], b"ab", "unbracketed by default");
        t.parser.process(b"\x1b[?2004h");
        t.paste("cd");
        let n = unsafe { libc::read(slave, buf.as_mut_ptr() as *mut _, buf.len()) };
        assert_eq!(
            &buf[..n as usize],
            b"\x1b[200~cd\x1b[201~",
            "bracketed when asked"
        );
        t.parser.process(b"\x1b[?2004l");
        t.paste("ef");
        let n = unsafe { libc::read(slave, buf.as_mut_ptr() as *mut _, buf.len()) };
        assert_eq!(&buf[..n as usize], b"ef", "and off again");
        unsafe {
            libc::close(slave);
        }
    }

    /// A shell that exits is noticed: the status is in the tab's title, not a silent dead pane.
    #[test]
    fn a_shell_that_exits_is_noticed_and_titled() {
        let mut t = Tab::open(Path::new("/tmp"), 40, 6, 200).expect("a pty");
        // A shell with no PS1 prints no prompt, and a gate box may spawn one that is not
        // interactive at all until it has read a line: give it a line to run.
        t.send(b"exit 42\n");
        let mut noticed = false;
        for _ in 0..3000 {
            t.feed();
            if t.exited.is_some() {
                noticed = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(noticed, "the exit was noticed");
        assert_eq!(t.exited, Some(42), "the status is the shell's own");
        assert!(t.title.contains("42"), "the tab says so: {}", t.title);
    }

    /// A dead tab is not written to: keys after the exit never reach the pty.
    #[test]
    fn a_dead_tab_is_not_written_to() {
        let (mut t, slave) = pipe_tab();
        unsafe {
            let mut tio: libc::termios = std::mem::zeroed();
            libc::tcgetattr(slave, &mut tio);
            libc::cfmakeraw(&mut tio);
            libc::tcsetattr(slave, libc::TCSANOW, &tio);
        }
        let mut buf = [0u8; 8];
        t.send(b"a");
        let n = unsafe { libc::read(slave, buf.as_mut_ptr() as *mut _, buf.len()) };
        assert_eq!(n, 1, "a live tab takes the key");
        t.exited = Some(0);
        t.send(b"b");
        unsafe {
            libc::fcntl(slave, libc::F_SETFL, libc::O_NONBLOCK);
        }
        let n = unsafe { libc::read(slave, buf.as_mut_ptr() as *mut _, buf.len()) };
        assert!(n <= 0, "a dead tab took no key: {n}");
        unsafe {
            libc::close(slave);
        }
    }

    /// The pty is the size the strip draws: `stty size` says so, before and after a resize.
    #[test]
    fn the_pty_size_is_what_stty_reports() {
        let mut t = Tab::open(Path::new("/tmp"), 40, 6, 200).expect("a pty");
        for (cols, rows) in [(40u16, 6u16), (50, 9)] {
            if (cols, rows) != (40, 6) {
                t.resize(cols, rows);
            }
            t.send(b"stty size\n");
            let want = format!("{rows} {cols}");
            let mut saw = false;
            for _ in 0..3000 {
                t.feed();
                if t.lines().iter().any(|l| l.plain().trim().ends_with(&want)) {
                    saw = true;
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            assert!(
                saw,
                "stty size said {want}: {:?}",
                t.lines().iter().map(|l| l.plain()).collect::<Vec<_>>()
            );
        }
    }

    /// The shell is told what the parser implements well.
    #[test]
    fn the_shell_is_told_the_terminal_is_xterm_256color() {
        let mut t = Tab::open(Path::new("/tmp"), 40, 6, 200).expect("a pty");
        t.send(b"echo $TERM\n");
        let mut saw = false;
        for _ in 0..3000 {
            t.feed();
            if t.lines()
                .iter()
                .any(|l| l.plain().trim().ends_with("xterm-256color"))
            {
                saw = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(
            saw,
            "echo $TERM answered xterm-256color: {:?}",
            t.lines().iter().map(|l| l.plain()).collect::<Vec<_>>()
        );
    }

    /// `yes | head -1` ends because the writer dies on SIGPIPE: no "Broken pipe" message, and
    /// the shell answers the next command.
    #[test]
    fn a_pipeline_ends_on_sigpipe_not_a_broken_pipe_message() {
        let mut t = Tab::open(Path::new("/tmp"), 40, 6, 200).expect("a pty");
        t.send(b"yes | head -1\n");
        t.send(b"echo after-pipe\n");
        let mut answered = false;
        for _ in 0..3000 {
            t.feed();
            if t.lines()
                .iter()
                .any(|l| l.plain().trim().ends_with("after-pipe") && !l.plain().contains("echo"))
            {
                answered = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(answered, "the shell answered after the pipeline");
        let text = t
            .lines()
            .iter()
            .map(|l| l.plain())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            !text.contains("Broken pipe"),
            "no broken-pipe whine: {text}"
        );
    }

    /// A tab is a real pty with a real shell behind it: it answers `echo`.
    #[test]
    fn a_tab_answers_a_command() {
        let mut t = Tab::open(Path::new("/tmp"), 40, 6, 200).expect("a pty");
        t.send(b"echo hi\n");
        for _ in 0..40 {
            t.feed();
            std::thread::sleep(std::time::Duration::from_millis(10));
            if t.lines().iter().any(|l| l.plain().contains("hi")) {
                break;
            }
        }
        assert!(
            t.lines().iter().any(|l| l.plain().contains("hi")),
            "{:?}",
            t.lines().iter().map(|l| l.plain()).collect::<Vec<_>>()
        );
    }

    /// The bytes a key becomes: control letters are their control bytes, and the pty gets them.
    #[test]
    fn the_pty_receives_the_control_bytes() {
        let t = Tab::open(Path::new("/tmp"), 40, 6, 200).expect("a pty");
        use crate::term::{KeyCode, Mods};
        for (code, mods, want) in [
            (KeyCode::Tab, Mods::NONE, vec![0x09]),
            (KeyCode::Char('c'), Mods::CTRL, vec![0x03]),
            (KeyCode::Char('d'), Mods::CTRL, vec![0x04]),
            (KeyCode::Char('k'), Mods::CTRL, vec![0x0b]),
            (KeyCode::Delete, Mods::NONE, b"\x1b[3~".to_vec()),
            (KeyCode::Char('x'), Mods::CTRL, vec![0x18]),
        ] {
            let got = t.key_bytes(&crate::term::KeyEvent { code, mods });
            assert_eq!(got, want, "{code:?}");
        }
    }

    /// SGR sets a foreground per cell, and reset clears it.
    #[test]
    fn sgr_colours_are_kept_per_cell() {
        let (mut t, slave) = pipe_tab();
        write_all(slave, b"\x1b[31mR\x1b[0mX");
        assert!(feed_until(&mut t, |t| t.lines()[0].plain().contains('X')));
        let line = &t.lines()[0];
        let first = line.spans.first().expect("a span");
        assert_eq!(first.content, "R");
        assert_eq!(first.style.raw.fg_rgb, Some(rgb(1)), "red at cell 0");
        let rest: String = line.spans[1..].iter().map(|s| s.content.clone()).collect();
        assert!(rest.starts_with('X'), "{rest:?}");
        assert_eq!(line.spans[1].style.raw.fg_rgb, None, "reset after it");
        unsafe {
            libc::close(slave);
        }
    }

    /// What scrolled off the top is in the scrollback, and PgUp reads it.
    #[test]
    fn a_scrolled_back_line_is_in_history() {
        let (mut t, slave) = pipe_tab();
        // A program's line ends: the pty's ONLCR turns `\n` into `\r\n`, so a bare `\n` would
        // make two newlines. Ten lines in a six-row screen leave four in the scrollback.
        for i in 1..=10 {
            write_all(slave, format!("line{i}\r\n").as_bytes());
        }
        assert!(feed_until(&mut t, |t| {
            t.lines().iter().any(|l| l.plain().trim() == "line10")
        }));
        t.scroll_up(100);
        let pos = t.scroll_pos();
        assert!(pos >= 4, "the lines that scrolled off are kept: {pos}");
        assert!(
            t.lines().iter().any(|l| l.plain().trim() == "line1"),
            "the oldest line is readable: {:?}",
            t.lines().iter().map(|l| l.plain()).collect::<Vec<_>>()
        );
        unsafe {
            libc::close(slave);
        }
    }

    /// Backspace is a cursor move, and a shell erases what it leaves with `\b \b`; the caret is
    /// drawn where the next character lands.
    #[test]
    fn backspace_erases_and_the_caret_is_drawn() {
        let (mut t, slave) = pipe_tab();
        // What a readline sends for one erased character: back, blank it, back again.
        write_all(slave, b"ab\x08 \x08");
        assert!(feed_until(&mut t, |t| {
            let row = t.lines()[0].plain();
            row.starts_with('a') && !row.starts_with("ab")
        }));
        let row = t.lines()[0].plain();
        assert!(row.starts_with('a'), "{row:?}");
        assert!(!row.starts_with("ab"), "the b is erased: {row:?}");
        assert!(
            t.lines()[0].spans.iter().any(|s| {
                s.style == crate::render::Style::of(crate::style::Role::Faint).reverse()
            }),
            "a caret cell"
        );
        unsafe {
            libc::close(slave);
        }
    }

    /// `clear` erases and `seq` writes three lines: what the crate brought is the erase, so the
    /// screen holds the numbers and a prompt, not the command's echo.
    #[test]
    fn clear_and_seq_leave_three_lines_and_a_prompt() {
        let mut t = Tab::open(Path::new("/tmp"), 40, 6, 200).expect("a pty");
        // The prompt this waits for is not the shell's own: a gate box spawns one with no PS1,
        // which never prints `$ `. Set one, so what is waited for can actually arrive.
        t.send(b"PS1='$ '\n");
        assert!(feed_until(&mut t, |t| {
            t.lines()
                .iter()
                .any(|l| l.plain().trim_end().ends_with('$'))
        }));
        t.send(b"clear; seq 1 3\n");
        // The numbers and the prompt that follows them: `seq` finishing is not the prompt
        // arriving, and a gate under load can put seconds between the two.
        assert!(feed_until(&mut t, |t| {
            t.lines().iter().any(|l| l.plain().trim() == "3")
                && t.lines().iter().any(|l| {
                    let s = l.plain();
                    s.starts_with('$') || s.starts_with('#')
                })
        }));
        let shown: Vec<String> = t
            .lines()
            .iter()
            .map(|l| l.plain().trim_end().to_string())
            .collect();
        for n in ["1", "2", "3"] {
            assert_eq!(
                shown.iter().filter(|l| *l == n).count(),
                1,
                "one {n} on the screen: {shown:?}"
            );
        }
        assert!(
            !shown.iter().any(|l| l.contains("clear")),
            "the echo is erased: {shown:?}"
        );
        assert!(
            shown
                .iter()
                .any(|l| l.starts_with("$") || l.starts_with("#")),
            "a prompt waits: {shown:?}"
        );
    }

    /// The painted caret is for an UNFOCUSED strip only: a focused one's caret is the real
    /// cursor, which the frame places, and drawing both would be two. Unfocused, it is the
    /// theme's faint role REVERSED, REPLACING the cell's own style — a reverse-video cell
    /// under the caret cannot stay a bright block, and a theme can name `faint` as a colour
    /// where a terminal ignores `dim`.
    #[test]
    fn the_painted_caret_is_for_an_unfocused_strip_only() {
        let (mut t, slave) = pipe_tab();
        // A reverse-video line, then the caret at its end: the caret must REPLACE the cell's
        // reverse style, not merge with it.
        write_all(slave, b"\x1b[7mXX\x1b[0m");
        assert!(feed_until(&mut t, |t| t.lines()[0]
            .plain()
            .starts_with("XX")));
        let faint_block = crate::render::Style::of(crate::style::Role::Faint).reverse();
        let painted = |lines: Vec<Line>| lines[0].spans.iter().any(|s| s.style == faint_block);
        assert!(
            painted(t.lines_for(false)),
            "the theme's faint block over a reverse cell"
        );
        assert!(
            !painted(t.lines_for(true)),
            "no painted caret with it: the real cursor is there"
        );
        // And the span is the caret cell alone, its own text kept: the caret is the faint
        // block over the cell, not a bright block added to it.
        let painted_lines = t.lines_for(false);
        let span = painted_lines[0]
            .spans
            .iter()
            .find(|s| s.style == faint_block)
            .expect("the caret span");
        assert_eq!(
            span.content.trim(),
            "",
            "one cell, blank or not: {:?}",
            span.content
        );
        unsafe {
            libc::close(slave);
        }
    }

    /// An OSC title is not text: the parser keeps it as the window title, and the screen shows
    /// only what followed it.
    #[test]
    fn an_osc_title_is_not_text_on_the_screen() {
        let mut t = Tab::open(Path::new("/tmp"), 40, 6, 200).expect("a pty");
        // The title is built in the shell, so the command's own echo cannot spell it: what is
        // left on the screen is only what followed it.
        t.send(b"T=$(printf 'the-%s' title); printf '\\033]0;%s\\007x\\n' \"$T\"\n");
        assert!(feed_until(&mut t, |t| {
            t.lines().iter().any(|l| l.plain().contains('x'))
        }));
        let shown: Vec<String> = t.lines().iter().map(|l| l.plain()).collect();
        assert!(
            !shown.iter().any(|l| l.contains("the-title")),
            "the title never reaches the screen: {shown:?}"
        );
    }

    /// A 256-colour SGR keeps the colour it named: 196 is the cube's red, not the nearest of
    /// sixteen, which is what the hand parser could do.
    #[test]
    fn a_256_colour_sgr_keeps_its_colour() {
        let (mut t, slave) = pipe_tab();
        write_all(slave, b"\x1b[38;5;196mX\x1b[0m");
        assert!(feed_until(&mut t, |t| {
            t.lines()[0].plain().contains('X')
        }));
        let line = &t.lines()[0];
        let span = line
            .spans
            .iter()
            .find(|s| s.content.contains('X'))
            .expect("the X's span");
        assert_eq!(
            span.style.raw.fg_rgb,
            Some([255, 0, 0]),
            "196 is the cube's red"
        );
        assert_eq!(
            span.style.raw.fg_rgb,
            Some([255, 0, 0]),
            "colour 196 as RGB"
        );
        unsafe {
            libc::close(slave);
        }
    }

    /// `less` draws on the alternate screen: its first line is at column one, and PgDn reaches
    /// the program rather than rano's scrollback.
    #[test]
    fn less_shows_line_one_in_column_one_and_pages_with_pgdn() {
        let mut t = Tab::open(Path::new("/tmp"), 40, 6, 200).expect("a pty");
        t.send(b"seq 1 300 > /tmp/rano-vt-less.txt; less /tmp/rano-vt-less.txt\n");
        assert!(feed_until(&mut t, |t| {
            t.lines().iter().any(|l| l.plain().starts_with('1'))
        }));
        let top = t.lines()[0].plain();
        assert!(top.starts_with('1'), "line 1 at column one: {top:?}");
        t.send(b"\x1b[6~");
        assert!(feed_until(&mut t, |t| { t.lines()[0].plain() != top }));
        t.send(b"q");
        let _ = std::fs::remove_file("/tmp/rano-vt-less.txt");
    }

    /// `vi` draws on the alternate screen, and `:q!` gives the shell's own screen back.
    #[test]
    fn vi_quits_and_the_shells_screen_comes_back() {
        let mut t = Tab::open(Path::new("/tmp"), 40, 6, 200).expect("a pty");
        t.send(b"echo before-vi\n");
        assert!(feed_until(&mut t, |t| {
            t.lines().iter().any(|l| l.plain().contains("before-vi"))
        }));
        t.send(b"vi -u NONE /tmp/rano-vt-vi.txt\n");
        assert!(
            feed_until(&mut t, |t| {
                !t.lines().iter().any(|l| l.plain().contains("before-vi"))
            }),
            "vi's alternate screen hides the shell's"
        );
        t.send(b":q!\r");
        assert!(
            feed_until(&mut t, |t| {
                t.lines().iter().any(|l| l.plain().contains("before-vi"))
            }),
            "the shell's screen comes back"
        );
        let _ = std::fs::remove_file("/tmp/rano-vt-vi.txt");
    }

    /// The emulator is not a byte pipe: a carriage return moves the cursor, it does not type one.
    #[test]
    fn a_carriage_return_is_not_a_character() {
        let (mut t, slave) = pipe_tab();
        write_all(slave, b"ab\rX");
        assert!(feed_until(&mut t, |t| t.lines()[0].plain().contains('X')));
        let row = t.lines()[0].plain();
        assert!(row.trim_end().starts_with("Xb"), "{row:?}");
        assert!(!row.contains('\r'), "{row:?}");
        unsafe {
            libc::close(slave);
        }
    }
}
