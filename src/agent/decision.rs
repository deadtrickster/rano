//! **The permission card**: a call waiting on the person, its ladder of options, and what
//! a call's row says about how it was decided once it has been.
//!
//! Ported from letibot's `crates/tui/src/ui/cards/decision.rs`.

use crate::render::Line;
use crate::style::Role;

use super::Fold;
use super::text::{clean, one, wrap};

/// How a settled decision went, in the word the row prints.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Allowed,
    Refused,
    Cancelled,
    /// The deadline passed with nobody answering.
    NotAnswered,
}

impl Verdict {
    /// An option the person (or a policy) selected, by its id: an `allow…` option allowed,
    /// anything else refused — letibot's reading of `DecisionOutcome::Selected`.
    pub fn of_option(option_id: &str) -> Verdict {
        if option_id.starts_with("allow") {
            Verdict::Allowed
        } else {
            Verdict::Refused
        }
    }

    pub fn word(self) -> &'static str {
        match self {
            Verdict::Allowed => "allowed",
            Verdict::Refused => "refused",
            Verdict::Cancelled => "cancelled",
            Verdict::NotAnswered => "not answered",
        }
    }
}

/// What a guard model said about a call, when one was asked — or layer A's own answer
/// arriving through the same door, when `consulted` is false.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Advice {
    /// Whether a model was actually asked. An oracle that was NOT consulted did not say
    /// anything, and the card must not claim it did.
    pub consulted: bool,
    /// Who answered: `oracle-local`, a model name.
    pub by: String,
    pub latency_ms: u64,
    /// What it would do: `admit`, `deny`, `ask`, `unavailable`.
    pub would: String,
    /// Its sentence.
    pub basis: String,
    /// The operator's own phrases it grounded the verdict in.
    pub cites: Vec<String>,
    /// Why a consulted guard did not decide, as the daemon's token: `out_of_room`,
    /// `unreadable`, `could_not_decide`, `between_thresholds`, or one this build does not
    /// know (shown, not swallowed).
    pub unsure: Option<String>,
}

/// The decision a settled call was gated by.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettledDecision {
    pub verdict: Verdict,
    /// The decider's kind: `operator`, `policy`, `oracle`.
    pub by_kind: String,
    /// The decider's identity, when it has one: a user name.
    pub by_identity: String,
    /// The ask as the daemon wrote it.
    pub summary: String,
    /// The DECIDER's reason — for an operator answer, `dead chose `allow_once` at the head`.
    pub basis: String,
    pub advice: Option<Advice>,
}

impl SettledDecision {
    fn who(&self) -> String {
        if self.by_identity.is_empty() {
            self.by_kind.clone()
        } else {
            format!("{} {}", self.by_kind, self.by_identity)
        }
    }

    /// The one line that survives every fold: `· allowed, by operator dead`.
    pub fn headline(&self) -> String {
        format!("· {}, by {}", self.verdict.word(), self.who())
    }
}

/// The decision a settled call was gated by, in the dim register: the approval is
/// a fact about the call, not a stray note. Folded it is one line — who decided
/// and how; open it adds what the oracle was shown and what it said back. Shared
/// by the one-line (inline) and the folded arms of a tool row, because a gated call
/// whose result fit on the header is no less gated for it.
pub fn decision_lines(d: &SettledDecision, tools: Fold, w: usize) -> Vec<Line> {
    let mut out = vec![one(format!("  {}", d.headline()), Role::Faint)];
    if tools.is_open() {
        for l in decision_detail(d, w.saturating_sub(4)) {
            out.push(one(format!("    {l}"), Role::Faint));
        }
    }
    out
}

/// **The two reasons a settled decision carries, labelled as whose they are.**
///
/// `basis` is the DECIDER's — for an operator answer, `dead chose `allow_once` at
/// the head`. `advice` is the guard model's, and only exists when one was
/// consulted. They used to be one line, rendered as `oracle: {basis}`, which under
/// `/supervise` printed the operator's own words under the oracle's name.
///
/// One function so the card and the settled row cannot label them differently.
/// Returns wrapped, unpainted lines; each caller indents and paints its own way.
pub fn decision_detail(d: &SettledDecision, w: usize) -> Vec<String> {
    let mut out = Vec::new();
    // **§3.1, and this is the last of the untrusted free text on a row.** Three
    // sentences here are somebody else's: the ask the daemon wrote, the DECIDER's
    // basis (a person's words, or a policy rule), and the guard model's verdict with
    // the operator's phrases it cites. All of them are cleaned before they are wrapped.
    if !d.summary.is_empty() {
        out.extend(wrap(&clean(&format!("asked: {}", d.summary)), w));
    }
    if !d.basis.is_empty() {
        // Named by the decider's own kind, so "decided:" never stands in for a
        // model when a person chose, or the reverse.
        let who = if d.by_kind.is_empty() {
            "decided"
        } else {
            &d.by_kind
        };
        out.extend(wrap(&clean(&format!("{who}: {}", d.basis)), w));
    }
    match &d.advice {
        Some(a) => {
            // **The same distinction the card draws**: a verdict from an oracle that was
            // asked, and layer A's answer from one that was not.
            out.extend(wrap(
                &clean(&if a.consulted {
                    format!(
                        "oracle ({}, {}ms) would {}: {}",
                        a.by, a.latency_ms, a.would, a.basis
                    )
                } else {
                    format!("no model verdict — {}", a.basis)
                }),
                w,
            ));
            // **Empty cites is loud.** An authorisation the oracle could not ground
            // in anything the operator said is a different fact from one it grounded
            // in four utterances, and rendering nothing for the first makes them
            // look the same.
            if a.cites.is_empty() {
                out.extend(wrap(
                    "oracle cited: nothing — it could not ground this in anything you said",
                    w,
                ));
            } else {
                for c in &a.cites {
                    out.extend(wrap(&clean(&format!("oracle cited: {c}")), w));
                }
            }
        }
        // Said out loud rather than left blank: "no oracle was asked" and "an
        // oracle was asked and said nothing" are different, and a blank looks
        // like the second.
        None => out.extend(wrap("no oracle was consulted for this one", w)),
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::testing::plain;

    fn settled(advice: Option<Advice>) -> SettledDecision {
        SettledDecision {
            verdict: Verdict::of_option("allow_once"),
            by_kind: "operator".into(),
            by_identity: "dead".into(),
            summary: "`bash` wants exec access to `ls`".into(),
            basis: "dead chose `allow_once` at the head".into(),
            advice,
        }
    }

    #[test]
    fn folded_it_is_who_and_how_and_open_it_labels_each_reason_by_its_speaker() {
        let d = settled(None);
        let folded = plain(&decision_lines(&d, Fold::Folded, 80));
        assert_eq!(folded, vec!["  · allowed, by operator dead"]);
        let open = plain(&decision_lines(&d, Fold::Open, 80));
        assert_eq!(
            open,
            vec![
                "  · allowed, by operator dead",
                "    asked: `bash` wants exec access to `ls`",
                "    operator: dead chose `allow_once` at the head",
                "    no oracle was consulted for this one",
            ]
        );
        for l in decision_lines(&d, Fold::Open, 80) {
            assert_eq!(l.spans[0].style.top(), Role::Faint);
        }
    }

    #[test]
    fn an_oracle_that_was_not_asked_said_nothing_and_empty_cites_are_loud() {
        let asked = settled(Some(Advice {
            consulted: true,
            by: "oracle-local".into(),
            latency_ms: 812,
            would: "admit".into(),
            basis: "the operator asked for it".into(),
            ..Advice::default()
        }));
        let open = decision_detail(&asked, 200).join("\n");
        assert!(
            open.contains("oracle (oracle-local, 812ms) would admit: the operator asked for it"),
            "{open}"
        );
        assert!(open.contains("oracle cited: nothing"), "{open}");
        let not_asked = settled(Some(Advice {
            consulted: false,
            would: "unavailable".into(),
            basis: "an always-ask rule".into(),
            ..Advice::default()
        }));
        let open = decision_detail(&not_asked, 200).join("\n");
        assert!(
            open.contains("no model verdict — an always-ask rule"),
            "{open}"
        );
        assert!(!open.contains("would unavailable"), "{open}");
    }

    #[test]
    fn the_verdict_words() {
        assert_eq!(Verdict::of_option("allow_always").word(), "allowed");
        assert_eq!(Verdict::of_option("reject_once").word(), "refused");
        assert_eq!(Verdict::Cancelled.word(), "cancelled");
        assert_eq!(Verdict::NotAnswered.word(), "not answered");
    }
}
