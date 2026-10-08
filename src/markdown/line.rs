//! What the painter produces: lines of role-tagged spans, and the two ways of
//! turning them back into bytes.
//!
//! # Why not a styled-text type from a TUI library
//!
//! letibot painted ANSI strings, and every defect class its `Painter` existed to
//! repair came from that: a span closed with a reset, the reset ended the colour
//! of the block it sat in, and a heading inside dim reasoning turned white for
//! the rest of the row. A concrete style baked into the output repeats the
//! mistake one level up — it decides the colours before the host has said what
//! its palette is.
//!
//! So a span here says what its text **means** — a [`Role`] from
//! [`crate::style`], plus the inline attributes markdown has (bold, italic,
//! struck) — and nothing about what that looks like. The meaning composes
//! instead of being restored: the register a whole reply is drawn in (letibot's
//! reasoning) is the line's [`MdLine::base`], not a sequence re-opened after
//! every span. A palette is applied once, at the edge, by [`to_ansi`] or by
//! whatever renderer a host draws with.

use std::fmt::Write as _;

use ratatui::style::{Color, Modifier, Style};

use crate::style::{Palette, Role};

/// Markdown's inline attributes, on top of a span's [`Role`].
///
/// Flags rather than more roles because they stack: `***both***` is bold and
/// italic, and a code span inside a heading is code that is still part of a
/// heading.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Attrs {
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    /// `~~struck~~`. Drawn **dim**, not crossed out, by [`to_ansi`]: crossed-out
    /// is an attribute half the terminals in use do not carry, and one a reader
    /// who has turned colour off would not see at all. Dim reads as "this was
    /// struck" next to the same sentence undimmed, and it is the weight the
    /// frames use.
    pub struck: bool,
}

impl Attrs {
    pub const NONE: Attrs = Attrs {
        bold: false,
        italic: false,
        underline: false,
        struck: false,
    };

    /// Both sets at once: an attribute is never taken away by nesting.
    pub fn union(self, o: Attrs) -> Attrs {
        Attrs {
            bold: self.bold || o.bold,
            italic: self.italic || o.italic,
            underline: self.underline || o.underline,
            struck: self.struck || o.struck,
        }
    }
}

/// A run of text with one meaning.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MdSpan {
    pub text: String,
    pub role: Role,
    pub attrs: Attrs,
}

impl MdSpan {
    pub fn new(text: impl Into<String>, role: Role) -> MdSpan {
        MdSpan {
            text: text.into(),
            role,
            attrs: Attrs::NONE,
        }
    }

    pub fn plain(text: impl Into<String>) -> MdSpan {
        MdSpan::new(text, Role::Plain)
    }

    pub fn with_attrs(mut self, attrs: Attrs) -> MdSpan {
        self.attrs = attrs;
        self
    }

    /// Same role and attributes: two spans that could be one.
    pub fn same_look(&self, o: &MdSpan) -> bool {
        self.role == o.role && self.attrs == o.attrs
    }
}

/// One terminal row.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MdLine {
    pub spans: Vec<MdSpan>,
    /// The register the whole row is drawn in, under every span's own role —
    /// `Some(Role::Reasoning)` for a model's working-out. `None` is the top
    /// level.
    pub base: Option<Role>,
}

impl MdLine {
    pub fn new(spans: Vec<MdSpan>) -> MdLine {
        MdLine { spans, base: None }
    }

    pub fn empty() -> MdLine {
        MdLine::default()
    }

    /// The row's text, styles dropped: what a reader with no colour sees, and
    /// what a test asserts on.
    pub fn text(&self) -> String {
        let mut s = String::new();
        for sp in &self.spans {
            s.push_str(&sp.text);
        }
        s
    }

    /// Columns the row occupies.
    pub fn width(&self) -> usize {
        spans_width(&self.spans)
    }

    pub fn is_empty(&self) -> bool {
        self.spans.iter().all(|s| s.text.is_empty())
    }

    /// Append a span, folding it into the last one when they look the same, so
    /// a row is not a confetti of one-character spans after a wrap.
    pub fn push(&mut self, s: MdSpan) {
        push_span(&mut self.spans, s);
    }
}

/// Append to a span list, coalescing equal looks and dropping empty text.
pub(crate) fn push_span(out: &mut Vec<MdSpan>, s: MdSpan) {
    if s.text.is_empty() {
        return;
    }
    if let Some(last) = out.last_mut()
        && last.same_look(&s)
    {
        last.text.push_str(&s.text);
        return;
    }
    out.push(s);
}

/// Columns a span list occupies, measured in clusters by [`crate::width`].
pub fn spans_width(spans: &[MdSpan]) -> usize {
    spans.iter().map(|s| text_width(&s.text)).sum()
}

/// Columns a string occupies. Wide characters are two, combining marks none.
pub fn text_width(s: &str) -> usize {
    let chars: Vec<char> = s.chars().collect();
    crate::width::width(&chars, 1)
}

/// Every row's text, joined with newlines.
pub fn to_plain(lines: &[MdLine]) -> String {
    lines
        .iter()
        .map(MdLine::text)
        .collect::<Vec<_>>()
        .join("\n")
}

/// Every row as ANSI-escaped text under `palette`, joined with newlines.
///
/// For a host that still prints strings rather than drawing cells, and for
/// tests that pin what a terminal is sent. Each span opens its own sequence and
/// closes with a reset, so every row is independently paintable — a full-screen
/// painter that erases to end of line erases in the *current* attributes, and a
/// row that left bold open paints the rest of the screen bold.
///
/// Under [`Palette::None`] there are no escapes at all: the output is
/// byte-identical to [`to_plain`], which is what a replay, a pipe and CI read.
pub fn to_ansi(lines: &[MdLine], palette: Palette) -> String {
    lines
        .iter()
        .map(|l| line_to_ansi(l, palette))
        .collect::<Vec<_>>()
        .join("\n")
}

/// One row as ANSI. See [`to_ansi`].
pub fn line_to_ansi(line: &MdLine, palette: Palette) -> String {
    let mut out = String::new();
    for sp in &line.spans {
        let seq = sgr(span_style(line.base, sp, palette));
        if seq.is_empty() {
            out.push_str(&sp.text);
        } else {
            let _ = write!(out, "{seq}{}{RESET}", sp.text);
        }
    }
    out
}

/// The reset every painted span closes with.
pub const RESET: &str = "\x1b[0m";

/// The style a span is drawn with: the row's register, then the span's role
/// patched over it, then its attributes. Composition, not restoration — a code
/// span inside reasoning is the code colour in reasoning's dim italic.
///
/// The attributes go through the palette's gate too: [`Palette::None`] draws
/// nothing at all, bold included, because its whole contract is plain text.
pub fn span_style(base: Option<Role>, sp: &MdSpan, palette: Palette) -> Style {
    if !palette.is_colour() {
        return Style::new();
    }
    let mut s = base.map(|b| palette.style(b)).unwrap_or_default();
    s = s.patch(palette.style(sp.role));
    let a = sp.attrs;
    let mut m = Modifier::empty();
    if a.bold {
        m |= Modifier::BOLD;
    }
    if a.italic {
        m |= Modifier::ITALIC;
    }
    if a.underline {
        m |= Modifier::UNDERLINED;
    }
    if a.struck {
        m |= Modifier::DIM;
    }
    s.add_modifier(m)
}

/// The SGR sequence for a style, empty when it draws nothing.
///
/// Named colours become the sixteen theme slots (`36`, `94`), not cube indices:
/// every role in [`crate::style`] is a slot so that the reader's theme decides
/// what it looks like, and `38;5;6` would be the same index spelled as an
/// absolute colour. The parameter order — attributes, then foreground, then
/// background — gives the sequences letibot's own palette emitted (`1;36` for a
/// heading), so a host switching over sees the same bytes.
fn sgr(st: Style) -> String {
    let mut p: Vec<String> = Vec::new();
    let m = st.add_modifier;
    for (bit, code) in [
        (Modifier::BOLD, "1"),
        (Modifier::DIM, "2"),
        (Modifier::ITALIC, "3"),
        (Modifier::UNDERLINED, "4"),
        (Modifier::REVERSED, "7"),
        (Modifier::CROSSED_OUT, "9"),
    ] {
        if m.contains(bit) {
            p.push(code.into());
        }
    }
    if let Some(c) = st.fg.and_then(|c| colour(c, false)) {
        p.push(c);
    }
    if let Some(c) = st.bg.and_then(|c| colour(c, true)) {
        p.push(c);
    }
    if p.is_empty() {
        String::new()
    } else {
        format!("\x1b[{}m", p.join(";"))
    }
}

fn colour(c: Color, bg: bool) -> Option<String> {
    let slot = match c {
        Color::Black => 0,
        Color::Red => 1,
        Color::Green => 2,
        Color::Yellow => 3,
        Color::Blue => 4,
        Color::Magenta => 5,
        Color::Cyan => 6,
        Color::Gray => 7,
        Color::DarkGray => 8,
        Color::LightRed => 9,
        Color::LightGreen => 10,
        Color::LightYellow => 11,
        Color::LightBlue => 12,
        Color::LightMagenta => 13,
        Color::LightCyan => 14,
        Color::White => 15,
        Color::Indexed(i) => return Some(format!("{};5;{i}", if bg { 48 } else { 38 })),
        Color::Rgb(r, g, b) => return Some(format!("{};2;{r};{g};{b}", if bg { 48 } else { 38 })),
        _ => return None,
    };
    let base = match (bg, slot < 8) {
        (false, true) => 30,
        (false, false) => 90 - 8,
        (true, true) => 40,
        (true, false) => 100 - 8,
    };
    Some((base + slot).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bold(t: &str) -> MdSpan {
        MdSpan::plain(t).with_attrs(Attrs {
            bold: true,
            ..Attrs::NONE
        })
    }

    #[test]
    fn plain_text_is_the_spans_text_and_nothing_else() {
        let l = MdLine::new(vec![
            MdSpan::new("## ", Role::Faint),
            MdSpan::new("Title", Role::Subheading),
        ]);
        assert_eq!(l.text(), "## Title");
        assert_eq!(l.width(), 8);
        assert_eq!(
            to_plain(&[l.clone(), MdLine::empty(), l]),
            "## Title\n\n## Title"
        );
    }

    #[test]
    fn the_none_palette_is_byte_identical_to_plain_text() {
        let lines = vec![
            MdLine::new(vec![bold("a"), MdSpan::new(" b", Role::Code)]),
            MdLine {
                spans: vec![MdSpan::new("c", Role::Heading)],
                base: Some(Role::Reasoning),
            },
        ];
        assert_eq!(to_ansi(&lines, Palette::None), to_plain(&lines));
    }

    /// The sequences a heading, a code span and bold come out as are the ones
    /// letibot's palette emitted, so a host switching over sees the same bytes.
    #[test]
    fn roles_emit_theme_slot_sequences() {
        let l = MdLine::new(vec![
            MdSpan::new("H", Role::Heading),
            MdSpan::new("c", Role::Code),
            bold("b"),
            MdSpan::new("f", Role::Faint),
            MdSpan::plain("p"),
        ]);
        assert_eq!(
            line_to_ansi(&l, Palette::Colour),
            "\x1b[1;36mH\x1b[0m\x1b[36mc\x1b[0m\x1b[1mb\x1b[0m\x1b[2mf\x1b[0mp"
        );
    }

    /// A span inside a register keeps the register: the defect letibot's
    /// `Painter` was written to repair cannot be expressed here.
    #[test]
    fn a_span_inside_reasoning_composes_with_it() {
        let l = MdLine {
            spans: vec![MdSpan::plain("x"), MdSpan::new("y", Role::Code)],
            base: Some(Role::Reasoning),
        };
        assert_eq!(
            line_to_ansi(&l, Palette::Colour),
            "\x1b[2;3mx\x1b[0m\x1b[2;3;36my\x1b[0m"
        );
    }

    #[test]
    fn struck_text_is_dim_not_crossed_out() {
        let l = MdLine::new(vec![MdSpan::plain("s").with_attrs(Attrs {
            struck: true,
            ..Attrs::NONE
        })]);
        assert_eq!(line_to_ansi(&l, Palette::Colour), "\x1b[2ms\x1b[0m");
    }

    #[test]
    fn bright_and_cube_colours_have_their_own_spelling() {
        assert_eq!(colour(Color::LightBlue, false).unwrap(), "94");
        assert_eq!(colour(Color::Blue, true).unwrap(), "44");
        assert_eq!(colour(Color::White, true).unwrap(), "107");
        assert_eq!(colour(Color::Indexed(22), true).unwrap(), "48;5;22");
    }

    #[test]
    fn push_coalesces_equal_looks_and_drops_empty_text() {
        let mut l = MdLine::empty();
        l.push(MdSpan::plain("a"));
        l.push(MdSpan::plain(""));
        l.push(MdSpan::plain("b"));
        l.push(MdSpan::new("c", Role::Code));
        assert_eq!(l.spans.len(), 2);
        assert_eq!(l.spans[0].text, "ab");
    }
}
