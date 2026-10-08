//! **A buffer as terminal rows**: one `String` per row, each independently paintable,
//! under a [`Palette`].
//!
//! # The rules, and what each one is for
//!
//! - **Minimal transitions.** Between two cells only what changed is said — `22` to drop
//!   bold, `39` to drop a colour — or a reset and the whole new pen when that is shorter.
//!   A frame that rewrites a row of syntax-coloured code is mostly these bytes, and the
//!   row-diffing painter only saves what the emitter does not waste.
//! - **Never an escape inside a cluster.** Transitions are written between cells and a
//!   cell is a whole cluster, so a combining mark, a ZWJ sequence or a kitty graphics
//!   placeholder's diacritics are never separated from their base.
//! - **Every row ends closed.** An open link is closed (`OSC 8 ;; ST`) and any open
//!   attribute reset at the end of the row: the painter erases each row with `ESC[K`
//!   *in the current attributes*, and letibot's string `wrap` and `truncate` carry a
//!   reset and a link close for the same reason — here it is one place instead of every
//!   function that cuts a string.
//! - **Trailing blanks are not written.** A plain space with no link at the end of a row
//!   is what the painter's erase leaves anyway; writing it is bytes for nothing.
//! - **`Palette::None` is plain text**: no SGR, no link, no raw colour — byte-identical
//!   on every machine, which is what a replay diff and a CI log compare.

use super::buffer::{Buffer, Cell, Rect};
use super::style::Style;
use super::widget::Widget;
use crate::style::{Attrs, Hue, Palette};

/// The close of an OSC 8 hyperlink.
const LINK_CLOSE: &str = "\x1b]8;;\x1b\\";

/// A colour as the emitter writes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Ink {
    Hue(Hue),
    Rgb([u8; 3]),
}

impl Ink {
    fn push(self, bg: bool, out: &mut String) {
        match self {
            Ink::Hue(h) => h.sgr(bg, out),
            Ink::Rgb([r, g, b]) => {
                out.push_str(&format!("{};2;{r};{g};{b}", if bg { 48 } else { 38 }))
            }
        }
    }
}

/// A style resolved under a palette: exactly what the terminal is asked for.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
struct Pen {
    attrs: Attrs,
    fg: Option<Ink>,
    bg: Option<Ink>,
    underline: Option<u8>,
}

impl Pen {
    fn of(s: &Style, p: Palette) -> Pen {
        if p == Palette::None {
            return Pen::default();
        }
        let l = s.look(p);
        Pen {
            attrs: l.attrs,
            fg: match s.raw.fg_rgb {
                Some(rgb) => Some(Ink::Rgb(rgb)),
                None => l.fg.map(Ink::Hue),
            },
            bg: l.bg.map(Ink::Hue),
            underline: s.raw.underline,
        }
    }

    /// Every parameter that turns this pen on from a reset.
    fn full(&self, out: &mut Vec<String>) {
        for (a, on, _) in Attrs::TABLE {
            if self.attrs.contains(a) {
                out.push(on.to_string());
            }
        }
        if let Some(f) = self.fg {
            let mut s = String::new();
            f.push(false, &mut s);
            out.push(s);
        }
        if let Some(b) = self.bg {
            let mut s = String::new();
            b.push(true, &mut s);
            out.push(s);
        }
        if let Some(u) = self.underline {
            out.push(format!("58;5;{u}"));
        }
    }
}

/// The SGR sequence that turns `from` into `to`, or `""` when they are the same.
fn transition(from: Pen, to: Pen) -> String {
    if from == to {
        return String::new();
    }
    if to == Pen::default() {
        return "\x1b[0m".into();
    }
    // The incremental spelling.
    let mut inc: Vec<String> = Vec::new();
    let mut have = from.attrs;
    // Bold and dim share their off (`22`): dropping either drops both, and whichever the
    // new pen keeps is turned back on below.
    let lost = |a| have.contains(a) && !to.attrs.contains(a);
    if lost(Attrs::BOLD) || lost(Attrs::DIM) {
        inc.push("22".into());
        have = have.without(Attrs::BOLD).without(Attrs::DIM);
    }
    for (a, on, off) in Attrs::TABLE {
        if off == 22 {
            if to.attrs.contains(a) && !have.contains(a) {
                inc.push(on.to_string());
            }
            continue;
        }
        match (have.contains(a), to.attrs.contains(a)) {
            (true, false) => inc.push(off.to_string()),
            (false, true) => inc.push(on.to_string()),
            _ => {}
        }
    }
    if from.fg != to.fg {
        match to.fg {
            Some(f) => {
                let mut s = String::new();
                f.push(false, &mut s);
                inc.push(s);
            }
            None => inc.push("39".into()),
        }
    }
    if from.bg != to.bg {
        match to.bg {
            Some(b) => {
                let mut s = String::new();
                b.push(true, &mut s);
                inc.push(s);
            }
            None => inc.push("49".into()),
        }
    }
    if from.underline != to.underline {
        match to.underline {
            Some(u) => inc.push(format!("58;5;{u}")),
            None => inc.push("59".into()),
        }
    }
    // The reset spelling, and whichever is shorter.
    let mut fresh = vec!["0".to_string()];
    to.full(&mut fresh);
    let inc = inc.join(";");
    let fresh = fresh.join(";");
    let params = if fresh.len() < inc.len() { fresh } else { inc };
    format!("\x1b[{params}m")
}

fn is_blank(c: &Cell, pen: Pen) -> bool {
    c.symbol == " " && pen == Pen::default() && c.style.link.is_none()
}

impl Buffer {
    /// Every row of the buffer as a terminal string, under `palette`. See the module
    /// header for the rules each row keeps.
    pub fn emit(&self, palette: Palette) -> Vec<String> {
        let a = self.area();
        (a.y..a.bottom())
            .map(|y| emit_row(self.row(y), palette))
            .collect()
    }
}

/// One row of cells as a terminal string.
pub fn emit_row(cells: &[Cell], palette: Palette) -> String {
    let pens: Vec<Pen> = cells.iter().map(|c| Pen::of(&c.style, palette)).collect();
    let linked = |c: &Cell| {
        if palette == Palette::None {
            None
        } else {
            c.style.link.clone()
        }
    };
    let end = cells
        .iter()
        .zip(&pens)
        .rposition(|(c, p)| !c.is_continuation() && !is_blank(c, *p))
        .map_or(0, |i| i + 1);
    let mut out = String::new();
    let mut pen = Pen::default();
    let mut link: Option<std::sync::Arc<str>> = None;
    for (c, p) in cells[..end].iter().zip(&pens) {
        if c.is_continuation() {
            continue;
        }
        let want = linked(c);
        if want != link {
            if link.is_some() {
                out.push_str(LINK_CLOSE);
            }
            if let Some(u) = &want {
                out.push_str("\x1b]8;;");
                out.push_str(u);
                out.push_str("\x1b\\");
            }
            link = want;
        }
        out.push_str(&transition(pen, *p));
        pen = *p;
        out.push_str(&c.symbol);
    }
    if link.is_some() {
        out.push_str(LINK_CLOSE);
    }
    if pen != Pen::default() {
        out.push_str("\x1b[0m");
    }
    out
}

/// **A buffer for tests**: draw a widget, read the result as plain text or as the rows a
/// palette emits. Tests that assert on what a reader sees use [`TestBuffer::plain`];
/// tests about bytes use [`TestBuffer::rows`].
pub struct TestBuffer {
    pub buf: Buffer,
}

impl TestBuffer {
    pub fn new(width: u16, height: u16) -> TestBuffer {
        TestBuffer {
            buf: Buffer::empty(Rect::new(0, 0, width, height)),
        }
    }

    /// Draw `w` over the whole buffer.
    pub fn draw(mut self, w: &impl Widget) -> TestBuffer {
        let a = self.buf.area();
        w.render(a, &mut self.buf);
        self
    }

    /// Each row's text, trailing blanks trimmed: exactly what `Palette::None` emits.
    pub fn plain(&self) -> Vec<String> {
        self.buf.emit(Palette::None)
    }

    pub fn rows(&self, p: Palette) -> Vec<String> {
        self.buf.emit(p)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::{Line, Raw, Span, Style};
    use crate::style::Role;
    use crate::width::text::width;

    fn row(line: &Line, w: u16, p: Palette) -> String {
        let mut b = Buffer::empty(Rect::new(0, 0, w, 1));
        b.set_line(0, 0, line, w as usize);
        b.emit(p).remove(0)
    }

    #[test]
    fn a_role_is_its_palette_sequence_and_closes_at_the_row_end() {
        let l = Line::new(vec![Span::raw("a "), Span::role("fail", Role::Failure)]);
        assert_eq!(row(&l, 10, Palette::Colour), "a \x1b[31mfail\x1b[0m");
        assert_eq!(row(&l, 10, Palette::None), "a fail");
    }

    #[test]
    fn transitions_say_only_what_changed() {
        // Bold to bold+cyan: just the colour.
        let l = Line::new(vec![
            Span::role("a", Role::Strong),
            Span::role("b", Role::Heading),
            Span::role("c", Role::Code),
            Span::raw("d"),
        ]);
        assert_eq!(
            row(&l, 4, Palette::Colour),
            "\x1b[1ma\x1b[36mb\x1b[22mc\x1b[0md"
        );
        // Dropping dim but keeping bold: `22` drops both, so bold has to come back —
        // `22;1` — and a reset then bold is a byte shorter, so that is what is written.
        let l = Line::new(vec![
            Span::styled("x", Style::new().bold().dim()),
            Span::styled("y", Style::new().bold()),
        ]);
        assert_eq!(row(&l, 2, Palette::Colour), "\x1b[1;2mx\x1b[0;1my\x1b[0m");
        // Dropping italic under a colour that stays: the incremental `23` wins.
        let l = Line::new(vec![
            Span::styled("x", Style::of(Role::Code).italic()),
            Span::styled("y", Style::of(Role::Code)),
        ]);
        assert_eq!(row(&l, 2, Palette::Colour), "\x1b[3;36mx\x1b[23my\x1b[0m");
    }

    #[test]
    fn a_tint_under_syntax_is_one_sequence_and_survives_the_span_change() {
        let mut b = Buffer::empty(Rect::new(0, 0, 4, 1));
        b.set_line(
            0,
            0,
            &Line::new(vec![Span::role("fn", Role::Keyword), Span::raw(" x")]),
            4,
        );
        b.set_style(b.area(), &Style::of(Role::Added));
        assert_eq!(
            b.emit(Palette::Colour)[0],
            "\x1b[35;48;5;22mfn\x1b[39m x\x1b[0m"
        );
        assert_eq!(
            b.emit(Palette::Light)[0],
            "\x1b[35;48;5;194mfn\x1b[39m x\x1b[0m"
        );
    }

    #[test]
    fn the_none_palette_is_the_plain_text_and_nothing_else() {
        let mut b = Buffer::empty(Rect::new(0, 0, 12, 2));
        let l = Line::new(vec![
            Span::styled("link", Style::of(Role::Heading).link("https://x.org")),
            Span::role(" 你好", Role::Failure),
            Span::styled(
                "\u{10EEEE}",
                Style::new().raw(Raw {
                    fg_rgb: Some([0, 0, 7]),
                    underline: Some(1),
                }),
            ),
        ]);
        b.set_line(0, 0, &l, 12);
        let none = b.emit(Palette::None);
        assert_eq!(none[0], "link 你好\u{10EEEE}");
        assert_eq!(none[1], "");
        for r in &none {
            assert!(!r.contains('\x1b'), "{r:?}");
        }
        // And it is the plain grid, trimmed.
        let plain: Vec<String> = b
            .to_plain_lines()
            .iter()
            .map(|s| s.trim_end().to_string())
            .collect();
        assert_eq!(none, plain);
    }

    #[test]
    fn a_link_opens_and_closes_around_its_cells_even_where_clipping_cuts_it() {
        let url = "https://example.com/a";
        let l = Line::new(vec![
            Span::raw("see "),
            Span::styled("example.com", Style::new().link(url)),
            Span::raw(" ok"),
        ]);
        let full = row(&l, 20, Palette::Colour);
        assert_eq!(
            full,
            format!("see \x1b]8;;{url}\x1b\\example.com\x1b]8;;\x1b\\ ok")
        );
        // Clipped through the middle of the link: still closed at the row end.
        let cut = row(&l, 8, Palette::Colour);
        assert_eq!(cut, format!("see \x1b]8;;{url}\x1b\\exam\x1b]8;;\x1b\\"));
        assert_eq!(width(&cut), 8);
        // Truncated through it: the ellipsis is linked, and the link is closed after it.
        let t = crate::render::truncate(&l, 8);
        let cut = row(&t, 20, Palette::Colour);
        assert!(cut.ends_with(&format!("exa…{LINK_CLOSE}")), "{cut:?}");
    }

    #[test]
    fn two_adjacent_links_are_two_links() {
        let l = Line::new(vec![
            Span::styled("a", Style::new().link("u1")),
            Span::styled("b", Style::new().link("u2")),
        ]);
        assert_eq!(
            row(&l, 2, Palette::Colour),
            "\x1b]8;;u1\x1b\\a\x1b]8;;\x1b\\\x1b]8;;u2\x1b\\b\x1b]8;;\x1b\\"
        );
    }

    /// The kitty graphics placeholders pass through as cells: the cluster whole (the
    /// placeholder and its two diacritics), the image id in a 24-bit foreground and the
    /// placement in the underline colour — what letibot's `image_rows` spells by hand.
    #[test]
    fn a_graphics_placeholder_row_survives_intact() {
        let raw = Raw {
            fg_rgb: Some([0x12, 0x34, 0x56]),
            underline: Some(1),
        };
        let st = Style::new().raw(raw);
        let l = Line::new(vec![Span::styled(
            "\u{10EEEE}\u{305}\u{305}\u{10EEEE}\u{10EEEE}",
            st,
        )]);
        let r = row(&l, 3, Palette::Colour);
        assert_eq!(
            r,
            "\x1b[38;2;18;52;86;58;5;1m\u{10EEEE}\u{305}\u{305}\u{10EEEE}\u{10EEEE}\x1b[0m"
        );
        assert_eq!(width(&r), 3);
    }

    #[test]
    fn wide_characters_are_written_once_and_rows_measure_their_width() {
        let l = Line::new(vec![
            Span::role("中文", Role::Code),
            Span::raw("ab"),
            Span::role("🎉", Role::Success),
        ]);
        let r = row(&l, 9, Palette::Colour);
        assert_eq!(r, "\x1b[36m中文\x1b[0mab\x1b[32m🎉\x1b[0m");
        assert_eq!(width(&r), 8, "trailing blank trimmed");
    }

    #[test]
    fn trailing_blanks_are_trimmed_unless_they_carry_something() {
        let mut b = Buffer::empty(Rect::new(0, 0, 6, 1));
        b.set_line(0, 0, &Line::raw("ab"), 6);
        assert_eq!(b.emit(Palette::Colour)[0], "ab");
        b.set_style(Rect::new(0, 0, 6, 1), &Style::of(Role::UserBlock));
        assert_eq!(b.emit(Palette::Colour)[0], "\x1b[7mab    \x1b[0m");
    }

    #[test]
    fn a_reset_is_used_when_it_is_shorter() {
        let from = Pen::of(
            &Style::of(Role::Heading).italic().underline().reverse(),
            Palette::Colour,
        );
        let to = Pen::of(&Style::of(Role::Faint), Palette::Colour);
        assert_eq!(transition(from, to), "\x1b[0;2m");
    }

    #[test]
    fn the_test_buffer_reads_like_a_reader() {
        let t =
            TestBuffer::new(8, 2).draw(&crate::render::Paragraph::new("hello there").wrap(true));
        assert_eq!(t.plain(), vec!["hello", "there"]);
        assert_eq!(t.rows(Palette::Colour), t.plain());
    }
}
