//! Key events in: the completion popup and a prompt take theirs first, and the
//! rest go through the keymaps in `commands.rs`.

use crate::keymap::Key;
use crate::term::{KeyCode, KeyEvent};

use crate::editor::Editor;

/// What became of a key, for a host that routes keys between the editor and
/// itself. A key the editor has no use for is handed back, not swallowed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyOutcome {
    /// The editor used the key: a command, a character, a prompt edit, a popup
    /// move, or a prefix waiting for its next key.
    Handled,
    /// Nothing is bound to the key here, and the editor does not type it; the
    /// editor's state is unchanged.
    Unhandled,
    /// The key completed a sequence a host keymap binds to a command the editor
    /// does not have: the host's to run, under the name the host bound.
    Host(String),
}

/// A key as the text a plugin reads: `n`, `M-x`, `C-a`, `S-Tab`, `Enter`, `Left`.
/// Deliberately narrow — a plugin reads a key, it does not parse a notation.
fn key_text(key: &crate::term::event::KeyEvent) -> String {
    use crate::term::event::{KeyCode, Mods};
    let mut out = String::new();
    if key.mods.ctrl() {
        out.push_str("C-");
    }
    if key.mods.alt() {
        out.push_str("M-");
    }
    if key.mods.shift() {
        out.push_str("S-");
    }
    match key.code {
        KeyCode::Char(c) => out.push(c),
        KeyCode::Enter => out.push_str("Enter"),
        KeyCode::Esc => out.push_str("Esc"),
        KeyCode::Tab => out.push_str("Tab"),
        KeyCode::Backspace => out.push_str("Backspace"),
        KeyCode::Delete => out.push_str("Delete"),
        KeyCode::Up => out.push_str("Up"),
        KeyCode::Down => out.push_str("Down"),
        KeyCode::Left => out.push_str("Left"),
        KeyCode::Right => out.push_str("Right"),
        KeyCode::Home => out.push_str("Home"),
        KeyCode::End => out.push_str("End"),
        KeyCode::PageUp => out.push_str("PageUp"),
        KeyCode::PageDown => out.push_str("PageDown"),
        other => out.push_str(&format!("{other:?}")),
    }
    let _ = Mods::NONE;
    out
}

impl Editor {
    // ---------- key dispatch ----------

    /// Hand a key to a provider's view, if one claims it; true when a plugin
    /// answered and the answer was opened. The key goes as `<path>\n<key>`.
    fn on_key_to_plugin(&mut self, key: &crate::term::event::KeyEvent) -> bool {
        if !self.bs().synthetic {
            return false;
        }
        let Some(path) = self.bs().buf.name.clone() else {
            return false;
        };
        let spelled = key_text(key);
        let ask = format!("{}\n{spelled}", path.display());
        let mut next = None;
        for plugin in self.plugin.iter().chain(self.builtins.iter()) {
            if !plugin.has("on_key") {
                continue;
            }
            match plugin.call("on_key", &ask) {
                Ok(Some(answer)) => {
                    next = Some(answer);
                    break;
                }
                Ok(None) => {}
                Err(e) => {
                    self.plugin_note = Some(format!("plugin {}: {e}", plugin.path().display()));
                }
            }
        }
        let Some(answer) = next else {
            return false;
        };
        // The answer is the path to show next: the reader keeps its page and zoom in it.
        if answer.trim().is_empty() {
            return false;
        }
        let path = std::path::PathBuf::from(answer.trim());
        if self.open_named(&path).is_err() {
            self.flash(&format!("{}: cannot open", path.display()));
        }
        true
    }

    /// One key from the terminal (or the host).
    pub fn handle_key(&mut self, key: KeyEvent) -> KeyOutcome {
        // Keep the soft-wrap table fresh: a resize or the M-\ toggle changed the
        // wrap width.
        self.ensure_wrap_prefix();
        // A provider's view can own its keys: a plugin that drew the buffer
        // is asked before any map, and answers with the path to open next.
        if self.on_key_to_plugin(&key) {
            return KeyOutcome::Handled;
        }
        // **The sidebar's keyboard**, when it holds it (M-J in, Esc out): the compose line and
        // the exchange's scrolling, ahead of the maps the way a prompt's keys are.
        if self.sidebar.focus
            && self.diff_view.is_none()
            && self.prompt.is_none()
            && self.sidebar_key(&key)
        {
            return KeyOutcome::Handled;
        }
        // Live completion popup: navigation/accept/cancel first; anything else
        // closes it and falls through (typing and backspace re-request instead).
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
        // A host's own chord reaches the host even over a prompt: the prompt stays
        // open. A chord the host bound to one of the editor's OWN commands runs that
        // command here, exactly as it does when no prompt is open — otherwise the
        // prompt would take the chord and type the letter it was pressed with.
        if self.prompt.is_some() && !self.host_keymaps.is_empty() {
            let host: Vec<&crate::keymap::Keymap> = self.host_keymaps.iter().rev().collect();
            if let crate::keymap::Lookup::Command(name) =
                crate::keymap::lookup(&host, &[Key::from_event(&key)])
            {
                if crate::commands::command(name).is_none() {
                    return KeyOutcome::Host(name.to_string());
                }
                let name = name.to_string();
                let held = self.prompt.take();
                self.run_command(&name);
                // A command that keeps the prompt (a flash, a search) gets it back; one that
                // closes it (C-g, a quit) has already decided.
                if self.prompt.is_none() && !self.wants_quit() && self.status_text().is_none() {
                    self.prompt = held;
                }
                return KeyOutcome::Handled;
            }
        }
        // A prompt edits its own line; everything else goes through the keymaps.
        if let Some(p) = self.prompt.take() {
            self.handle_prompt_key(p, key);
            return KeyOutcome::Handled;
        }
        self.dispatch_key(Key::from_event(&key))
    }
}

#[cfg(test)]
mod key_text_tests {
    use super::*;
    use crate::term::event::{KeyCode, Mods};

    fn k(code: KeyCode, mods: Mods) -> crate::term::event::KeyEvent {
        crate::term::event::KeyEvent { code, mods }
    }

    /// The grammar a plugin reads: bare characters, modifier prefixes, named keys.
    #[test]
    fn a_key_is_the_text_a_plugin_reads() {
        assert_eq!(key_text(&k(KeyCode::Char('n'), Mods::NONE)), "n");
        assert_eq!(key_text(&k(KeyCode::Char('g'), Mods::NONE)), "g");
        assert_eq!(key_text(&k(KeyCode::Char('x'), Mods::ALT)), "M-x");
        assert_eq!(key_text(&k(KeyCode::Char('a'), Mods::CTRL)), "C-a");
        assert_eq!(
            key_text(&k(KeyCode::Char('X'), Mods::CTRL | Mods::ALT)),
            "C-M-X"
        );
        assert_eq!(key_text(&k(KeyCode::Tab, Mods::SHIFT)), "S-Tab");
        assert_eq!(key_text(&k(KeyCode::Enter, Mods::NONE)), "Enter");
        assert_eq!(key_text(&k(KeyCode::Esc, Mods::NONE)), "Esc");
        assert_eq!(key_text(&k(KeyCode::PageDown, Mods::NONE)), "PageDown");
    }
}
