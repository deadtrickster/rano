//! **Blocks with a header, a state and a bounded body**: a tool call's card, and the
//! model's reasoning.
//!
//! Ported from letibot's `crates/ui/src/card.rs`, which was a string renderer; this one
//! builds [`Line`]s of roles. The header below is letibot's, kept because each point is
//! a defect it paid for.
//!
//! # What is wrong with a one-line call
//!
//! letibot rendered a settled tool call as `● edit(call_7) — ok · 214 B`. That is the
//! wrong shape for a person:
//!
//! - **The argument is missing.** `edit` is not the interesting word; the path
//!   is. Every tool has exactly one argument that identifies *what it did to
//!   what*, and it is the only part a reader scans for.
//! - **`running` does not say for how long.** The single most common question
//!   during a turn is "is this stuck", and an elapsed time answers it while a
//!   spinner does not.
//! - **A progress note has nowhere to go** in a one-line renderer.
//!
//! # Three fold states, not two
//!
//! From grok-build. Collapsed / Truncated / Expanded. The middle state is the one that
//! earns its place: a finished `bash` call should show its last three lines without
//! being asked, because those are the lines that say whether it worked.
//!
//! # Provenance
//!
//! **Adapted from grok-build** (xAI, Apache-2.0),
//! `crates/codegen/xai-grok-pager/src/scrollback/`: `DisplayMode` (`types.rs:52`), the
//! tense-flipping verb (`blocks/tool/mod.rs:107`), the head/tail budgets
//! (`read.rs:16`, `use_tool.rs:15`, `appearance/config.rs:613`), the `… +{n} lines`
//! separator row (`execute.rs:549`), and the thinking block's rail, attribute-based
//! de-emphasis and `Replayed` rule (`blocks/thinking.rs`).
//!
//! Their `execute.rs` gives the head and tail chunks **different selection range ids**
//! so a drag-copy across the ellipsis cannot silently splice non-adjacent text. There is
//! no mouse selection here, so there is nothing to port — but the hazard is real the
//! moment one is added, and it is recorded here rather than rediscovered.

use crate::render::text::truncate;
use crate::render::{Line, Span, Style};
use crate::style::Role;
use crate::width::text as wt;

use super::decision::{SettledDecision, decision_detail};
use super::outcome::Outcome;
use super::text::{bytes_human, clean, clean_line, duration, header_names_the_file, one, wrap};
use super::{Fold, lines_widget};

/// How much of a block is shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DisplayMode {
    /// Header only.
    Collapsed,
    /// Header, plus a head and a tail of the body with the middle elided.
    #[default]
    Truncated,
    /// Everything (bounded by [`Budget::expanded_max`]).
    Expanded,
}

impl DisplayMode {
    /// The cycle a fold key walks. Running blocks skip `Expanded`: a body that
    /// is still growing pushes the prompt off the screen a line at a time,
    /// which is the thing that reads as flicker.
    pub fn next(self, running: bool) -> DisplayMode {
        match (self, running) {
            (DisplayMode::Collapsed, _) => DisplayMode::Truncated,
            (DisplayMode::Truncated, false) => DisplayMode::Expanded,
            (DisplayMode::Truncated, true) => DisplayMode::Collapsed,
            (DisplayMode::Expanded, _) => DisplayMode::Collapsed,
        }
    }
}

/// How the card is titled, and in which tense.
///
/// The tense is not decoration. `Reading src/main.rs` and `Read src/main.rs` are
/// the difference between "wait" and "done", read at a glance from the first
/// word, without a colour or a glyph — which matters because the glyph is the
/// part a screen reader or a `--replay` transcript loses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verb {
    Read,
    Edit,
    Write,
    Search,
    List,
    Run,
    Fetch,
    /// Anything without an opinion here; the tool's own name is used.
    Other(String),
}

impl Verb {
    /// Map a tool name to a verb. Unknown names keep their own name, which is
    /// right: inventing a verb for a tool we do not know is a guess presented as
    /// a fact.
    pub fn of(tool: &str) -> Verb {
        match tool {
            "read" | "read_file" | "cat" | "view" => Verb::Read,
            "edit" | "patch" | "apply_patch" | "str_replace" => Verb::Edit,
            "write" | "write_file" | "create" => Verb::Write,
            "grep" | "search" | "rg" | "find" => Verb::Search,
            "ls" | "list" | "list_dir" | "glob" => Verb::List,
            "bash" | "shell" | "run" | "exec" => Verb::Run,
            "fetch" | "web_fetch" | "http" => Verb::Fetch,
            other => Verb::Other(other.to_string()),
        }
    }

    pub fn label(&self, running: bool) -> &str {
        match (self, running) {
            (Verb::Read, false) => "Read",
            (Verb::Read, true) => "Reading",
            (Verb::Edit, false) => "Edited",
            (Verb::Edit, true) => "Editing",
            (Verb::Write, false) => "Wrote",
            (Verb::Write, true) => "Writing",
            (Verb::Search, false) => "Searched",
            (Verb::Search, true) => "Searching",
            (Verb::List, false) => "Listed",
            (Verb::List, true) => "Listing",
            (Verb::Run, false) => "Ran",
            (Verb::Run, true) => "Running",
            (Verb::Fetch, false) => "Fetched",
            (Verb::Fetch, true) => "Fetching",
            (Verb::Other(s), _) => s,
        }
    }

    /// Whether this verb's subject names a file (and may be drawn as a link to it).
    pub fn names_a_file(&self) -> bool {
        matches!(self, Verb::Read | Verb::Edit | Verb::Write | Verb::List)
    }

    /// Whether a before/after diff is this verb's body.
    pub fn is_an_edit(&self) -> bool {
        matches!(self, Verb::Edit | Verb::Write)
    }
}

/// **The fewest columns a running call's subject may keep**, while the tail keeps its own.
///
/// leticl's `(max 8 (- cols fixed …))` — *"a subject squeezed below a few columns says nothing"* —
/// and a floor rather than a fair share on purpose: the row is allowed to run long and be trimmed
/// by the frame, because the trim takes the tail's END (a note, a reason) and the clock sits at
/// the tail's HEAD. If the subject ate into the tail instead, the number that says the call is
/// alive would be what disappeared.
const MIN_SUBJECT: usize = 8;

/// Where a block is in its life.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Phase {
    /// The model asked for it; nothing has run.
    ///
    /// `note` is what is happening WHILE nothing runs — the guard being asked whether
    /// the call follows from what the operator wanted. That wait is seconds long and used
    /// to render as the bare word "proposed", so a turn sat still with no reason given:
    /// *"the tool call latency grew, i almost thought something stalled and looked at
    /// htop"*.
    Proposed { note: Option<String> },
    /// In flight. `elapsed_ms` is supplied by the caller, never read from a clock here.
    Running {
        elapsed_ms: u64,
        /// The most recent progress note.
        note: Option<String>,
    },
    Finished {
        outcome: Outcome,
        elapsed_ms: Option<u64>,
    },
    /// Reconstructed from a log rather than watched live.
    ///
    /// The distinction exists because a replay has no honest elapsed time, and
    /// printing `0.0s` is worse than printing nothing — it is a measurement that
    /// was never taken, rendered as one that was.
    Replayed { outcome: Outcome },
}

impl Phase {
    pub fn is_running(&self) -> bool {
        matches!(self, Phase::Running { .. } | Phase::Proposed { .. })
    }
}

/// Head and tail line counts per fold state. The defaults are grok-build's shipped
/// numbers: somebody else's calibration rather than our guess.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Budget {
    pub first_lines: usize,
    pub last_lines: usize,
    /// Cap even in [`DisplayMode::Expanded`]. A 40,000-line tool result
    /// expanded into a terminal is not "expanded", it is the conversation gone.
    pub expanded_max: usize,
}

impl Budget {
    /// Reading a file: enough head to see what it is, enough tail to see it ended.
    pub const READ: Budget = Budget {
        first_lines: 5,
        last_lines: 3,
        expanded_max: 400,
    };
    /// A shell command: the tail is what matters, the head almost never is.
    pub const SHELL: Budget = Budget {
        first_lines: 2,
        last_lines: 3,
        expanded_max: 400,
    };
    /// Anything else.
    pub const GENERIC: Budget = Budget {
        first_lines: 10,
        last_lines: 3,
        expanded_max: 400,
    };

    /// The budget a verb deserves.
    pub fn for_verb(v: &Verb) -> Budget {
        match v {
            Verb::Read | Verb::List => Budget::READ,
            Verb::Run => Budget::SHELL,
            _ => Budget::GENERIC,
        }
    }
}

impl Default for Budget {
    fn default() -> Self {
        Budget::GENERIC
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CardConfig {
    pub width: usize,
    pub mode: DisplayMode,
    pub budget: Budget,
    /// Show the call id. Off by default: it is a correlation key for a log, not
    /// something a person reads, and it costs a dozen columns of a header that
    /// has a path to show.
    pub show_id: bool,
}

impl Default for CardConfig {
    fn default() -> Self {
        CardConfig {
            width: 100,
            mode: DisplayMode::Truncated,
            budget: Budget::GENERIC,
            show_id: false,
        }
    }
}

/// One tool call, ready to draw: the generic card. [`ToolCall`] is the view model a host
/// fills; it builds one of these.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Card {
    pub verb: Verb,
    /// The tool's own name, for the cases where the verb hides it.
    pub tool: String,
    pub call_id: String,
    /// The one argument that says what was acted on: a path, a pattern, a
    /// command line. Empty when the tool has none.
    pub target: String,
    /// The body: already-built lines. A diff, a file excerpt, stdout. This type does
    /// not know which, on purpose — it lays them out.
    pub body: Vec<Line>,
    pub phase: Phase,
    /// §8.3's disclosure: bytes shown inline, and bytes that exist. Rendered
    /// only when they differ, because `8 KB` beside a 480 KB output is a number
    /// that misleads.
    pub bytes: Option<(u64, u64)>,
    /// Content hash of the spilled full output, when there is one.
    pub spill: Option<String>,
}

fn push(l: &mut Line, s: impl Into<String>, r: Role) {
    l.spans.push(Span::role(s, r));
}

impl Card {
    pub fn new(tool: &str, call_id: &str) -> Card {
        Card {
            verb: Verb::of(tool),
            tool: tool.to_string(),
            call_id: call_id.to_string(),
            target: String::new(),
            body: Vec::new(),
            phase: Phase::Proposed { note: None },
            bytes: None,
            spill: None,
        }
    }

    pub fn target(mut self, t: &str) -> Card {
        self.target = t.to_string();
        self
    }

    pub fn phase(mut self, p: Phase) -> Card {
        self.phase = p;
        self
    }

    pub fn body(mut self, lines: Vec<Line>) -> Card {
        self.body = lines;
        self
    }

    /// The header line, which is what survives every fold state.
    pub fn header(&self, cfg: &CardConfig) -> Line {
        let running = self.phase.is_running();
        let (mark, mark_role) = match &self.phase {
            Phase::Proposed { .. } => ('○', Role::Faint),
            Phase::Running { .. } => ('◐', Role::Pending),
            Phase::Finished { outcome, .. } | Phase::Replayed { outcome } => ('●', outcome.role()),
        };
        // **The head is built first and the SUBJECT is filled in last** — R53 §1.5, and it is
        // leticl's own rule: *"the tail is measured first and the subject is given what is left…
        // the tail does not shrink — it is the fact, and half of `· 12.4s` is not a duration."*
        //
        // The order used to be the other way round: the subject was written into the row at full
        // length and the tail appended, so when the two did not fit the WHOLE row was truncated —
        // and the clock, which is the one number that says this call is alive, went with it. The
        // operator's own report: a running call rendered as
        // `◐ Running "cd /tmp && (sleep 6; …) & …"` with no `· 12.4s` anywhere on it.
        let mut s = Line::default();
        push(&mut s, mark.to_string(), mark_role);
        push(&mut s, " ", Role::Plain);
        push(&mut s, self.verb.label(running), Role::Strong);

        // The right-hand side: state, timing, disclosure.
        let mut tail: Vec<String> = Vec::new();
        let ended = |tail: &mut Vec<String>, outcome: &Outcome| {
            if !matches!(outcome, Outcome::Ok) {
                tail.push(outcome.word().to_string());
                if let Some(r) = outcome.reason() {
                    tail.push(clean_line(r));
                }
            }
        };
        match &self.phase {
            Phase::Proposed { note } => match note {
                // The reason beats the state: "proposed" says what it is, and the
                // note says why nothing is happening yet, which is the question the
                // operator actually has while looking at it.
                Some(n) => tail.push(clean_line(n)),
                None => tail.push("proposed".into()),
            },
            Phase::Running { elapsed_ms, note } => {
                tail.push(duration(*elapsed_ms));
                if let Some(n) = note {
                    tail.push(clean_line(n));
                }
            }
            Phase::Finished {
                outcome,
                elapsed_ms,
            } => {
                if let Some(ms) = elapsed_ms {
                    tail.push(duration(*ms));
                }
                ended(&mut tail, outcome);
            }
            Phase::Replayed { outcome } => ended(&mut tail, outcome),
        }
        if let Some((inline, full)) = self.bytes
            && inline != full
        {
            tail.push(format!("{inline} B of {full} B"));
        }
        if let Some(h) = &self.spill {
            tail.push(format!("spill {h}"));
        }
        if tail.is_empty() {
            return truncate(&s, cfg.width);
        }
        let role = match &self.phase {
            Phase::Finished { outcome, .. } | Phase::Replayed { outcome }
                if !matches!(outcome, Outcome::Ok) =>
            {
                outcome.role()
            }
            _ => Role::Faint,
        };
        let joined = format!(" · {}", tail.join(" · "));
        // The call id, kept beside the subject because it is the same kind of fact — and measured
        // before the subject, for the same reason the tail is.
        let id_str = if cfg.show_id {
            format!(" ({})", clean_line(&self.call_id))
        } else {
            String::new()
        };

        // **What the subject may have: everything the tail and the id have not claimed.** The floor
        // is leticl's `(max 8 …)`: a subject squeezed below a few columns says nothing, and letting
        // the row run long instead means the frame's own trim takes the tail's END — the note —
        // while the clock at its head survives. What must never give way is the tail's beginning.
        if !self.target.is_empty() {
            // **§3.1: a card is text this head did not author.** Target, call id, note and
            // outcome reason all come from a tool call or from the daemon.
            let target = clean_line(&self.target);
            let spare = cfg
                .width
                .saturating_sub(s.width() + wt::width(&joined) + wt::width(&id_str) + 1);
            // A subject that fits whole keeps its own length; one that does not is cut with an
            // ellipsis.
            let room = spare.max(MIN_SUBJECT);
            // **A path is cut from the left and anything else from the right** — the rule
            // the transcript row's `shorten_subject` applies too, so one call does not read two
            // ways on two rows. A glob or a quoted sentence is not a path however many
            // separators it contains: cutting `**/*.{md,json,toml,yaml,yml} 40` from the left
            // loses the fact that it is a glob.
            let not_a_path = target.contains(['*', '?', '{', '[', '"']);
            let shown = if target.contains('/') && !not_a_path {
                wt::ellipsise_left(&target, room)
            } else {
                wt::truncate(&target, room)
            };
            push(&mut s, " ", Role::Plain);
            push(&mut s, shown, Role::Plain);
        }
        if !id_str.is_empty() {
            push(&mut s, id_str, Role::Faint);
        }
        push(&mut s, joined, role);
        // One last guard for the case the floor above creates — a tail longer than any subject
        // could leave room for — and it takes the END, so the clock survives it.
        truncate(&s, cfg.width)
    }

    /// The whole card.
    pub fn render(&self, cfg: &CardConfig) -> Vec<Line> {
        let mut out = vec![self.header(cfg)];
        if cfg.mode == DisplayMode::Collapsed || self.body.is_empty() {
            if cfg.mode == DisplayMode::Collapsed && !self.body.is_empty() {
                out.push(one(format!("  … {} lines", self.body.len()), Role::Faint));
            }
            return out;
        }
        let body = match cfg.mode {
            DisplayMode::Expanded => head_tail(&self.body, cfg.budget.expanded_max, 0),
            _ => head_tail(&self.body, cfg.budget.first_lines, cfg.budget.last_lines),
        };
        for l in body {
            out.push(truncate(&indent(l, 2), cfg.width));
        }
        out
    }
}

/// `l` with `n` columns of blank before it.
pub(crate) fn indent(mut l: Line, n: usize) -> Line {
    l.spans.insert(0, Span::raw(" ".repeat(n)));
    l
}

/// Keep `first` lines, then `last` lines, and say how many went.
///
/// The marker is grok-build's `… +{n} lines` (`execute.rs:549`) and it is a
/// **separator row**, not a line of the content — the distinction matters
/// because a reader must never mistake the elision for output. It is never a
/// silent cut: the count is the disclosure.
pub fn head_tail(lines: &[Line], first: usize, last: usize) -> Vec<Line> {
    if lines.len() <= first + last + 1 {
        return lines.to_vec();
    }
    let hidden = lines.len() - first - last;
    let mut out: Vec<Line> = lines[..first].to_vec();
    out.push(one(format!("… +{hidden} lines"), Role::Faint));
    if last > 0 {
        out.extend_from_slice(&lines[lines.len() - last..]);
    }
    out
}

/// Columns a caller must subtract from the width before wrapping a reasoning
/// body, so that the wrap and the rail agree.
pub const REASONING_RAIL_WIDTH: usize = 2;

/// The model's reasoning, rendered so it can never be mistaken for its answer.
///
/// Three separate signals, because any one of them is lost somewhere: the word
/// (`Thinking…` / `Thought for 4.2s`), the rail (`┃`), and the [`Role::Reasoning`]
/// register (dim and italic — an attribute, because de-emphasis by **colour** is a
/// no-op under a terminal-native palette).
///
/// `elapsed_ms` is `None` for a replayed session. It renders as `Thought` with
/// no duration rather than `Thought for 0.0s`.
///
/// The body is laid **inside** the reasoning role: each line's own spans stack over it,
/// so a code span in the model's working-out keeps its look and closes back to the
/// block rather than to the terminal's default.
pub fn reasoning(
    body: &[Line],
    running: bool,
    elapsed_ms: Option<u64>,
    cfg: &CardConfig,
) -> Vec<Line> {
    let head = if running {
        one("Thinking…", Role::Reasoning)
    } else {
        match elapsed_ms {
            Some(ms) => Line::new(vec![
                Span::role("Thought", Role::Strong),
                Span::role(format!(" for {}", duration(ms)), Role::Faint),
            ]),
            None => one("Thought", Role::Strong),
        }
    };
    let mut out = vec![truncate(&head, cfg.width)];
    if cfg.mode == DisplayMode::Collapsed {
        if !body.is_empty() {
            out.push(one(format!("  … {} lines", body.len()), Role::Faint));
        }
        return out;
    }
    // The rail is two columns, so the body was wrapped two columns narrower.
    // Getting this wrong is how a "reasoning" block ends up one row taller than
    // the space reserved for it, which pushes everything below it by a line
    // every frame.
    let shown = match cfg.mode {
        DisplayMode::Expanded => head_tail(body, cfg.budget.expanded_max, 0),
        _ => head_tail(body, cfg.budget.first_lines, cfg.budget.last_lines),
    };
    for l in shown {
        let mut row = Line::new(vec![Span::role("┃", Role::Faint), Span::raw(" ")]);
        let inner = Style::of(Role::Reasoning).patch(&l.style);
        row.spans.extend(l.spans.into_iter().map(|sp| Span {
            content: sp.content,
            style: inner.patch(&sp.style),
        }));
        out.push(truncate(&row, cfg.width));
    }
    out
}

/// **A file edit's before/after, as the host rendered it** — rano's `sidediff` edit view
/// (split or unified, the operator's `/diff` toggle), already laid out at
/// [`ToolCall::diff_width`] / the tool row's diff width.
///
/// `rows[0]` is the edit view's **name line** (the file's path); a widget drops it when its
/// own header already names the file. Sanitise the two sides *before* diffing them — §3.1:
/// a diff's sides are a file's bytes, and the two sides compared must be the two sides
/// shown.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EditDiff {
    /// The path the excerpt is of.
    pub path: String,
    /// The rendered view, name line first.
    pub rows: Vec<Line>,
    /// `Some(n)` when the excerpt was capped: the file is `n` lines now.
    pub capped_at: Option<usize>,
}

impl EditDiff {
    /// The rows to draw under a header that says `target`: the name line dropped when
    /// the header names the file, and the cap's disclosure appended.
    pub fn rows_under(&self, target: &str) -> Vec<Line> {
        let mut rows = self.rows.clone();
        if header_names_the_file(target, &self.path) && !rows.is_empty() {
            rows.remove(0);
        }
        if let Some(n) = self.capped_at {
            rows.push(one(
                format!("… the excerpt was capped; the file is {n} lines now"),
                Role::Faint,
            ));
        }
        rows
    }
}

/// Where a live call is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallState {
    Proposed {
        note: Option<String>,
    },
    Running {
        /// Since the call started, on ONE clock (see [`ToolCall`]).
        elapsed_ms: u64,
        note: Option<String>,
    },
    Finished {
        outcome: Outcome,
        /// `None` when the call was not watched (a snapshot has no timestamps).
        elapsed_ms: Option<u64>,
        /// Bytes that went to the model.
        inline_bytes: u64,
        /// Bytes the tool produced.
        full_bytes: u64,
        /// The spill's content hash, when the rest was kept aside.
        spill: Option<String>,
    },
}

/// **One live tool call, as a card** — the view model of letibot's `call_card`.
///
/// It replaced `call_line`, which produced `● edit(call_7) — ok · 214 B` and could
/// produce nothing else. A card keeps the disclosure and adds the three things a
/// person watching a call is actually looking for: how long it has been running,
/// what it last said, and — for a spill — what happened to the rest of the output.
///
/// **The body is empty while a call is in flight, and that is deliberate.** A finished
/// event carries digests and byte counts, never a payload; the payload reaches a head
/// only as a transcript row ([`super::tool_row::ToolRow`]).
///
/// **One clock** (letibot R13): `Running::elapsed_ms` must be computed from a start and a
/// now on the same clock — the head's when the call has a head-side anchor, the log's when
/// it does not. Mixing the log's start with the head's now reports the time since the
/// started event rather than since the call started, which is a *smaller* number and so
/// reads like progress.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolCall {
    /// The tool's name: `bash`, `read`, `edit`.
    pub name: String,
    pub call_id: String,
    /// The bounded display target — the path, the pattern, the command line. Empty is
    /// rendered as nothing: a digest is not a display string and a guess is worse than a
    /// blank.
    pub target: String,
    pub state: CallState,
    /// The before/after view, for an edit or a write the host holds both sides of.
    pub diff: Option<EditDiff>,
    /// The decision this call was gated by.
    pub decision: Option<SettledDecision>,
    /// The conversation's tool fold: open shows the whole body and the decision's detail.
    pub fold: Fold,
}

impl ToolCall {
    /// The width a host renders [`EditDiff::rows`] at for this card: the card indents its
    /// body by two, and a transcript steps the whole card in by the activity indent after
    /// it has rendered, so the panels are built for the width the row will actually have —
    /// or the frame trims the right panel's tail off and the diff lies by omission.
    pub fn diff_width(width: usize) -> usize {
        width.saturating_sub(2 + activity_indent(width))
    }

    /// The generic card this call draws as.
    pub fn card(&self, width: usize) -> Card {
        let mut card = Card::new(&self.name, &self.call_id);
        card.target = self.target.clone();
        let mut body: Vec<Line> = Vec::new();
        card.phase = match &self.state {
            CallState::Proposed { note } => Phase::Proposed { note: note.clone() },
            CallState::Running { elapsed_ms, note } => Phase::Running {
                elapsed_ms: *elapsed_ms,
                note: note.clone(),
            },
            CallState::Finished {
                outcome,
                elapsed_ms,
                inline_bytes,
                full_bytes,
                spill,
            } => {
                // §8.3's disclosure, as prose and in units a person reads. It goes in
                // the body rather than the header tail because the header tail is
                // dropped whole when it does not fit, and "there is more, and here is
                // how to get it" is not a line that may vanish on a narrow terminal.
                if let Some(hash) = spill {
                    body.push(Line::raw(format!(
                        "{} of {} went to the model, the rest is kept — read_spill hash={}",
                        bytes_human(*inline_bytes),
                        bytes_human(*full_bytes),
                        clean_line(hash),
                    )));
                } else {
                    card.bytes = Some((*inline_bytes, *inline_bytes));
                    body.push(Line::raw(bytes_human(*inline_bytes)));
                }
                match elapsed_ms {
                    // A snapshot has no timestamps, and `0.0s` is a measurement that
                    // was never taken rendered as one that was.
                    None => Phase::Replayed {
                        outcome: outcome.clone(),
                    },
                    Some(ms) => Phase::Finished {
                        outcome: outcome.clone(),
                        elapsed_ms: Some(*ms),
                    },
                }
            }
        };
        // The before/after view. Split or unified is the operator's toggle and nothing
        // else — no width gate, because the two answers a width gate ever gave were a
        // cramped diff or no diff at all.
        if card.verb.is_an_edit()
            && matches!(self.state, CallState::Finished { .. })
            && let Some(d) = &self.diff
        {
            body = d.rows_under(&card.target);
        }
        // The decision this call was gated by, in the dim register: the approval is a
        // fact about the call, not a stray note. Folded it is one line — who decided
        // and how; open it adds what the oracle was shown and what it said back.
        if let Some(d) = &self.decision {
            body.push(one(d.headline(), Role::Faint));
            if self.fold.is_open() {
                for l in decision_detail(d, width.saturating_sub(4)) {
                    body.push(one(format!("  {l}"), Role::Faint));
                }
            }
        }
        card.body = body;
        card
    }

    pub fn lines(&self, width: usize) -> Vec<Line> {
        let card = self.card(width);
        card.render(&CardConfig {
            width,
            // Never `Collapsed`: the spill disclosure lives in the body and a fold is
            // not a licence to hide it.
            mode: match self.fold {
                Fold::Open => DisplayMode::Expanded,
                Fold::Folded => DisplayMode::Truncated,
            },
            budget: Budget::for_verb(&card.verb),
            show_id: false,
        })
    }
}

lines_widget!(ToolCall);

/// **The indent a turn's working is stepped in by** — thinking and acting sit one step
/// under the conversation (the operator's question and the model's answer), which gives a
/// turn readable levels out of the vocabulary already on the screen, at no cost in colour.
///
/// **Two columns, matching the reasoning rail's width**, so the page reads as one
/// repeated step. Given up below sixty columns, where two columns out of every line is a
/// bigger fraction than the hierarchy is worth.
pub fn activity_indent(w: usize) -> usize {
    if w >= 60 { REASONING_RAIL_WIDTH } else { 0 }
}

/// **The affordance that stands in for a tool call while the model is writing it.**
///
/// The defect this replaced: *"tool calls — i see `<function…` like strings first,
/// then closing tag arrives and it becomes a toolcall."* What is left is the question
/// that markup was accidentally answering — *is something happening?* — and this
/// answers it without showing anybody a half-written `<parameter=`.
///
/// `now_ms` drives the spinner off a clock the host chose (the log's own, so a replay
/// animates the way the live session did).
pub struct WritingCall {
    pub now_ms: u64,
}

impl WritingCall {
    pub fn line(&self, width: usize) -> Line {
        let spin = super::text::spinner(self.now_ms).to_string();
        truncate(
            &Line::new(vec![
                Span::role(spin, Role::Pending),
                // The space between the spinner and the words is unpainted, as letibot's
                // row has always had it: two pending runs either side of a plain space.
                Span::raw(" "),
                Span::role("writing a tool call", Role::Pending),
                Span::role(" · ctrl-x for the raw form", Role::Faint),
            ]),
            width,
        )
    }

    pub fn lines(&self, width: usize) -> Vec<Line> {
        vec![self.line(width)]
    }
}

lines_widget!(WritingCall);

/// **The raw, unparsed text of a tool call, behind `ctrl-x`.**
///
/// Rendered as a labelled block rather than inline, because the whole point is that this
/// is *not* the assistant speaking. Faint and fenced: it is evidence, and evidence that
/// looks like prose is how the defect started. The text is the model's own markup, so it
/// is foreign and is cleaned before it is wrapped.
pub struct RawCall {
    pub raw: String,
}

impl RawCall {
    pub fn lines(&self, width: usize) -> Vec<Line> {
        let mut out = vec![one("┌─ raw tool call · ctrl-x", Role::Faint)];
        let raw = clean(&self.raw);
        for l in raw.lines() {
            for w in wrap(l, width.saturating_sub(2)) {
                out.push(Line::new(vec![
                    Span::role("│ ", Role::Faint),
                    Span::role(w, Role::Code),
                ]));
            }
        }
        out.push(one("└─", Role::Faint));
        out
    }
}

lines_widget!(RawCall);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::testing::{plain, role_of};

    fn body(n: usize) -> Vec<Line> {
        (0..n).map(|i| Line::raw(format!("line {i}"))).collect()
    }

    fn cfg() -> CardConfig {
        CardConfig {
            width: 80,
            ..Default::default()
        }
    }

    fn h(c: &Card, cfg: &CardConfig) -> String {
        c.header(cfg).plain()
    }

    // ---- letibot `crates/ui/src/card.rs` tests ----

    #[test]
    fn the_header_names_what_was_acted_on_not_just_the_tool() {
        let c = Card::new("read", "call_7")
            .target("crates/tui/src/app.rs")
            .phase(Phase::Finished {
                outcome: Outcome::Ok,
                elapsed_ms: Some(120),
            });
        let h = h(&c, &cfg());
        assert!(h.contains("crates/tui/src/app.rs"), "{h}");
        assert!(h.starts_with("● Read "), "{h}");
    }

    #[test]
    fn the_tense_says_whether_to_wait_without_a_colour() {
        let running = Card::new("bash", "c1")
            .target("cargo test")
            .phase(Phase::Running {
                elapsed_ms: 4_300,
                note: None,
            });
        let done = Card::new("bash", "c1")
            .target("cargo test")
            .phase(Phase::Finished {
                outcome: Outcome::Ok,
                elapsed_ms: Some(4_300),
            });
        assert!(h(&running, &cfg()).contains("Running cargo test"));
        assert!(h(&done, &cfg()).contains("Ran cargo test"));
        // And the elapsed time is there while it runs, which is the answer to
        // "is it stuck".
        assert!(h(&running, &cfg()).contains("4.3s"));
        // The mark and the verb carry their roles.
        let l = running.header(&cfg());
        assert_eq!(role_of(&l, "◐"), Some(Role::Pending));
        assert_eq!(role_of(&l, "Running"), Some(Role::Strong));
        assert_eq!(role_of(&l, "4.3s"), Some(Role::Faint));
    }

    #[test]
    fn a_tool_progress_note_has_somewhere_to_go() {
        let c = Card::new("bash", "c1")
            .target("cargo build")
            .phase(Phase::Running {
                elapsed_ms: 9_000,
                note: Some("Compiling letibot-ui".into()),
            });
        assert!(h(&c, &cfg()).contains("Compiling letibot-ui"));
    }

    #[test]
    fn abstention_does_not_read_like_success() {
        let c = Card::new("read", "c1")
            .target("/etc/shadow")
            .phase(Phase::Finished {
                outcome: Outcome::Abstained("outside the workspace".into()),
                elapsed_ms: Some(1),
            });
        let l = c.header(&cfg());
        let h = l.plain();
        assert!(h.contains("ABSTAINED"), "{h}");
        assert!(h.contains("outside the workspace"), "{h}");
        assert!(!h.contains("ok"), "{h}");
        assert_eq!(role_of(&l, "ABSTAINED"), Some(Role::Attention));
        assert_eq!(role_of(&l, "●"), Some(Role::Attention));
    }

    #[test]
    fn a_long_result_shows_a_head_a_count_and_a_tail() {
        let c = Card::new("bash", "c1")
            .target("ls -R")
            .body(body(400))
            .phase(Phase::Finished {
                outcome: Outcome::Ok,
                elapsed_ms: Some(300),
            });
        let cfg = CardConfig {
            budget: Budget::SHELL,
            ..cfg()
        };
        let out = plain(&c.render(&cfg));
        // header + 2 head + marker + 3 tail
        assert_eq!(out.len(), 7, "{out:#?}");
        assert!(out[1].contains("line 0"));
        assert!(out[3].contains("+395 lines"), "{:?}", out[3]);
        assert!(out.last().unwrap().contains("line 399"));
    }

    #[test]
    fn expanding_is_still_bounded_and_says_so() {
        let c = Card::new("read", "c1").body(body(5_000));
        let cfg = CardConfig {
            mode: DisplayMode::Expanded,
            ..cfg()
        };
        let out = plain(&c.render(&cfg));
        assert!(out.len() <= 402, "{} lines", out.len());
        assert!(
            out.iter().any(|l| l.contains("+4600 lines")),
            "{:?}",
            &out[..3]
        );
    }

    #[test]
    fn collapsing_keeps_the_header_and_admits_what_it_hid() {
        let c = Card::new("read", "c1").target("a.rs").body(body(40));
        let cfg = CardConfig {
            mode: DisplayMode::Collapsed,
            ..cfg()
        };
        let out = plain(&c.render(&cfg));
        assert_eq!(out.len(), 2);
        assert!(out[1].contains("40 lines"));
    }

    #[test]
    fn the_fold_cycle_never_expands_a_running_block() {
        // A body that is still growing pushes the prompt down a line at a time.
        let mut m = DisplayMode::Truncated;
        for _ in 0..6 {
            m = m.next(true);
            assert_ne!(m, DisplayMode::Expanded);
        }
        // Settled, it does.
        assert_eq!(DisplayMode::Truncated.next(false), DisplayMode::Expanded);
    }

    #[test]
    fn a_replayed_turn_does_not_invent_a_duration() {
        let live = Card::new("read", "c1").phase(Phase::Finished {
            outcome: Outcome::Ok,
            elapsed_ms: Some(0),
        });
        let replayed = Card::new("read", "c1").phase(Phase::Replayed {
            outcome: Outcome::Ok,
        });
        assert!(h(&live, &cfg()).contains("0ms"));
        assert!(
            !h(&replayed, &cfg()).contains('0'),
            "{}",
            h(&replayed, &cfg())
        );
    }

    #[test]
    fn the_byte_split_is_shown_only_when_it_discloses_something() {
        let mut c = Card::new("bash", "c1").phase(Phase::Finished {
            outcome: Outcome::Ok,
            elapsed_ms: Some(1),
        });
        c.bytes = Some((214, 214));
        assert!(!h(&c, &cfg()).contains("of"), "{}", h(&c, &cfg()));
        c.bytes = Some((8_192, 491_000));
        assert!(h(&c, &cfg()).contains("8192 B of 491000 B"));
    }

    #[test]
    fn a_narrow_terminal_keeps_the_clock_and_the_paths_own_name() {
        let c = Card::new("read", "call_00000007")
            .target("crates/sessionlog/src/protocol.rs")
            .phase(Phase::Finished {
                outcome: Outcome::Ok,
                elapsed_ms: Some(1_200),
            });
        for w in [20usize, 40, 60, 100] {
            let cfg = CardConfig { width: w, ..cfg() };
            assert!(c.header(&cfg).width() <= w, "{w}: {:?}", h(&c, &cfg));
        }
        let narrow = h(&c, &CardConfig { width: 44, ..cfg() });
        // **The path keeps the half that identifies it, and the tail keeps its place.** A tail
        // dropped whole takes the CLOCK with it, and the clock is the one number that says a
        // running call is alive. What gives way is the SUBJECT, from the LEFT, because a path
        // is recognised by where it ends.
        assert!(narrow.contains("protocol.rs"), "{narrow}");
        assert!(
            narrow.contains("1.2s"),
            "the clock is not the thing to drop: {narrow}"
        );
        assert!(
            narrow.contains('…'),
            "and the cut is disclosed where it happened: {narrow}"
        );
    }

    /// **R25: the head cuts the target to ITS OWN viewport, and 227 shows more than 80.**
    /// More than one head may be attached to one session at different widths at the same
    /// time, so this is the only layer that can decide what a reader sees.
    #[test]
    fn the_head_cuts_a_long_target_to_its_own_viewport_and_says_so() {
        let target = format!(
            "cd /opt/secure_auth && gcc -o test_auth test_auth.c {} \
             -Llib -lsecure_auth -Wl,-rpath,/opt/secure_auth/lib && ./test_auth --selftest",
            "-Iinclude ".repeat(12)
        );
        assert!(target.len() > 227);
        let c = Card::new("bash", "call_00000007")
            .target(&target)
            .phase(Phase::Finished {
                outcome: Outcome::Ok,
                elapsed_ms: Some(900),
            });
        let wide = c.header(&CardConfig {
            width: 227,
            ..cfg()
        });
        let narrow = c.header(&CardConfig { width: 80, ..cfg() });
        assert!(wide.width() <= 227 && narrow.width() <= 80);
        assert!(wide.width() > narrow.width());
        assert!(wide.width() > 120);
        for l in [&wide, &narrow] {
            let s = l.plain();
            assert!(s.contains('…'), "the cut is not disclosed: {s:?}");
            assert!(s.contains("900ms"), "the row lost its clock: {s:?}");
        }
        // **And a target that fits is shown whole and marked not at all.**
        let short = Card::new("bash", "c1")
            .target("cargo test --workspace")
            .phase(Phase::Finished {
                outcome: Outcome::Ok,
                elapsed_ms: Some(900),
            });
        let fits = h(
            &short,
            &CardConfig {
                width: 227,
                ..cfg()
            },
        );
        assert!(fits.contains("cargo test --workspace"), "{fits}");
        assert!(!fits.contains('…'), "{fits}");
    }

    #[test]
    fn nothing_a_card_renders_ever_exceeds_the_width() {
        let c = Card::new("edit", "c1")
            .target("a/very/long/path/that/keeps/going/and/going/src/lib.rs")
            .body(
                (0..50)
                    .map(|i| Line::raw(format!("{}{i}", "x".repeat(120))))
                    .collect(),
            )
            .phase(Phase::Finished {
                outcome: Outcome::Failed("permission denied on a long path".into()),
                elapsed_ms: Some(4),
            });
        for w in [16usize, 30, 60, 120] {
            let cfg = CardConfig { width: w, ..cfg() };
            for l in c.render(&cfg) {
                assert!(l.width() <= w, "{w}: {} cols {:?}", l.width(), l.plain());
            }
        }
    }

    #[test]
    fn reasoning_is_marked_three_ways_so_no_single_loss_hides_it() {
        let out = reasoning(&body(20), false, Some(4_200), &cfg());
        assert_eq!(out[0].plain(), "Thought for 4.2s");
        assert!(out[1].plain().starts_with('┃'), "{:?}", out[1].plain());
        // The body carries the reasoning register, and the rail is faint.
        let running = reasoning(&body(4), true, None, &cfg());
        assert_eq!(running[0].plain(), "Thinking…");
        assert_eq!(role_of(&running[1], "line 0"), Some(Role::Reasoning));
        assert_eq!(role_of(&running[1], "┃"), Some(Role::Faint));
    }

    #[test]
    fn a_replayed_reasoning_block_does_not_say_zero_seconds() {
        let out = reasoning(&body(3), false, None, &cfg());
        assert_eq!(out[0].plain(), "Thought");
    }

    #[test]
    fn a_span_inside_reasoning_keeps_its_own_role_over_the_block() {
        let l = Line::new(vec![Span::raw("see "), Span::role("x()", Role::Code)]);
        let out = reasoning(&[l], true, None, &cfg());
        let sp = out[1].spans.iter().find(|s| s.content == "x()").unwrap();
        assert_eq!(
            sp.style.roles().collect::<Vec<_>>(),
            vec![Role::Reasoning, Role::Code]
        );
    }

    // ---- letibot `app/tests/tools.rs`, at the card ----

    fn call(state: CallState) -> ToolCall {
        ToolCall {
            name: "bash".into(),
            call_id: "c1".into(),
            target: String::new(),
            state,
            diff: None,
            decision: None,
            fold: Fold::Folded,
        }
    }

    /// `a_running_tool_call_shows_how_long_it_has_been_running_and_what_it_last_said`:
    /// started at 2.0s, progress at 6.2s, finished at 9.5s.
    #[test]
    fn a_running_tool_call_shows_how_long_it_has_been_running_and_what_it_last_said() {
        let running = call(CallState::Running {
            elapsed_ms: 4_200,
            note: Some("compiling letibot-tui".into()),
        });
        let screen = plain(&running.lines(120)).join("\n");
        assert!(screen.contains("Running"), "{screen}");
        assert!(screen.contains("4.2s"), "{screen}");
        assert!(screen.contains("compiling letibot-tui"), "{screen}");
        let done = call(CallState::Finished {
            outcome: Outcome::Ok,
            elapsed_ms: Some(7_500),
            inline_bytes: 214,
            full_bytes: 214,
            spill: None,
        });
        let screen = plain(&done.lines(120)).join("\n");
        assert!(
            screen.contains("Ran"),
            "past tense once it is done: {screen}"
        );
        assert!(screen.contains("7.5s"), "{screen}");
        assert!(screen.contains("214 B"), "{screen}");
    }

    /// `a_running_tool_call_says_what_it_is_running_on`.
    #[test]
    fn a_running_tool_call_says_what_it_is_running_on() {
        let mut c = call(CallState::Running {
            elapsed_ms: 0,
            note: None,
        });
        c.target = "\"cargo test --workspace\"".into();
        let screen = plain(&c.lines(120)).join("\n");
        assert!(
            screen.contains("Running \"cargo test --workspace\""),
            "a running call renders its argument, not just its verb:\n{screen}"
        );
    }

    /// `a_spilled_result_reads_as_the_harness_working_not_as_damage`.
    #[test]
    fn a_spilled_result_reads_as_the_harness_working_not_as_damage() {
        let mut c = call(CallState::Finished {
            outcome: Outcome::Ok,
            elapsed_ms: None,
            inline_bytes: 8192,
            full_bytes: 480_000,
            spill: Some("9fa3c1".into()),
        });
        c.name = "grep".into();
        let screen = plain(&c.lines(160)).join("\n");
        assert!(screen.contains("8.0 KB of 468.8 KB"), "{screen}");
        assert!(screen.contains("read_spill hash=9fa3c1"), "{screen}");
        assert!(screen.contains("the rest is kept"), "{screen}");
    }

    /// `a_call_from_a_snapshot_has_no_duration_rather_than_a_zero_one`.
    #[test]
    fn a_call_from_a_snapshot_has_no_duration_rather_than_a_zero_one() {
        let c = call(CallState::Finished {
            outcome: Outcome::Ok,
            elapsed_ms: None,
            inline_bytes: 10,
            full_bytes: 10,
            spill: None,
        });
        let head = plain(&c.lines(120))[0].clone();
        assert_eq!(head, "● Ran");
    }

    fn edit_call(target: &str) -> ToolCall {
        ToolCall {
            name: "edit".into(),
            call_id: "c1".into(),
            target: target.into(),
            state: CallState::Finished {
                outcome: Outcome::Ok,
                elapsed_ms: Some(1_000),
                inline_bytes: 64,
                full_bytes: 64,
                spill: None,
            },
            diff: Some(EditDiff {
                path: "a.rs".into(),
                rows: vec![
                    Line::raw("a.rs"),
                    Line::raw("1 - fn a() {}"),
                    Line::raw("1 + fn a() {"),
                ],
                capped_at: None,
            }),
            decision: None,
            fold: Fold::Open,
        }
    }

    /// `an_edit_card_names_its_file_once`.
    #[test]
    fn an_edit_card_names_its_file_once() {
        let count = |rows: &[String]| rows.iter().filter(|l| l.contains("a.rs")).count();
        let named = plain(&edit_call("a.rs").lines(120));
        assert_eq!(count(&named), 1, "{}", named.join("\n"));
        let unnamed = plain(&edit_call("").lines(120));
        assert_eq!(
            count(&unnamed),
            1,
            "a header with no file keeps the diff's name line:\n{}",
            unnamed.join("\n")
        );
        // And the diff replaced the byte count: a change, not a size.
        assert!(!named.iter().any(|l| l.contains("64 B")), "{named:?}");
    }

    /// `a_created_file_renders_as_all_right_panel_and_a_cap_says_so` (its cap half).
    #[test]
    fn a_capped_excerpt_says_how_long_the_file_is_now() {
        let mut c = edit_call("a.rs");
        c.diff.as_mut().unwrap().capped_at = Some(900);
        let screen = plain(&c.lines(120)).join("\n");
        assert!(
            screen.contains("the excerpt was capped; the file is 900 lines now"),
            "{screen}"
        );
    }

    /// `a_non_edit_call_never_grows_a_second_panel`.
    #[test]
    fn a_non_edit_call_never_grows_a_second_panel() {
        let mut c = edit_call("a.rs");
        c.name = "bash".into();
        let screen = plain(&c.lines(120)).join("\n");
        assert!(!screen.contains("fn a()"), "{screen}");
    }

    #[test]
    fn the_gate_rides_the_card_folded_and_opens_with_the_fold() {
        let mut c = call(CallState::Running {
            elapsed_ms: 10,
            note: None,
        });
        c.decision = Some(SettledDecision {
            verdict: super::super::decision::Verdict::Allowed,
            by_kind: "operator".into(),
            by_identity: "dead".into(),
            summary: "`bash` wants exec access".into(),
            basis: String::new(),
            advice: None,
        });
        let folded = plain(&c.lines(120));
        assert_eq!(folded[1], "  · allowed, by operator dead");
        assert_eq!(folded.len(), 2);
        c.fold = Fold::Open;
        let open = plain(&c.lines(120)).join("\n");
        assert!(open.contains("asked: `bash` wants exec access"), "{open}");
    }

    #[test]
    fn the_diff_is_built_for_the_width_the_row_will_have() {
        assert_eq!(ToolCall::diff_width(120), 116);
        assert_eq!(ToolCall::diff_width(50), 48);
    }

    #[test]
    fn writing_and_raw_calls_are_evidence_not_prose() {
        let w = WritingCall { now_ms: 0 }.line(80);
        assert_eq!(w.plain(), "⠋ writing a tool call · ctrl-x for the raw form");
        assert_eq!(role_of(&w, "writing"), Some(Role::Pending));
        assert_eq!(
            w.to_ansi(crate::style::Palette::Colour),
            "\x1b[33m⠋\x1b[0m \x1b[33mwriting a tool call\x1b[0m\x1b[2m · ctrl-x for the raw form\x1b[0m",
            "letibot's bytes: the space between spinner and words is unpainted"
        );
        let raw = RawCall {
            raw: "<function=bash>\x1b[2J<parameter=cmd>ls".into(),
        }
        .lines(80);
        assert_eq!(
            plain(&raw),
            vec![
                "┌─ raw tool call · ctrl-x",
                "│ <function=bash><parameter=cmd>ls",
                "└─"
            ]
        );
        assert_eq!(role_of(&raw[1], "<function"), Some(Role::Code));
    }
}
