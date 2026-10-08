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

/// Shift, Alt and Ctrl, as the wire numbers them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Mods(u8);

impl Mods {
    pub const NONE: Mods = Mods(0);
    pub const SHIFT: Mods = Mods(1);
    pub const ALT: Mods = Mods(2);
    pub const CTRL: Mods = Mods(4);

    /// From the wire's bit field (the parameter minus one); bits past ctrl (super, hyper,
    /// meta, caps and num lock) are dropped, because no binding here reads them.
    pub const fn from_bits(b: u8) -> Mods {
        Mods(b & 7)
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
