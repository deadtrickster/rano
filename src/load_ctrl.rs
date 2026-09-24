//! The editor's side of async loading: adopt rows into the buffer as they
//! arrive, and say so on the status line while it is happening.
//!
//! The pattern is `lsp_ctrl.rs`'s: a job held on the editor, polled once per
//! loop iteration, returning whether anything changed so the frame is redrawn.
//! What is different here is that the job feeds the *buffer* rather than a
//! side channel, so adoption is what makes the file appear.

use crate::buffer::Buffer;
use crate::editor::Editor;
use crate::loader::{ADOPT_BUDGET as budget, Adopted, LoadJob};
use std::path::Path;

impl Editor {
    /// Start reading `path` on a worker. The buffer becomes an empty document
    /// with that name — so language detection, the title bar and the gutter are
    /// right from the first frame — and rows are adopted as they arrive.
    ///
    /// Returns whether a job started; a file that cannot be opened is reported
    /// by the caller (it is the one failure that should not become a status
    /// line, because there is nothing on screen to attach it to).
    pub(crate) fn start_load(&mut self, path: &Path) -> std::io::Result<()> {
        let job = LoadJob::spawn(path.to_path_buf())?;
        {
            let bs = self.bs_mut();
            bs.buf = Buffer::new();
            bs.buf.name = Some(path.to_path_buf());
            // The rows arrive one batch at a time; one empty row is what a
            // buffer holds before any do, and the invariant `Buffer` needs.
            bs.cursor = crate::buffer::Pos { row: 0, col: 0 };
            bs.scroll = 0;
            bs.load = Some(job);
        }
        // A new file is a new language, a new LSP session and a new highlight.
        self.lsp_sync();
        self.highlight_dirty = true;
        self.ensure_wrap_prefix();
        Ok(())
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
            let bs = self.bs_mut();
            // An append, not an edit: tell the wrap table it can extend from
            // where it already is rather than re-measuring the whole file.
            let was = bs.buf.lines.len();
            // The buffer starts with one empty row. The first batch replaces
            // it rather than following it, so a file does not appear to begin
            // with a blank line.
            if bs.buf.lines.len() == 1 && bs.buf.lines[0].is_empty() && !bs.buf.modified {
                bs.buf.lines.clear();
            }
            bs.buf.lines.append(&mut rows);
            // The first batch replaces the seed row, so the table has to move
            // its start; after that every batch is a pure append.
            bs.wrap_extend_from = Some(
                if was == 1 && bs.buf.lines.len() > 1 && bs.wrap_rows.len() <= 1 {
                    0
                } else {
                    was.min(bs.wrap_rows.len())
                },
            );
            bs.edit_gen = bs.edit_gen.wrapping_add(1);
            self.highlight_dirty = true;
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
                        Ok(read) => {
                            let bs = self.bs_mut();
                            bs.buf.lines = read.lines;
                            bs.buf.crlf = read.crlf;
                            bs.buf.encoding = read.encoding;
                            bs.cursor = crate::buffer::Pos { row: 0, col: 0 };
                            bs.scroll = 0;
                            bs.edit_gen = bs.edit_gen.wrapping_add(1);
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
                let rows = {
                    let bs = self.bs_mut();
                    bs.load = None;
                    bs.buf.crlf = crlf;
                    if bs.buf.lines.is_empty() {
                        bs.buf.lines.push(Vec::new());
                    }
                    bs.buf.lines.len()
                };
                self.flash(&format!(
                    "Read {} line{}",
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

    /// Whether a load is in flight. Used by the loop to keep adopting, and by
    /// the status line to say so rather than looking frozen.
    pub(crate) fn loading(&self) -> bool {
        self.bs().load.is_some()
    }

    /// The status-line text for a load in flight, or `None`.
    ///
    /// The operator's criterion from TODO.md §13.6 — "the minimum that makes
    /// the user happy and not confused" — is why this exists at all: a blank
    /// frame is confusing, "reading huge.log" is not.
    pub(crate) fn loading_text(&self) -> Option<String> {
        let job = self.bs().load.as_ref()?;
        Some(format!(
            "Reading… {} line{} so far",
            job.rows_read,
            crate::editor::plural(job.rows_read)
        ))
    }

    /// Abandon an in-flight load: `^C`, an edit, or another file.
    pub(crate) fn cancel_load(&mut self) {
        if let Some(job) = self.bs().load.as_ref() {
            job.cancel();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::buffer::Pos;
    use crate::config;
    use crate::editor::Editor;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
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
        ed.bs()
            .buf
            .lines
            .iter()
            .map(|l| l.iter().collect())
            .collect()
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
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).expect("backend");
        terminal.draw(|f| crate::ui::draw(f, &ed)).expect("draw");
        let buf = terminal.backend().buffer();
        let status: String = (0..80)
            .map(|x| buf.cell((x, 21)).unwrap().symbol())
            .collect();
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
        while ed.bs().buf.lines.len() < 10 {
            assert!(Instant::now() < deadline, "no rows adopted");
            ed.load_poll();
            std::thread::sleep(Duration::from_millis(1));
        }
        let adopted = ed.bs().buf.lines.len();
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
        assert!(!ed.bs().buf.lines[0].contains(&'\u{FEFF}'));
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
