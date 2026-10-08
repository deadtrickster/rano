//! What a host's loop calls between events: [`Editor::tick`], and
//! [`Editor::next_wakeup`] to know when to call it again.
//!
//! The binary's run loop used to make some twenty calls of its own between
//! two events — the loader, the language server, `^T` jobs, the update
//! check, debounced diagnostics, status expiry, the which-key card's delay,
//! the overlays' refreshes, the scroll. A host embedding the editor as a pane
//! would have had to copy that list and keep it in step with rano; now it is
//! one call, and the binary makes the same one.

use std::time::{Duration, Instant};

use crate::editor::Editor;

/// How long an idle editor may be left without a tick. Nothing is waiting on
/// it then, but the pollers are how a language server's reply, a finished
/// `^T` job or an expired status message reach the screen, so "idle" is a
/// long wait rather than a forever one.
const IDLE_WAIT: Duration = Duration::from_millis(200);

/// While a file is arriving: a frame at 120 Hz. Longer, and the text appears
/// in lumps and the load looks like a stutter rather than a stream.
const LOADING_WAIT: Duration = Duration::from_millis(8);

impl Editor {
    /// Everything the editor does between events, at `now`. Returns whether
    /// the screen changed, i.e. whether the host should draw.
    ///
    /// Call it once per loop iteration and before every draw: it is also what
    /// brings the frame's highlight and scroll up to date, so a host that
    /// draws after handling a key must still tick first even if it is only
    /// drawing because of that key. Never blocks; each poller does a bounded
    /// amount of work and comes back.
    pub fn tick(&mut self, now: Instant) -> bool {
        // The pollers first: what they adopt (rows, diagnostics, a job's
        // output) is what the refreshes and the scroll below must see.
        // The load is bounded per call, never waiting, and reports its own
        // state changes.
        let mut dirty = self.load_poll();
        dirty |= self.diag_flush(now);
        dirty |= self.tick_status();
        dirty |= self.lsp_poll();
        dirty |= self.lsp_flush(now);
        self.completion_retry_poll();
        dirty |= self.exec_poll();
        dirty |= self.update_poll();
        // A file from the command line whose buffer just became current.
        dirty |= self.start_pending_load();
        dirty |= self.refresh_prompt_hints();
        dirty |= self.refresh_diff_view();
        dirty |= self.refresh_info_view();
        // A prefix's card appears after a pause: redraw when it becomes due.
        let card = self.pending_card().is_some();
        if card != self.pending.card_shown {
            self.pending.card_shown = card;
            dirty = true;
        }
        // Before the scroll: `--line` CENTRES the target, and centring sets
        // the scroll. Running `adjust_scroll` first would then pull the view
        // back to the nearest edge, which is the opposite of centring. It is
        // also why this waits for the row — see `apply_startup_pos`.
        self.apply_startup_pos();
        self.adjust_scroll(self.text_h);
        self.adjust_scroll_x();
        // The frame's highlight, after the scroll the window is measured
        // against. Lazy on purpose: a burst of keystrokes between two frames
        // costs one highlight here rather than one per key, and a huge file's
        // first highlight is this window rather than the whole document. A
        // no-op when the highlight already covers the viewport, which is why
        // it does not need to know whether a frame will be drawn.
        self.ensure_highlight();
        dirty
    }

    /// How long a host may wait for input before it must tick again.
    ///
    /// Zero while the loader is saturated — it filled its adoption budget, so
    /// more is ready — because the budget is what keeps one iteration short,
    /// not what paces the load: sleeping between batches made a 2.6M-line
    /// file take ten seconds instead of one, with the disk idle in between.
    /// Shorter than idle while a file is arriving, and never past the moment
    /// a pending prefix's card is due, so it appears on time rather than at
    /// the next keystroke.
    pub fn next_wakeup(&self) -> Duration {
        let wait = if self.load_saturated {
            Duration::ZERO
        } else if self.loading() {
            LOADING_WAIT
        } else {
            IDLE_WAIT
        };
        match self.card_due() {
            // The +1 ms rounds past the deadline: waking a hair early would
            // find the card not yet due and sleep the whole wait again.
            Some(d) => wait.min(Duration::from_millis(d.as_millis() as u64 + 1)),
            None => wait,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::buffer::{Buffer, Pos};
    use crate::config::Config;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn ed(text: &str) -> Editor {
        let mut buf = Buffer::new();
        buf.set_rows(text.lines().map(|l| l.chars().collect()).collect());
        let mut ed = Editor::new(buf, Config::default());
        ed.text_w = 80;
        ed.text_h = 10;
        ed
    }

    #[test]
    fn tick_adopts_a_loading_file() {
        let path = std::env::temp_dir().join(format!("rano_tick_load_{}", std::process::id()));
        let text: String = (1..=300).map(|i| format!("row {i}\n")).collect();
        std::fs::write(&path, &text).unwrap();
        let mut ed = Editor::new(Buffer::new(), Config::default());
        ed.start_load(&path).unwrap();
        let mut changed = false;
        for _ in 0..500 {
            changed |= ed.tick(Instant::now());
            if !ed.loading() {
                break;
            }
            std::thread::sleep(ed.next_wakeup().max(Duration::from_millis(1)));
        }
        let _ = std::fs::remove_file(&path);
        assert!(!ed.loading(), "the load finished under tick alone");
        assert!(changed, "and tick reported it");
        assert_eq!(ed.bs().buf.row_count(), 300);
    }

    #[test]
    fn tick_applies_a_pending_position_and_scrolls_to_it() {
        let text: String = (1..=100).map(|i| format!("line {i}\n")).collect();
        let mut ed = ed(&text);
        ed.bs_mut().goto = Some(Pos { row: 59, col: 2 });
        ed.tick(Instant::now());
        assert_eq!(ed.bs().cursor, Pos { row: 59, col: 2 });
        let scroll = ed.bs().scroll;
        assert!(
            scroll <= 59 && 59 < scroll + ed.text_h,
            "row 59 is in view, scroll {scroll}"
        );
    }

    #[test]
    fn tick_expires_the_cursor_position_readout() {
        let mut ed = ed("x");
        ed.loc_until = Some(Instant::now() - Duration::from_secs(1));
        assert!(ed.tick(Instant::now()), "clearing it is a change on screen");
        assert!(ed.loc_until.is_none());
        assert!(!ed.tick(Instant::now()), "and then nothing is");
    }

    #[test]
    fn tick_shows_a_prefix_card_once_it_is_due() {
        let mut ed = ed("- [ ] task");
        ed.handle_key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::ALT));
        assert!(!ed.tick(Instant::now()), "the card waits a moment");
        // The loop is told to come back when it is due, not later.
        let due = ed.card_due().expect("waiting for its card");
        assert!(ed.next_wakeup() <= due + Duration::from_millis(1));
        ed.pending.since = Some(Instant::now() - Duration::from_secs(1));
        assert!(ed.tick(Instant::now()), "due now: draw it");
        assert!(!ed.tick(Instant::now()), "shown once, not every tick");
    }

    #[test]
    fn tick_flushes_debounced_diagnostics() {
        // A syntax error typed into Rust surfaces 300 ms after the last
        // edit, through tick's `now` rather than the wall clock.
        let mut buf = Buffer::new();
        buf.set_rows(vec!["fn main() {}".chars().collect()]);
        buf.name = Some("t.rs".into());
        let mut ed = Editor::new(buf, Config::default());
        ed.text_w = 80;
        ed.text_h = 10;
        ed.tick(Instant::now());
        ed.bs_mut().cursor = Pos { row: 0, col: 11 };
        ed.handle_key(KeyEvent::new(KeyCode::Char('('), KeyModifiers::NONE));
        let now = Instant::now();
        ed.tick(now);
        assert!(ed.bs().syntax_diags.is_empty(), "not within the debounce");
        assert!(ed.tick(now + Duration::from_millis(400)));
        assert!(!ed.bs().syntax_diags.is_empty(), "flushed after it");
    }

    #[test]
    fn an_idle_editor_waits_the_idle_wait() {
        assert_eq!(ed("x").next_wakeup(), IDLE_WAIT);
    }
}
