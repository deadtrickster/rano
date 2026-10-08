//! **The conversation's own rows**: what the person said, what the session said to the
//! model on its behalf, how a turn ended, and the marker that stands in for a run of rows a
//! rung hides.
//!
//! Ported from letibot's `ui/transcript/blocks.rs`, `turn.rs` and the painting half of
//! `markers.rs`. The rows are facts the host holds — the text, a clock time it formatted,
//! the counts it took — and this lays them out; deciding *which* rows a run hides, or how
//! many lines a reasoning block is, stays with the host.

use crate::render::{Line, Span};
use crate::style::Role;

use super::text::{clean, one, visible_width, wrap};

/// **The person's own words**: the `▌` bar in the user-accent register and the text in the
/// user block (reverse video, to the full width, so the question is the brightest band on
/// the screen), the clock time at the right of its first row.
///
/// Three signals, because each is lost somewhere: the bar is a glyph (it survives a pipe and
/// a copy), the band is reverse video (it survives a terminal-native palette), and the row
/// sits at the body's own column, where the conversation is.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UserBlock {
    /// What they said, as the host folded it (a pasted block already shortened).
    pub text: String,
    /// The clock time, as the host formats one; empty draws none.
    pub stamp: String,
}

impl UserBlock {
    pub fn lines(&self, w: usize) -> Vec<Line> {
        let w = w.max(20);
        let stamp_w = visible_width(&self.stamp);
        let head_w = w.saturating_sub(2 + stamp_w + usize::from(!self.stamp.is_empty()));
        let mut rows = wrap(&clean(&self.text), head_w.max(8));
        if rows.is_empty() {
            rows.push(String::new());
        }
        rows.iter()
            .enumerate()
            .map(|(i, l)| {
                let tail = if i == 0 && !self.stamp.is_empty() {
                    let pad = w
                        .saturating_sub(2)
                        .saturating_sub(visible_width(l))
                        .saturating_sub(stamp_w);
                    format!("{}{}", " ".repeat(pad), self.stamp)
                } else {
                    " ".repeat(w.saturating_sub(2).saturating_sub(visible_width(l)))
                };
                Line::new(vec![
                    Span::role("▌", Role::UserAccent),
                    Span::raw(" "),
                    Span::role(format!("{l}{tail}"), Role::UserBlock),
                ])
            })
            .collect()
    }
}

/// **A line the person sent that the conversation has not taken yet** — queued behind a
/// running turn, or sent and not yet confirmed: the bar, the mark (`queued`, `unconfirmed`)
/// in the notice register, and the text faint.
///
/// **R33: it is a thing WAITING, not content to read — so folded it is ONE elided headline.**
/// The operator, looking at three of their own messages queued: *"three giant messages
/// queued"* — a 63-row pane filled with the reader's own words. They typed it; they do not
/// need it read back. The seam counts **screen rows**, the cost on the terminal.
///
/// **A row that has landed carries no mark**, and is drawn in the shape the settled row has —
/// bar and text, no label — not with a bare ` · ` where the mark would be.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct QueuedBlock {
    pub text: String,
    /// `queued`, `unconfirmed`, or empty for a row that has landed.
    pub mark: String,
    /// Unfolded (the host's `/t`): every row of the text.
    pub open: bool,
}

impl QueuedBlock {
    pub fn lines(&self, w: usize) -> Vec<Line> {
        let w = w.max(20);
        let text = clean(&self.text);
        let bar = || Span::role("▌", Role::UserAccent);
        if self.mark.is_empty() {
            let mut rows = wrap(&text, w.saturating_sub(2).max(8));
            if rows.is_empty() {
                rows.push(String::new());
            }
            return rows
                .into_iter()
                .enumerate()
                .map(|(i, l)| {
                    if i == 0 {
                        Line::new(vec![bar(), Span::raw(format!(" {l}"))])
                    } else {
                        Line::raw(format!("  {l}"))
                    }
                })
                .collect();
        }
        // The first row shares its width with the mark; the rest hang under the text.
        let head_w = w.saturating_sub(2 + visible_width(&self.mark) + 3);
        let mut rows = wrap(&text, head_w.max(8));
        if rows.is_empty() {
            rows.push(String::new());
        }
        let mark = || Span::role(format!("{} · ", self.mark), Role::Pending);
        if !self.open && rows.len() > 1 {
            let seam = format!("  … +{} lines · /t opens it", rows.len() - 1);
            let room = head_w.saturating_sub(visible_width(&seam));
            if room >= 16 {
                return vec![Line::new(vec![
                    bar(),
                    Span::raw(" "),
                    mark(),
                    Span::role(super::text::trim_to(&rows[0], room), Role::Faint),
                    Span::role(seam, Role::Faint),
                ])];
            }
            // **A terminal too narrow for the seam still gets one row**: the headline alone,
            // elided by the bar's own width.
            return vec![Line::new(vec![
                bar(),
                Span::raw(" "),
                mark(),
                Span::role(super::text::trim_to(&rows[0], head_w.max(8)), Role::Faint),
            ])];
        }
        let indent = " ".repeat(visible_width(&self.mark) + 3);
        rows.iter()
            .enumerate()
            .map(|(i, l)| {
                Line::new(vec![
                    bar(),
                    Span::raw(" "),
                    if i == 0 {
                        mark()
                    } else {
                        Span::raw(indent.clone())
                    },
                    Span::role(l.clone(), Role::Faint),
                ])
            })
            .collect()
    }
}

/// The label a session row carries, and the columns it takes.
const SESSION: &str = "session · ";

/// **What the session said to the model**, on the person's behalf — a completion notice, a
/// reminder: `session · ` faint at the left, the text faint after it, the clock time at the
/// end of its last row. Not the person's bar: these are not their words.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionBlock {
    pub text: String,
    pub stamp: String,
}

impl SessionBlock {
    /// The columns the text is wrapped at, for a host that cuts its own lines first (a folded
    /// notice, one settlement a line).
    pub fn text_cols(stamp: &str, w: usize) -> usize {
        w.max(20)
            .saturating_sub(visible_width(SESSION) + visible_width(stamp) + 2)
            .max(8)
    }

    pub fn lines(&self, w: usize) -> Vec<Line> {
        let w = w.max(20);
        let mut rows = wrap(&clean(&self.text), Self::text_cols(&self.stamp, w));
        if rows.is_empty() {
            rows.push(String::new());
        }
        let indent = " ".repeat(visible_width(SESSION));
        let last = rows.len() - 1;
        rows.iter()
            .enumerate()
            .map(|(i, l)| {
                let label = if i == 0 {
                    Span::role(SESSION, Role::Faint)
                } else {
                    Span::raw(indent.clone())
                };
                let tail = if i == last && !self.stamp.is_empty() {
                    format!("  {}", self.stamp)
                } else {
                    String::new()
                };
                crate::render::text::truncate_owned(
                    Line::new(vec![
                        Span::raw("  "),
                        label,
                        Span::role(format!("{l}{tail}"), Role::Faint),
                    ]),
                    w,
                )
            })
            .collect()
    }
}

/// **How a turn ended, when the ending is news.** An ordinary ending (the model stopped, or
/// a stop word) draws nothing: the answer is the ending.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TurnEnd {
    /// Still running, or ended the ordinary way: no row.
    Quiet,
    /// It hit the output limit mid-answer.
    CutShort,
    Aborted,
    /// A finish reason this host does not know, said in its own word.
    Other(String),
    Interrupted {
        reason: String,
        kept: bool,
    },
    Failed {
        error: String,
        kept: bool,
    },
}

impl TurnEnd {
    pub fn lines(&self, w: usize) -> Vec<Line> {
        let notice = |s: String| vec![one(s, Role::Pending)];
        match self {
            TurnEnd::Quiet => Vec::new(),
            TurnEnd::CutShort => notice(
                "── CUT SHORT — it hit the output limit mid-answer; ask it to continue".into(),
            ),
            TurnEnd::Aborted => notice("── stopped early (aborted)".into()),
            TurnEnd::Other(s) => notice(format!("── ended for an unrecognised reason: {s}")),
            TurnEnd::Interrupted { reason, kept } => notice(format!(
                "── interrupted: {reason} ({})",
                if *kept {
                    "what it had written is kept"
                } else {
                    "nothing kept"
                }
            )),
            // The one ending in the failure register, and wrapped: an error is read, not
            // glanced at.
            TurnEnd::Failed { error, kept } => {
                let kept = if *kept {
                    "what it had written is kept"
                } else {
                    "nothing was recorded"
                };
                wrap(&format!("── FAILED — {error} ({kept})"), w)
                    .into_iter()
                    .map(|l| one(l, Role::Failure))
                    .collect()
            }
        }
    }
}

/// **The line that stands in for a run of rows the rung hides** — `[2 tool calls, 3 thinking
/// lines] · ctrl-v opens it`.
///
/// **The number goes pending and its noun does not** — the operator: *"yellow <count> not
/// entire <Count> tool call"*, twice, the second time correcting the first fix. The digits are
/// the thing that moves (the calls still running); `tool call` is what they count, and a
/// phrase in yellow on a line whose job is to be punctuation inside the model's sentence reads
/// as a highlight rather than a signal. The thinking count is never coloured.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RunMarker {
    /// `("2", " tool calls")`, the number and its noun apart.
    pub calls: Option<(String, String)>,
    pub think: Option<(String, String)>,
    /// The fallback clause, drawn only when the run is one neither count describes.
    pub events: Option<(String, String)>,
    /// ` · ctrl-v opens it` — the affordance, faint.
    pub seam: String,
    /// The calls are still going: their number is pending.
    pub live: bool,
}

impl RunMarker {
    pub fn line(&self) -> Line {
        let mut l = Line::raw("[");
        let mut first = true;
        let mut clause = |l: &mut Line, (n, noun): &(String, String), live: bool| {
            if !first {
                l.push(Span::raw(", "));
            }
            first = false;
            if live {
                l.push(Span::role(n.clone(), Role::Pending));
            } else {
                l.push(Span::raw(n.clone()));
            }
            l.push(Span::raw(noun.clone()));
        };
        if self.calls.is_none() && self.think.is_none() {
            if let Some(e) = &self.events {
                clause(&mut l, e, false);
            }
        } else {
            if let Some(c) = &self.calls {
                clause(&mut l, c, self.live);
            }
            if let Some(t) = &self.think {
                clause(&mut l, t, false);
            }
        }
        l.push(Span::raw("]"));
        // Painted even when empty, as letibot's marker always carried its seam's run.
        l.push(Span::role(self.seam.clone(), Role::Faint));
        l
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::testing::{plain, role_of};
    use crate::style::Palette;

    /// The person's row: the bar, the band to the full width, the time on the first row.
    #[test]
    fn the_persons_row_is_a_band_with_its_time_at_the_right() {
        let b = UserBlock {
            text: "why is the prefix cache missing?".into(),
            stamp: "09:41".into(),
        };
        let l = b.lines(60);
        assert_eq!(l.len(), 1);
        assert_eq!(l[0].width(), 60);
        assert!(l[0].plain().starts_with("▌ why is"));
        assert!(l[0].plain().ends_with("09:41"));
        assert_eq!(role_of(&l[0], "▌"), Some(Role::UserAccent));
        assert_eq!(
            l[0].to_ansi(Palette::Colour),
            format!(
                "\x1b[34m▌\x1b[0m \x1b[40mwhy is the prefix cache missing?{}09:41\x1b[0m",
                " ".repeat(60 - 2 - 32 - 5)
            )
        );
    }

    /// A queued line folds to one headline and a seam; a landed one has no mark.
    #[test]
    fn a_queued_line_folds_to_one_headline() {
        let q = QueuedBlock {
            text: "word ".repeat(60),
            mark: "queued".into(),
            open: false,
        };
        let l = q.lines(80);
        assert_eq!(l.len(), 1);
        assert!(
            l[0].plain().starts_with("▌ queued · word"),
            "{}",
            l[0].plain()
        );
        assert!(l[0].plain().contains("/t opens it"));
        let open = QueuedBlock {
            open: true,
            ..q.clone()
        }
        .lines(80);
        assert!(open.len() > 1);
        let landed = QueuedBlock {
            mark: String::new(),
            ..q
        };
        assert!(landed.lines(80)[0].plain().starts_with("▌ word"));
    }

    /// A session row is labelled and faint, its time at the end of its last row.
    #[test]
    fn a_session_row_is_labelled_and_faint() {
        let s = SessionBlock {
            text: "Job j4 exited 0 after 3.1s".into(),
            stamp: "09:41".into(),
        };
        let rows = plain(&s.lines(80));
        assert_eq!(rows, vec!["  session · Job j4 exited 0 after 3.1s  09:41"]);
        assert_eq!(SessionBlock::text_cols("09:41", 80), 80 - 10 - 5 - 2);
    }

    /// An ordinary ending draws nothing; a failure is wrapped in the failure register.
    #[test]
    fn a_turn_ending_is_said_only_when_it_is_news() {
        assert!(TurnEnd::Quiet.lines(80).is_empty());
        let f = TurnEnd::Failed {
            error: "the model server closed the connection".into(),
            kept: true,
        }
        .lines(30);
        assert!(f.len() > 1);
        assert_eq!(f[0].spans[0].style.top(), Role::Failure);
        assert_eq!(
            plain(&TurnEnd::CutShort.lines(200))[0],
            "── CUT SHORT — it hit the output limit mid-answer; ask it to continue"
        );
    }

    /// Only the calls' number is pending, and only while they run.
    #[test]
    fn only_the_live_calls_number_is_pending() {
        let m = RunMarker {
            calls: Some(("2".into(), " tool calls".into())),
            think: Some(("3".into(), " thinking lines".into())),
            events: None,
            seam: " · ctrl-v opens it".into(),
            live: true,
        };
        let l = m.line();
        assert_eq!(
            l.plain(),
            "[2 tool calls, 3 thinking lines] · ctrl-v opens it"
        );
        assert_eq!(
            l.to_ansi(Palette::Colour),
            "[\x1b[33m2\x1b[0m tool calls, 3 thinking lines]\x1b[2m · ctrl-v opens it\x1b[0m"
        );
        let events = RunMarker {
            events: Some(("1".into(), " head event".into())),
            ..RunMarker::default()
        };
        assert_eq!(
            events.line().to_ansi(Palette::Colour),
            "[1 head event]\x1b[2m\x1b[0m"
        );
    }
}
