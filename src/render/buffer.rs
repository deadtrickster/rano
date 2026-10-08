//! A grid of cells: what one frame (or one widget's corner of it) will put on the glass.

use super::style::Style;
use super::text::{Line, Span};
use crate::width::text::for_each_cell;

/// A rectangle of cells, in absolute screen coordinates.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Rect {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
}

impl Rect {
    pub const fn new(x: u16, y: u16, width: u16, height: u16) -> Rect {
        Rect {
            x,
            y,
            width,
            height,
        }
    }

    pub const fn area(&self) -> usize {
        self.width as usize * self.height as usize
    }

    pub const fn is_empty(&self) -> bool {
        self.width == 0 || self.height == 0
    }

    /// One past the last column.
    pub const fn right(&self) -> u16 {
        self.x.saturating_add(self.width)
    }

    /// One past the last row.
    pub const fn bottom(&self) -> u16 {
        self.y.saturating_add(self.height)
    }

    /// The part of `self` that is also in `other` (empty, at `self`'s corner, if none).
    pub fn intersection(&self, other: Rect) -> Rect {
        let x = self.x.max(other.x);
        let y = self.y.max(other.y);
        let r = self.right().min(other.right());
        let b = self.bottom().min(other.bottom());
        if r <= x || b <= y {
            return Rect::new(self.x, self.y, 0, 0);
        }
        Rect::new(x, y, r - x, b - y)
    }

    /// Shrunk by `n` cells on every side.
    pub fn inner(&self, n: u16) -> Rect {
        let w = self.width.saturating_sub(2 * n);
        let h = self.height.saturating_sub(2 * n);
        Rect::new(self.x.saturating_add(n), self.y.saturating_add(n), w, h)
    }

    /// The single row `i` of this rect (empty past the bottom).
    pub fn row(&self, i: u16) -> Rect {
        if i >= self.height {
            return Rect::new(self.x, self.y, 0, 0);
        }
        Rect::new(self.x, self.y + i, self.width, 1)
    }

    /// Split off the top `h` rows: `(top, rest)`.
    pub fn split_top(&self, h: u16) -> (Rect, Rect) {
        let h = h.min(self.height);
        (
            Rect::new(self.x, self.y, self.width, h),
            Rect::new(self.x, self.y + h, self.width, self.height - h),
        )
    }

    pub fn contains(&self, x: u16, y: u16) -> bool {
        x >= self.x && x < self.right() && y >= self.y && y < self.bottom()
    }
}

/// One cell: a grapheme cluster and its style.
///
/// A wide cluster occupies two cells: the **lead**, which holds the cluster and
/// `width == 2`, and a **continuation** after it with `width == 0` and an empty symbol.
/// The emitter writes the lead and skips the continuation, and every write that lands on
/// half of a wide pair blanks the other half — a terminal given half a wide glyph draws
/// something different on every terminal, and a row that measures differently from what
/// the painter believes is how a diff paints over the wrong cells.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cell {
    /// The whole cluster — a base and every mark that joined it, so a kitty graphics
    /// placeholder keeps its row and column diacritics and no escape can ever be written
    /// between them.
    pub symbol: String,
    /// Columns: 1 or 2 for a lead, 0 for a continuation.
    pub width: u8,
    pub style: Style,
}

impl Cell {
    fn blank() -> Cell {
        Cell {
            symbol: " ".into(),
            width: 1,
            style: Style::new(),
        }
    }

    fn set(&mut self, sym: &str, width: u8, style: &Style) {
        self.symbol.clear();
        self.symbol.push_str(sym);
        self.width = width;
        if self.style != *style {
            self.style = style.clone();
        }
    }

    pub fn is_continuation(&self) -> bool {
        self.width == 0
    }
}

/// Tab stops in a span are every eight columns from where the line was placed.
const TAB: u16 = 8;

/// A grid of [`Cell`]s covering `area`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Buffer {
    area: Rect,
    cells: Vec<Cell>,
}

impl Buffer {
    /// Blank cells over `area`.
    pub fn empty(area: Rect) -> Buffer {
        Buffer {
            area,
            cells: vec![Cell::blank(); area.area()],
        }
    }

    pub fn area(&self) -> Rect {
        self.area
    }

    /// Every cell back to a blank, keeping the allocations.
    pub fn reset(&mut self) {
        let blank = Style::new();
        for c in &mut self.cells {
            c.set(" ", 1, &blank);
        }
    }

    /// A different area, blank; reuses the storage when it can.
    pub fn resize(&mut self, area: Rect) {
        self.area = area;
        self.cells.resize(area.area(), Cell::blank());
        self.reset();
    }

    fn index(&self, x: u16, y: u16) -> Option<usize> {
        if !self.area.contains(x, y) {
            return None;
        }
        Some((y - self.area.y) as usize * self.area.width as usize + (x - self.area.x) as usize)
    }

    pub fn cell(&self, x: u16, y: u16) -> Option<&Cell> {
        self.index(x, y).map(|i| &self.cells[i])
    }

    /// The cells of row `y` (absolute), left to right.
    pub fn row(&self, y: u16) -> &[Cell] {
        match self.index(self.area.x, y) {
            Some(i) => &self.cells[i..i + self.area.width as usize],
            None => &[],
        }
    }

    /// Blank whatever half of a wide pair would be left behind by writing `w` cells at `x`.
    fn unpair(&mut self, x: u16, y: u16, w: u16) {
        // Writing onto a continuation orphans its lead.
        if let Some(i) = self.index(x, y)
            && self.cells[i].is_continuation()
            && x > self.area.x
        {
            let st = self.cells[i - 1].style.clone();
            self.cells[i - 1].set(" ", 1, &st);
        }
        // Covering a lead whose continuation lies past the write orphans the continuation.
        let last = x + w - 1;
        if let Some(i) = self.index(last, y)
            && self.cells[i].width == 2
            && let Some(j) = self.index(last + 1, y)
        {
            let st = self.cells[j].style.clone();
            self.cells[j].set(" ", 1, &st);
        }
    }

    fn put(&mut self, x: u16, y: u16, sym: &str, w: u8, style: &Style) {
        self.unpair(x, y, w as u16);
        if let Some(i) = self.index(x, y) {
            self.cells[i].set(sym, w, style);
        }
        if w == 2
            && let Some(j) = self.index(x + 1, y)
        {
            self.cells[j].set("", 0, style);
        }
    }

    /// Write `s` at `(x, y)` in `style`, at most `max` columns and never past the buffer's
    /// right edge. Returns the column after the last one written.
    ///
    /// What does not get a cell: an escape sequence (dropped — see [`Span`]), a control
    /// character, and a zero-width mark with no base in `s` (it joins the cell to its
    /// left, which is where a span boundary put its base). A tab advances to the next stop
    /// counted from `origin`, in spaces. A wide cluster that does not fit whole is not
    /// halved: the column it would have started is filled with a space instead.
    pub fn set_str(&mut self, x: u16, y: u16, s: &str, style: &Style, max: usize) -> u16 {
        self.set_str_from(x, x, y, s, style, max)
    }

    fn set_str_from(
        &mut self,
        origin: u16,
        x: u16,
        y: u16,
        s: &str,
        style: &Style,
        max: usize,
    ) -> u16 {
        if y < self.area.y || y >= self.area.bottom() {
            return x;
        }
        let limit = (x as usize + max).min(self.area.right() as usize) as u16;
        let mut cx = x;
        for_each_cell(s, |c| {
            if cx >= limit || c.text.is_empty() {
                return;
            }
            if c.text == "\t" {
                let stop = origin + ((cx - origin) / TAB + 1) * TAB;
                while cx < stop.min(limit) {
                    if cx >= self.area.x {
                        self.put(cx, y, " ", 1, style);
                    }
                    cx += 1;
                }
                return;
            }
            if c.cols == 0 {
                let ch = c.text.chars().next().unwrap_or('\0');
                if !crate::width::text::is_control(ch) && cx > self.area.x.max(origin) {
                    // A mark whose base is in the previous span: it belongs to that cell.
                    let mut i = self.index(cx - 1, y).unwrap_or(0);
                    if self.cells[i].is_continuation() && i > 0 {
                        i -= 1;
                    }
                    self.cells[i].symbol.push_str(c.text);
                }
                return;
            }
            let w = c.cols as u16;
            if cx + w > limit {
                // Half a wide character is not a thing a terminal draws the same way twice.
                while cx < limit {
                    if cx >= self.area.x {
                        self.put(cx, y, " ", 1, style);
                    }
                    cx += 1;
                }
                return;
            }
            if cx >= self.area.x {
                self.put(cx, y, c.text, w as u8, style);
            }
            cx += w;
        });
        cx
    }

    /// Write a span at `(x, y)`, at most `max` columns. Returns the column after it.
    pub fn set_span(&mut self, x: u16, y: u16, span: &Span, max: usize) -> u16 {
        self.set_str(x, y, &span.content, &span.style, max)
    }

    /// Write a line at `(x, y)`, at most `max` columns, each span's style patched over the
    /// line's. Returns the column after it.
    pub fn set_line(&mut self, x: u16, y: u16, line: &Line, max: usize) -> u16 {
        let mut cx = x;
        let end = x as usize + max;
        for sp in &line.spans {
            let left = end.saturating_sub(cx as usize);
            if left == 0 {
                break;
            }
            let st = line.style.patch(&sp.style);
            cx = self.set_str_from(x, cx, y, &sp.content, &st, left);
        }
        cx
    }

    /// Patch `style` over every cell in `area`, keeping their text: a row's tint under
    /// text already drawn, a selection over a list.
    pub fn set_style(&mut self, area: Rect, style: &Style) {
        let a = self.area.intersection(area);
        for y in a.y..a.bottom() {
            for x in a.x..a.right() {
                if let Some(i) = self.index(x, y) {
                    let st = self.cells[i].style.patch(style);
                    self.cells[i].style = st;
                }
            }
        }
    }

    /// Blank every cell in `area` and give it `style`.
    pub fn fill(&mut self, area: Rect, style: &Style) {
        let a = self.area.intersection(area);
        for y in a.y..a.bottom() {
            self.unpair(a.x, y, 1);
            if a.right() > a.x {
                self.unpair(a.right() - 1, y, 1);
            }
            for x in a.x..a.right() {
                if let Some(i) = self.index(x, y) {
                    self.cells[i].set(" ", 1, style);
                }
            }
        }
    }

    /// The text of every row, full width, styles dropped. A wide cluster is its lead's
    /// symbol, so each string measures exactly the buffer's width.
    pub fn to_plain_lines(&self) -> Vec<String> {
        (self.area.y..self.area.bottom())
            .map(|y| {
                self.row(y)
                    .iter()
                    .filter(|c| !c.is_continuation())
                    .map(|c| c.symbol.as_str())
                    .collect()
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::style::Role;

    fn buf(w: u16, h: u16) -> Buffer {
        Buffer::empty(Rect::new(0, 0, w, h))
    }

    #[test]
    fn a_line_is_clipped_to_its_width_and_to_the_buffer() {
        let mut b = buf(6, 2);
        let end = b.set_line(0, 0, &Line::raw("hello world"), 4);
        assert_eq!(end, 4);
        b.set_line(3, 1, &Line::raw("abcdef"), 99);
        assert_eq!(b.to_plain_lines(), vec!["hell  ", "   abc"]);
        // Off the buffer entirely: nothing, and no panic.
        b.set_line(0, 9, &Line::raw("x"), 9);
    }

    #[test]
    fn a_wide_character_takes_two_cells_and_is_never_halved() {
        let mut b = buf(5, 1);
        b.set_line(0, 0, &Line::raw("你好世"), 5);
        // Two wide clusters fit; the third would straddle the edge and becomes a space.
        assert_eq!(b.to_plain_lines(), vec!["你好 "]);
        assert_eq!(b.cell(1, 0).unwrap().width, 0, "continuation");
        // Overwriting the continuation blanks the lead it belonged to.
        b.set_str(1, 0, "x", &Style::new(), 1);
        assert_eq!(b.to_plain_lines(), vec![" x好 "]);
        // Overwriting a lead blanks its continuation.
        b.set_str(2, 0, "y", &Style::new(), 1);
        assert_eq!(b.to_plain_lines(), vec![" xy  "]);
        for row in b.to_plain_lines() {
            assert_eq!(crate::width::text::width(&row), 5);
        }
    }

    #[test]
    fn a_cluster_is_one_cell_with_all_its_marks() {
        let mut b = buf(4, 1);
        b.set_line(0, 0, &Line::raw("e\u{301}x"), 4);
        assert_eq!(b.cell(0, 0).unwrap().symbol, "e\u{301}");
        // A mark at the start of a span joins the previous span's last cell.
        let mut b = buf(4, 1);
        let l = Line::new(vec![Span::raw("e"), Span::role("\u{301}z", Role::Code)]);
        b.set_line(0, 0, &l, 4);
        assert_eq!(b.cell(0, 0).unwrap().symbol, "e\u{301}");
        assert_eq!(b.cell(1, 0).unwrap().symbol, "z");
    }

    #[test]
    fn escapes_and_controls_in_content_never_reach_a_cell() {
        let mut b = buf(8, 1);
        b.set_line(0, 0, &Line::raw("a\x1b[31mb\x07c\x1b]0;pwned\x07d"), 8);
        assert_eq!(b.to_plain_lines(), vec!["abcd    "]);
    }

    #[test]
    fn a_tab_advances_to_the_next_stop_from_the_line_start() {
        let mut b = buf(12, 1);
        let l = Line::new(vec![Span::raw("ab"), Span::raw("\tc")]);
        b.set_line(2, 0, &l, 10);
        assert_eq!(b.to_plain_lines(), vec!["  ab      c "]);
    }

    #[test]
    fn set_style_patches_and_keeps_the_text() {
        let mut b = buf(3, 1);
        b.set_line(0, 0, &Line::styled("abc", Role::Keyword), 3);
        b.set_style(Rect::new(0, 0, 3, 1), &Style::of(Role::Added));
        assert_eq!(b.to_plain_lines(), vec!["abc"]);
        let roles: Vec<_> = b.cell(0, 0).unwrap().style.roles().collect();
        assert_eq!(roles, vec![Role::Keyword, Role::Added]);
    }

    #[test]
    fn a_buffer_with_an_offset_area_addresses_absolutely() {
        let mut b = Buffer::empty(Rect::new(10, 5, 4, 2));
        b.set_line(10, 6, &Line::raw("hi"), 4);
        b.set_line(0, 0, &Line::raw("nope"), 4);
        assert_eq!(b.to_plain_lines(), vec!["    ", "hi  "]);
    }

    #[test]
    fn rects_intersect_and_shrink() {
        let a = Rect::new(0, 0, 10, 10);
        assert_eq!(
            a.intersection(Rect::new(5, 5, 10, 10)),
            Rect::new(5, 5, 5, 5)
        );
        assert!(a.intersection(Rect::new(20, 20, 1, 1)).is_empty());
        assert_eq!(a.inner(1), Rect::new(1, 1, 8, 8));
        assert_eq!(Rect::new(0, 0, 1, 1).inner(1).area(), 0);
        let (t, r) = a.split_top(3);
        assert_eq!((t.height, r.y, r.height), (3, 3, 7));
    }
}
