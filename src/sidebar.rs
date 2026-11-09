//! The sidebar: a chat beside the text. The shape is letibot's rooms sidebar's
//! (`crates/tui/src/app/sidebar.rs` in the letibot tree): the main content keeps a minimum, the
//! sidebar is a third of what is left unless the operator dragged its border, and below the
//! minimum it is not drawn at all rather than squeezing the text into a column nothing fits in.

/// The columns the text keeps before the sidebar gives way.
pub(crate) const MAIN_MIN: usize = 48;
/// The narrowest sidebar worth drawing.
pub(crate) const SIDE_MIN: usize = 20;

/// One line of the exchange: who said it, and the line. A reply arrives whole and is split into
/// entries by the caller, so a screen row is always one entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatEntry {
    pub who: String,
    pub text: String,
}

/// The sidebar's own state. `follow` is the reader's place: true keeps the newest exchange line
/// on screen, and any scroll up turns it off until something new arrives.
#[derive(Debug, Default)]
pub struct Sidebar {
    pub on: bool,
    pub focus: bool,
    /// The width the operator dragged the border to, kept for the session.
    pub width: Option<usize>,
    /// The border is held: motion resizes instead of scrolling.
    pub dragging: bool,
    /// The exchange, oldest first.
    pub lines: Vec<ChatEntry>,
    /// The compose line at the foot, and its cursor in chars.
    pub input: String,
    pub cursor: usize,
    /// The first drawn entry; `follow` pins it to whatever keeps the end on screen.
    pub scroll: usize,
    pub follow: bool,
}

/// How the sidebar sits in a pane of `w` columns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fit {
    Off,
    Wide(usize),
    /// On, and no room for it: not drawn, and the text keeps the pane.
    Collapsed,
}

impl Sidebar {
    /// letibot's arithmetic: a third of the pane unless chosen, never so wide the text drops
    /// under its minimum, and nothing at all below the two minimums together.
    pub fn fit(&self, w: usize) -> Fit {
        if !self.on {
            return Fit::Off;
        }
        if w < MAIN_MIN + 1 + SIDE_MIN {
            return Fit::Collapsed;
        }
        let want = self.width.unwrap_or(w / 3);
        Fit::Wide(want.clamp(SIDE_MIN, w - MAIN_MIN - 1))
    }

    /// Something new arrived: pin the end to the screen.
    pub fn scroll_to_end(&mut self) {
        self.follow = true;
    }

    /// The compose line, said back as `(before, at, after)` around the cursor.
    pub fn input_parts(&self) -> (String, char, String) {
        let chars: Vec<char> = self.input.chars().collect();
        let at = chars.get(self.cursor).copied().unwrap_or(' ');
        (
            chars[..self.cursor.min(chars.len())].iter().collect(),
            at,
            chars[(self.cursor + 1).min(chars.len())..].iter().collect(),
        )
    }
}
