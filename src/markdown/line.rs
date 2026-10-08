//! What the painter produces: [`crate::render`] lines whose spans say what their
//! text means, and the two ways of turning them back into bytes.
//!
//! # Why roles, not colours
//!
//! letibot painted ANSI strings, and every defect class its `Painter` existed to
//! repair came from that: a span closed with a reset, the reset ended the colour
//! of the block it sat in, and a heading inside dim reasoning turned white for
//! the rest of the row. A concrete style baked into the output repeats the
//! mistake one level up — it decides the colours before the host has said what
//! its palette is.
//!
//! So a span here says what its text **means** — a [`Role`] in its
//! [`Style`], plus the inline attributes markdown has (bold, italic, struck) —
//! and nothing about what that looks like. The meaning composes instead of being
//! restored: the register a whole reply is drawn in (letibot's reasoning) is the
//! line's own style, under every span, not a sequence re-opened after every
//! span. A palette is applied once, at the edge: by [`to_ansi`], or by a
//! [`crate::render::Buffer`] the host draws the lines into and emits.
//!
//! The markdown-only `MdLine`/`MdSpan` this module used to define are gone: they
//! were [`Line`] and [`Span`] with the style spelled differently, and two
//! vocabularies for one thing is one too many for a host drawing both.

use crate::render::{Line, Span, Style};
use crate::style::{Attrs, Palette, Role};

/// A span of `text` meaning `role`.
pub fn span(text: impl Into<String>, role: Role) -> Span {
    Span::role(text, role)
}

/// `~~struck~~` text's attribute. Drawn **dim**, not crossed out: crossed-out is
/// an attribute half the terminals in use do not carry, and one a reader who has
/// turned colour off would not see at all. Dim reads as "this was struck" next
/// to the same sentence undimmed, and it is the weight the frames use.
pub const STRUCK: Attrs = Attrs::DIM;

/// The row's register, as the role at the bottom of its style: `Some` for
/// letibot's reasoning, `None` at the top level.
pub fn base(l: &Line) -> Option<Role> {
    l.style.roles().next()
}

/// A row in `base`'s register.
pub(crate) fn based_line(spans: Vec<Span>, base: Option<Role>) -> Line {
    Line {
        spans,
        style: base.map(Style::of).unwrap_or_default(),
    }
}

/// Append to a span list, coalescing equal styles and dropping empty text, so a
/// row is not a confetti of one-character spans after a wrap.
pub(crate) fn push_span(out: &mut Vec<Span>, s: Span) {
    if s.content.is_empty() {
        return;
    }
    if let Some(last) = out.last_mut()
        && last.style == s.style
    {
        last.content.push_str(&s.content);
        return;
    }
    out.push(s);
}

/// Columns a span list occupies, measured in clusters by [`crate::width`].
pub fn spans_width(spans: &[Span]) -> usize {
    spans.iter().map(|s| text_width(&s.content)).sum()
}

/// Columns a string occupies. Wide characters are two, combining marks none.
pub fn text_width(s: &str) -> usize {
    let chars: Vec<char> = s.chars().collect();
    crate::width::width(&chars, 1)
}

/// Every row's text, joined with newlines.
pub fn to_plain(lines: &[Line]) -> String {
    lines.iter().map(Line::plain).collect::<Vec<_>>().join("\n")
}

/// Every row as ANSI-escaped text under `palette`, joined with newlines: each
/// row is [`Line::to_ansi`].
///
/// For a host that still prints strings rather than drawing cells, and for
/// tests that pin what a terminal is sent. Under [`Palette::None`] there are no
/// escapes at all: the output is byte-identical to [`to_plain`], which is what a
/// replay, a pipe and CI read.
pub fn to_ansi(lines: &[Line], palette: Palette) -> String {
    lines
        .iter()
        .map(|l| l.to_ansi(palette))
        .collect::<Vec<_>>()
        .join("\n")
}

/// One row as ANSI. See [`to_ansi`].
pub fn line_to_ansi(line: &Line, palette: Palette) -> String {
    line.to_ansi(palette)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bold(t: &str) -> Span {
        Span::styled(t, Style::new().bold())
    }

    #[test]
    fn plain_text_is_the_spans_text_and_nothing_else() {
        let l = Line::new(vec![
            span("## ", Role::Faint),
            span("Title", Role::Subheading),
        ]);
        assert_eq!(l.plain(), "## Title");
        assert_eq!(spans_width(&l.spans), 8);
        assert_eq!(
            to_plain(&[l.clone(), Line::default(), l]),
            "## Title\n\n## Title"
        );
    }

    #[test]
    fn the_none_palette_is_byte_identical_to_plain_text() {
        let lines = vec![
            Line::new(vec![bold("a"), span(" b", Role::Code)]),
            based_line(vec![span("c", Role::Heading)], Some(Role::Reasoning)),
        ];
        assert_eq!(to_ansi(&lines, Palette::None), to_plain(&lines));
    }

    /// The sequences a heading, a code span and bold come out as are the ones
    /// letibot's palette emitted, so a host switching over sees the same bytes.
    #[test]
    fn roles_emit_theme_slot_sequences() {
        let l = Line::new(vec![
            span("H", Role::Heading),
            span("c", Role::Code),
            bold("b"),
            span("f", Role::Faint),
            Span::raw("p"),
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
        let l = based_line(
            vec![Span::raw("x"), span("y", Role::Code)],
            Some(Role::Reasoning),
        );
        assert_eq!(base(&l), Some(Role::Reasoning));
        assert_eq!(
            line_to_ansi(&l, Palette::Colour),
            "\x1b[2;3mx\x1b[0m\x1b[2;3;36my\x1b[0m"
        );
    }

    #[test]
    fn struck_text_is_dim_not_crossed_out() {
        let mut st = Style::new();
        st.attrs = STRUCK;
        let l = Line::new(vec![Span::styled("s", st)]);
        assert_eq!(line_to_ansi(&l, Palette::Colour), "\x1b[2ms\x1b[0m");
    }

    #[test]
    fn push_coalesces_equal_looks_and_drops_empty_text() {
        let mut l = Vec::new();
        push_span(&mut l, Span::raw("a"));
        push_span(&mut l, Span::raw(""));
        push_span(&mut l, Span::raw("b"));
        push_span(&mut l, span("c", Role::Code));
        assert_eq!(l.len(), 2);
        assert_eq!(l[0].content, "ab");
    }
}
