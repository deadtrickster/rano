//! What the terminal said: a key and its mods, a paste, a mouse report, a focus change, the
//! background answer. An event says what was pressed, not what an app binds it to.

/// Shift, Alt, Ctrl and Super, as the wire numbers them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Mods(u8);

impl Mods {
    pub const NONE: Mods = Mods(0);
    pub const SHIFT: Mods = Mods(1);
    pub const ALT: Mods = Mods(2);
    pub const CTRL: Mods = Mods(4);
    /// ⌘, only on the kitty keyboard protocol; a key carrying it never goes back to legacy bytes.
    pub const SUPER: Mods = Mods(8);

    /// From the wire's bit field (the parameter minus one); hyper, meta and locks dropped.
    pub const fn from_bits(b: u8) -> Mods {
        Mods(b & 15)
    }
    pub const fn bits(self) -> u8 {
        self.0
    }
    pub const fn contains(self, o: Mods) -> bool {
        self.0 & o.0 == o.0
    }
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }
    pub const fn shift(self) -> bool {
        self.contains(Mods::SHIFT)
    }
    pub const fn alt(self) -> bool {
        self.contains(Mods::ALT)
    }
    pub const fn ctrl(self) -> bool {
        self.contains(Mods::CTRL)
    }
    /// ⌘ (or whatever the terminal calls super). `super` is a keyword, hence the name.
    pub const fn sup(self) -> bool {
        self.contains(Mods::SUPER)
    }
}

#[cfg(test)]
mod mods_tests {
    use super::*;

    #[test]
    fn super_is_a_modifier_and_not_an_empty_one() {
        let m = Mods::from_bits(8);
        assert_eq!(m, Mods::SUPER);
        assert_eq!(m.bits(), 8);
        assert!(!m.is_empty(), "⌘ alone is not \"no modifier held\"");
        assert!(m.sup());
        assert!(!m.ctrl() && !m.alt() && !m.shift());
        assert!(m.contains(Mods::SUPER));
        assert!(!m.contains(Mods::CTRL));
        assert!((Mods::SUPER | Mods::CTRL).ctrl() && (Mods::SUPER | Mods::CTRL).sup());
        assert_eq!(Mods::from_bits(0), Mods::NONE);
        assert!(Mods::NONE.is_empty());
        for b in [16u8, 32, 64, 128] {
            assert_eq!(Mods::from_bits(b), Mods::NONE, "{b}");
        }
        assert_eq!(Mods::from_bits(5), Mods::SHIFT | Mods::CTRL);
    }
}

impl std::ops::BitOr for Mods {
    type Output = Mods;
    fn bitor(self, o: Mods) -> Mods {
        Mods(self.0 | o.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KeyCode {
    /// A character; shifted letters arrive upper case, control letters with [`Mods::CTRL`].
    Char(char),
    Enter,
    Tab,
    BackTab,
    Backspace,
    Esc,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    Insert,
    Delete,
    /// F1–F12.
    F(u8),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct KeyEvent {
    pub code: KeyCode,
    pub mods: Mods,
}

impl KeyEvent {
    pub const fn new(code: KeyCode, mods: Mods) -> KeyEvent {
        KeyEvent { code, mods }
    }
    pub const fn plain(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, Mods::NONE)
    }
    pub const fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), Mods::CTRL)
    }
    pub const fn alt(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, Mods::ALT)
    }
    pub const fn char(c: char) -> KeyEvent {
        KeyEvent::plain(KeyCode::Char(c))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MouseButton {
    Left,
    Middle,
    Right,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MouseKind {
    Press(MouseButton),
    Release(MouseButton),
    /// Motion with a button held (`?1002`).
    Drag(MouseButton),
    /// Motion with no button held (`?1003` only).
    Move,
    WheelUp,
    WheelDown,
    WheelLeft,
    WheelRight,
}

/// A mouse report, on 0-based cell coordinates (the wire's are 1-based).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MouseEvent {
    pub kind: MouseKind,
    pub x: u16,
    pub y: u16,
    pub mods: Mods,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Event {
    Key(KeyEvent),
    /// A bracketed paste, whole, markers stripped: its newlines are content, not Enter.
    Paste(String),
    Mouse(MouseEvent),
    FocusGained,
    FocusLost,
    /// The OSC 11 answer; a light background chooses `Palette::Light`.
    Background {
        light: bool,
    },
}

impl From<KeyEvent> for Event {
    fn from(k: KeyEvent) -> Event {
        Event::Key(k)
    }
}
