//! **How a tool call ended**: the one word, the one register, and the reason — one list,
//! read by every widget that draws a call (the live card and the settled row).
//!
//! Ported from letibot's `letibot_ui::card::Outcome` and `ui/transcript/call.rs`
//! (`display_outcome`, `outcome_word`, `outcome_role`, `outcome_why`). A host maps its own
//! outcome type onto this once; letibot's `display_outcome` is that mapping and says why
//! it is the right place for the cost to land.

use crate::style::Role;

/// How a tool call ended.
///
/// Mirrors the transcript's distinctions rather than collapsing them, because §8.2's
/// rule — *abstention is not a flavour of success and must not read like one* — is a rule
/// about this display.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Ok,
    /// The tool declined to act and said why. Not a failure and not a success.
    Abstained(String),
    Failed(String),
    /// A person or a policy refused the call. Carries the sentence
    /// (letibot: `the call was denied ({req_id})`).
    Denied(String),
    /// The turn was interrupted while this call was in flight.
    Interrupted,
    /// **The call ran out of its own time and was killed.** Its own variant because the word is
    /// its own: `failed · timed out` is a sentence about a `failed`, and a reader cannot tell it
    /// from a command that ran and returned an error — the two want different next moves.
    Timeout,
    /// **The call never ran, and the daemon said why.** Not a `Failed`: nothing was attempted, and
    /// a retry is not obviously the answer until the `why` has been read.
    NotRun(String),
    /// **Still running, in the background, and reachable.** Carries the sentence with the
    /// handle in it (letibot: ``as `j4` after 0.4s — `/job j4 out` to read it``).
    ///
    /// Its own variant for the same reason `Abstained` is one: a backgrounded call rendered as
    /// `Failed` reads as something to retry, and rendered as `Ok` reads as something that
    /// finished with nothing to say. Both are wrong about a process that is still working.
    Backgrounded(String),
}

impl Outcome {
    /// **The register this outcome is drawn in** — the one mapping.
    ///
    /// A settled row needs the same answer the card's own header gets, and the defect this
    /// exists to prevent is a row asking the question a second time with a coarser test:
    /// `let bad = !matches!(outcome, Ok)` puts `backgrounded`, `denied` and `abstained` all in
    /// `Failure`. The operator, looking at a command the harness had just backgrounded: *"why on
    /// earth backgrounding message is in red"*.
    pub fn role(&self) -> Role {
        match self {
            Outcome::Ok => Role::Success,
            Outcome::Abstained(_) => Role::Attention,
            Outcome::Denied(_) => Role::Attention,
            Outcome::Failed(_) => Role::Failure,
            Outcome::Interrupted => Role::Failure,
            // **FAINT, not Attention, and the call is OVER the moment it is backgrounded.**
            //
            // It was `Attention`, which meant the card of a call that had already returned
            // stayed lit for the whole life of the job behind it, while the composer's edge and
            // the jobs pane carried the same job's liveness. The operator ruled against
            // re-reading the job's current state for it: *"wait, color change can mean some
            // rerenders, so lets make it white as soon as job starts"*. So the call draws settled
            // from the start, and **the job's liveness is the jobs pane's and the edge's**.
            Outcome::Backgrounded(_) => Role::Faint,
            // A call that timed out or never ran is not work that is happening.
            Outcome::Timeout | Outcome::NotRun(_) => Role::Failure,
        }
    }

    /// **The register a settled call's ROW is drawn in** — R51 item 9.
    ///
    /// Through [`Outcome::role`], so there is ONE outcome→register mapping and the row cannot
    /// disagree with the card the live call was drawn as. One local decision is layered on it:
    /// `ok` is drawn FAINT rather than `Success` — a green line under every command is a colour
    /// that says nothing, and the boring case is most of them. That is a decision about one row
    /// rather than about the mapping, so it is applied here, on top.
    pub fn row_role(&self) -> Role {
        match self {
            Outcome::Ok => Role::Faint,
            other => other.role(),
        }
    }

    /// **The word this outcome prints — the ONE list.**
    ///
    /// MEASURED in letibot before there was one list, on the same call, live and settled:
    ///
    /// ```text
    ///   live       refused            failed · timed out        failed · not run — {why}
    ///   settled    REFUSED            timeout                   not run
    /// ```
    ///
    /// — the word changed as the row landed. Shouted where §8.2 requires it: abstention is not a
    /// flavour of success, and a refusal is not a flavour of failure.
    pub fn word(&self) -> &'static str {
        match self {
            Outcome::Ok => "ok",
            Outcome::Abstained(_) => "ABSTAINED",
            Outcome::Failed(_) => "failed",
            Outcome::Denied(_) => "REFUSED",
            Outcome::Interrupted => "interrupted",
            Outcome::Timeout => "timeout",
            Outcome::NotRun(_) => "not run",
            // **`backgrounded`, not `STILL RUNNING`.** The call is over; what continues is a JOB
            // with a handle, and the handle is in the reason beside this word. Lowercase, because
            // it is a fact about how the call ended rather than a decision of the operator's.
            Outcome::Backgrounded(_) => "backgrounded",
        }
    }

    /// **The why, or nothing** — the other half of [`Outcome::word`], one list for the same
    /// reason. The caller supplies the sentences: this returns what the outcome was handed.
    pub fn reason(&self) -> Option<&str> {
        match self {
            Outcome::Ok | Outcome::Interrupted | Outcome::Timeout => None,
            Outcome::Abstained(r)
            | Outcome::Failed(r)
            | Outcome::Denied(r)
            | Outcome::NotRun(r)
            | Outcome::Backgrounded(r) => Some(r),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// letibot `app/tests/tools.rs::one_call_reads_the_same_word_whatever_row_draws_it`,
    /// with the host's mapping (`display_outcome`) already applied to each reason.
    #[test]
    fn one_call_reads_the_same_word_whatever_row_draws_it() {
        let cases: Vec<(Outcome, &str, Option<&str>)> = vec![
            (Outcome::Ok, "ok", None),
            (
                Outcome::Abstained("no answer in the corpus".into()),
                "ABSTAINED",
                Some("no answer in the corpus"),
            ),
            (
                Outcome::Failed("exit 101".into()),
                "failed",
                Some("exit 101"),
            ),
            (
                Outcome::Denied("the call was denied (req_1)".into()),
                "REFUSED",
                Some("the call was denied (req_1)"),
            ),
            (Outcome::Timeout, "timeout", None),
            (
                Outcome::NotRun("the turn was interrupted".into()),
                "not run",
                Some("the turn was interrupted"),
            ),
            (
                Outcome::Backgrounded("as `j4` after 0.4s — `/job j4 out` to read it".into()),
                "backgrounded",
                Some("as `j4` after 0.4s — `/job j4 out` to read it"),
            ),
        ];
        for (outcome, word, why) in cases {
            assert_eq!(outcome.word(), word, "{outcome:?}");
            assert_eq!(outcome.reason(), why, "{outcome:?}");
        }
    }

    /// letibot `app/tests/tools.rs::a_settled_rows_register_comes_from_the_outcome_not_from_a_not_ok_test`.
    #[test]
    fn a_settled_rows_register_comes_from_the_outcome_not_from_a_not_ok_test() {
        // The two the operator named that are still *something to look at*: neither is a failure.
        for waiting in [
            Outcome::Denied("the call was denied (d1)".into()),
            Outcome::Abstained("nothing to do".into()),
        ] {
            assert_eq!(waiting.row_role(), Role::Attention, "{waiting:?}");
        }
        // **A backgrounded call is neither loud nor lit**: finished work whose product is a job.
        assert_eq!(
            Outcome::Backgrounded("as `j1` after 0.0s — read it".into()).row_role(),
            Role::Faint,
            "the call is over the moment it is backgrounded — the JOB is what runs on"
        );
        // A real failure is still loud, and `ok` is the quiet case the row chose.
        assert_eq!(Outcome::Failed("boom".into()).row_role(), Role::Failure);
        assert_eq!(Outcome::Ok.row_role(), Role::Faint);
        assert_eq!(Outcome::Ok.role(), Role::Success);
        let nr = Outcome::NotRun("the scope closed".into());
        assert_eq!(nr.row_role(), nr.role());
    }
}
