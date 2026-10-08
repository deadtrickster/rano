//! Key events in: the completion popup and a prompt take theirs first, and the
//! rest go through the keymaps (`commands.rs`), which is where every key's
//! meaning is written down.

use crate::keymap::Key;
use crate::term::{KeyCode, KeyEvent};

use crate::editor::Editor;

/// What became of a key, for a host that routes keys between the editor and
/// itself.
///
/// The editor does not know what else is on the screen, so a key it has no
/// use for is handed back rather than swallowed: a host that sends every key
/// to its focused pane can then try the key on its own bindings. Keys a host
/// wants to take *before* the editor — a chord that must work whatever the
/// editor would do with it — go in a host keymap instead
/// ([`Editor::push_keymap`]), and come back as [`KeyOutcome::Host`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyOutcome {
    /// The editor used the key: it ran a command, typed a character, edited
    /// a prompt, moved a popup, or is waiting for the rest of a prefix.
    Handled,
    /// Nothing is bound to the key where the editor is, and it is not one
    /// the editor types. The editor's state is unchanged.
    Unhandled,
    /// The key completed a sequence bound, in a host keymap, to a command
    /// the editor does not have: the host's to run. The name is the one the
    /// host bound.
    Host(String),
}

impl Editor {
    // ---------- key dispatch ----------

    /// One key from the terminal (or the host). See [`KeyOutcome`] for what
    /// comes back.
    pub fn handle_key(&mut self, key: KeyEvent) -> KeyOutcome {
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
            let ctrl = key.mods.ctrl();
            let alt = key.mods.alt();
            let plain = !ctrl && !alt;
            match key.code {
                KeyCode::Up if plain => {
                    self.completion_up();
                    return KeyOutcome::Handled;
                }
                KeyCode::Down if plain => {
                    self.completion_down();
                    return KeyOutcome::Handled;
                }
                KeyCode::Char('p') if ctrl && !alt => {
                    self.completion_up();
                    return KeyOutcome::Handled;
                }
                KeyCode::Char('n') if ctrl && !alt => {
                    self.completion_down();
                    return KeyOutcome::Handled;
                }
                KeyCode::Enter | KeyCode::Tab => {
                    self.completion_accept();
                    return KeyOutcome::Handled;
                }
                KeyCode::Esc => {
                    self.completion_close();
                    return KeyOutcome::Handled;
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
            return KeyOutcome::Handled;
        }
        self.dispatch_key(Key::from_event(&key))
    }
}
