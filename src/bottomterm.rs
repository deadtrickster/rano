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
    pub fn feed(&mut self) {
        let mut buf = [0u8; 4096];
        loop {
            let n = unsafe { libc::read(self.master, buf.as_mut_ptr() as *mut _, buf.len()) };
            if n <= 0 {
                break;
            }
            self.parser.process(&buf[..n as usize]);
        }
    }

    /// A key, as the shell's bytes.
    pub fn send(&mut self, bytes: &[u8]) {
        let _ = unsafe { libc::write(self.master, bytes.as_ptr() as *const _, bytes.len()) };
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
            let mut status = 0;
            libc::waitpid(self.pid, &mut status, 0);
        }
    }
}

/// The bottom terminal: a row of tabs, the active one drawn, and which of them has the keyboard.
pub struct BottomTerm {
    pub open: bool,
    pub focus: bool,
    pub tabs: Vec<Tab>,
    pub active: usize,
}

impl BottomTerm {
    pub fn new() -> BottomTerm {
        BottomTerm {
            open: false,
            focus: false,
            tabs: Vec::new(),
            active: 0,
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

    /// Feed every tab: a quiet shell is a cheap frame.
    pub fn feed(&mut self) {
        for t in &mut self.tabs {
            t.feed();
        }
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
    x: usize,
    y: usize,
}

impl Vt {
    fn new(cols: usize, rows: usize) -> Vt {
        Vt {
            cols,
            rows,
            cells: vec![' '; cols * rows],
            x: 0,
            y: 0,
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
                '\x08' => self.x = self.x.saturating_sub(1),
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
        }
        self.x += 1;
    }

    fn linefeed(&mut self) {
        self.x = 0;
        if self.y + 1 >= self.rows {
            self.cells.drain(..self.cols);
            self.cells.resize(self.rows * self.cols, ' ');
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
            _ => {}
        }
    }

    fn lines(&self) -> Vec<Line> {
        (0..self.rows)
            .map(|y| {
                let text: String = (0..self.cols)
                    .map(|x| self.cells[y * self.cols + x])
                    .collect();
                Line::new(vec![Span::raw(text.trim_end().to_string())])
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    /// The emulator is not a byte pipe: a carriage return moves the cursor, it does not type one.
    #[test]
    fn a_carriage_return_is_not_a_character() {
        let mut p = Vt::new(10, 3);
        p.process(b"ab\rX");
        let text: String = (0..10).map(|x| p.cells[x]).collect();
        assert_eq!(text.trim_end(), "Xb");
    }
}
