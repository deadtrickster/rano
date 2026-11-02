//! **What the terminal said**, in rano's own vocabulary: a key and its modifiers, a paste, a
//! mouse report, a focus change, or the answer to "is your background light?".
//!
//! # Why not letibot's `Key`
//!
//! letibot's decoder emitted its app's vocabulary directly — `0x0b` was `KillToEnd`, `ESC b`
//! was `WordLeft`, Ctrl+X was "show the raw tool call". That is right for one app and wrong for
//! a library: the editor binds `C-k` to something else, and a second app would have to undo the
//! first one's meanings. So this says what was *pressed* — `k` with Ctrl — and each app keeps
//! its own map from that to what it means. letibot's map is a `match` over these.
//!
//! The modifier bits are the wire's (xterm and the kitty keyboard both send `1 + bits`), so a
//! decoder never has to translate them.

/// Shift, Alt, Ctrl and Super, as the wire numbers them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Mods(u8);

impl Mods {
    pub const NONE: Mods = Mods(0);
    pub const SHIFT: Mods = Mods(1);
    pub const ALT: Mods = Mods(2);
    pub const CTRL: Mods = Mods(4);
    /// ⌘ on the kitty keyboard's bit 8. No terminal without the protocol has a byte for it,
    /// which is why a key carrying one is reported as itself and never put back into legacy
    /// bytes — see `decode::kitty_key`. Dropped here, ⌘C arrived as a typed `c`.
    pub const SUPER: Mods = Mods(8);

    /// From the wire's bit field (the parameter minus one). Shift, alt, ctrl and super are
    /// kept; hyper, meta and the two lock bits are dropped, because there is no bit here to
    /// put them in — and a key carrying one is decoded as itself rather than through the
    /// legacy bytes, so a decoder never has to name it.
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

    /// **A `Mods(8)` is a modifier, not none** — every helper here was written when the only
    /// bits that existed were shift, alt and ctrl, and `is_empty` is what an app asks before
    /// it types a letter.
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
        // Hyper, meta and the lock bits have no bit of their own.
        for b in [16u8, 32, 64, 128] {
            assert_eq!(Mods::from_bits(b), Mods::NONE, "{b}");
        }
        // And the three that always worked still do.
        assert_eq!(Mods::from_bits(5), Mods::SHIFT | Mods::CTRL);
    }
}

impl std::ops::BitOr for Mods {
    type Output = Mods;
    fn bitor(self, o: Mods) -> Mods {
        Mods(self.0 | o.0)
    }
}

/// Which key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KeyCode {
    /// A character. A shifted letter is its upper case (the terminal already applied shift);
    /// a control letter is its lower case with [`Mods::CTRL`].
    Char(char),
    Enter,
    Tab,
    /// Shift+Tab, which terminals send as its own sequence (`CSI Z`).
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

/// A key and the modifiers held with it.
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

/// One thing the terminal said.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Event {
    Key(KeyEvent),
    /// A bracketed paste, whole, markers stripped: its newlines are content, not Enter.
    Paste(String),
    Mouse(MouseEvent),
    FocusGained,
    FocusLost,
    /// The answer to the OSC 11 question asked at enter: whether the background is light,
    /// which is what chooses `Palette::Light`.
    Background {
        light: bool,
    },
}

impl From<KeyEvent> for Event {
    fn from(k: KeyEvent) -> Event {
        Event::Key(k)
    }
}
