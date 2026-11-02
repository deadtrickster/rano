//! Styled text: [`Span`], [`Line`], [`Text`], and [`wrap`], [`truncate`], [`ellipsise_left`],
//! [`fit`] — `crate::width::text`'s string functions over styled spans, sharing its breakpoint
//! finder. A style is an attribute of the cells, not bytes in the text.

use super::style::Style;
use crate::style::{Look, Palette, Role};
use crate::width::text::{Cell, break_cells, for_each_cell};

/// A run of text in one style. An escape sequence in the content is dropped when measured or
/// drawn: foreign text lands in spans, and a terminal would execute its escapes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Span {
    pub content: String,
    pub style: Style,
}

impl Span {
    /// Unstyled text.
    pub fn raw(s: impl Into<String>) -> Span {
        Span {
            content: s.into(),
            style: Style::new(),
        }
    }

    pub fn styled(s: impl Into<String>, style: impl Into<Style>) -> Span {
        Span {
            content: s.into(),
            style: style.into(),
        }
    }

    /// Text that means `r`.
    pub fn role(s: impl Into<String>, r: Role) -> Span {
        Span::styled(s, Style::of(r))
    }

    /// Columns on a terminal.
    pub fn width(&self) -> usize {
        crate::width::text::width(&self.content)
    }
}

impl From<&str> for Span {
    fn from(s: &str) -> Span {
        Span::raw(s)
    }
}

impl From<String> for Span {
    fn from(s: String) -> Span {
        Span::raw(s)
    }
}

/// One row's worth of spans. `style` lies under every span, so a line can say "this is an added
/// diff row" once.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Line {
    pub spans: Vec<Span>,
    pub style: Style,
}

impl Line {
    pub fn new(spans: Vec<Span>) -> Line {
        Line {
            spans,
            style: Style::new(),
        }
    }

    pub fn raw(s: impl Into<String>) -> Line {
        Line::new(vec![Span::raw(s)])
    }

    pub fn styled(s: impl Into<String>, style: impl Into<Style>) -> Line {
        Line::new(vec![Span::styled(s, style)])
    }

    /// The same line on `style`.
    pub fn on(mut self, style: impl Into<Style>) -> Line {
        self.style = style.into();
        self
    }

    pub fn push(&mut self, s: impl Into<Span>) {
        self.spans.push(s.into());
    }

    /// Columns on a terminal.
    pub fn width(&self) -> usize {
        self.spans.iter().map(Span::width).sum()
    }

    /// The text, styles dropped.
    pub fn plain(&self) -> String {
        let mut s = String::new();
        for sp in &self.spans {
            for_each_cell(&sp.content, |c| s.push_str(c.text));
        }
        s
    }

    pub fn is_empty(&self) -> bool {
        self.spans.iter().all(|s| s.content.is_empty())
    }

    /// Room for a row's ANSI form in one allocation, instead of reallocating per frame.
    fn ansi_capacity(&self) -> usize {
        self.spans.iter().map(|s| s.content.len() + 16).sum()
    }

    /// The line as one ANSI string, for a host that prints strings rather than drawing a
    /// [`super::Buffer`]: each span opens its own look and closes with a reset.
    pub fn to_ansi(&self, palette: Palette) -> String {
        let mut out = String::with_capacity(self.ansi_capacity());
        for sp in &self.spans {
            let look = self.style.patch(&sp.style).look(palette);
            push_span_ansi(&mut out, sp, look, None);
        }
        out
    }

    /// [`Line::to_ansi`] for a host that keeps the row's own style open: a styled span closes
    /// with a reset and the row's style again, since a reset alone restores the terminal default.
    pub fn to_ansi_inside(&self, palette: Palette) -> String {
        let restore = self.style.look(palette);
        let mut out = String::with_capacity(self.ansi_capacity() + 8 * self.spans.len());
        for sp in &self.spans {
            push_span_ansi(&mut out, sp, sp.style.look(palette), Some(restore));
        }
        out
    }
}

/// One span as ANSI: link opened, raw colours set, look, text, then reset, colours unset, link
/// closed.
fn push_span_ansi(out: &mut String, sp: &Span, look: Look, restore: Option<Look>) {
    use std::fmt::Write;
    if let Some(url) = &sp.style.link {
        out.push_str("\x1b]8;;");
        out.push_str(url);
        out.push_str("\x1b\\");
    }
    let raw = sp.style.raw;
    if let Some([r, g, b]) = raw.fg_rgb {
        let _ = write!(out, "\x1b[38;2;{r};{g};{b}m");
    }
    if let Some(u) = raw.underline {
        let _ = write!(out, "\x1b[58;5;{u}m");
    }
    look.write_sgr(out);
    for_each_cell(&sp.content, |c| out.push_str(c.text));
    if !look.is_plain() {
        out.push_str(crate::width::text::RESET);
        if let Some(r) = restore {
            r.write_sgr(out);
        }
    }
    if raw.fg_rgb.is_some() {
        out.push_str("\x1b[39m");
    }
    if raw.underline.is_some() {
        out.push_str("\x1b[59m");
    }
    if sp.style.link.is_some() {
        out.push_str("\x1b]8;;\x1b\\");
    }
}

impl From<&str> for Line {
    fn from(s: &str) -> Line {
        Line::raw(s)
    }
}

impl From<String> for Line {
    fn from(s: String) -> Line {
        Line::raw(s)
    }
}

impl From<Span> for Line {
    fn from(s: Span) -> Line {
        Line::new(vec![s])
    }
}

impl From<Vec<Span>> for Line {
    fn from(s: Vec<Span>) -> Line {
        Line::new(s)
    }
}

/// Lines, top to bottom.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Text {
    pub lines: Vec<Line>,
    pub style: Style,
}

impl Text {
    pub fn new(lines: Vec<Line>) -> Text {
        Text {
            lines,
            style: Style::new(),
        }
    }

    /// One line per `\n`-separated piece of `s`.
    pub fn raw(s: &str) -> Text {
        Text::new(s.split('\n').map(Line::raw).collect())
    }

    pub fn height(&self) -> usize {
        self.lines.len()
    }

    pub fn width(&self) -> usize {
        self.lines.iter().map(Line::width).max().unwrap_or(0)
    }
}

impl From<&str> for Text {
    fn from(s: &str) -> Text {
        Text::raw(s)
    }
}

impl From<Vec<Line>> for Text {
    fn from(l: Vec<Line>) -> Text {
        Text::new(l)
    }
}

impl From<Line> for Text {
    fn from(l: Line) -> Text {
        Text::new(vec![l])
    }
}

fn flatten(line: &Line) -> (Vec<Cell<'_>>, Vec<usize>) {
    let mut cells = Vec::new();
    let mut owner = Vec::new();
    for (i, sp) in line.spans.iter().enumerate() {
        for_each_cell(&sp.content, |c| {
            if !c.text.is_empty() {
                cells.push(Cell { esc: "", ..c });
                owner.push(i);
            }
        });
    }
    (cells, owner)
}

fn rebuild(line: &Line, cells: &[Cell], owner: &[usize]) -> Line {
    let mut out = Line {
        spans: Vec::new(),
        style: line.style.clone(),
    };
    let mut last: Option<usize> = None;
    for (c, &o) in cells.iter().zip(owner) {
        if last != Some(o) {
            out.spans.push(Span {
                content: String::new(),
                style: line.spans[o].style.clone(),
            });
            last = Some(o);
        }
        out.spans.last_mut().unwrap().content.push_str(c.text);
    }
    out
}

/// Wrap to `cols` columns by `width::text::wrap`'s break rules, styles kept on their cells.
pub fn wrap(line: &Line, cols: usize) -> Vec<Line> {
    let (cells, owner) = flatten(line);
    let rows = break_cells(&cells, cols.max(4));
    rows.into_iter()
        .map(|(a, mut b)| {
            while b > a && matches!(cells[b - 1].text, " " | "\n" | "\r" | "\r\n") {
                b -= 1;
            }
            rebuild(line, &cells[a..b], &owner[a..b])
        })
        .collect()
}

/// Truncate to `cols` columns, never splitting a cluster. The last column becomes `…`, in the
/// style of the first cell it stands for — a cut through a link leaves the `…` linked.
pub fn truncate(line: &Line, cols: usize) -> Line {
    if line.width() <= cols {
        return line.clone();
    }
    if cols == 0 {
        return Line {
            spans: Vec::new(),
            style: line.style.clone(),
        };
    }
    let (cells, owner) = flatten(line);
    let mut used = 0usize;
    let mut k = 0usize;
    while k < cells.len() && used + cells[k].cols < cols {
        used += cells[k].cols;
        k += 1;
    }
    let mut out = rebuild(line, &cells[..k], &owner[..k]);
    let at = owner.get(k).copied().unwrap_or(line.spans.len() - 1);
    let style = &line.spans[at].style;
    // Into the run it ends, when it is the same run — the shape a string truncate writes.
    match out.spans.last_mut() {
        Some(last) if &last.style == style => last.content.push('…'),
        _ => out.spans.push(Span::styled("…", style.clone())),
    }
    out
}

/// [`truncate`] for a line the caller is done with: handed back as it is when it fits.
pub fn truncate_owned(line: Line, cols: usize) -> Line {
    if line.width() <= cols {
        line
    } else {
        truncate(&line, cols)
    }
}

/// Shorten to `cols` columns by eating the left, keeping the end — the one to reach for on a
/// path, landing on a separator when one is within six columns.
pub fn ellipsise_left(line: &Line, cols: usize) -> Line {
    if line.width() <= cols {
        return line.clone();
    }
    if cols < 2 {
        return Line {
            spans: Vec::new(),
            style: line.style.clone(),
        };
    }
    let keep = cols - 1;
    let (cells, owner) = flatten(line);
    let mut used = 0usize;
    let mut start = cells.len();
    for (i, c) in cells.iter().enumerate().rev() {
        if used + c.cols > keep {
            start = i + 1;
            break;
        }
        used += c.cols;
        start = i;
    }
    const NUDGE: usize = 6;
    if start < cells.len()
        && !cells[start].text.starts_with('/')
        && let Some(next) = cells[start..].iter().position(|c| c.text.starts_with('/'))
        && next <= NUDGE
    {
        start += next;
    }
    let mut out = rebuild(line, &cells[start..], &owner[start..]);
    // The ellipsis is drawn in the style of what follows it, so a dim path stays dim.
    let style = owner
        .get(start)
        .map(|&o| line.spans[o].style.clone())
        .unwrap_or_default();
    out.spans.insert(0, Span::styled("…", style));
    out
}

/// Exactly `cols` columns: padded with spaces (in the line's own style), or truncated.
pub fn fit(line: &Line, cols: usize) -> Line {
    let w = line.width();
    if w > cols {
        return truncate(line, cols);
    }
    let mut out = line.clone();
    if w < cols {
        out.spans.push(Span::raw(" ".repeat(cols - w)));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::width::text as wt;

    fn two_tone(a: &str, b: &str) -> Line {
        Line::new(vec![
            Span::role(a, Role::Keyword),
            Span::role(b, Role::Code),
        ])
    }

    /// A link and a picture's placeholder colours are written under every palette.
    #[test]
    fn to_ansi_writes_links_and_placeholder_colours_under_every_palette() {
        let l = Line::new(vec![Span::styled(
            "a.rs",
            Style::new().link("file:///w/a.rs"),
        )]);
        for p in [Palette::Colour, Palette::None] {
            assert_eq!(
                l.to_ansi(p),
                "\x1b]8;;file:///w/a.rs\x1b\\a.rs\x1b]8;;\x1b\\"
            );
        }
        let lines = crate::term::graphics::image_lines(0x01_02_03, 3, 2);
        let rows = crate::term::graphics::image_rows(0x01_02_03, 3, 2);
        for (l, r) in lines.iter().zip(&rows) {
            assert_eq!(&l.to_ansi(Palette::None), r);
            assert_eq!(&l.to_ansi_inside(Palette::Colour), r);
        }
    }

    /// Inside a register a plain span is bare text and a styled one closes back to the register.
    #[test]
    fn to_ansi_inside_restores_the_rows_style_after_every_span() {
        let p = Palette::Colour;
        let mut l = Line::new(vec![
            Span::raw("a "),
            Span::role("kw", Role::Keyword),
            Span::raw(" b"),
        ]);
        assert_eq!(l.to_ansi_inside(p), l.to_ansi(p));
        l.style = Style::of(Role::Reasoning);
        let kw = p.open(Role::Keyword);
        let back = p.open(Role::Reasoning);
        assert_eq!(
            l.to_ansi_inside(p),
            format!("a {kw}kw{}{back} b", wt::RESET)
        );
        assert_eq!(l.to_ansi_inside(Palette::None), "a kw b");
    }

    #[test]
    fn wrap_breaks_where_the_string_wrap_does() {
        for s in [
            "the quick brown fox jumps over the lazy dog",
            "你好世界，这是一个测试。abc def",
            "mixed 混合 text with émojis 🎉 and more",
            &"x".repeat(120),
            "two\nlines",
            "",
        ] {
            for w in [5usize, 8, 13, 40] {
                let mid = s
                    .char_indices()
                    .nth(s.chars().count() / 2)
                    .map_or(0, |p| p.0);
                let line = two_tone(&s[..mid], &s[mid..]);
                let ours: Vec<String> = wrap(&line, w).iter().map(Line::plain).collect();
                assert_eq!(ours, wt::wrap(s, w), "{s:?} @{w}");
                for l in wrap(&line, w) {
                    assert!(l.width() <= w.max(4), "{s:?} @{w}: {l:?}");
                }
            }
        }
    }

    #[test]
    fn a_wrapped_span_keeps_its_style_on_both_rows() {
        let line = two_tone("aaaa ", "bbbb cccc");
        let rows = wrap(&line, 6);
        assert_eq!(
            rows.iter().map(Line::plain).collect::<Vec<_>>(),
            vec!["aaaa", "bbbb", "cccc"]
        );
        assert_eq!(rows[1].spans[0].style.top(), Role::Code);
        assert_eq!(rows[2].spans[0].style.top(), Role::Code);
        assert_eq!(rows[0].spans[0].style.top(), Role::Keyword);
    }

    #[test]
    fn truncation_does_not_cut_a_cluster_and_shows_the_cut() {
        let line = Line::raw("a\u{301}b\u{301}c\u{301}");
        let t = truncate(&line, 2);
        assert_eq!(t.width(), 2);
        assert_eq!(t.plain(), "a\u{301}…");
        let t = truncate(&Line::raw("你好世界"), 4);
        assert_eq!(t.plain(), "你…");
        assert_eq!(truncate(&Line::raw("short"), 10).plain(), "short");
        assert_eq!(truncate(&Line::raw("short"), 0).width(), 0);
        let t = truncate(&Line::new(vec![Span::role("abcdef", Role::Faint)]), 4);
        assert_eq!(t.spans.len(), 1);
        assert_eq!(t.to_ansi(Palette::Colour), "\x1b[2mabc…\x1b[0m");
    }

    #[test]
    fn ellipsise_left_keeps_the_end_and_lands_on_a_separator() {
        let l = Line::raw("crates/sessionlog/src/protocol.rs");
        let e = ellipsise_left(&l, 16);
        assert_eq!(e.plain(), wt::ellipsise_left(&l.plain(), 16));
        assert!(e.plain().ends_with("protocol.rs"));
        assert!(e.width() <= 16);
    }

    #[test]
    fn fit_is_exactly_the_width() {
        for (s, w) in [("ab", 5), ("你好世界", 5), ("", 3), ("toolong", 4)] {
            assert_eq!(fit(&Line::raw(s), w).width(), w, "{s:?}");
            assert_eq!(fit(&Line::raw(s), w).plain(), wt::fit(s, w), "{s:?}");
        }
    }

    #[test]
    fn an_escape_in_span_content_is_not_content() {
        let l = Line::raw("\x1b[31mred\x1b[0m");
        assert_eq!(l.width(), 3);
        assert_eq!(l.plain(), "red");
        assert_eq!(wrap(&l, 10)[0].plain(), "red");
    }
}
