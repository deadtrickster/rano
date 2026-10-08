//! **The quit card**: what leaving would stop, and the choices for it.
//!
//! Ported from letibot's `crates/tui/src/ui/cards/quit.rs`. letibot painted the title
//! `sgr::BOLD` ([`Role::Strong`] here) and the consequences `sgr::DIM` ([`Role::Faint`]).

use crate::render::{Line, Span};
use crate::style::Role;

use super::text::{one, wrap};

/// The quit card's view model.
///
/// letibot fills it from: `selected` ← `quit_sel`; `other_heads` ← `heads - 1`;
/// `running_jobs` ← jobs with `running`; `running_subagents` ← subagents whose state is
/// `"running"` (only the RUNNING count: a settled job is history the store keeps, and
/// `opening` is a subagent that has not started — nothing that dies).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct QuitCard {
    /// 0 leaves this head (the default, the cheap one), 1 stops the daemon too.
    pub selected: usize,
    pub other_heads: usize,
    pub running_jobs: usize,
    pub running_subagents: usize,
}

impl QuitCard {
    /// The two rows: what Enter does, and the consequence of it.
    ///
    /// The consequence is on the row rather than in a footnote because it is the whole reason
    /// the card exists — one of these two is cheap and the other is not, and a card that made
    /// them look alike would be a card that answered for the operator.
    pub fn choices(&self) -> [(&'static str, String); 2] {
        [
            (
                "leave this head",
                "the daemon keeps running: the session stays warm and `letibot` \
                 reattaches to it"
                    .to_string(),
            ),
            ("leave and stop the daemon", {
                // **AND THE WORK THAT DIES WITH IT, SAID FIRST** — the operator's ask,
                // 2026-10-05: stopping the daemon stops the jobs and the subagents with it, and
                // a card that named only the cold prefill made the cheap row and the killing row
                // read alike. A running job is `cargo test --release` an hour in; a running
                // subagent is a session mid-task. Neither survives the stop.
                let (j, s) = (self.running_jobs, self.running_subagents);
                let plural = |n: usize| if n == 1 { "" } else { "s" };
                let dies = if j > 0 && s > 0 {
                    format!(
                        "{j} job{} and {s} subagent{} are running and stop with the daemon. ",
                        plural(j),
                        plural(s)
                    )
                } else if j > 0 {
                    // letibot's spelling pasted `is` straight onto `job` (`1 jobis running`) and
                    // said the verb twice (`it stops stop with the daemon`); both are fixed here.
                    format!(
                        "{j} job{} running — {} with the daemon. ",
                        if j == 1 { " is" } else { "s are" },
                        if j == 1 { "it stops" } else { "they stop" }
                    )
                } else if s > 0 {
                    format!(
                        "{s} subagent{} running — {} with the daemon. ",
                        if s == 1 { " is" } else { "s are" },
                        if s == 1 { "it stops" } else { "they stop" }
                    )
                } else {
                    String::new()
                };
                match self.other_heads {
                    0 => format!(
                        "{dies}the session is written to disk and `letibot --continue` \
                         reopens it — but its prompt leaves the model server's cache, \
                         so the next turn prefills cold"
                    ),
                    1 => format!(
                        "{dies}one other head is attached and will be told. The session \
                         is on disk; the next turn after reopening prefills cold"
                    ),
                    n => format!(
                        "{dies}{n} other heads are attached and will be told. The session \
                         is on disk; the next turn after reopening prefills cold"
                    ),
                }
            }),
        ]
    }

    /// The card. Its shape is load-bearing for a host's click arithmetic: the title is row 0,
    /// and choice `i` starts at the row [`QuitCard::choice_rows`] reports.
    pub fn lines(&self, w: usize) -> Vec<Line> {
        self.layout(w).0
    }

    /// The row each choice's name line was drawn on, for mouse hit-testing.
    pub fn choice_rows(&self, w: usize) -> [usize; 2] {
        self.layout(w).1
    }

    fn layout(&self, w: usize) -> (Vec<Line>, [usize; 2]) {
        let mut out = vec![one("leave — and what happens to the daemon", Role::Strong)];
        let mut rows = [0usize; 2];
        for (i, (name, why)) in self.choices().iter().enumerate() {
            let picked = i == self.selected.min(1);
            rows[i] = out.len();
            let l = Line::new(vec![
                Span::raw(format!("{} {:>2}  ", if picked { "▸" } else { " " }, i + 1)),
                Span::role(*name, if picked { Role::Strong } else { Role::Plain }),
            ]);
            out.push(crate::render::text::truncate_owned(l, w));
            for l in wrap(why, w.saturating_sub(8)) {
                out.push(one(format!("       {l}"), Role::Faint));
            }
        }
        (out, rows)
    }
}

super::lines_widget!(QuitCard);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::testing::{plain, role_of};

    /// letibot `app/tests/misc.rs::the_quit_card_offers_both_exits_and_defaults_to_the_cheap_one`
    /// (the card).
    #[test]
    fn the_quit_card_offers_both_exits_and_defaults_to_the_cheap_one() {
        let card = QuitCard::default();
        let lines = card.lines(100);
        let s = plain(&lines).join("\n");
        assert!(s.contains("leave this head"), "{s}");
        assert!(s.contains("leave and stop the daemon"), "{s}");
        assert!(s.contains("prefills cold"), "{s}");
        assert!(
            !s.contains("stop with the daemon"),
            "nothing is running, so nothing is claimed to die: {s}"
        );
        let rows = card.choice_rows(100);
        assert!(lines[rows[0]].plain().starts_with("▸  1  leave this head"));
        assert_eq!(role_of(&lines[rows[0]], "leave"), Some(Role::Strong));
        assert_eq!(role_of(&lines[rows[1]], "leave"), Some(Role::Plain));
    }

    /// letibot `app/tests/subagents.rs` — the running pair is named on the stop row, and the
    /// cheap row names no dying work.
    #[test]
    fn the_stop_row_names_the_work_that_dies_with_it() {
        let card = QuitCard {
            running_jobs: 1,
            running_subagents: 1,
            ..QuitCard::default()
        };
        let c = card.choices();
        assert!(
            c[1].1
                .contains("1 job and 1 subagent are running and stop with the daemon"),
            "{:?}",
            c[1].1
        );
        assert!(!c[0].1.contains("stop with the daemon"));
        assert!(
            plain(&card.lines(110))
                .join(" ")
                .contains("stop with the daemon")
        );
        let jobs = QuitCard {
            running_jobs: 2,
            other_heads: 2,
            ..QuitCard::default()
        };
        assert!(
            jobs.choices()[1]
                .1
                .starts_with("2 jobs are running — they stop with the daemon. 2 other heads")
        );
        let one_job = QuitCard {
            running_jobs: 1,
            ..QuitCard::default()
        };
        assert!(
            one_job.choices()[1]
                .1
                .starts_with("1 job is running — it stops")
        );
    }
}
