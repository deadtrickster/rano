//! How a tool call ended: the one word, the one register, and the reason — one list, read by every
//! widget that draws a call (the live card and the settled row).

use crate::style::Role;

/// How a tool call ended.
///
/// It mirrors the transcript's distinctions rather than collapsing them: abstention is not a flavour
/// of success and must not read like one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Ok,
    /// The tool declined to act and said why. Not a failure and not a success.
    Abstained(String),
    Failed(String),
    /// A person or a policy refused the call. Carries the sentence.
    Denied(String),
    /// The turn was interrupted while this call was in flight.
    Interrupted,
    /// The call ran out of its own time and was killed: its own variant because `failed · timed out`
    /// cannot be told from a command that ran and returned an error, and the two want different next
    /// moves.
    Timeout,
    /// The call never ran, and the daemon said why. Not a `Failed`: nothing was attempted, and a retry
    /// is not obviously the answer until the `why` has been read.
    NotRun(String),
    /// Still running, in the background, and reachable. Carries the sentence with the handle in it.
    ///
    /// Its own variant as `Abstained` is: rendered as `Failed` it reads as something to retry, and as
    /// `Ok` as something that finished with nothing to say — both wrong about a working process.
    Backgrounded(String),
}

impl Outcome {
    /// The register this outcome is drawn in — the one mapping. A row asking the question a second
    /// time with a coarser `!matches!(outcome, Ok)` test puts `backgrounded`, `denied` and `abstained`
    /// all in `Failure`.
    pub fn role(&self) -> Role {
        match self {
            Outcome::Ok => Role::Success,
            Outcome::Abstained(_) => Role::Attention,
            Outcome::Denied(_) => Role::Attention,
            Outcome::Failed(_) => Role::Failure,
            Outcome::Interrupted => Role::Failure,
            // Faint, not attention, and the call is over the moment it is backgrounded: a card that
            // stayed lit for the life of the job behind it duplicated what the composer's edge and the
            // jobs pane already say. The job's liveness is theirs.
            Outcome::Backgrounded(_) => Role::Faint,
            // A call that timed out or never ran is not work that is happening.
            Outcome::Timeout | Outcome::NotRun(_) => Role::Failure,
        }
    }

    /// The register a settled call's row is drawn in, through [`Outcome::role`] so the row cannot
    /// disagree with the card the live call was drawn as — with `ok` faint rather than success, since
    /// a green line under every command is a colour that says nothing.
    pub fn row_role(&self) -> Role {
        match self {
            Outcome::Ok => Role::Faint,
            other => other.role(),
        }
    }

    /// The word this outcome prints — the one list, so the word does not change as the row lands.
    /// Shouted where the distinction requires it: abstention is not a flavour of success, and a refusal
    /// is not a flavour of failure.
    pub fn word(&self) -> &'static str {
        match self {
            Outcome::Ok => "ok",
            Outcome::Abstained(_) => "ABSTAINED",
            Outcome::Failed(_) => "failed",
            Outcome::Denied(_) => "REFUSED",
            Outcome::Interrupted => "interrupted",
            Outcome::Timeout => "timeout",
            Outcome::NotRun(_) => "not run",
            // `backgrounded`, not a shouted still-running: the call is over, what continues is a job
            // with a handle, and the handle is in the reason beside this word.
            Outcome::Backgrounded(_) => "backgrounded",
        }
    }

    /// The why, or nothing — the other half of [`Outcome::word`], one list for the same reason.
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

    /// One call reads the same word whatever row draws it.
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

    #[test]
    fn a_settled_rows_register_comes_from_the_outcome_not_from_a_not_ok_test() {
        // The two the operator named that are still *something to look at*: neither is a failure.
        for waiting in [
            Outcome::Denied("the call was denied (d1)".into()),
            Outcome::Abstained("nothing to do".into()),
        ] {
            assert_eq!(waiting.row_role(), Role::Attention, "{waiting:?}");
        }
        // A backgrounded call is neither loud nor lit: finished work whose product is a job.
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
