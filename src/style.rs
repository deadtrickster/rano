//! What a piece of text means, and the one place that meaning gets a look.
//!
//! A caller asks for [`Role::Failure`], not for red; a [`Palette`] maps roles to [`Look`]s, and each
//! medium spells a look its own way ([`Palette::open`] as SGR for painted strings, [`Palette::style`]
//! for renderers that build lines, the emitter for a cell buffer). The no-colour palette is not a
//! theme: it is the `--replay`, pipe-to-a-file and CI case, which must be byte-identical everywhere.
//!
//! Every look is a basic attribute or one of the terminal's sixteen slots. An absolute colour — the
//! xterm cube, an exact RGB — is the same under every theme, beside the theme rather than within
//! it, so it is spent only where a slot cannot do the job and only by name: the diff tints below,
//! and `Warning`'s orange on a terminal that said it draws exact colours. Roles that had to be told
//! apart differ by an attribute instead of a second shade, which is stronger: two shades are
//! indistinguishable under half the themes on this box.
//!
//! The diff tints ([`Role::Added`], [`Role::Removed`]) are the cube's darkest green and red as a
//! background only, so the text keeps its foreground and the sign carries the colour.

/// What a piece of text means. Callers name these; only [`Palette`] knows what they look like.
///
/// A role has a wire name (`kebab-case`: `plain`, `faint`, `user-accent`, …) because a pane plugin's
/// rows carry the roles they mean, and a second vocabulary of names on the host's side of the
/// boundary was the alternative.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Role {
    Plain,
    /// Structure a reader skips: frame lines, counts, ids, a diff's gutter.
    Faint,
    /// Anything that anchors a scan.
    Strong,
    /// A top-level heading in rendered markdown: bold alone would leave `## Findings` and a bold
    /// phrase inside a paragraph looking the same.
    Heading,
    /// A second-level heading: a different hue rather than a dimmer one, since two shades of one
    /// hue are indistinguishable under half the terminal themes.
    Subheading,
    /// The bar down the left of a user's own message.
    UserAccent,
    /// The user's own words, on a band of the theme's background. Under [`Palette::None`] the
    /// accent glyph is what survives.
    UserBlock,
    Success,
    /// Information: something happened and nothing is asked of the reader. Cyan, so it cannot be
    /// read as the yellow of work in progress or the red of a failure.
    Info,
    /// Work in progress: running, retrying, not finished. Yellow.
    Pending,
    /// Something went wrong and recovered, or is worth knowing before it does: orange where the
    /// terminal draws exact colours, and a `Pending` yellow an underline tells apart where it does
    /// not.
    Warning,
    /// Something that needs a person: a decision request, a prompt for the reader. The loudest role
    /// short of [`Role::Failure`], and never "is happening" or "went wrong on its own".
    Attention,
    Failure,
    /// A model's own reasoning, which must never read like its answer.
    Reasoning,
    Code,
    /// A diff line that was added. Background only.
    Added,
    /// A diff line that was removed. Background only.
    Removed,
    /// The changed run *inside* an added or removed line.
    Emphasis,
    Keyword,
    StringLit,
    NumberLit,
    Comment,
    TypeName,
    FuncName,
    /// Syntax: an operator or punctuation, drawn in the reader's own foreground. Named so a delimiter
    /// is told from the text it delimits.
    Punctuation,
    /// A link or a reference in prose (a markdown URL), where it is text rather than an OSC 8
    /// hyperlink.
    Link,
    /// A key as the reader would press it: a card's `C-x`, which-key's entries, `M-x`'s keys column.
    Key,
    /// The editor's chrome: the title, prompt and function bars and an overlay's header row, in
    /// reverse video, which is what nano emits.
    Bar,
    Selected,
    /// The search match the cursor is on: nano's black on yellow.
    Match,
    /// The marked region: two explicit theme colours rather than reverse, so the selection is
    /// visible on terminals whose reverse comes out white on white.
    Selection,
    /// A diagnostic of error severity: the gutter number, and (underlined) the code it is about.
    DiagError,
    DiagWarning,
    /// A diagnostic of any lesser severity (information, hint).
    DiagNote,
}

impl Role {
    /// Every role, for tests and for anything that lists them.
    pub const ALL: [Role; 34] = [
        Role::Plain,
        Role::Faint,
        Role::Strong,
        Role::Heading,
        Role::Subheading,
        Role::UserAccent,
        Role::UserBlock,
        Role::Success,
        Role::Info,
        Role::Pending,
        Role::Warning,
        Role::Attention,
        Role::Failure,
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
        Role::Punctuation,
        Role::Link,
        Role::Key,
        Role::Bar,
        Role::Selected,
        Role::Match,
        Role::Selection,
        Role::DiagError,
        Role::DiagWarning,
        Role::DiagNote,
    ];

    /// The role whose foreground this role stands for: a diff tint spends its look on a background,
    /// and the sign glyph and line number paint green and red through this.
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
    /// One of the theme's sixteen slots, `0`–`15`: 0–7 the normal eight, 8–15 the bright eight.
    Slot(u8),
    /// An absolute xterm-cube index. Only the diff tints use one.
    Cube(u8),
    /// An exact colour, written as truecolor: the default table spends one only for
    /// [`Role::Warning`], and a [`crate::theme::Theme`] may, to look the same whatever the
    /// terminal's palette.
    Rgb(u8, u8, u8),
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
            Hue::Rgb(r, g, b) => {
                let _ = write!(out, "{};2;{r};{g};{b}", if bg { 48 } else { 38 });
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

    /// In SGR order, with each one's on and off parameter. Bold and dim share an off (`22`), which
    /// the emitter has to know.
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

/// What a role looks like under a palette: medium-neutral, so a string painter, a cell emitter and
/// an export all read one table.
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

    /// `over` laid on top of `self`: its colours where it has them, attributes added. This is how a
    /// syntax colour over a diff row's tint keeps the tint.
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

    /// The SGR parameters (no `ESC [`, no `m`) that turn this on from a reset: attributes first,
    /// then foreground, then background.
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
        let mut s = String::new();
        self.write_sgr(&mut s);
        s
    }

    /// [`Look::sgr`] written onto the end of `out`, allocating nothing of its own — the form a string
    /// emitter uses per span, and a frame converts every row it shows.
    pub fn write_sgr(&self, out: &mut String) {
        if self.is_plain() {
            return;
        }
        out.push_str("\x1b[");
        let mut first = true;
        for (a, on, _) in Attrs::TABLE {
            if self.attrs.contains(a) {
                if !first {
                    out.push(';');
                }
                first = false;
                push_u8(out, on);
            }
        }
        for (h, bg) in [(self.fg, false), (self.bg, true)] {
            if let Some(h) = h {
                if !first {
                    out.push(';');
                }
                first = false;
                h.sgr(bg, out);
            }
        }
        out.push('m');
    }
}

/// A small number in decimal, onto `out`, with no allocation.
fn push_u8(out: &mut String, n: u8) {
    if n >= 100 {
        out.push((b'0' + n / 100) as char);
    }
    if n >= 10 {
        out.push((b'0' + (n / 10) % 10) as char);
    }
    out.push((b'0' + n % 10) as char);
}

/// A mapping from roles to looks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Palette {
    /// A colour terminal: the theme's sixteen slots and the attributes.
    Colour,
    /// The same, on a terminal that said its background is light (OSC 11): every role but three is a
    /// theme slot or an attribute and needs no change, and the diff's backgrounds swap to the cube's
    /// palest.
    Light,
    /// A colour terminal that said it draws exact colours (`COLORTERM=truecolor`): the sixteen
    /// slots, plus the one orange the severity ramp has no slot for ([`Role::Warning`]).
    Exact,
    /// [`Palette::Exact`] on a light background, where the orange is a darker one.
    ExactLight,
    /// No look at all: the output is plain text, which is what a replay diff and a CI log need.
    None,
}

impl Palette {
    /// The palette for a terminal: `light` from its OSC 11 answer, `exact` from whether it draws
    /// exact colours. The one place the two axes pick a variant.
    pub fn terminal(light: bool, exact: bool) -> Palette {
        match (light, exact) {
            (false, false) => Palette::Colour,
            (true, false) => Palette::Light,
            (false, true) => Palette::Exact,
            (true, true) => Palette::ExactLight,
        }
    }

    /// [`Palette::terminal`] with the exact-colour question put to `COLORTERM`, which is the one
    /// statement terminals share — a terminal that says nothing gets the sixteen slots.
    pub fn detected(light: bool) -> Palette {
        Palette::detected_with(light, |k| std::env::var(k).ok())
    }

    /// [`Palette::detected`] with the environment read through `get`, so a test can answer for it
    /// without touching the process's.
    pub fn detected_with(light: bool, get: impl Fn(&str) -> Option<String>) -> Palette {
        let said = get("COLORTERM").unwrap_or_default().to_ascii_lowercase();
        Palette::terminal(light, said.contains("truecolor") || said.contains("24bit"))
    }

    /// What `r` looks like: the active [`crate::theme::Theme`]'s look when it names the role, the
    /// default table otherwise — and [`Look::PLAIN`] for [`Palette::None`], whatever is installed.
    pub fn look(self, r: Role) -> Look {
        if self == Palette::None {
            return Look::PLAIN;
        }
        crate::theme::override_for(r).unwrap_or_else(|| self.base_look(r))
    }

    /// The one table: what `r` looks like with no theme. [`Look::PLAIN`] for [`Palette::None`] and
    /// for [`Role::Plain`], so patching either over something changes nothing.
    pub fn base_look(self, r: Role) -> Look {
        use Attrs as A;
        use Hue::Slot;
        if self == Palette::None {
            return Look::PLAIN;
        }
        let light = self.is_light();
        let exact = self.is_exact();
        match r {
            Role::Plain => Look::PLAIN,
            // Dim, not a grey: "bright black" sits within a hair of the background in several light
            // themes, and the attribute de-emphasises whatever the foreground already is.
            Role::Faint => Look::attrs(A::DIM),
            Role::Strong => Look::attrs(A::BOLD),
            // The two levels differ by hue — cyan and blue are both theme slots — and the level is
            // carried by the hashes too, for the monochrome reader.
            Role::Heading => Look::fg(Slot(6)).with(A::BOLD),
            Role::Subheading => Look::fg(Slot(4)).with(A::BOLD),
            Role::UserAccent => Look::fg(Slot(4)),
            // A band, not an inverse: reverse would paint the operator's rows as a bar of the
            // theme's foreground, the loudest thing on the screen. Slot 0 behind the text as it is,
            // or slot 7 on a light theme, whose slot 0 is black.
            Role::UserBlock => Look {
                bg: Some(Slot(if light { 7 } else { 0 })),
                ..Look::PLAIN
            },
            Role::Success => Look::fg(Slot(2)),
            // Cyan, the slot the reference theme gives information: neither the yellow of work in
            // progress nor the red of a failure, so an unknown code can be told from a real one.
            Role::Info => Look::fg(Slot(6)),
            Role::Pending => Look::fg(Slot(3)),
            // Orange only where the terminal said it draws exact colours: no theme slot is one, so
            // a palette without them keeps the yellow and tells it apart with an underline.
            Role::Warning if exact => Look::fg(if light {
                Hue::Rgb(0xb3, 0x5c, 0x00)
            } else {
                Hue::Rgb(0xff, 0x87, 0x00)
            }),
            Role::Warning => Look::fg(Slot(3)).with(A::UNDERLINE),
            // Bold and underlined against `Pending`'s plain yellow: weight alone was what let
            // "needs a person" and "is happening" share one look.
            Role::Attention => Look::fg(Slot(3)).with(A::BOLD.union(A::UNDERLINE)),
            Role::Failure => Look::fg(Slot(1)),
            // Dim italic: de-emphasis by colour is a no-op under a terminal-native palette, so the
            // attribute is what carries it.
            Role::Reasoning => Look::attrs(A::DIM.union(A::ITALIC)),
            Role::Code => Look::fg(Slot(6)),
            // Background only: the theme's own slots were bands rather than tints, and a tinted
            // foreground turned every plain run green-on-green. The cube's darkest green and red,
            // under whatever foreground the text already had.
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
            // The same attribute as `Faint`, and for the same reason: a comment is structure the
            // reader skips. Two roles may look alike; one role may not mean two things.
            Role::Comment => Look::attrs(A::DIM),
            Role::TypeName => Look::fg(Slot(6)),
            Role::FuncName => Look::fg(Slot(4)),
            Role::Punctuation => Look::PLAIN,
            Role::Link => Look::fg(Slot(4)),
            Role::Key => Look::fg(Slot(6)),
            // The editor's reverse video is the terminal's one reverse, legible on any theme.
            Role::Bar | Role::Selected => Look::attrs(A::REVERSE),
            Role::Match => Look {
                fg: Some(Slot(0)),
                bg: Some(Slot(3)),
                attrs: A::NONE,
            },
            // Bright white on bright black: the editor's selection was `White`-on-`DarkGray`, which
            // are the theme's slots 15 and 8.
            Role::Selection => Look {
                fg: Some(Slot(15)),
                bg: Some(Slot(8)),
                attrs: A::NONE,
            },
            Role::DiagError => Look::fg(Slot(1)),
            Role::DiagWarning => Look::fg(Slot(3)),
            Role::DiagNote => Look::fg(Slot(4)),
        }
    }

    /// Whether this palette draws colour at all: everything but [`Palette::None`].
    pub fn is_colour(self) -> bool {
        self != Palette::None
    }

    /// Whether this palette may spend an exact colour: the terminal said it draws truecolor.
    pub fn is_exact(self) -> bool {
        matches!(self, Palette::Exact | Palette::ExactLight)
    }

    /// Whether this palette is the one for a light background.
    pub fn is_light(self) -> bool {
        matches!(self, Palette::Light | Palette::ExactLight)
    }

    /// A role as a [`crate::render::Style`] under this palette: the role itself on a colour palette,
    /// and nothing at all under [`Palette::None`], which promises plain lines (the `+`/`-` glyph
    /// carries the change) whatever buffer or host they end up in.
    pub fn style(self, r: Role) -> crate::render::Style {
        if self == Palette::None {
            crate::render::Style::new()
        } else {
            crate::render::Style::of(r)
        }
    }

    /// A program's own background slot, `0`–`15`, as a look: plain for [`Palette::None`] and for a
    /// slot above the sixteen.
    ///
    /// Not a [`Role`], deliberately: a full-screen program's panel is its own drawing, and what
    /// passes through is the theme position it asked for. Not for a payload row — text on a block
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

    /// Reverse video as a look: plain for [`Palette::None`]. The program's reverse (`mc`'s selected
    /// row, a pane's highlighted entry) and the same look as [`Role::Selected`].
    pub fn reverse_look(self) -> Look {
        self.look(Role::Selected)
    }

    /// The opening SGR sequence for a role. Empty for [`Palette::None`] and [`Role::Plain`].
    pub fn open(self, r: Role) -> String {
        self.look(r).sgr()
    }

    /// Wrap `s` in the role, closed with a reset. A no-op for [`Palette::None`] and for
    /// [`Role::Plain`], so neither costs bytes.
    ///
    /// It deliberately does not sanitise `s`: this is called on the caller's own composed text,
    /// including strings it composed by calling this, and stripping here removes the caller's own
    /// colour. The guard belongs where foreign text enters — and a cell buffer has no such hazard.
    pub fn paint(self, r: Role, s: &str) -> String {
        let o = self.open(r);
        if o.is_empty() {
            return s.to_string();
        }
        format!("{o}{s}{}", crate::width::text::RESET)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_none_palette_styles_nothing() {
        for r in Role::ALL {
            assert_eq!(Palette::None.style(r), crate::render::Style::new(), "{r:?}");
            assert_eq!(Palette::None.look(r), Look::PLAIN, "{r:?}");
            assert_eq!(Palette::None.paint(r, "x"), "x");
        }
    }

    /// The port's parity check: the sequences spelled from the look are letibot's table, byte for
    /// byte, so a caller moving here sees no change — `Info` and `Warning` are the two it is about to
    /// read. The rows from `Punctuation` on are the editor's own roles, which letibot never had.
    #[test]
    fn the_spelled_sequences_are_letibots_table() {
        let table: [(Role, &str); 34] = [
            (Role::Plain, ""),
            (Role::Faint, "\x1b[2m"),
            (Role::Strong, "\x1b[1m"),
            (Role::Heading, "\x1b[1;36m"),
            (Role::Subheading, "\x1b[1;34m"),
            (Role::UserAccent, "\x1b[34m"),
            (Role::UserBlock, "\x1b[40m"),
            (Role::Success, "\x1b[32m"),
            (Role::Info, "\x1b[36m"),
            (Role::Pending, "\x1b[33m"),
            (Role::Warning, "\x1b[4;33m"),
            (Role::Attention, "\x1b[1;4;33m"),
            (Role::Failure, "\x1b[31m"),
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
            (Role::Punctuation, ""),
            (Role::Link, "\x1b[34m"),
            (Role::Key, "\x1b[36m"),
            (Role::Bar, "\x1b[7m"),
            (Role::Selected, "\x1b[7m"),
            (Role::Match, "\x1b[30;43m"),
            (Role::Selection, "\x1b[97;100m"),
            (Role::DiagError, "\x1b[31m"),
            (Role::DiagWarning, "\x1b[33m"),
            (Role::DiagNote, "\x1b[34m"),
        ];
        for (r, want) in table {
            assert_eq!(Palette::Colour.open(r), want, "{r:?}");
        }
    }

    #[test]
    fn every_role_closes_what_it_opens() {
        for r in Role::ALL {
            let s = Palette::Colour.paint(r, "abc");
            if !Palette::Colour.look(r).is_plain() {
                assert!(s.ends_with(crate::width::text::RESET), "{r:?} -> {s:?}");
            }
            assert_eq!(crate::width::text::width(&s), 3, "{r:?} changed the width");
        }
    }

    /// Colors that do not match the theme are settled by the sequences, since a screenshot on one
    /// theme looks fine either way.
    ///
    /// One exception, by name: the diff roles' backgrounds are cube colours, exactly two roles and
    /// exactly a background, so the loophole is a named pair rather than a door.
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
            // And the look's side of the same rule, whatever medium spells it.
            let l = Palette::Colour.look(r);
            for h in [l.fg, l.bg].into_iter().flatten() {
                assert!(matches!(h, Hue::Slot(n) if n < 16), "{r:?} paints {h:?}");
            }
        }
    }

    #[test]
    fn a_program_background_is_its_theme_slot_and_nothing_past_sixteen() {
        let bg = |p: Palette, n| p.background_look(n).bg;
        assert_eq!(bg(Palette::Colour, 4), Some(Hue::Slot(4)));
        assert_eq!(bg(Palette::Colour, 15), Some(Hue::Slot(15)));
        assert_eq!(Palette::Colour.background_look(16), Look::PLAIN);
        assert_eq!(Palette::Colour.background_look(255), Look::PLAIN);
        for slot in 0..=255u8 {
            assert_eq!(Palette::None.background_look(slot), Look::PLAIN, "{slot}");
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
    fn reverse_is_the_programs_and_the_user_block_is_a_band() {
        assert_eq!(Palette::Colour.reverse_look(), Look::attrs(Attrs::REVERSE));
        assert_eq!(Palette::Colour.reverse_look().sgr(), "\x1b[7m");
        // The user block is no longer the program's inverse: it is a band on slot 0.
        assert_eq!(Palette::Colour.look(Role::UserBlock).sgr(), "\x1b[40m");
        assert_eq!(Palette::None.reverse_look(), Look::PLAIN);
    }

    #[test]
    fn a_diff_role_hands_its_colour_to_the_sign() {
        assert_eq!(Role::Added.foreground(), Role::Success);
        assert_eq!(Role::Removed.foreground(), Role::Failure);
        assert_eq!(Role::Keyword.foreground(), Role::Keyword);
        assert_eq!(Palette::Colour.look(Role::Success).fg, Some(Hue::Slot(2)));
        assert_eq!(Palette::Colour.look(Role::Failure).fg, Some(Hue::Slot(1)));
    }

    /// The table every caller asks: each role resolves under each palette, and no palette has a hole
    /// where a role should be — a role it forgot would fall to `Plain` and draw as prose.
    #[test]
    fn every_role_has_a_look_in_every_palette() {
        const PALETTES: [Palette; 5] = [
            Palette::Colour,
            Palette::Light,
            Palette::Exact,
            Palette::ExactLight,
            Palette::None,
        ];
        for p in PALETTES {
            for r in Role::ALL {
                let l = p.look(r);
                if p == Palette::None {
                    assert_eq!(l, Look::PLAIN, "{r:?} under the no-colour palette");
                } else if !matches!(r, Role::Plain | Role::Punctuation) {
                    assert!(!l.is_plain(), "{p:?} has no look for {r:?}");
                }
            }
        }
    }

    /// Roles that mean different things have to look different, or moving to sixteen slots would
    /// have traded one defect for another. Every palette is asked, since the orange is what tells a
    /// warning from work in progress on the ones that can draw it.
    #[test]
    fn roles_a_reader_has_to_tell_apart_still_differ() {
        const PAIRS: &[(Role, Role)] = &[
            (Role::Heading, Role::Subheading),
            (Role::Info, Role::Pending),
            (Role::Info, Role::Failure),
            (Role::Pending, Role::Warning),
            (Role::Warning, Role::Attention),
            (Role::Attention, Role::Pending),
            (Role::Reasoning, Role::Plain),
            (Role::Success, Role::Failure),
            (Role::Added, Role::Removed),
            (Role::Keyword, Role::StringLit),
            (Role::Keyword, Role::NumberLit),
            (Role::TypeName, Role::FuncName),
            (Role::Match, Role::Selection),
            (Role::DiagError, Role::DiagWarning),
            (Role::DiagWarning, Role::DiagNote),
        ];
        for p in [
            Palette::Colour,
            Palette::Light,
            Palette::Exact,
            Palette::ExactLight,
        ] {
            for (a, b) in PAIRS {
                assert_ne!(p.look(*a), p.look(*b), "{p:?}: {a:?} and {b:?}");
            }
        }
    }

    /// `Warning` is the one role that spends an exact colour, and only where the terminal said it
    /// draws them; the sixteen-slot palettes keep a yellow an underline tells from the others.
    #[test]
    fn warning_is_orange_where_the_terminal_draws_exact_colours() {
        assert_eq!(Palette::Exact.open(Role::Warning), "\x1b[38;2;255;135;0m");
        assert_eq!(
            Palette::ExactLight.open(Role::Warning),
            "\x1b[38;2;179;92;0m"
        );
        for p in [Palette::Colour, Palette::Light] {
            assert_eq!(p.open(Role::Warning), "\x1b[4;33m", "{p:?}");
        }
        // An exact palette is a colour one with the orange in it, and nothing else moved.
        for r in Role::ALL {
            if r != Role::Warning {
                assert_eq!(Palette::Exact.look(r), Palette::Colour.look(r), "{r:?}");
            }
        }
    }

    /// The capability the orange hangs on is read from `COLORTERM`, through a door a test can answer
    /// for: a terminal that says it draws truecolor gets the exact palette.
    #[test]
    fn a_terminal_that_says_truecolor_gets_the_exact_palette() {
        let says = |v: &'static str| move |k: &str| (k == "COLORTERM").then(|| v.to_string());
        assert_eq!(
            Palette::detected_with(false, says("truecolor")),
            Palette::Exact
        );
        assert_eq!(
            Palette::detected_with(true, says("TrueColor")),
            Palette::ExactLight
        );
        assert_eq!(
            Palette::detected_with(true, says("24bit")),
            Palette::ExactLight
        );
        assert_eq!(Palette::detected_with(false, says("")), Palette::Colour);
        assert_eq!(Palette::detected_with(true, |_| None), Palette::Light);
        assert_eq!(Palette::terminal(false, false), Palette::Colour);
        assert_eq!(Palette::terminal(true, false), Palette::Light);
        assert_eq!(Palette::terminal(false, true), Palette::Exact);
        assert_eq!(Palette::terminal(true, true), Palette::ExactLight);
    }

    /// A light theme changes the two absolute colours and the user band, and nothing else. The band
    /// is slot 7 there, because a light theme's slot 0 is black.
    #[test]
    fn the_light_palette_differs_only_in_the_diff_backgrounds_and_the_band() {
        assert_eq!(Palette::Light.open(Role::UserBlock), "\x1b[47m");
        for r in Role::ALL {
            if matches!(r, Role::Added | Role::Removed | Role::UserBlock) {
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
