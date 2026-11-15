//! What a host's loop calls between events: [`Editor::tick`], and
//! [`Editor::next_wakeup`] for when to call it again.

use std::time::{Duration, Instant};

use crate::editor::{Editor, StatusMode};
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
        dirty |= self.panel.tick_turn();
        dirty |= self.lsp_poll();
        dirty |= self.lsp_flush(now);
        self.completion_retry_poll();
        dirty |= self.exec_poll();
        dirty |= self.update_poll();
        // The bottom terminal's ptys: drained on every tick, so `ls` answers even when the operator
        // is not pressing keys. Feeding only on a draw is what made it look hung.
        dirty |= self.bottom.feed();
        // A file from the command line whose buffer just became current.
        dirty |= self.start_pending_load();
        dirty |= self.refresh_prompt_hints();
        dirty |= self.refresh_diff_view();
        dirty |= self.refresh_info_view();
        // The panel follows the focused buffer into its own folder: one `git` call per buffer
        // change, never per frame.
        dirty |= self.panel_watch_folder();
        // The status row comes and goes with a prompt or a message, and the scroll below measures
        // against the text height: a frame that changed it and not this would be a row out. Only
        // `WhenBusy` can change it, so the standalone binary keeps the height it computed.
        if !self.area.is_empty() && self.chrome.status != StatusMode::Always {
            let h = (self.area.h as usize).saturating_sub(self.chrome_rows());
            if h != self.text_h {
                self.text_h = h;
                dirty = true;
            }
        }
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
        // No special case for a running turn's status line: the ordinary idle wait already
        // ticks several times a second, and `Panel::tick_turn` marks the frame dirty once per
        // whole second. A clamp here could only shorten a wait that is already shorter.
        match self.card_due() {
            // The +1 ms rounds past the deadline: waking a hair early would
            // find the card not due and sleep the whole wait again.
            Some(d) => wait.min(Duration::from_millis(d.as_millis() as u64 + 1)),
            None => wait,
        }
    }
}

/// A key as the host answers it: the bottom terminal's keys first, then the editor's maps. The
/// binary routes through here, so rano and an embedder agree — and an embedder that only ever
/// calls `Editor::handle_key` never forks a shell it cannot draw.
pub fn key(ed: &mut Editor, k: crate::term::KeyEvent) -> crate::editor::KeyOutcome {
    if ed.bottom_key(&k) {
        return crate::editor::KeyOutcome::Handled;
    }
    ed.handle_key(k)
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
    fn m_zero_opens_the_bottom_terminal() {
        let mut ed = ed("abc");
        // The host's map is where the toggle lives: an embedder pushes it, or has no terminal.
        let mut host = crate::keymap::Keymap::new("host");
        host.bind("M-0", "toggle-terminal");
        ed.push_keymap(host);
        let out = super::key(&mut ed, key(crate::term::KeyCode::Char('0'), Mods::ALT));
        assert_eq!(out, crate::editor::KeyOutcome::Handled);
        assert!(ed.bottom.open, "M-0 opens it");
        assert_eq!(ed.bottom.tabs.len(), 1, "one shell");
    }

    /// A focused terminal must not leak a chord to the editor: Tab does not indent, C-x does not
    /// ask to quit, and Delete deletes nothing in the file.
    #[test]
    fn a_focused_terminal_leaks_no_key_to_the_editor() {
        let mut ed = ed("abc");
        ed.bottom.open = true;
        ed.focus = crate::editor::Focus::Terminal;
        let before = text(&ed);
        for (code, mods) in [
            (KeyCode::Tab, Mods::NONE),
            (KeyCode::Char('c'), Mods::CTRL),
            (KeyCode::Char('d'), Mods::CTRL),
            (KeyCode::Char('k'), Mods::CTRL),
            (KeyCode::Delete, Mods::NONE),
            (KeyCode::Char('x'), Mods::CTRL),
        ] {
            super::key(&mut ed, key(code, mods));
        }
        assert_eq!(text(&ed), before, "nothing reached the buffer");
        assert!(!ed.wants_quit(), "C-x did not ask to quit");
    }

    /// A paste at a focused terminal is the shell's input, not the buffer's.
    #[test]
    fn a_paste_at_a_focused_terminal_reaches_the_pty() {
        let mut ed = ed("abc");
        ed.bottom.open = true;
        ed.focus = crate::editor::Focus::Terminal;
        ed.bottom
            .new_tab(std::path::Path::new("/tmp"), 40, 6, 200)
            .expect("a shell");
        let before = text(&ed);
        ed.paste_text("echo pasted-123");
        let tab = &mut ed.bottom.tabs[ed.bottom.active];
        let mut seen = false;
        for _ in 0..3000 {
            tab.feed();
            if tab.lines().iter().any(|l| l.plain().contains("pasted-123")) {
                seen = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(seen, "the pty echoed the paste");
        assert_eq!(text(&ed), before, "the buffer is unmodified");
    }

    /// A dead shell's tab swallows keys, changes nothing in the buffer, and Enter closes it.
    #[test]
    fn a_dead_shell_takes_no_keys_and_enter_closes_it() {
        let mut ed = ed("abc");
        ed.bottom.open = true;
        ed.focus = crate::editor::Focus::Terminal;
        ed.bottom
            .new_tab(std::path::Path::new("/tmp"), 40, 6, 200)
            .expect("a shell");
        ed.bottom.tabs[0].send(b"exit 0\n");
        for _ in 0..3000 {
            ed.bottom.tabs[0].feed();
            if ed.bottom.tabs[0].exited.is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(ed.bottom.tabs[0].exited, Some(0), "the exit was noticed");
        let before = text(&ed);
        super::key(&mut ed, key(KeyCode::Char('x'), Mods::NONE));
        assert_eq!(text(&ed), before, "nothing reached the buffer");
        assert_eq!(ed.bottom.tabs.len(), 1, "the dead tab is still there");
        super::key(&mut ed, key(KeyCode::Enter, Mods::NONE));
        assert!(ed.bottom.tabs.is_empty(), "Enter closed the dead tab");
    }

    /// A click in the strip focuses it, and the next key is the shell's: the buffer is untouched
    /// and the pty echoes it.
    #[test]
    fn a_click_in_the_strip_sends_the_next_key_to_the_shell() {
        use crate::term::{MouseButton, MouseEvent, MouseKind};
        let mut ed = ed("abc");
        ed.bottom.open = true;
        ed.bottom.height = 12;
        ed.focus = crate::editor::Focus::Text;
        ed.bottom
            .new_tab(std::path::Path::new("/tmp"), 40, 6, 200)
            .expect("a shell");
        let press = MouseEvent {
            kind: MouseKind::Press(MouseButton::Left),
            x: 5,
            y: 20,
            mods: crate::term::event::Mods::NONE,
        };
        assert!(ed.bottom_mouse(press, 18, 12));
        assert_eq!(
            ed.focus,
            crate::editor::Focus::Terminal,
            "the click focused it"
        );
        let before = text(&ed);
        super::key(&mut ed, key(KeyCode::Char('y'), Mods::NONE));
        assert_eq!(text(&ed), before, "the buffer is unmodified");
        let tab = &mut ed.bottom.tabs[0];
        let mut seen = false;
        for _ in 0..3000 {
            tab.feed();
            if tab.lines().iter().any(|l| l.plain().contains('y')) {
                seen = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(
            seen,
            "the shell echoed the key: exited={:?} lines={:?}",
            ed.bottom.tabs[0].exited,
            ed.bottom.tabs[0]
                .lines()
                .iter()
                .map(|l| l.plain())
                .collect::<Vec<_>>()
        );
    }

    /// On the alternate screen the page keys are the program's: `less` pages its own document
    /// with PgDn, rather than rano scrolling a scrollback that is not on the glass.
    #[test]
    fn pgdn_reaches_less_on_the_alternate_screen() {
        let mut ed = ed("abc");
        let mut host = crate::keymap::Keymap::new("host");
        host.bind("M-0", "toggle-terminal");
        ed.push_keymap(host);
        super::key(&mut ed, key(KeyCode::Char('0'), Mods::ALT));
        assert!(ed.bottom.open, "the strip is open");
        let file = "/tmp/rano-alt-less.txt";
        ed.bottom.tabs[0].send(format!("seq 1 300 > {file}; less {file}\n").as_bytes());
        let mut top = String::new();
        for _ in 0..3000 {
            ed.bottom.tabs[0].feed();
            top = ed.bottom.tabs[0].lines()[0].plain();
            if top.trim() == "1" {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(top.trim(), "1", "less shows its first line: {top:?}");
        // PgDn through the host path: the map's scroll-down, but the program has the screen.
        super::key(&mut ed, key(KeyCode::PageDown, Mods::NONE));
        let mut moved = false;
        for _ in 0..3000 {
            ed.bottom.tabs[0].feed();
            if ed.bottom.tabs[0].lines()[0].plain().trim() != "1" {
                moved = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(moved, "PgDn paged less, not rano's scrollback");
        ed.bottom.tabs[0].send(b"q");
        let _ = std::fs::remove_file(file);
    }

    /// The terminal's chords are its map's, and the map's entries are commands: help, M-x and
    /// the bars read what the terminal answers, and a chord through the host path runs it.
    #[test]
    fn the_terminals_chords_are_map_commands() {
        use crate::keymap::{Lookup, lookup, parse_seq};
        let mut ed = ed("abc");
        for (chord, name) in [
            ("M-n", "terminal-new"),
            ("M-w", "terminal-close"),
            ("M-[", "terminal-prev"),
            ("M-]", "terminal-next"),
            ("<pgup>", "terminal-scroll-up"),
            ("<pgdn>", "terminal-scroll-down"),
            ("M-0", "terminal-focus-text"),
        ] {
            let seq = parse_seq(chord).expect("a chord");
            assert_eq!(
                lookup(&[&ed.keymaps.terminal], &seq),
                Lookup::Command(name),
                "{chord}"
            );
            assert!(
                crate::commands::command(name).is_some(),
                "{name} is a command"
            );
        }
        // Through the host path: M-n opens a second tab, and M-w closes it again.
        let mut host = crate::keymap::Keymap::new("host");
        host.bind("M-0", "toggle-terminal");
        ed.push_keymap(host);
        let m0 = key(KeyCode::Char('0'), Mods::ALT);
        super::key(&mut ed, m0);
        assert!(ed.bottom.open && ed.focus == crate::editor::Focus::Terminal);
        super::key(&mut ed, key(KeyCode::Char('n'), Mods::ALT));
        assert_eq!(ed.bottom.tabs.len(), 2, "M-n is the map's terminal-new");
        super::key(&mut ed, key(KeyCode::Char('w'), Mods::ALT));
        assert_eq!(ed.bottom.tabs.len(), 1, "and M-w its terminal-close");
    }

    /// An embedder that drives `Editor::handle_key` alone never forks a shell: the terminal's
    /// chords are the host's, so M-0, M-n and M-w are simply unbound keys there.
    #[test]
    fn an_editor_driven_through_handle_key_alone_never_forks() {
        let mut ed = ed("abc");
        for (code, mods) in [
            (KeyCode::Char('0'), Mods::ALT),
            (KeyCode::Char('n'), Mods::ALT),
            (KeyCode::Char('w'), Mods::ALT),
        ] {
            ed.handle_key(key(code, mods));
        }
        assert!(!ed.bottom.open, "no strip was opened");
        assert!(ed.bottom.tabs.is_empty(), "no shell was forked");
        assert_eq!(text(&ed).join("\n"), "abc", "and the file is untouched");
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

    /// Standalone rano ignores `Unhandled`, so its picture does not change — but the value itself is
    /// what the embedded pane reads to move focus, and it has to be honest about having no work.
    #[test]
    fn esc_with_nothing_to_cancel_is_handed_back() {
        let mut ed = ed("abc");
        assert_eq!(
            ed.handle_key(key(KeyCode::Esc, Mods::NONE)),
            KeyOutcome::Unhandled,
            "no mark, no view, no prompt: Esc is the host's"
        );
        // With a mark it is the editor's own, and it clears it.
        ed.handle_key(key(KeyCode::Char('a'), Mods::CTRL));
        assert!(ed.bs().mark.is_some(), "C-a set the mark");
        assert_eq!(
            ed.handle_key(key(KeyCode::Esc, Mods::NONE)),
            KeyOutcome::Handled
        );
        assert!(ed.bs().mark.is_none(), "Esc cleared it");
    }

    #[test]
    fn c_g_cancels_a_prompt_rather_than_typing_g() {
        let mut ed = ed("abc");
        ed.prompt = Some(crate::prompt::Prompt {
            kind: crate::prompt::PromptKind::Search,
            text: String::new(),
            cursor: 0,
        });
        assert_eq!(
            ed.handle_key(key(KeyCode::Char('g'), Mods::CTRL)),
            KeyOutcome::Handled
        );
        assert!(ed.prompt.is_none(), "C-g closes the prompt");
        assert_eq!(text(&ed), vec!["abc"], "and types nothing into the file");
    }

    #[test]
    fn a_host_chord_bound_to_an_editor_command_runs_it_over_a_prompt() {
        let mut ed = ed("abc");
        let mut map = Keymap::new("host");
        map.bind("C-q", "exit");
        ed.push_keymap(map);
        ed.prompt = Some(crate::prompt::Prompt {
            kind: crate::prompt::PromptKind::Search,
            text: String::new(),
            cursor: 0,
        });
        // The chord is the host's and the command it names is the editor's own: it runs, and the
        // prompt does not get to type the `q` it was pressed with.
        assert_eq!(
            ed.handle_key(key(KeyCode::Char('q'), Mods::CTRL)),
            KeyOutcome::Handled
        );
        assert!(ed.wants_quit(), "the host's chord reached `exit`");
        assert_eq!(text(&ed), vec!["abc"], "and typed nothing into the file");
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
