//! **The bridge to crossterm's events, for the transition and nothing else.**
//!
//! rano's editor still takes `crossterm::event::KeyEvent`s (its keymap's `Key::from_event`).
//! Until it takes [`super::event`] directly, a host that reads input through
//! [`super::decode`] can feed it through these. This is the only place in `term` that names
//! crossterm, and it is deleted with the editor's last crossterm call.
//!
//! The conversions spell keys the way crossterm itself reports them where the editor's keymap
//! depends on it: an upper-case character carries SHIFT, and every event is a press.

use crossterm::event as ct;

use super::event::{Event, KeyCode, KeyEvent, Mods, MouseButton, MouseEvent, MouseKind};

fn mods(m: Mods) -> ct::KeyModifiers {
    let mut out = ct::KeyModifiers::NONE;
    if m.shift() {
        out |= ct::KeyModifiers::SHIFT;
    }
    if m.alt() {
        out |= ct::KeyModifiers::ALT;
    }
    if m.ctrl() {
        out |= ct::KeyModifiers::CONTROL;
    }
    out
}

impl From<KeyEvent> for ct::KeyEvent {
    fn from(k: KeyEvent) -> ct::KeyEvent {
        let mut m = mods(k.mods);
        let code = match k.code {
            KeyCode::Char(c) => {
                if c.is_uppercase() {
                    m |= ct::KeyModifiers::SHIFT;
                }
                ct::KeyCode::Char(c)
            }
            KeyCode::Enter => ct::KeyCode::Enter,
            KeyCode::Tab => ct::KeyCode::Tab,
            KeyCode::BackTab => ct::KeyCode::BackTab,
            KeyCode::Backspace => ct::KeyCode::Backspace,
            KeyCode::Esc => ct::KeyCode::Esc,
            KeyCode::Up => ct::KeyCode::Up,
            KeyCode::Down => ct::KeyCode::Down,
            KeyCode::Left => ct::KeyCode::Left,
            KeyCode::Right => ct::KeyCode::Right,
            KeyCode::Home => ct::KeyCode::Home,
            KeyCode::End => ct::KeyCode::End,
            KeyCode::PageUp => ct::KeyCode::PageUp,
            KeyCode::PageDown => ct::KeyCode::PageDown,
            KeyCode::Insert => ct::KeyCode::Insert,
            KeyCode::Delete => ct::KeyCode::Delete,
            KeyCode::F(n) => ct::KeyCode::F(n),
        };
        ct::KeyEvent::new(code, m)
    }
}

fn button(b: MouseButton) -> ct::MouseButton {
    match b {
        MouseButton::Left => ct::MouseButton::Left,
        MouseButton::Middle => ct::MouseButton::Middle,
        MouseButton::Right => ct::MouseButton::Right,
    }
}

impl From<MouseEvent> for ct::MouseEvent {
    fn from(m: MouseEvent) -> ct::MouseEvent {
        let kind = match m.kind {
            MouseKind::Press(b) => ct::MouseEventKind::Down(button(b)),
            MouseKind::Release(b) => ct::MouseEventKind::Up(button(b)),
            MouseKind::Drag(b) => ct::MouseEventKind::Drag(button(b)),
            MouseKind::Move => ct::MouseEventKind::Moved,
            MouseKind::WheelUp => ct::MouseEventKind::ScrollUp,
            MouseKind::WheelDown => ct::MouseEventKind::ScrollDown,
            MouseKind::WheelLeft => ct::MouseEventKind::ScrollLeft,
            MouseKind::WheelRight => ct::MouseEventKind::ScrollRight,
        };
        ct::MouseEvent {
            kind,
            column: m.x,
            row: m.y,
            modifiers: mods(m.mods),
        }
    }
}

impl Event {
    /// This event as crossterm would have reported it, or `None` for the one it has no
    /// spelling for (the OSC 11 background reply).
    pub fn to_crossterm(&self) -> Option<ct::Event> {
        Some(match self {
            Event::Key(k) => ct::Event::Key((*k).into()),
            Event::Paste(s) => ct::Event::Paste(s.clone()),
            Event::Mouse(m) => ct::Event::Mouse((*m).into()),
            Event::FocusGained => ct::Event::FocusGained,
            Event::FocusLost => ct::Event::FocusLost,
            Event::Background { .. } => return None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keymap::Key;
    use crate::term::decode::decode;

    fn editor_key(bytes: &[u8]) -> Key {
        match &decode(bytes)[..] {
            [Event::Key(k)] => Key::from_event(&(*k).into()),
            other => panic!("{bytes:?} -> {other:?}"),
        }
    }

    /// **The editor's keymap reads decoded keys the way it read crossterm's.** The keys whose
    /// spelling differs between the two decoders are the ones checked: crossterm reports
    /// `0x1c` as Ctrl+4 and `0x1f` as Ctrl+7, this decoder as Ctrl+`\` and Ctrl+`_`, and the
    /// keymap must land both on the same key.
    #[test]
    fn the_editor_keymap_sees_the_keys_it_binds() {
        assert_eq!(editor_key(b"\x18"), Key::ctrl('x'));
        assert_eq!(editor_key(b"\x1bx"), Key::meta('x'));
        assert_eq!(editor_key(b"\x1c"), Key::ctrl('\\'));
        assert_eq!(editor_key(b"\x1f"), Key::ctrl('/'));
        assert_eq!(editor_key(b"\x00"), Key::ctrl(' '));
        assert_eq!(editor_key(b"A"), Key::plain(ct::KeyCode::Char('A')));
        let tab = editor_key(b"\x1b[Z");
        assert_eq!((tab.code, tab.shift), (ct::KeyCode::Tab, true));
        let up = editor_key(b"\x1b[1;2A");
        assert_eq!((up.code, up.shift, up.ctrl), (ct::KeyCode::Up, true, false));
        assert_eq!(editor_key(b"\x1b[99;5u"), Key::ctrl('c'));
    }

    #[test]
    fn every_event_but_the_background_reply_has_a_crossterm_spelling() {
        let click = decode(b"\x1b[<0;5;6M").remove(0);
        assert_eq!(
            click.to_crossterm(),
            Some(ct::Event::Mouse(ct::MouseEvent {
                kind: ct::MouseEventKind::Down(ct::MouseButton::Left),
                column: 4,
                row: 5,
                modifiers: ct::KeyModifiers::NONE,
            }))
        );
        assert_eq!(
            Event::Paste("x".into()).to_crossterm(),
            Some(ct::Event::Paste("x".into()))
        );
        assert_eq!(Event::Background { light: true }.to_crossterm(), None);
    }
}
