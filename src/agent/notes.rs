//! **The notes**: the disclosures a head shows in the conversation — a guard that fired, a
//! decision that settled, a sentence the daemon interrupted with, a pane that ended — and
//! the `/notes` listing that holds all of them.
//!
//! Ported from letibot's `ui/panes/notes.rs`. One renderer for the transcript row and for
//! the listing, so the listing cannot disagree with the screen about the text: the row is
//! the listing's text folded to [`NOTE_LINES`].
//!
//! # §3.1: a note carries text from elsewhere
//!
//! A warning's detail is the daemon's or a guard's sentence, a decision's summary and basis
//! are the ask and the decider's own words, a pane's rows are whatever its program last
//! drew. All of it is cleaned before it is wrapped.

use crate::render::Line;
use crate::style::Role;

use super::text::{clean, one, wrap};

/// **How many lines of a note the conversation shows before it is a wall.**
///
/// The number comes from the operator's own screen rather than from taste: two gate timeouts
/// rendered **27 red lines** (*"how to remove this red wall?"*), around thirteen lines each — a
/// `denied:` detail with the whole rule in it. Three lines keeps the code, the first sentence
/// and the fact that there is more, and puts the rest one verb away; the listing prints the
/// whole thing, so this is a disclosure decision and never a cap on the record.
pub const NOTE_LINES: usize = 3;

/// **The register a warning's code was classified into** — letibot's
/// `letibot_sessionlog::warning::Class`, the log's vocabulary rather than the head's.
///
/// ```text
///   · code — sentence      faint    housekeeping: nothing to do
///   × code — sentence      notice   the answer to what you just typed; retype
///   ! code — sentence      red      the session is in trouble; stop and look
/// ```
///
/// The mark goes with the colour, because colour is a no-op under a replay, a pipe and a
/// light theme, and three registers that collapse to one appearance in half the terminals
/// they are read in are one register with extra steps.
///
/// **Routine is not failure** — letibot's R19, the operator's ruling of 2026-09-22: *"routine
/// is painted as failure"* — `compacted`, `auto_compact`, `daemon_stopping` and a fourth
/// arrived on a head that had just attached, all four in the red a denial gets, and four
/// notes read as a wall. A housekeeping notice and a refused call must not look alike, and
/// the argument is not taste: a person met by a red block on every restart learns to skip
/// it, and the block is where a real denial lives. So routine is a `·` and faint, and the
/// code is kept, because it is the word a reader greps the log for. Which codes are routine
/// is the host's log's table, not this widget's opinion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoteClass {
    Routine,
    Refused,
    Failure,
}

/// How a decision settled, for its note.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Settled {
    /// An `allow…` option was chosen; the option id.
    Allowed(String),
    /// Any other option; the option id.
    Refused(String),
    Cancelled,
    /// The deadline decided it.
    TimedOut,
}

/// One note.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Note {
    /// A warning the daemon sent: its code, its sentence, and the register its code is in.
    Warned {
        code: String,
        detail: String,
        class: NoteClass,
    },
    /// A call that did not run. No `!`, no red, no request id: nothing here is answerable,
    /// and the detail that was cut is in the model's own tool result, folded, one row above.
    NotRun { detail: String },
    /// A pane that ended: the line that ran it, why it ended, its program's last rows, and
    /// whether the person ended it themselves (`closed`) — housekeeping then, faint and a
    /// `·`; ended without them it is the answer to what they typed, the notice register and
    /// a `×`.
    Pane {
        line: String,
        said: Vec<String>,
        reason: String,
        closed: bool,
    },
    /// A decision that settled.
    Decided {
        summary: String,
        outcome: Settled,
        by_kind: String,
        by_identity: String,
        basis: String,
        /// An answer arrived after it had settled.
        late: bool,
    },
}

impl Note {
    /// **The whole note, with no fold** — what the listing prints and what the transcript
    /// shows the head of.
    pub fn unfolded(&self, w: usize) -> Vec<Line> {
        match self {
            Note::Warned {
                code,
                detail,
                class,
            } => {
                let (mark, role) = match class {
                    NoteClass::Routine => ("·", Role::Faint),
                    NoteClass::Refused => ("×", Role::Pending),
                    NoteClass::Failure => ("!", Role::Failure),
                };
                wrapped(&format!("{mark} {code} — {detail}"), w, role)
            }
            Note::NotRun { detail } => wrapped(&format!("· {detail}"), w, Role::Faint),
            Note::Pane {
                line,
                said,
                reason,
                closed,
            } => {
                let (mark, role) = if *closed {
                    ("·", Role::Faint)
                } else {
                    ("×", Role::Pending)
                };
                let mut rows: Vec<String> = Vec::with_capacity(said.len() + 1);
                rows.push(format!("{mark} {line} — {reason}"));
                // The program's own rows, indented under the line that ran it.
                rows.extend(said.iter().map(|l| format!("    {l}")));
                rows.iter().flat_map(|l| wrapped(l, w, role)).collect()
            }
            Note::Decided {
                summary,
                outcome,
                by_kind,
                by_identity,
                basis,
                late,
            } => {
                let (word, role) = match outcome {
                    Settled::Allowed(id) => (format!("allowed ({id})"), Role::Success),
                    Settled::Refused(id) => (format!("REFUSED ({id})"), Role::Failure),
                    Settled::Cancelled => ("cancelled".to_string(), Role::Pending),
                    // A deadline is not an answer, and must not read like one.
                    Settled::TimedOut => (
                        "NOT ANSWERED — the deadline decided it".to_string(),
                        Role::Failure,
                    ),
                };
                let who = if by_identity.is_empty() {
                    by_kind.clone()
                } else {
                    format!("{by_kind} {by_identity}")
                };
                let late = if *late {
                    " · an answer arrived after it had settled"
                } else {
                    ""
                };
                let basis = if basis.is_empty() {
                    String::new()
                } else {
                    format!(" ({basis})")
                };
                wrapped(
                    &format!("? {summary} — {word}, by {who}{basis}{late}"),
                    w,
                    role,
                )
            }
        }
    }

    /// **One note, as the conversation draws it**: at most [`NOTE_LINES`] lines and a seam
    /// naming where the rest is.
    ///
    /// **A pane's ending is the one note that is not folded**, and the reason is the tail:
    /// what a program says as it dies is the *last* thing it printed, so a fold that kept the
    /// first three lines would keep the least useful three. Its length is bounded where it is
    /// captured instead (letibot's `PANE_LAST_LINES`).
    pub fn lines(&self, w: usize) -> Vec<Line> {
        let all = self.unfolded(w);
        if matches!(self, Note::Pane { .. }) || all.len() <= NOTE_LINES {
            return all;
        }
        let hidden = all.len() - NOTE_LINES;
        let mut out: Vec<Line> = all.into_iter().take(NOTE_LINES).collect();
        out.push(one(format!("  … +{hidden} lines · /notes"), Role::Faint));
        out
    }
}

/// `s` cleaned, wrapped to `w`, each row in `role`.
fn wrapped(s: &str, w: usize, role: Role) -> Vec<Line> {
    wrap(&clean(s), w)
        .into_iter()
        .map(|l| one(l, role))
        .collect()
}

/// One note in the listing, and why it is not on the screen if it is not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listed {
    pub note: Note,
    /// Retired (`/notes dismiss`): hidden, still counted.
    pub retired: bool,
    /// From before the conversation's window: a fact the head holds and has chosen not to
    /// plant in the conversation.
    pub before: bool,
}

/// **The `/notes` listing**: every note the head holds, in the order the conversation has
/// them, numbered for `/notes dismiss N`.
///
/// **Including the ones it is not drawing.** A retired note and a note from before the
/// window are both marked, and marked differently: two different reasons for an absence must
/// not look like one.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NotesList {
    pub notes: Vec<Listed>,
}

impl NotesList {
    /// Every row of the listing, each note unfolded at `w` columns.
    pub fn lines(&self, w: usize) -> Vec<Line> {
        let n = self.notes.len();
        let retired = self.notes.iter().filter(|l| l.retired).count();
        let before = self.notes.iter().filter(|l| l.before).count();
        let mut out = vec![Line::raw(if n == 0 {
            "this head holds no notes. A note is a disclosure: a guard that fired, a \
             decision that settled, a sentence the daemon interrupted with. They are \
             in the session log whether or not this head is showing them."
                .to_string()
        } else {
            format!(
                "{n} note(s), {retired} retired{} — the log holds the durable fact; a note is \
                 how a head shows it once",
                if before == 0 {
                    String::new()
                } else {
                    format!(", {before} from before this window")
                }
            )
        })];
        for (i, l) in self.notes.iter().enumerate() {
            let mut marks: Vec<&str> = Vec::new();
            if l.retired {
                marks.push("retired");
            }
            if l.before {
                marks.push("before this window");
            }
            let mark = if marks.is_empty() {
                String::new()
            } else {
                format!("[{}]", marks.join(", "))
            };
            out.push(Line::raw(format!("{:>3}  {mark}", i + 1)));
            // **The same renderer the transcript uses, unfolded.** A listing that hid the
            // tail of the very thing it exists to make findable would be the defect again.
            out.extend(l.note.unfolded(w));
        }
        if n > 0 {
            out.push(Line::default());
            out.push(Line::raw(
                "/notes dismiss [N|all] retires one, or every one · /notes restore brings \
                 them all back",
            ));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::testing::{plain, role_of};

    fn warned(class: NoteClass, detail: &str) -> Note {
        Note::Warned {
            code: "gate_timeout".into(),
            detail: detail.into(),
            class,
        }
    }

    /// The register is the code's class, and the mark goes with it.
    #[test]
    fn a_warning_is_drawn_in_its_class() {
        for (class, mark, role) in [
            (NoteClass::Routine, "·", Role::Faint),
            (NoteClass::Refused, "×", Role::Pending),
            (NoteClass::Failure, "!", Role::Failure),
        ] {
            let l = warned(class, "it timed out").lines(80);
            assert_eq!(l[0].plain(), format!("{mark} gate_timeout — it timed out"));
            assert_eq!(role_of(&l[0], mark), Some(role));
        }
    }

    /// A long note folds to three lines and a seam in the conversation, and is whole in the
    /// listing; a pane's ending is never folded.
    #[test]
    fn a_long_note_folds_and_the_listing_keeps_it_whole() {
        let long = warned(NoteClass::Failure, &"the rule says no. ".repeat(20));
        let folded = plain(&long.lines(40));
        assert_eq!(folded.len(), NOTE_LINES + 1);
        assert!(folded.last().unwrap().contains("· /notes"), "{folded:#?}");
        let list = NotesList {
            notes: vec![Listed {
                note: long.clone(),
                retired: true,
                before: false,
            }],
        };
        let rows = plain(&list.lines(40));
        assert!(rows[0].starts_with("1 note(s), 1 retired"), "{rows:#?}");
        assert_eq!(rows[1], "  1  [retired]");
        assert_eq!(rows.len(), 2 + long.unfolded(40).len() + 2);
        let pane = Note::Pane {
            line: "!term mc".into(),
            said: (0..6).map(|i| format!("row {i}")).collect(),
            reason: "it exited 1".into(),
            closed: false,
        };
        assert_eq!(pane.lines(80).len(), 7, "a pane's ending is not folded");
        assert_eq!(role_of(&pane.lines(80)[0], "×"), Some(Role::Pending));
    }

    /// A settled decision says who decided and how, and a deadline is not an answer.
    #[test]
    fn a_decision_note_names_the_decider_and_a_deadline_is_not_an_answer() {
        let d = |outcome| Note::Decided {
            summary: "`bash` wants exec access".into(),
            outcome,
            by_kind: "operator".into(),
            by_identity: "dead".into(),
            basis: String::new(),
            late: false,
        };
        let l = d(Settled::Allowed("allow_once".into())).lines(200);
        assert_eq!(
            l[0].plain(),
            "? `bash` wants exec access — allowed (allow_once), by operator dead"
        );
        assert_eq!(l[0].spans[0].style.top(), Role::Success);
        let l = d(Settled::TimedOut).lines(200);
        assert!(l[0].plain().contains("NOT ANSWERED"));
        assert_eq!(l[0].spans[0].style.top(), Role::Failure);
    }

    /// §3.1: an escape in a note's foreign text does not survive into the row.
    #[test]
    fn a_notes_foreign_text_is_cleaned() {
        let l = warned(NoteClass::Routine, "a\x1b[31mred\x1b[0m end").lines(80);
        assert_eq!(l[0].plain(), "· gate_timeout — ared end");
    }
}
