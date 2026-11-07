//! A pane: a list the person moves through, drawn in place of the conversation — the shape the todos,
//! subagents, jobs, queue and picker panes share.
//!
//! Content is every row the pane has plus the row each stop was drawn on; a window is that content in
//! exactly the room, with the scroll clamped so the last content row is reachable. A pane longer than
//! its room spends one row on a seam that counts what is out of view, and a shorter one is padded, so
//! the frame is the room's height and not the data's. The host owns the scroll and the cursor, because
//! both outlive a frame and are moved by keys; this clamps them and hands them back.
//!
//! The stop rows are a record and not arithmetic: a click handler doing arithmetic over the list
//! (`line - header`) while the pane drew something else is exactly the bug that made the arrows and
//! the mouse disagree, so the drawing writes the record both of them read.

use crate::render::{Buffer, Line, Rect, Span, Widget};
use crate::style::Role;

/// A pane's rows and the row each stop landed on.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PaneLines {
    pub lines: Vec<Line>,
    /// `stop_rows[k]` is the index in [`PaneLines::lines`] of stop `k`'s first row.
    pub stop_rows: Vec<usize>,
}

impl PaneLines {
    pub fn new() -> PaneLines {
        PaneLines::default()
    }

    /// A row that is not a stop.
    pub fn push(&mut self, l: impl Into<Line>) {
        self.lines.push(l.into());
    }

    /// The first row of the next stop: records where it landed, then pushes it.
    pub fn push_stop(&mut self, l: impl Into<Line>) {
        self.stop_rows.push(self.lines.len());
        self.lines.push(l.into());
    }

    /// A blank row.
    pub fn blank(&mut self) {
        self.lines.push(Line::default());
    }

    /// The pane row the stop at the cursor was drawn on, read out of the record the pane wrote while
    /// drawing. The cursor is clamped first: the table can move under it, and an arrow pressed against a
    /// shorter list must land on a row rather than on an index that no longer exists.
    pub fn row_of(&self, cursor: usize) -> usize {
        let at = cursor.min(self.stop_rows.len().saturating_sub(1));
        self.stop_rows.get(at).copied().unwrap_or(0)
    }

    /// Which stop was drawn on pane row `row`, if any.
    pub fn stop_on(&self, row: usize) -> Option<usize> {
        self.stop_rows.iter().position(|r| *r == row)
    }

    /// Every row cut to `w` columns.
    pub fn trimmed(mut self, w: usize) -> PaneLines {
        for l in &mut self.lines {
            if l.width() > w {
                *l = crate::render::text::truncate(l, w);
            }
        }
        self
    }
}

/// What a pane drew this frame: the visible rows, and the numbers the host keeps.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PaneLayout {
    /// The visible slice, exactly `room` rows: the seam at one end when the content did not fit, and
    /// `w`-wide pads under the last row when it did.
    pub lines: Vec<Line>,
    /// The scroll actually used — rows hidden above the top, clamped. Store it back.
    pub scroll: usize,
    /// The rows the window had (letibot's `pane_room`).
    pub room: usize,
    /// The rows the content has (letibot's `pane_len`), for PageDown's clamp. The content's height,
    /// never the window's: a seam spends a row of the room, not a row of the list.
    pub len: usize,
    /// Where each stop landed, in content rows (not screen rows): see [`PaneLines`].
    pub stop_rows: Vec<usize>,
    /// The row that counts what the window could not draw, when anything is out of view.
    pub seam: Option<Seam>,
}

/// Rows a window could not draw, and the row it says so on: `… 3 above, 12 below · pgup/pgdn
/// scrolls`. A count and not "there is more", because a reader has to know whether one row or four
/// hundred are missing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Seam {
    /// The row of the window the count is drawn on: 0 at the head, the last row at the end.
    pub row: usize,
    /// Rows hidden above the top.
    pub above: usize,
    /// Rows hidden below the bottom.
    pub below: usize,
}

impl PaneLayout {
    /// The stop drawn on row `y` of the pane's area, or nothing. `y` is relative to the pane's first
    /// drawn row, and it is guarded on the window: a row below the last drawn one, a pad and the seam
    /// are not rows anybody can land on.
    pub fn stop_at(&self, y: usize) -> Option<usize> {
        if y >= self.room.min(self.lines.len()) {
            return None;
        }
        // A seam at the head pushes the content down one row. That offset is read off the record the
        // drawing wrote rather than recomputed here, for the reason the stop rows are a record at all.
        let top = usize::from(matches!(self.seam, Some(s) if s.row == 0));
        if y < top {
            return None;
        }
        let row = y - top + self.scroll;
        if row >= self.len {
            return None;
        }
        self.stop_rows.iter().position(|r| *r == row)
    }
}

impl Widget for PaneLayout {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        crate::agent::draw_lines(&self.lines, area, buf);
    }
}

/// The visible slice of a pane, and the numbers the scroll keys need.
///
/// The frame is `room` rows whatever the content is: a pane shorter than its room is padded with
/// `w`-wide space rows, which is what clears the rows under it (`draw_lines` paints the lines it is
/// given and leaves the rest of the area as it was), and a pane longer than its room spends one row on
/// a seam that counts what is out of view — spent out of the frame rather than appended past it, so a
/// full pane never draws taller than the room it was given.
///
/// Clamped here rather than at the keypress, since the key handler knows neither the terminal's height
/// nor the pane's row count, and a scroll clamped against a stale height scrolls past the end. `scroll`
/// counts rows hidden above the top — literally `skip(scroll)` — so Up decreases it. The furthest
/// anything scrolls is the last screenful, and that is one row less of list than the room because the
/// seam spent the other: clamped against `len - room` the last content row could never be drawn.
///
/// `w` is the width the content was built at, and it is the pads' width: a pad narrower than the area
/// would leave the right of its row as the frame before it drew it.
pub fn window(content: PaneLines, w: usize, scroll: usize, room: usize) -> PaneLayout {
    let len = content.lines.len();
    if room == 0 {
        return PaneLayout {
            lines: Vec::new(),
            scroll: 0,
            room,
            len,
            stop_rows: content.stop_rows,
            seam: None,
        };
    }
    // The furthest anything scrolls keeps the last content row at the bottom of the frame. When the
    // seam draws, that is one row less of list than the room has.
    let shown = shown(room, len);
    let scroll = scroll.min(len - shown);
    let above = scroll;
    let below = len.saturating_sub(scroll + shown);
    let mut lines: Vec<Line> = Vec::with_capacity(room);
    let mut seam = None;
    // At the end of the list the seam moves to the head, so the row it costs is never the row the last
    // content row would have been drawn on.
    if below == 0 && above > 0 {
        seam = Some(Seam {
            row: 0,
            above,
            below,
        });
        lines.push(seam_line(above, below, w));
    }
    lines.extend(content.lines.into_iter().skip(scroll).take(shown));
    if below > 0 {
        seam = Some(Seam {
            row: lines.len(),
            above,
            below,
        });
        lines.push(seam_line(above, below, w));
    }
    while lines.len() < room {
        lines.push(Line::raw(" ".repeat(w)));
    }
    PaneLayout {
        lines,
        scroll,
        room,
        len,
        stop_rows: content.stop_rows,
        seam,
    }
}

/// The row that counts what the window could not draw. The words are `card_window`'s
/// (agent/decision.rs): one sentence for one situation wherever a reader meets it. A pane too narrow
/// for the sentence drops the key hint before it drops the count, which is the part a reader cannot
/// get anywhere else.
fn seam_line(above: usize, below: usize, w: usize) -> Line {
    let said = match (above, below) {
        (0, below) => format!("  … {below} line(s) out of view · pgdn scrolls"),
        (above, 0) => format!("  … {above} line(s) out of view · pgup scrolls"),
        (above, below) => format!("  … {above} above, {below} below · pgup/pgdn scrolls"),
    };
    let mut line = Line::new(vec![Span::role(said, Role::Faint)]);
    if line.width() > w {
        line = Line::new(vec![Span::role(
            format!("  … {} out of view", above + below),
            Role::Faint,
        )]);
    }
    if line.width() > w {
        line = crate::render::text::truncate(&line, w);
    }
    line
}

/// How many content rows a window of `room` rows draws: all of them when they fit, and one fewer when
/// the seam has to be drawn — the seam is spent out of the room, not appended past it.
pub fn shown(room: usize, len: usize) -> usize {
    if len <= room {
        len
    } else {
        room.saturating_sub(1)
    }
}

/// Keep the cursor on screen after an arrow moved it: the scroll that shows content row `row` in a
/// window of `room` rows over `len` rows of content, moving as little as it can. Without it an arrow
/// that walks the selection out of the window looks like a key that does nothing.
///
/// It asks for `len` as well as `room` because the window draws one row fewer whenever the seam is
/// drawn, and a row scrolled onto the seam row is a row the pane did not draw: the cursor would walk
/// off the bottom of a list that still had rows under it.
pub fn scroll_into_view(scroll: usize, room: usize, len: usize, row: usize) -> usize {
    let shown = shown(room, len);
    if shown == 0 {
        scroll
    } else if row < scroll {
        row
    } else if row >= scroll + shown {
        row + 1 - shown
    } else {
        scroll
    }
}

/// The cursor's mark: `▸` on the row Enter takes, a space elsewhere, so the mark is the thing Enter
/// takes.
pub fn mark(picked: bool) -> &'static str {
    if picked { "▸" } else { " " }
}

/// The picked row's highlight: inverse video rather than another colour, since a second hue reads as a
/// second kind of thing. Nothing under `Palette::None`, where the `▸` is what survives.
pub fn picked(l: Line, on: bool) -> Line {
    if on {
        let st = l.style.clone().reverse();
        Line { style: st, ..l }
    } else {
        l
    }
}

/// `left`, then `right` pushed to the right edge — or `left` alone, cut to `w`, when the two do not fit
/// with two columns between them.
pub fn split_row(left: Line, right: Line, w: usize) -> Line {
    let (lw, rw) = (left.width(), right.width());
    if lw + rw + 2 <= w {
        let mut out = Line::new(Vec::new());
        // The left's own style (a picked row's inverse) stays on the left alone.
        out.spans.extend(
            left.spans
                .iter()
                .map(|s| Span::styled(s.content.clone(), left.style.patch(&s.style))),
        );
        out.spans.push(Span::raw(" ".repeat(w - lw - rw)));
        out.spans.extend(
            right
                .spans
                .iter()
                .map(|s| Span::styled(s.content.clone(), right.style.patch(&s.style))),
        );
        out
    } else {
        crate::render::text::truncate_owned(left, w)
    }
}

/// A row in `Role::Faint`: a pane's detail lines, empty states and footers.
pub fn faint(s: impl Into<String>) -> Line {
    Line::new(vec![Span::role(s, Role::Faint)])
}

/// A pane's title: bold, alone on its row.
pub fn title(s: impl Into<String>) -> Line {
    Line::new(vec![Span::role(s, Role::Strong)])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::testing::{plain, role_of};

    fn content(n: usize) -> PaneLines {
        let mut p = PaneLines::new();
        p.push(title("a pane"));
        p.blank();
        for i in 0..n {
            p.push_stop(format!("item {i}"));
            p.push(faint(format!("    detail {i}")));
        }
        p.push(faint("    esc closes · up/down scrolls"));
        p
    }

    /// The width the panes in these tests are built at: wide enough for the seam's whole sentence.
    const W: usize = 48;

    /// A pane taller than the screen scrolls to its end: PageDown past the end is clamped to the last
    /// screenful, and the final row is still drawn — under the seam that counted what is above it.
    #[test]
    fn a_pane_taller_than_the_screen_scrolls_to_its_end() {
        let top = window(content(30), W, 0, 10);
        assert_eq!(plain(&top.lines)[0], "a pane");
        assert_eq!(top.len, 63);
        assert_eq!(top.lines.len(), 10, "the frame is the room, seam included");
        assert_eq!(
            plain(&top.lines)[9].trim(),
            "… 54 line(s) out of view · pgdn scrolls"
        );
        let end = window(content(30), W, 10_000, 10);
        assert_eq!(
            end.scroll,
            63 - 9,
            "the last screenful, one row of it a seam"
        );
        assert_eq!(
            plain(&end.lines)[0].trim(),
            "… 54 line(s) out of view · pgup scrolls"
        );
        assert_eq!(
            plain(&end.lines).last().unwrap().trim(),
            "esc closes · up/down scrolls"
        );
        assert!(!plain(&end.lines).iter().any(|l| l == "item 0"));
        // A pane shorter than its room never scrolls at all.
        assert_eq!(window(content(2), W, 5, 40).scroll, 0);
    }

    /// A frame is its room whatever the content is: the rows a short pane has nothing to say on are
    /// pads, real rows of spaces, since `draw_lines` leaves the rows it is not given as they were.
    #[test]
    fn a_short_pane_is_padded_to_its_room() {
        let l = window(content(1), 12, 0, 10);
        assert_eq!(l.lines.len(), 10);
        assert_eq!(l.seam, None, "nothing is out of view");
        assert_eq!(l.scroll, 0);
        assert_eq!(plain(&l.lines)[4].trim(), "esc closes · up/down scrolls");
        for pad in &l.lines[5..] {
            assert_eq!(pad.width(), 12, "a pad is as wide as the pane it clears");
            assert_eq!(pad.plain(), " ".repeat(12));
        }
    }

    /// A pane longer than its room spends one row of the room on the count rather than appending it
    /// past the frame, and the count is of the rows that did not fit.
    #[test]
    fn a_long_pane_counts_what_is_out_of_view_on_one_row() {
        let l = window(content(30), W, 0, 10);
        assert_eq!(l.lines.len(), 10, "never room + 1");
        assert_eq!(
            l.seam,
            Some(Seam {
                row: 9,
                above: 0,
                below: 54,
            })
        );
        assert_eq!(role_of(&l.lines[9], "line(s)"), Some(Role::Faint));
        assert_eq!(l.stop_at(9), None, "the seam is not a row to land on");
    }

    /// At the end of the list the seam moves to the head, and `stop_at` reads that offset off the record
    /// the drawing wrote: row 0 is the count, row 1 is the first stop drawn.
    #[test]
    fn a_head_seam_moves_the_first_visible_stop_down_one_row() {
        let l = window(content(30), W, usize::MAX, 10);
        assert_eq!(l.scroll, 54);
        assert_eq!(
            l.seam,
            Some(Seam {
                row: 0,
                above: 54,
                below: 0,
            })
        );
        // Content row 54 is stop 26 ("item 26"), the first row the window draws under the seam.
        assert_eq!(plain(&l.lines)[1], "item 26");
        assert_eq!(l.stop_at(0), None, "the seam");
        assert_eq!(l.stop_at(1), Some(26));
    }

    /// The seam spends a row of the room and not a row of the list: at the last screenful the final
    /// content row is drawn, which a clamp against `len - room` could never reach.
    #[test]
    fn the_last_content_row_is_reachable_at_the_end() {
        let end = window(content(30), W, usize::MAX, 10);
        assert_eq!(
            plain(&end.lines).last().unwrap().trim(),
            "esc closes · up/down scrolls"
        );
        // The cursor's own scroll never parks the cursor's row on the seam either: `scroll_into_view`
        // asks for the list's length because the window draws one row fewer when the seam draws.
        let row = 2 + 2 * 29; // "item 29", the last stop
        let scroll = scroll_into_view(0, 10, end.len, row);
        let l = window(content(30), W, scroll, 10);
        let at = match l.seam {
            Some(s) if s.row == 0 => 1 + (row - scroll),
            _ => row - scroll,
        };
        assert_eq!(plain(&l.lines)[at], "item 29");
        assert_ne!(
            Some(at),
            l.seam.map(|s| s.row),
            "the cursor row is the seam"
        );
    }

    /// A seam in a pane too narrow for the sentence keeps the count, which is the part a reader cannot
    /// get anywhere else, and is cut with the renderer's `…` rather than mid-word.
    #[test]
    fn a_narrow_seam_drops_the_key_hint_before_the_count() {
        let l = window(content(30), 12, 0, 10);
        let seam = &l.lines[9];
        assert_eq!(seam.width(), 12, "{}", seam.plain());
        assert!(seam.plain().starts_with("  … 54"), "{}", seam.plain());
        assert!(seam.plain().ends_with('…'), "{}", seam.plain());
    }

    /// A room of one row is all seam when the list does not fit, and a room of none draws nothing.
    #[test]
    fn a_one_row_room_spends_its_only_row_on_the_count() {
        let l = window(content(3), W, 0, 1);
        assert_eq!(l.lines.len(), 1);
        assert_eq!(
            plain(&l.lines)[0].trim(),
            "… 9 line(s) out of view · pgdn scrolls"
        );
        let none = window(content(3), W, 0, 0);
        assert!(none.lines.is_empty());
        assert_eq!(none.seam, None);
        assert_eq!(none.len, 9, "the content's height, not the window's");
    }

    /// Thirty Downs walk the cursor off the bottom and the window follows it, reading the row the stop
    /// was drawn on.
    #[test]
    fn the_cursor_scrolls_itself_into_view() {
        let c = content(40);
        let room = 10;
        let mut scroll = 0;
        for cursor in 0..=30 {
            scroll = scroll_into_view(scroll, room, c.lines.len(), c.row_of(cursor));
        }
        assert!(scroll > 0, "the window never followed the cursor");
        let drawn = plain(&window(c.clone(), W, scroll, room).lines);
        assert!(drawn.iter().any(|l| l == "item 30"), "{drawn:?}");
        // And back up: the window moves as little as it can.
        assert_eq!(
            scroll_into_view(scroll, room, c.lines.len(), c.row_of(0)),
            2
        );
        // A cursor past the end lands on the last stop rather than nowhere.
        assert_eq!(c.row_of(999), *c.stop_rows.last().unwrap());
        assert_eq!(PaneLines::new().row_of(3), 0);
    }

    /// A click is read against the rows the last draw recorded, through the scroll, and a click under a
    /// short list or on a detail row is nothing.
    #[test]
    fn a_click_lands_on_the_stop_drawn_on_that_row_and_nowhere_else() {
        let l = window(content(40), W, 5, 10);
        // Content row 6 is stop 2 ("item 2"), drawn at pane row 1 after a scroll of 5.
        assert_eq!(plain(&l.lines)[1], "item 2");
        assert_eq!(l.stop_at(1), Some(2));
        assert_eq!(l.stop_at(2), None, "a detail row is not a stop");
        assert_eq!(l.stop_at(10), None, "below the window");
        let short = window(content(1), W, 0, 10);
        assert_eq!(short.stop_at(2), Some(0));
        assert_eq!(short.stop_at(8), None, "a pad under a short list");
    }

    #[test]
    fn a_split_row_pushes_its_right_half_to_the_edge_or_drops_it() {
        let r = split_row(Line::raw("left"), Line::raw("right"), 20);
        assert_eq!(r.plain(), "left           right");
        assert_eq!(r.width(), 20);
        assert_eq!(
            split_row(Line::raw("left"), Line::raw("right"), 10).plain(),
            "left"
        );
        // The highlight stays on the left half.
        let r = split_row(picked(Line::raw("a"), true), Line::raw("b"), 10);
        assert!(
            r.spans[0]
                .style
                .attrs
                .contains(crate::style::Attrs::REVERSE)
        );
        assert!(
            !r.spans[2]
                .style
                .attrs
                .contains(crate::style::Attrs::REVERSE)
        );
    }
}
