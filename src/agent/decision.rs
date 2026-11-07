//! The permission card: a call waiting on the person, its ladder of options, and what a call's row
//! says about how it was decided once it has been.

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
    /// An option the person (or a policy) selected, by its id: an `allow…` option allowed, anything
    /// else refused.
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

/// What a guard model said about a call, when one was asked — or layer A's own answer through the
/// same door, when `consulted` is false.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Advice {
    /// Whether a model was actually asked: an oracle not consulted said nothing, and the card must
    /// not claim it did.
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
    /// Why a consulted guard did not decide, as the daemon's token — or one this build does not
    /// know, which is shown rather than swallowed.
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

/// The decision a settled call was gated by, in the dim register: a fact about the call, not a
/// stray note. Folded it is one line; open it adds what the oracle was shown and what it said back.
pub fn decision_lines(d: &SettledDecision, tools: Fold, w: usize) -> Vec<Line> {
    let mut out = vec![one(format!("  {}", d.headline()), Role::Faint)];
    if tools.is_open() {
        for l in decision_detail(d, w.saturating_sub(4)) {
            out.push(one(format!("    {l}"), Role::Faint));
        }
    }
    out
}

/// The two reasons a settled decision carries, labelled as whose they are: `basis` is the
/// decider's, `advice` the guard model's and only when one was consulted. They used to be one line,
/// which printed the operator's own words under the oracle's name.
///
/// Returns wrapped, unpainted lines; each caller indents and paints its own way.
pub fn decision_detail(d: &SettledDecision, w: usize) -> Vec<String> {
    let mut out = Vec::new();
    // The last untrusted free text on a row: the ask, the decider's basis and the guard's verdict
    // with the phrases it cites. All of them are cleaned before they are wrapped.
    if !d.summary.is_empty() {
        out.extend(wrap(&clean(&format!("asked: {}", d.summary)), w));
    }
    if !d.basis.is_empty() {
        // Named by the decider's own kind, so a person's words never print under a model's name.
        let who = if d.by_kind.is_empty() {
            "decided"
        } else {
            &d.by_kind
        };
        out.extend(wrap(&clean(&format!("{who}: {}", d.basis)), w));
    }
    match &d.advice {
        Some(a) => {
            // A verdict from an oracle that was asked, and layer A's answer from one that was not.
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
            // Empty cites is loud: an authorisation the oracle could not ground in anything the
            // operator said is its own fact, and rendering nothing makes the two look alike.
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
        // Said out loud rather than left blank: "no oracle was asked" and "an oracle was asked and
        // said nothing" are different, and a blank looks like the second.
        None => out.extend(wrap("no oracle was consulted for this one", w)),
    }
    out
}

/// One rung of a permission's ladder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecisionOption {
    /// What the option says: `Allow once`, `Always allow bash(cargo test *)`.
    pub label: String,
    /// The id typing it answers with: `allow_once`, `deny`.
    pub option_id: String,
    pub kind: OptionKind,
}

/// The kinds of answer a permission offers: an always-allow is offered with a glob to edit, and a
/// reject-always asks for words.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OptionKind {
    AllowOnce,
    AllowSession,
    AllowAlways,
    RejectOnce,
    RejectAlways,
}

/// What silence does, in the daemon's own three words.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OnTimeout {
    #[default]
    Deny,
    Allow,
    Ask,
}

impl OnTimeout {
    /// The order a person reads a card in is the question, the choices, then what happens if they do
    /// nothing — and the upper case is the point: `RUNS` does something nobody asked for.
    pub fn clause(self) -> &'static str {
        match self {
            OnTimeout::Deny => "if nobody answers, nothing runs",
            OnTimeout::Allow => "if nobody answers, it RUNS anyway",
            OnTimeout::Ask => "if nobody answers, the guard model decides",
        }
    }
}

/// A file the action would write, as the classifier placed it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriteTarget {
    pub path: String,
    /// The classifier saw a write and could not read where to: `open(sys.argv[1], 'w')`.
    pub unresolved: bool,
}

/// Whose call this is, when it is a subagent's and not this session's own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubagentAsk {
    pub handle: String,
    pub task: String,
}

/// An open decision: a call waiting on the person.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DecisionCard {
    /// `permission`, `question`, … — drawn in brackets after the ask; `question` changes the card.
    pub kind: String,
    /// The ask as the daemon wrote it: `` `bash` wants exec access to `cargo test` ``.
    pub summary: String,
    /// The thing being asked about — the command, the path. Its own lines.
    pub target: String,
    pub subagent: Option<SubagentAsk>,
    pub write_targets: Vec<WriteTarget>,
    /// Layer A's deterministic reading.
    pub detail: String,
    /// The tool's declared access: `exec`, `read`, … Empty from a daemon that did not say.
    pub access: String,
    /// Why it is asking, in the words of whoever is stuck (on a question, the model's).
    pub because: String,
    pub advice: Option<Advice>,
    /// A permission's ladder.
    pub options: Vec<DecisionOption>,
    /// A question's ladder: the model's prose, offered back as an index.
    pub choices: Vec<String>,
    /// The highlighted rung (clamped to the ladder).
    pub selected: usize,
    /// When the gate gives up, on the same clock as `now_ms`. `None`: it waits forever, and nothing
    /// is drawn.
    pub deadline_ms: Option<u64>,
    pub now_ms: u64,
    pub on_timeout: OnTimeout,
}

impl DecisionCard {
    fn is_question(&self) -> bool {
        self.kind == "question"
    }

    /// The rows the ladder walks: a question's `choices`, a permission's `options`.
    pub fn rows(&self) -> usize {
        if self.is_question() {
            self.choices.len()
        } else {
            self.options.len()
        }
    }

    /// The card, split where it must be: `content` — the question, what it is about, the evidence —
    /// shrinks to the room left and scrolls, while `choices` — the ladder, the deadline, the hints —
    /// is never trimmed, because a card that dropped its choices is a question with no way to
    /// answer it.
    pub fn split(&self, w: usize) -> (Vec<Line>, Vec<Line>) {
        let (content, choices, _) = self.layout(w);
        (content, choices)
    }

    /// Where each rung of the ladder was drawn, in the `choices` half [`DecisionCard::split`] returns
    /// at the same `w`: ranges, because a rung wraps — a host that re-wrapped to find them would
    /// repeat a layout it does not own, and drift.
    pub fn choice_rows(&self, w: usize) -> Vec<std::ops::Range<usize>> {
        self.layout(w).2
    }

    fn layout(&self, w: usize) -> (Vec<Line>, Vec<Line>, Vec<std::ops::Range<usize>>) {
        let dim = |s: &str| wrap(&clean(s), w).into_iter().map(|l| one(l, Role::Faint));
        // The question, then the thing itself, then the evidence. One line used to carry all three,
        // and a permission an operator cannot evaluate is one they approve out of fatigue.
        let headline =
            ask_without_target(&self.summary, &self.target).unwrap_or_else(|| self.summary.clone());
        // The question is one row, cut rather than wrapped, in yellow: the ladder's own plain
        // colour, so the highlighted rung is the one thing that stands out.
        let mut out: Vec<Line> = vec![one(
            super::text::trim_to(&clean_one(&format!("? {headline} [{}]", self.kind)), w),
            Role::Pending,
        )];
        // Whose call this is, directly under the question, because it changes what is being
        // decided: a subagent's gate posts its card to the root, and one answered for the wrong
        // thing is the defect.
        if let Some(s) = &self.subagent {
            let said = if s.task.is_empty() {
                format!("    a subagent's call — {}", s.handle)
            } else {
                format!("    a subagent's call — {} · {}", s.handle, s.task)
            };
            out.extend(dim(&said));
        }
        if !self.target.is_empty() {
            // Strong rather than attention: the question is in attention, and this is not a second
            // question.
            for l in wrap(&clean(&format!("    {}", self.target)), w) {
                out.push(one(l, Role::Strong));
            }
        }
        // The files this action would write, at the place and in the style the single target has.
        // The count goes above the names (the names elide, the number must not), an unresolved path
        // is drawn in the attention register, and an empty list draws nothing.
        if !self.write_targets.is_empty() {
            let unresolved = self.write_targets.iter().filter(|t| t.unresolved).count();
            if self.write_targets.len() > 1 {
                out.push(one(
                    format!("    {} files:", self.write_targets.len()),
                    Role::Faint,
                ));
            }
            for t in &self.write_targets {
                let r = if t.unresolved {
                    Role::Pending
                } else {
                    Role::Strong
                };
                out.push(one(format!("    {}", clean_one(&t.path)), r));
            }
            // The count of unresolved ones is a sentence: "a write whose target could not be read",
            // or "2 writes whose targets could not be read".
            if unresolved > 0 {
                out.push(one(
                    format!(
                        "    {} whose target could not be read",
                        if unresolved == 1 {
                            "a write".to_string()
                        } else {
                            format!("{unresolved} writes")
                        }
                    ),
                    Role::Pending,
                ));
            }
        }
        if !self.detail.is_empty() {
            out.extend(dim(&format!("  {}", self.detail)));
        }
        // The sentence that joins the headline's declared access to the line above, layer A's
        // reading of the action: without it the card read as arguing with itself. Said only where
        // the declaration is what asks — on a `read` call it would be false.
        const ACCESS_ASKS: &str = "the access is what asks: a tool declared to `exec` is \
                                   asked about on its declaration, and the line above is a \
                                   reading of this action";
        if self.access == "exec" && !self.is_question() {
            out.extend(dim(&format!("  {ACCESS_ASKS}")));
        }
        // Why it is asking, labelled with its speaker: a reader who cannot tell whose sentence it
        // is cannot weigh it against what they know.
        if !self.because.is_empty() {
            out.extend(dim(&format!("  because: {}", self.because)));
        }
        // The model's verdict, above the ladder: at `/mode supervised` a person cannot agree with
        // what they were not shown, and nothing here preselects an option.
        if let Some(a) = &self.advice {
            // The four non-answers and the fifth fact that is one: without `unsure` all five
            // reached the screen as one line, for facts whose remedies differ.
            let said = if a.consulted {
                match a.unsure.as_deref() {
                    Some("out_of_room") => format!(
                        "  the guard ran out of room before it answered — {} (a budget, not an \
                         opinion; `--oracle-max-tokens` is the knob)",
                        a.basis
                    ),
                    Some("unreadable") => {
                        format!("  the guard's reply was not a verdict — {}", a.basis)
                    }
                    Some("could_not_decide") => {
                        format!("  the guard answered unsure — {}", a.basis)
                    }
                    Some("between_thresholds") => format!(
                        "  the guard's two scores fell between the thresholds — {}",
                        a.basis
                    ),
                    // A token this card does not know is shown, not swallowed.
                    Some(other) => format!("  the guard did not decide ({other}) — {}", a.basis),
                    // The fifth fact: a consulted oracle that answered.
                    None => format!("  model says {}: {}", a.would, a.basis),
                }
            } else {
                // An oracle that was not consulted did not say anything.
                format!("  no model verdict — {}", a.basis)
            };
            out.extend(dim(&said));
            // Said when it is a fact, omitted when it is not: an oracle that authorised something
            // while citing none of your words is the case most worth a second look.
            let grounds = if !a.cites.is_empty() {
                Some(format!("cites {}", a.cites.join(" · ")))
            } else if a.would == "admit" {
                Some("cites nothing from your words".to_string())
            } else {
                None
            };
            let tail = match &grounds {
                Some(g) => format!("  {} · {g} · {} ms", a.by, a.latency_ms),
                None => format!("  {} · {} ms", a.by, a.latency_ms),
            };
            out.extend(dim(&tail));
        } else if !self.is_question() {
            // And when nobody was asked, the card says that too: "no oracle consulted" and "the
            // oracle said nothing" must not be the same screen. Only where a gate is.
            out.extend(dim(
                "  no oracle was consulted for this one — the judgement is yours alone",
            ));
        }
        // Here the card stops being content and becomes the answer.
        let mut choices: Vec<Line> = Vec::new();
        // One row per answer, the picked one marked: a ladder the eye can walk is also one Up/Down
        // can walk, and the marker is the thing Enter takes. A question's rows carry no ids.
        let rows: Vec<String> = if self.is_question() {
            self.choices.iter().map(|c| clean_one(c)).collect()
        } else {
            self.options
                .iter()
                .map(|o| format!("{}  ({})", clean_one(&o.label), clean_one(&o.option_id)))
                .collect()
        };
        let sel = self.selected.min(rows.len().saturating_sub(1));
        let mut spans = Vec::with_capacity(rows.len());
        for (i, body) in rows.iter().enumerate() {
            let picked = i == sel;
            let first = choices.len();
            // The id stays on the line: typing it still works, and a reader learning the ladder sees
            // both spellings of one choice.
            for l in wrap(&format!("  {} {body}", if picked { "▸" } else { " " }), w) {
                choices.push(if picked {
                    // Reverse video rather than another colour: the prompt is already in attention,
                    // and a second hue reads as a second kind of thing rather than "this one".
                    super::text::styled(l, crate::render::Style::new().reverse())
                } else {
                    one(l, Role::Pending)
                });
            }
            spans.push(first..choices.len());
        }
        // How long there is, and what silence will do — below the options, because that is the order
        // a person reads, and only where there is a deadline.
        if let Some(deadline) = self.deadline_ms {
            let mut said: Vec<String> = Vec::new();
            said.push(match deadline.checked_sub(self.now_ms) {
                Some(left) => super::text::countdown(left),
                // Past its deadline: the daemon settled it without telling this head, and it must
                // never count into negative seconds, which reads as a rendering fault.
                None => format!(
                    "past its deadline by {}s; the daemon has not said what became of it",
                    (self.now_ms - deadline) / 1000
                ),
            });
            said.push(self.on_timeout.clause().to_string());
            for l in wrap(&format!("  {}", said.join(" · ")), w) {
                choices.push(one(l, Role::Faint));
            }
        }
        // The glob line only when an always-allow is actually on offer: a hint for an option this
        // request does not have teaches the operator to stop reading. A question's hints are its own.
        let hint = if self.is_question() {
            "  ↑↓ to choose · Enter to answer · or type your own answer"
        } else if self
            .options
            .iter()
            .any(|o| o.kind == OptionKind::AllowAlways)
        {
            // The rule the always-allow writes is in the option's own label, so the hint points at
            // editing it: a pattern they cannot see is one they cannot adjust. The run of spaces is
            // its own, byte for byte.
            "  ↑↓ to choose · Enter to answer · or type the id ·              `allow_always <glob>` to widen or narrow the rule shown above"
        } else {
            "  ↑↓ to choose · Enter to answer · or type the id"
        };
        // The hints are trimmed to the card rather than wrapped: they are the least of it, and a
        // wrapped hint is a row the content's window pays for.
        choices.push(one(super::text::trim_to(hint, w), Role::Pending));
        // The option that asks for words says where to type them: its label promised "tell the model
        // why" and the card never said how.
        if self
            .options
            .iter()
            .any(|o| o.kind == OptionKind::RejectAlways)
        {
            choices.push(one(
                super::text::trim_to(
                    "  `deny_and_tell <why>` denies and sends those words to the model",
                    w,
                ),
                Role::Pending,
            ));
        }
        (out, choices, spans)
    }

    /// The whole card as one list (the transcript's shape); a screen that has to fit it uses
    /// [`DecisionCard::split`] and [`card_window`].
    pub fn lines(&self, w: usize) -> Vec<Line> {
        let (mut content, choices) = self.split(w);
        content.extend(choices);
        content
    }
}

super::lines_widget!(DecisionCard);

fn clean_one(s: &str) -> String {
    super::text::clean_line(s)
}

/// The ask with its target taken off the end: `` `bash` wants exec access `` from
/// `` `bash` wants exec access to `cargo test` ``. `None` when the sentence does not end in the
/// target — then the whole sentence is shown and nothing is lost.
pub fn ask_without_target(summary: &str, target: &str) -> Option<String> {
    if target.is_empty() {
        return None;
    }
    let head = summary.strip_suffix(&format!("`{target}`"))?;
    // " to " is the joint in every sentence the daemon writes; trimming it makes the rest read as a
    // heading rather than as a clipped sentence.
    let head = head.trim_end();
    Some(head.strip_suffix(" to").unwrap_or(head).to_string())
}

/// What [`card_window`] drew.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CardWindow {
    /// The seam and the window, in reading order.
    pub lines: Vec<Line>,
    /// The scroll actually used (clamped): the host stores it back.
    pub scroll: usize,
    /// Content lines the viewport showed, seam excluded — what a page key moves by.
    pub shown: usize,
    /// All the content there is.
    pub len: usize,
}

/// The card's content as a window: `room` rows, seam included. The seam says how many lines are out
/// of view and which key moves toward them — never "there is more", since a reader must know
/// whether one line or four hundred are missing. It starts at the head and its seam changes ends
/// with the scroll, and the scroll is clamped here because the drawn length is the width's.
///
/// It keeps its own arithmetic rather than calling `pane::window`, and the two differences are the
/// reasons: a card never pads — its box is the room exactly — and its seam spends a row of `room`
/// that the host reads back as `shown`, where a pane has to place that row and record it for its
/// click map. The SENTENCE is [`crate::agent::pane::seam_said`]'s, so one situation reads one way.
pub fn card_window(content: &[Line], room: usize, scroll: usize) -> CardWindow {
    let len = content.len();
    if content.is_empty() || room == 0 {
        return CardWindow {
            lines: Vec::new(),
            scroll: 0,
            shown: 0,
            len,
        };
    }
    if len <= room {
        return CardWindow {
            lines: content.to_vec(),
            scroll: 0,
            shown: len,
            len,
        };
    }
    // One row of the viewport is the seam, and it is spent even at `room == 1`: a viewport whose
    // whole height says how much is missing is the honest shape of a screen with nowhere for it.
    let shown = room - 1;
    let at = scroll.min(len - shown);
    let above = at;
    let below = len - (at + shown);
    let seam = one(crate::agent::pane::seam_said(above, below), Role::Faint);
    let mut lines = Vec::with_capacity(room);
    if below == 0 {
        lines.push(seam.clone());
    }
    lines.extend(content[at..at + shown].iter().cloned());
    if below != 0 {
        lines.push(seam);
    }
    CardWindow {
        lines,
        scroll: at,
        shown,
        len,
    }
}

/// A permission card fitted to `room` rows: the choices whole, the content windowed into what is
/// left. The host owns `scroll` and stores [`CardWindow::scroll`] back.
pub fn fitted(
    card: &DecisionCard,
    w: usize,
    room: usize,
    scroll: usize,
) -> (Vec<Line>, CardWindow) {
    let (content, choices) = card.split(w);
    let win = card_window(&content, room.saturating_sub(choices.len()), scroll);
    let mut out = win.lines.clone();
    out.extend(choices);
    (out, win)
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

    fn decision_with(kinds: &[OptionKind]) -> DecisionCard {
        DecisionCard {
            kind: "permission".into(),
            summary: "edit a file".into(),
            options: kinds
                .iter()
                .map(|k| DecisionOption {
                    option_id: match k {
                        OptionKind::AllowOnce => "allow_once",
                        OptionKind::AllowSession => "allow_session",
                        OptionKind::AllowAlways => "allow_always",
                        OptionKind::RejectOnce => "deny",
                        OptionKind::RejectAlways => "deny_always",
                    }
                    .to_string(),
                    label: "x".into(),
                    kind: *k,
                })
                .collect(),
            ..DecisionCard::default()
        }
    }

    fn two() -> DecisionCard {
        decision_with(&[OptionKind::AllowOnce, OptionKind::RejectOnce])
    }

    fn drawn(d: &DecisionCard, w: usize) -> String {
        plain(&d.lines(w)).join("\n")
    }

    fn line_with<'a>(lines: &'a [Line], needle: &str) -> &'a Line {
        lines
            .iter()
            .find(|l| l.plain().contains(needle))
            .unwrap_or_else(|| panic!("{needle}: {:?}", plain(lines)))
    }

    #[test]
    fn the_card_states_why_it_is_asking_in_the_speakers_own_words() {
        assert!(!drawn(&two(), 100).contains("because"));
        let mut d = two();
        d.because = "the answer changes which migration I write".into();
        let s = drawn(&d, 100);
        assert!(
            s.contains("because: the answer changes which migration I write"),
            "{s}"
        );
        let lines = d.lines(100);
        assert_eq!(
            line_with(&lines, "because:").spans[0].style.top(),
            Role::Faint
        );
        assert!(
            s.find("because:").unwrap() < s.find("allow_once").unwrap(),
            "{s}"
        );
    }

    #[test]
    fn the_card_says_the_declared_access_is_what_asks_and_only_when_it_is() {
        let says = |access: &str| {
            let mut d = two();
            d.summary = "`bash` wants exec access to `cargo test`".into();
            d.target = "cargo test".into();
            d.detail = "auto — intents [read_file] — auto (a read inside the boundary)".into();
            d.access = access.into();
            drawn(&d, 200)
        };
        let exec = says("exec");
        assert!(exec.contains("a tool declared to `exec`"), "{exec}");
        assert!(
            exec.find("intents [read_file]").unwrap()
                < exec.find("the access is what asks").unwrap(),
            "under the two statements it joins: {exec}"
        );
        for other in ["read", "write", "network", ""] {
            let s = says(other);
            assert!(!s.contains("the access is what asks"), "{other:?}: {s}");
            assert!(s.contains("allow_once"), "{s}");
        }
        let mut q = decision_with(&[]);
        q.kind = "question".into();
        q.access = "exec".into();
        q.choices = vec!["a".into()];
        assert!(!drawn(&q, 200).contains("the access is what asks"));
    }

    #[test]
    fn a_card_that_was_never_taken_to_an_oracle_says_so_and_one_that_was_does_not() {
        let asked = "no oracle was consulted for this one — the judgement is yours alone";
        assert!(drawn(&two(), 100).contains(asked));
        let mut d = two();
        d.advice = Some(Advice {
            consulted: true,
            would: "unavailable".into(),
            by: "adjudicator".into(),
            basis: "the verdict could not be read".into(),
            latency_ms: 4_000,
            ..Advice::default()
        });
        let s = drawn(&d, 100);
        assert!(!s.contains(asked), "{s}");
        assert!(s.contains("model says unavailable"), "{s}");
        let mut q = decision_with(&[]);
        q.kind = "question".into();
        let s = drawn(&q, 100);
        assert!(!s.contains(asked) && !s.contains("model says"), "{s}");
    }

    #[test]
    fn the_glob_hint_is_absent_when_no_rule_can_be_written() {
        let with = decision_with(&[OptionKind::AllowOnce, OptionKind::AllowAlways]);
        assert!(drawn(&with, 100).contains("allow_always <glob>"));
        assert!(!drawn(&two(), 100).contains("<glob>"));
        let tell = decision_with(&[OptionKind::AllowOnce, OptionKind::RejectAlways]);
        assert!(drawn(&tell, 100).contains("`deny_and_tell <why>`"));
    }

    #[test]
    fn the_command_being_approved_gets_its_own_line() {
        let mut d = two();
        let cmd = "cargo test -p letibot-tools --test clauses -- --nocapture";
        d.summary = format!("`bash` wants exec access to `{cmd}`");
        d.target = cmd.to_string();
        d.detail = "ask — intents [execute_code] over [host_other]".into();
        let lines = d.lines(100);
        let own = line_with(&lines, cmd);
        assert_eq!(
            own.plain().trim(),
            cmd,
            "the command shares its line with prose"
        );
        assert_eq!(own.spans[0].style.top(), Role::Strong);
        let head = lines[0].plain();
        assert_eq!(head, "? `bash` wants exec access [permission]");
        assert_eq!(lines[0].spans[0].style.top(), Role::Pending);
        assert!(
            plain(&lines)
                .iter()
                .any(|l| l.contains("intents [execute_code]"))
        );
        assert_eq!(
            ask_without_target("`bash` wants exec access to `ls`", "ls").as_deref(),
            Some("`bash` wants exec access")
        );
        assert_eq!(ask_without_target("something else", "ls"), None);
    }

    #[test]
    fn the_five_facts_behind_one_ask_are_five_lines_on_the_card() {
        let rendered = |unsure: Option<&str>| {
            let mut d = two();
            d.advice = Some(Advice {
                consulted: true,
                would: "ask".into(),
                by: "model:test".into(),
                basis: "THE-BASIS".into(),
                unsure: unsure.map(str::to_string),
                latency_ms: 12,
                ..Advice::default()
            });
            drawn(&d, 100)
        };
        let mut seen: Vec<String> = Vec::new();
        for u in [
            Some("could_not_decide"),
            Some("between_thresholds"),
            Some("unreadable"),
            Some("out_of_room"),
            None,
        ] {
            let s = rendered(u);
            assert!(s.contains("THE-BASIS"), "{u:?}: {s}");
            assert!(!seen.contains(&s), "{u:?} renders like another: {s}");
            seen.push(s);
        }
        assert!(rendered(Some("fifth_kind")).contains("fifth_kind"));
    }

    #[test]
    fn the_ladder_is_walked_by_a_marker_and_the_marker_is_the_answer() {
        let mut d = decision_with(&[
            OptionKind::AllowOnce,
            OptionKind::AllowSession,
            OptionKind::RejectOnce,
        ]);
        d.selected = 1;
        let (_, choices) = d.split(100);
        assert_eq!(
            plain(&choices)[..3],
            [
                "    x  (allow_once)",
                "  ▸ x  (allow_session)",
                "    x  (deny)"
            ]
        );
        assert!(
            choices[1].spans[0]
                .style
                .attrs
                .contains(crate::style::Attrs::REVERSE)
        );
        assert_eq!(choices[0].spans[0].style.top(), Role::Pending);
        // Past the end clamps to the last rung rather than marking nothing.
        d.selected = 99;
        assert!(plain(&d.split(100).1)[2].starts_with("  ▸"));
    }

    /// The ladder's row map is the rows the card drew: each rung's range starts on its marker
    /// column, the ranges tile the ladder from row 0 with no gap, and each holds that rung's words.
    #[test]
    fn the_ladder_rows_are_the_rows_the_card_drew() {
        let mut perm = decision_with(&[
            OptionKind::AllowOnce,
            OptionKind::AllowAlways,
            OptionKind::RejectAlways,
        ]);
        perm.options[1].label = "Always allow bash(cargo test --workspace --all-features *)".into();
        perm.options[2].label = "Deny, and tell the model why it should not try this again".into();
        perm.deadline_ms = Some(60_000);
        let question = DecisionCard {
            kind: "question".into(),
            summary: "which one?".into(),
            choices: vec![
                "the short one".into(),
                "a much longer choice that the model wrote out as a whole sentence of prose".into(),
                "third".into(),
            ],
            ..DecisionCard::default()
        };
        for mut d in [perm, question] {
            for sel in 0..3 {
                d.selected = sel;
                for w in [24usize, 30, 45, 60, 200] {
                    let (_, choices) = d.split(w);
                    let rows = d.choice_rows(w);
                    let text = plain(&choices);
                    assert_eq!(rows.len(), d.rows());
                    assert_eq!(rows[0].start, 0, "w={w}");
                    for (i, r) in rows.iter().enumerate() {
                        assert!(!r.is_empty(), "w={w} rung {i}: {rows:?}");
                        if i > 0 {
                            assert_eq!(rows[i - 1].end, r.start, "w={w}: {rows:?}");
                        }
                        let mark = if i == sel { "  ▸ " } else { "    " };
                        assert!(text[r.start].starts_with(mark), "w={w} {i}: {text:#?}");
                        // Every row of the range belongs to rung i: the reverse run is on exactly
                        // the picked rung's rows.
                        for row in r.clone() {
                            let reversed = choices[row]
                                .spans
                                .iter()
                                .any(|s| s.style.attrs.contains(crate::style::Attrs::REVERSE));
                            assert_eq!(reversed, i == sel, "w={w} row {row}: {text:#?}");
                        }
                    }
                    // A wrap actually happened somewhere at the narrow widths.
                    if w <= 30 {
                        assert!(rows.iter().any(|r| r.len() > 1), "w={w}: {rows:?}");
                    }
                    // The row after the ladder is not a rung: the deadline or the hint.
                    let end = rows.last().unwrap().end;
                    assert!(!text[end].starts_with("  ▸"), "w={w}: {text:#?}");
                    let words: String = text[..end].concat().split_whitespace().collect();
                    assert!(
                        words.contains("third") || words.contains("deny_always"),
                        "{words}"
                    );
                }
            }
        }
    }

    #[test]
    fn the_row_count_is_the_kinds_own_field_in_both_directions() {
        assert_eq!(two().rows(), 2);
        let mut q = decision_with(&[]);
        q.kind = "question".into();
        q.choices = vec!["a".into(), "b".into(), "c".into()];
        assert_eq!(q.rows(), 3);
        let s = drawn(&q, 100);
        assert!(s.contains("  ▸ a\n    b\n    c"), "{s}");
        assert!(s.contains("or type your own answer"), "{s}");
    }

    #[test]
    fn the_ladder_survives_a_card_whose_content_is_taller_than_the_screen() {
        let mut d = decision_with(&[
            OptionKind::AllowOnce,
            OptionKind::AllowSession,
            OptionKind::RejectOnce,
        ]);
        d.target = "crates/tui/src/app.rs".into();
        d.detail = (0..40)
            .map(|i| format!("  line {i} of layer A's reading of this call"))
            .collect::<Vec<_>>()
            .join("\n");
        let (out, win) = fitted(&d, 80, 14, 0);
        let screen = plain(&out).join("\n");
        assert_eq!(out.len(), 14, "{screen}");
        for opt in ["allow_once", "allow_session", "deny"] {
            assert!(screen.contains(opt), "`{opt}` was trimmed away:\n{screen}");
        }
        assert!(
            screen.contains("↑↓ to choose · Enter to answer"),
            "{screen}"
        );
        assert!(
            screen
                .lines()
                .any(|l| l.contains("line(s) out of view") && l.contains("pgdn scrolls")),
            "{screen}"
        );
        let at = |s: &str| screen.find(s).unwrap_or_else(|| panic!("{s}: {screen}"));
        assert!(at("? edit a file [permission]") < at("crates/tui/src/app.rs"));
        assert!(at("crates/tui/src/app.rs") < at("line(s) out of view"));
        assert!(at("line(s) out of view") < at("allow_once"));
        assert!(!screen.contains("line 39 of layer A"), "{screen}");
        assert_eq!(win.scroll, 0);
        // Scrolled to the end, the seam moves above the window, and an overshoot clamps.
        let (out, win) = fitted(&d, 80, 14, 1_000);
        let screen = plain(&out).join("\n");
        assert!(screen.contains("line 39 of layer A"), "{screen}");
        assert!(screen.contains("pgup scrolls"), "{screen}");
        assert_eq!(win.scroll, win.len - win.shown);
        // In between, both counts.
        let (out, _) = fitted(&d, 80, 14, 3);
        assert!(
            plain(&out).join("\n").contains("3 above,"),
            "{:?}",
            plain(&out)
        );
    }

    #[test]
    fn the_gate_card_says_how_long_there_is_and_what_silence_does() {
        const NOW: u64 = 1_788_984_000_000;
        let ladder = |deadline: u64, on: OnTimeout| {
            let mut d = two();
            d.now_ms = NOW;
            d.deadline_ms = Some(deadline);
            d.on_timeout = on;
            drawn(&d, 200)
        };
        assert!(ladder(NOW + 300_000, OnTimeout::Deny).contains("expires in 5 min"));
        assert!(ladder(NOW + 181_000, OnTimeout::Deny).contains("expires in 4 min"));
        assert!(ladder(NOW + 119_000, OnTimeout::Deny).contains("1m59s left"));
        assert!(ladder(NOW + 47_000, OnTimeout::Deny).contains("47s left"));
        assert!(!ladder(NOW + 47_500, OnTimeout::Deny).contains("47.5"));
        let deny = ladder(NOW + 300_000, OnTimeout::Deny);
        assert!(deny.contains("if nobody answers, nothing runs"), "{deny}");
        assert!(
            ladder(NOW + 300_000, OnTimeout::Allow).contains("if nobody answers, it RUNS anyway")
        );
        assert!(
            ladder(NOW + 300_000, OnTimeout::Ask)
                .contains("if nobody answers, the guard model decides")
        );
        assert!(deny.find("allow_once").unwrap() < deny.find("if nobody answers").unwrap());
        let expired = ladder(NOW - 14_000, OnTimeout::Allow);
        assert!(
            expired.contains("past its deadline by 14s; the daemon has not said what became of it"),
            "{expired}"
        );
        assert!(
            expired.contains("it RUNS anyway") && !expired.contains('-'),
            "{expired}"
        );
        let bare = drawn(&two(), 200);
        assert!(
            !bare.contains("expires in") && !bare.contains("s left"),
            "{bare}"
        );
        assert!(
            !bare.contains("if nobody answers") && bare.contains("allow_once"),
            "{bare}"
        );
    }

    #[test]
    fn the_card_names_the_files_the_action_would_write() {
        let w = |p: &str, u: bool| WriteTarget {
            path: p.into(),
            unresolved: u,
        };
        let mut d = two();
        d.access = "exec".into();
        d.target = "python3 edit.py".into();
        d.write_targets = vec![w("src/syntax.rs", false)];
        let one_ = drawn(&d, 200);
        assert!(
            one_.contains("src/syntax.rs") && !one_.contains("1 files:"),
            "{one_}"
        );
        d.write_targets = ["a.rs", "b.rs", "c.rs", "d.rs", "e.rs"]
            .iter()
            .map(|p| w(p, false))
            .collect();
        let many = drawn(&d, 200);
        assert!(
            many.find("5 files:").unwrap() < many.find("a.rs").unwrap(),
            "{many}"
        );
        d.write_targets = vec![w("src/syntax.rs", false), w("Path.home() / argv[1]", true)];
        let lines = d.lines(200);
        assert_eq!(
            line_with(&lines, "Path.home()").spans[0].style.top(),
            Role::Pending,
            "an unresolved target is not drawn as a path"
        );
        assert_eq!(
            line_with(&lines, "src/syntax.rs").spans[0].style.top(),
            Role::Strong
        );
        assert!(
            plain(&lines)
                .join("\n")
                .contains("a write whose target could not be read")
        );
        d.write_targets = Vec::new();
        let none = drawn(&d, 200);
        assert!(
            !none.contains("files:") && !none.contains("could not be read"),
            "{none}"
        );
    }

    #[test]
    fn a_subagents_call_says_whose_it_is_under_the_question() {
        let mut d = two();
        d.subagent = Some(SubagentAsk {
            handle: "s2".into(),
            task: "port the widgets".into(),
        });
        let s = plain(&d.lines(100));
        assert_eq!(s[1], "    a subagent's call — s2 · port the widgets");
    }

    #[test]
    fn nothing_on_the_card_is_wider_than_the_card() {
        let mut d = two();
        d.target = "x".repeat(300);
        d.detail = "y ".repeat(300);
        d.because = "z".repeat(200);
        for w in [20usize, 40, 80] {
            for l in d.lines(w) {
                assert!(l.width() <= w, "{w}: {}", l.plain());
            }
        }
    }
}
