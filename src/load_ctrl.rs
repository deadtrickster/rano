//! The editor's side of async loading: adopt rows into the buffer as they
//! arrive, and say so on the status line while it is happening.
//!
//! The pattern is `lsp_ctrl.rs`'s: a job held on the editor, polled once per
//! loop iteration, returning whether anything changed so the frame is redrawn.
//! What is different here is that the job feeds the *buffer* rather than a
//! side channel, so adoption is what makes the file appear.

use crate::buffer::Buffer;
use crate::editor::Editor;
use crate::linecount::{Counted, LineCountJob};
use crate::loader::{
    ADOPT_BUDGET as budget, Adopted, LoadJob, TAIL_ROWS, TAIL_WINDOW, Tail, tail_offset,
};
use std::path::Path;

impl Editor {
    /// Start reading `path` on a worker. The buffer becomes an empty document
    /// with that name — so language detection, the title bar and the gutter are
    /// right from the first frame — and rows are adopted as they arrive.
    ///
    /// Returns whether a job started; a file that cannot be opened is reported
    /// by the caller (it is the one failure that should not become a status
    /// line, because there is nothing on screen to attach it to).
    pub fn start_load(&mut self, path: &Path) -> std::io::Result<()> {
        // `spawn` rather than `spawn_from(path, 0)`: reading a whole file is
        // the case with a name of its own, and what every caller but the tail
        // wants.
        let job = LoadJob::spawn(path.to_path_buf())?;
        self.begin_load(path, job);
        Ok(())
    }

    /// [`Self::start_load`], from byte `from` to the end of the file — the tail
    /// path, whose offset comes from [`crate::loader::tail_offset`].
    ///
    /// Everything else is identical on purpose: a tail is a file read that
    /// begins later, so the reader, the batching, the adoption budget, the
    /// diagnostics refresh and the cursor's arrival are all the same machinery.
    /// What makes it a tail is the offset, and what remembers that it is one is
    /// [`crate::BufferState::tail`].
    pub fn start_load_from(&mut self, path: &Path, from: u64) -> std::io::Result<()> {
        let job = LoadJob::spawn_from(path.to_path_buf(), from)?;
        self.begin_load(path, job);
        Ok(())
    }

    /// [`Self::start_load_from`], **and keep reading the file as it grows** —
    /// `-f`/`--follow` (TODO.md §20.3 D). Same load, same buffer, same
    /// everything; the reader simply does not stop at the end.
    pub fn start_follow_from(&mut self, path: &Path, from: u64) -> std::io::Result<()> {
        let job = LoadJob::spawn_follow_from(path.to_path_buf(), from)?;
        self.begin_load(path, job);
        Ok(())
    }

    /// The buffer's half of opening a file: name it, give it the one row a
    /// `Buffer` always holds, hand it the job, and invalidate what a new file
    /// invalidates.
    ///
    /// Shared by the two openers above because a tail differs from a plain read
    /// in exactly one thing — the byte the reader starts at — and in nothing a
    /// buffer knows. Which is also why the view flags are cleared here and set
    /// by `start_tail` afterwards: reading a file is reading a file, and what
    /// view it is read INTO comes from the caller.
    fn begin_load(&mut self, path: &Path, job: LoadJob) {
        {
            let bs = self.bs_mut();
            bs.buf = Buffer::new();
            bs.buf.name = Some(path.to_path_buf());
            // Before the reader starts, as `Buffer::from_file` does.
            bs.buf.disk = crate::buffer::DiskStamp::of(path);
            // The rows arrive one batch at a time; one empty row is what a
            // buffer holds before any do, and the invariant `Buffer` needs.
            bs.cursor = crate::buffer::Pos { row: 0, col: 0 };
            bs.scroll = 0;
            bs.load = Some(job);
            // This BufferState holds a NEW document from disk, so the two flags
            // that describe the previous one's view are cleared. `tail` because
            // a load that is not a tail must not look like one — it would land at
            // the bottom and remember a size from another file. `read_only`
            // because that is what `open_file` does too: a freshly read document
            // is writable unless the caller says otherwise, and `start_tail` says
            // otherwise one line after this returns.
            bs.tail = None;
            bs.read_only = false;
            bs.lines_before = Some(0);
            bs.line_count = None;
        }
        // A new file is a new language, a new LSP session and a new highlight.
        self.lsp_sync();
        self.highlight_dirty = true;
        self.ensure_wrap_prefix();
        // **A picture opens as the picture** — the rows arriving underneath are a lossy
        // decode of bytes that are not text (see `Editor::preview_if_picture`). Done before
        // the loader's rows land, because the view reads the file, not the buffer.
        self.preview_if_picture();
    }

    /// Open `path` as a tail: the last few screens, read from the end, read-only,
    /// and followed as it grows — `-f`/`--follow`, and one day a `.log` name
    /// (TODO.md §20).
    ///
    /// Three things are decided here and nowhere else:
    ///
    /// - **Where to start reading.** [`tail_offset`] scans *backwards*, so a
    ///   2 GiB log opens showing its last screens for a read of a few hundred
    ///   KiB rather than the whole file.
    /// - **That it follows.** The reader does not stop when it reaches the end
    ///   (§20.3 D). A log is a file somebody is still writing; a view that froze
    ///   at the instant it was opened would be a snapshot nobody asked for.
    /// - **That it is a view.** The operator's rule, and the reason is at the
    ///   keystroke (§20.6): a tailed file is one something else is writing, so
    ///   an edit that could never be saved is work lost silently.
    ///
    /// `size_at_open` is recorded rather than used: §20.3 E is its reader, and
    /// it is the line between "this row was here before us and can be styled
    /// like a file's" and "this row is still arriving". Remembered now so that
    /// opening a tail and following one are not two notions of "the file".
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
            // The tail's row 1 is the file's row `lines_before + 1`, and nothing
            // knows `lines_before` yet — so the numbering is honestly unknown
            // until the scan below answers, and the gutter says so rather than
            // saying `1` (TODO.md §20.3 C).
            bs.lines_before = None;
        }
        self.start_line_count(path, from);
        Ok(())
    }

    /// Start the background count of the file's lines before byte `upto` — the
    /// tail's offset, so **only the bytes above the tail are scanned**: the
    /// rows below it are the ones already in the buffer.
    ///
    /// A failure here is not fatal and is not flashed: the only consequence is
    /// that the numbering stays unknown, which `BufferState::line_number`
    /// already expresses, and an error banner at open for a file that read
    /// perfectly would be a worse answer than a blank gutter.
    fn start_line_count(&mut self, path: &Path, upto: u64) {
        let job = LineCountJob::spawn(path.to_path_buf(), upto).ok();
        self.bs_mut().line_count = job;
    }

    /// Adopt the line count if it has answered. Never blocks.
    ///
    /// Returns whether the frame needs redrawing. A `Progress` one does: the
    /// status line counts up, which is how a long scan on a 2 GiB log is told
    /// apart from a hung one.
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
            // Cancelled, or its worker went away: no number, and no guess. The
            // gutter stays blank for the rest of the session, which is the
            // honest half of a cancelled scan.
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

    /// A line count is in flight. Used by the loop to keep the wait short, and
    /// by the status line to say why the numbers are missing.
    pub(crate) fn counting(&self) -> bool {
        self.bs().line_count.is_some()
    }

    /// Adopt whatever the loader has ready, bounded by
    /// [`loader::ADOPT_BUDGET`]. Returns whether the frame needs redrawing.
    ///
    /// Never waits: an idle job returns `false` immediately, which is what
    /// keeps the loop's iteration short and the keyboard live while a 184 MB
    /// file is still arriving. When the budget is exhausted it says so, so the
    /// loop can skip its idle wait and keep going — see `load_saturated`.
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
            // An append, not an edit: tell the wrap table it can extend from
            // where it already is rather than re-measuring the whole file.
            let was = bs.buf.row_count();
            // **The view holds the end if it already had it.** A tail that
            // yanked the scroll back on every arriving line would make reading
            // the last few hundred lines of a busy log impossible; one that
            // never moved would stop being a follow at all. So: stay at the
            // bottom if the bottom was on screen, and otherwise leave the view
            // exactly where the user put it.
            //
            // Compared in rows, and `scroll` counts VISUAL rows when wrap is
            // on — the same number for a log, whose rows wrap only if it has
            // very long ones, and being wrong costs one frame's scroll
            // position rather than a wrong row.
            let at_end = was <= text_h || bs.scroll + text_h >= was;
            // The buffer starts with one empty row. The first batch replaces
            // it rather than following it, so a file does not appear to begin
            // with a blank line.
            if bs.buf.row_count() == 1 && bs.buf.row(0).is_empty() && !bs.buf.modified {
                bs.buf.clear_rows();
            }
            bs.buf.append_rows(&mut rows);
            // The first batch replaces the seed row, so the table has to move
            // its start; after that every batch is a pure append.
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
            // **A settled tail does not re-highlight when the file grows.** Rows
            // past the boundary are never styled (§20.3 E) and the rows before
            // it have not changed, so a refresh has nothing to do — and a log
            // being written appends constantly, which is the per-keystroke cost
            // §16 exists to avoid, arriving in a new shape. An ordinary buffer,
            // and a tail whose boundary is not known yet, dirty as before.
            let settled_tail = bs.tail_settled();
            bs.edit_gen = bs.edit_gen.wrapping_add(1);
            if !settled_tail {
                self.highlight_dirty = true;
            }
        }
        match outcome {
            Adopted::Nothing | Adopted::Rows(_) => {}
            Adopted::Encoding(enc) => {
                // Recorded on the buffer so a save writes the file back the way
                // it was found; no rows yet, so nothing to redraw.
                self.bs_mut().buf.encoding = enc;
            }
            Adopted::NeedsEager(enc) => {
                // UTF-16: this reader cannot split its rows on byte newlines,
                // so the file is read whole. Correct, and the eager path is
                // what any size of such a file needs anyway — but it must not
                // leave the editor stuck in a loading state.
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
                            // The file is whole now, so it is not a tail and
                            // there is nothing left to count.
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
                // Once the file is whole, re-run the diagnostics so what is
                // reported is the document rather than whatever had arrived at
                // the 300 ms mark. Deliberately here and not per batch: a
                // whole-document parse every 300 ms through a large load is the
                // cost §16 exists to avoid, and a todo file is kilobytes.
                self.diag_dirty = true;
                let rows = {
                    let bs = self.bs_mut();
                    bs.load = None;
                    bs.buf.crlf = crlf;
                    if bs.buf.rows_is_empty() {
                        bs.buf.push_row(Vec::new());
                    }
                    let rows = bs.buf.row_count();
                    // A tail read without following (`start_load_from`) settles
                    // here instead of at `CaughtUp`.
                    if let Some(t) = bs.tail.as_mut() {
                        t.rows_at_open = Some(rows);
                    }
                    rows
                };
                if self.bs().tail.is_some() {
                    // **Not "Read N lines".** `rows` here is the tail we asked
                    // for — 200, or the whole file if it is shorter — and the
                    // file's own line count has not been measured. §20.1's idea
                    // 3 is the increment that measures it; this line is the one
                    // that would otherwise state it wrongly (TODO.md §20.3 C).
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
                    // **The styling boundary** (§20.3 E): this is the moment it
                    // becomes true that everything from here down arrived after
                    // the tail was read, so it is a row the file is still
                    // writing. Recorded on the way through rather than computed
                    // later, because later is after more rows have arrived.
                    if let Some(t) = bs.tail.as_mut() {
                        t.rows_at_open = Some(rows);
                    }
                    rows
                };
                // **Not "Read N lines".** `rows` here is the tail we asked for,
                // and the file's own line count is the background scan's
                // (`BufferState::line_number`), not this. Saying 200 for a
                // 400 000-line file is the lie §20.3 C exists to remove.
                //
                // It is also a statement about the MOMENT the file was read, so
                // a row arriving afterwards makes it stale — bounded by the
                // flash's own three seconds rather than tracked, and after that
                // the gutter's numbers (which do keep up) are the answer.
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

    /// Whether the buffer's text is still arriving — and a follower that has
    /// read everything the file had answers **false**, because there is nothing
    /// in flight: the view is complete and something else may or may not write
    /// more to it. A host that asked "has the text all arrived" wants that
    /// answer, and it is what keeps the loop's wait at the idle cadence rather
    /// than at the loading one (see `next_wakeup`).
    pub fn loading(&self) -> bool {
        self.bs().load.as_ref().is_some_and(|j| !j.caught_up())
    }

    /// The status-line text for a load in flight, or `None`.
    ///
    /// The operator's criterion from TODO.md §13.6 — "the minimum that makes
    /// the user happy and not confused" — is why this exists at all: a blank
    /// frame is confusing, "reading huge.log" is not.
    pub(crate) fn loading_text(&self) -> Option<String> {
        let job = self.bs().load.as_ref()?;
        // A follower with nothing left to read is not a load in flight, so it
        // does not get to say "Reading…" over every other status it outranks.
        if job.caught_up() {
            return None;
        }
        Some(format!(
            "Reading… {} line{} so far",
            job.rows_read,
            crate::editor::plural(job.rows_read)
        ))
    }

    /// The status-line text for a tail's line count in flight, or `None`.
    ///
    /// **Deliberately not `loading_text`'s**, because a load in flight and a
    /// count in flight are not equally important: a load is the difference
    /// between a frozen frame and a working one, while a count is a number
    /// arriving late. So this one loses to a flash — a refusal such as
    /// "Read-only buffer — typing refused" must never be displaced by it — and
    /// it is said only while the gutter has nothing to say (§20.3 C).
    pub(crate) fn counting_text(&self) -> Option<String> {
        let job = self.bs().line_count.as_ref()?;
        Some(format!("Counting lines… {} so far", job.counted))
    }

    /// Abandon an in-flight read — rows or line count: `^C`, an edit, or
    /// another file. Both, because both are work whose answer the caller has
    /// just stopped waiting for.
    pub(crate) fn cancel_load(&mut self) {
        if let Some(job) = self.bs().load.as_ref() {
            job.cancel();
        }
        if let Some(job) = self.bs().line_count.as_ref() {
            job.cancel();
        }
    }

    /// Start the deferred read of a file named on the command line, now that
    /// its buffer is current. Called once per loop iteration, so whatever
    /// made the buffer current — M->, the buffer list, a close, ^X's save
    /// prompt — the file starts arriving on the next frame. Returns whether
    /// anything changed.
    pub(crate) fn start_pending_load(&mut self) -> bool {
        let Some(path) = self.bs_mut().pending_load.take() else {
            return false;
        };
        if let Err(e) = self.start_load(&path) {
            self.flash(&format!("Cannot read {}: {e}", path.display()));
        }
        true
    }

    /// Make the current buffer whole, synchronously: a deferred file is read
    /// now and a load in flight is replaced by a full read. For jumps
    /// (definition, usages) into a buffer that has not finished arriving —
    /// they need the target row to exist before the cursor is put on it.
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
        match Buffer::from_file(&path) {
            Ok(buf) => {
                let bs = self.bs_mut();
                bs.load = None;
                bs.buf = buf;
                bs.edit_gen = bs.edit_gen.wrapping_add(1);
                bs.wrap_extend_from = None;
                // The buffer is the whole file now, so it is no longer a tail:
                // `size_at_open` would be describing a view that this read has
                // just replaced with the file itself. Read-only is left alone —
                // that was asked for, and a whole read does not unask it.
                bs.tail = None;
                // And every row of the file is here, so the file's line 1 is
                // row 0 and there is nothing left to count.
                bs.lines_before = Some(0);
                bs.line_count = None;
                self.highlight_dirty = true;
                self.diag_dirty = true;
                self.lsp_sync();
                // A reload of a picture is that picture again (a revert, or a buffer that
                // waited its turn behind another file).
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
        // The name is set before any row arrives, so detection and the title
        // bar are right from the first frame.
        assert!(ed.bs().buf.name.is_some());
        assert!(ed.loading());
        finish(&mut ed);
        assert!(!ed.loading());
        assert_eq!(rows(&ed), ["one", "two", "three"], "no leading blank row");
    }

    #[test]
    fn a_loading_editor_draws_a_frame_and_says_so() {
        // The requirement: the screen is never a dead blank. A frame is drawn
        // from the first iteration, and the status line says what is happening.
        let body: String = (0..300_000).map(|i| format!("row {i}\n")).collect();
        let t = Temp::new("slow.txt", body.as_bytes());
        let mut ed = ed();
        ed.start_load(&t.0).expect("start");
        // Draw WITHOUT polling: this is the very first frame, before any row
        // has been adopted.
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
        // Poll until the count moves — or the load finishes first, which is
        // also fine. One poll is not enough to assert anything: the first one
        // may only carry the encoding decision.
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
        // Adopt until there is something to edit.
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
        // The bug this fixes: a file that is not valid UTF-8 used to be refused
        // outright. It is now the last rung of the ladder, so it opens — and
        // the buffer remembers how to save it back.
        let t = Temp::new("latin1b.bin", b"caf\xe9 latin-1\n");
        let mut ed = ed();
        ed.start_load(&t.0).expect("start");
        finish(&mut ed);
        assert_eq!(rows(&ed), ["caf\u{e9} latin-1"]);
        assert_eq!(ed.bs().buf.encoding, crate::encoding::Encoding::Cp1252);
        // And it saves back to the same bytes rather than being rewritten as
        // UTF-8 with a replacement character in it.
        assert_eq!(ed.bs().buf.file_bytes().unwrap(), b"caf\xe9 latin-1\n");
    }

    #[test]
    fn a_loading_utf16_file_falls_back_to_the_eager_path() {
        // UTF-16 rows cannot be found by scanning bytes, so the streaming
        // reader declines and the file is read whole. The user sees the file;
        // the editor is not left stuck in a loading state.
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
        // The quiet bug: the BOM validated as UTF-8, so the file opened with
        // U+FEFF as the buffer's first character — invisible, but Home, click
        // positioning and `^`-anchored regexes all saw it.
        let bytes = [&[0xEF, 0xBB, 0xBF][..], b"hello\n"].concat();
        let t = Temp::new("bom.txt", &bytes);
        let mut ed = ed();
        ed.start_load(&t.0).expect("start");
        finish(&mut ed);
        assert_eq!(rows(&ed), ["hello"], "no U+FEFF in the first row");
        assert!(!ed.bs().buf.row(0).contains(&'\u{FEFF}'));
        assert_eq!(ed.bs().buf.encoding, crate::encoding::Encoding::Utf8Bom);
        // And the BOM comes back on save.
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
        // A UTF-16 file that cannot be decoded: odd byte length, so it is
        // neither streamable nor decodable. The message reaches the status
        // line rather than a thread nobody reads.
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
        // Not a status line: with nothing on screen there is nothing to attach
        // one to, so this one is the caller's to report.
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
        // The common case, and the one that must not regress: an ordinary file
        // is fully present by the time the first poll happens, so the user sees
        // no loading state at all.
        let t = Temp::new("small.txt", b"let x = 1;\n");
        let mut ed = ed();
        ed.start_load(&t.0).expect("start");
        // Poll once; the worker may need a moment, so allow a few.
        let deadline = Instant::now() + Duration::from_secs(5);
        while ed.loading() {
            assert!(Instant::now() < deadline);
            ed.load_poll();
        }
        assert_eq!(rows(&ed), ["let x = 1;"]);
        // The flash reports the line count, as the old eager path did.
        let status = ed.status_text().unwrap_or_default();
        assert!(status.contains('1'), "reports the line count: {status:?}");
    }
}
