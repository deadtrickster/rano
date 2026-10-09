//! **The log pane**: the tail of a file that is still being written.
//!
//! TODO.md §20.3 F and §20.4 — the pane half of log mode. The *bookkeeping* half
//! is [`crate::logtail::LogTail`], and that is where §20.4's budget lives: it
//! holds a file's line count, a sparse row → byte index and its block boundaries
//! and **no rows at all**, so a log nobody is looking at costs those and nothing
//! else. This widget is the other side of the same line — it draws the rows that
//! have been read for the window on screen, and the host drops them when the pane
//! goes away.
//!
//! ## What the host supplies, and what it must not
//!
//! [`LogPane`] is a view model like every other widget here: rows, a count, a
//! scroll. It reads no file and knows no path. `LogTail::last_rows` is what a host
//! calls to fill it, and it is deliberately outside this module — everything in
//! `agent` is pure drawing, so no widget here touches the filesystem (see
//! `agent/mod.rs`).
//!
//! ## The structure §20.4 pays for
//!
//! [`LogRow::continues`] is [`crate::logblocks`]' verdict, carried into the pane
//! so a stack trace reads as one thing: a continuation row is drawn against a dim
//! `│` instead of looking like a record of its own. That is the visible payoff of
//! having worked the block boundaries out while nobody was looking, and it is the
//! whole of what this widget does with them — folding is §20.5's *later* step, and
//! a block a growing log has not finished writing is not a block to hide rows in.
//!
//! ## Numbering
//!
//! Every row is numbered **by the file**, not by the window: `first_row` is the
//! file row of `rows[0]` and `total` is the file's row count, both from the same
//! bookkeeping pass. A pane that numbered its own window from 1 would be the lie
//! TODO.md §20.3 C exists to remove, and here there is no reason to tell it: the
//! count is already known and costs nothing.

use crate::agent::pane;
use crate::agent::text::clean_line;
use crate::render::{Line, Widget};

/// One row of the pane.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LogRow {
    /// The row's text, as the file has it.
    pub text: String,
    /// **This row continues the block above it** — a stack frame, a wrapped
    /// continuation — under [`crate::logblocks`]' one rule.
    pub continues: bool,
}

/// The log pane's view model: a window of rows from a file being written.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LogPane {
    /// The file's name, as the host knows it. Foreign text.
    pub name: String,
    /// The rows read for this window, oldest first. **The host drops these when
    /// the pane is not shown**, which is §20.4's budget in one sentence.
    pub rows: Vec<LogRow>,
    /// The FILE row number of `rows[0]`, 1-based.
    pub first_row: u64,
    /// Rows in the file, from the same pass that produced `first_row`.
    pub total: u64,
    /// Rows scrolled back from the tail. The host's state; clamped when drawn.
    pub scroll: usize,
    /// The file is still being written and the view holds its end.
    pub following: bool,
}

impl LogPane {
    /// The view, exactly `room` rows or fewer, and the scroll actually used.
    ///
    /// The tail shows by default — the newest rows are the reason a log pane
    /// exists — and `scroll` walks back from there, clamped to what has actually
    /// been read, so a host that kept a scroll across a shorter read gets a
    /// window rather than a blank pane.
    pub fn lines(&self, room: usize) -> (Vec<Line>, usize) {
        let mut out = vec![pane::title(format!("log — {}", clean_line(&self.name)))];
        let mut meta = format!("    {} lines", self.total);
        if self.rows.is_empty() {
            meta.push_str(" · nothing read yet");
        } else {
            meta.push_str(&format!(
                " · showing {}..{}",
                self.first_row,
                self.first_row + self.rows.len() as u64 - 1
            ));
        }
        if self.following {
            meta.push_str(" · following");
        }
        out.push(pane::faint(meta));
        out.push(Line::default());

        let visible = room.saturating_sub(out.len()).max(1);
        let scroll = self.scroll.min(self.rows.len().saturating_sub(visible));
        let end = self.rows.len() - scroll;
        let start = end.saturating_sub(visible);
        // The gutter is as wide as the file's last row needs, so a 4 000 000-row
        // log's numbers do not push the text sideways.
        let w = self.total.to_string().len().max(4);
        for (i, r) in self.rows[start..end].iter().enumerate() {
            let n = self.first_row + (start + i) as u64;
            let marker = if r.continues { "│" } else { " " };
            out.push(Line::raw(format!(
                "{n:>w$} {marker} {}",
                clean_line(&r.text)
            )));
        }
        (out, scroll)
    }
}

impl Widget for LogPane {
    fn render(&self, area: crate::render::Rect, buf: &mut crate::render::Buffer) {
        crate::agent::draw_lines(&self.lines(area.height as usize).0, area, buf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::testing::{drawn, plain};

    fn row(text: &str, continues: bool) -> LogRow {
        LogRow {
            text: text.into(),
            continues,
        }
    }

    /// A pane of `n` rows ending at file row 4000, as a tail of a big log reads.
    fn tail_pane(n: usize) -> LogPane {
        let first_row = 4000 - n as u64 + 1;
        LogPane {
            name: "server.log".into(),
            rows: (0..n)
                .map(|i| row(&format!("row {}", first_row + i as u64), false))
                .collect(),
            first_row,
            total: 4000,
            scroll: 0,
            following: true,
        }
    }

    /// The tail shows by default, and scrolling back stops at the window's
    /// beginning rather than running off it — letibot's `job_out_lines` rule,
    /// which this pane keeps because it is the same pane.
    #[test]
    fn the_tail_shows_and_scrolling_back_stops_at_the_beginning() {
        let p = tail_pane(50);
        let (rows, scroll) = p.lines(12);
        assert_eq!(scroll, 0, "the tail is the default");
        let body: Vec<String> = plain(&rows).into_iter().skip(3).collect();
        assert_eq!(body.len(), 9, "a window of nine: {body:?}");
        assert!(body[0].contains("row 3992"), "{body:?}");
        assert!(body.last().unwrap().contains("row 4000"), "{body:?}");

        let p = LogPane { scroll: 999, ..p };
        let (rows, scroll) = p.lines(12);
        assert_eq!(scroll, 50 - 9, "clamped at the beginning of the window");
        let body: Vec<String> = plain(&rows).into_iter().skip(3).collect();
        assert!(body[0].contains("row 3951"), "{body:?}");
    }

    /// **The numbers are the FILE's.** A window of rows 3951..4000 is numbered
    /// 3951..4000, not 1..50, and the header says how many rows the file has.
    #[test]
    fn every_number_is_the_files_own() {
        let (rows, _) = tail_pane(50).lines(12);
        let text = plain(&rows);
        assert!(text[1].contains("4000 lines"), "{:?}", text[1]);
        assert!(text[1].contains("4000"), "{:?}", text[1]);
        let body: Vec<String> = text.into_iter().skip(3).collect();
        assert_eq!(body.len(), 9, "a window of nine: {body:?}");
        assert!(body[0].starts_with("3992 "), "{:?}", body[0]);
        assert!(body.last().unwrap().starts_with("4000 "), "{body:?}");
        // And scrolled to the top of what was read, the same arithmetic on the
        // other end — so the numbers are the file's wherever the window sits.
        let p = LogPane {
            scroll: 999,
            ..tail_pane(50)
        };
        let body: Vec<String> = plain(&p.lines(12).0).into_iter().skip(3).collect();
        assert!(body[0].starts_with("3951 "), "{:?}", body[0]);
        assert!(body.last().unwrap().starts_with("3959 "), "{body:?}");
    }

    /// A continuation row is drawn hanging off its block, which is the whole of
    /// what §20.4's boundaries buy a reader.
    #[test]
    fn a_continuation_row_is_marked_and_a_record_is_not() {
        let p = LogPane {
            name: "trace.log".into(),
            rows: vec![
                row("ERROR boom", false),
                row("  at frame::one", true),
                row("  at frame::two", true),
                row("next record", false),
            ],
            first_row: 1,
            total: 4,
            scroll: 0,
            following: false,
        };
        let text = plain(&p.lines(10).0);
        assert!(text[3].contains("ERROR boom"), "{text:?}");
        assert!(
            text[3].contains("  ERROR boom"),
            "no marker column: {text:?}"
        );
        assert!(text[4].contains("│   at frame::one"), "{text:?}");
        assert!(text[5].contains("│   at frame::two"), "{text:?}");
        assert!(
            !text[6].contains('│'),
            "a record is not a continuation: {text:?}"
        );
    }

    /// An empty window is said rather than drawn as blank rows.
    ///
    /// The pane is content-driven, as every height-follows-content widget here
    /// is: `lines(room)` gives `room` rows or fewer, and a host lays out a screen
    /// from the count it gets back.
    #[test]
    fn an_empty_window_says_so() {
        let p = LogPane {
            name: "fresh.log".into(),
            total: 0,
            ..LogPane::default()
        };
        let text = plain(&p.lines(6).0);
        assert_eq!(text[0], "log — fresh.log");
        assert!(text[1].contains("0 lines"), "{text:?}");
        assert!(text[1].contains("nothing read yet"), "{text:?}");
        assert_eq!(text.len(), 3, "no rows, so no body: {text:?}");
    }

    /// A pane that is not following says so, because "this stopped" and "this is
    /// still arriving" are the two things a reader needs to tell apart.
    #[test]
    fn following_is_only_said_when_it_is_true() {
        let p = tail_pane(5);
        assert!(plain(&p.lines(8).0)[1].contains("following"));
        let p = LogPane {
            following: false,
            ..p
        };
        assert!(!plain(&p.lines(8).0)[1].contains("following"));
    }

    /// **No file, no rows, no scroll — the widget still draws its frame**, which
    /// is what a host gets for a pane it has opened but not yet filled.
    #[test]
    fn a_blank_model_draws_a_titled_empty_pane() {
        let rows = drawn(&LogPane::default(), 40, 5);
        assert_eq!(rows[0].trim_end(), "log —");
        assert_eq!(rows.len(), 5);
        assert!(rows[3].trim().is_empty());
    }
}
