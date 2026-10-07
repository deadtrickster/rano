//! What a piece of text *means*, and the one place that meaning gets a look.
//!
//! # Provenance
//!
//! Ported from letibot's `crates/ui/src/style.rs`, where the reasoning below was
//! worked out against the operator's screen; the change is the medium. letibot
//! painted ANSI strings, so a role opened an SGR sequence and a span closed with a
//! reset — and a reset inside a tinted cell ended the tint, which is what its
//! `Painter` existed to repair. Here a role is a ratatui [`Style`], and styles
//! *compose*: a syntax colour patched over a diff row's background keeps the
//! background without anyone having to restore it. That whole class of defect has
//! no representation, so `Painter` did not come along.
//!
//! # Name the roles, not the colours
//!
//! A caller asks for [`Role::Failure`], not for red. A [`Palette`] maps roles to
//! styles, and there are two of them — colour and none — because the second is not
//! a theme, it is the pipe-to-a-file and test case that must carry the meaning in
//! the text alone (a diff's `+`/`-` glyph, a heading's hashes).
//!
//! # The sixteen theme slots, and two exceptions
//!
//! Every colour below is one of the sixteen named colours or an attribute, because a
//! terminal theme defines slots 0–15 and nothing else: a colour from the 256-colour
//! cube is the same absolute RGB under every theme and paints *beside* it rather
//! than within it. The exceptions are the diff tints ([`Role::Added`] /
//! [`Role::Removed`]), which are the cube's darkest green and red as a **background
//! only** — the theme's own slots were bands rather than tints, and a tinted
//! foreground turned every plain run green-on-green. The text keeps its own
//! foreground; the `+`/`-` sign carries the green and red through
//! [`Role::foreground`].
//!
//! There is no light mode and no dark mode here: [`Role::UserBlock`] is reverse
//! video, the reader's own pair swapped, which is legible under any theme.

use ratatui::style::{Color, Modifier, Style};

/// What a piece of text means. Callers name these; only [`Palette`] knows what
/// they look like.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// Ordinary body text. No style at all.
    Plain,
    /// Structure a reader skips: frame lines, counts, ids, a diff's gutter.
    Faint,
    /// Anything that anchors a scan.
    Strong,
    /// A top-level heading in rendered markdown.
    Heading,
    /// A second-level heading: a different hue rather than a dimmer one, since two
    /// shades of one hue are indistinguishable under half the terminal themes.
    Subheading,
    /// The bar down the left of a user's own message.
    UserAccent,
    /// The user's own words, on a raised block: reverse video.
    UserBlock,
    /// Something completed successfully.
    Success,
    /// Something in flight.
    Pending,
    /// Something that failed, was refused, or was interrupted.
    Failure,
    /// Something that needs a person: a decision request, a warning.
    Attention,
    /// A model's own reasoning, which must never read like its answer.
    Reasoning,
    /// A code span or a code block's body.
    Code,
    /// A diff line that was added. Background only.
    Added,
    /// A diff line that was removed. Background only.
    Removed,
    /// The changed run *inside* an added or removed line.
    Emphasis,
    /// Syntax: a language keyword.
    Keyword,
    /// Syntax: a string literal.
    StringLit,
    /// Syntax: a numeric literal.
    NumberLit,
    /// Syntax: a comment.
    Comment,
    /// Syntax: a type or a constructor.
    TypeName,
    /// Syntax: a function name at its definition or call site.
    FuncName,
}

impl Role {
    /// The role whose **foreground** this role stands for. The diff roles spend
    /// their style on a background so the text keeps its own colours; what still
    /// wants green and red is the sign glyph and the line number, and they paint
    /// through this.
    pub fn foreground(self) -> Role {
        match self {
            Role::Added => Role::Success,
            Role::Removed => Role::Failure,
            other => other,
        }
    }
}

/// A mapping from roles to styles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Palette {
    /// A colour terminal.
    Colour,
    /// No style at all: the meaning has to survive in the text.
    None,
}

impl Palette {
    /// The style for a role. [`Style::default`] for [`Palette::None`] and for
    /// [`Role::Plain`], so patching either over something changes nothing.
    pub fn style(self, r: Role) -> Style {
        if self == Palette::None {
            return Style::new();
        }
        let s = Style::new();
        match r {
            Role::Plain => s,
            // Dim, not a grey: "bright black" sits within a hair of the
            // background in several light themes; the attribute de-emphasises
            // whatever the foreground already is.
            Role::Faint => s.add_modifier(Modifier::DIM),
            Role::Strong => s.add_modifier(Modifier::BOLD),
            Role::Heading => s.fg(Color::Cyan).add_modifier(Modifier::BOLD),
            Role::Subheading => s.fg(Color::Blue).add_modifier(Modifier::BOLD),
            Role::UserAccent => s.fg(Color::Blue),
            Role::UserBlock => self.reverse(),
            Role::Success => s.fg(Color::Green),
            Role::Pending => s.fg(Color::Yellow),
            Role::Failure => s.fg(Color::Red),
            // Bold yellow against `Pending`'s plain yellow: a weight, not a
            // second orange no theme defines.
            Role::Attention => s.fg(Color::Yellow).add_modifier(Modifier::BOLD),
            Role::Reasoning => s.add_modifier(Modifier::DIM | Modifier::ITALIC),
            Role::Code => s.fg(Color::Cyan),
            Role::Added => s.bg(Color::Indexed(22)),
            Role::Removed => s.bg(Color::Indexed(52)),
            Role::Emphasis => s.add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
            Role::Keyword => s.fg(Color::Magenta),
            Role::StringLit => s.fg(Color::Green),
            Role::NumberLit => s.fg(Color::Yellow),
            // The same attribute as `Faint`, for the same reason: a comment is
            // structure the reader skips.
            Role::Comment => s.add_modifier(Modifier::DIM),
            Role::TypeName => s.fg(Color::Cyan),
            Role::FuncName => s.fg(Color::Blue),
        }
    }

    pub fn is_colour(self) -> bool {
        self == Palette::Colour
    }

    /// **A program's own background slot**, `0`–`15`: [`Style::default`] for
    /// [`Palette::None`] and for a slot above the sixteen.
    ///
    /// Not a [`Role`], deliberately. A role is a meaning the caller has; a
    /// full-screen program's panel (`mc`'s blue, `nano`'s status bar) is the
    /// program's own drawing, and the job is to put it on the glass rather than
    /// re-mean it. So what passes through is the **theme position** the program
    /// asked for, and the reader's theme supplies the colour — the same rule as
    /// every role above. Slots 0–7 are the normal colours, 8–15 the bright ones.
    ///
    /// Not for a payload row: text that already sits on a block the caller chose
    /// should not carry a foreign program's background beside it.
    pub fn background(self, slot: u8) -> Style {
        if self == Palette::None || slot > 15 {
            return Style::new();
        }
        Style::new().bg(Color::Indexed(slot))
    }

    /// **Reverse video**: the terminal's own pair of colours, swapped —
    /// [`Style::default`] for [`Palette::None`].
    ///
    /// [`Palette::background`]'s counterpart for a program that asked for
    /// reverse (`mc`'s selected row, `less`'s status bar), and the same style as
    /// [`Role::UserBlock`]: a terminal has one way to say reverse, so the two
    /// cannot drift into disagreeing about what it looks like.
    pub fn reverse(self) -> Style {
        if self == Palette::None {
            return Style::new();
        }
        Style::new().add_modifier(Modifier::REVERSED)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [Role; 22] = [
        Role::Plain,
        Role::Faint,
        Role::Strong,
        Role::Heading,
        Role::Subheading,
        Role::UserAccent,
        Role::UserBlock,
        Role::Success,
        Role::Pending,
        Role::Failure,
        Role::Attention,
        Role::Reasoning,
        Role::Code,
        Role::Added,
        Role::Removed,
        Role::Emphasis,
        Role::Keyword,
        Role::StringLit,
        Role::NumberLit,
        Role::Comment,
        Role::TypeName,
        Role::FuncName,
    ];

    #[test]
    fn the_none_palette_styles_nothing() {
        for r in ALL {
            assert_eq!(Palette::None.style(r), Style::new(), "{r:?}");
        }
    }

    /// Only the two diff tints leave the sixteen theme slots, and they leave as a
    /// background only, so the text over them keeps its own foreground.
    #[test]
    fn no_role_paints_outside_the_theme_slots_but_the_diff_tints() {
        for r in ALL {
            let s = Palette::Colour.style(r);
            for c in [s.fg, s.bg].into_iter().flatten() {
                if let Color::Indexed(i) = c {
                    assert!(
                        matches!(r, Role::Added | Role::Removed) && s.fg.is_none(),
                        "{r:?} paints cube colour {i}"
                    );
                }
            }
        }
    }

    #[test]
    fn a_program_background_is_its_theme_slot_and_nothing_past_sixteen() {
        assert_eq!(
            Palette::Colour.background(4),
            Style::new().bg(Color::Indexed(4))
        );
        assert_eq!(
            Palette::Colour.background(15),
            Style::new().bg(Color::Indexed(15))
        );
        assert_eq!(Palette::Colour.background(16), Style::new());
        assert_eq!(Palette::Colour.background(255), Style::new());
        for slot in 0..=255u8 {
            assert_eq!(Palette::None.background(slot), Style::new(), "{slot}");
        }
    }

    #[test]
    fn reverse_is_one_style_for_the_program_and_the_user_block() {
        assert_eq!(
            Palette::Colour.reverse(),
            Style::new().add_modifier(Modifier::REVERSED)
        );
        assert_eq!(
            Palette::Colour.reverse(),
            Palette::Colour.style(Role::UserBlock)
        );
        assert_eq!(Palette::None.reverse(), Style::new());
    }

    #[test]
    fn a_diff_role_hands_its_colour_to_the_sign() {
        assert_eq!(Role::Added.foreground(), Role::Success);
        assert_eq!(Role::Removed.foreground(), Role::Failure);
        assert_eq!(Role::Keyword.foreground(), Role::Keyword);
        assert_eq!(Palette::Colour.style(Role::Success).fg, Some(Color::Green));
        assert_eq!(Palette::Colour.style(Role::Failure).fg, Some(Color::Red));
    }
}
