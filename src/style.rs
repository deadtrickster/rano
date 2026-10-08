//! What a piece of text *means*, and the one place that meaning gets a look.
//!
//! # Provenance
//!
//! Two copies of this file existed: letibot's `crates/ui/src/style.rs`, which maps a
//! role to an SGR string, and an earlier port of it here that mapped a role to a
//! ratatui `Style`. They had already drifted — letibot had grown [`Palette::Light`]
//! and this copy had not. Now there is one table, [`Palette::look`], in a
//! representation that belongs to neither medium ([`Look`]: two colours and a set
//! of attributes), and each medium reads it:
//!
//! - [`Palette::open`] / [`Palette::paint`] — letibot's SGR strings, spelled from
//!   the look, for text painted as a string.
//! - `crate::render`'s emitter — a cell buffer's rows, with minimal SGR transitions
//!   between cells.
//! - [`Palette::style`] — the ratatui `Style` the editor and `diff`/`sidediff` still
//!   draw with. Transitional: it goes when the editor moves off ratatui.
//!
//! # Name the roles, not the colours
//!
//! A caller asks for [`Role::Failure`], not for red. A [`Palette`] maps roles to
//! looks, and the no-colour one is not a theme: it is the `--replay`, pipe-to-a-file
//! and CI case that must produce byte-identical output on every machine, and carry the
//! meaning in the text alone (a diff's `+`/`-` glyph, a heading's hashes).
//!
//! # The ANSI 16, not the 256-colour cube
//!
//! Every look below is a basic attribute or one of the sixteen named colours. That is
//! not a downgrade for its own sake, it is the only way a role can mean anything to the
//! person looking at it.
//!
//! **Colours 16–255 of the xterm cube are absolute RGB.** A terminal theme defines
//! slots 0–15 and nothing else, so a role painted `38;5;167` is the same salmon on
//! Solarized Light, Gruvbox and Nord — it paints *beside* the theme rather than within
//! it. letibot's table used to be entirely cube indices and the operator's report was
//! the predictable one: "colors not matching theme". Now `Role::Failure` is `31`, which
//! is whatever the reader calls red.
//!
//! The roles that looked like they needed more precision — the two heading levels,
//! `Attention` against `Pending`, `Reasoning` against `Faint` — are separated by an
//! **attribute** instead of by a second shade, which is stronger: two shades of one hue
//! are indistinguishable under half the themes on this box, and bold-vs-plain is not.
//! Truecolour is not an option either: it is not universally forwarded through `tmux`,
//! `screen` or `ssh` with an old `TERM`.
//!
//! **The exceptions are the diff tints** ([`Role::Added`] / [`Role::Removed`]), which
//! are the cube's darkest green and red as a **background only** — the theme's own
//! slots were bands rather than tints, and a tinted foreground turned every plain run
//! green-on-green. The text keeps its own foreground; the `+`/`-` sign carries the green
//! and red through [`Role::foreground`]. Under [`Palette::Light`] the two swap to the
//! cube's palest, which is the only difference between the two colour palettes.
//!
//! # There is no light-mode and no dark-mode for anything else
//!
//! [`Role::UserBlock`] is reverse video, the reader's own pair swapped, which is
//! legible under any theme by construction. letibot's earlier table named a dark grey
//! behind a near-white for it, which on a light terminal is a black bar across the
//! transcript.

/// What a piece of text means. Callers name these; only [`Palette`] knows what
/// they look like.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Role {
    /// Ordinary body text. No style at all.
    Plain,
    /// Structure a reader skips: frame lines, counts, ids, a diff's gutter.
    Faint,
    /// Anything that anchors a scan.
    Strong,
    /// A top-level heading in rendered markdown.
    ///
    /// letibot painted every heading bold-and-nothing-else, which is why a long answer
    /// read as one slab: `## Findings` and `### Why` are constant in a model's output and
    /// bold alone does not separate them from the emphasis inside a paragraph.
    Heading,
    /// A second-level heading: a different hue rather than a dimmer one, since two
    /// shades of one hue are indistinguishable under half the terminal themes.
    Subheading,
    /// The bar down the left of a user's own message.
    UserAccent,
    /// The user's own words, on a raised block: reverse video. Nothing at all under
    /// [`Palette::None`], where the accent glyph is what survives.
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
    /// Every role, for tests and for anything that lists them.
    pub const ALL: [Role; 22] = [
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

    /// The role whose **foreground** this role stands for.
    ///
    /// The operator, on the first cube tint: *"please keep original foregrounds"* — a
    /// cell opened with `32;48;5;22` and every plain run in it inherited the green, so
    /// the diff read as green text on green. The diff roles now spend their look on a
    /// background only and the text keeps its own foreground; what still wants green
    /// and red is the sign glyph and the line number, and they paint through this.
    pub fn foreground(self) -> Role {
        match self {
            Role::Added => Role::Success,
            Role::Removed => Role::Failure,
            other => other,
        }
    }
}

/// A terminal colour, as this crate is willing to name one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Hue {
    /// One of the theme's sixteen slots, `0`–`15`: 0–7 the normal eight, 8–15 the
    /// bright eight. What the reader's theme says it is.
    Slot(u8),
    /// An absolute xterm-cube index. Only the diff tints use one; see the module
    /// header, and the test that holds them to it.
    Cube(u8),
}

impl Hue {
    /// SGR parameters for this as a foreground (`bg == false`) or background.
    pub(crate) fn sgr(self, bg: bool, out: &mut String) {
        use std::fmt::Write;
        match self {
            Hue::Slot(n) if n < 8 => {
                let _ = write!(out, "{}", if bg { 40 } else { 30 } + n as u32);
            }
            Hue::Slot(n) if n < 16 => {
                let _ = write!(out, "{}", if bg { 100 } else { 90 } + (n - 8) as u32);
            }
            Hue::Slot(n) | Hue::Cube(n) => {
                let _ = write!(out, "{};5;{n}", if bg { 48 } else { 38 });
            }
        }
    }
}

/// Text attributes, as a set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Attrs(u8);

impl Attrs {
    pub const NONE: Attrs = Attrs(0);
    pub const BOLD: Attrs = Attrs(1);
    pub const DIM: Attrs = Attrs(2);
    pub const ITALIC: Attrs = Attrs(4);
    pub const UNDERLINE: Attrs = Attrs(8);
    pub const REVERSE: Attrs = Attrs(16);

    /// In SGR order, with each one's on and off parameter. Bold and dim share an off
    /// (`22`), which the emitter has to know.
    pub(crate) const TABLE: [(Attrs, u8, u8); 5] = [
        (Attrs::BOLD, 1, 22),
        (Attrs::DIM, 2, 22),
        (Attrs::ITALIC, 3, 23),
        (Attrs::UNDERLINE, 4, 24),
        (Attrs::REVERSE, 7, 27),
    ];

    pub const fn union(self, o: Attrs) -> Attrs {
        Attrs(self.0 | o.0)
    }
    pub const fn contains(self, o: Attrs) -> bool {
        self.0 & o.0 == o.0
    }
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }
    pub const fn without(self, o: Attrs) -> Attrs {
        Attrs(self.0 & !o.0)
    }
}

impl std::ops::BitOr for Attrs {
    type Output = Attrs;
    fn bitor(self, o: Attrs) -> Attrs {
        self.union(o)
    }
}

/// What a role looks like under a palette: medium-neutral, so a string painter, a
/// cell emitter and the ratatui adapter all read the same table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Look {
    pub fg: Option<Hue>,
    pub bg: Option<Hue>,
    pub attrs: Attrs,
}

impl Look {
    pub const PLAIN: Look = Look {
        fg: None,
        bg: None,
        attrs: Attrs::NONE,
    };

    const fn fg(h: Hue) -> Look {
        Look {
            fg: Some(h),
            bg: None,
            attrs: Attrs::NONE,
        }
    }
    const fn attrs(a: Attrs) -> Look {
        Look {
            fg: None,
            bg: None,
            attrs: a,
        }
    }
    const fn with(self, a: Attrs) -> Look {
        Look {
            fg: self.fg,
            bg: self.bg,
            attrs: self.attrs.union(a),
        }
    }

    /// `over` laid on top of `self`: its colours where it has them, attributes added.
    /// This is how a span inside a block keeps the block — a syntax colour over a diff
    /// row's tint keeps the tint — which is what letibot's `Painter` existed to repair
    /// when the medium was strings closed with a reset.
    pub fn patch(self, over: Look) -> Look {
        Look {
            fg: over.fg.or(self.fg),
            bg: over.bg.or(self.bg),
            attrs: self.attrs.union(over.attrs),
        }
    }

    pub fn is_plain(&self) -> bool {
        *self == Look::PLAIN
    }

    /// The SGR parameters (no `ESC [`, no `m`) that turn this on from a reset:
    /// attributes first, then foreground, then background — letibot's spelling.
    pub fn sgr_params(&self) -> String {
        let mut s = String::new();
        for (a, on, _) in Attrs::TABLE {
            if self.attrs.contains(a) {
                if !s.is_empty() {
                    s.push(';');
                }
                s.push_str(&on.to_string());
            }
        }
        for (h, bg) in [(self.fg, false), (self.bg, true)] {
            if let Some(h) = h {
                if !s.is_empty() {
                    s.push(';');
                }
                h.sgr(bg, &mut s);
            }
        }
        s
    }

    /// The whole opening sequence, or `""` for a plain look.
    pub fn sgr(&self) -> String {
        if self.is_plain() {
            return String::new();
        }
        format!("\x1b[{}m", self.sgr_params())
    }
}

/// A mapping from roles to looks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Palette {
    /// A colour terminal.
    Colour,
    /// **The same, on a terminal that said its background is light** (OSC 11). Every
    /// role but two is a theme slot or an attribute and needs no change; the two that
    /// name an absolute colour — the diff's backgrounds — swap the cube's darkest green
    /// and red, which are dark bands under dark text on a light theme, for its palest.
    Light,
    /// No look at all: the output is plain text, which is what a replay diff and a CI
    /// log need.
    None,
}

impl Palette {
    /// The one table: what `r` looks like. [`Look::PLAIN`] for [`Palette::None`] and
    /// for [`Role::Plain`], so patching either over something changes nothing.
    pub fn look(self, r: Role) -> Look {
        use Attrs as A;
        use Hue::Slot;
        if self == Palette::None {
            return Look::PLAIN;
        }
        let light = self == Palette::Light;
        match r {
            Role::Plain => Look::PLAIN,
            // Dim, not a grey: "bright black" sits within a hair of the background in
            // several light themes; the attribute de-emphasises whatever the foreground
            // already is.
            Role::Faint => Look::attrs(A::DIM),
            Role::Strong => Look::attrs(A::BOLD),
            // The two levels differ by **hue** — cyan and blue are both theme slots —
            // and the level is carried by the hashes too, for the monochrome reader.
            Role::Heading => Look::fg(Slot(6)).with(A::BOLD),
            Role::Subheading => Look::fg(Slot(4)).with(A::BOLD),
            Role::UserAccent => Look::fg(Slot(4)),
            Role::UserBlock => Look::attrs(A::REVERSE),
            Role::Success => Look::fg(Slot(2)),
            Role::Pending => Look::fg(Slot(3)),
            Role::Failure => Look::fg(Slot(1)),
            // Bold yellow against `Pending`'s plain yellow: "needs a person" and "is
            // happening" are close enough that a *weight* is the right distinction; a
            // second orange was never one, since the cube's orange is no theme's slot.
            Role::Attention => Look::fg(Slot(3)).with(A::BOLD),
            // Dim italic: de-emphasis by colour is a no-op under a terminal-native
            // palette, so the attribute is what actually carries it.
            Role::Reasoning => Look::attrs(A::DIM.union(A::ITALIC)),
            Role::Code => Look::fg(Slot(6)),
            // Background only — see the module header. The trial ran three times: the
            // theme's slots (`42`/`41`) were bands, *"too much color, the diff is
            // unreadable"*; the cube tint beside the role's own foreground turned every
            // plain run green-on-green; what remains is the cube's darkest green and
            // red alone, under whatever foreground the text already had.
            Role::Added => Look {
                bg: Some(Hue::Cube(if light { 194 } else { 22 })),
                ..Look::PLAIN
            },
            Role::Removed => Look {
                bg: Some(Hue::Cube(if light { 224 } else { 52 })),
                ..Look::PLAIN
            },
            Role::Emphasis => Look::attrs(A::BOLD.union(A::UNDERLINE)),
            Role::Keyword => Look::fg(Slot(5)),
            Role::StringLit => Look::fg(Slot(2)),
            Role::NumberLit => Look::fg(Slot(3)),
            // The same attribute as `Faint`, for the same reason: a comment is
            // structure the reader skips. Two roles may look alike; one role may not
            // mean two things.
            Role::Comment => Look::attrs(A::DIM),
            Role::TypeName => Look::fg(Slot(6)),
            Role::FuncName => Look::fg(Slot(4)),
        }
    }

    pub fn is_colour(self) -> bool {
        matches!(self, Palette::Colour | Palette::Light)
    }

    /// **A program's own background slot**, `0`–`15`, as a look: plain for
    /// [`Palette::None`] and for a slot above the sixteen.
    ///
    /// Not a [`Role`], deliberately. A role is a meaning the caller has; a full-screen
    /// program's panel (`mc`'s blue, `nano`'s status bar) is the program's own drawing,
    /// and the job is to put it on the glass rather than re-mean it. So what passes
    /// through is the **theme position** the program asked for, and the reader's theme
    /// supplies the colour. Not for a payload row: text that already sits on a block
    /// the caller chose should not carry a foreign program's background beside it.
    pub fn background_look(self, slot: u8) -> Look {
        if self == Palette::None || slot > 15 {
            return Look::PLAIN;
        }
        Look {
            bg: Some(Hue::Slot(slot)),
            ..Look::PLAIN
        }
    }

    /// **Reverse video** as a look: plain for [`Palette::None`].
    ///
    /// The program's reverse (`mc`'s selected row) and [`Role::UserBlock`] are one
    /// look, because a terminal has one way to say reverse and the two must not drift
    /// into disagreeing about what it looks like.
    pub fn reverse_look(self) -> Look {
        self.look(Role::UserBlock)
    }

    /// The opening SGR sequence for a role — letibot's `Palette::open`. Empty for
    /// [`Palette::None`] and [`Role::Plain`].
    pub fn open(self, r: Role) -> String {
        self.look(r).sgr()
    }

    /// Wrap `s` in the role, closed with a reset. A no-op for [`Palette::None`] and for
    /// [`Role::Plain`], so neither costs bytes.
    ///
    /// **It deliberately does NOT sanitise `s`** (letibot §3.1, and the regression
    /// `81990b3` shipped there): this is called on the caller's own composed text,
    /// including strings it composed *by calling this*, and sanitising here stripped the
    /// caller's own colour and left the body of the escape behind. The guard belongs
    /// where **foreign** text enters. A cell buffer has no such hazard: `render` drops
    /// escapes from span content, since a style there is an attribute and not bytes.
    pub fn paint(self, r: Role, s: &str) -> String {
        let o = self.open(r);
        if o.is_empty() {
            return s.to_string();
        }
        format!("{o}{s}{}", crate::width::text::RESET)
    }
}

// ---------------------------------------------------------------------------
// The ratatui adapter. Transitional: the editor and `diff`/`sidediff` still draw
// with ratatui, and these are the functions they (and letibot's pinned rano) call.
// It reads the same table as everything above, so it cannot drift from it; it
// goes when the last ratatui caller does.
// ---------------------------------------------------------------------------

use ratatui::style::{Color, Modifier, Style};

impl Hue {
    fn ratatui(self) -> Color {
        match self {
            Hue::Slot(n) => match n {
                0 => Color::Black,
                1 => Color::Red,
                2 => Color::Green,
                3 => Color::Yellow,
                4 => Color::Blue,
                5 => Color::Magenta,
                6 => Color::Cyan,
                7 => Color::Gray,
                8 => Color::DarkGray,
                9 => Color::LightRed,
                10 => Color::LightGreen,
                11 => Color::LightYellow,
                12 => Color::LightBlue,
                13 => Color::LightMagenta,
                14 => Color::LightCyan,
                15 => Color::White,
                n => Color::Indexed(n),
            },
            Hue::Cube(n) => Color::Indexed(n),
        }
    }
}

impl Look {
    /// This look as a ratatui [`Style`].
    pub fn ratatui(&self) -> Style {
        let mut s = Style::new();
        if let Some(f) = self.fg {
            s = s.fg(f.ratatui());
        }
        if let Some(b) = self.bg {
            s = s.bg(b.ratatui());
        }
        for (a, m) in [
            (Attrs::BOLD, Modifier::BOLD),
            (Attrs::DIM, Modifier::DIM),
            (Attrs::ITALIC, Modifier::ITALIC),
            (Attrs::UNDERLINE, Modifier::UNDERLINED),
            (Attrs::REVERSE, Modifier::REVERSED),
        ] {
            if self.attrs.contains(a) {
                s = s.add_modifier(m);
            }
        }
        s
    }
}

impl Palette {
    /// The ratatui style for a role: [`Palette::look`], adapted.
    pub fn style(self, r: Role) -> Style {
        self.look(r).ratatui()
    }

    /// [`Palette::background_look`] as a ratatui style. Slots go out as
    /// `Color::Indexed`, which is the slot number on the wire.
    pub fn background(self, slot: u8) -> Style {
        match self.background_look(slot).bg {
            Some(Hue::Slot(n)) => Style::new().bg(Color::Indexed(n)),
            _ => Style::new(),
        }
    }

    /// [`Palette::reverse_look`] as a ratatui style.
    pub fn reverse(self) -> Style {
        self.reverse_look().ratatui()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_none_palette_styles_nothing() {
        for r in Role::ALL {
            assert_eq!(Palette::None.style(r), Style::new(), "{r:?}");
            assert_eq!(Palette::None.look(r), Look::PLAIN, "{r:?}");
            assert_eq!(Palette::None.paint(r, "x"), "x");
        }
    }

    /// **The port's parity check**: the sequences spelled from the look are letibot's
    /// table, byte for byte, so a letibot caller moving to this file sees no change.
    #[test]
    fn the_spelled_sequences_are_letibots_table() {
        let table: [(Role, &str); 22] = [
            (Role::Plain, ""),
            (Role::Faint, "\x1b[2m"),
            (Role::Strong, "\x1b[1m"),
            (Role::Heading, "\x1b[1;36m"),
            (Role::Subheading, "\x1b[1;34m"),
            (Role::UserAccent, "\x1b[34m"),
            (Role::UserBlock, "\x1b[7m"),
            (Role::Success, "\x1b[32m"),
            (Role::Pending, "\x1b[33m"),
            (Role::Failure, "\x1b[31m"),
            (Role::Attention, "\x1b[1;33m"),
            (Role::Reasoning, "\x1b[2;3m"),
            (Role::Code, "\x1b[36m"),
            (Role::Added, "\x1b[48;5;22m"),
            (Role::Removed, "\x1b[48;5;52m"),
            (Role::Emphasis, "\x1b[1;4m"),
            (Role::Keyword, "\x1b[35m"),
            (Role::StringLit, "\x1b[32m"),
            (Role::NumberLit, "\x1b[33m"),
            (Role::Comment, "\x1b[2m"),
            (Role::TypeName, "\x1b[36m"),
            (Role::FuncName, "\x1b[34m"),
        ];
        for (r, want) in table {
            assert_eq!(Palette::Colour.open(r), want, "{r:?}");
        }
    }

    #[test]
    fn every_role_closes_what_it_opens() {
        for r in Role::ALL {
            let s = Palette::Colour.paint(r, "abc");
            if r != Role::Plain {
                assert!(s.ends_with(crate::width::text::RESET), "{r:?} -> {s:?}");
            }
            assert_eq!(crate::width::text::width(&s), 3, "{r:?} changed the width");
        }
    }

    /// The operator's report: *"colors not matching theme."* Settled by the sequences,
    /// since a screenshot on one theme looks fine either way.
    ///
    /// **One exception, by name.** The diff roles' backgrounds are cube colours, exactly
    /// two roles and exactly a background: no foreground may leave the sixteen, and no
    /// third role may follow, so the loophole is a named pair rather than a door.
    #[test]
    fn no_role_paints_outside_the_sixteen_colours_a_theme_defines() {
        const CUBE_EXCEPTIONS: &[(Role, &str)] = &[
            (Role::Added, "\x1b[48;5;22m"),
            (Role::Removed, "\x1b[48;5;52m"),
        ];
        for r in Role::ALL {
            let o = Palette::Colour.open(r);
            if let Some((_, allowed)) = CUBE_EXCEPTIONS.iter().find(|(e, _)| e == &r) {
                assert_eq!(o, *allowed, "{r:?} is the named cube exception: {o:?}");
                continue;
            }
            for p in o
                .trim_start_matches("\x1b[")
                .trim_end_matches('m')
                .split(';')
                .filter(|p| !p.is_empty())
            {
                let n: u32 = p.parse().expect("every parameter is numeric");
                assert!(
                    n <= 29
                        || (30..=37).contains(&n)
                        || (40..=47).contains(&n)
                        || (90..=97).contains(&n)
                        || (100..=107).contains(&n),
                    "{r:?} uses SGR parameter {n}, which is not a theme slot: {o:?}"
                );
            }
            // And the ratatui side of the same rule.
            let s = Palette::Colour.style(r);
            for c in [s.fg, s.bg].into_iter().flatten() {
                assert!(!matches!(c, Color::Indexed(_)), "{r:?} paints {c:?}");
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
        // Spelled the way a terminal spells them: the eight, then the bright eight at 100.
        for (slot, n) in (0u8..16).zip((40..48).chain(100..108)) {
            assert_eq!(
                Palette::Colour.background_look(slot).sgr(),
                format!("\x1b[{n}m"),
                "slot {slot}"
            );
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
        assert_eq!(Palette::Colour.reverse_look().sgr(), "\x1b[7m");
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

    /// Roles that mean different things have to *look* different, or moving to sixteen
    /// slots would have traded one defect for another.
    #[test]
    fn roles_a_reader_has_to_tell_apart_still_differ() {
        for (a, b) in [
            (Role::Heading, Role::Subheading),
            (Role::Attention, Role::Pending),
            (Role::Reasoning, Role::Plain),
            (Role::Success, Role::Failure),
            (Role::Added, Role::Removed),
            (Role::Keyword, Role::StringLit),
            (Role::Keyword, Role::NumberLit),
            (Role::TypeName, Role::FuncName),
        ] {
            assert_ne!(
                Palette::Colour.look(a),
                Palette::Colour.look(b),
                "{a:?} {b:?}"
            );
        }
    }

    /// **A light theme changes the two absolute colours and nothing else.**
    #[test]
    fn the_light_palette_differs_only_in_the_diff_backgrounds() {
        for r in Role::ALL {
            if matches!(r, Role::Added | Role::Removed) {
                continue;
            }
            assert_eq!(Palette::Light.look(r), Palette::Colour.look(r), "{r:?}");
        }
        assert_eq!(Palette::Light.open(Role::Added), "\x1b[48;5;194m");
        assert_eq!(Palette::Light.open(Role::Removed), "\x1b[48;5;224m");
        assert!(Palette::Light.is_colour());
        assert!(!Palette::None.is_colour());
    }

    #[test]
    fn a_span_look_over_a_block_keeps_the_block() {
        let row = Palette::Colour.look(Role::Added);
        let kw = Palette::Colour.look(Role::Keyword);
        let both = row.patch(kw);
        assert_eq!(both.bg, row.bg, "the tint survives the syntax colour");
        assert_eq!(both.fg, kw.fg);
        assert_eq!(both.sgr(), "\x1b[35;48;5;22m");
    }
}
