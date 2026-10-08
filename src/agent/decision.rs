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

/// One rung of a permission's ladder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecisionOption {
    /// What the option says: `Allow once`, `Always allow bash(cargo test *)`.
    pub label: String,
    /// The id typing it answers with: `allow_once`, `deny`.
    pub option_id: String,
    pub kind: OptionKind,
}

/// The kinds of answer a permission offers. Two of them change the card: an
/// always-allow is offered with a glob to edit, and a reject-always asks for words.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OptionKind {
    AllowOnce,
    AllowSession,
    AllowAlways,
    RejectOnce,
    RejectAlways,
}

/// **What silence does**, in the daemon's own three words (§1.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OnTimeout {
    #[default]
    Deny,
    Allow,
    Ask,
}

impl OnTimeout {
    /// The order a person reads a card in is the question, the choices, then what happens
    /// if they do nothing — and until this existed the third of those was unanswerable
    /// from the screen. The operator: *"two gate cards timed out unanswered at 300 seconds
    /// with `not_run by gate:timeout` — nothing on the card had said that was coming."*
    ///
    /// **The upper case on `RUNS` is the point of the line.** It is the only one of the
    /// three that does something nobody asked for, and an operator who walked away
    /// believing the default was `deny` when it was `allow` has been told nothing at all by
    /// a clock.
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

/// **An open decision: a call waiting on the person.** The view model of letibot's
/// `OpenDecision` plus the two things the head kept beside it — the selected rung and the
/// clock.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DecisionCard {
    /// `permission`, `question`, … — drawn in brackets after the ask, and `question`
    /// changes the card (its ladder is `choices`, nothing is consulted for it).
    pub kind: String,
    /// The ask as the daemon wrote it: `` `bash` wants exec access to `cargo test` ``.
    pub summary: String,
    /// The thing being asked about — the command, the path. Its own lines.
    pub target: String,
    pub subagent: Option<SubagentAsk>,
    pub write_targets: Vec<WriteTarget>,
    /// Layer A's deterministic reading.
    pub detail: String,
    /// The tool's declared access: `exec`, `read`, … Empty from a daemon older than the
    /// field.
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
    /// When the gate gives up, on the same clock as `now_ms`. `None` is §11.5's *wait
    /// forever*: a policy, so nothing is drawn.
    pub deadline_ms: Option<u64>,
    pub now_ms: u64,
    pub on_timeout: OnTimeout,
}

impl DecisionCard {
    fn is_question(&self) -> bool {
        self.kind == "question"
    }

    /// The rows the ladder walks: a question's are its `choices`, a permission's its
    /// `options` (letibot's `decision_rows`). Drawing `options` for a question is what
    /// left a card with an empty ladder under a question.
    pub fn rows(&self) -> usize {
        if self.is_question() {
            self.choices.len()
        } else {
            self.options.len()
        }
    }

    /// **The card, split where R20 says the split is.**
    ///
    /// R20, ruled on a permission card carrying a giant replace or a commit message: *"I'm
    /// shown a permission prompt and I just cant see the selector."* The screen-fit loop
    /// shrank the card from the end, and the card was built headline, target, intents,
    /// because, advice, options, hint — so the loop ate the hint, then the options
    /// bottom-up, and kept the wall. **A card that has dropped its choices is a question
    /// with no way to answer it.**
    ///
    /// So the card has two halves and only one of them gives way:
    ///
    /// * **`content`** — the question, what it is about, and the evidence. A viewport over
    ///   this ([`card_window`]) shrinks to the room that is left, and scrolls, so the whole
    ///   diff or message can still be read;
    /// * **`choices`** — the ladder and what pressing it means: the deadline, the hint, the
    ///   `deny_and_tell` line. **Never trimmed and never scrolled**, because this half is
    ///   the answer.
    pub fn split(&self, w: usize) -> (Vec<Line>, Vec<Line>) {
        let dim = |s: &str| wrap(&clean(s), w).into_iter().map(|l| one(l, Role::Faint));
        // **The question, then the thing itself, then the evidence.**
        //
        // One line used to carry all three — the sentence with the target interpolated
        // into it, layer A's verdict and intent list, and the kind — and the operator's
        // reading of it was *"no possible to see wtf was the command i supposed to
        // approve"*. A permission an operator cannot evaluate is one they approve out of
        // fatigue, which is the whole mechanism this gate exists to interrupt.
        //
        // So: the ask in the attention register, the target alone and indented under it,
        // the deterministic reading faint below that.
        let headline =
            ask_without_target(&self.summary, &self.target).unwrap_or_else(|| self.summary.clone());
        // letibot's colours, as roles: YELLOW → Attention, BOLD → Strong, DIM → Faint,
        // REVERSE → the reverse attribute.
        // Wrapped rather than left to the frame's trim: the question is the one line on the
        // card that may not lose its end.
        let mut out: Vec<Line> = wrap(&clean_one(&format!("? {headline} [{}]", self.kind)), w)
            .into_iter()
            .map(|l| one(l, Role::Attention))
            .collect();
        // **Whose call this is, when it is not this session's own.** A subagent's gate posts
        // its card to the root — *"who asks subagents permissions? i think they should
        // surface to the parent head all the way to the root obviously"* — and without this
        // clause the card is indistinguishable from one this session's own model raised. A
        // card answered for the wrong thing is the defect. **Directly under the question**:
        // it changes what is being decided.
        if let Some(s) = &self.subagent {
            let said = if s.task.is_empty() {
                format!("    a subagent's call — {}", s.handle)
            } else {
                format!("    a subagent's call — {} · {}", s.handle, s.task)
            };
            out.extend(dim(&said));
        }
        if !self.target.is_empty() {
            // Strong rather than attention: the question is in attention, and the thing
            // being asked about is not a second question.
            for l in wrap(&clean(&format!("    {}", self.target)), w) {
                out.push(one(l, Role::Strong));
            }
        }
        // **AND THE FILES THIS ACTION WOULD WRITE** — R35. The operator: *"it cant catch
        // those pesky python edits"*. It could; nothing showed them. At the place and in the
        // style the single target has, so an `edit` card and a script card read alike.
        //
        // **The count goes ABOVE the names**, because the names are content and content
        // elides to the card's viewport while the number must not. One path gets no header.
        // **An unresolved path is drawn in the attention register**, never as a path: a
        // write whose target could not be read is the case a person most needs to see.
        // **Nothing is drawn when the list is empty**: empty means *no write the scanner
        // could place*, and *writes nothing* would claim a negative it cannot support.
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
                    Role::Attention
                } else {
                    Role::Strong
                };
                out.push(one(format!("    {}", clean_one(&t.path)), r));
            }
            // The count of unresolved ones is a SENTENCE: one says *a write whose target
            // could not be read* and two say *2 writes whose targets could not be read*.
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
                    Role::Attention,
                ));
            }
        }
        if !self.detail.is_empty() {
            out.extend(dim(&format!("  {}", self.detail)));
        }
        // **§11.7: the one sentence that joins the two statements above.** The headline says
        // what the tool **declares** and the line above is layer A's reading of the
        // **action**, and nothing joined them, so *"the classifier decided this needed no
        // asking"* read as being argued with by the card going up anyway — which cost three
        // 300-second refusals in one night. **Said only where the declaration is what
        // asks**: on a `read` call it would be false on the very card carrying it, and an
        // empty `access` is a daemon that did not say.
        const ACCESS_ASKS: &str = "the access is what asks: a tool declared to `exec` is \
                                   asked about on its declaration, and the line above is a \
                                   reading of this action";
        if self.access == "exec" && !self.is_question() {
            out.extend(dim(&format!("  {ACCESS_ASKS}")));
        }
        // **Why it is asking, in the words of whoever is stuck**, labelled with its speaker
        // like every other borrowed sentence on this card: a reader who cannot tell whose
        // sentence it is cannot weigh it against their own knowledge.
        if !self.because.is_empty() {
            out.extend(dim(&format!("  because: {}", self.because)));
        }
        // **The model's verdict, above the ladder.** At `/mode supervised` the question is
        // *do you agree with the model*, and a person cannot agree with something they were
        // not shown. Nothing here preselects an option: the verdict informs the answer and
        // must never supply it.
        if let Some(a) = &self.advice {
            // **R12's four non-answers, and the fifth fact that is an answer.** Without the
            // `unsure` token all five reached the glass as one line, for facts whose remedies
            // differ. The card AUTHORS the classification; the daemon's prose follows it.
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
                    // **A token this card does not know is SHOWN, not swallowed.**
                    Some(other) => format!("  the guard did not decide ({other}) — {}", a.basis),
                    // The fifth fact: a consulted oracle that answered.
                    None => format!("  model says {}: {}", a.would, a.basis),
                }
            } else {
                // **An oracle that was NOT consulted did not say anything.**
                format!("  no model verdict — {}", a.basis)
            };
            out.extend(dim(&said));
            // **Said out loud when it is a fact, omitted when it is not one.** An oracle that
            // AUTHORISED something while citing none of your words is the case most worth a
            // second look. Printed unconditionally it contradicted the line above it: *"it
            // also told that i didnt mention anything while it was clear that i instructed
            // the model to use worktrees"*.
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
            // **And when nobody was asked, the card says that too.** *No oracle was
            // consulted* and *the oracle was asked and said nothing* must not be the same
            // screen. **Only where a gate is**: nothing is consulted for a question by
            // construction.
            out.extend(dim(
                "  no oracle was consulted for this one — the judgement is yours alone",
            ));
        }
        // **R20: here the card stops being content and becomes the answer.**
        let mut choices: Vec<Line> = Vec::new();
        // **One row per answer, with the highlighted one marked.** They used to be joined
        // with `·` onto one wrapped line, which is readable but is not a control. A ladder
        // the eye can walk is also a ladder Up/Down can walk, and the two have to agree —
        // the marker IS the thing Enter takes. A question's rows have no ids: they are the
        // model's prose, offered back to it as an index.
        let rows: Vec<String> = if self.is_question() {
            self.choices.iter().map(|c| clean_one(c)).collect()
        } else {
            self.options
                .iter()
                .map(|o| format!("{}  ({})", clean_one(&o.label), clean_one(&o.option_id)))
                .collect()
        };
        let sel = self.selected.min(rows.len().saturating_sub(1));
        for (i, body) in rows.iter().enumerate() {
            let picked = i == sel;
            // The id stays on the line. Typing it still works, a script still uses it, and
            // a reader learning the ladder sees both spellings of the same choice.
            for l in wrap(&format!("  {} {body}", if picked { "▸" } else { " " }), w) {
                choices.push(if picked {
                    // Reverse video rather than another colour: the prompt is already in
                    // attention, and a highlight that is a second hue reads as a second kind
                    // of thing rather than as "this one".
                    super::text::styled(l, crate::render::Style::new().reverse())
                } else {
                    one(l, Role::Attention)
                });
            }
        }
        // **§1.6: how long there is, and what silence will do.** Both facts were on the
        // wire and neither was drawn; the operator was bitten by exactly that. Below the
        // options, because that is the order a person reads. Drawn only where there is a
        // deadline.
        if let Some(deadline) = self.deadline_ms {
            let mut said: Vec<String> = Vec::new();
            said.push(match deadline.checked_sub(self.now_ms) {
                Some(left) => super::text::countdown(left),
                // **A card still on the screen after its own deadline**: one the daemon has
                // settled without telling this head. It must **never count into negative
                // seconds**, which reads as a rendering fault.
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
        // The glob line is only shown when an *always allow* is actually on offer. A hint for
        // an option this request does not have is an affordance that does nothing, which
        // teaches the operator to stop reading the hints. **A question's hints are its own**:
        // *"or type the id"* names something a question does not have.
        let hint = if self.is_question() {
            "  ↑↓ to choose · Enter to answer · or type your own answer"
        } else if self
            .options
            .iter()
            .any(|o| o.kind == OptionKind::AllowAlways)
        {
            // The rule the *Always allow* answer will write is in the option's own label, so
            // the hint points at editing it: a pattern they cannot see is a pattern they
            // cannot adjust.
            "  ↑↓ to choose · Enter to answer · or type the id · `allow_always <glob>` to widen \
             or narrow the rule shown above"
        } else {
            "  ↑↓ to choose · Enter to answer · or type the id"
        };
        // The hints are trimmed to the card rather than wrapped (the frame trimmed them in
        // letibot): they are the least of the card, and a wrapped hint is a row the
        // content's window pays for.
        choices.push(one(super::text::trim_to(hint, w), Role::Attention));
        // **The option that asks for words says where to type them.** Its label promised
        // *"tell the model why"* and the card never said how.
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
                Role::Attention,
            ));
        }
        (out, choices)
    }

    /// The whole card as one list (the transcript's shape and the tests' entry point;
    /// a screen that has to fit the card uses [`DecisionCard::split`] and [`card_window`]).
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
/// `` `bash` wants exec access to `cargo test` ``.
///
/// The daemon sends both the sentence and the target, and the sentence is the one
/// every other reader of the log already has. Rather than change what that sentence is,
/// the card that lays the two out separately takes the target back off. `None` when the
/// sentence does not end in the target: then the whole sentence is shown and nothing is
/// lost.
pub fn ask_without_target(summary: &str, target: &str) -> Option<String> {
    if target.is_empty() {
        return None;
    }
    let head = summary.strip_suffix(&format!("`{target}`"))?;
    // " to " is the joint in every sentence the daemon writes; trimming it is what makes
    // the remainder read as a heading rather than as a clipped sentence.
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

/// **The card's content as a window** (R20).
///
/// `room` is how many rows the viewport may occupy, seam included. The seam is the whole
/// of the disclosure, so it is written as one: **how many lines are out of view** and which
/// key moves toward them — never "there is more", because a reader has to know whether one
/// line or four hundred are missing before deciding whether to scroll at all.
///
/// **The window starts at the head**, where a card is read from, and `pgdn` walks down into
/// the wall. The seam changes ends with the scroll — above the window once there is nothing
/// left below it — so the sentence is always the boundary the reader is looking at.
///
/// The scroll is clamped here rather than in a key handler: the drawn length is a function
/// of the width, and the key handler knows neither.
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
    // One row of the viewport is the seam, and it is spent even at `room == 1` — a
    // viewport whose whole height is the sentence saying how much is missing is the
    // honest shape of a screen with nowhere to put the content.
    let shown = room - 1;
    let at = scroll.min(len - shown);
    let above = at;
    let below = len - (at + shown);
    let seam = one(
        if below == 0 {
            format!("  … {above} line(s) out of view · pgup scrolls")
        } else if above == 0 {
            format!("  … {below} line(s) out of view · pgdn scrolls")
        } else {
            format!("  … {above} above, {below} below · pgup/pgdn scrolls")
        },
        Role::Faint,
    );
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

/// **A permission card fitted to `room` rows** (R20): the choices whole, the content
/// windowed into what is left. The host owns `scroll` and stores [`CardWindow::scroll`]
/// back.
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

    // ---- the open card: letibot `app/tests/decisions.rs`, at the card ----

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
        assert_eq!(lines[0].spans[0].style.top(), Role::Attention);
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
        assert_eq!(choices[0].spans[0].style.top(), Role::Attention);
        // Past the end clamps to the last rung rather than marking nothing.
        d.selected = 99;
        assert!(plain(&d.split(100).1)[2].starts_with("  ▸"));
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
            Role::Attention,
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
