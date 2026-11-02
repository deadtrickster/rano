//! The editor's side of async loading: adopt rows into the buffer as they arrive, and say so
//! on the status line. The job feeds the buffer itself, unlike `lsp_ctrl`'s pattern.

use crate::buffer::Buffer;
use crate::editor::Editor;
use crate::linecount::{Counted, LineCountJob};
use crate::loader::{
    ADOPT_BUDGET as budget, Adopted, LoadJob, TAIL_ROWS, TAIL_WINDOW, Tail, tail_offset,
};
use std::path::Path;

impl Editor {
    /// Start reading `path` on a worker: the buffer becomes an empty document with that name,
    /// so the title bar is right from the first frame.
    pub fn start_load(&mut self, path: &Path) -> std::io::Result<()> {
        // A plugin with `on_open` reads a small file whole, synchronously: a transform needs
        // the whole text.
        if self.plugin.as_ref().is_some_and(|p| p.has("on_open")) {
            match std::fs::metadata(path) {
                Ok(m) if m.len() as usize <= crate::plugin::MAX_HOOK_BYTES => {
                    let buf = self.read_file(path)?;
                    self.adopt_read(path, buf);
                    let n = self.bs().buf.row_count();
                    self.flash_with_note(&format!("Read {n} lines"));
                    return Ok(());
                }
                Ok(m) => {
                    self.flash(&format!(
                        "plugin: not run on {} ({} MiB, above the {} MiB cap)",
                        path.display(),
                        m.len() >> 20,
                        crate::plugin::MAX_HOOK_BYTES >> 20
                    ));
                }
                Err(_) => {}
            }
        }
        let job = LoadJob::spawn(path.to_path_buf())?;
        self.begin_load(path, job);
        Ok(())
    }

    /// Make `buf` the current buffer, whole: the synchronous twin of [`Self::begin_load`].
    fn adopt_read(&mut self, path: &Path, mut buf: Buffer) {
        buf.name = Some(path.to_path_buf());
        buf.disk = crate::buffer::DiskStamp::of(path);
        let bs = self.bs_mut();
        bs.buf = buf;
        bs.cursor = crate::buffer::Pos { row: 0, col: 0 };
        bs.scroll = 0;
        bs.load = None;
        bs.tail = None;
        bs.read_only = false;
        bs.lines_before = Some(0);
        bs.line_count = None;
        bs.edit_gen = bs.edit_gen.wrapping_add(1);
        bs.wrap_extend_from = None;
        self.highlight_dirty = true;
        self.diag_dirty = true;
        self.lsp_sync();
    }

    /// [`Self::start_load`], from byte `from` to the end — the tail path.
    pub fn start_load_from(&mut self, path: &Path, from: u64) -> std::io::Result<()> {
        let job = LoadJob::spawn_from(path.to_path_buf(), from)?;
        self.begin_load(path, job);
        Ok(())
    }

    /// [`Self::start_load_from`], and keep reading as the file grows (`-f`/`--follow`).
    pub fn start_follow_from(&mut self, path: &Path, from: u64) -> std::io::Result<()> {
        let job = LoadJob::spawn_follow_from(path.to_path_buf(), from)?;
        self.begin_load(path, job);
        Ok(())
    }

    /// The buffer's half of opening a file: name it, hand it the job, invalidate what a new
    /// file invalidates.
    fn begin_load(&mut self, path: &Path, job: LoadJob) {
        {
            let bs = self.bs_mut();
            bs.buf = Buffer::new();
            bs.buf.name = Some(path.to_path_buf());
            bs.buf.disk = crate::buffer::DiskStamp::of(path);
            // One empty row is the invariant `Buffer` needs before any arrive.
            bs.cursor = crate::buffer::Pos { row: 0, col: 0 };
            bs.scroll = 0;
            bs.load = Some(job);
            // A new document: the previous view's flags are cleared.
            bs.tail = None;
            bs.read_only = false;
            bs.lines_before = Some(0);
            bs.line_count = None;
        }
        // A new file is a new language, a new LSP session and a new highlight.
        self.lsp_sync();
        self.highlight_dirty = true;
        self.ensure_wrap_prefix();
        // A picture opens as the picture; the rows under it are a lossy decode.
        self.preview_if_picture();
    }

    /// Whether `path` opens in log mode: the name ends in `.log` — a name rule rather than a
    /// content sniff, because a rule per directory its author saw is a bug per directory they
    /// did not.
    pub fn log_mode_for(&self, path: &Path) -> bool {
        if self.edit_override {
            return false;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            return false;
        };
        name.len() > 4 && name.as_bytes()[name.len() - 4..].eq_ignore_ascii_case(b".log")
    }

    /// Open `path` the way its name says — one decision, called from every door.
    pub fn start_named(&mut self, path: &Path) -> std::io::Result<()> {
        if self.log_mode_for(path) {
            self.start_tail(path)
        } else {
            self.start_load(path)
        }
    }

    /// Open `path` as a tail: the last few screens, read-only, followed as it grows.
    pub fn start_tail(&mut self, path: &Path) -> std::io::Result<()> {
        let mut file = std::fs::File::open(path)?;
        let size = file.metadata()?.len();
        let from = tail_offset(&mut file, size, TAIL_ROWS, TAIL_WINDOW)?;
        self.start_follow_from(path, from)?;
        {
            let bs = self.bs_mut();
            bs.tail = Some(Tail {
                size_at_open: size,
                // Unknown until the initial read finishes; see `Tail`.
                rows_at_open: None,
            });
            bs.read_only = true;
            // The tail's row 1 is not the file's row 1; unknown until the scan answers.
            bs.lines_before = None;
        }
        self.start_line_count(path, from);
        Ok(())
    }

    /// Start the background count of the lines before `upto`.
    fn start_line_count(&mut self, path: &Path, upto: u64) {
        let job = LineCountJob::spawn(path.to_path_buf(), upto).ok();
        self.bs_mut().line_count = job;
    }

    /// Adopt the line count if it has answered; never blocks.
    pub(crate) fn line_count_poll(&mut self) -> bool {
        let Some(job) = self.bs_mut().line_count.as_mut() else {
            return false;
        };
        match job.poll() {
            Counted::Nothing => false,
            Counted::Progress(_) => true,
            Counted::Done(n) => {
                let bs = self.bs_mut();
                bs.line_count = None;
                bs.lines_before = Some(n);
                true
            }
            // Cancelled: no number and no guess; the gutter stays blank.
            Counted::Stopped => {
                self.bs_mut().line_count = None;
                false
            }
            Counted::Failed(e) => {
                self.bs_mut().line_count = None;
                self.flash(&format!("Cannot count the file's lines: {e}"));
                true
            }
        }
    }

    /// A line count is in flight.
    pub(crate) fn counting(&self) -> bool {
        self.bs().line_count.is_some()
    }

    /// Adopt whatever the loader has ready, bounded by `loader::ADOPT_BUDGET`, returning
    /// whether to redraw. Never waits; a saturated job says so.
    pub(crate) fn load_poll(&mut self) -> bool {
        let Some(job) = self.bs_mut().load.as_mut() else {
            return false;
        };
        let mut rows = Vec::new();
        let outcome = job.poll(&mut rows);
        self.load_saturated = matches!(outcome, Adopted::Rows(_)) && rows.len() >= budget;
        let mut dirty = !rows.is_empty();
        if !rows.is_empty() {
            let text_h = self.text_h;
            let bs = self.bs_mut();
            // An append, not an edit: the wrap table extends instead of re-measuring.
            let was = bs.buf.row_count();
            // The view holds the end if it already had it: a tail that yanked the scroll back
            // would be unreadable.
            let at_end = was <= text_h || bs.scroll + text_h >= was;
            // The first batch replaces the seed row, so a file does not begin with a blank line.
            if bs.buf.row_count() == 1 && bs.buf.row(0).is_empty() && !bs.buf.modified {
                bs.buf.clear_rows();
            }
            bs.buf.append_rows(&mut rows);
            bs.wrap_extend_from = Some(
                if was == 1 && bs.buf.row_count() > 1 && bs.wrap_rows.len() <= 1 {
                    0
                } else {
                    was.min(bs.wrap_rows.len())
                },
            );
            if bs.tail.is_some() && at_end {
                bs.cursor = crate::buffer::Pos {
                    row: bs.buf.row_count().saturating_sub(1),
                    col: 0,
                };
                // `--line` and a tail ask for opposite ends of the file.
                bs.goto = None;
            }
            // A settled tail does not re-highlight on growth.
            let settled_tail = bs.tail_settled();
            bs.edit_gen = bs.edit_gen.wrapping_add(1);
            if !settled_tail {
                self.highlight_dirty = true;
            }
        }
        match outcome {
            Adopted::Nothing | Adopted::Rows(_) => {}
            Adopted::Binary => {
                self.load_saturated = false;
                {
                    let bs = self.bs_mut();
                    bs.load = None;
                    bs.buf.binary = true;
                    // A view, refused at the keystroke rather than at a save.
                    bs.read_only = true;
                }
                // A PDF, a `.pyc`: binary, and things a provider could open.
                let configured = self.config.plugin.is_some()
                    || std::env::var_os("RANO_PLUGIN").is_some()
                    || self.plugin.is_some()
                    || !self.builtins.is_empty();
                if configured {
                    self.flash(
                        "Binary: no provider claimed it — a plugin's capabilities or its load \
                         may be what is missing",
                    );
                } else {
                    self.flash("Binary file — not shown (read only)");
                }
                dirty = true;
            }
            Adopted::Encoding(enc) => {
                // Recorded so a save writes the file back as found.
                self.bs_mut().buf.encoding = enc;
            }
            Adopted::NeedsEager(enc) => {
                // UTF-16 cannot be split on byte newlines: read whole rather than staying stuck.
                self.bs_mut().load = None;
                let path = self.bs().buf.name.clone();
                if let Some(path) = path {
                    match Buffer::from_file(&path) {
                        Ok(mut read) => {
                            let bs = self.bs_mut();
                            bs.buf.set_rows(read.take_rows());
                            bs.buf.crlf = read.crlf;
                            bs.buf.encoding = read.encoding;
                            bs.cursor = crate::buffer::Pos { row: 0, col: 0 };
                            bs.scroll = 0;
                            bs.edit_gen = bs.edit_gen.wrapping_add(1);
                            bs.tail = None;
                            bs.lines_before = Some(0);
                            bs.line_count = None;
                        }
                        Err(e) => self.flash(&format!("Read failed: {e}")),
                    }
                }
                let _ = enc;
                self.highlight_dirty = true;
                dirty = true;
            }
            Adopted::Finished { crlf, .. } => {
                self.load_saturated = false;
                // Once whole, re-run the diagnostics — once, not per batch.
                self.diag_dirty = true;
                let rows = {
                    let bs = self.bs_mut();
                    bs.load = None;
                    bs.buf.crlf = crlf;
                    if bs.buf.rows_is_empty() {
                        bs.buf.push_row(Vec::new());
                    }
                    let rows = bs.buf.row_count();
                    // A tail read without following settles here, not at `CaughtUp`.
                    if let Some(t) = bs.tail.as_mut() {
                        t.rows_at_open = Some(rows);
                    }
                    rows
                };
                if self.bs().tail.is_some() {
                    self.flash(&format!(
                        "Tailing — showing the last {} line{}",
                        rows,
                        crate::editor::plural(rows)
                    ));
                } else {
                    self.flash(&format!(
                        "Read {} line{}",
                        rows,
                        crate::editor::plural(rows)
                    ));
                }
                dirty = true;
            }
            Adopted::CaughtUp { crlf, .. } => {
                self.load_saturated = false;
                self.diag_dirty = true;
                let rows = {
                    let bs = self.bs_mut();
                    bs.buf.crlf = crlf;
                    if bs.buf.rows_is_empty() {
                        bs.buf.push_row(Vec::new());
                    }
                    let rows = bs.buf.row_count();
                    // The styling boundary: everything from here down arrived after the tail.
                    if let Some(t) = bs.tail.as_mut() {
                        t.rows_at_open = Some(rows);
                    }
                    rows
                };
                self.flash(&format!(
                    "Tailing — showing the last {} line{}",
                    rows,
                    crate::editor::plural(rows)
                ));
                dirty = true;
            }
            Adopted::Failed(e) => {
                self.load_saturated = false;
                self.bs_mut().load = None;
                self.flash(&format!("Read failed: {e}"));
                dirty = true;
            }
        }
        dirty
    }

    /// Whether the buffer's text is still arriving. A follower that has read everything answers
    /// false.
    pub fn loading(&self) -> bool {
        self.bs().load.as_ref().is_some_and(|j| !j.caught_up())
    }

    /// The status-line text for a load in flight, or `None`.
    pub(crate) fn loading_text(&self) -> Option<String> {
        let job = self.bs().load.as_ref()?;
        // A follower with nothing left to read does not get to say "Reading…".
        if job.caught_up() {
            return None;
        }
        Some(format!(
            "Reading… {} line{} so far",
            job.rows_read,
            crate::editor::plural(job.rows_read)
        ))
    }

    /// The status-line text for a tail's line count, or `None`; weaker than `loading_text`,
    /// which a flash displaces.
    pub(crate) fn counting_text(&self) -> Option<String> {
        let job = self.bs().line_count.as_ref()?;
        Some(format!("Counting lines… {} so far", job.counted))
    }

    /// Abandon an in-flight read — rows or line count.
    pub(crate) fn cancel_load(&mut self) {
        if let Some(job) = self.bs().load.as_ref() {
            job.cancel();
        }
        if let Some(job) = self.bs().line_count.as_ref() {
            job.cancel();
        }
    }

    /// Start the deferred read of a command-line file, now that its buffer is current.
    pub(crate) fn start_pending_load(&mut self) -> bool {
        let Some(path) = self.bs_mut().pending_load.take() else {
            return false;
        };
        if let Err(e) = self.start_named(&path) {
            self.flash(&format!("Cannot read {}: {e}", path.display()));
        }
        true
    }

    /// Make the current buffer whole, synchronously: a jump into a half-arrived buffer needs
    /// the target row to exist first.
    pub(crate) fn load_now(&mut self) {
        let pending = self.bs_mut().pending_load.take();
        let in_flight = self.bs().load.is_some();
        let path = match pending {
            Some(p) => p,
            None if in_flight && !self.bs().buf.modified => match self.bs().buf.name.clone() {
                Some(p) => p,
                None => return,
            },
            None => return,
        };
        self.cancel_load();
        match self.read_file(&path) {
            Ok(buf) => {
                let bs = self.bs_mut();
                bs.load = None;
                bs.buf = buf;
                bs.edit_gen = bs.edit_gen.wrapping_add(1);
                bs.wrap_extend_from = None;
                // No longer a tail: `size_at_open` would describe a view this read replaced.
                bs.tail = None;
                bs.lines_before = Some(0);
                bs.line_count = None;
                self.highlight_dirty = true;
                self.diag_dirty = true;
                self.lsp_sync();
                self.preview_if_picture();
            }
            Err(e) => self.flash(&format!("Cannot read {}: {e}", path.display())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::buffer::Pos;
    use crate::config;
    use crate::editor::Editor;
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    /// A scratch file that removes itself.
    struct Temp(PathBuf);

    impl Temp {
        fn new(name: &str, contents: &[u8]) -> Self {
            let p = std::env::temp_dir().join(format!("rano_loadctrl_{name}"));
            std::fs::write(&p, contents).expect("write fixture");
            Self(p)
        }
    }

    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    fn ed() -> Editor {
        let mut ed = Editor::new(Buffer::new(), config::Config::default());
        ed.text_w = 80;
        ed.text_h = 24;
        ed
    }

    /// Drive the loop's poll until the load finishes, with a deadline.
    fn finish(ed: &mut Editor) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            assert!(Instant::now() < deadline, "load did not finish");
            let was = ed.loading();
            ed.load_poll();
            if was && !ed.loading() {
                return;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    fn rows(ed: &Editor) -> Vec<String> {
        ed.bs().buf.rows().map(|l| l.iter().collect()).collect()
    }

    #[test]
    fn a_load_fills_the_buffer_and_replaces_the_seed_row() {
        let t = Temp::new("basic.txt", b"one\ntwo\nthree\n");
        let mut ed = ed();
        ed.start_load(&t.0).expect("start");
        assert!(ed.bs().buf.name.is_some());
        assert!(ed.loading());
        finish(&mut ed);
        assert!(!ed.loading());
        assert_eq!(rows(&ed), ["one", "two", "three"], "no leading blank row");
    }

    #[test]
    fn a_loading_editor_draws_a_frame_and_says_so() {
        // The screen is never a dead blank: a frame from the first iteration, and a status line.
        let body: String = (0..300_000).map(|i| format!("row {i}\n")).collect();
        let t = Temp::new("slow.txt", body.as_bytes());
        let mut ed = ed();
        ed.start_load(&t.0).expect("start");
        let status = crate::ui::Screen::of(&ed, 80, 24).row(21);
        assert!(
            status.contains("Reading"),
            "the status line should say so, got {status:?}"
        );
        ed.cancel_load();
    }

    #[test]
    fn the_load_reports_progress() {
        let body: String = (0..300_000).map(|i| format!("row {i}\n")).collect();
        let t = Temp::new("progress.txt", body.as_bytes());
        let mut ed = ed();
        ed.start_load(&t.0).expect("start");
        let first = ed.loading_text().expect("loading text");
        assert!(first.starts_with("Reading"), "{first}");
        let deadline = Instant::now() + Duration::from_secs(5);
        while ed.loading() && ed.loading_text().as_deref() == Some(first.as_str()) {
            assert!(Instant::now() < deadline, "the count never advanced");
            ed.load_poll();
            std::thread::sleep(Duration::from_millis(1));
        }
        if ed.loading() {
            let later = ed.loading_text().expect("still loading");
            assert_ne!(later, first, "the count should advance");
            assert!(later.contains("line"), "{later}");
        }
        ed.cancel_load();
    }

    #[test]
    fn an_edit_during_a_load_stops_the_rest_and_keeps_what_arrived() {
        let body: String = (0..300_000).map(|i| format!("row {i}\n")).collect();
        let t = Temp::new("edited.txt", body.as_bytes());
        let mut ed = ed();
        ed.start_load(&t.0).expect("start");
        let deadline = Instant::now() + Duration::from_secs(5);
        while ed.bs().buf.row_count() < 10 {
            assert!(Instant::now() < deadline, "no rows adopted");
            ed.load_poll();
            std::thread::sleep(Duration::from_millis(1));
        }
        let adopted = ed.bs().buf.row_count();
        assert!(ed.loading(), "still arriving");
        // Type a character: the load must stop, and what arrived must stay.
        ed.bs_mut().cursor = Pos { row: 0, col: 0 };
        ed.insert_char('X');
        assert!(!ed.loading(), "an edit cancels the load");
        let after = rows(&ed);
        assert_eq!(after.len(), adopted, "the adopted rows are kept");
        assert_eq!(after[0], "Xrow 0", "the edit landed where asked");
        assert_ne!(
            ed.bs().hl.styled_window(),
            Some(crate::syntax::Window::rows(0, 0))
        );
    }

    #[test]
    fn a_latin1_file_opens_with_its_letters_intact() {
        let t = Temp::new("latin1b.bin", b"caf\xe9 latin-1\n");
        let mut ed = ed();
        ed.start_load(&t.0).expect("start");
        finish(&mut ed);
        assert_eq!(rows(&ed), ["caf\u{e9} latin-1"]);
        assert_eq!(ed.bs().buf.encoding, crate::encoding::Encoding::Cp1252);
        assert_eq!(ed.bs().buf.file_bytes().unwrap(), b"caf\xe9 latin-1\n");
    }

    #[test]
    fn a_loading_utf16_file_falls_back_to_the_eager_path() {
        // UTF-16 cannot be streamed: read whole, and do not leave the editor stuck loading.
        let text = "one\ntwo\nthree\n";
        let bytes = crate::encoding::encode(text, crate::encoding::Encoding::Utf16Le).unwrap();
        let t = Temp::new("u16.txt", &bytes);
        let mut ed = ed();
        ed.start_load(&t.0).expect("start");
        finish(&mut ed);
        assert!(!ed.loading(), "not left loading");
        assert_eq!(rows(&ed), ["one", "two", "three"]);
        assert_eq!(ed.bs().buf.encoding, crate::encoding::Encoding::Utf16Le);
    }

    #[test]
    fn a_utf8_bom_is_not_the_first_column() {
        let bytes = [&[0xEF, 0xBB, 0xBF][..], b"hello\n"].concat();
        let t = Temp::new("bom.txt", &bytes);
        let mut ed = ed();
        ed.start_load(&t.0).expect("start");
        finish(&mut ed);
        assert_eq!(rows(&ed), ["hello"], "no U+FEFF in the first row");
        assert!(!ed.bs().buf.row(0).contains(&'\u{FEFF}'));
        assert_eq!(ed.bs().buf.encoding, crate::encoding::Encoding::Utf8Bom);
        assert!(
            ed.bs()
                .buf
                .file_bytes()
                .unwrap()
                .starts_with(&[0xEF, 0xBB, 0xBF])
        );
    }

    #[test]
    fn a_failed_fallback_becomes_a_status_line() {
        let t = Temp::new("u16bad.bin", &[0xFF, 0xFE, 0x41]);
        let mut ed = ed();
        ed.start_load(&t.0).expect("start");
        finish(&mut ed);
        assert!(!ed.loading());
        let status = ed.status_text().unwrap_or_default();
        assert!(
            status.contains("failed") || status.contains("UTF-16"),
            "the failure is reported, got {status:?}"
        );
    }

    #[test]
    fn a_missing_file_is_reported_by_start_load() {
        let mut ed = ed();
        let missing = std::env::temp_dir().join("rano_loadctrl_no_such_file");
        let _ = std::fs::remove_file(&missing);
        assert!(ed.start_load(&missing).is_err());
    }

    #[test]
    fn a_buffer_with_no_load_is_untouched_by_polling() {
        let mut ed = ed();
        assert!(!ed.loading());
        assert!(!ed.load_poll(), "no job, nothing dirtied");
        assert!(ed.loading_text().is_none());
        assert_eq!(rows(&ed), [""], "the seed row is still there");
    }

    #[test]
    fn a_small_file_loads_between_two_frames() {
        let t = Temp::new("small.txt", b"let x = 1;\n");
        let mut ed = ed();
        ed.start_load(&t.0).expect("start");
        let deadline = Instant::now() + Duration::from_secs(5);
        while ed.loading() {
            assert!(Instant::now() < deadline);
            ed.load_poll();
        }
        assert_eq!(rows(&ed), ["let x = 1;"]);
        let status = ed.status_text().unwrap_or_default();
        assert!(status.contains('1'), "reports the line count: {status:?}");
    }
}
