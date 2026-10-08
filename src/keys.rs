//! Key events in: the completion popup and a prompt take theirs first, and the
//! rest go through the keymaps (`commands.rs`), which is where every key's
//! meaning is written down.

use crate::keymap::Key;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::editor::Editor;

impl Editor {
    // ---------- key dispatch ----------

    pub fn handle_key(&mut self, key: KeyEvent) {
        // Keep the soft-wrap visual-row table fresh (M-\): a resize or the
        // M-\ toggle itself may have changed the wrap width.
        self.ensure_wrap_prefix();
        // Live completion popup: navigation/accept/cancel first; anything
        // else closes it and falls through to normal handling (typing and
        // backspace stay open — they re-request with the new prefix).
        if self
            .completion
            .as_ref()
            .is_some_and(|c| !c.items.is_empty())
        {
            let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
            let alt = key.modifiers.contains(KeyModifiers::ALT);
            let plain = !ctrl && !alt;
            match key.code {
                KeyCode::Up if plain => {
                    self.completion_up();
                    return;
                }
                KeyCode::Down if plain => {
                    self.completion_down();
                    return;
                }
                KeyCode::Char('p') if ctrl && !alt => {
                    self.completion_up();
                    return;
                }
                KeyCode::Char('n') if ctrl && !alt => {
                    self.completion_down();
                    return;
                }
                KeyCode::Enter | KeyCode::Tab => {
                    self.completion_accept();
                    return;
                }
                KeyCode::Esc => {
                    self.completion_close();
                    return;
                }
                KeyCode::Backspace if plain => {}
                KeyCode::Char(_) if plain => {}
                _ => self.completion_close(),
            }
        }
        // A prompt edits its own line of text; everything else — the text, a
        // list, a diff, a help page, M-x — goes through the keymaps.
        if let Some(p) = self.prompt.take() {
            self.handle_prompt_key(p, key);
            return;
        }
        self.dispatch_key(Key::from_event(&key));
    }
}
