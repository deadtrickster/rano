//! **The merge queue**: what is waiting to land, and one entry opened whole.
//!
//! Ported from letibot's `ui/panes/queue.rs`. The words are the daemon's — the state, the
//! evidence, the reviewer's verdict — and the marks are the head's reading of them; the
//! order is the daemon's too, and is not sorted here: the queue is the queue's own
//! scheduling, and a pane that re-sorted it would be a second opinion about what should
//! land next.

use crate::render::{Line, Span};
use crate::style::Role;

use super::pane::{self, PaneLines};
use super::text::{clean_line, one, trim_to, wrap};

/// The head's reading of an entry's state, for its mark.
///
/// `waiting` is not a problem, `taken` is work in progress, `landed` is done, and the three
/// that park an entry for a person (failed, conflict, stale) are the loud ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MergeMark {
    #[default]
    Waiting,
    Taken,
    Landed,
    Parked,
}

impl MergeMark {
    fn mark(self) -> (&'static str, Role) {
        match self {
            MergeMark::Waiting => ("[ ]", Role::Pending),
            MergeMark::Taken => ("[~]", Role::Code),
            MergeMark::Landed => ("[x]", Role::Success),
            MergeMark::Parked => ("[!]", Role::Failure),
        }
    }
}

/// Where an entry's review is. *No verdict yet* and *nobody has asked* are different facts,
/// and the pane tells them apart.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum ReviewState {
    #[default]
    NotAsked,
    Asked,
    /// The reviewer's decision word.
    Decided(String),
}

/// One entry in the queue.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct QueueEntry {
    pub id: String,
    pub branch: String,
    /// The daemon's state word: `waiting`, `taken`, `landed`, `failed`, …
    pub state: String,
    pub mark: MergeMark,
    /// How long ago it was queued, as the host spells a duration.
    pub age: String,
    pub review: ReviewState,
    /// Why it is where it is — the queue's own words. Empty draws nothing.
    pub evidence: String,
}

/// The queue pane's facts.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct QueuePane {
    pub entries: Vec<QueueEntry>,
    pub selected: usize,
}

impl QueuePane {
    /// **The merge queue, as rows** — one entry per row plus its state and reason beneath
    /// it.
    ///
    /// Two lines an entry, like the jobs pane's rows and for the same reason: the facts a
    /// reader needs are (what is it) and (why is it where it is), and a single line would
    /// truncate the second to make room for the first. Each entry's first row is a stop.
    pub fn content(&self, w: usize) -> PaneLines {
        let mut out = PaneLines::new();
        out.push(pane::title("merge queue"));
        out.blank();
        if self.entries.is_empty() {
            out.push(pane::faint(
                "    none. A branch lands here when a `task_start` child finishes; nothing \
                 lands without the gatekeeper's verdict and the gate.",
            ));
            return out;
        }
        let cursor = self.selected.min(self.entries.len().saturating_sub(1));
        for (i, e) in self.entries.iter().enumerate() {
            let (mark, role) = e.mark.mark();
            out.push_stop(Line::new(vec![
                Span::raw(format!("{} ", pane::mark(i == cursor))),
                Span::role(mark, role),
                Span::raw(format!(" {} ", clean_line(&e.branch))),
                Span::role(
                    format!("· {} · {}", clean_line(&e.state), e.age),
                    Role::Faint,
                ),
            ]));
            let review = match &e.review {
                ReviewState::NotAsked => " · nobody has reviewed it".to_string(),
                ReviewState::Asked => {
                    " · the reviewer has been asked and has not answered".to_string()
                }
                ReviewState::Decided(d) => format!(" · reviewer: {d}"),
            };
            let evidence = if e.evidence.is_empty() {
                String::new()
            } else {
                format!(" — {}", clean_line(&e.evidence))
            };
            out.push(pane::faint(trim_to(
                &format!("         {}{review}{evidence}", e.id),
                w,
            )));
        }
        out.blank();
        out.push(pane::faint(
            "    enter opens the entry: the ask it was built from, the gate's own words, and \
             the reviewer's verdict",
        ));
        out
    }
}

/// The review record, for an opened entry.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReviewRecord {
    /// The decision word, `None` while asked and not answered.
    pub decision: Option<String>,
    /// How long ago it was asked, as the host spells a duration.
    pub asked: String,
    /// The reviewer's own session — the argument is there.
    pub session_id: String,
    pub reasons: Vec<String>,
    pub files: Vec<String>,
    pub commands: Vec<String>,
}

/// **One entry, whole** — the overlay the queue pane's Enter opens.
///
/// Everything is a row the host already holds: the entry's own fields, the evidence (the
/// gate's captured words when the gate failed, the reviewer's verdict when the review
/// refused it), and the review record.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct QueueEntryView {
    /// The id the overlay was opened on.
    pub id: String,
    /// `None` when the queue no longer holds it (a `recover` moved it, or the host switched
    /// and took a fresh snapshot): said, rather than drawing an empty overlay.
    pub entry: Option<OpenedEntry>,
}

/// An opened entry's fields, as the host spells them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OpenedEntry {
    pub branch: String,
    pub state: String,
    pub priority: String,
    pub base_sha: String,
    pub age: String,
    pub worktree: Option<String>,
    pub landed_sha: Option<String>,
    pub needs: Vec<String>,
    /// The ask it was built from.
    pub brief: String,
    pub evidence: String,
    /// `None` when nobody has asked for a review.
    pub review: Option<ReviewRecord>,
}

/// A label in the faint register, nine columns, then its value on the same row: one row
/// rather than two, because a label alone on a line reads as a heading.
fn labelled(k: &str, v: &str) -> Line {
    Line::new(vec![
        Span::raw("  "),
        Span::role(format!("{k:<9}"), Role::Faint),
        Span::raw(clean_line(v)),
    ])
}

impl QueueEntryView {
    /// Every row of the overlay, at `w` columns.
    pub fn lines(&self, w: usize) -> Vec<Line> {
        let mut out = vec![pane::title("merge queue · entry"), Line::default()];
        let Some(e) = &self.entry else {
            out.push(Line::raw(format!(
                "  `{}` is not in the queue any more. esc goes back to the list.",
                clean_line(&self.id)
            )));
            return out;
        };
        for (k, v) in [
            ("branch", &e.branch),
            ("state", &e.state),
            ("entry", &self.id),
            ("priority", &e.priority),
            ("base", &e.base_sha),
            ("age", &e.age),
        ] {
            out.push(labelled(k, v));
        }
        if let Some(wt) = &e.worktree {
            out.push(labelled("worktree", wt));
        }
        if let Some(tip) = &e.landed_sha {
            out.push(labelled("landed", tip));
        }
        if !e.needs.is_empty() {
            out.push(labelled("needs", &e.needs.join(", ")));
        }
        out.push(Line::default());
        out.push(one("  the ask it was built from", Role::Strong));
        out.push(Line::default());
        if e.brief.trim().is_empty() {
            out.push(one(
                "  (nobody recorded one — the reviewer has nothing to review against)",
                Role::Faint,
            ));
        } else {
            for l in e.brief.lines() {
                out.push(Line::raw(format!("  {}", clean_line(l))));
            }
        }
        out.push(Line::default());
        out.push(one(
            "  why it is where it is — the queue's own words",
            Role::Strong,
        ));
        out.push(Line::default());
        if e.evidence.is_empty() {
            out.push(one("  (nothing yet)", Role::Faint));
        } else {
            // **Not trimmed to one line.** The gate's failure is up to four kilobytes of the
            // CI command's own output, and this overlay is the only place it is readable at
            // all, so it is wrapped rather than elided and the pane scrolls.
            for l in e.evidence.lines() {
                for wrapped in wrap(l, w.saturating_sub(4)) {
                    out.push(Line::raw(format!("  {}", clean_line(&wrapped))));
                }
            }
        }
        out.push(Line::default());
        out.push(one("  the reviewer's verdict", Role::Strong));
        out.push(Line::default());
        match &e.review {
            None => out.push(one(
                "  nobody has asked. An entry with no review does not land — the queue waits.",
                Role::Faint,
            )),
            Some(r) => {
                out.push(labelled(
                    "decision",
                    r.decision.as_deref().unwrap_or("(asked, no answer yet)"),
                ));
                out.push(labelled("asked", &r.asked));
                // **Where the argument is.** A verdict is a summary of a review, and the
                // review itself is a conversation — the person can attach to it with the
                // session's own id, which is why the reviewer is a session and not a call.
                out.push(labelled(
                    "session",
                    &format!("{} — attach to it to read the argument", r.session_id),
                ));
                out.push(Line::default());
                out.push(one("  reasons", Role::Faint));
                if r.reasons.is_empty() {
                    out.push(one("    (none given)", Role::Faint));
                } else {
                    for reason in &r.reasons {
                        for wrapped in wrap(reason, w.saturating_sub(6)) {
                            out.push(Line::raw(format!("    - {}", clean_line(&wrapped))));
                        }
                    }
                }
                out.push(Line::default());
                out.push(one("  what it looked at", Role::Faint));
                if r.files.is_empty() && r.commands.is_empty() {
                    out.push(one(
                        "    (nothing named — a verdict with no evidence is an opinion)",
                        Role::Faint,
                    ));
                } else {
                    for f in &r.files {
                        out.push(Line::raw(format!("    file    {}", clean_line(f))));
                    }
                    for c in &r.commands {
                        out.push(Line::raw(format!("    command {}", clean_line(c))));
                    }
                }
            }
        }
        out
    }
}

super::lines_widget!(QueueEntryView);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::testing::{plain, role_of};
    use crate::style::Palette;

    fn entry(id: &str, mark: MergeMark, review: ReviewState) -> QueueEntry {
        QueueEntry {
            id: id.into(),
            branch: format!("task/{id}"),
            state: "waiting".into(),
            mark,
            age: "4.0s".into(),
            review,
            evidence: String::new(),
        }
    }

    /// Two rows an entry, the first a stop; nobody-asked and asked-not-answered read
    /// differently.
    #[test]
    fn an_entry_is_two_rows_and_its_review_says_where_it_is() {
        let p = QueuePane {
            entries: vec![
                entry("m1", MergeMark::Waiting, ReviewState::NotAsked),
                entry("m2", MergeMark::Parked, ReviewState::Asked),
            ],
            selected: 1,
        };
        let c = p.content(120);
        let rows = plain(&c.lines);
        assert_eq!(c.stop_rows, vec![2, 4]);
        assert_eq!(rows[2], "  [ ] task/m1 · waiting · 4.0s");
        assert_eq!(rows[3], "         m1 · nobody has reviewed it");
        assert_eq!(rows[4], "▸ [!] task/m2 · waiting · 4.0s");
        assert!(rows[5].contains("has been asked and has not answered"));
        assert_eq!(role_of(&c.lines[4], "[!]"), Some(Role::Failure));
        assert_eq!(
            c.lines[2].to_ansi(Palette::Colour),
            "  \x1b[33m[ ]\x1b[0m task/m1 \x1b[2m· waiting · 4.0s\x1b[0m"
        );
    }

    /// An empty queue says how a branch gets there, and has no stops.
    #[test]
    fn an_empty_queue_says_how_a_branch_gets_there() {
        let c = QueuePane::default().content(120);
        assert!(c.stop_rows.is_empty());
        assert!(plain(&c.lines)[2].contains("A branch lands here"));
    }

    /// An entry the queue no longer holds is said, not drawn empty; one it holds names
    /// where the reviewer's argument is.
    #[test]
    fn an_opened_entry_is_whole_or_says_it_has_gone() {
        let gone = QueueEntryView {
            id: "m9".into(),
            entry: None,
        };
        assert!(plain(&gone.lines(80))[2].contains("`m9` is not in the queue any more"));
        let open = QueueEntryView {
            id: "m1".into(),
            entry: Some(OpenedEntry {
                branch: "task/m1".into(),
                state: "failed".into(),
                priority: "subagent".into(),
                base_sha: "abc123".into(),
                age: "2m03s".into(),
                evidence: "cargo test failed\n2 tests".into(),
                review: Some(ReviewRecord {
                    decision: Some("refuse".into()),
                    asked: "1.0s".into(),
                    session_id: "s-rev".into(),
                    ..ReviewRecord::default()
                }),
                ..OpenedEntry::default()
            }),
        };
        let rows = plain(&open.lines(80));
        assert_eq!(rows[2], "  branch   task/m1");
        assert!(rows.iter().any(|l| l == "  decision refuse"), "{rows:#?}");
        assert!(rows.iter().any(|l| l.contains("s-rev — attach to it")));
        assert!(rows.iter().any(|l| l == "  cargo test failed"));
    }
}
