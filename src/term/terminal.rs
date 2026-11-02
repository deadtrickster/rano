//! Raw mode, the alternate screen, the window size, reading input, and the row-diffing
//! painter. Fifty lines of `termios` rather than a TUI framework, because a framework's
//! event loop would be the second one in the process. Every mode `enter` turns on is
//! turned off by `restore`: on `Drop`, and in the panic hook before the message prints.

use std::io::{Read, Write};
use std::os::fd::AsRawFd;

use super::decode::{decode_prefix, legacy_bytes, paste_open};
use super::event::Event;
use super::features::Features;
use crate::render::{Buffer, Palette};

/// Mouse tracking, as [`Terminal::enter_with`] turns it on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mouse {
    /// None: the terminal's own selection works without Shift.
    Off,
    /// Presses, releases, drags and the wheel (`?1002`); the wheel scrolls, and selecting
    /// text stays the terminal's own Shift+drag.
    #[default]
    Buttons,
    /// Every motion too (`?1003`), for hover.
    Any,
}

/// The modes [`Terminal::enter_with`] turns on beyond what every full-screen program needs
/// (alternate screen, bracketed paste, the title stack).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Options {
    pub mouse: Mouse,
    /// A steady block cursor (`CSI 2 SP q`), given back as the terminal's own on exit: a
    /// composer whose whole affordance is the caret cannot use a one-pixel bar.
    pub block_cursor: bool,
}

impl Default for Options {
    fn default() -> Options {
        Options {
            mouse: Mouse::Buttons,
            block_cursor: true,
        }
    }
}

pub struct Terminal {
    original: libc::termios,
    fd: i32,
    entered: bool,
    /// The window title last written (see [`Terminal::set_title`]), so a frame that does
    /// not change it writes nothing.
    title: std::cell::RefCell<String>,
    /// The frame currently on the glass. [`Terminal::draw`] writes the difference
    /// against it and nothing else; see the note on flicker.
    shown: std::cell::RefCell<Vec<String>>,
    /// The glass state being built, kept between frames so its rows keep their buffers.
    /// A second buffer rather than in-place editing of `shown`, because a write can fail
    /// partway and `shown` must only ever record what actually reached the glass.
    next: std::cell::RefCell<Vec<String>>,
    /// Where the cursor was left, so an unchanged frame does not even move it.
    cursor: std::cell::Cell<Option<(usize, usize)>>,
    /// Frames drawn, and frames that needed no bytes at all. Kept in the shipping type
    /// because "is it repainting when nothing changed" is unanswerable after the fact.
    frames: std::cell::Cell<u64>,
    silent: std::cell::Cell<u64>,
    /// The rest of the encoder: bytes, row rewrites, repeated payloads, screen
    /// erases. See [`WriteStats`].
    stats: std::cell::Cell<WriteStats>,
    /// The previous frame's payload, kept only while `stats_to` is set — a clone
    /// per frame is not something an uninstrumented head should pay for.
    prev_payload: std::cell::RefCell<String>,
    /// `RANO_WRITE_STATS`: a path to write the line to on restore, or `-`
    /// for stderr. `None` switches the whole encoder off.
    stats_to: Option<String>,
    /// Bytes read but not yet decodable: a partial UTF-8 sequence, an escape
    /// that arrived in halves, or a bracketed paste whose terminator has not
    /// come. Carried to the next read rather than dropped.
    pending: std::cell::RefCell<Vec<u8>>,
    /// The bytes the last [`Terminal::events`] call consumed, verbatim; see
    /// [`Terminal::raw_input`] for who reads this and why an [`Event`] cannot be re-encoded.
    last_raw: std::cell::RefCell<Vec<u8>>,
    /// The terminal size the last frame was painted at, and whether the next
    /// frame must erase everything before it paints.
    last_size: std::cell::Cell<(usize, usize)>,
    full: std::cell::Cell<bool>,
    /// What this terminal speaks beyond cells — see [`super::features`]. Decided once at
    /// [`Terminal::enter`], and every mode it turns on there is turned off by [`restore`].
    features: Features,
    /// The modes `enter` turned on, so `restore` turns off exactly those.
    options: Options,
    /// The progress state last written (OSC 9;4), so a tick that does not change it writes
    /// nothing.
    progress: std::cell::Cell<Progress>,
}

/// The tab's progress bar (OSC 9;4), as the head means it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Progress {
    /// Nothing to show: the bar is removed.
    #[default]
    Idle,
    /// The model is working. No percentage exists, so the bar is the indeterminate one.
    Busy,
    /// Something waits on the person. Drawn in the paused colour, so a tab that wants you
    /// looks different from a tab that is working.
    Waiting,
    /// The last turn ended in an error.
    Failed,
}

impl Progress {
    /// The OSC 9;4 state and value for this.
    fn osc(self) -> &'static [u8] {
        match self {
            Progress::Idle => b"\x1b]9;4;0\x07",
            Progress::Busy => b"\x1b]9;4;3\x07",
            Progress::Waiting => b"\x1b]9;4;4;100\x07",
            Progress::Failed => b"\x1b]9;4;2;100\x07",
        }
    }
}

/// How much is read at once: large enough that a paste is one or two reads, and a
/// ceiling rather than a promise, since [`Terminal::events`] loops while bytes keep coming.
const READ_CHUNK: usize = 8192;

/// A hard ceiling on the carry, so a paste that never closes cannot grow it without
/// bound. Past it the carry is decoded as-is — visibly wrong beats invisibly unbounded.
const MAX_PENDING: usize = 4 * 1024 * 1024;

/// What one run wrote to the terminal. `rows` is the quantity that destroys a
/// selection, which is over drawn cells, so it counts the `ESC[K` that begins each row
/// rewrite — [`paint_full`] emits exactly one per row repainted and nothing else does.
#[derive(Default, Clone, Copy, Debug, PartialEq, Eq)]
pub struct WriteStats {
    /// Calls to [`Terminal::draw_with_cursor`].
    pub frames: u64,
    /// Of those, the ones that wrote no bytes at all.
    pub silent: u64,
    /// Bytes written to stdout, the `?2026` wrappers included.
    pub bytes: u64,
    /// Row rewrites — the cells a selection would have been sitting on.
    pub rows: u64,
    /// Frames whose payload was byte-identical to the previous written one.
    pub repeats: u64,
    /// Whole-screen erases (`ESC[2J`): a resize, or Ctrl-L.
    pub clears: u64,
}

impl WriteStats {
    /// One line, so a replay can be diffed against another replay.
    pub fn line(&self) -> String {
        let per = if self.frames > self.silent {
            self.bytes as f64 / (self.frames - self.silent) as f64
        } else {
            0.0
        };
        format!(
            "frames={} silent={} written={} bytes={} bytes_per_written={:.1} \
             rows={} repeats={} clears={}",
            self.frames,
            self.silent,
            self.frames - self.silent,
            self.bytes,
            per,
            self.rows,
            self.repeats,
            self.clears,
        )
    }
}

impl Terminal {
    /// Put the terminal in raw mode on the alternate screen, with letibot's modes
    /// ([`Options::default`]). `Err` when stdin is not a tty — the `--replay` and CI case —
    /// so the caller renders once to stdout instead of failing.
    pub fn enter() -> std::io::Result<Terminal> {
        Terminal::enter_with(Options::default())
    }

    /// [`Terminal::enter`], choosing the mouse mode and the cursor shape.
    pub fn enter_with(options: Options) -> std::io::Result<Terminal> {
        let fd = std::io::stdin().as_raw_fd();
        if unsafe { libc::isatty(fd) } != 1 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "stdin is not a terminal",
            ));
        }
        let mut original: libc::termios = unsafe { std::mem::zeroed() };
        if unsafe { libc::tcgetattr(fd, &mut original) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        let mut raw = original;
        unsafe { libc::cfmakeraw(&mut raw) };
        // A 100 ms read timeout so the render loop can also service the network
        // without a second thread poking at stdin.
        raw.c_cc[libc::VMIN] = 0;
        raw.c_cc[libc::VTIME] = 1;
        if unsafe { libc::tcsetattr(fd, libc::TCSANOW, &raw) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        let mut out = std::io::stdout();
        // Alternate screen, cursor hidden until a frame says where it goes, bracketed paste
        // so a paste is one key, and the title stack (`CSI 22;0 t`) because the head sets its
        // own title and gives the terminal back the one it had.
        //
        // `let _ =` on every write: there is nowhere to report to, and a lost mode sequence
        // at startup is not worth aborting an editor over.
        let _ = out.write_all(b"\x1b[22;0t\x1b[?1049h\x1b[?25l\x1b[?2004h");
        match options.mouse {
            Mouse::Off => {}
            Mouse::Buttons => {
                let _ = out.write_all(b"\x1b[?1002h\x1b[?1006h");
            }
            Mouse::Any => {
                let _ = out.write_all(b"\x1b[?1003h\x1b[?1006h");
            }
        }
        if options.block_cursor {
            let _ = out.write_all(b"\x1b[2 q");
        }
        // The terminal's extras, each only where it is spoken (see `super::features`):
        // `CSI > 1 u` disambiguates the kitty keyboard (Shift+Enter becomes a key),
        // `?1004h` asks for focus reports, and `OSC 11 ; ?` asks the background colour once.
        let features = Features::detect();
        if features.keys {
            let _ = out.write_all(b"\x1b[>1u");
        }
        if features.notify {
            let _ = out.write_all(b"\x1b[?1004h");
        }
        if features.background {
            let _ = out.write_all(b"\x1b]11;?\x1b\\");
        }
        let _ = out.flush();

        // Restore before anything is printed, or the panic message is a staircase.
        let saved = original;
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            restore(fd, &saved, features, options);
            prev(info);
        }));

        Ok(Terminal {
            original,
            fd,
            entered: true,
            title: std::cell::RefCell::new(String::new()),
            shown: std::cell::RefCell::new(Vec::new()),
            next: std::cell::RefCell::new(Vec::new()),
            cursor: std::cell::Cell::new(None),
            frames: std::cell::Cell::new(0),
            silent: std::cell::Cell::new(0),
            stats: std::cell::Cell::new(WriteStats::default()),
            prev_payload: std::cell::RefCell::new(String::new()),
            stats_to: std::env::var("RANO_WRITE_STATS")
                .ok()
                .or_else(|| std::env::var("LETIBOT_TUI_WRITE_STATS").ok())
                .filter(|s| !s.is_empty()),
            pending: std::cell::RefCell::new(Vec::new()),
            last_raw: std::cell::RefCell::new(Vec::new()),
            last_size: std::cell::Cell::new((0, 0)),
            full: std::cell::Cell::new(true),
            features,
            options,
            progress: std::cell::Cell::new(Progress::Idle),
        })
    }

    /// What this terminal was found to speak. See [`super::features`].
    pub fn features(&self) -> Features {
        self.features
    }

    /// The tab's progress bar, written only when it changed and only where [`Features::progress`]
    /// says it is spoken: a terminal that does not know OSC 9;4 may read OSC 9 as a notification.
    pub fn set_progress(&self, p: Progress) {
        if !self.features.progress || self.progress.get() == p {
            return;
        }
        let mut out = std::io::stdout();
        let _ = out.write_all(p.osc());
        let _ = out.flush();
        self.progress.set(p);
    }

    /// A desktop notification (OSC 9), where spoken. The text is stripped of control
    /// characters — it carries a session's words, and an escape in it would be one the
    /// terminal executes.
    pub fn notify(&self, text: &str) {
        if !self.features.notify {
            return;
        }
        let clean: String = window_title_text(text).replace(';', ",");
        if clean.is_empty() {
            return;
        }
        let mut out = std::io::stdout();
        let _ = write!(out, "\x1b]9;{clean}\x07");
        let _ = out.flush();
    }

    /// Put text on the system clipboard (OSC 52), where spoken. Base64 of the bytes as they
    /// are: the clipboard is the operator's, and what they asked to copy is what lands.
    pub fn copy(&self, text: &str) -> bool {
        if !self.features.clipboard {
            return false;
        }
        let mut out = std::io::stdout();
        let _ = write!(out, "\x1b]52;c;{}\x07", base64(text.as_bytes()));
        let _ = out.flush();
        true
    }

    /// Write bytes that are not part of the frame — an inline image's upload — outside the
    /// diffing encoder, which only knows rows of text.
    pub fn write_raw(&self, bytes: &[u8]) {
        let mut out = std::io::stdout();
        let _ = out.write_all(bytes);
        let _ = out.flush();
    }

    /// Columns and rows, or a sane default when `TIOCGWINSZ` says nothing.
    pub fn size(&self) -> (usize, usize) {
        let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
        if unsafe { libc::ioctl(self.fd, libc::TIOCGWINSZ, &mut ws) } == 0
            && ws.ws_col > 0
            && ws.ws_row > 0
        {
            (ws.ws_col as usize, ws.ws_row as usize)
        } else {
            (100, 30)
        }
    }

    /// Wait up to `timeout` for input, and say whether any is there to read.
    ///
    /// [`Terminal::events`] waits ~100 ms when nothing comes, which any host with a shorter
    /// deadline cannot afford, so such a host polls here and reads only when this says so.
    pub fn poll(&self, timeout: std::time::Duration) -> bool {
        let mut fds = libc::pollfd {
            fd: self.fd,
            events: libc::POLLIN,
            revents: 0,
        };
        let ms = timeout.as_millis().min(i32::MAX as u128) as i32;
        let n = unsafe { libc::poll(&mut fds, 1, ms) };
        n > 0 && fds.revents & libc::POLLIN != 0
    }

    /// Read whatever input is available, as events. Returns after at most ~100 ms of quiet.
    ///
    /// The loop continues while the last read filled the buffer — the only signal there is
    /// under `VMIN=0 VTIME=1` — or while a bracketed paste is open. Whatever is left
    /// undecodable is carried, not dropped.
    pub fn events(&self) -> Vec<Event> {
        let mut buf = [0u8; READ_CHUNK];
        let mut pending = self.pending.borrow_mut();
        // Cleared first, and that is not tidiness: a stale carry would be re-forwarded to
        // the pane on the next read, so this is the one place it is allowed to be stale.
        self.last_raw.borrow_mut().clear();
        while let Ok(n) = std::io::stdin().read(&mut buf) {
            if n == 0 {
                break;
            }
            pending.extend_from_slice(&buf[..n]);
            let more = n == buf.len() || (paste_open(&pending) && pending.len() < MAX_PENDING);
            if !more {
                break;
            }
        }
        if pending.is_empty() {
            return Vec::new();
        }
        let force = pending.len() >= MAX_PENDING;
        let (keys, used) = decode_prefix(&pending, force);
        // What the decoding consumed, before it is dropped. See [`Terminal::raw_input`].
        self.last_raw
            .borrow_mut()
            .extend_from_slice(&pending[..used]);
        pending.drain(..used);
        keys
    }

    /// The bytes the last [`Terminal::events`] call consumed, verbatim.
    ///
    /// A pane needs them because an [`Event`] is a lossy reading: `ESC [ A` and `ESC O A`
    /// are both Up and different byte strings, and a paste has lost its markers.
    pub fn raw_input(&self) -> Vec<u8> {
        // Under the kitty keyboard the raw stream is in a spelling the pane's program never
        // asked for (Ctrl-C arrives as `CSI 99;5u`), so it is put back into legacy bytes.
        if self.features.keys {
            legacy_bytes(&self.last_raw.borrow())
        } else {
            self.last_raw.borrow().clone()
        }
    }

    /// The window title — the session's name, so a tab says which conversation it is rather
    /// than the name of the program, which every tab shares.
    ///
    /// Control characters are dropped first: anyone with the socket can set a session title,
    /// and an escape inside one would be executed by the terminal.
    pub fn set_title(&self, title: &str) {
        let clean: String = window_title_text(title);
        if *self.title.borrow() == clean {
            return;
        }
        let mut out = std::io::stdout();
        let _ = write!(out, "\x1b]2;{clean}\x07");
        let _ = out.flush();
        *self.title.borrow_mut() = clean;
    }

    /// Paint the difference between this frame and the one on the glass.
    ///
    /// Only rows whose text changed are written, and a frame equal to the last one writes
    /// zero bytes: a full repaint ten times a second flicks, destroys a selection and pins
    /// a core. A resize forces one full repaint, because every row moved.
    pub fn draw(&self, lines: &[String]) {
        self.draw_with_cursor(lines, None)
    }

    /// A cell buffer, painted: its rows emitted under `palette` (see `crate::render::emit`)
    /// and handed to [`Terminal::draw_with_cursor`]. The buffer's rows are the screen's from
    /// the top.
    pub fn draw_buffer(&self, buf: &Buffer, palette: Palette, cursor: Option<(usize, usize)>) {
        let rows = buf.emit(palette);
        self.draw_with_cursor(&rows, cursor)
    }

    /// As [`Terminal::draw`], with the terminal's own cursor parked at `(row, col)`, zero-based,
    /// and made visible there: a text field with no caret reads as "not listening".
    pub fn draw_with_cursor(&self, lines: &[String], cursor: Option<(usize, usize)>) {
        // Counted per attempt, which is what "frames drawn" means: the caller asked for a
        // frame and one was composed. Whether its bytes reached the glass is `paint_to`'s
        // question, and a different number.
        self.frames.set(self.frames.get() + 1);
        let mut out = std::io::stdout();
        let _ = self.paint_to(&mut out, lines, cursor);
    }

    /// One pass: build the bytes, write them, and adopt the glass state they leave — or
    /// adopt nothing at all.
    ///
    /// A `write_all` that fails may have written any prefix, so the head cannot know which
    /// rows landed: a failed write clears the memory and forces a full repaint. `writers` are
    /// split out so the decision is testable without a pty. Returns whether the glass was
    /// invalidated.
    fn paint_to(
        &self,
        out: &mut dyn std::io::Write,
        lines: &[String],
        cursor: Option<(usize, usize)>,
    ) -> bool {
        let mut st = self.stats.get();
        st.frames += 1;
        // A resize really does move every row: the terminal reflowed the glass and this
        // head's memory of it is fiction. A frame that merely changed height — the composer
        // wrapping — is not that, and erasing the screen for it is a flash per wrap.
        let size = self.size();
        if size != self.last_size.get() {
            self.last_size.set(size);
            self.full.set(true);
        }
        // The two buffers are EXCHANGED, not copied: the scratch is filled from the frame and
        // then becomes the memory of the glass, so both keep the rows they have allocated.
        let s = {
            let mut next = self.next.borrow_mut();
            paint_full(
                &self.shown.borrow(),
                lines,
                &mut next,
                cursor,
                self.cursor.get(),
                self.full.replace(false),
            )
        };
        if s.is_empty() {
            self.silent.set(self.silent.get() + 1);
            st.silent += 1;
            self.stats.set(st);
            // Adopted, and nothing was written: the frame needed no bytes, so the memory it
            // describes is already true — and taking it keeps `shown` the frame's length.
            self.adopt_next();
            return false;
        }
        // The encoder. `?2026h` and `?2026l` are bytes the terminal really is sent, so they
        // are counted rather than discounted as chrome.
        let wrote = out
            .write_all(b"\x1b[?2026h")
            .and_then(|()| out.write_all(s.as_bytes()))
            .and_then(|()| out.write_all(b"\x1b[?2026l"))
            .and_then(|()| out.flush());
        if wrote.is_err() {
            // No record of a frame that may not be on the glass: cleared and marked unknown,
            // so the next frame is a full repaint.
            self.shown.borrow_mut().clear();
            self.full.set(true);
            self.stats.set(st);
            return true;
        }
        // The bytes are out, so now the memory is true: the adoption is one statement later
        // than it was, and that statement is the difference between a diff against the glass
        // and a diff against an intention.
        st.bytes += s.len() as u64 + 16;
        st.rows += s.matches("\x1b[K").count() as u64;
        st.clears += s.matches("\x1b[2J").count() as u64;
        if self.stats_to.is_some() {
            let mut prev = self.prev_payload.borrow_mut();
            if *prev == s {
                st.repeats += 1;
            }
            prev.clear();
            prev.push_str(&s);
        }
        self.stats.set(st);
        self.adopt_next();
        self.cursor.set(cursor);
        false
    }

    /// The built state becomes the memory of the glass, in one exchange.
    ///
    /// A swap rather than a fresh `Vec`: the buffer `shown` gives up is the scratch the next
    /// frame rebuilds into, so its rows keep their allocations.
    fn adopt_next(&self) {
        self.shown.swap(&self.next);
    }

    /// Forget what is on the glass, so the next draw repaints everything. Ctrl-L, and the
    /// only honest answer to "something else wrote to my terminal".
    pub fn invalidate(&self) {
        self.shown.borrow_mut().clear();
        self.full.set(true);
    }

    /// Frames drawn, and how many of those wrote nothing.
    pub fn frame_counts(&self) -> (u64, u64) {
        (self.frames.get(), self.silent.get())
    }

    /// Everything this run wrote. See [`WriteStats`].
    pub fn write_stats(&self) -> WriteStats {
        self.stats.get()
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        if self.entered {
            restore(self.fd, &self.original, self.features, self.options);
        }
        // After the restore, so the line lands on a terminal out of raw mode and off the
        // alternate screen: a report printed before it scrolls away with the screen.
        if let Some(to) = self.stats_to.clone() {
            let line = format!("write stats: {}\n", self.stats.get().line());
            if to == "-" || to == "1" {
                let _ = std::io::stderr().write_all(line.as_bytes());
            } else {
                use std::io::Write as _;
                if let Ok(mut f) = std::fs::File::create(&to) {
                    let _ = f.write_all(line.as_bytes());
                }
            }
        }
    }
}

/// The bytes that turn `shown` into `lines`, and the memory of the glass those bytes would
/// leave behind.
///
/// Two values rather than one, so a caller that dies before writing holds the old memory:
/// there is no window in which the head believes a byte it has not sent.
pub fn paint(
    shown: &[String],
    lines: &[String],
    cursor: Option<(usize, usize)>,
    prev_cursor: Option<(usize, usize)>,
) -> (String, Vec<String>) {
    let mut next = Vec::new();
    let s = paint_full(
        shown,
        lines,
        &mut next,
        cursor,
        prev_cursor,
        shown.is_empty(),
    );
    (s, next)
}

/// As [`paint`], with `full` forcing a whole-screen erase first.
///
/// A frame that grew a row is not a resize: the composer grows every time a prompt wraps,
/// and erasing for that is a flash per wrap. Rows are addressed absolutely anyway.
pub fn paint_full(
    shown: &[String],
    lines: &[String],
    next: &mut Vec<String>,
    cursor: Option<(usize, usize)>,
    prev_cursor: Option<(usize, usize)>,
    full: bool,
) -> String {
    let mut s = String::new();
    if full {
        s.push_str("\x1b[2J");
    }
    // Rows the frame no longer has: erase them, rather than the whole screen. Over `shown`,
    // because what is on the glass is what must be erased; `next` is a scratch.
    for i in lines.len()..shown.len() {
        s.push_str(&format!("\x1b[{};1H\x1b[0m\x1b[K", i + 1));
    }
    // `next` becomes this frame, and a row it already holds costs nothing: two frames of
    // identical text leave every row untouched, and a changed row gets `clear()` + `push_str`
    // into the buffer it already had. The comparison is against the buffer, not `shown`, since
    // `next` decides whether a copy is needed; `shown` decides what goes on the wire.
    next.resize(lines.len(), String::new());
    for (i, l) in lines.iter().enumerate() {
        let on_the_glass = !full && shown.get(i) == Some(l);
        if !on_the_glass {
            s.push_str(&format!("\x1b[{};1H\x1b[0m\x1b[K", i + 1));
            s.push_str(l);
        }
        if next[i] != *l {
            next[i].clear();
            next[i].push_str(l);
        }
    }
    if s.is_empty() && cursor == prev_cursor {
        return String::new();
    }
    // The cursor is hidden for the write and shown at its destination, and the hide is the
    // half that matters: without it the cursor stays live between one frame's park and the
    // next frame's writes, so a terminal that paints as bytes arrive shows it wherever the
    // last row-write left it. `?25l` does not rely on DEC 2026 compositing, which some
    // terminals do not pass through.
    match cursor {
        Some((r, c)) => {
            s.insert_str(0, "\x1b[?25l");
            s.push_str(&format!("\x1b[{};{}H\x1b[?25h", r + 1, c + 1));
        }
        None => s.push_str("\x1b[?25l"),
    }
    s
}

/// A terminal that writes where it is told, for the tests that are about the encoder
/// rather than about the pty: `enter` needs a real terminal, the diff and the failure
/// policy do not.
#[cfg(test)]
impl Terminal {
    fn headless() -> Terminal {
        Terminal {
            original: unsafe { std::mem::zeroed() },
            fd: -1,
            entered: false,
            title: std::cell::RefCell::new(String::new()),
            shown: std::cell::RefCell::new(Vec::new()),
            next: std::cell::RefCell::new(Vec::new()),
            cursor: std::cell::Cell::new(None),
            frames: std::cell::Cell::new(0),
            silent: std::cell::Cell::new(0),
            stats: std::cell::Cell::new(WriteStats::default()),
            prev_payload: std::cell::RefCell::new(String::new()),
            stats_to: None,
            pending: std::cell::RefCell::new(Vec::new()),
            last_raw: std::cell::RefCell::new(Vec::new()),
            last_size: std::cell::Cell::new((80, 24)),
            full: std::cell::Cell::new(true),
            features: Features::default(),
            options: Options::default(),
            progress: std::cell::Cell::new(Progress::Idle),
        }
    }
}

/// A title's text with every control character removed and its length capped — what
/// [`Terminal::set_title`] writes. Separate so it can be tested without a terminal.
pub fn window_title_text(title: &str) -> String {
    title
        .chars()
        .filter(|c| !c.is_control())
        .take(120)
        .collect::<String>()
        .trim()
        .to_string()
}

fn restore(fd: i32, original: &libc::termios, features: Features, options: Options) {
    unsafe { libc::tcsetattr(fd, libc::TCSANOW, original) };
    let mut out = std::io::stdout();
    // The extras first, each only where `enter` turned it on: the kitty keyboard popped,
    // focus reports off, the progress bar removed.
    if features.keys {
        let _ = out.write_all(b"\x1b[<u");
    }
    if features.notify {
        let _ = out.write_all(b"\x1b[?1004l");
    }
    if features.progress {
        let _ = out.write_all(Progress::Idle.osc());
    }
    // Every mode `enter` turned on, off again in reverse: end any open synchronised update,
    // mouse tracking off, bracketed paste off, the cursor shape back, then show the cursor,
    // leave the alternate screen and pop the title `enter` saved (`CSI 23;0 t`).
    let _ = out.write_all(b"\x1b[?2026l");
    match options.mouse {
        Mouse::Off => {}
        Mouse::Buttons => {
            let _ = out.write_all(b"\x1b[?1006l\x1b[?1002l");
        }
        Mouse::Any => {
            let _ = out.write_all(b"\x1b[?1006l\x1b[?1003l");
        }
    }
    let _ = out.write_all(b"\x1b[?2004l");
    if options.block_cursor {
        let _ = out.write_all(b"\x1b[0 q");
    }
    let _ = out.write_all(b"\x1b[?25h\x1b[?1049l\x1b[23;0t");
    let _ = out.flush();
}

/// Standard base64, padded, for OSC 52. A copy rather than a dependency: rano depends on no
/// letibot crate, and the alphabet is RFC 4648's, so the two cannot mean different things.
pub fn base64(bytes: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(A[(n >> 18) as usize & 63] as char);
        out.push(A[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            A[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            A[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unchanged_frame_writes_nothing_at_all() {
        // The flicker, as an assertion: an idle loop wakes ten times a second with nothing
        // new, and repainting the screen for that is what flicker is.
        let frame: Vec<String> = ["one", "two", "three"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let mut shown = Vec::new();
        let cur = Some((2, 4));
        let (first, next) = paint(&shown, &frame, cur, None);
        assert!(!first.is_empty(), "first draw");
        shown = next;
        for _ in 0..100 {
            let (bytes, next) = paint(&shown, &frame, cur, cur);
            assert_eq!(bytes, "");
            // And the memory is still adopted: a frame needing no bytes is one whose memory
            // is already true, and a `paint` that returned the old memory would never learn.
            shown = next;
        }
        assert_eq!(shown, frame);
    }

    /// [`WriteStats::rows`] counts row rewrites by counting `ESC[K`, which holds only while
    /// [`paint_full`] emits exactly one per repainted row — a fact that can rot, so it is
    /// asserted: two rows changed of five is two.
    #[test]
    fn the_write_counter_counts_rows_and_not_frames() {
        let a: Vec<String> = ["one", "two", "three", "four", "five"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let mut shown = Vec::new();
        let first;
        let next;
        {
            let mut scratch = Vec::new();
            first = paint_full(&shown, &a, &mut scratch, None, None, true);
            next = scratch;
        }
        assert_eq!(first.matches("\x1b[K").count(), 5, "every row, once");
        shown = next;

        let mut b = a.clone();
        b[1] = "TWO".into();
        b[3] = "FOUR".into();
        let mut scratch = Vec::new();
        let second = paint_full(&shown, &b, &mut scratch, None, None, false);
        assert_eq!(
            second.matches("\x1b[K").count(),
            2,
            "only the rows that changed: {second:?}"
        );
        assert_eq!(second.matches("\x1b[2J").count(), 0, "and no erase");
    }

    /// A frame that parks the cursor hides it before writing the rows.
    ///
    /// Asserted as an order — hide, rows, park, show — because a `?25l` in the wrong place
    /// is a cursor hidden after the flash.
    #[test]
    fn a_frame_that_parks_the_cursor_hides_it_first() {
        let a: Vec<String> = ["one", "two"].iter().map(|s| s.to_string()).collect();
        let shown: Vec<String> = Vec::new();
        let mut next = Vec::new();
        let bytes = paint_full(&shown, &a, &mut next, Some((1, 3)), None, true);
        assert!(
            bytes.starts_with("\x1b[?25l"),
            "the cursor is hidden before anything is written, or it is visible while the \
             rows arrive: {bytes:?}"
        );
        let hide = bytes.find("\x1b[?25l").expect("hidden");
        let row = bytes.find("two").expect("a row");
        let park = bytes.find("\x1b[2;4H").expect("the destination");
        let show = bytes.find("\x1b[?25h").expect("shown");
        assert!(
            hide < row && row < park && park < show,
            "hide, then the rows, then park and show: {bytes:?}"
        );
    }

    #[test]
    fn a_frame_that_changed_height_does_not_erase_the_screen() {
        // The composer grows a row every time a prompt wraps; erasing for that is a flash
        // per wrap.
        let a: Vec<String> = ["one", "two", "three"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let mut shown = Vec::new();
        let mut next = Vec::new();
        paint_full(&shown, &a, &mut next, None, None, true);
        shown = next;
        let mut b = a.clone();
        b.push("four".into());
        let mut next = Vec::new();
        let bytes = paint_full(&shown, &b, &mut next, None, None, false);
        shown = next;
        assert!(!bytes.contains("\x1b[2J"), "{bytes:?}");
        assert!(bytes.contains("four"), "{bytes:?}");
        assert!(!bytes.contains("one"), "unchanged rows stay put: {bytes:?}");
        // And shrinking erases exactly the row that went, not the screen.
        let mut next = Vec::new();
        let bytes = paint_full(&shown, &a, &mut next, None, None, false);
        shown = next;
        assert!(!bytes.contains("\x1b[2J"), "{bytes:?}");
        assert!(bytes.contains("\x1b[4;1H"), "row four is erased: {bytes:?}");
        // …and the glass is still an honest model of itself.
        let mut scratch = Vec::new();
        assert_eq!(paint_full(&shown, &a, &mut scratch, None, None, false), "");
    }

    /// A paint that dies part way writes no record of the frame it did not finish: a
    /// `write_all` that fails may have written any prefix, so the head cannot know which rows
    /// landed, and a wrong memory would make the missing row permanent.
    ///
    /// The writer fails on its second call, so the first write really does reach the glass
    /// and "commit nothing" and "commit everything" are both wrong.
    #[test]
    fn a_paint_that_dies_part_way_leaves_no_record() {
        /// Writes `left` times, then fails for ever.
        struct Dies {
            left: usize,
            wrote: Vec<u8>,
        }
        impl std::io::Write for Dies {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                if self.left == 0 {
                    return Err(std::io::Error::from(std::io::ErrorKind::BrokenPipe));
                }
                self.left -= 1;
                self.wrote.extend_from_slice(buf);
                Ok(buf.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        let t = Terminal::headless();
        let frame: Vec<String> = ["one", "two", "three"]
            .iter()
            .map(|s| s.to_string())
            .collect();

        // The frame draws onto the glass, and the glass is remembered.
        let mut ok = Dies {
            left: usize::MAX,
            wrote: Vec::new(),
        };
        assert!(
            !t.paint_to(&mut ok, &frame, None),
            "a good write invalidates nothing"
        );
        assert_eq!(*t.shown.borrow(), frame, "the frame is on the glass");
        assert!(!ok.wrote.is_empty(), "the premise: bytes really went out");

        // A frame whose write dies after one call: `paint_to` writes the synchronised-output
        // opener first, so the glass got that and nothing of the row itself.
        let mut dead = Dies {
            left: 1,
            wrote: Vec::new(),
        };
        let mut changed = frame.clone();
        changed[1] = "TWO".into();
        assert!(
            t.paint_to(&mut dead, &changed, None),
            "a failed write must report that the glass is unknown"
        );
        // Nothing is claimed: not the old frame and not the new one, because the head cannot
        // know which rows landed.
        assert!(
            t.shown.borrow().is_empty(),
            "a frame that died part-way was recorded: {:?}",
            t.shown.borrow()
        );
        // And the next frame is a full repaint, so the hole cannot outlive the failure.
        let mut ok = Dies {
            left: usize::MAX,
            wrote: Vec::new(),
        };
        t.paint_to(&mut ok, &changed, None);
        let sent = String::from_utf8_lossy(&ok.wrote).to_string();
        assert!(
            sent.contains("\x1b[2J"),
            "the recovery must be a full repaint, not a diff against a guess: {sent:?}"
        );
        for row in ["one", "TWO", "three"] {
            assert!(
                sent.contains(row),
                "the repaint must carry every row: {sent:?}"
            );
        }

        // The counters do not claim bytes that never reached the glass, but the attempt is
        // still a frame.
        let t = Terminal::headless();
        let mut dead = Dies {
            left: 1,
            wrote: Vec::new(),
        };
        t.paint_to(&mut dead, &frame, None);
        assert_eq!(
            t.write_stats().bytes,
            0,
            "a failed frame counted its bytes as written"
        );
        assert_eq!(t.write_stats().frames, 1, "but the frame was attempted");
    }

    #[test]
    fn only_the_rows_that_changed_are_written() {
        let a: Vec<String> = ["one", "two", "three"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let mut b = a.clone();
        b[1] = "TWO".into();
        let mut shown = Vec::new();
        let (_, next) = paint(&shown, &a, None, None);
        shown = next;
        let (bytes, _) = paint(&shown, &b, None, None);
        assert!(bytes.contains("TWO"));
        assert!(
            !bytes.contains("one") && !bytes.contains("three"),
            "{bytes:?}"
        );
        // Addressed absolutely: row 2, column 1.
        assert!(bytes.contains("\x1b[2;1H"), "{bytes:?}");
    }

    /// The render core's rows go through the painter like any rows: emitted twice unchanged
    /// writes nothing the second time, and one changed cell rewrites only its own row.
    #[test]
    fn a_buffer_frame_rewrites_only_the_row_a_cell_changed_in() {
        use crate::render::{Buffer, Line, Palette, Rect, Role};
        let mut buf = Buffer::empty(Rect::new(0, 0, 20, 4));
        for (i, s) in ["alpha", "beta", "gamma", "delta"].iter().enumerate() {
            buf.set_line(0, i as u16, &Line::styled(*s, Role::Keyword), 20);
        }
        let t = Terminal::headless();
        let mut sink = Vec::new();
        t.paint_to(&mut sink, &buf.emit(Palette::Colour), None);
        let before = t.write_stats();
        assert_eq!(before.rows, 4, "the first frame paints every row");

        let mut sink = Vec::new();
        t.paint_to(&mut sink, &buf.emit(Palette::Colour), None);
        assert!(sink.is_empty(), "an unchanged buffer writes zero bytes");
        assert_eq!(t.write_stats().silent, before.silent + 1);

        buf.set_str(2, 2, "M", &crate::render::Style::of(Role::Failure), 1);
        let mut sink = Vec::new();
        t.paint_to(&mut sink, &buf.emit(Palette::Colour), None);
        let sent = String::from_utf8(sink).unwrap();
        assert_eq!(t.write_stats().rows, before.rows + 1, "{sent:?}");
        assert!(
            sent.contains("\x1b[3;1H"),
            "row three, addressed absolutely: {sent:?}"
        );
        assert!(sent.starts_with("\x1b[?2026h") && sent.ends_with("\x1b[?2026l"));
    }

    #[test]
    fn a_window_title_carries_no_control_character() {
        assert_eq!(window_title_text("a\x1b]0;pwned\x07b"), "a]0;pwnedb");
        assert_eq!(window_title_text("  x  "), "x");
        assert_eq!(window_title_text(&"y".repeat(300)).len(), 120);
    }

    #[test]
    fn base64_is_the_standard_padded_alphabet() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64("ключ".as_bytes()), "0LrQu9GO0Yc=");
    }
}
