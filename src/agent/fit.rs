//! What fits on a short screen: the rows under the conversation — the cards, the disclosures, the
//! composer and its box, the hint bar — compete for a screen that may not hold them all, and this
//! decides which give way, and in what order, before anything is drawn.

/// What the ladder is told: the screen's height and what each row would cost.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FitInput {
    /// The terminal's height.
    pub h: usize,
    /// The rows the composer wants.
    pub composer: usize,
    /// The open card's rows, all of them.
    pub card: usize,
    /// Rows pinned under the card.
    pub pinned: usize,
    /// A notice is waiting to be shown.
    pub notice: bool,
    /// The stuck-turn disclosure has something to say.
    pub stuck: bool,
    /// A program is running in the pane off-screen.
    pub pane: bool,
    /// The completion row is reserved.
    pub completion_slot: bool,
    /// The link's lines, never given up.
    pub link: usize,
    /// A stop's lines, never given up.
    pub stopping: usize,
    /// Counters are alarmed, so an unboxed composer still needs their row.
    pub alarmed: bool,
}

/// What the ladder decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fit {
    /// The composer's rows.
    pub rows: usize,
    pub hint: bool,
    pub show_notice: bool,
    pub show_stuck: bool,
    pub show_pane: bool,
    pub show_status: bool,
    /// The composer is drawn in its box.
    pub boxed: bool,
    /// How many of the card's rows are shown.
    pub content_rows: usize,
}

/// The ladder: start with everything, and give up the most expendable row until the whole thing fits
/// with a line of transcript left over.
pub fn fit_ladder(i: &FitInput) -> Fit {
    // The ladder deletes the most expendable row first and stops as soon as the whole thing fits with
    // a line of transcript left over: draining the chrome from the front would eat a box's top border
    // and leave the bottom, and a container with one side is worse than none.
    let mut rows = i.composer;
    let mut hint = true;
    let mut show_notice = i.notice;
    let mut show_stuck = i.stuck;
    // The pane's line is news too, and given up last of the news: a program running off-screen is a
    // fact the reader can act on and cannot get elsewhere, so the composer's own rows go first.
    let mut show_pane = i.pane;
    // The turn's row is reserved, not conditional: the row exists only while a turn does, and the
    // moment one starts every row above it moves — so a reserved line costs one row on an idle
    // session and buys a frame that does not move. The rows around it stay conditional, since each is
    // news, and a reserved line for news is furniture.
    let mut show_status = true;
    let mut boxed = true;
    // Only the content viewport shrinks: shrinking the whole card trims from the END, which is the
    // ladder, the deadline and the hint — the last rows the reader needed given up first.
    let mut content_rows = i.card;
    loop {
        // A windowed content costs one row for the seam that says so.
        let seam = usize::from(content_rows > 0 && i.card > content_rows);
        let n = content_rows
            + seam
            + i.pinned
            + usize::from(show_stuck)
            + usize::from(show_pane)
            + usize::from(show_status)
            + usize::from(show_notice)
            // Counted by the slot, not by the text in it: the row's height is a fact about the
            // composer's line, so the transcript's budget does not change with the candidates.
            + usize::from(i.completion_slot)
            + i.link
            // Counted in full and never sacrificed: a stop's sentence is the one thing on this
            // screen the reader must not have to go looking for, and dropping it would make the head
            // wait in silence. The link's lines count the same way, for the same reason.
            + i.stopping
            // Unboxed costs one row only when there is an alarm to show: the counters move off the
            // border onto a line of their own, and a clean head owes that row to the transcript.
            + if boxed { 2 } else { usize::from(i.alarmed) }
            + rows
            + usize::from(hint);
        if n < i.h {
            break;
        }
        if hint {
            hint = false;
        } else if show_notice {
            show_notice = false;
        } else if rows > 1 {
            rows -= 1;
        } else if show_stuck {
            show_stuck = false;
        } else if show_pane {
            // The pane's line goes before the turn's row and after the stuck disclosure: a reader can
            // still get it another way — `!term` finds the program whether this head said so or not.
            show_pane = false;
        } else if show_status {
            // The turn's row goes before the box does, and after the stuck disclosure: what a reader
            // loses least by losing is the aside about silence rather than the line saying work is
            // happening.
            show_status = false;
        } else if boxed {
            boxed = false;
        } else if content_rows > 0 {
            content_rows -= 1;
        } else {
            break;
        }
    }
    Fit {
        rows,
        hint,
        show_notice,
        show_stuck,
        show_pane,
        show_status,
        boxed,
        content_rows,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(h: usize) -> FitInput {
        FitInput {
            h,
            composer: 3,
            card: 4,
            pinned: 0,
            notice: true,
            stuck: true,
            pane: true,
            completion_slot: false,
            link: 0,
            stopping: 0,
            alarmed: false,
        }
    }

    /// Room for everything: nothing gives way.
    #[test]
    fn with_room_everything_is_shown() {
        let f = fit_ladder(&input(40));
        assert_eq!(
            f,
            Fit {
                rows: 3,
                hint: true,
                show_notice: true,
                show_stuck: true,
                show_pane: true,
                show_status: true,
                boxed: true,
                content_rows: 4,
            }
        );
    }

    /// The order things give way in, one row at a time as the screen shrinks: the hint, the notice,
    /// the composer's extra rows, the stuck disclosure, the pane's line, the turn's row, the box — and
    /// the card's own rows last of all.
    #[test]
    fn rows_give_way_in_the_ladders_order() {
        let mut seen = Vec::new();
        let mut last = fit_ladder(&input(40));
        for h in (1..40).rev() {
            let f = fit_ladder(&input(h));
            let gave = |a: bool, b: bool| a && !b;
            if gave(last.hint, f.hint) {
                seen.push("hint");
            }
            if gave(last.show_notice, f.show_notice) {
                seen.push("notice");
            }
            if f.rows < last.rows {
                seen.push("composer");
            }
            if gave(last.show_stuck, f.show_stuck) {
                seen.push("stuck");
            }
            if gave(last.show_pane, f.show_pane) {
                seen.push("pane");
            }
            if gave(last.show_status, f.show_status) {
                seen.push("status");
            }
            if gave(last.boxed, f.boxed) {
                seen.push("box");
            }
            if f.content_rows < last.content_rows {
                seen.push("card");
            }
            last = f;
        }
        seen.dedup();
        assert_eq!(
            seen,
            [
                "hint", "notice", "composer", "stuck", "pane", "status", "box", "card"
            ]
        );
    }

    /// The link's and a stop's lines are counted and never given up: they take room from everything
    /// else instead.
    #[test]
    fn the_link_and_a_stop_are_never_given_up() {
        let mut i = input(12);
        let roomy = fit_ladder(&i);
        i.link = 2;
        i.stopping = 2;
        let tight = fit_ladder(&i);
        let shown = |f: &Fit| {
            usize::from(f.hint)
                + usize::from(f.show_notice)
                + usize::from(f.show_stuck)
                + usize::from(f.show_pane)
                + usize::from(f.show_status)
                + f.rows
                + f.content_rows
        };
        assert!(shown(&tight) < shown(&roomy), "{roomy:?} vs {tight:?}");
    }
}
