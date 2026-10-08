//! Styled text: [`Span`], [`Line`], [`Text`], and the line-level [`wrap`], [`truncate`],
//! [`ellipsise_left`] and [`fit`].
//!
//! The four helpers are `crate::width::text`'s string functions over styled spans
//! instead of over strings carrying escapes. They are **the same breakpoints**, not a
//! re-implementation: [`wrap`] hands the flattened cells to `width::text::break_cells`,
//! the one breakpoint finder letibot's `wrap` and `wrap_ranges` already share, because
//! two wrappers kept in sync by a comment is a bug with a schedule.
//!
//! What changes with the medium: a style is an attribute of the cells, not bytes in the
//! text, so there is no SGR state to carry across a break and no reset to append — every
//! returned line is independently paintable by construction, and a link cut by
//! [`truncate`] is closed by the emitter where the linked cells end.

use super::style::Style;
use crate::style::Role;
use crate::width::text::{Cell, break_cells, for_each_cell};

/// A run of text in one style.
///
/// The content is text: an escape sequence in it is **dropped** when it is measured or
/// drawn, never passed through. Foreign text (a model's prose, a tool's output) lands in
/// spans, and an escape that reached the terminal from there would be one the terminal
/// executes; a style is said with [`Style`], not with bytes.
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

/// One row's worth of spans. `style` lies under every span (a span's own style is
/// patched over it), so a line can say "this is an added diff row" once.
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

/// A line's grapheme cells, each with the index of the span it came from.
fn flatten(line: &Line) -> (Vec<Cell<'_>>, Vec<usize>) {
    let mut cells = Vec::new();
    let mut owner = Vec::new();
    for (i, sp) in line.spans.iter().enumerate() {
        for_each_cell(&sp.content, |c| {
            if !c.text.is_empty() {
                // The escape run is dropped here: see [`Span`].
                cells.push(Cell { esc: "", ..c });
                owner.push(i);
            }
        });
    }
    (cells, owner)
}

/// Rebuild a line from a run of flattened cells, one span per run of a single owner.
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

/// Wrap to `cols` columns: `width::text::wrap`'s three break rules (a space; between two
/// wide clusters, which is what makes CJK wrap at all; anywhere, when one unbreakable run
/// is longer than a row), with the styles kept on the cells they belong to.
///
/// Trailing spaces and the newline at a break are dropped: invisible, and a painter that
/// erases to end of line would otherwise paint the row's background one column further
/// than the text goes; a newline is worse, the terminal acts on it.
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

/// Truncate to `cols` columns, never splitting a cluster.
///
/// When something is dropped the last column becomes `…`, so the elision is visible
/// rather than silent. The ellipsis takes the style of the first cell it stands for —
/// a cut through a link leaves the `…` linked, and the emitter closes the link after it.
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
    while k < cells.len() && used + cells[k].cols <= cols - 1 {
        used += cells[k].cols;
        k += 1;
    }
    let mut out = rebuild(line, &cells[..k], &owner[..k]);
    let at = owner.get(k).copied().unwrap_or(line.spans.len() - 1);
    out.spans
        .push(Span::styled("…", line.spans[at].style.clone()));
    out
}

/// **Shorten to `cols` columns by eating the LEFT, keeping the end** — the pair to
/// [`truncate`], and the one to reach for on a path.
///
/// A path is recognised by where it ends: `…/src/protocol.rs` names the file, while
/// `crates/sessionlog/src/protoco…` names only the tree it is in. The cut lands at a
/// separator when one is within six columns, so the result reads as a path rather than as
/// a word with a piece missing — bounded, because letibot measured the unbounded nudge
/// eating a long command down to its last fifty columns.
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
    if start < cells.len() && !cells[start].text.starts_with('/') {
        if let Some(next) = cells[start..].iter().position(|c| c.text.starts_with('/'))
            && next <= NUDGE
        {
            start += next;
        }
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

    /// The styled wrap and letibot's string wrap break in the same places, because they
    /// are the same breakpoint finder.
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
        // A wide character that does not fit is not halved.
        let t = truncate(&Line::raw("你好世界"), 4);
        assert_eq!(t.plain(), "你…");
        assert_eq!(truncate(&Line::raw("short"), 10).plain(), "short");
        assert_eq!(truncate(&Line::raw("short"), 0).width(), 0);
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
