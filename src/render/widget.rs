//! Things that draw themselves into an area of a [`Buffer`].
//!
//! Deliberately few: the trait, and the primitives that prove its shape — text as it
//! is, a paragraph that wraps, and a bordered box. The editor's panes and letibot's
//! cards are built from these by their owners, not added here.

use super::buffer::{Buffer, Rect};
use super::style::Style;
use super::text::{Line, Span, Text, wrap};

/// Draw into `area` of `buf`. Nothing outside `area` may change.
pub trait Widget {
    fn render(&self, area: Rect, buf: &mut Buffer);
}

impl Widget for Span {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        if !area.is_empty() {
            buf.set_span(area.x, area.y, self, area.width as usize);
        }
    }
}

impl Widget for Line {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        if !area.is_empty() {
            buf.set_line(area.x, area.y, self, area.width as usize);
        }
    }
}

impl Widget for Text {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        for (i, l) in self.lines.iter().take(area.height as usize).enumerate() {
            let l = Line {
                spans: l.spans.clone(),
                style: self.style.patch(&l.style),
            };
            buf.set_line(area.x, area.y + i as u16, &l, area.width as usize);
        }
    }
}

/// Text in an area: wrapped to its width or clipped at it, scrolled by whole rows, on an
/// optional background style that covers the whole area (so a tinted block is tinted
/// past the end of its text, which a style on the text alone cannot do).
#[derive(Debug, Clone, Default)]
pub struct Paragraph {
    pub text: Text,
    pub wrap: bool,
    /// Rows skipped from the top, counted after wrapping.
    pub scroll: u16,
    pub style: Option<Style>,
}

impl Paragraph {
    pub fn new(text: impl Into<Text>) -> Paragraph {
        Paragraph {
            text: text.into(),
            ..Paragraph::default()
        }
    }

    pub fn wrap(mut self, on: bool) -> Paragraph {
        self.wrap = on;
        self
    }

    pub fn scroll(mut self, rows: u16) -> Paragraph {
        self.scroll = rows;
        self
    }

    pub fn style(mut self, s: impl Into<Style>) -> Paragraph {
        self.style = Some(s.into());
        self
    }

    /// The rows this paragraph is at `width` columns, before scrolling — what a caller
    /// sizing an area for it needs.
    pub fn rows(&self, width: u16) -> Vec<Line> {
        let mut out = Vec::new();
        for l in &self.text.lines {
            let l = Line {
                spans: l.spans.clone(),
                style: self.text.style.patch(&l.style),
            };
            if self.wrap {
                out.extend(wrap(&l, width as usize));
            } else {
                out.push(l);
            }
        }
        out
    }
}

impl Widget for Paragraph {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        if area.is_empty() {
            return;
        }
        if let Some(s) = &self.style {
            buf.fill(area, s);
        }
        for (i, l) in self
            .rows(area.width)
            .iter()
            .skip(self.scroll as usize)
            .take(area.height as usize)
            .enumerate()
        {
            // Text drawn over a filled area keeps the fill underneath it.
            let l = match &self.style {
                Some(s) => Line {
                    spans: l.spans.clone(),
                    style: s.patch(&l.style),
                },
                None => l.clone(),
            };
            buf.set_line(area.x, area.y + i as u16, &l, area.width as usize);
        }
    }
}

/// A box drawn with the light box-drawing set, an optional title in its top edge, and
/// whatever is put in [`Bordered::inner`] by the caller.
///
/// The box-drawing characters are one column on every terminal and in both width tables
/// in this tree (letibot asserts that for its cell grid too), which is why a frame can be
/// measured by counting them.
#[derive(Debug, Clone, Default)]
pub struct Bordered {
    pub title: Option<Line>,
    /// The frame's own style; [`crate::style::Role::Faint`] is the usual one, since a
    /// frame is structure a reader skips.
    pub border: Style,
}

impl Bordered {
    pub fn new() -> Bordered {
        Bordered::default()
    }

    pub fn title(mut self, t: impl Into<Line>) -> Bordered {
        self.title = Some(t.into());
        self
    }

    pub fn border(mut self, s: impl Into<Style>) -> Bordered {
        self.border = s.into();
        self
    }

    /// Where the content goes: one cell in from every edge.
    pub fn inner(&self, area: Rect) -> Rect {
        area.inner(1)
    }
}

impl Widget for Bordered {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        if area.width < 2 || area.height < 2 {
            return;
        }
        let (x, y, r, b) = (area.x, area.y, area.right() - 1, area.bottom() - 1);
        let st = &self.border;
        let inner = (area.width - 2) as usize;
        let edge = "─".repeat(inner);
        buf.set_str(x, y, &format!("┌{edge}┐"), st, area.width as usize);
        buf.set_str(x, b, &format!("└{edge}┘"), st, area.width as usize);
        for row in y + 1..b {
            buf.set_str(x, row, "│", st, 1);
            buf.set_str(r, row, "│", st, 1);
        }
        if let Some(t) = &self.title {
            // ` title ` in the top edge, after the corner and one rule: room for it or
            // nothing, never a title that eats the corner.
            if inner >= 4 {
                let room = inner - 3;
                let t = super::text::truncate(t, room);
                let mut l = Line::new(vec![Span::raw(" ")]);
                l.spans.extend(t.spans.iter().map(|s| Span {
                    content: s.content.clone(),
                    style: t.style.patch(&s.style),
                }));
                l.spans.push(Span::raw(" "));
                buf.set_line(x + 2, y, &l.on(st.clone()), room + 2);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::style::Role;

    fn draw(w: &impl Widget, width: u16, height: u16) -> Vec<String> {
        let mut b = Buffer::empty(Rect::new(0, 0, width, height));
        w.render(Rect::new(0, 0, width, height), &mut b);
        b.to_plain_lines()
    }

    #[test]
    fn a_paragraph_wraps_scrolls_and_clips() {
        let p = Paragraph::new("the quick brown fox jumps").wrap(true);
        assert_eq!(
            draw(&p, 10, 3),
            vec!["the quick ", "brown fox ", "jumps     "]
        );
        assert_eq!(
            draw(&p.clone().scroll(1), 10, 2),
            vec!["brown fox ", "jumps     "]
        );
        let clipped = Paragraph::new("the quick brown fox jumps");
        assert_eq!(draw(&clipped, 10, 2), vec!["the quick ", "          "]);
    }

    #[test]
    fn a_filled_paragraph_keeps_its_fill_under_the_text() {
        let p = Paragraph::new(Text::from(Line::styled("kw", Role::Keyword))).style(Role::Added);
        let mut b = Buffer::empty(Rect::new(0, 0, 4, 1));
        p.render(b.area(), &mut b);
        let roles = |x| b.cell(x, 0).unwrap().style.roles().collect::<Vec<_>>();
        assert_eq!(roles(0), vec![Role::Added, Role::Keyword]);
        assert_eq!(roles(3), vec![Role::Added], "the fill runs past the text");
    }

    #[test]
    fn a_box_frames_its_inner_area_and_titles_its_edge() {
        let bx = Bordered::new().title("notes").border(Role::Faint);
        let area = Rect::new(0, 0, 12, 3);
        let mut b = Buffer::empty(area);
        bx.render(area, &mut b);
        Line::raw("inside text").render(bx.inner(area), &mut b);
        assert_eq!(
            b.to_plain_lines(),
            vec!["┌─ notes ──┐", "│inside tex│", "└──────────┘"]
        );
        for row in b.to_plain_lines() {
            assert_eq!(crate::width::text::width(&row), 12);
        }
        // A title too long for the edge is truncated, not allowed to eat the corner.
        let bx = Bordered::new().title("a very long title");
        let mut b = Buffer::empty(Rect::new(0, 0, 10, 2));
        bx.render(b.area(), &mut b);
        assert_eq!(b.to_plain_lines()[0], "┌─ a ve… ┐");
    }
}
