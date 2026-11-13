//! Blocks with a header, a state and a bounded body: a tool call's card, and the model's
//! reasoning.
//!
//! A one-line call — `● edit(call_7) — ok · 214 B` — is the wrong shape: it drops the argument (the
//! one part a reader scans for) and the elapsed time (which answers "is it stuck"), and leaves a
//! progress note nowhere to go. The body folds in three states, and the middle one earns its place:
//! a finished `bash` call shows its last lines without being asked, because those say whether it
//! worked.
//!
//! Adapted from grok-build (xAI, Apache-2.0): `DisplayMode`, the tense-flipping verb, the head/tail
//! budgets and the reasoning rail.

use crate::markdown::code_runs;
use crate::markdown::render::runs_spans;
use crate::render::text::{truncate, truncate_owned};
use crate::render::{Line, Span, Style};
use crate::style::Role;
use crate::width::text as wt;

use super::decision::{SettledDecision, decision_detail};
use super::outcome::Outcome;
use super::text::{bytes_human, clean, clean_line, duration, header_names_the_file, one, wrap};
use super::tool_row::restates_reason;
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
    /// The cycle a fold key walks. Running blocks skip `Expanded`: a body still growing pushes the
    /// prompt off the screen a line at a time, which reads as flicker.
    pub fn next(self, running: bool) -> DisplayMode {
        match (self, running) {
            (DisplayMode::Collapsed, _) => DisplayMode::Truncated,
            (DisplayMode::Truncated, false) => DisplayMode::Expanded,
            (DisplayMode::Truncated, true) => DisplayMode::Collapsed,
            (DisplayMode::Expanded, _) => DisplayMode::Collapsed,
        }
    }
}

/// How the card is titled, and in which tense: `Reading src/main.rs` against `Read src/main.rs` is
/// the difference between wait and done, read from the first word — no colour or glyph needed.
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
    /// Map a tool name to a verb. Unknown names keep their own name, which is right: inventing a
    /// verb for a tool we do not know is a guess presented as a fact.
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

/// The fewest columns a running call's subject may keep, while the tail keeps its own: a subject
/// squeezed below a few columns says nothing, and the tail's head is the clock that says it lives.
const MIN_SUBJECT: usize = 8;

/// Where a block is in its life.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Phase {
    /// The model asked for it; nothing has run. `note` says what is happening meanwhile — the guard
    /// deciding whether the call follows from what the operator wanted — so a wait says why.
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
    /// Reconstructed from a log rather than watched live: a replay has no honest elapsed time, and
    /// `0.0s` is a measurement never taken, rendered as one that was.
    Replayed { outcome: Outcome },
}

impl Phase {
    pub fn is_running(&self) -> bool {
        matches!(self, Phase::Running { .. } | Phase::Proposed { .. })
    }
}

/// Head and tail line counts per fold state; the defaults are grok-build's shipped numbers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Budget {
    pub first_lines: usize,
    pub last_lines: usize,
    /// Cap even in [`DisplayMode::Expanded`]: 40,000 lines expanded is the conversation gone.
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
    /// Show the call id: a correlation key for a log, not something a person reads.
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

/// One tool call, ready to draw: the generic card. [`ToolCall`] is the view model a host fills.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Card {
    pub verb: Verb,
    /// The tool's own name, for the cases where the verb hides it.
    pub tool: String,
    pub call_id: String,
    /// The one argument that says what was acted on: a path, a pattern, a command line.
    pub target: String,
    /// The body, already-built lines — a diff, an excerpt, stdout. This type lays them out, whatever
    /// they are.
    pub body: Vec<Line>,
    pub phase: Phase,
    /// Bytes shown inline and bytes that exist, rendered only when they differ: `8 KB` beside a
    /// 480 KB output is a number that misleads.
    pub bytes: Option<(u64, u64)>,
    /// Content hash of the spilled full output, when there is one.
    pub spill: Option<String>,
}

fn push(l: &mut Line, s: impl Into<String>, r: Role) {
    l.spans.push(Span::role(s, r));
}

/// Faint text joined onto a faint run before it, so an `ok` tail stays one run.
fn faint(tail: &mut Vec<Span>, t: &str) {
    match tail.last_mut() {
        Some(last) if last.style == Style::of(Role::Faint) => last.content.push_str(t),
        _ => tail.push(Span::role(t, Role::Faint)),
    }
}

/// A note or a reason with its `code` spans drawn the way the settled row draws them.
fn prose(text: &str, role: Role) -> Vec<Span> {
    runs_spans(&code_runs(text), role)
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
            // The mark takes the outcome's colour without Attention's underline, as the row's does.
            Phase::Finished { outcome, .. } | Phase::Replayed { outcome } => match outcome.role() {
                Role::Attention => ('●', Role::Pending),
                r => ('●', r),
            },
        };
        // The head is built first and the subject filled in last: the tail does not shrink, since
        // half of `· 12.4s` is not a duration, and the clock is what says the call is alive.
        let mut s = Line::default();
        push(&mut s, mark.to_string(), mark_role);
        push(&mut s, " ", Role::Plain);
        push(&mut s, self.verb.label(running), Role::Strong);

        // The right-hand side: state, timing, disclosure. Only the outcome word wears the outcome;
        // separators are faint and notes and reasons are prose.
        let mut tail: Vec<Span> = Vec::new();
        let ended = |tail: &mut Vec<Span>, outcome: &Outcome| {
            if matches!(outcome, Outcome::Ok) {
                return;
            }
            let role = outcome.role();
            faint(tail, " · ");
            if role == Role::Faint {
                faint(tail, outcome.word());
            } else {
                tail.push(Span::role(outcome.word(), role));
            }
            if let Some(r) = outcome.reason() {
                let r = clean_line(r);
                if !r.trim().is_empty() && !r.trim().eq_ignore_ascii_case(outcome.word()) {
                    faint(tail, " · ");
                    // A faint outcome (backgrounded) keeps its reason faint, as the row does.
                    let body = if role == Role::Faint {
                        role
                    } else {
                        Role::Plain
                    };
                    tail.extend(prose(&r, body));
                }
            }
        };
        let note = |tail: &mut Vec<Span>, n: &str| {
            faint(tail, " · ");
            tail.extend(prose(&clean_line(n), Role::Plain));
        };
        match &self.phase {
            Phase::Proposed { note: n } => match n {
                // The note beats the state: "proposed" says what it is, the note says why nothing
                // is happening yet, which is the question the reader has.
                Some(n) => note(&mut tail, n),
                None => faint(&mut tail, " · proposed"),
            },
            Phase::Running {
                elapsed_ms,
                note: n,
            } => {
                faint(&mut tail, &format!(" · {}", duration(*elapsed_ms)));
                if let Some(n) = n {
                    note(&mut tail, n);
                }
            }
            Phase::Finished {
                outcome,
                elapsed_ms,
            } => {
                if let Some(ms) = elapsed_ms {
                    faint(&mut tail, &format!(" · {}", duration(*ms)));
                }
                ended(&mut tail, outcome);
            }
            Phase::Replayed { outcome } => ended(&mut tail, outcome),
        }
        if let Some((inline, full)) = self.bytes
            && inline != full
        {
            faint(&mut tail, &format!(" · {inline} B of {full} B"));
        }
        if let Some(h) = &self.spill {
            faint(&mut tail, &format!(" · spill {h}"));
        }
        if tail.is_empty() {
            return truncate_owned(s, cfg.width);
        }
        let tail_width: usize = tail.iter().map(|sp| wt::width(&sp.content)).sum();
        // The call id sits beside the subject, and is measured before it for the same reason the
        // tail is.
        let id_str = if cfg.show_id {
            format!(" ({})", clean_line(&self.call_id))
        } else {
            String::new()
        };

        // What the subject may have: everything the tail and the id have not claimed, with a floor
        // so that a few columns still say what it is and the frame's trim takes the tail's end.
        if !self.target.is_empty() {
            // A card is text this head did not author: target, call id, note and outcome reason all
            // come from a tool call or from the daemon.
            let target = clean_line(&self.target);
            let spare = cfg
                .width
                .saturating_sub(s.width() + tail_width + wt::width(&id_str) + 1);
            let room = spare.max(MIN_SUBJECT);
            // A path is cut from the left and anything else from the right — the transcript row's
            // `shorten_subject` does the same, so one call does not read two ways. A glob or a quoted
            // sentence is not a path however many separators it has: cutting `**/*.{md,json}` from the
            // left loses the fact that it is a glob.
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
        s.spans.extend(tail);
        // One last guard for the case the floor above creates, taking the END so the clock survives.
        truncate_owned(s, cfg.width)
    }

    /// The whole card.
    pub fn render(&self, cfg: &CardConfig) -> Vec<Line> {
        let mut out = vec![self.header(cfg)];
        // The header prints the reason; a body line that only restates it is dropped.
        let own: Vec<Line> = match &self.phase {
            Phase::Finished { outcome, .. } | Phase::Replayed { outcome } => self
                .body
                .iter()
                .filter(|l| !restates_reason(&clean_line(&l.plain()), outcome))
                .cloned()
                .collect(),
            _ => self.body.clone(),
        };
        if cfg.mode == DisplayMode::Collapsed || own.is_empty() {
            if cfg.mode == DisplayMode::Collapsed && !own.is_empty() {
                out.push(one(format!("  … {} lines", own.len()), Role::Faint));
            }
            return out;
        }
        let body = match cfg.mode {
            DisplayMode::Expanded => head_tail(&own, cfg.budget.expanded_max, 0),
            _ => head_tail(&own, cfg.budget.first_lines, cfg.budget.last_lines),
        };
        for l in body {
            out.push(truncate_owned(indent(l, 2), cfg.width));
        }
        out
    }
}

/// `l` with `n` columns of blank before it.
pub(crate) fn indent(mut l: Line, n: usize) -> Line {
    l.spans.insert(0, Span::raw(" ".repeat(n)));
    l
}

/// Keep `first` lines, then `last`, and say how many went: the marker is a separator row and not
/// a line of content, since a reader must never mistake the elision for output — and the count is
/// the disclosure.
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

/// Columns a caller must subtract from the width before wrapping a reasoning body, so the wrap and
/// the rail agree.
pub const REASONING_RAIL_WIDTH: usize = 2;

/// The model's reasoning, rendered so it can never be mistaken for its answer: the word, the rail
/// and the [`Role::Reasoning`] attribute — three signals, because any one is lost somewhere.
/// `elapsed_ms` is `None` for a replay, which renders as `Thought` rather than `Thought for 0.0s`.
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
    let mut out = vec![truncate_owned(head, cfg.width)];
    if cfg.mode == DisplayMode::Collapsed {
        if !body.is_empty() {
            out.push(one(format!("  … {} lines", body.len()), Role::Faint));
        }
        return out;
    }
    // The body was wrapped two columns narrower than the rail: otherwise the block is one row
    // taller than the space reserved for it and pushes everything below it down every frame.
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
        out.push(truncate_owned(row, cfg.width));
    }
    out
}

/// A file edit's before/after, as the host rendered it — rano's `sidediff` edit view, split or
/// unified by the operator's toggle, already laid out at the width its row will have.
///
/// `rows[0]` is the edit view's name line (the file's path); a widget drops it when its own header
/// already names the file.
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
    /// The rows to draw under a header that says `target`: the name line dropped when the header
    /// names the file, and the cap's disclosure appended.
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

/// One live tool call, as a card.
///
/// The body is empty while a call is in flight, and that is deliberate: a finished event carries
/// digests and byte counts, never a payload, which reaches a head as a transcript row.
///
/// `Running::elapsed_ms` must be computed from a start and a now on the same clock. Mixing the
/// log's start with the head's now reports the time since the started event, a smaller number that
/// reads like progress.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolCall {
    /// The tool's name: `bash`, `read`, `edit`.
    pub name: String,
    pub call_id: String,
    /// The bounded display target — a path, a pattern, a command line. Empty draws nothing: a
    /// guess is worse than a blank.
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
    /// The width to render [`EditDiff::rows`] at: the card indents its body by two and a transcript
    /// steps the card in, so the panels are built for the width the row will actually have.
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
                // The disclosure goes in the body, not the header tail: the tail is dropped whole
                // when it does not fit, and "there is more, here is how to get it" may not vanish.
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
                    // A snapshot has no timestamps: `0.0s` would be a measurement never taken.
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
        // Split or unified is the operator's toggle and nothing else — a width gate ever gave only
        // a cramped diff or no diff at all.
        if card.verb.is_an_edit()
            && matches!(self.state, CallState::Finished { .. })
            && let Some(d) = &self.diff
        {
            body = d.rows_under(&card.target);
        }
        // The decision it was gated by, in the dim register. Folded it is one line — who decided and
        // how; open it adds what the oracle was shown and what it said back.
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
            // Never `Collapsed`: the spill disclosure lives in the body and a fold is not a licence
            // to hide it.
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

/// The indent a turn's working is stepped in by: thinking and acting sit one step under the
/// conversation, which gives a turn readable levels from the vocabulary already on the screen.
///
/// Two columns, matching the reasoning rail, so the page reads as one repeated step — given up
/// below sixty columns, where it is a bigger fraction than the hierarchy is worth.
pub fn activity_indent(w: usize) -> usize {
    if w >= 60 { REASONING_RAIL_WIDTH } else { 0 }
}

/// The affordance that stands in for a tool call while the model is writing it: the question a
/// half-written `<parameter=` accidentally answered — is something happening — without showing the
/// markup. `now_ms` drives the spinner off the host's clock, so a replay animates as the live
/// session did.
pub struct WritingCall {
    pub now_ms: u64,
}

impl WritingCall {
    pub fn line(&self, width: usize) -> Line {
        let spin = super::text::spinner(self.now_ms).to_string();
        truncate(
            &Line::new(vec![
                Span::role(spin, Role::Pending),
                // The space between the spinner and the words is unpainted: two pending runs either
                // side of a plain space.
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

/// The raw, unparsed text of a tool call, behind `ctrl-x`: a labelled block rather than inline,
/// because the whole point is that this is not the assistant speaking. It is foreign markup, so it
/// is cleaned before it is wrapped.
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
        // And the elapsed time is there while it runs, which is the answer to "is it stuck".
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
        // Not success, and not a person's to answer either: information for the model.
        assert_eq!(role_of(&l, "ABSTAINED"), Some(Role::Info));
        assert_eq!(role_of(&l, "●"), Some(Role::Info));
    }

    #[test]
    fn a_settled_cards_outcome_is_on_its_word_only() {
        use crate::style::Palette;
        let c = Card::new("grep", "c1")
            .target("a.md")
            .phase(Phase::Finished {
                outcome: Outcome::Abstained("`a|b` does not occur".into()),
                elapsed_ms: Some(40),
            });
        assert_eq!(
            c.header(&cfg()).to_ansi(Palette::Colour),
            "\x1b[36m●\x1b[0m \x1b[1mSearched\x1b[0m a.md\x1b[2m · 40ms · \x1b[0m\x1b[36mABSTAINED\x1b[0m\x1b[2m · \x1b[0m\x1b[36ma|b\x1b[0m does not occur"
        );
        // An ok card keeps its one faint run.
        let mut ok = Card::new("read", "c1")
            .target("a.rs")
            .phase(Phase::Finished {
                outcome: Outcome::Ok,
                elapsed_ms: Some(1_200),
            });
        ok.bytes = Some((8, 9));
        assert_eq!(
            ok.header(&cfg()).to_ansi(Palette::Colour),
            "\x1b[32m●\x1b[0m \x1b[1mRead\x1b[0m a.rs\x1b[2m · 1.2s · 8 B of 9 B\x1b[0m"
        );
    }

    #[test]
    fn a_refused_card_underlines_only_its_word() {
        let c = Card::new("bash", "c1")
            .target("rm -rf /")
            .body(vec![
                Line::raw("reason: the call was denied (d1)"),
                Line::raw("more"),
            ])
            .phase(Phase::Finished {
                outcome: Outcome::Denied("the call was denied (d1)".into()),
                elapsed_ms: Some(3),
            });
        let out = c.render(&cfg());
        let l = &out[0];
        let underlined: Vec<&str> = l
            .spans
            .iter()
            .filter(|s| l.style.patch(&s.style).top() == Role::Attention)
            .map(|s| s.content.as_str())
            .collect();
        assert_eq!(underlined, vec!["REFUSED"]);
        assert_eq!(role_of(l, "●"), Some(Role::Pending));
        assert_eq!(role_of(l, "the call"), Some(Role::Plain));
        // The header says the reason; the body does not say it again.
        assert_eq!(plain(&out[1..]), vec!["  more"]);
    }

    #[test]
    fn a_note_draws_its_code_spans() {
        let c = Card::new("bash", "c1").phase(Phase::Running {
            elapsed_ms: 9_000,
            note: Some("compiling `rano`".into()),
        });
        let l = c.header(&cfg());
        assert_eq!(l.plain(), "◐ Running · 9.0s · compiling rano");
        assert_eq!(role_of(&l, "rano"), Some(Role::Code));
        assert_eq!(role_of(&l, "compiling"), Some(Role::Plain));
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
        // The path keeps the half that identifies it, and the tail keeps its place: a tail dropped
        // whole takes the clock with it, so what gives way is the subject, from the left.
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

    /// The head cuts the target to its own viewport: more than one head may be attached at
    /// different widths, so this is the only layer that can decide what a reader sees.
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
        // And a target that fits is shown whole and marked not at all.
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

    /// A running call says how long it has been going and what it last said.
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

    /// A running call renders its argument, not just its verb.
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

    /// A spilled result reads as the harness working, not as damage.
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

    /// A call from a snapshot has no duration rather than a zero one.
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

    /// An edit card names its file once.
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

    /// A capped excerpt says how long the file is now.
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

    /// A non-edit call never grows a second panel.
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
