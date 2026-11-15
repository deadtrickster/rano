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
    parser: Vt,
    master: RawFd,
    pid: libc::pid_t,
    /// The tab's label: the shell's name, and whether it has exited.
    pub title: String,
}

impl Tab {
    /// Open a shell in `cwd`, its pty `cols`×`rows`. The parent's end is non-blocking.
    pub fn open(cwd: &Path, cols: u16, rows: u16) -> std::io::Result<Tab> {
        let (master, slave) = openpty(cols, rows)?;
        let pid = unsafe { libc::fork() };
        match pid {
            0 => {
                // The child is the shell, on the slave end, in the folder the editor is in.
                unsafe {
                    libc::close(master);
                    libc::setsid();
                    libc::dup2(slave, 0);
                    libc::dup2(slave, 1);
                    libc::dup2(slave, 2);
                    libc::close(slave);
                    // The pty is the child's controlling terminal: without it the shell says it
                    // has no tty and turns job control off, which is a warning on every open.
                    libc::ioctl(0, libc::TIOCSCTTY as _, 0);
                    let shell = shell();
                    let cwd = std::ffi::CString::new(cwd.as_os_str().as_encoded_bytes())
                        .expect("no NUL in a path");
                    libc::chdir(cwd.as_ptr());
                    libc::execlp(
                        shell.as_ptr(),
                        shell.as_ptr(),
                        std::ptr::null::<libc::c_char>(),
                    );
                    libc::_exit(127);
                }
            }
            p if p > 0 => {
                unsafe {
                    libc::close(slave);
                    libc::fcntl(master, libc::F_SETFL, libc::O_NONBLOCK);
                    // A child of the editor (a compiler, a test runner) must not inherit the
                    // shell's pty: it would hold the terminal open after the tab closed. The
                    // fcntl is checked, and a failure still kills and reaps the shell.
                    if libc::fcntl(master, libc::F_SETFD, libc::FD_CLOEXEC) == -1 {
                        let e = std::io::Error::last_os_error();
                        libc::close(master);
                        libc::kill(p, libc::SIGKILL);
                        let mut status = 0;
                        libc::waitpid(p, &mut status, 0);
                        return Err(e);
                    }
                }
                Ok(Tab {
                    parser: Vt::new(cols as usize, rows as usize),
                    master,
                    pid: p,
                    title: "sh".to_string(),
                })
            }
            _ => Err(std::io::Error::last_os_error()),
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
            if n <= 0 {
                break;
            }
            dirty = true;
            budget = budget.saturating_sub(n as usize);
            self.parser.process(&buf[..n as usize]);
        }
        dirty
    }

    /// A key, as the shell's bytes. Every key translates or is swallowed: a chord this cannot
    /// spell must not fall through to the editor, which would cut or indent the file behind the
    /// reader's back.
    pub fn send(&mut self, bytes: &[u8]) {
        let _ = unsafe { libc::write(self.master, bytes.as_ptr() as *const _, bytes.len()) };
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
        if self.parser.app_cursor {
            vec![0x1b, b'O', which as u8]
        } else {
            vec![0x1b, b'[', which as u8]
        }
    }

    /// Scroll back by `n` lines, capped at what the scrollback holds.
    pub fn scroll_up(&mut self, n: usize) {
        self.parser.scroll = (self.parser.scroll + n).min(self.parser.history.len());
    }

    /// Scroll forward by `n` lines, back to the live screen at zero.
    pub fn scroll_down(&mut self, n: usize) {
        self.parser.scroll = self.parser.scroll.saturating_sub(n);
    }

    /// The screen, in the panel's rows.
    pub fn lines(&self) -> Vec<Line> {
        self.parser.lines()
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
    pub focus: bool,
    pub tabs: Vec<Tab>,
    pub active: usize,
    /// The strip's height in rows, remembered across toggles: a terminal the operator has resized
    /// stays that size.
    pub height: usize,
}

impl BottomTerm {
    pub fn new() -> BottomTerm {
        BottomTerm {
            open: false,
            focus: false,
            tabs: Vec::new(),
            active: 0,
            height: 12,
        }
    }

    /// Open a tab if there are none, and show it. The first tab opens in the editor's folder.
    pub fn toggle(&mut self, cwd: &Path, cols: u16, rows: u16) -> std::io::Result<()> {
        self.open = !self.open;
        self.focus = self.open;
        if self.open && self.tabs.is_empty() {
            self.tabs.push(Tab::open(cwd, cols, rows)?);
        }
        Ok(())
    }

    pub fn new_tab(&mut self, cwd: &Path, cols: u16, rows: u16) -> std::io::Result<()> {
        self.tabs.push(Tab::open(cwd, cols, rows)?);
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
            self.focus = false;
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
        let mut out = Vec::new();
        if self.tabs.is_empty() {
            out.push(Line::new(vec![Span::role(
                format!("no terminal open — M-n opens one in {}", cwd.display()),
                Role::Faint,
            )]));
            return out;
        }
        let mut strip = String::from("tabs ");
        for (i, _) in self.tabs.iter().enumerate() {
            let mark = if i == self.active { ">" } else { " " };
            strip.push_str(&format!("{mark}{} ", i + 1));
        }
        strip.push_str("(M-n new, M-[ M-] switch, M-w close)");
        // A scroll position that is off the bottom says so: the view is not the newest output, and
        // that must be visible or a reader thinks the shell is quiet.
        if self.tabs[self.active].parser.scroll > 0 {
            strip.push_str(&format!(" ↑{} more", self.tabs[self.active].parser.scroll));
        }
        out.push(Line::new(vec![Span::role(
            crate::width::text::truncate(&strip, w),
            Role::Faint,
        )]));
        out.extend(
            self.tabs[self.active]
                .lines()
                .into_iter()
                .map(|l| crate::render::text::truncate(&l, w)),
        );
        out
    }
}

impl Default for BottomTerm {
    fn default() -> Self {
        Self::new()
    }
}

/// The pty pair, sized to the panel. `openpty` is libc's, since that is the only C this needs.
fn openpty(cols: u16, rows: u16) -> std::io::Result<(RawFd, RawFd)> {
    let mut master: libc::c_int = 0;
    let mut slave: libc::c_int = 0;
    let size = libc::winsize {
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
            &size,
        )
    };
    if rc != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok((master, slave))
}

/// The shell to open: `$SHELL` if it names one, `/bin/sh` otherwise.
fn shell() -> std::ffi::CString {
    let s = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());
    std::ffi::CString::new(s).expect("no NUL in a shell's path")
}

/// A VT100-subset emulator, in-tree because this box's registry is read-only: printable UTF-8,
/// CR/LF/BS/TAB, cursor motion, erase in line and in display, and scrolling. A shell and its output
/// is what it is for; a full-screen program wants more, and that is a later piece, not a silent gap.
struct Vt {
    cols: usize,
    rows: usize,
    cells: Vec<char>,
    /// The foreground SGR chose for each cell, and whether it is bold.
    fg: Vec<Option<[u8; 3]>>,
    bold: Vec<bool>,
    /// The lines that scrolled off the top, for PgUp/PgDn: a scrollback, not a blank pane.
    history: Vec<String>,
    /// How far back the view is scrolled: 0 is the live screen.
    scroll: usize,
    x: usize,
    y: usize,
    /// The style SGR is currently setting, applied to every character that follows.
    cur_fg: Option<[u8; 3]>,
    cur_bold: bool,
    /// DECCKM: application cursor mode, which makes arrows SS3 rather than CSI.
    pub app_cursor: bool,
}

impl Vt {
    fn new(cols: usize, rows: usize) -> Vt {
        Vt {
            cols,
            rows,
            cells: vec![' '; cols * rows],
            fg: vec![None; cols * rows],
            bold: vec![false; cols * rows],
            history: Vec::new(),
            scroll: 0,
            x: 0,
            y: 0,
            cur_fg: None,
            cur_bold: false,
            app_cursor: false,
        }
    }

    fn process(&mut self, bytes: &[u8]) {
        let text = String::from_utf8_lossy(bytes);
        let mut it = text.chars().peekable();
        while let Some(c) = it.next() {
            match c {
                '\x1b' => self.escape(&mut it),
                '\r' => self.x = 0,
                '\n' => self.linefeed(),
                // Backspace erases as well as moves: the cell under the new cursor position is
                // blanked, or a delete happens only in the shell and never on the glass.
                '\x08' => {
                    self.x = self.x.saturating_sub(1);
                    self.cells[self.y * self.cols + self.x] = ' ';
                }
                '\t' => {
                    self.x = ((self.x / 8) + 1) * 8;
                    if self.x >= self.cols {
                        self.x = self.cols - 1;
                    }
                }
                c if !c.is_control() => self.put(c),
                _ => {}
            }
        }
    }

    fn put(&mut self, c: char) {
        if self.x >= self.cols {
            self.linefeed();
        }
        let i = self.y * self.cols + self.x;
        if i < self.cells.len() {
            self.cells[i] = c;
            self.fg[i] = self.cur_fg;
            self.bold[i] = self.cur_bold;
        }
        self.x += 1;
    }

    fn linefeed(&mut self) {
        self.x = 0;
        if self.y + 1 >= self.rows {
            // The top row is scrollback now, not lost: that is what PgUp reads.
            let top: String = (0..self.cols).map(|x| self.cells[x]).collect();
            self.history.push(top.trim_end().to_string());
            if self.history.len() > 200 {
                self.history.remove(0);
            }
            self.cells.drain(..self.cols);
            self.cells.resize(self.rows * self.cols, ' ');
            self.fg.drain(..self.cols);
            self.fg.resize(self.rows * self.cols, None);
            self.bold.drain(..self.cols);
            self.bold.resize(self.rows * self.cols, false);
        } else {
            self.y += 1;
        }
    }

    fn escape(&mut self, it: &mut std::iter::Peekable<std::str::Chars>) {
        let mut params = String::new();
        for c in it.by_ref() {
            if c.is_ascii_alphabetic() {
                self.csi(&params, c);
                return;
            }
            params.push(c);
            if params.len() > 16 {
                return;
            }
        }
    }

    fn csi(&mut self, params: &str, last: char) {
        // CSI's own bracket is not a parameter: `\x1b[31m` is 31, not "[31" — a parse that
        // includes it silently reads 0 and resets the colour instead of setting it.
        let params = params.trim_start_matches('[');
        let n = |d: usize| {
            params
                .split(';')
                .next()
                .and_then(|s| s.parse().ok())
                .unwrap_or(d)
        };
        match last {
            'A' => self.y = self.y.saturating_sub(n(1)),
            'B' => self.y = (self.y + n(1)).min(self.rows - 1),
            'C' => self.x = (self.x + n(1)).min(self.cols - 1),
            'D' => self.x = self.x.saturating_sub(n(1)),
            'H' | 'f' => {
                let mut p = params.split(';');
                self.y = p
                    .next()
                    .and_then(|s| s.parse::<usize>().ok())
                    .unwrap_or(1)
                    .saturating_sub(1)
                    .min(self.rows - 1);
                self.x = p
                    .next()
                    .and_then(|s| s.parse::<usize>().ok())
                    .unwrap_or(1)
                    .saturating_sub(1)
                    .min(self.cols - 1);
            }
            'J' => {
                if params.parse::<usize>().unwrap_or(0) == 2 {
                    self.cells.fill(' ');
                    self.x = 0;
                    self.y = 0;
                }
            }
            'K' => {
                let start = self.y * self.cols + self.x;
                for i in start..(start + self.cols - self.x) {
                    if i < self.cells.len() {
                        self.cells[i] = ' ';
                    }
                }
            }
            'm' => self.sgr(params),
            _ => {}
        }
    }

    /// SGR: the codes a shell actually emits — reset, bold, the sixteen foreground colours.
    /// Anything else is left alone, which is the honest subset for `ls --color` and git.
    fn sgr(&mut self, params: &str) {
        for part in params.split(';') {
            let n: usize = part.parse().unwrap_or(0);
            match n {
                0 => {
                    self.cur_fg = None;
                    self.cur_bold = false;
                }
                1 => self.cur_bold = true,
                30..=37 => self.cur_fg = Some(rgb(n - 30)),
                90..=97 => self.cur_fg = Some(rgb(n - 90 + 8)),
                39 => self.cur_fg = None,
                _ => {}
            }
        }
    }

    fn lines(&self) -> Vec<Line> {
        if self.scroll > 0 {
            return self.history_lines();
        }
        (0..self.rows).map(|y| self.row_line(y)).collect()
    }

    /// One row of the live screen, styled per cell.
    fn row_line(&self, y: usize) -> Line {
        let mut spans: Vec<Span> = Vec::new();
        let mut text = String::new();
        let mut style = crate::render::Style::new();
        for x in 0..self.cols {
            let i = y * self.cols + x;
            let cell = self.cells[i];
            let mut st = crate::render::Style::new();
            if let Some(rgb) = self.fg[i] {
                st.raw.fg_rgb = Some(rgb);
            }
            if self.bold[i] {
                st.attrs = crate::style::Attrs::BOLD;
            }
            if x == self.x && y == self.y {
                if !text.is_empty() {
                    spans.push(Span::styled(std::mem::take(&mut text), style.clone()));
                }
                spans.push(Span::styled(
                    cell.to_string(),
                    crate::render::Style::new().reverse(),
                ));
                style = st;
            } else {
                if st != style {
                    if !text.is_empty() {
                        spans.push(Span::styled(std::mem::take(&mut text), style.clone()));
                    }
                    style = st;
                }
                text.push(cell);
            }
        }
        if !text.is_empty() {
            spans.push(Span::styled(text, style));
        }
        if spans.is_empty() {
            spans.push(Span::raw(" ".to_string()));
        }
        Line::new(spans)
    }

    /// The scrollback window: history rows, ending `scroll` lines back from the newest.
    fn history_lines(&self) -> Vec<Line> {
        let end = self.history.len().saturating_sub(self.scroll);
        let start = end.saturating_sub(self.rows);
        self.history[start..end]
            .iter()
            .map(|t| Line::raw(t.clone()))
            .collect()
    }
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
                parser: Vt::new(40, 6),
                master,
                pid,
                title: "t".to_string(),
            },
            slave,
        )
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
            parser: Vt::new(40, 6),
            master,
            pid,
            title: "t".to_string(),
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
        for _ in 0..100 {
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
        let t = Tab::open(Path::new("/tmp"), 40, 6).expect("a pty");
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

    /// A tab is a real pty with a real shell behind it: it answers `echo`.
    #[test]
    fn a_tab_answers_a_command() {
        let mut t = Tab::open(Path::new("/tmp"), 40, 6).expect("a pty");
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
        let t = Tab::open(Path::new("/tmp"), 40, 6).expect("a pty");
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
        let mut p = Vt::new(10, 2);
        p.process(b"\x1b[31mR\x1b[0mX");
        println!("fg={:?} cur={:?}", p.fg[0], p.cur_fg);
        assert_eq!(p.fg[0], Some(rgb(1)), "red at cell 0");
        assert_eq!(p.fg[1], None, "reset after it");
    }

    /// What scrolled off the top is in the scrollback, and PgUp reads it.
    #[test]
    fn a_scrolled_back_line_is_in_history() {
        let mut p = Vt::new(10, 2);
        p.process(b"one\ntwo\nthree\n");
        assert_eq!(p.history.len(), 2, "both scrolled-off lines are scrollback");
        p.scroll = 1;
        assert!(p.lines().iter().any(|l| l.plain().contains("one")));
    }

    /// Backspace erases as well as moves, and the caret is drawn where the next character lands.
    #[test]
    fn backspace_erases_and_the_caret_is_drawn() {
        let mut p = Vt::new(10, 2);
        p.process(b"ab");
        p.process(b"\x08");
        let row: String = (0..10).map(|x| p.cells[x]).collect();
        assert_eq!(row, "a         ", "the b is erased, not merely moved over");
        let lines = p.lines();
        assert!(
            lines[0]
                .spans
                .iter()
                .any(|s| s.style.attrs.contains(crate::style::Attrs::REVERSE)),
            "a caret cell"
        );
    }

    /// The emulator is not a byte pipe: a carriage return moves the cursor, it does not type one.
    #[test]
    fn a_carriage_return_is_not_a_character() {
        let mut p = Vt::new(10, 3);
        p.process(b"ab\rX");
        let text: String = (0..10).map(|x| p.cells[x]).collect();
        assert_eq!(text.trim_end(), "Xb");
    }
}
