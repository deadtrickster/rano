//! The panel's widget: one conversation beside the text, as the top edge that names it, the rows
//! that were said, the composer's line and the keys it is listening for.
//!
//! The rows come in already laid out ([`crate::panel`] builds them at the width the panel has), so
//! this windows them into the room it is given: one row at the top for the session, one at the foot
//! for the composer, one above that for the key row, and a seam counting what the window could not
//! draw. The scroll it used is handed back, because the host keeps it and the keys move it.

use crate::render::{Buffer, Line, Rect, Span, Style, Widget};
use crate::style::Role;

/// A panel drawn in an area, and what the draw settled: where the caret went and how far the
/// conversation is scrolled.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Drawn {
    /// The composer's cursor cell, when the panel holds the keyboard.
    pub cursor: Option<(u16, u16)>,
    /// Content rows hidden above the top, clamped: the host stores it back.
    pub scroll: usize,
}

/// What the panel draws this frame: the session and its state, the conversation, the composer, the
/// keys. Plain facts, in rano's words — no socket, no daemon and no session type here.
pub struct View<'a> {
    /// The session's name; empty until one is attached.
    pub title: String,
    /// The state's two or three words, and the role they are read in.
    pub state: &'a str,
    pub state_role: Role,
    /// The folder the conversation belongs to, at the right of the top edge.
    pub folder: String,
    /// A folder the focused buffer moved to, when following it is the operator's to say.
    pub moved: Option<String>,
    /// The conversation at this width, oldest first.
    pub body: &'a [Line],
    /// Content rows hidden above the top; [`View::follow`] pins the end instead.
    pub scroll: usize,
    pub follow: bool,
    /// The composer's line, and its cursor in chars.
    pub input: &'a str,
    pub cursor: usize,
    /// The panel holds the keyboard: the composer is live and the key row is drawn.
    pub focus: bool,
    /// The key row, as the bar's own `(key, title)` pairs.
    pub hints: &'a [(String, String)],
}

/// A widget, so a host can put the panel in a frame; [`View::draw`] is the same rows with the
/// caret the frame cannot carry.
impl Widget for View<'_> {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        self.draw(area, buf);
    }
}

impl View<'_> {
    /// Draw into `area`, answering where the caret is and what scroll was used.
    pub fn draw(&self, area: Rect, buf: &mut Buffer) -> Drawn {
        let area = area.intersection(buf.area());
        if area.is_empty() {
            return Drawn::default();
        }
        buf.fill(area, &Style::new());
        let w = area.width as usize;
        let h = area.height as usize;
        // The top edge names the session; below it the conversation, then the composer, then the
        // key row. Two rows is the least that is anything at all.
        if h < 2 {
            buf.set_line(area.x, area.y, &self.top_edge(w), w);
            return Drawn::default();
        }
        let hint_row = self.focus && !self.hints.is_empty() && h >= 5;
        let room = h - 1 - 1 - usize::from(hint_row);
        let composer_row = room + 1;
        let hint_row = composer_row + 1;

        // ---- the top edge: the session, its state, and the folder it follows ----
        buf.set_line(area.x, area.y, &self.top_edge(w), w);

        // ---- the conversation, windowed ----
        let len = self.body.len();
        let shown = if len > room {
            room.saturating_sub(1)
        } else {
            len
        };
        let scroll = if self.follow {
            len.saturating_sub(shown)
        } else {
            self.scroll.min(len.saturating_sub(shown))
        };
        let below = len.saturating_sub(scroll + shown);
        let mut row = 1usize;
        if scroll > 0 && room > 0 {
            buf.set_line(
                area.x,
                area.y + row as u16,
                &seam(scroll, "above", w),
                area.width as usize,
            );
            row += 1;
        }
        for line in self.body.iter().skip(scroll).take(shown) {
            if row >= composer_row {
                break;
            }
            buf.set_line(area.x, area.y + row as u16, line, w);
            row += 1;
        }
        if below > 0 && row < composer_row {
            buf.set_line(
                area.x,
                area.y + row as u16,
                &seam(below, "below", w),
                area.width as usize,
            );
            row += 1;
        }
        while row < composer_row {
            buf.set_line(area.x, area.y + row as u16, &Line::raw(" ".repeat(w)), w);
            row += 1;
        }

        // ---- the composer, and the caret in it ----
        let (line, at) = self.composer(w);
        buf.set_line(area.x, area.y + composer_row as u16, &line, w);
        let cursor = self
            .focus
            .then(|| (area.x + at as u16, area.y + composer_row as u16));

        // ---- the keys, from the map the panel mode is bound in ----
        if hint_row < h {
            buf.set_line(area.x, area.y + hint_row as u16, &self.hint_line(w), w);
        }
        Drawn { cursor, scroll }
    }

    /// `▌ title · state` on the left, the folder on the right; a folder that moved is said instead
    /// of the one in use, because following it is the thing to do next.
    fn top_edge(&self, w: usize) -> Line {
        let title = if self.title.is_empty() {
            "letibot".to_string()
        } else {
            self.title.clone()
        };
        let mut spans = vec![
            Span::role("▌ ", Role::UserAccent),
            Span::role(
                crate::width::text::truncate(&title, w.saturating_sub(3)),
                Role::Strong,
            ),
            Span::role(format!(" · {}", self.state), self.state_role),
        ];
        let left = spans.iter().map(Span::width).sum::<usize>();
        let right = match &self.moved {
            Some(folder) => format!("moved to {folder} · alt-f follows"),
            None => self.folder.clone(),
        };
        let role = if self.moved.is_some() {
            Role::Attention
        } else {
            Role::Faint
        };
        if !right.is_empty() && w > left + 2 {
            let room = w - left - 1;
            let right = left_cut(&right, room);
            let pad = room.saturating_sub(right.chars().count());
            spans.push(Span::raw(" ".repeat(pad + 1)));
            spans.push(Span::styled(right, Style::of(role)));
        }
        Line::new(spans)
    }

    /// The composer's row and the caret's column in it: the line shows the tail the caret is in
    /// when the text is longer than the row.
    fn composer(&self, w: usize) -> (Line, usize) {
        let lead = if self.focus { "› " } else { "  " };
        let room = w.saturating_sub(lead.chars().count());
        let chars: Vec<char> = self.input.chars().collect();
        let at = self.cursor.min(chars.len());
        // Keep the caret on screen: the window starts at it when it runs off the right edge.
        let start = if room == 0 {
            0
        } else {
            at.saturating_sub(room.saturating_sub(1))
        };
        let shown: String = chars.iter().skip(start).take(room).collect();
        let lead_role = if self.focus {
            Role::UserAccent
        } else {
            Role::Faint
        };
        (
            Line::new(vec![
                Span::role(lead, lead_role),
                Span::styled(shown, Style::of(Role::Plain)),
            ]),
            lead.chars().count() + at - start,
        )
    }

    /// The panel's keys on one row, as the bar draws them: the chord, then its title.
    fn hint_line(&self, w: usize) -> Line {
        let mut spans: Vec<Span> = Vec::new();
        let mut used = 0usize;
        for (key, title) in self.hints {
            let room = w.saturating_sub(used + 1);
            if room < key.chars().count() + 2 {
                break;
            }
            if used > 0 {
                spans.push(Span::raw(" "));
                used += 1;
            }
            let key = crate::width::text::truncate(key, room);
            used += key.chars().count();
            spans.push(Span::styled(key, Style::of(Role::Strong).reverse()));
            let room = w.saturating_sub(used + 1);
            let tag = crate::width::text::truncate(title, room);
            used += 1 + tag.chars().count();
            spans.push(Span::raw(format!(" {tag}")));
        }
        while used < w {
            spans.push(Span::raw(" "));
            used += 1;
        }
        Line::new(spans)
    }
}

/// `s` cut to `cols` columns at its left end, which is the end of a path that says nothing:
/// `…/rano/src` beats `/home/dead/Project`.
fn left_cut(s: &str, cols: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= cols {
        return s.to_string();
    }
    if cols == 0 {
        return String::new();
    }
    let tail: String = chars[chars.len() - (cols - 1)..].iter().collect();
    format!("…{tail}")
}

/// `… 12 above`: the rows the window could not draw, and which side they are on.
fn seam(count: usize, side: &str, w: usize) -> Line {
    let text = format!("… {count} {side}");
    let mut spans = vec![Span::styled(
        crate::width::text::truncate(&text, w),
        Style::of(Role::Faint),
    )];
    let used = spans[0].width();
    if used < w {
        spans.push(Span::raw(" ".repeat(w - used)));
    }
    Line::new(spans)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::Buffer;

    fn blank_panel<'a>(hints: &'a [(String, String)]) -> View<'a> {
        View {
            title: "the one".to_string(),
            state: "idle",
            state_role: Role::Faint,
            folder: "/w/project".to_string(),
            moved: None,
            body: &[],
            scroll: 0,
            follow: true,
            input: "",
            cursor: 0,
            focus: true,
            hints,
        }
    }

    /// The view drawn in a `w`×`h` frame, as plain rows.
    fn rows(v: &View<'_>, w: u16, h: u16) -> Vec<String> {
        let mut buf = Buffer::empty(Rect::new(0, 0, w, h));
        v.draw(Rect::new(0, 0, w, h), &mut buf);
        buf.to_plain_lines()
    }

    /// The top edge names the session, its state and the folder, and the foot is the composer.
    #[test]
    fn the_top_edge_names_the_session_and_the_foot_is_the_composer() {
        let body = vec![Line::raw("the answer")];
        let mut view = blank_panel(&[]);
        view.body = &body;
        let rows = rows(&view, 40, 8);
        assert!(rows[0].contains("the one"), "{rows:?}");
        assert!(rows[0].contains("idle"), "{rows:?}");
        assert!(rows[0].contains("/w/project"), "the folder: {rows:?}");
        assert_eq!(rows[1].trim_end(), "the answer");
        assert!(rows[7].contains('›'), "the composer's foot: {rows:?}");
    }

    /// A conversation longer than the room is windowed from the end when it is following, and the
    /// row it could not draw says how many are out of view.
    #[test]
    fn a_long_conversation_follows_its_end_and_counts_what_is_out_of_view() {
        let body: Vec<Line> = (0..30).map(|i| Line::raw(format!("row {i}"))).collect();
        let mut view = blank_panel(&[]);
        view.body = &body;
        let drawn = rows(&view, 30, 8);
        assert!(drawn[1].contains("above"), "a seam at the head: {drawn:?}");
        assert!(drawn.iter().any(|r| r.trim_end() == "row 29"), "{drawn:?}");
        assert!(!drawn.iter().any(|r| r.trim_end() == "row 0"), "{drawn:?}");

        // Scrolled to the top instead: the seam moves to the foot.
        view.follow = false;
        view.scroll = 0;
        let drawn = rows(&view, 30, 8);
        assert_eq!(drawn[1].trim_end(), "row 0", "{drawn:?}");
        assert!(drawn.iter().any(|r| r.contains("below")), "{drawn:?}");
    }

    /// The caret is where the composer's cursor is, and the text it is in is the tail when the row
    /// is too narrow to hold all of it.
    #[test]
    fn the_caret_sits_where_the_composer_says_and_the_tail_is_what_shows() {
        let mut view = blank_panel(&[]);
        view.input = "a long question that does not fit in twenty columns";
        view.cursor = view.input.chars().count();
        let rect = Rect::new(0, 0, 24, 6);
        let mut buf = Buffer::empty(rect);
        let drawn = view.draw(rect, &mut buf);
        let (x, y) = drawn.cursor.expect("the panel has the keyboard");
        assert_eq!(y, 5, "the composer's row");
        assert!(x < 24, "inside the row: {x}");
        let rows = buf.to_plain_lines();
        assert!(
            rows[5].trim_end().ends_with('s'),
            "the caret is at the end: {rows:?}"
        );

        // Without the keyboard there is no caret.
        view.focus = false;
        let mut buf = Buffer::empty(rect);
        assert_eq!(view.draw(rect, &mut buf).cursor, None);
    }

    /// The keys the panel is listening for are the bar's own entries, on one row above the
    /// composer.
    #[test]
    fn the_key_row_is_the_maps_own_entries() {
        let hints = vec![
            ("enter".to_string(), "Send".to_string()),
            ("esc".to_string(), "Back".to_string()),
        ];
        let view = blank_panel(&hints);
        let keys = rows(&view, 40, 8);
        assert!(keys[7].contains("enter"), "{keys:?}");
        assert!(keys[7].contains("Send"), "{keys:?}");
        assert!(keys[7].contains("esc"), "{keys:?}");
        // Without the keyboard there is no key row; the composer takes the foot.
        let mut view = blank_panel(&hints);
        view.focus = false;
        view.input = "a draft";
        view.cursor = 7;
        let drawn = rows(&view, 40, 8);
        assert!(
            !drawn[7].contains("esc"),
            "no keys while the text has it: {drawn:?}"
        );
        assert!(
            drawn[7].contains("a draft"),
            "the composer's foot: {drawn:?}"
        );
    }

    /// A moved folder is said on the top edge instead of the one in use: following it is the next
    /// thing to do, and the row is where the operator is looking.
    #[test]
    fn a_moved_folder_is_named_with_the_key_that_follows_it() {
        let mut view = blank_panel(&[]);
        view.moved = Some("/w/other".to_string());
        let rows = rows(&view, 60, 6);
        assert!(rows[0].contains("/w/other"), "{rows:?}");
        assert!(rows[0].contains("alt-f"), "{rows:?}");
    }
}
