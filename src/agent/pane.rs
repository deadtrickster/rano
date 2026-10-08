//! **A pane: a list the person moves through**, drawn in place of the conversation —
//! the shape letibot's todos, subagents, jobs, queue and picker panes share.
//!
//! Ported from letibot's `ui/panes/*.rs` and `app/panes.rs` (`pane_window`,
//! `scroll_into_view`, the `*_stop_rows` records and `*_stop_at_row`). The shape is:
//!
//! - **Content** ([`PaneLines`]): every row the pane has, top to bottom, and **the row
//!   each stop was drawn on** — a stop is a row the cursor can rest on (a job, a todo, a
//!   `finished (N)` group), among rows it cannot (a title, a job's detail line, a footer).
//! - **A window** ([`window`]): the slice of the content that fits the room, with the
//!   scroll clamped to the last screenful.
//! - **The host owns the scroll and the cursor.** Both are state that outlives a frame
//!   and is moved by keys, so they live with the host; this module takes them in, clamps
//!   them against what it actually drew, and hands the clamped values back
//!   ([`PaneLayout::scroll`]) for the host to store.
//!
//! # Why the stop rows are a record and not arithmetic
//!
//! letibot's own words, from the todos pane: *"mouse doesnt click"* — a click on the add
//! row computing a negative index, because the click handler did arithmetic over the list
//! (`line - header`) while the pane drew something else. **The pane records where each stop
//! landed while drawing**, and both the arrows (scroll the cursor's row into view) and a
//! click (which stop is on screen row `y`) read that record, so the drawing and the hit
//! test cannot disagree about where a row is. See [`PaneLines::row_of`] and
//! [`PaneLayout::stop_at`].
//!
//! # No seam
//!
//! letibot's panes draw no "N more below" seam: a pane is a whole screen of its own, its
//! footer names the keys (`up/down scrolls`), and the window is a plain slice. That is
//! kept. The cards that do say what is out of view (the decision card, a tool row's
//! payload window) carry their own seams.

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

    /// **The pane row the stop at the cursor was DRAWN on**, read out of the record the
    /// pane wrote while drawing, never arithmetic over the table it drew from.
    ///
    /// The cursor is clamped first: the table can move under it (a job settling, a queue
    /// entry arriving), and an arrow pressed against a shorter list must land on a row
    /// rather than on an index that no longer exists. `0` when there are no stops.
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
    /// The visible slice, at most `room` rows.
    pub lines: Vec<Line>,
    /// The scroll actually used — rows hidden above the top, clamped. Store it back.
    pub scroll: usize,
    /// The rows the window had (letibot's `pane_room`).
    pub room: usize,
    /// The rows the content has (letibot's `pane_len`), for PageDown's clamp.
    pub len: usize,
    /// Where each stop landed, in content rows (not screen rows): see [`PaneLines`].
    pub stop_rows: Vec<usize>,
}

impl PaneLayout {
    /// **The stop drawn on row `y` of the pane's area, or nothing** — letibot's
    /// `todo_stop_at_row` / `queue_stop_at_row`.
    ///
    /// `y` is relative to the pane's first drawn row: a host measures a click's screen row
    /// against where it put the pane (letibot's `todos_pane_top` — the session header sits
    /// above the pane when the frame is tall enough for one, and it is not a row of this
    /// pane). Guarded on the WINDOW: a row below the last drawn row is not a row anybody is
    /// looking at, and a click into the blank space under a short list moves nothing.
    pub fn stop_at(&self, y: usize) -> Option<usize> {
        if y >= self.room.min(self.lines.len()) {
            return None;
        }
        let row = y + self.scroll;
        self.stop_rows.iter().position(|r| *r == row)
    }
}

impl Widget for PaneLayout {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        crate::agent::draw_lines(&self.lines, area, buf);
    }
}

/// **The visible slice of a pane**, and the numbers the scroll keys need.
///
/// Clamped here rather than at the keypress: the key handler does not know how tall the
/// terminal is or how many rows the pane has, and a scroll clamped against a stale height
/// scrolls past the end and shows a blank screen the operator has to page back from.
///
/// `scroll` counts rows hidden **above the top** — this is literally `skip(scroll)` — so
/// Up DECREASES it. letibot had the sign inverted once on a listing whose footer said
/// `up/down scrolls`, and leticl the same: *"called with the top-origin sign, ↑ walked
/// toward the END while the hint bar said otherwise."*
pub fn window(content: PaneLines, scroll: usize, room: usize) -> PaneLayout {
    let len = content.lines.len();
    // The last screenful is the furthest anything scrolls: past that is blank rows, which
    // is not a place to be.
    let max = len.saturating_sub(room);
    let scroll = scroll.min(max);
    PaneLayout {
        lines: content.lines.into_iter().skip(scroll).take(room).collect(),
        scroll,
        room,
        len,
        stop_rows: content.stop_rows,
    }
}

/// **Keep the cursor on screen after an arrow moved it**: the scroll that shows content
/// row `row` in a window of `room` rows, moving as little as it can.
///
/// An arrow that walks the selection out of the window otherwise looks like a key that
/// does nothing — the operator's *"subagents panel doesnt scroll"*: a child below the fold
/// looked unreachable. `room == 0` (never drawn yet) leaves the scroll alone.
pub fn scroll_into_view(scroll: usize, room: usize, row: usize) -> usize {
    if room == 0 {
        scroll
    } else if row < scroll {
        row
    } else if row >= scroll + room {
        row + 1 - room
    } else {
        scroll
    }
}

/// The cursor's mark: `▸` on the row Enter takes, a space elsewhere — the same ladder the
/// decision card draws, so *the mark IS the thing Enter takes*.
pub fn mark(picked: bool) -> &'static str {
    if picked { "▸" } else { " " }
}

/// The picked row's highlight: inverse video rather than another colour, because a
/// highlight that is a second hue reads as a second kind of thing rather than as "this
/// one". Nothing under `Palette::None`, where the `▸` is what survives.
pub fn picked(l: Line, on: bool) -> Line {
    if on {
        let st = l.style.clone().reverse();
        Line { style: st, ..l }
    } else {
        l
    }
}

/// `left`, then `right` pushed to the right edge — or `left` alone, cut to `w`, when the
/// two do not fit with two columns between them.
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
    use crate::agent::testing::plain;

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

    /// letibot `app/tests/scroll.rs::a_pane_taller_than_the_screen_scrolls_to_its_end`, at
    /// the window: PageDown past the end is clamped to the last screenful, and the final row
    /// is still drawn.
    #[test]
    fn a_pane_taller_than_the_screen_scrolls_to_its_end() {
        let top = window(content(30), 0, 10);
        assert_eq!(plain(&top.lines)[0], "a pane");
        assert_eq!(top.len, 63);
        let end = window(content(30), 10_000, 10);
        assert_eq!(end.scroll, 63 - 10, "clamped to the last screenful");
        assert_eq!(
            plain(&end.lines).last().unwrap().trim(),
            "esc closes · up/down scrolls"
        );
        assert!(!plain(&end.lines).iter().any(|l| l == "item 0"));
        // A pane shorter than its room never scrolls at all.
        assert_eq!(window(content(2), 5, 40).scroll, 0);
    }

    /// letibot `app/tests/scroll.rs::the_cursor_scrolls_itself_into_view` and
    /// `the_arrows_scroll_the_subagent_cursor_into_view`: thirty Downs walk the cursor off
    /// the bottom, and the window follows it — reading the row the stop was DRAWN on.
    #[test]
    fn the_cursor_scrolls_itself_into_view() {
        let c = content(40);
        let room = 10;
        let mut scroll = 0;
        for cursor in 0..=30 {
            scroll = scroll_into_view(scroll, room, c.row_of(cursor));
        }
        assert!(scroll > 0, "the window never followed the cursor");
        let shown = plain(&window(c.clone(), scroll, room).lines);
        assert!(shown.iter().any(|l| l == "item 30"), "{shown:?}");
        // And back up: the window moves as little as it can.
        assert_eq!(scroll_into_view(scroll, room, c.row_of(0)), 2);
        // A cursor past the end lands on the last stop rather than nowhere.
        assert_eq!(c.row_of(999), *c.stop_rows.last().unwrap());
        assert_eq!(PaneLines::new().row_of(3), 0);
    }

    /// letibot `app/todos.rs::todo_stop_at_row`'s rule, which answered *"mouse doesnt
    /// click"*: a click is read against the rows the last draw recorded, through the scroll,
    /// and a click under a short list or on a detail row is nothing.
    #[test]
    fn a_click_lands_on_the_stop_drawn_on_that_row_and_nowhere_else() {
        let l = window(content(40), 5, 10);
        // Content row 6 is stop 2 ("item 2"), drawn at pane row 1 after a scroll of 5.
        assert_eq!(plain(&l.lines)[1], "item 2");
        assert_eq!(l.stop_at(1), Some(2));
        assert_eq!(l.stop_at(2), None, "a detail row is not a stop");
        assert_eq!(l.stop_at(10), None, "below the window");
        let short = window(content(1), 0, 10);
        assert_eq!(short.stop_at(2), Some(0));
        assert_eq!(short.stop_at(8), None, "the blank space under a short list");
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
