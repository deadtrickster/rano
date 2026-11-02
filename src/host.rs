//! What a host's loop calls between events: [`Editor::tick`], and
//! [`Editor::next_wakeup`] for when to call it again.

use std::time::{Duration, Instant};

use crate::editor::Editor;
use crate::keymap::Keymap;

/// How long an idle editor may go without a tick: the pollers are how an async
/// reply or an expired status reaches the screen.
const IDLE_WAIT: Duration = Duration::from_millis(200);

/// While a file is arriving: a frame at 120 Hz. Longer, and the text appears in
/// lumps rather than as a stream.
const LOADING_WAIT: Duration = Duration::from_millis(8);

impl Editor {
    /// Everything the editor does between events, at `now`; true when the screen
    /// changed. Call once per loop iteration, before every draw. Never blocks.
    pub fn tick(&mut self, now: Instant) -> bool {
        // The pollers first: what they adopt is what the refreshes and the scroll
        // below must see.
        let mut dirty = self.load_poll();
        // A tail's line count, which decides whether the gutter can state a number
        // at all. After the load poll: the tail's rows are what it is an offset into.
        dirty |= self.line_count_poll();
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
        // Before the scroll: `--line` centres the target, and centring sets the
        // scroll; adjust_scroll first would pull the view back. See `apply_startup_pos`.
        self.apply_startup_pos();
        self.adjust_scroll(self.text_h);
        self.adjust_scroll_x();
        // The frame's highlight, after the scroll the window is measured against:
        // once per frame, and only this window on a huge file.
        self.ensure_highlight();
        dirty
    }

    /// Stack a host's keymap over the editor's; its bindings win key by key in
    /// every mode. A name that is not an editor command comes back as `KeyOutcome::Host`.
    pub fn push_keymap(&mut self, map: Keymap) {
        self.host_keymaps.push(map);
    }

    /// Take the last map [`Self::push_keymap`] stacked, if any.
    pub fn pop_keymap(&mut self) -> Option<Keymap> {
        self.host_keymaps.pop()
    }

    /// The editor has been asked to exit and wants to be closed; tearing down is
    /// the host's.
    pub fn wants_quit(&self) -> bool {
        self.quit
    }

    /// How long a host may wait for input before ticking again: zero while the
    /// loader is saturated, never past a prefix card's due moment.
    pub fn next_wakeup(&self) -> Duration {
        let wait = if self.load_saturated {
            Duration::ZERO
        } else if self.loading() || self.counting() {
            LOADING_WAIT
        } else {
            IDLE_WAIT
        };
        match self.card_due() {
            // The +1 ms rounds past the deadline: waking a hair early would
            // find the card not due and sleep the whole wait again.
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
    use crate::term::{KeyCode, KeyEvent, Mods};

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
        ed.handle_key(KeyEvent::new(KeyCode::Char('t'), Mods::ALT));
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
        // A syntax error typed into Rust surfaces 300 ms after the last edit,
        // through tick's `now` rather than the wall clock.
        let mut buf = Buffer::new();
        buf.set_rows(vec!["fn main() {}".chars().collect()]);
        buf.name = Some("t.rs".into());
        let mut ed = Editor::new(buf, Config::default());
        ed.text_w = 80;
        ed.text_h = 10;
        ed.tick(Instant::now());
        ed.bs_mut().cursor = Pos { row: 0, col: 11 };
        ed.handle_key(KeyEvent::new(KeyCode::Char('('), Mods::NONE));
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

    // ---- keys a host routes ----

    use crate::editor::KeyOutcome;
    use crate::keymap::Keymap;

    fn key(code: KeyCode, mods: Mods) -> KeyEvent {
        KeyEvent::new(code, mods)
    }

    fn text(ed: &Editor) -> Vec<String> {
        ed.bs().buf.rows().map(|r| r.iter().collect()).collect()
    }

    #[test]
    fn an_unbound_key_is_handed_back_and_changes_nothing() {
        let mut ed = ed("abc");
        let out = ed.handle_key(key(KeyCode::F(12), Mods::NONE));
        assert_eq!(out, KeyOutcome::Unhandled);
        assert_eq!(text(&ed), vec!["abc"]);
        assert!(ed.pending.keys.is_empty());
        assert!(
            ed.status_text().is_none(),
            "nothing was reported to the reader"
        );
    }

    #[test]
    fn typing_and_commands_are_handled() {
        let mut ed = ed("");
        let typed = ed.handle_key(key(KeyCode::Char('a'), Mods::NONE));
        assert_eq!(typed, KeyOutcome::Handled);
        assert_eq!(text(&ed), vec!["a"]);
        let moved = ed.handle_key(key(KeyCode::Left, Mods::NONE));
        assert_eq!(moved, KeyOutcome::Handled);
        // A prefix waiting for its next key is the editor's too.
        let prefix = ed.handle_key(key(KeyCode::Char('t'), Mods::ALT));
        assert_eq!(prefix, KeyOutcome::Handled);
    }

    #[test]
    fn a_host_chord_comes_back_by_name_in_every_mode() {
        let mut ed = ed("abc");
        let mut map = Keymap::new("host");
        map.bind("C-q", "host-close-pane");
        ed.push_keymap(map);
        let close = || key(KeyCode::Char('q'), Mods::CTRL);
        assert_eq!(
            ed.handle_key(close()),
            KeyOutcome::Host("host-close-pane".into())
        );
        assert_eq!(text(&ed), vec!["abc"]);
        // Over a mode as well: M-x's own map is in effect there.
        ed.handle_key(key(KeyCode::Char('x'), Mods::ALT));
        assert!(ed.palette.is_some());
        assert_eq!(
            ed.handle_key(close()),
            KeyOutcome::Host("host-close-pane".into())
        );
    }

    #[test]
    fn a_host_chord_comes_back_over_an_open_prompt() {
        let mut ed = ed("abc");
        let mut map = Keymap::new("host");
        map.bind("C-q", "host-close-pane");
        ed.push_keymap(map);
        ed.prompt = Some(crate::prompt::Prompt {
            kind: crate::prompt::PromptKind::Search,
            text: String::new(),
            cursor: 0,
        });
        assert_eq!(
            ed.handle_key(key(KeyCode::Char('q'), Mods::CTRL)),
            KeyOutcome::Host("host-close-pane".into())
        );
        assert!(
            ed.prompt.is_some(),
            "the prompt stays open for the host to come back to"
        );
        assert_eq!(
            ed.handle_key(key(KeyCode::Char('b'), Mods::NONE)),
            KeyOutcome::Handled
        );
        assert_eq!(
            text(&ed),
            vec!["abc"],
            "typing went into the prompt, not the text"
        );
    }

    #[test]
    fn a_host_map_does_not_stop_typing_and_can_rebind() {
        let mut ed = ed("");
        let mut map = Keymap::new("host");
        // An editor command under a host's key: it simply runs.
        map.bind("<f12>", "undo");
        ed.push_keymap(map);
        // Unbound characters are still typed: the editor's own map decides that,
        // not the host's on top of it.
        ed.handle_key(key(KeyCode::Char('z'), Mods::NONE));
        assert_eq!(text(&ed), vec!["z"]);
        let out = ed.handle_key(key(KeyCode::F(12), Mods::NONE));
        assert_eq!(out, KeyOutcome::Handled);
        assert_eq!(text(&ed), vec![""], "F12 ran undo");
        // Popped, F12 is unbound again.
        assert_eq!(ed.pop_keymap().map(|m| m.name), Some("host".into()));
        assert_eq!(
            ed.handle_key(key(KeyCode::F(12), Mods::NONE)),
            KeyOutcome::Unhandled
        );
    }

    #[test]
    fn exit_is_reported_as_wanting_to_quit() {
        let mut ed = ed("x");
        assert!(!ed.wants_quit());
        ed.handle_key(key(KeyCode::Char('x'), Mods::CTRL));
        assert!(ed.wants_quit(), "an unmodified editor exits at once");
    }

    #[test]
    fn a_host_opens_a_file_at_a_position_and_ticks_it_in() {
        // The whole embedding flow, short of a terminal: an empty editor, a pane,
        // a file opened at line 40 column 3, and ticks until it is read.
        let path = std::env::temp_dir().join(format!("rano_host_open_{}.txt", std::process::id()));
        let text: String = (1..=80).map(|i| format!("line {i}\n")).collect();
        std::fs::write(&path, &text).unwrap();
        let mut ed = Editor::new(Buffer::new(), Config::default());
        ed.set_area(crate::editor::Area::new(5, 2, 60, 20));
        ed.open_at(&path, 40, Some(3)).unwrap();
        for _ in 0..500 {
            ed.tick(Instant::now());
            if !ed.loading() {
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        ed.tick(Instant::now());
        let _ = std::fs::remove_file(&path);
        assert_eq!(ed.bs().buf.name.as_deref(), Some(path.as_path()));
        assert_eq!(ed.bs().cursor, Pos { row: 39, col: 2 });
        let s = ed.bs().scroll;
        assert!(s <= 39 && 39 < s + ed.text_h, "in view: scroll {s}");
    }
}
