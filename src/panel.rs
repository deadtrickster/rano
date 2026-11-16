//! The panel: one letibot session beside the text, with its conversation, its composer and the
//! folder it follows.
//!
//! Drawing is [`crate::agent::panel`]. The socket is not here: a host opens it, translates the
//! [`Act`]s this module hands out into frames, and gives the frames back through
//! [`Panel::apply`]. So the editor never blocks on a wire, and a test can drive a whole
//! conversation without one.
//!
//! The panel is a **mode** of the editor (`panel` in the keymaps): while it holds the keyboard its
//! map is in effect, so the key bar and the help pages name its keys the way they name every other
//! mode's.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::ask::AskEvent;
use crate::letibot::{
    AnswerMode, Citation, CitationKind, ContextAsk, DeltaTarget, EditorContext, Event, Frame,
    MarkOutcome, NoDaemon, Session, ToolEdit,
};
use crate::markdown::RenderOptions;
use crate::marks::{Mark, Outcome, Severity};
use crate::render::{Line, Span};
use crate::review::Change;
use crate::send::SendEvent;

// The vocabulary lives in `conversation` so a build without the panel still has it.
pub use crate::conversation::{
    Act, Chip, Markset, PanelState, Reply, Row, folder_for, remember_in, remembered_in,
};
use crate::style::Role;

/// What a place is called on the wire and in the composer: who captured it.
const ORIGIN: &str = "rano";

/// A prompt asked for before there was anything to ask: held until the `Hello` lands.
#[derive(Debug)]
struct Pending {
    text: String,
    context: Option<EditorContext>,
}

/// The panel, and the conversation it is showing.
#[derive(Debug)]
pub struct Panel {
    /// Drawn beside the text.
    pub open: bool,
    /// `alt-m`: the focused surface takes the whole width even on a wide terminal.
    pub zoom: bool,
    /// The folder this conversation belongs to. The host resolves a socket from it.
    pub folder: PathBuf,
    /// A folder the focused buffer has moved to, waiting for the operator to follow it. Following
    /// is explicit, so a stray `open-file` does not drop a live stream.
    pub moved: Option<PathBuf>,
    pub session: String,
    pub title: String,
    pub state: PanelState,
    /// Whether a session was CHOSEN — by the pick, by the remembered one, or by the rule. The bare
    /// attach the daemon requires as its first frame lands on its current session and answers a
    /// Hello, which is not a choice: without this the panel would never move off it.
    picked: bool,
    /// What is typed while the pick is up: a title, an id prefix, or a row number. The list
    /// narrows to it, the way letibot's session picker does — a digit is not a different gesture.
    pick_query: String,
    /// What the state says in full: the no-daemon line, a refusal, what just happened.
    pub said: String,
    pub rows: Vec<Row>,
    /// The composer's line, and its cursor in chars.
    pub input: String,
    pub cursor: usize,
    /// Content rows hidden above the top of the body; `follow` pins the end instead.
    pub scroll: usize,
    pub follow: bool,
    /// The folder's sessions, newest first, as the daemon listed them.
    pub sessions: Vec<Session>,
    /// The pick's cursor: an index into [`Panel::sessions`].
    pub pick: usize,
    /// Where the last session per folder is remembered, when the host wants that at all. Unset in
    /// tests, and in a host that keeps its own record.
    pub state_file: Option<PathBuf>,
    /// What the next prompt carries beside its words: `alt-s` and `alt-k` put places here.
    pub chips: Vec<Chip>,
    /// The file each reply's highlights last marked, so a newer reply replaces an older one.
    cited_by: HashMap<PathBuf, String>,
    /// The label each mark was set under, which is how an outcome is said back to the model.
    labels: HashMap<String, String>,
    /// What the operator decided about earlier marks, sent on the next prompt and then forgotten.
    pub outcomes: Vec<MarkOutcome>,
    /// Marks the host has not landed yet: a file that is not open is kept until it is.
    pending_marks: Vec<Markset>,
    /// Prompts asked for before the panel could send them.
    waiting: Vec<Pending>,
    /// The file the folder was last worked out for, so the git call happens once per buffer.
    seen_name: Option<PathBuf>,
    acts: Vec<Act>,
    /// When the running turn started: the status line's clock, and what makes a spinner move
    /// without a token arriving.
    turn_started: Option<std::time::Instant>,
    /// The second the status line last said, so the host redraws once a second and not more.
    shown_second: u64,
    /// When the turn last emitted anything — a delta, a call starting or settling. `slow` on the
    /// status line means SILENT past the widget's threshold, never merely old: a turn streaming
    /// text for a minute is not slow.
    last_output: Option<std::time::Instant>,
    /// The card the head is waiting on: its request id, so another head's answer clears exactly
    /// this ask, and the one phrase the status line names.
    pending_ask: Option<(String, String)>,
    /// A new turn's first text delta opens a NEW reply row: without this, two turns' replies
    /// merged into one row whenever nothing else was filed between them, and a replayed
    /// conversation made that visible.
    fresh_turn: bool,
    /// The conversation's tool fold (`M-t` in the panel): with false, a run of settled calls
    /// collapses to one line and only the newest and the failed stay open.
    pub tools_open: bool,
    /// Tool calls still running: `(call_id, row, name, started, call markup)`.
    running: Vec<(String, usize, String, std::time::Instant, String)>,
    /// The model's raw call markup for the call about to start: the wire sends a tool's
    /// arguments as that stream and nowhere else.
    call_markup: String,
    /// The conversation at the last built width, oldest first.
    built: Vec<Line>,
    built_for: Option<(usize, u64)>,
    version: u64,
}

impl Default for Panel {
    fn default() -> Self {
        Panel {
            open: false,
            zoom: false,
            folder: PathBuf::new(),
            moved: None,
            session: String::new(),
            title: String::new(),
            state: PanelState::Closed,
            picked: false,
            pick_query: String::new(),
            said: String::new(),
            rows: Vec::new(),
            input: String::new(),
            cursor: 0,
            scroll: 0,
            follow: true,
            sessions: Vec::new(),
            pick: 0,
            state_file: None,
            chips: Vec::new(),
            cited_by: HashMap::new(),
            labels: HashMap::new(),
            outcomes: Vec::new(),
            pending_marks: Vec::new(),
            waiting: Vec::new(),
            seen_name: None,
            acts: Vec::new(),
            turn_started: None,
            fresh_turn: false,
            shown_second: 0,
            last_output: None,
            pending_ask: None,
            tools_open: false,
            running: Vec::new(),
            call_markup: String::new(),
            built: Vec::new(),
            built_for: None,
            version: 0,
        }
    }
}

impl Panel {
    /// Something changed that the built rows depend on.
    fn touch(&mut self) {
        self.version = self.version.wrapping_add(1);
    }

    /// Talking to a session at all.
    pub fn attached(&self) -> bool {
        !self.session.is_empty()
    }

    /// Show the panel and hold the keyboard. The same folder keeps its conversation; a folder it
    /// has not met is a conversation of its own.
    pub fn open(&mut self, folder: PathBuf) {
        self.open = true;
        self.seen_name = None;
        if folder != self.folder || self.state == PanelState::Closed {
            self.folder = folder;
            self.connect();
        }
    }

    /// Hide it. The connection, the rows and the session stay: hiding is not leaving, and showing
    /// it again is the split it was, not the zoom one frame was under.
    pub fn close(&mut self) {
        self.open = false;
        self.zoom = false;
    }

    /// Ask the host for the folder's daemon and its sessions.
    fn connect(&mut self) {
        self.state = PanelState::Connecting;
        self.session.clear();
        self.title.clear();
        self.said.clear();
        self.moved = None;
        self.drop_turn_state();
        self.acts.push(Act::Connect(self.folder.clone()));
        self.touch();
    }

    /// A different conversation has none of the old one's turn state: a status row naming the
    /// previous session's ask, or clocking its turn, is the failure this row exists to remove.
    /// `connect` and `new_session` both start one, and `follow` goes through `connect`.
    fn drop_turn_state(&mut self) {
        self.turn_started = None;
        self.shown_second = 0;
        self.last_output = None;
        self.pending_ask = None;
    }

    /// The focused buffer's folder, when it is not the one on screen. A live conversation is not
    /// dropped by opening a file; one that never connected is simply the new folder's.
    pub fn folder_moved(&mut self, folder: PathBuf) {
        if !self.open {
            return;
        }
        if folder == self.folder {
            self.moved = None;
            return;
        }
        if self.attached() {
            self.moved = Some(folder);
        } else {
            self.folder = folder;
            self.connect();
        }
    }

    /// Follow the moved folder: a new conversation, because it is a different daemon.
    pub fn follow(&mut self) {
        let Some(folder) = self.moved.take() else {
            self.said = "the folder has not moved".to_string();
            self.touch();
            return;
        };
        self.rows.clear();
        self.folder = folder;
        self.connect();
    }

    /// Start a daemon for a folder the launcher says has none.
    pub fn start(&mut self) {
        if !matches!(self.state, PanelState::Offline { startable: true }) {
            return;
        }
        self.acts.push(Act::Start(self.folder.clone()));
        self.state = PanelState::Connecting;
        self.said.clear();
        self.touch();
    }

    /// No daemon, in the three cases the design says differently: nothing installed, nothing
    /// running for this folder (which Enter can start), and a launcher that cannot be asked.
    pub fn no_daemon(&mut self, why: &NoDaemon) {
        self.state = PanelState::Offline {
            startable: matches!(why, NoDaemon::NotRunning { .. }),
        };
        self.said = why.to_string();
        if !self.waiting.is_empty() {
            self.waiting.clear();
            self.said.push_str("; nothing was sent");
        }
        self.touch();
    }

    /// The acts the host must perform, in the order they were asked for.
    pub fn take_acts(&mut self) -> Vec<Act> {
        std::mem::take(&mut self.acts)
    }

    /// A frame off the socket.
    pub fn apply(&mut self, frame: &Frame) -> bool {
        // A page of rows is the head's to draw, and a frame or event this build does not know is
        // the pump's to count: neither changes a row here.
        match frame {
            Frame::RowsFetched { .. } | Frame::Unknown { .. } => return false,
            Frame::Event {
                event: Event::Unknown { .. },
                ..
            } => return false,
            _ => {}
        }
        self.touch();
        match frame {
            Frame::Hello(hello) => {
                if !hello.session_id.is_empty() {
                    self.session.clone_from(&hello.session_id);
                }
                self.title = hello
                    .sessions
                    .iter()
                    .find(|s| s.id == self.session)
                    .map(|s| s.title.clone())
                    .unwrap_or_default();
                if !hello.sessions.is_empty() {
                    self.sessions.clone_from(&hello.sessions);
                }
                self.state = PanelState::Idle;
                self.said.clear();
                self.follow = true;
                if let Some(snapshot) = &hello.snapshot {
                    self.replay(snapshot);
                }
                self.remember();
                self.flush();
            }
            Frame::Sessions {
                created, sessions, ..
            } => {
                self.sessions.clone_from(sessions);
                if let Some(id) = created {
                    self.attach(id);
                } else if !self.picked
                    || matches!(self.state, PanelState::Connecting | PanelState::Picking)
                {
                    self.choose();
                }
            }
            Frame::Accepted { .. } => self.state = PanelState::Thinking,
            Frame::Rejected { reason, .. } => {
                self.state = PanelState::Idle;
                self.said = format!("the daemon refused that: {reason}");
            }
            Frame::Bye { reason } => {
                // A call in flight when the daemon left will never settle: its note says so,
                // and the half-streamed markup is not the next call's.
                self.call_markup.clear();
                self.settle_running("abandoned");
                self.state = PanelState::Offline { startable: false };
                self.said = if reason.is_empty() {
                    "the daemon left".to_string()
                } else {
                    format!("the daemon left: {reason}")
                };
            }
            Frame::Resync { .. } => {
                self.call_markup.clear();
                self.said = "the stream restarted".to_string();
            }
            Frame::Event { event, .. } => self.event(event),
            // Answered before the match, and here only because the compiler asks for every arm.
            Frame::RowsFetched { .. } | Frame::Unknown { .. } => return false,
        }
        true
    }

    /// Attach to a session: the host does the frame, and the `Hello` is what settles the panel.
    fn attach(&mut self, session: &str) {
        if session.is_empty() {
            return;
        }
        self.session = session.to_string();
        self.picked = true;
        self.state = PanelState::Connecting;
        // A picked session is a different conversation as much as a followed folder is.
        self.drop_turn_state();
        self.acts.push(Act::Attach(session.to_string()));
        self.touch();
    }

    /// Which of the folder's sessions the list means: the remembered one, else the only one, else a
    /// pick.
    fn choose(&mut self) {
        let remembered = self
            .state_file
            .as_deref()
            .and_then(|f| remembered_in(f, &self.folder));
        if let Some(id) = remembered
            && self.sessions.iter().any(|s| s.id == id)
        {
            self.attach(&id);
            return;
        }
        match self.sessions.len() {
            0 => {
                // Look UP before saying there is nothing: the operator's session usually belongs to
                // the project above the file's folder, and that is the one to offer. Each parent is
                // asked in turn until one has sessions, and the top says so instead of leaving a
                // blank panel with no way in.
                if let Some(parent) = self.folder.parent().map(Path::to_path_buf) {
                    self.state = PanelState::Connecting;
                    self.said = format!("no session here; asking {}", parent.display());
                    self.acts.push(Act::Connect(parent));
                } else {
                    self.state = PanelState::Idle;
                    self.said =
                        "no session anywhere above this folder: enter starts one".to_string();
                }
            }
            1 => {
                let id = self.sessions[0].id.clone();
                self.attach(&id);
            }
            _ => {
                self.state = PanelState::Picking;
                self.pick = 0;
                self.pick_query.clear();
                self.follow = true;
            }
        }
        self.touch();
    }

    /// The sessions a query narrows to: an id prefix or a title substring, case-insensitive, in the
    /// list's own order. A number is the row's number in the FULL list, so what is typed and what is
    /// on screen agree — a picker that numbers two different lists is the two-enumerations defect.
    fn picked_sessions(&self) -> Vec<(usize, &Session)> {
        let q = self.pick_query.to_lowercase();
        self.sessions
            .iter()
            .enumerate()
            .filter(|(i, s)| {
                if q.is_empty() {
                    return true;
                }
                if q.chars().all(|c| c.is_ascii_digit()) {
                    return q.parse::<usize>().ok() == Some(i + 1);
                }
                s.id.starts_with(&q) || s.title.to_lowercase().contains(&q)
            })
            .collect()
    }

    /// The pick took the highlighted row, or the only match when a query narrowed to one.
    fn pick_now(&mut self) {
        let list = self.picked_sessions();
        let id = if list.len() == 1 {
            Some(list[0].1.id.clone())
        } else {
            list.iter()
                .find(|(i, _)| *i == self.pick)
                .map(|(_, s)| s.id.clone())
        };
        let Some(session) = id else {
            self.said = "no session matches that — esc closes the list".to_string();
            self.touch();
            return;
        };
        self.attach(&session);
    }

    /// A session of this folder, made and then attached.
    fn new_session(&mut self) {
        self.acts.push(Act::New);
        self.state = PanelState::Connecting;
        self.said.clear();
        self.drop_turn_state();
        self.touch();
    }

    /// A snapshot attach: the conversation as of the snapshot's seq, drawn at once rather than
    /// an empty pane until the next event. The rows are REPLACED — whatever a live event drew
    /// between the attach and this hello is in the snapshot already.
    fn replay(&mut self, snapshot: &crate::letibot::Snapshot) {
        use crate::letibot::TranscriptItem as Item;
        self.rows.clear();
        self.running.clear();
        self.call_markup.clear();
        for row in &snapshot.items {
            match &row.item {
                Some(Item::User { text, speaker }) => {
                    // Only the operator's own words are the User row: a job's completion is a
                    // note, and a peer's message names the peer rather than reading as the
                    // person's instruction.
                    match speaker.as_str() {
                        "" | "operator" => self.rows.push(Row::User(text.clone())),
                        "agent" => self.rows.push(Row::Note(text.clone())),
                        peer => self.rows.push(Row::User(format!("{peer}: {text}"))),
                    }
                }
                Some(Item::Assistant { text }) => {
                    self.rows.push(Row::Reply(Box::default()));
                    if let Some(Row::Reply(reply)) = self.rows.last_mut() {
                        reply.push(text);
                    }
                }
                Some(Item::ToolResult {
                    call_id,
                    name,
                    outcome,
                    why,
                    payload,
                    edits,
                }) => {
                    // A replayed row HAS its payload: the folded line count is real. No
                    // inline/full byte counts exist on it, so `bytes` stays unset rather
                    // than claiming full == inline.
                    let payload: Vec<Line> = payload.lines().map(Line::raw).collect();
                    let mut t = crate::agent::tool_row::ToolRow::new(
                        name,
                        call_id,
                        settled_outcome(outcome, why),
                        payload,
                    );
                    t.fold = if matches!(
                        t.outcome,
                        crate::agent::outcome::Outcome::Failed(_)
                            | crate::agent::outcome::Outcome::Denied(_)
                    ) {
                        crate::agent::Fold::Open
                    } else {
                        crate::agent::Fold::Folded
                    };
                    self.rows.push(Row::Tool(Box::new(t)));
                    self.edits(call_id, edits);
                }
                Some(Item::Notice { text }) => self.rows.push(Row::Note(text.clone())),
                // Reasoning folds, offers are for the model, a row with no reconciled content
                // is skipped rather than drawn empty.
                Some(Item::Other { .. }) | None => {}
            }
        }
        // The turn in flight: its streaming text is no transcript row yet, so it follows the
        // rows. Its SETTLED calls already have tool_result rows above — only the proposed and
        // running ones get a note, registered so a late finish replaces the note in place.
        if let Some(turn) = &snapshot.turn {
            for call in turn.calls.iter().filter(|c| c.running) {
                // The daemon measured the start: the duration the settled row will print is
                // real, not the time since this attach.
                let started = if call.started_ts > 0 {
                    let now_ms = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_millis() as u64)
                        .unwrap_or(0);
                    std::time::Instant::now()
                        - std::time::Duration::from_millis(now_ms.saturating_sub(call.started_ts))
                } else {
                    std::time::Instant::now()
                };
                self.running.push((
                    call.call_id.clone(),
                    self.rows.len(),
                    call.name.clone(),
                    started,
                    // No streamed markup survives a restart: a replayed call that settles
                    // draws its verb alone rather than a target nobody has.
                    String::new(),
                ));
                self.rows.push(Row::Note(format!("{} …", call.name)));
            }
            if turn.running && !turn.text.is_empty() {
                self.rows.push(Row::Reply(Box::default()));
                if let Some(Row::Reply(reply)) = self.rows.last_mut() {
                    reply.push(&turn.text);
                }
            }
            if turn.running {
                self.state = PanelState::Thinking;
                // The snapshot has no start time: the status line's "started before this head
                // attached" is the honest clock. Nothing is known about its silence either,
                // so the row is not the slow colour.
                self.turn_started = None;
                self.last_output = None;
            }
        }
        // A card still open is named exactly as if it had just been asked, and answered on
        // whichever head can decide.
        if let Some(card) = snapshot.open_decisions.last() {
            self.pending_ask = Some((
                card.req_id.clone(),
                format!("{} · {}", card.kind, card.summary),
            ));
            self.state = PanelState::Waiting;
        }
        // The next delta continues the snapshot's streaming reply; a NEW turn opens a new one.
        self.fresh_turn = false;
        self.follow = true;
    }

    fn event(&mut self, event: &Event) {
        match event {
            Event::TurnStarted { .. } => {
                // The clock the status line reads, and no ask is pending in a fresh turn.
                let now = std::time::Instant::now();
                self.turn_started = Some(now);
                self.last_output = Some(now);
                self.pending_ask = None;
                self.fresh_turn = true;
                self.state = PanelState::Thinking;
            }
            Event::Delta {
                target: DeltaTarget::Text,
                text,
                ..
            } => {
                self.last_output = Some(std::time::Instant::now());
                if self.fresh_turn || !matches!(self.rows.last(), Some(Row::Reply(_))) {
                    self.rows.push(Row::Reply(Box::default()));
                }
                self.fresh_turn = false;
                if let Some(Row::Reply(reply)) = self.rows.last_mut() {
                    reply.push(text);
                }
                // The view is left where the operator put it: a reader who scrolled up to an
                // earlier reply is not dragged to the end by every delta. Their own message, or a
                // new conversation, is what pins it again.
                self.state = PanelState::Thinking;
            }
            // Thinking is not prose and the panel folds it; the raw call markup is kept —
            // it is the only copy of a tool call's arguments this side of the wire, and a
            // settled row reads its TARGET from it. It is never filed as the payload, which
            // is the tool's output and not on this wire at all.
            Event::Delta { target, text, .. } => {
                // Streaming call markup is the turn emitting, as much as prose is.
                self.last_output = Some(std::time::Instant::now());
                if *target == DeltaTarget::ToolCall {
                    self.call_markup.push_str(text);
                }
            }
            Event::ToolStarted { call_id, name, .. } => {
                self.last_output = Some(std::time::Instant::now());
                // A call that has not settled is a NOTE, not a ToolRow: the row is "one SETTLED
                // tool call" and its Outcome has no running variant, so drawing one here would
                // claim `ok` for as long as the command runs. The streamed arguments are kept
                // for the target a settled row will carry.
                // One call's markup out of the stream; anything after it is the next call's.
                let (markup, rest) = split_call_markup(std::mem::take(&mut self.call_markup));
                self.call_markup = rest;
                self.running.push((
                    call_id.clone(),
                    self.rows.len(),
                    name.clone(),
                    std::time::Instant::now(),
                    markup,
                ));
                self.rows.push(Row::Note(format!("{name} …")));
            }
            Event::ToolFinished {
                call_id,
                outcome,
                why,
                edits,
                bytes,
                ..
            } => {
                self.last_output = Some(std::time::Instant::now());
                let settled = settled_outcome(outcome, why);
                match self.running.iter().position(|(id, ..)| id == call_id) {
                    Some(at) => {
                        let (_, row, name, started, markup) = self.running.remove(at);
                        // The wire carries no tool OUTPUT (ToolFinished is outcome, why and
                        // edits): the payload stays empty rather than filing the arguments
                        // under a field the widget documents as the result, where `· N lines`
                        // would count arguments while reading as the output.
                        let mut t = crate::agent::tool_row::ToolRow::new(
                            &name,
                            call_id,
                            settled,
                            Vec::new(),
                        );
                        t.target = call_target(&markup);
                        t.bytes = *bytes;
                        // A failure's reason is the reason you read the row: the failed call
                        // draws it whole, wrapped, because this surface has no unfold key.
                        t.fold = if matches!(
                            t.outcome,
                            crate::agent::outcome::Outcome::Failed(_)
                                | crate::agent::outcome::Outcome::Denied(_)
                        ) {
                            crate::agent::Fold::Open
                        } else {
                            crate::agent::Fold::Folded
                        };
                        t.elapsed_ms =
                            Some(started.elapsed().as_millis().min(u64::MAX as u128) as u64);
                        if let Some(slot) = self.rows.get_mut(row) {
                            *slot = Row::Tool(Box::new(t));
                        }
                    }
                    None => self.rows.push(Row::Note(if why.is_empty() {
                        outcome.clone()
                    } else {
                        format!("{outcome}: {why}")
                    })),
                }
                self.edits(call_id, edits);
            }
            Event::TurnFinished { ends_turn, .. } => {
                // A round that is not the end of the prompt leaves the panel thinking, which is
                // what only the daemon can tell.
                self.state = if *ends_turn {
                    self.turn_started = None;
                    self.last_output = None;
                    PanelState::Idle
                } else {
                    PanelState::Thinking
                };
            }
            Event::TurnInterrupted { reason, .. } => {
                // A call in flight when the turn ended never gets its finish: its note says so
                // rather than standing as a call that will never settle.
                self.call_markup.clear();
                self.turn_started = None;
                self.last_output = None;
                self.pending_ask = None;
                self.settle_running("interrupted");
                self.rows.push(Row::Note(if reason.is_empty() {
                    "interrupted".to_string()
                } else {
                    format!("interrupted: {reason}")
                }));
                self.state = PanelState::Idle;
            }
            Event::TurnFailed { error, .. } => {
                self.call_markup.clear();
                self.turn_started = None;
                self.last_output = None;
                self.pending_ask = None;
                self.settle_running("not run");
                self.rows.push(Row::Note(format!("failed: {error}")));
                self.state = PanelState::Idle;
            }
            Event::DecisionRequested(card) => {
                self.pending_ask = Some((
                    card.req_id.clone(),
                    format!("{} · {}", card.kind, card.summary),
                ));
                // This head says `can_decide: false`, so the daemon put the card in front of one
                // that can answer it; saying where is the panel's whole job here.
                self.rows.push(Row::Note(format!(
                    "{} waiting in letibot: {}",
                    card.kind, card.summary
                )));
                self.state = PanelState::Waiting;
            }
            Event::DecisionAnswered { req_id } => {
                // Answered here or on another head: the card is gone, so the row stops naming
                // it. Only THIS request's answer clears it — a subagent's card settling is not
                // the ask this session is waiting on.
                if self
                    .pending_ask
                    .as_ref()
                    .is_some_and(|(id, _)| id == req_id)
                {
                    self.pending_ask = None;
                    if self.state == PanelState::Waiting {
                        self.state = if self.turn_started.is_some() {
                            PanelState::Thinking
                        } else {
                            PanelState::Idle
                        };
                    }
                }
            }
            // A citation is a mark on the text, which is where the operator is working: the panel's
            // conversation is the summary, the gutter is the answer.
            Event::Citations { item_id, citations } => self.citations(item_id, citations),
            Event::Unknown { .. } => {}
        }
        self.touch();
    }

    /// Write the remembered session for this folder, when the host named a file to keep it in.
    fn remember(&mut self) {
        let Some(file) = self.state_file.clone() else {
            return;
        };
        let id = self.session.clone();
        let _ = remember_in(&file, &self.folder, &id);
    }

    /// The composer's Enter: pick a session, start a daemon, or send what was typed.
    pub fn send(&mut self) {
        match self.state {
            PanelState::Picking => self.pick_now(),
            PanelState::Offline { startable: true } => self.start(),
            // Nothing to attach to and nothing typed: Enter starts the session this panel is for,
            // instead of sending an empty prompt into a panel with no session.
            _ if self.sessions.is_empty()
                && !self.attached()
                && self.input.trim().is_empty()
                // Not offline: a session can only be made where a daemon answers.
                && !matches!(self.state, PanelState::Offline { .. }) =>
            {
                self.new_session();
            }
            _ => {
                let text = self.input.trim().to_string();
                if text.is_empty() {
                    return;
                }
                self.input.clear();
                self.cursor = 0;
                let context = self.context();
                self.submit(text, context);
            }
        }
    }

    /// `alt-s`: the place as a chip on the next prompt. Nothing is sent: the words are still the
    /// operator's to write.
    pub fn attach_place(&mut self, send: &SendEvent, lang: &str) -> bool {
        let Some(chip) = Chip::of(send, lang) else {
            self.said = "an unnamed buffer has no place to attach".to_string();
            self.touch();
            return false;
        };
        self.said = format!("attached {}", chip.label(&self.folder));
        self.chips.push(chip);
        self.touch();
        true
    }

    /// A quick ask: the preset's words with the place beside them, sent at once, or — for `alt-k k`,
    /// which has no prompt — the place as a chip and the composer waiting for the words.
    pub fn ask(&mut self, ask: &AskEvent, lang: &str) -> bool {
        let chip = Chip::of(&ask.send, lang);
        if ask.prompt.is_empty() {
            if let Some(chip) = chip {
                self.chips.push(chip);
            }
            self.said = "type the question".to_string();
            self.touch();
            return true;
        }
        let mut context = EditorContext {
            origin: ORIGIN.to_string(),
            ..EditorContext::default()
        };
        context.refs = chip.iter().map(Chip::reference).collect();
        context.ask = Some(ContextAsk {
            preset: ask.preset.clone(),
            answer: AnswerMode::of(ask.answer),
        });
        self.submit(ask.prompt.clone(), context);
        true
    }

    /// Send a prompt now, or hold it until there is something to send it to. A chip and a preset are
    /// what the operator asked for; losing them because the panel was still attaching would be the
    /// panel's fault, not theirs.
    fn submit(&mut self, text: String, mut context: EditorContext) {
        // Read before the state moves: what is being asked is whether there is anything to ask YET.
        let sendable = self.attached()
            && matches!(
                self.state,
                PanelState::Idle | PanelState::Thinking | PanelState::Waiting
            );
        self.rows.push(Row::User(text.clone()));
        self.scroll = 0;
        self.follow = true;
        context.outcomes.append(&mut self.outcomes);
        let context = (!context.is_empty()).then_some(context);
        if sendable {
            self.state = PanelState::Thinking;
            self.acts.push(Act::Prompt { text, context });
        } else if matches!(self.state, PanelState::Offline { .. }) {
            // Nowhere to send it: said on the panel's own line rather than held in silence.
            self.said = "no daemon to send that to: nothing was sent".to_string();
        } else {
            // The state stays what the connection is doing — a panel still attaching must keep
            // connecting, or the `Hello` that would send this has nowhere to land.
            self.waiting.push(Pending { text, context });
        }
        self.touch();
    }

    /// The `Hello` landed: the prompts asked for while the panel was connecting go out now.
    fn flush(&mut self) {
        for pending in std::mem::take(&mut self.waiting) {
            self.acts.push(Act::Prompt {
                text: pending.text,
                context: pending.context,
            });
        }
    }

    /// What the next prompt carries: the chips as references, and the decisions taken since the last
    /// one. The host adds the editor's other open files at submit time, which is the one fact only
    /// it has.
    fn context(&mut self) -> EditorContext {
        let chips = std::mem::take(&mut self.chips);
        EditorContext {
            origin: ORIGIN.to_string(),
            refs: chips.iter().map(Chip::reference).collect(),
            outcomes: std::mem::take(&mut self.outcomes),
            ..EditorContext::default()
        }
    }

    /// What the operator decided about earlier marks, in the editor's own ids: named back to the
    /// range the model wrote, and sent on the next prompt.
    pub fn remember_outcomes(&mut self, taken: &[(String, Outcome)]) {
        for (id, outcome) in taken {
            let label = self.labels.get(id).cloned().unwrap_or_else(|| id.clone());
            let outcome = match outcome {
                Outcome::Accepted => "accepted",
                Outcome::Rejected => "rejected",
                Outcome::Reverted => "reverted",
            };
            self.outcomes.push(MarkOutcome {
                label,
                outcome: outcome.to_string(),
            });
        }
    }

    /// The marks a reply or a call set, for the host to land on the buffers it has open.
    pub fn take_marks(&mut self) -> Vec<Markset> {
        std::mem::take(&mut self.pending_marks)
    }

    /// Marks the host could not land, because the file is not open: kept for the pass that opens it.
    pub fn keep_marks(&mut self, kept: Vec<Markset>) {
        let mut kept = kept;
        kept.append(&mut self.pending_marks);
        self.pending_marks = kept;
    }

    /// A reply's citations, as marks: a bare range is a highlight, a `note` a remark with its
    /// severity from the body's first word, a `suggest` a hunk waiting to be taken.
    fn citations(&mut self, item_id: &str, citations: &[Citation]) {
        let highlight = format!("reply:{item_id}");
        let hunk = format!("reply:{item_id}:hunk");
        let mut sets: Vec<(PathBuf, String, Vec<Mark>)> = Vec::new();
        for (i, c) in citations.iter().enumerate() {
            let path = self.resolve(&c.path);
            let id = format!("{item_id}:{}", i + 1);
            let label = format!(
                "{}:{}-{}",
                shown(&path, &self.folder),
                c.start.max(1),
                c.end.max(c.start)
            );
            self.labels.insert(id.clone(), label);
            let (source, mark) = match c.kind {
                CitationKind::Cite => {
                    (&highlight, Mark::cite(&id, c.start, c.end, c.lines.clone()))
                }
                CitationKind::Note => {
                    let (severity, text) = note_body(&c.body);
                    (
                        &highlight,
                        Mark::note(&id, c.start, c.end, c.lines.clone(), severity, &text),
                    )
                }
                CitationKind::Suggest => (
                    &hunk,
                    Mark::suggest(&id, c.start, c.end, c.lines.clone(), &c.body),
                ),
            };
            match sets.iter_mut().find(|(p, s, _)| *p == path && s == source) {
                Some((_, _, marks)) => marks.push(mark),
                None => sets.push((path, source.clone(), vec![mark])),
            }
        }
        for (path, source, marks) in sets {
            // A highlight is the newest reply's: an older reply's on the same file goes. Hunks are
            // work and stay until they are decided, which is why they are their own source.
            let replaces: Vec<String> = if source == highlight {
                self.cited_by
                    .insert(path.clone(), source.clone())
                    .filter(|old| *old != source)
                    .into_iter()
                    .collect()
            } else {
                Vec::new()
            };
            self.pending_marks.push(Markset {
                path,
                source,
                replaces,
                marks,
            });
        }
    }

    /// A tool call's edits, as `edit` marks: the change is on disk, and `before` is what a revert puts
    /// back in the buffer.
    fn edits(&mut self, call_id: &str, edits: &[ToolEdit]) {
        if edits.is_empty() {
            return;
        }
        let source = format!("call:{call_id}");
        let mut sets: Vec<(PathBuf, Vec<Mark>)> = Vec::new();
        for (i, e) in edits.iter().enumerate() {
            let path = self.resolve(&e.path);
            let mark = Mark::edit(
                &format!("{call_id}:{}", i + 1),
                Change {
                    before: e.before.clone(),
                    after: e.after.clone(),
                    before_start: e.before_start,
                    after_start: e.after_start,
                },
            );
            let label = format!(
                "{}:{}-{}",
                shown(&path, &self.folder),
                mark.line,
                mark.end_line.max(mark.line)
            );
            self.labels.insert(mark.id.clone(), label);
            match sets.iter_mut().find(|(p, _)| *p == path) {
                Some((_, marks)) => marks.push(mark),
                None => sets.push((path, vec![mark])),
            }
        }
        for (path, marks) in sets {
            self.pending_marks.push(Markset {
                path,
                source: source.clone(),
                replaces: Vec::new(),
                marks,
            });
        }
    }

    /// Every call still running gets its word in place of the pending note: a turn that ends
    /// mid-call must not leave a row that reads like a call about to answer.
    fn settle_running(&mut self, word: &str) {
        // The entries stay: a `tool_finished` that arrives after the turn ended still owns its
        // row and replaces the note, rather than pushing a second row for one call.
        for (_, row, name, _, _) in &self.running {
            if let Some(Row::Note(note)) = self.rows.get_mut(*row) {
                *note = format!("{name} · {word}");
            }
        }
    }

    /// A path as the daemon named it: absolute, or relative to the workspace the conversation is in.
    fn resolve(&self, path: &str) -> PathBuf {
        let path = PathBuf::from(path);
        if path.is_absolute() {
            path
        } else {
            self.folder.join(path)
        }
    }

    /// The turn's one status line: `Responding` with its spinner and clock while the turn
    /// runs, or the ask being waited on, named. `None` when nothing is happening.
    pub fn turn_line(&self, w: usize) -> Option<Line> {
        if let Some((_, ask)) = &self.pending_ask {
            return Some(Line::new(vec![Span::styled(
                format!("waiting · {ask}"),
                crate::render::Style::of(Role::Pending),
            )]));
        }
        if self.state != PanelState::Thinking {
            return None;
        }
        let elapsed = self.turn_started.map(|t| t.elapsed().as_millis() as u64);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        // `slow` is the widget's word for GENERATING AND SILENT, and it only colours the row.
        // Age alone is not silence: a turn that has streamed text for a minute is not stuck.
        let slow = self.last_output.or(self.turn_started).is_some_and(|t| {
            t.elapsed().as_millis() as u64 > crate::agent::turn_status::SLOW_AFTER_MS
        });
        let line = crate::agent::turn_status::TurnStatus {
            busy: true,
            prefill: None,
            elapsed_ms: elapsed,
            now_ms: now,
            slow,
        }
        .line(w);
        (!line.is_empty()).then_some(line)
    }

    /// True once per whole second while a turn runs: the host redraws on it, so the spinner
    /// moves and the clock counts without a token arriving. The host's ordinary idle poll is
    /// what calls this — several times a second already, so no shorter wait is needed.
    pub fn tick_turn(&mut self) -> bool {
        if self.state != PanelState::Thinking {
            return false;
        }
        let secs = self
            .turn_started
            .map(|t| t.elapsed().as_secs())
            .unwrap_or(0);
        if secs != self.shown_second {
            self.shown_second = secs;
            return true;
        }
        false
    }

    /// The conversation at `width`, oldest first: what the widget windows and draws.
    pub fn build(&mut self, width: usize) {
        let key = (width, self.version);
        if self.built_for == Some(key) {
            return;
        }
        self.built_for = Some(key);
        let w = width.max(8);
        let mut out: Vec<Line> = Vec::new();
        if self.state == PanelState::Picking {
            self.build_pick(&mut out, w);
        } else {
            let fold = tool_fold(&self.rows, self.tools_open);

            let mut it = self.rows.iter_mut().peekable();
            let mut i = 0usize;
            while let Some(row) = it.next() {
                let next_row = it.peek().is_some_and(|r| matches!(r, Row::Tool(_)));
                let this_row = i;
                i += 1;
                match row {
                    Row::User(text) => {
                        let text = crate::agent::text::clean(text).into_owned();
                        out.extend(
                            crate::agent::blocks::UserBlock {
                                text,
                                stamp: String::new(),
                            }
                            .lines(w),
                        );
                    }
                    Row::Reply(reply) => {
                        let opts = RenderOptions::default();
                        out.extend(reply.view.lines(&reply.md, w, &opts));
                    }
                    Row::Note(note) => out.push(note_line(note)),
                    Row::Tool(t) => {
                        if !fold.hidden[this_row] {
                            out.extend(t.lines(w));
                        }
                        if !next_row {
                            if let Some(summary) = &fold.summary_at[this_row] {
                                out.push(note_line(summary));
                            }
                            out.push(Line::default());
                        }
                        continue;
                    }
                }
                out.push(Line::default());
            }
        }
        if !self.said.is_empty() {
            out.extend(crate::agent::text::wrapped(&self.said, w, Role::Faint));
            out.push(Line::default());
        }
        while out.last().is_some_and(Line::is_empty) {
            out.pop();
        }
        self.built = out;
    }

    /// The session list, and the `n` that makes a new one, as the body's rows.
    fn build_pick(&self, out: &mut Vec<Line>, w: usize) {
        out.push(Line::new(vec![Span::role(
            "the folder's sessions",
            Role::Faint,
        )]));
        if !self.pick_query.is_empty() {
            out.push(Line::new(vec![
                Span::role("› ", Role::UserAccent),
                Span::raw(self.pick_query.clone()),
            ]));
        }
        for (i, s) in self.picked_sessions() {
            let title = if s.title.is_empty() {
                "untitled"
            } else {
                s.title.as_str()
            };
            let picked = i == self.pick;
            let mark = if picked { "▸ " } else { "  " };
            let role = if picked { Role::Strong } else { Role::Plain };
            let age = aged(s.created_ms);
            let room = w.saturating_sub(mark.chars().count() + age.chars().count() + 1);
            out.push(Line::new(vec![
                Span::role(mark, Role::UserAccent),
                Span::role(crate::width::text::truncate(title, room), role),
                Span::role(format!(" {age}"), Role::Faint),
            ]));
        }
        out.push(Line::default());
        out.push(Line::new(vec![
            Span::role("enter ", Role::Strong),
            Span::role("attach the one above", Role::Faint),
        ]));
        out.push(Line::new(vec![
            Span::role("n ", Role::Strong),
            Span::role("a new session", Role::Faint),
        ]));
    }

    /// The built rows the widget draws, and the state the top edge names.
    /// The next build lays the conversation out again — the tool fold changed what is drawn
    /// without a row changing.
    pub fn rebuilt(&mut self) {
        self.built_for = None;
    }

    pub fn lines(&self) -> &[Line] {
        &self.built
    }

    pub(crate) fn insert(&mut self, c: char) {
        let at = self.cursor.min(self.input.chars().count());
        let byte = self
            .input
            .char_indices()
            .nth(at)
            .map(|(i, _)| i)
            .unwrap_or(self.input.len());
        self.input.insert(byte, c);
        self.cursor = at + 1;
        self.touch();
    }

    /// The character before the cursor, which is a char index and not a byte offset.
    pub(crate) fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        self.cursor -= 1;
        if let Some((byte, _)) = self.input.char_indices().nth(self.cursor) {
            self.input.remove(byte);
        }
        self.touch();
    }

    /// The character under the cursor.
    pub(crate) fn delete(&mut self) {
        if let Some((byte, _)) = self.input.char_indices().nth(self.cursor) {
            self.input.remove(byte);
        }
        self.touch();
    }
}

/// What the conversation's tool fold hides: `hidden[i]` for a Tool row folded away, and
/// `summary_at[i]` for the run's one line — `8 tool calls · 1 failed` — drawn after its open
/// rows, which are the newest call and any failure.
struct ToolFold {
    hidden: Vec<bool>,
    summary_at: Vec<Option<String>>,
}

/// A run of consecutive Tool rows: folded, its settled non-newest successes hide behind one
/// line and the newest and failed stay open; open, nothing is hidden and no line is added.
fn tool_fold(rows: &[Row], open: bool) -> ToolFold {
    let mut fold = ToolFold {
        hidden: vec![false; rows.len()],
        summary_at: vec![None; rows.len()],
    };
    if open {
        return fold;
    }
    let mut i = 0;
    while i < rows.len() {
        if !matches!(rows[i], Row::Tool(_)) {
            i += 1;
            continue;
        }
        let start = i;
        while i < rows.len() && matches!(rows[i], Row::Tool(_)) {
            i += 1;
        }
        let run = &rows[start..i];
        let mut failed = 0usize;
        let mut words: Vec<String> = Vec::new();
        let mut hidden = 0usize;
        for (k, r) in run.iter().enumerate() {
            let Row::Tool(t) = r else { continue };
            let failed_here = !matches!(t.outcome, crate::agent::outcome::Outcome::Ok)
                && !matches!(t.outcome, crate::agent::outcome::Outcome::Abstained(_));
            if failed_here {
                failed += 1;
                let w = t.outcome.word().to_lowercase();
                if !words.iter().any(|have| have == &w) {
                    words.push(w.clone());
                }
            }
            // The newest call is the run's last row whatever its outcome; a failure keeps
            // its own line wherever it sits.
            if t.newest || k + 1 == run.len() || failed_here {
                continue;
            }
            fold.hidden[start + k] = true;
            hidden += 1;
        }
        if hidden > 0 {
            let calls = if run.len() == 1 {
                "tool call"
            } else {
                "tool calls"
            };
            // The summary names the dominant non-success kind — a refusal is `refused`, not a
            // `failed` the row underneath contradicts; several kinds say `not ok` and leave
            // the word to the rows, which are one line away.
            let summary = match words.len() {
                0 => format!("{} {calls}", run.len()),
                1 => format!("{} {calls} · {failed} {}", run.len(), words[0]),
                _ => format!("{} {calls} · {failed} not ok", run.len()),
            };
            fold.summary_at[i - 1] = Some(summary);
        }
    }
    fold
}

/// The call's target: the first named string among its arguments — a command, a path, a
/// pattern — read from the raw call markup. Empty when the markup names none, which draws as
/// nothing rather than as a guess.
/// One call's markup out of the stream: serde reads the first JSON value and the rest is the
/// next call's. A message that streams two calls before either starts must not charge the
/// second's arguments to the first (nor concatenate them into unparseable junk).
fn split_call_markup(stream: String) -> (String, String) {
    use serde_json::Deserializer;
    let mut de = Deserializer::from_str(&stream).into_iter::<serde_json::Value>();
    match de.next() {
        Some(Ok(_)) => {
            let end = de.byte_offset();
            (stream[..end].to_string(), stream[end..].to_string())
        }
        _ => (stream, String::new()),
    }
}

fn call_target(markup: &str) -> String {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(markup.trim()) else {
        return String::new();
    };
    for key in [
        "command",
        "cmd",
        "path",
        "file_path",
        "file",
        "pattern",
        "query",
        "url",
    ] {
        if let Some(v) = v.get(key).and_then(|v| v.as_str()) {
            return v.to_string();
        }
    }
    String::new()
}

/// The wire's outcome word and reason as the typed outcome: one mapping, so the row's colour
/// and its word cannot disagree with the card the live call was drawn as.
fn settled_outcome(word: &str, why: &str) -> crate::agent::outcome::Outcome {
    use crate::agent::outcome::Outcome as Tool;
    match word {
        "ok" => Tool::Ok,
        "abstained" => Tool::Abstained(why.to_string()),
        "denied" | "refused" => Tool::Denied(why.to_string()),
        "timeout" | "timed out" => Tool::Timeout,
        "interrupted" => Tool::Interrupted,
        "backgrounded" => Tool::Backgrounded(why.to_string()),
        "not run" | "not_run" => Tool::NotRun(why.to_string()),
        other => Tool::Failed(if why.is_empty() {
            other.to_string()
        } else {
            why.to_string()
        }),
    }
}

/// One note: a faint bullet and its words, cleaned of anything a terminal would draw wrongly.
fn note_line(note: &str) -> Line {
    Line::new(vec![
        Span::role("· ", Role::Faint),
        Span::role(crate::agent::text::clean_line(note), Role::Faint),
    ])
}

/// A path as a person reads it: relative to the conversation's folder when it is inside it.
fn shown(path: &Path, folder: &Path) -> String {
    path.strip_prefix(folder)
        .unwrap_or(path)
        .display()
        .to_string()
}

/// A note's severity and its words: the model writes `ok:`, `warn:` or `wrong:` first, and the rest
/// is the sentence a person reads on the status row.
fn note_body(body: &str) -> (Severity, String) {
    let body = body.trim();
    for (prefix, severity) in [
        ("ok:", Severity::Ok),
        ("warn:", Severity::Warn),
        ("wrong:", Severity::Error),
    ] {
        if let Some(rest) = body.strip_prefix(prefix) {
            return (severity, rest.trim().to_string());
        }
    }
    (Severity::Info, body.to_string())
}

/// `3m`, `2h`, `4d`: how long ago a session was made, in a column's worth of characters.
fn aged(created_ms: u64) -> String {
    if created_ms == 0 {
        return String::new();
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(created_ms);
    let secs = now.saturating_sub(created_ms) / 1000;
    match secs {
        0..=59 => format!("{secs}s"),
        60..=3599 => format!("{}m", secs / 60),
        3600..=86_399 => format!("{}h", secs / 3600),
        _ => format!("{}d", secs / 86_400),
    }
}

impl crate::editor::Editor {
    /// `alt-i`: the side surface on or off, keeping the buffers and asking nothing.
    pub fn panel_toggle(&mut self) {
        if self.panel.open {
            self.panel.close();
            self.focus = crate::editor::Focus::Text;
        } else {
            self.panel_open();
        }
    }

    /// Show the panel and give it the keyboard, without losing a conversation it already has.
    pub fn panel_open(&mut self) {
        let folder = self.panel_folder();
        self.panel.open(folder);
        self.focus = crate::editor::Focus::Panel;
    }

    /// `ctrl-]`: the keyboard to the other surface, opening the panel when it is shut.
    pub fn panel_cross(&mut self) {
        // The surfaces that are open, in the order the cycle visits them. With nothing else open
        // the panel is what there is to cross to, so it opens.
        let mut order = vec![crate::editor::Focus::Text];
        if self.panel.open {
            order.push(crate::editor::Focus::Panel);
        }
        if self.bottom.open {
            order.push(crate::editor::Focus::Terminal);
        }
        if order.len() == 1 {
            self.panel_open();
            return;
        }
        if !order.contains(&self.focus) {
            self.focus = order[1];
            return;
        }
        let i = order.iter().position(|f| *f == self.focus).unwrap_or(0);
        self.focus = order[(i + 1) % order.len()];
    }

    /// `alt-m`: the focused surface takes the whole width, or gives it back.
    pub fn panel_zoom(&mut self) {
        self.panel.zoom = !self.panel.zoom;
    }

    /// There is nothing to open a panel on — no launcher at all. Say it on the status row, where the
    /// reader is already looking, and leave the panel shut: a surface with no way on is worse than a
    /// line saying why not.
    pub fn panel_refuse(&mut self, said: &str) {
        self.panel.close();
        self.focus = crate::editor::Focus::Text;
        self.panel.said = said.to_string();
        self.flash(said);
    }

    /// The folder the panel is on: the focused file's repository, else the folder rano was opened
    /// on.
    pub fn panel_folder(&self) -> PathBuf {
        let name = self.bs().buf.name.clone();
        let cwd = std::env::current_dir().unwrap_or_default();
        folder_for(name.as_deref(), &cwd)
    }

    /// The language of the focused buffer, as a fenced block names it: what the daemon labels the
    /// context with, and what a citation comes back as.
    pub(crate) fn panel_lang(&self) -> String {
        crate::syntax::language_of(&self.bs().buf)
            .map(|l| l.name().to_string())
            .unwrap_or_default()
    }

    /// A quick ask reached the host while the panel may be shut: show it, attach the place, and send.
    /// The keyboard stays in the text, which is where the operator is working.
    pub fn panel_ask(&mut self, ask: &crate::ask::AskEvent) -> bool {
        if !self.panel.open {
            let folder = self.panel_folder();
            // The answer lands in the panel; the keyboard stays where it is.
            self.panel.open(folder);
        }
        let lang = self.panel_lang();
        let sent = self.panel.ask(ask, &lang);
        if ask.prompt.is_empty() {
            // A free ask has no question to answer: the composer waits for the operator's words.
            self.focus = crate::editor::Focus::Panel;
        }
        sent
    }

    /// `alt-s` with no `send_command`: the place becomes a chip on the next prompt rather than a
    /// paste into it, so the words stay the operator's to write.
    pub fn panel_attach(&mut self, send: &crate::send::SendEvent) -> bool {
        if !self.panel.open {
            let folder = self.panel_folder();
            self.panel.open(folder);
        }
        let lang = self.panel_lang();
        self.panel.attach_place(send, &lang)
    }

    /// Watch the focused buffer for a move to another folder. Called from `tick`, and cheap: the
    /// repository is worked out when the buffer changes, never per frame.
    pub(crate) fn panel_watch_folder(&mut self) -> bool {
        if !self.panel.open {
            return false;
        }
        let name = self.bs().buf.name.clone();
        if name == self.panel.seen_name {
            return false;
        }
        self.panel.seen_name = name;
        let folder = self.panel_folder();
        self.panel.folder_moved(folder);
        true
    }

    // ---------- the panel's commands ----------

    pub fn panel_send(&mut self) {
        self.panel.send();
    }

    pub fn panel_new(&mut self) {
        self.panel.new_session();
    }

    pub fn panel_leave(&mut self) {
        self.focus = crate::editor::Focus::Text;
    }

    pub fn panel_interrupt(&mut self) {
        if self.panel.attached() {
            self.panel.acts.push(Act::Interrupt);
            self.panel.said = "stopping the turn".to_string();
            self.panel.touch();
        }
    }

    pub fn panel_follow(&mut self) {
        self.panel.follow();
    }

    pub fn panel_up(&mut self) {
        if self.panel.state == PanelState::Picking {
            self.panel.pick = self.panel.pick.saturating_sub(1);
            return;
        }
        self.panel.follow = false;
        self.panel.scroll = self.panel.scroll.saturating_sub(1);
    }

    pub fn panel_down(&mut self) {
        if self.panel.state == PanelState::Picking {
            let last = self.panel.sessions.len().saturating_sub(1);
            self.panel.pick = (self.panel.pick + 1).min(last);
            return;
        }
        self.panel.follow = false;
        self.panel.scroll = self.panel.scroll.saturating_add(1);
    }

    pub fn panel_page_up(&mut self) {
        self.panel.follow = false;
        self.panel.scroll = self.panel.scroll.saturating_sub(10);
    }

    pub fn panel_page_down(&mut self) {
        self.panel.follow = false;
        self.panel.scroll = self.panel.scroll.saturating_add(10);
    }

    /// One key for the panel while it holds the keyboard: its own map first, then the composer's
    /// readline. A key neither claims is the host's.
    pub fn panel_key(&mut self, key: &crate::term::KeyEvent) -> bool {
        use crate::term::KeyCode;
        let plain = !key.mods.ctrl() && !key.mods.alt();
        // While the pick is up, typing NARROWS it — an id prefix, a title, or a row number, the
        // way letibot's session picker works. A composer cannot have those keys: nothing is typed
        // into it while a pick is up, and a new session is `M-n`, the map's binding.
        if self.panel.state == PanelState::Picking && plain {
            match key.code {
                KeyCode::Char(c) => {
                    self.panel.pick_query.push(c);
                    self.panel.pick = self
                        .panel
                        .picked_sessions()
                        .first()
                        .map(|(i, _)| *i)
                        .unwrap_or(0);
                    return true;
                }
                KeyCode::Backspace => {
                    self.panel.pick_query.pop();
                    self.panel.pick = 0;
                    return true;
                }
                _ => {}
            }
        }
        let as_key = crate::keymap::Key::from_event(key);
        let found = crate::keymap::lookup(&[&self.keymaps.panel], &[as_key]);
        if let crate::keymap::Lookup::Command(name) = found {
            let name = name.to_string();
            self.run_command(&name);
            return true;
        }
        // The composer, readline's way: one line, a cursor in chars.
        let chars = self.panel.input.chars().count();
        match key.code {
            KeyCode::Char(c) if plain => {
                self.panel.insert(c);
                true
            }
            KeyCode::Backspace => {
                self.panel.backspace();
                true
            }
            KeyCode::Delete => {
                self.panel.delete();
                true
            }
            KeyCode::Left => {
                self.panel.cursor = self.panel.cursor.saturating_sub(1);
                true
            }
            KeyCode::Right => {
                self.panel.cursor = (self.panel.cursor + 1).min(chars);
                true
            }
            KeyCode::Home => {
                self.panel.cursor = 0;
                true
            }
            KeyCode::End => {
                self.panel.cursor = chars;
                true
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::buffer::Buffer;
    use crate::config::Config;
    use crate::editor::Editor;
    use crate::keys::KeyOutcome;
    use crate::letibot::{DecisionRequest, Hello};
    use crate::send::{Point, Selection};
    use crate::term::event::{KeyCode, KeyEvent, Mods};

    fn held(key: &KeyCode) -> KeyEvent {
        KeyEvent {
            code: *key,
            mods: Mods::NONE,
        }
    }

    fn hello(session: &str, title: &str) -> Frame {
        Frame::Hello(Hello {
            protocol_version: 70,
            session_id: session.to_string(),
            head_id: "h".to_string(),
            dropped: 0,
            resumed_from: None,
            seq: Some(0),
            sessions: vec![Session {
                id: session.to_string(),
                title: title.to_string(),
                created_ms: 0,
                live: true,
            }],
            snapshot: None,
        })
    }

    fn sessions(ids: &[&str]) -> Frame {
        Frame::Sessions {
            current: String::new(),
            created: None,
            sessions: ids
                .iter()
                .map(|id| Session {
                    id: id.to_string(),
                    title: format!("the {id}"),
                    created_ms: 0,
                    live: true,
                })
                .collect(),
        }
    }

    fn delta(text: &str) -> Frame {
        Frame::Event {
            seq: 1,
            event: Event::Delta {
                turn_id: "t".to_string(),
                target: DeltaTarget::Text,
                text: text.to_string(),
            },
        }
    }

    fn opened() -> Panel {
        let mut panel = Panel::default();
        panel.open(PathBuf::from("/w"));
        panel
    }

    /// Typing narrows the pick — an id prefix, a title substring, or a row number — and a unique
    /// match is the answer without moving a cursor. That is letibot's session picker, in the panel.
    #[test]
    fn typing_narrows_the_pick_and_a_unique_match_is_the_answer() {
        let mut panel = Panel {
            sessions: vec![
                Session {
                    id: "s-1".into(),
                    title: "the script".into(),
                    created_ms: 0,
                    live: true,
                },
                Session {
                    id: "s-2".into(),
                    title: "the other".into(),
                    created_ms: 0,
                    live: true,
                },
            ],
            state: PanelState::Picking,
            ..Panel::default()
        };
        for query in ["s-2", "other", "2"] {
            panel.pick_query = query.to_string();
            assert_eq!(panel.picked_sessions().len(), 1, "{query} narrows to one");
            panel.pick_now();
            assert_eq!(
                panel.take_acts(),
                vec![Act::Attach("s-2".to_string())],
                "{query} picked it"
            );
        }
    }

    /// A folder with no session of its own asks the one ABOVE it: the operator's session usually
    /// belongs to the project, not to the subdirectory the file happens to sit in.
    #[test]
    fn a_folder_with_no_session_asks_the_one_above_it() {
        let mut panel = Panel {
            folder: PathBuf::from("/home/dead/Projects/rano/rano"),
            state: PanelState::Connecting,
            ..Panel::default()
        };
        panel.apply(&Frame::Sessions {
            current: String::new(),
            created: None,
            sessions: Vec::new(),
        });
        assert_eq!(
            panel.take_acts(),
            vec![Act::Connect(PathBuf::from("/home/dead/Projects/rano"))]
        );
    }

    /// And with nothing anywhere above it, Enter is how a session comes to exist — not a prompt
    /// typed into a panel that has none.
    #[test]
    fn a_folder_with_no_session_anywhere_starts_one_on_enter() {
        let mut panel = Panel {
            folder: PathBuf::from("/"),
            state: PanelState::Idle,
            ..Panel::default()
        };
        panel.send();
        assert_eq!(panel.take_acts(), vec![Act::New]);
    }

    /// The panel asks the host for a connection and nothing else: it holds no socket, so every
    /// wire action is an `Act` the host translates.
    #[test]
    fn opening_asks_the_host_to_connect_and_the_session_is_attached_from_the_list() {
        let mut panel = opened();
        assert!(panel.open);
        assert_eq!(panel.state, PanelState::Connecting);
        assert_eq!(panel.take_acts(), vec![Act::Connect(PathBuf::from("/w"))]);

        panel.apply(&sessions(&["s-1"]));
        // One session is nothing to choose between: the panel takes it.
        assert_eq!(panel.take_acts(), vec![Act::Attach("s-1".to_string())]);
        panel.apply(&hello("s-1", "the one"));
        assert_eq!(panel.state, PanelState::Idle);
        assert_eq!(panel.title, "the one");
    }

    /// A streamed reply is one row, folded out of the deltas in the order they arrived; thinking
    /// A snapshot attach draws the conversation at once: the operator's prompt, the reply as
    /// markdown, a settled tool row with its REAL line count — not an empty pane until the
    /// next event. Reasoning stays folded, as it is live.
    #[test]
    fn attaching_replays_the_conversation() {
        let mut panel = opened();
        let frame = crate::letibot::parse(
            r#"{"frame":"hello","protocol_version":70,"session_id":"s-1","head_id":"h",
                "dropped":0,"resumed_from":null,
                "snapshot":{"session_id":"s-1","seq":41,"dropped":0,"items_dropped":0,
                  "items":[
                    {"item_id":"i-1","kind":"user","ledger_head":"l","ts":1,
                     "item":{"type":"user","speaker":"operator",
                             "parts":[{"kind":"text","text":"fix the parser"}]}},
                    {"item_id":"i-2","kind":"assistant","ledger_head":"l","ts":2,
                     "item":{"type":"assistant","text":"Start with **the tokenizer**."}},
                    {"item_id":"i-3","kind":"tool_result","ledger_head":"l","ts":3,
                     "item":{"type":"tool_result","call_id":"c-1","name":"bash",
                             "outcome":{"outcome":"ok"},"payload":"line one\nline two",
                             "edits":[]}},
                    {"item_id":"i-4","kind":"reasoning","ledger_head":"l","ts":4,
                     "item":{"type":"reasoning","text":"a draft nobody replays","field":"full"}},
                    {"item_id":"i-5","kind":"user","ledger_head":"l","ts":5,"item":null}
                  ],
                  "open_decisions":[],"turn":null}}"#,
        );
        panel.apply(&frame);
        panel.build(80);
        let text = panel
            .lines()
            .iter()
            .map(|l| l.plain())
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(
            panel.rows.len(),
            3,
            "prompt, reply, tool row — and an unreconciled row is not drawn empty"
        );
        assert!(text.contains("fix the parser"), "the prompt: {text}");
        assert!(text.contains("the tokenizer"), "the reply: {text}");
        assert!(!text.contains("a draft nobody replays"), "reasoning folds");
        let tools = panel
            .rows
            .iter()
            .filter(|r| matches!(r, Row::Tool(_)))
            .count();
        assert_eq!(tools, 1, "one settled tool row");
        let Some(Row::Tool(t)) = panel.rows.iter().find(|r| matches!(r, Row::Tool(_))) else {
            unreachable!()
        };
        assert_eq!(t.payload.len(), 2, "the replayed payload is really there");
        assert!(text.contains("2 lines"), "and its count is drawn: {text}");
        // A new turn after the replay opens a NEW reply rather than extending the old one.
        panel.apply(&Frame::Event {
            seq: 42,
            event: Event::TurnStarted {
                turn_id: "t".to_string(),
                model: "m".to_string(),
            },
        });
        panel.apply(&Frame::Event {
            seq: 43,
            event: Event::Delta {
                turn_id: "t".to_string(),
                target: DeltaTarget::Text,
                text: "the next answer".to_string(),
            },
        });
        let replies = panel
            .rows
            .iter()
            .filter(|r| matches!(r, Row::Reply(_)))
            .count();
        assert_eq!(replies, 2, "one reply per turn, the replayed one included");
    }

    /// The snapshot's open card is named, and a turn in flight says the honest clock: the
    /// snapshot has no start time, so "started before this head attached". A call still
    /// running is a note its late finish replaces, never a second row.
    #[test]
    fn the_snapshot_names_the_card_and_the_live_turn() {
        let mut waiting = opened();
        waiting.apply(&crate::letibot::parse(
            r#"{"frame":"hello","protocol_version":70,"session_id":"s-1","head_id":"h",
                "dropped":0,"resumed_from":null,
                "snapshot":{"session_id":"s-1","seq":9,"dropped":0,"items_dropped":0,
                  "items":[],
                  "open_decisions":[{"req_id":"r-1","kind":"permission",
                    "summary":"bash would run rm -rf build","options":[],"choices":[],
                    "because":"","deadline":null}],
                  "turn":null}}"#,
        ));
        let line = waiting.turn_line(60).expect("the card is named");
        assert!(
            line.plain().contains("waiting") && line.plain().contains("rm -rf build"),
            "{:?}",
            line.plain()
        );
        assert_eq!(waiting.state, PanelState::Waiting);

        let mut panel = opened();
        panel.apply(&crate::letibot::parse(
            r#"{"frame":"hello","protocol_version":70,"session_id":"s-1","head_id":"h",
                "dropped":0,"resumed_from":null,
                "snapshot":{"session_id":"s-1","seq":9,"dropped":0,"items_dropped":0,
                  "items":[],
                  "open_decisions":[],
                  "turn":{"turn_id":"t","model":"m","ledger_head":"l",
                    "text":"the half-written answer","reasoning":"","raw_calls":"",
                    "calls":[{"call_id":"c-9","name":"bash","args_digest":"d",
                              "target":"make all","started_ts":1000,
                              "state":{"state":"running"}}],
                    "appended":[],"progress":null,"tokens":0,
                    "state":{"state":"running"}}}}"#,
        ));
        assert_eq!(panel.state, PanelState::Thinking);
        let line = panel.turn_line(60).expect("a turn is running");
        assert!(
            line.plain().contains("started before this head attached"),
            "no clock is invented: {:?}",
            line.plain()
        );
        panel.build(80);
        let text = panel
            .lines()
            .iter()
            .map(|l| l.plain())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            text.contains("the half-written answer"),
            "the live text: {text}"
        );
        assert!(
            text.contains("bash …"),
            "the running call is a note: {text}"
        );
        // Its finish arrives after the attach: the note becomes the row, in place.
        panel.apply(&Frame::Event {
            seq: 10,
            event: Event::ToolFinished {
                turn_id: "t".to_string(),
                call_id: "c-9".to_string(),
                outcome: "ok".to_string(),
                why: String::new(),
                edits: Vec::new(),
                bytes: None,
            },
        });
        let tools = panel
            .rows
            .iter()
            .filter(|r| matches!(r, Row::Tool(_)))
            .count();
        assert_eq!(tools, 1, "the note became the row");
        assert!(
            !panel
                .rows
                .iter()
                .any(|r| matches!(r, Row::Note(n) if n.contains("bash …"))),
            "and no second row for the same call"
        );
    }

    /// A replayed user row says who spoke: a job's completion is a note, a peer's message names
    /// the peer — neither reads as the operator's own words.
    #[test]
    fn a_replayed_prompt_says_who_spoke() {
        let mut panel = opened();
        panel.apply(&crate::letibot::parse(
            r#"{"frame":"hello","protocol_version":70,"session_id":"s-1","head_id":"h",
                "dropped":0,"resumed_from":null,
                "snapshot":{"session_id":"s-1","seq":9,"dropped":0,"items_dropped":0,
                  "items":[
                    {"item_id":"i-1","kind":"user","ledger_head":"l","ts":1,
                     "item":{"type":"user","speaker":"agent",
                             "parts":[{"kind":"text","text":"job j1 finished"}]}},
                    {"item_id":"i-2","kind":"user","ledger_head":"l","ts":2,
                     "item":{"type":"user","speaker":{"peer":{"from":"cc@rano"}},
                             "parts":[{"kind":"text","text":"the gate is red"}]}}
                  ],
                  "open_decisions":[],"turn":null}}"#,
        ));
        assert_eq!(panel.rows.len(), 2);
        assert!(
            matches!(&panel.rows[0], Row::Note(n) if n == "job j1 finished"),
            "the session's own delivery is a note: {:?}",
            panel.rows[0]
        );
        assert!(
            matches!(&panel.rows[1], Row::User(t) if t == "cc@rano: the gate is red"),
            "the peer is named: {:?}",
            panel.rows[1]
        );
    }

    /// and a turn's end change the state and never the prose.
    #[test]
    fn a_streamed_reply_is_one_row_and_the_state_follows_the_turn() {
        let mut panel = opened();
        panel.apply(&hello("s-1", "the one"));
        panel.take_acts();
        panel.input = "why?".to_string();
        panel.send();
        assert_eq!(
            panel.take_acts(),
            vec![Act::Prompt {
                text: "why?".to_string(),
                context: None,
            }]
        );
        assert!(matches!(panel.rows.as_slice(), [Row::User(t)] if t == "why?"));
        assert_eq!(panel.state, PanelState::Thinking);
        assert!(panel.input.is_empty() && panel.cursor == 0);

        panel.apply(&Frame::Event {
            seq: 2,
            event: Event::TurnStarted {
                turn_id: "t".to_string(),
                model: "m".to_string(),
            },
        });
        for text in ["the ", "answer."] {
            panel.apply(&delta(text));
        }
        panel.apply(&Frame::Event {
            seq: 4,
            event: Event::Delta {
                turn_id: "t".to_string(),
                target: DeltaTarget::Reasoning,
                text: "hmm".to_string(),
            },
        });
        let rows = panel.rows.len();
        panel.apply(&Frame::Event {
            seq: 5,
            event: Event::TurnFinished {
                turn_id: "t".to_string(),
                ends_turn: true,
            },
        });
        assert_eq!(
            panel.rows.len(),
            rows,
            "no row for the reasoning or the end"
        );
        let Some(Row::Reply(reply)) = panel.rows.last() else {
            panic!("the reply is the last row: {:?}", panel.rows);
        };
        assert_eq!(reply.text, "the answer.");
        assert_eq!(panel.state, PanelState::Idle);
    }

    /// The header names the size the wire reports — never a count of zero about output the
    /// panel simply does not hold — and a long failure's reason is drawn whole, wrapped,
    /// because this surface has no unfold key to offer.
    #[test]
    fn a_settled_row_names_the_size_and_wraps_a_long_reason() {
        let mut panel = opened();
        panel.apply(&Frame::Event {
            seq: 1,
            event: Event::ToolStarted {
                turn_id: "t".to_string(),
                call_id: "c-1".to_string(),
                name: "bash".to_string(),
                access: "exec".to_string(),
            },
        });
        let long = "cannot resolve dependency `serde_derive` for the crate at /w: the lock                     file disagrees with the manifest, and the registry cache holds a version                     that no longer matches";
        panel.apply(&Frame::Event {
            seq: 2,
            event: Event::ToolFinished {
                turn_id: "t".to_string(),
                call_id: "c-1".to_string(),
                outcome: "failed".to_string(),
                why: long.to_string(),
                edits: Vec::new(),
                bytes: Some((300, 4096)),
            },
        });
        panel.build(80);
        let text = panel
            .lines()
            .iter()
            .map(|l| l.plain())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(!text.contains("0 lines"), "no invented line count: {text}");
        assert!(
            text.contains("4.0 KiB") && text.contains("spilled"),
            "the wire's size, said: {text}"
        );
        assert!(
            text.contains("no longer matches"),
            "the reason's tail is drawn, wrapped not cut: {text}"
        );
        assert!(
            !text.contains("/t unfolds"),
            "and no chord this surface lacks: {text}"
        );
    }

    /// Markup that streams for two calls is split per call, and markup left by a turn that
    /// ended never becomes the next call's target.
    #[test]
    fn markup_is_one_calls_own_and_never_leaks() {
        let mut panel = opened();
        // One message streams both calls' arguments before either starts.
        panel.apply(&Frame::Event {
            seq: 1,
            event: Event::Delta {
                turn_id: "t".to_string(),
                target: DeltaTarget::ToolCall,
                text: "{\"command\": \"echo one\"}{\"command\": \"echo two\"}".to_string(),
            },
        });
        for (seq, id) in [(2, "c-1"), (3, "c-2")] {
            panel.apply(&Frame::Event {
                seq,
                event: Event::ToolStarted {
                    turn_id: "t".to_string(),
                    call_id: id.to_string(),
                    name: "bash".to_string(),
                    access: "exec".to_string(),
                },
            });
            panel.apply(&Frame::Event {
                seq: seq + 10,
                event: Event::ToolFinished {
                    turn_id: "t".to_string(),
                    call_id: id.to_string(),
                    outcome: "ok".to_string(),
                    why: String::new(),
                    edits: Vec::new(),
                    bytes: Some((0, 0)),
                },
            });
        }
        let targets: Vec<&str> = panel
            .rows
            .iter()
            .filter_map(|r| match r {
                Row::Tool(t) => Some(t.target.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            targets,
            vec!["echo one", "echo two"],
            "each row names its own command"
        );
        // A turn ends with markup half-streamed: it is not the next turn's target.
        panel.apply(&Frame::Event {
            seq: 30,
            event: Event::Delta {
                turn_id: "t".to_string(),
                target: DeltaTarget::ToolCall,
                text: "{\"command\": \"never ran\"}".to_string(),
            },
        });
        panel.apply(&Frame::Event {
            seq: 31,
            event: Event::TurnFailed {
                turn_id: "t".to_string(),
                error: "gone".to_string(),
            },
        });
        panel.apply(&Frame::Event {
            seq: 32,
            event: Event::ToolStarted {
                turn_id: "u".to_string(),
                call_id: "c-3".to_string(),
                name: "bash".to_string(),
                access: "exec".to_string(),
            },
        });
        panel.apply(&Frame::Event {
            seq: 33,
            event: Event::ToolFinished {
                turn_id: "u".to_string(),
                call_id: "c-3".to_string(),
                outcome: "ok".to_string(),
                why: String::new(),
                edits: Vec::new(),
                bytes: Some((0, 0)),
            },
        });
        let last = panel
            .rows
            .iter()
            .rev()
            .find_map(|r| match r {
                Row::Tool(t) => Some(t.target.clone()),
                _ => None,
            })
            .expect("the last call's row");
        assert!(
            !last.contains("never ran"),
            "the dead turn's markup is not charged to this call: {last:?}"
        );
    }

    /// A `tool_finished` that arrives after the turn ended owns its row: the settled note is
    /// replaced, not doubled.
    #[test]
    fn a_late_finish_replaces_its_note() {
        let mut panel = opened();
        panel.apply(&Frame::Event {
            seq: 1,
            event: Event::ToolStarted {
                turn_id: "t".to_string(),
                call_id: "c-1".to_string(),
                name: "bash".to_string(),
                access: "exec".to_string(),
            },
        });
        panel.apply(&Frame::Event {
            seq: 2,
            event: Event::TurnInterrupted {
                turn_id: "t".to_string(),
                reason: String::new(),
            },
        });
        panel.apply(&Frame::Event {
            seq: 3,
            event: Event::ToolFinished {
                turn_id: "t".to_string(),
                call_id: "c-1".to_string(),
                outcome: "interrupted".to_string(),
                why: String::new(),
                edits: Vec::new(),
                bytes: Some((0, 0)),
            },
        });
        let tool_rows = panel
            .rows
            .iter()
            .filter(|r| matches!(r, Row::Tool(_)))
            .count();
        let notes = panel
            .rows
            .iter()
            .filter(|r| matches!(r, Row::Note(_)))
            .count();
        assert_eq!(tool_rows, 1, "one row for the call");
        assert_eq!(
            notes, 1,
            "the turn's own note, and no second row for the call"
        );
    }

    /// The turn's state is one line, above the composer: Responding with its clock while the
    /// turn runs, the ask named while one waits, gone when nothing is happening.
    #[test]
    fn the_turns_state_is_one_line_above_the_composer() {
        let mut panel = opened();
        assert!(panel.turn_line(60).is_none(), "idle says nothing");
        panel.apply(&Frame::Event {
            seq: 1,
            event: Event::TurnStarted {
                turn_id: "t".to_string(),
                model: "m".to_string(),
            },
        });
        let line = panel.turn_line(60).expect("a turn is running");
        assert!(line.plain().contains("Responding"), "{:?}", line.plain());
        assert!(
            line.plain().contains("0ms") || line.plain().contains("0.0s"),
            "the clock is on it: {:?}",
            line.plain()
        );
        // The row sits above the composer, one row apart, inside the panel.
        let view = crate::agent::panel::View {
            title: "s".to_string(),
            state: "thinking",
            state_role: Role::Faint,
            folder: "/w".to_string(),
            moved: None,
            body: &[],
            scroll: 0,
            follow: true,
            turn: Some(line.clone()),
            input: "typed words",
            cursor: 11,
            chips: &[],
            focus: false,
            hints: &[],
        };
        let area = crate::render::Rect::new(0, 0, 60, 12);
        let mut buf = crate::render::Buffer::empty(area);
        view.draw(area, &mut buf);
        let rows = buf.emit(crate::render::Palette::None);
        let at = |needle: &str| rows.iter().position(|r| r.contains(needle));
        let turn_row =
            at("Responding").unwrap_or_else(|| panic!("the turn row is drawn: {rows:?}"));
        let composer = at("typed words").expect("the composer is drawn");
        assert_eq!(
            composer,
            turn_row + 1,
            "the status line is directly above the composer: {rows:?}"
        );
        // A card waiting is named on the same line.
        panel.apply(&Frame::Event {
            seq: 2,
            event: Event::DecisionRequested(DecisionRequest {
                req_id: "r-1".to_string(),
                kind: "permission".to_string(),
                summary: "bash would run rm -rf build".to_string(),
                ..DecisionRequest::default()
            }),
        });
        let line = panel.turn_line(60).expect("the ask is shown");
        assert!(
            line.plain().contains("waiting") && line.plain().contains("rm -rf build"),
            "{:?}",
            line.plain()
        );
        // The turn's end does not take the ask away.
        panel.apply(&Frame::Event {
            seq: 3,
            event: Event::TurnFinished {
                turn_id: "t".to_string(),
                ends_turn: true,
            },
        });
        // The ask belongs to the session, not the turn: it stays named after the turn ends,
        // because it is still waiting on a person.
        assert!(
            panel
                .turn_line(60)
                .is_some_and(|l| l.plain().contains("waiting")),
            "the ask outlives the turn"
        );
        // A turn that ends with nothing pending says nothing at all.
        let mut idle = opened();
        idle.apply(&Frame::Event {
            seq: 1,
            event: Event::TurnStarted {
                turn_id: "t".to_string(),
                model: "m".to_string(),
            },
        });
        idle.apply(&Frame::Event {
            seq: 2,
            event: Event::TurnFinished {
                turn_id: "t".to_string(),
                ends_turn: true,
            },
        });
        assert!(idle.turn_line(60).is_none(), "an idle panel says nothing");
        // And the host's clock: one tick per whole second while the turn runs.
        let mut panel = opened();
        panel.apply(&Frame::Event {
            seq: 1,
            event: Event::TurnStarted {
                turn_id: "t".to_string(),
                model: "m".to_string(),
            },
        });
        assert!(!panel.tick_turn(), "no new second yet: no redraw");
        panel.turn_started =
            Some(std::time::Instant::now() - std::time::Duration::from_millis(1100));
        assert!(
            panel.tick_turn(),
            "a whole second elapsed: the status redraws even with no event"
        );
        assert!(!panel.tick_turn(), "and not again inside the same second");
    }

    /// The named ask is true or it is not drawn: another head's answer clears exactly that card,
    /// and a different conversation never inherits it.
    #[test]
    fn an_answered_or_abandoned_ask_is_no_longer_named() {
        let ask = |seq: u64| Frame::Event {
            seq,
            event: Event::DecisionRequested(DecisionRequest {
                req_id: "r-1".to_string(),
                kind: "permission".to_string(),
                summary: "bash would run rm -rf build".to_string(),
                ..DecisionRequest::default()
            }),
        };
        let answered = |seq: u64, req_id: &str| Frame::Event {
            seq,
            event: Event::DecisionAnswered {
                req_id: req_id.to_string(),
            },
        };
        let mut panel = opened();
        panel.apply(&Frame::Event {
            seq: 1,
            event: Event::TurnStarted {
                turn_id: "t".to_string(),
                model: "m".to_string(),
            },
        });
        panel.apply(&ask(2));
        assert_eq!(panel.state, PanelState::Waiting);
        // Another request's answer is not this card's: a subagent's card settling, say.
        panel.apply(&answered(3, "r-2"));
        assert!(
            panel
                .turn_line(60)
                .is_some_and(|l| l.plain().contains("waiting")),
            "a different card's answer clears nothing"
        );
        panel.apply(&answered(4, "r-1"));
        let line = panel.turn_line(60).expect("the turn still runs");
        assert!(
            !line.plain().contains("waiting"),
            "the answered ask is gone: {:?}",
            line.plain()
        );
        assert!(
            line.plain().contains("Responding"),
            "and the turn's own state is back: {:?}",
            line.plain()
        );
        assert_eq!(panel.state, PanelState::Thinking);
        // Following a moved folder is a different conversation: the old folder's card is not
        // this session's to name, and no event in the new stream would ever clear it.
        let mut panel = opened();
        panel.apply(&ask(1));
        panel.session = "s-1".to_string();
        panel.folder_moved(PathBuf::from("/elsewhere"));
        panel.follow();
        assert!(
            panel.turn_line(60).is_none(),
            "a different conversation has no such ask"
        );
        // Nor does a session picked in the same daemon.
        let mut panel = opened();
        panel.apply(&ask(1));
        panel.attach("s-2");
        assert!(
            panel.turn_line(60).is_none(),
            "a picked session starts with no ask of the last one's"
        );
    }

    /// `slow` only colours the row, and it means SILENT: a turn that has streamed for twenty
    /// seconds is not stuck, and one silent for twenty is.
    #[test]
    fn a_streaming_turn_is_not_slow_and_a_silent_one_is() {
        let mut panel = opened();
        panel.apply(&Frame::Event {
            seq: 1,
            event: Event::TurnStarted {
                turn_id: "t".to_string(),
                model: "m".to_string(),
            },
        });
        panel.turn_started = Some(std::time::Instant::now() - std::time::Duration::from_secs(20));
        // A delta arrives now, as the stream has been doing all along.
        panel.apply(&Frame::Event {
            seq: 2,
            event: Event::Delta {
                turn_id: "t".to_string(),
                target: crate::letibot::DeltaTarget::Text,
                text: "still going".to_string(),
            },
        });
        let line = panel.turn_line(60).expect("the turn runs");
        assert_eq!(
            line.spans[0].style.top(),
            Role::Success,
            "twenty seconds old and streaming: not slow"
        );
        // Silence instead: nothing emitted for the same twenty seconds.
        panel.last_output = Some(std::time::Instant::now() - std::time::Duration::from_secs(20));
        let line = panel.turn_line(60).expect("the turn runs");
        assert_eq!(
            line.spans[0].style.top(),
            Role::Pending,
            "twenty seconds silent: the row turns the slow colour"
        );
    }

    /// Scrolled up during a running turn, BOTH seams and the status each keep their own row:
    /// the count of newer lines is never the row the status draws over.
    #[test]
    fn the_seams_and_the_turn_row_do_not_share_a_row() {
        let body: Vec<Line> = (0..30).map(|i| Line::raw(format!("line {i}"))).collect();
        let view = crate::agent::panel::View {
            title: "s".to_string(),
            state: "thinking",
            state_role: Role::Faint,
            folder: "/w".to_string(),
            moved: None,
            body: &body,
            scroll: 10,
            follow: false,
            turn: Some(Line::raw("Responding")),
            input: "typed words",
            cursor: 11,
            chips: &[],
            focus: false,
            hints: &[],
        };
        let area = crate::render::Rect::new(0, 0, 60, 12);
        let mut buf = crate::render::Buffer::empty(area);
        view.draw(area, &mut buf);
        let rows = buf.emit(crate::render::Palette::None);
        let at = |needle: &str| rows.iter().position(|r| r.contains(needle));
        let above = at("above").expect("the count of older lines: {rows:?}");
        let below = at("below").unwrap_or_else(|| panic!("the count of newer lines: {rows:?}"));
        let turn = at("Responding").expect("the status: {rows:?}");
        let composer = at("typed words").expect("the composer: {rows:?}");
        assert!(above < below, "above before below: {rows:?}");
        assert_eq!(
            turn,
            below + 1,
            "the below seam sits above the status: {rows:?}"
        );
        assert_eq!(
            composer,
            turn + 1,
            "and the status above the composer: {rows:?}"
        );
    }

    /// A call that has not settled is a note, never a row claiming an outcome: the wire's
    /// ToolStarted has no outcome to draw, and a turn ending mid-call settles the note.
    #[test]
    fn a_running_call_is_a_note_until_it_settles() {
        let mut panel = opened();
        panel.apply(&Frame::Event {
            seq: 1,
            event: Event::ToolStarted {
                turn_id: "t".to_string(),
                call_id: "c-1".to_string(),
                name: "bash".to_string(),
                access: "exec".to_string(),
            },
        });
        assert!(
            !panel.rows.iter().any(|r| matches!(r, Row::Tool(_))),
            "no settled row while the call runs"
        );
        panel.build(80);
        let text = panel
            .lines()
            .iter()
            .map(|l| l.plain())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("bash …"), "the pending note: {text}");
        assert!(!text.contains("· ok"), "and no outcome claimed: {text}");
        // The turn ends before the call: the note says so, and nothing reads as pending.
        panel.apply(&Frame::Event {
            seq: 2,
            event: Event::TurnFailed {
                turn_id: "t".to_string(),
                error: "gone".to_string(),
            },
        });
        panel.build(80);
        let text = panel
            .lines()
            .iter()
            .map(|l| l.plain())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("bash · not run"), "{text}");
    }

    /// A tool call is a ToolRow: verb and target from the streamed call, its outcome and
    /// duration when it settles, its payload folded with a line count, and a failure's reason
    /// wrapped rather than cut off.
    #[test]
    fn a_tool_call_is_a_tool_row() {
        let mut panel = opened();
        panel.apply(&Frame::Event {
            seq: 1,
            event: Event::Delta {
                turn_id: "t".to_string(),
                target: DeltaTarget::ToolCall,
                text: "{\"command\": \"cat /nope\"}".to_string(),
            },
        });
        panel.apply(&Frame::Event {
            seq: 2,
            event: Event::ToolStarted {
                turn_id: "t".to_string(),
                call_id: "c-1".to_string(),
                name: "bash".to_string(),
                access: "exec".to_string(),
            },
        });
        panel.apply(&Frame::Event {
            seq: 3,
            event: Event::ToolFinished {
                turn_id: "t".to_string(),
                call_id: "c-1".to_string(),
                outcome: "failed".to_string(),
                why: "cat: /nope: no such file or directory".to_string(),
                edits: Vec::new(),
                bytes: None,
            },
        });
        // Settled rows only carry an outcome word: nothing claims `ok` while the call runs.
        assert!(
            !panel
                .rows
                .iter()
                .any(|r| matches!(r, Row::Tool(_))
                    && panel.running.iter().any(|(id, ..)| id == "c-1")),
            "a running call is not a settled row"
        );
        let tool = panel
            .rows
            .iter()
            .find_map(|r| match r {
                Row::Tool(t) => Some(t.as_ref()),
                _ => None,
            })
            .expect("a tool row");
        assert_eq!(tool.name, "bash");
        assert_eq!(tool.target, "cat /nope", "the command is the target");
        assert_eq!(
            tool.outcome.reason(),
            Some("cat: /nope: no such file or directory"),
            "the failure's reason is kept whole"
        );
        assert!(tool.elapsed_ms.is_some(), "a duration was taken");
        assert!(
            tool.payload.is_empty(),
            "the wire carries no output: the arguments are not the payload, so `· N lines` \
             cannot count them as the result: {:?}",
            tool.payload
        );
        // Rendered: the verb and target on the row, the reason wrapped, nothing cut.
        panel.build(60);
        let text = panel
            .lines()
            .iter()
            .map(|l| l.plain())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("Ran"), "the row carries the verb: {text}");
        assert!(
            text.contains("cat /nope"),
            "the target is on the row: {text}"
        );
        assert!(
            text.contains("no such file or directory"),
            "the reason is drawn: {text}"
        );
    }

    /// A run of settled calls folds to one line — `8 tool calls · 1 failed` — with the newest
    /// and the failed kept open; the key opens every call again.
    #[test]
    fn a_run_of_tool_calls_folds_to_one_line() {
        let mut panel = opened();
        let mut seq = 1;
        for i in 1..=8 {
            let id = format!("c-{i}");
            panel.apply(&Frame::Event {
                seq,
                event: Event::ToolStarted {
                    turn_id: "t".to_string(),
                    call_id: id.clone(),
                    name: "read".to_string(),
                    access: "read".to_string(),
                },
            });
            seq += 1;
            panel.apply(&Frame::Event {
                seq,
                event: Event::ToolFinished {
                    turn_id: "t".to_string(),
                    call_id: id,
                    outcome: if i == 4 { "failed" } else { "ok" }.to_string(),
                    why: if i == 4 {
                        "gone".to_string()
                    } else {
                        String::new()
                    },
                    edits: Vec::new(),
                    bytes: Some((0, 0)),
                },
            });
            seq += 1;
        }
        panel.build(80);
        let text = panel
            .lines()
            .iter()
            .map(|l| l.plain())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            text.contains("8 tool calls · 1 failed"),
            "one line names the run and its kind: {text}"
        );
        // The newest and the failed stay open: three rows of calls and the summary — not
        // eight calls and a summary.
        let call_rows = text.matches("Read").count();
        assert_eq!(
            call_rows, 2,
            "the newest and the failed stay open, the other six fold: {text}"
        );
        // The key opens every call.
        panel.tools_open = true;
        panel.rebuilt();
        panel.build(80);
        let text = panel
            .lines()
            .iter()
            .map(|l| l.plain())
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(
            text.matches("Read").count(),
            8,
            "the fold is open: every call drawn: {text}"
        );
        assert!(
            !text.contains("tool calls ·"),
            "and no summary line while open: {text}"
        );
    }

    /// The summary names the kind it counted: a run of refusals says `refused`, not `failed`,
    /// and a run of mixed kinds says `not ok` rather than choosing one word and lying.
    #[test]
    fn the_fold_summary_names_the_kind_it_counted() {
        let mut panel = opened();
        let mut seq = 1;
        for (i, word) in ["ok", "denied", "ok", "timeout"].iter().enumerate() {
            let id = format!("c-{i}");
            panel.apply(&Frame::Event {
                seq,
                event: Event::ToolStarted {
                    turn_id: "t".to_string(),
                    call_id: id.clone(),
                    name: "bash".to_string(),
                    access: "exec".to_string(),
                },
            });
            seq += 1;
            panel.apply(&Frame::Event {
                seq,
                event: Event::ToolFinished {
                    turn_id: "t".to_string(),
                    call_id: id,
                    outcome: word.to_string(),
                    why: String::new(),
                    edits: Vec::new(),
                    bytes: Some((0, 0)),
                },
            });
            seq += 1;
        }
        panel.build(80);
        let text = panel
            .lines()
            .iter()
            .map(|l| l.plain())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            text.contains("4 tool calls · 2 not ok"),
            "two kinds say `not ok`: {text}"
        );
        assert!(
            !text.contains("2 failed"),
            "a refusal is not a failure: {text}"
        );
        // One kind says its own word.
        let mut panel = opened();
        for (i, word) in ["ok", "denied", "denied", "ok"].iter().enumerate() {
            let id = format!("d-{i}");
            panel.apply(&Frame::Event {
                seq: i as u64 + 1,
                event: Event::ToolStarted {
                    turn_id: "t".to_string(),
                    call_id: id.clone(),
                    name: "bash".to_string(),
                    access: "exec".to_string(),
                },
            });
            panel.apply(&Frame::Event {
                seq: i as u64 + 11,
                event: Event::ToolFinished {
                    turn_id: "t".to_string(),
                    call_id: id,
                    outcome: word.to_string(),
                    why: String::new(),
                    edits: Vec::new(),
                    bytes: Some((0, 0)),
                },
            });
        }
        panel.build(80);
        let text = panel
            .lines()
            .iter()
            .map(|l| l.plain())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            text.contains("4 tool calls · 2 refused"),
            "a refusal says refused: {text}"
        );
    }

    /// Consecutive tool rows touch: a run of calls is one block of work, no blank row between
    /// neighbours, and a reply after them still breathes.
    #[test]
    fn tool_rows_have_no_blank_between_them() {
        let mut panel = opened();
        for i in 1..=3 {
            let id = format!("c-{i}");
            panel.apply(&Frame::Event {
                seq: i,
                event: Event::ToolStarted {
                    turn_id: "t".to_string(),
                    call_id: id.clone(),
                    name: "read".to_string(),
                    access: "read".to_string(),
                },
            });
            panel.apply(&Frame::Event {
                seq: i + 10,
                event: Event::ToolFinished {
                    turn_id: "t".to_string(),
                    call_id: id,
                    outcome: "ok".to_string(),
                    why: String::new(),
                    edits: Vec::new(),
                    bytes: None,
                },
            });
        }
        // The fold open: the test is about the rows touching, not about the fold.
        panel.tools_open = true;
        panel.build(60);
        let lines = panel.lines();
        // Every rendered row between the first tool's row and the last non-blank row is a
        // tool row: no empty line splits them.
        let plains: Vec<String> = lines.iter().map(|l| l.plain()).collect();
        let first = plains
            .iter()
            .position(|l| l.contains("Read"))
            .expect("a tool row");
        let last = plains
            .iter()
            .rposition(|l| l.contains("Read"))
            .expect("a tool row");
        let run = &plains[first..=last];
        assert_eq!(
            run.len(),
            3,
            "three calls, three rows, nothing between: {plains:?}"
        );
        for l in run {
            assert!(!l.trim().is_empty(), "a tool row, not a gap: {plains:?}");
        }
    }

    /// The three no-daemon states say three different things, and only one of them can be started
    /// from here.
    #[test]
    fn the_no_daemon_states_are_three_different_sentences() {
        let not_running = NoDaemon::NotRunning {
            folder: PathBuf::from("/w"),
            said: String::new(),
        };
        let no_verb = NoDaemon::NoVerb {
            said: "no such flag".to_string(),
        };

        let mut panel = opened();
        panel.take_acts();
        panel.no_daemon(&NoDaemon::NotInstalled);
        assert_eq!(panel.said, "no letibot on PATH");
        assert_eq!(panel.state, PanelState::Offline { startable: false });
        panel.send();
        assert!(panel.take_acts().is_empty(), "nothing can be started");

        panel.no_daemon(&not_running);
        assert!(
            panel.said.contains("no letibot daemon for /w"),
            "{}",
            panel.said
        );
        assert_eq!(panel.state, PanelState::Offline { startable: true });
        panel.send();
        assert_eq!(panel.take_acts(), vec![Act::Start(PathBuf::from("/w"))]);

        panel.no_daemon(&no_verb);
        assert!(panel.said.contains("no such flag"), "{}", panel.said);
        assert_eq!(panel.state, PanelState::Offline { startable: false });
        panel.send();
        assert!(
            panel.take_acts().is_empty(),
            "a launcher cannot be asked to start"
        );
        // Words typed into a panel with nowhere to send them are said to be unsent, not swallowed.
        panel.input = "are you there?".to_string();
        panel.send();
        assert!(panel.take_acts().is_empty(), "nothing went out");
        assert!(
            panel.said.contains("nothing was sent"),
            "the panel says so: {}",
            panel.said
        );
    }

    /// The remembered session is re-attached without asking; with none remembered, several
    /// sessions are a pick and nothing else.
    #[test]
    fn the_remembered_session_is_attached_and_the_others_are_a_pick() {
        let dir = std::env::temp_dir().join(format!("rano-panel-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("panel-sessions");
        let listed = sessions(&["s-2", "s-1"]);

        let mut panel = Panel {
            state_file: Some(file.clone()),
            ..Panel::default()
        };
        panel.open(PathBuf::from("/w"));
        panel.take_acts();
        panel.apply(&listed);
        assert_eq!(panel.state, PanelState::Picking, "two sessions: a pick");
        assert_eq!(panel.pick, 0, "the newest first");
        panel.send();
        assert_eq!(panel.take_acts(), vec![Act::Attach("s-2".to_string())]);
        panel.apply(&hello("s-2", "the s-2"));
        assert_eq!(
            remembered_in(&file, Path::new("/w")).as_deref(),
            Some("s-2"),
            "the pick is what the folder remembers"
        );

        let mut again = Panel {
            state_file: Some(file.clone()),
            ..Panel::default()
        };
        again.open(PathBuf::from("/w"));
        again.take_acts();
        again.apply(&listed);
        assert_eq!(again.take_acts(), vec![Act::Attach("s-2".to_string())]);
        assert_eq!(again.state, PanelState::Connecting, "attaching is a wait");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Zero sessions here, two above: the walk asks the parent, and the parent's two are a pick.
    #[test]
    fn no_sessions_here_asks_the_parent_and_the_parents_two_are_a_pick() {
        let mut panel = Panel::default();
        panel.open(PathBuf::from("/w/sub"));
        panel.take_acts();
        panel.apply(&sessions(&[]));
        assert_eq!(
            panel.take_acts(),
            vec![Act::Connect(PathBuf::from("/w"))],
            "the parent is asked, not a blank panel"
        );
        assert_eq!(panel.state, PanelState::Connecting);
        assert!(panel.said.contains("no session here"), "{}", panel.said);
        panel.apply(&sessions(&["s-2", "s-1"]));
        assert_eq!(
            panel.state,
            PanelState::Picking,
            "the parent's two are a pick"
        );
    }

    /// Zero sessions at the top: the walk stops and says so, and Enter starts one.
    #[test]
    fn no_sessions_anywhere_above_says_so_and_enter_starts_one() {
        let mut panel = Panel::default();
        panel.open(PathBuf::from("/"));
        panel.take_acts();
        panel.apply(&sessions(&[]));
        assert!(panel.take_acts().is_empty(), "nobody above to ask");
        assert_eq!(panel.state, PanelState::Idle);
        assert!(
            panel.said.contains("no session anywhere above this folder"),
            "{}",
            panel.said
        );
        panel.new_session();
        assert_eq!(panel.take_acts(), vec![Act::New], "Enter starts one");
    }

    /// Two sessions here: no walk at all, just the pick.
    #[test]
    fn two_sessions_here_never_asks_the_parent() {
        let mut panel = Panel::default();
        panel.open(PathBuf::from("/w/sub"));
        panel.take_acts();
        panel.apply(&sessions(&["s-2", "s-1"]));
        assert!(panel.take_acts().is_empty(), "no parent is asked");
        assert_eq!(panel.state, PanelState::Picking);
    }

    /// A `NewSession` is answered by the frame a list is answered with, `created` naming the one to
    /// enter: a session made must be moved into, not left behind.
    #[test]
    fn a_new_session_is_attached_when_the_frame_names_it() {
        let mut panel = opened();
        panel.take_acts();
        panel.new_session();
        assert_eq!(panel.take_acts(), vec![Act::New]);
        panel.apply(&Frame::Sessions {
            current: String::new(),
            created: Some("s-9".to_string()),
            sessions: Vec::new(),
        });
        assert_eq!(panel.take_acts(), vec![Act::Attach("s-9".to_string())]);
    }

    /// A folder that moved is a question, not an action: the stream keeps running until the
    /// operator says so, and following starts the conversation over in the new folder.
    #[test]
    fn a_moved_folder_is_followed_only_when_asked() {
        let mut panel = opened();
        panel.apply(&hello("s-1", "the one"));
        panel.take_acts();
        panel.folder_moved(PathBuf::from("/other"));
        assert_eq!(panel.moved, Some(PathBuf::from("/other")));
        assert_eq!(panel.folder, PathBuf::from("/w"), "still the old folder");
        assert!(
            !panel.rows.is_empty() || panel.rows.is_empty(),
            "the stream is untouched"
        );
        panel.follow();
        assert_eq!(panel.folder, PathBuf::from("/other"));
        assert_eq!(
            panel.take_acts(),
            vec![Act::Connect(PathBuf::from("/other"))]
        );
        assert!(
            panel.rows.is_empty(),
            "a different daemon is a different conversation"
        );
    }

    /// The panel's keyboard is its map first and the composer second: a letter is typed into the
    /// composer and never into the file, and Esc gives the text its keyboard back.
    #[test]
    fn esc_leaves_the_panel_and_the_letters_go_to_the_composer() {
        let mut ed = Editor::new(Buffer::new(), Config::default());
        ed.panel.open = true;
        ed.focus = crate::editor::Focus::Panel;
        ed.panel.state = PanelState::Idle;
        for c in "hi".chars() {
            assert_eq!(
                ed.handle_key(held(&KeyCode::Char(c))),
                KeyOutcome::Handled,
                "{c}"
            );
        }
        assert_eq!(ed.panel.input, "hi");
        assert_eq!(
            ed.bs().buf.row_count(),
            1,
            "nothing was typed into the file"
        );
        assert_eq!(
            ed.handle_key(held(&KeyCode::Esc)),
            KeyOutcome::Handled,
            "the panel's Esc is its own"
        );
        assert!(
            ed.focus != crate::editor::Focus::Panel,
            "Esc gives the keyboard back to the text"
        );
        assert!(ed.panel.open, "and leaves the panel showing");
    }

    /// `enter` in the composer sends, and the row it leaves is the person's own words.
    #[test]
    fn the_composers_enter_sends_what_was_typed() {
        let mut ed = Editor::new(Buffer::new(), Config::default());
        ed.panel.open = true;
        ed.focus = crate::editor::Focus::Panel;
        ed.panel.state = PanelState::Idle;
        ed.panel.session = "s-1".to_string();
        for c in "ping".chars() {
            ed.handle_key(held(&KeyCode::Char(c)));
        }
        assert_eq!(ed.handle_key(held(&KeyCode::Enter)), KeyOutcome::Handled);
        assert_eq!(
            ed.panel.take_acts(),
            vec![Act::Prompt {
                text: "ping".to_string(),
                context: None,
            }]
        );
        assert!(matches!(ed.panel.rows.as_slice(), [Row::User(t)] if t == "ping"));
        assert!(ed.panel.input.is_empty());
    }

    /// The composer's cursor is a character and not a byte: a letter outside ASCII is typed and
    /// deleted by the character, which a byte offset would cut in half.
    #[test]
    fn a_letter_outside_ascii_is_typed_and_deleted_by_the_character() {
        let mut ed = Editor::new(Buffer::new(), Config::default());
        ed.panel.open = true;
        ed.focus = crate::editor::Focus::Panel;
        ed.panel.state = PanelState::Idle;
        for c in "aéb".chars() {
            ed.handle_key(held(&KeyCode::Char(c)));
        }
        assert_eq!(ed.panel.input, "aéb");
        assert_eq!(ed.panel.cursor, 3);
        ed.handle_key(held(&KeyCode::Backspace));
        assert_eq!((ed.panel.input.as_str(), ed.panel.cursor), ("aé", 2));
        ed.handle_key(held(&KeyCode::Left));
        ed.handle_key(held(&KeyCode::Backspace));
        assert_eq!((ed.panel.input.as_str(), ed.panel.cursor), ("é", 0));
        ed.handle_key(held(&KeyCode::Delete));
        assert!(ed.panel.input.is_empty());
    }

    /// No launcher at all: the line belongs on the status row and the panel stays shut, because
    /// there is nothing behind it that could open.
    #[test]
    fn a_refused_panel_is_not_shown_at_all() {
        let mut ed = Editor::new(Buffer::new(), Config::default());
        ed.panel.open = true;
        ed.focus = crate::editor::Focus::Panel;
        ed.panel_refuse("no letibot on PATH");
        assert!(!ed.panel.open && ed.focus != crate::editor::Focus::Panel);
        assert_eq!(ed.status_text().as_deref(), Some("no letibot on PATH"));
    }

    /// An event this build does not know is skipped, and the stream carries on.
    #[test]
    fn an_unknown_event_does_not_end_anything() {
        let mut panel = opened();
        panel.apply(&hello("s-1", "the one"));
        let changed = panel.apply(&Frame::Event {
            seq: 4,
            event: Event::Unknown {
                tag: "explain".to_string(),
            },
        });
        assert!(!changed, "an unknown event changes no row");
        assert_eq!(panel.state, PanelState::Idle);
        panel.apply(&delta("still here"));
        let Some(Row::Reply(reply)) = panel.rows.last() else {
            panic!("a reply");
        };
        assert_eq!(reply.text, "still here");
    }

    /// A card is a row and a state: this head cannot answer one, so it says where it can be.
    #[test]
    fn a_card_says_where_it_can_be_answered() {
        let mut panel = opened();
        panel.apply(&Frame::Event {
            seq: 1,
            event: Event::DecisionRequested(DecisionRequest {
                req_id: "r-1".to_string(),
                kind: "permission".to_string(),
                summary: "bash would run rm -rf build".to_string(),
                ..DecisionRequest::default()
            }),
        });
        assert_eq!(panel.state, PanelState::Waiting);
        let Some(Row::Note(note)) = panel.rows.last() else {
            panic!("a note");
        };
        assert!(note.contains("waiting in letibot"), "{note}");
    }

    /// The folder is the focused file's own directory, and a directory that is in no repository is
    /// its own folder.
    #[test]
    fn the_folder_is_the_files_directory_when_there_is_no_repository() {
        let dir = std::env::temp_dir().join(format!("rano-panel-git-{}", std::process::id()));
        let sub = dir.join("a/b");
        std::fs::create_dir_all(&sub).unwrap();
        assert_eq!(folder_for(None, &sub), sub);
        assert_eq!(folder_for(Some(&sub.join("f.rs")), &dir), sub);
        // A file named with no directory is the directory rano was opened on.
        assert_eq!(folder_for(Some(Path::new("f.rs")), &sub), sub);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The conversation is built at the width it is drawn at, and a reply is rendered, not read
    /// back as its own markdown source.
    #[test]
    fn the_built_rows_are_the_conversation_at_that_width() {
        let mut panel = opened();
        panel.input = "the question".to_string();
        panel.send();
        panel.apply(&delta("the **answer**"));
        panel.build(40);
        let text: Vec<String> = panel.lines().iter().map(Line::plain).collect();
        assert!(text.iter().any(|l| l.contains("the question")), "{text:?}");
        assert!(text.iter().any(|l| l == "the answer"), "{text:?}");
        assert!(
            !text.iter().any(|l| l.contains("**")),
            "markdown is rendered: {text:?}"
        );
    }

    /// A place as the editor hands it over: a file, a cursor and the marked lines' text.
    fn place() -> SendEvent {
        SendEvent {
            path: Some(PathBuf::from("/w/notes.txt")),
            cursor: Point { line: 3, column: 1 },
            selection: Some(Selection {
                start: Point { line: 3, column: 1 },
                end: Point { line: 4, column: 9 },
                text: "one\ntwo".to_string(),
            }),
            modified: true,
            ask: None,
        }
    }

    /// `M-k f`: the preset's words, with the selection beside them and the answer mode the daemon
    /// needs to know which fence to ask for.
    #[test]
    fn an_ask_sends_the_preset_with_the_selection_beside_it() {
        let mut panel = opened();
        panel.apply(&hello("s-1", "the one"));
        panel.take_acts();
        let ask = AskEvent {
            send: place(),
            preset: "fact-check".to_string(),
            answer: crate::ask::Answer::Notes,
            prompt: "Check each claim.".to_string(),
        };
        assert!(panel.ask(&ask, "markdown"));
        let acts = panel.take_acts();
        let [Act::Prompt { text, context }] = acts.as_slice() else {
            panic!("one prompt: {acts:?}");
        };
        assert_eq!(text, "Check each claim.");
        let context = context.as_ref().expect("a context");
        assert_eq!(context.origin, "rano");
        assert_eq!(
            context.ask,
            Some(ContextAsk {
                preset: "fact-check".to_string(),
                answer: AnswerMode::Notes,
            })
        );
        assert_eq!(context.refs.len(), 1);
        assert_eq!(context.refs[0].file, "/w/notes.txt");
        assert_eq!(context.refs[0].text, "one\ntwo");
        assert!(context.refs[0].unsaved, "the buffer differs from disk");
        assert_eq!(context.refs[0].lang, "markdown");
        let range = context.refs[0].range.expect("the lines the ask is about");
        assert_eq!((range.start.line, range.end.line), (3, 4));
        assert_eq!(panel.state, PanelState::Thinking);
        // The operator's own row is the preset's words, so the conversation reads as the question.
        assert!(matches!(panel.rows.as_slice(), [Row::User(t)] if t == "Check each claim."));
    }

    /// An ask pressed before the panel was attached waits for the `Hello` instead of being dropped:
    /// the answer is the point of asking, and the connection is not the operator's to wait for.
    #[test]
    fn an_ask_before_the_attach_goes_out_when_the_hello_lands() {
        let mut panel = opened();
        panel.take_acts();
        let ask = AskEvent {
            send: place(),
            preset: "explain".to_string(),
            answer: crate::ask::Answer::Chat,
            prompt: "Explain it.".to_string(),
        };
        panel.ask(&ask, "markdown");
        assert!(
            panel.take_acts().is_empty(),
            "nothing to send it to yet, so nothing is sent"
        );
        // The ask must not move the state off Connecting: the session list and the hello are what the
        // held prompt is waiting for, and a panel that said `thinking` would never see them.
        assert_eq!(panel.state, PanelState::Connecting);
        panel.apply(&sessions(&["s-1"]));
        assert_eq!(panel.take_acts(), vec![Act::Attach("s-1".to_string())]);
        panel.apply(&hello("s-1", "the one"));
        let acts = panel.take_acts();
        let [Act::Prompt { text, .. }] = acts.as_slice() else {
            panic!("the held prompt goes out: {acts:?}");
        };
        assert_eq!(text, "Explain it.");
    }

    /// `M-s` with no `send_command`: the place is a chip on the next prompt, and the prompt after it
    /// has none — a place is attached once.
    #[test]
    fn a_chip_rides_one_prompt_and_is_then_gone() {
        let mut panel = opened();
        panel.apply(&hello("s-1", "the one"));
        panel.take_acts();
        assert!(panel.attach_place(&place(), "markdown"));
        assert_eq!(panel.chips.len(), 1);
        assert!(panel.said.contains("notes.txt:3-4"), "{}", panel.said);

        panel.input = "what about this?".to_string();
        panel.send();
        let acts = panel.take_acts();
        let [Act::Prompt { text, context }] = acts.as_slice() else {
            panic!("one prompt: {acts:?}");
        };
        assert_eq!(text, "what about this?");
        let context = context.as_ref().expect("the chip rode along");
        assert_eq!(context.refs.len(), 1);
        assert!(context.ask.is_none(), "a chip is not an ask");
        assert!(panel.chips.is_empty(), "the chip was spent");

        panel.input = "and this?".to_string();
        panel.send();
        let acts = panel.take_acts();
        let [Act::Prompt { context, .. }] = acts.as_slice() else {
            panic!("one prompt: {acts:?}");
        };
        assert!(context.is_none(), "nothing left to carry");
    }

    /// `M-k k`: the place is a chip and the composer waits for the words, because there is no prompt
    /// to send.
    #[test]
    fn the_free_ask_attaches_the_place_and_takes_the_keyboard() {
        let mut panel = opened();
        let ask = AskEvent {
            send: place(),
            preset: "ask".to_string(),
            answer: crate::ask::Answer::Chat,
            prompt: String::new(),
        };
        assert!(panel.ask(&ask, "markdown"));
        assert!(
            panel
                .take_acts()
                .iter()
                .all(|a| !matches!(a, Act::Prompt { .. }))
        );
        assert_eq!(panel.chips.len(), 1);
        // The keyboard is the host's one focus now, not a flag on the panel.
        assert_eq!(panel.said, "type the question");
    }

    fn citation(kind: CitationKind, start: usize, end: usize, body: &str) -> Event {
        Event::Citations {
            item_id: "it-1".to_string(),
            citations: vec![Citation {
                label: "notes.txt:3-4".to_string(),
                path: "notes.txt".to_string(),
                start,
                end,
                lines: vec!["one".to_string(), "two".to_string()],
                error: None,
                kind,
                body: body.to_string(),
            }],
        }
    }

    /// A `note` citation is a mark on those lines, with the severity the body's first word names and
    /// the sentence the status row shows at it.
    #[test]
    fn a_note_citation_becomes_a_mark_with_its_severity() {
        let mut panel = opened();
        panel.apply(&Frame::Event {
            seq: 1,
            event: citation(CitationKind::Note, 3, 4, "warn: no source for this"),
        });
        let sets = panel.take_marks();
        assert_eq!(sets.len(), 1);
        let set = &sets[0];
        assert_eq!(set.path, PathBuf::from("/w/notes.txt"));
        assert_eq!(set.source, "reply:it-1");
        assert_eq!(set.marks.len(), 1);
        let mark = &set.marks[0];
        assert_eq!(mark.kind, crate::marks::MarkKind::Note);
        assert_eq!(mark.severity, Severity::Warn);
        assert_eq!((mark.line, mark.end_line), (3, 4));
        assert_eq!(
            mark.text, "no source for this",
            "the prefix is the severity"
        );
        assert_eq!(mark.anchor, vec!["one".to_string(), "two".to_string()]);
    }

    /// A `suggest` is a hunk, not a remark: it is decided in the editor, and its decision comes back
    /// to the model named by the range it wrote rather than by a mark id.
    #[test]
    fn a_suggestion_is_a_hunk_and_its_decision_is_named_by_the_range() {
        let mut panel = opened();
        panel.apply(&hello("s-1", "the one"));
        panel.take_acts();
        panel.apply(&Frame::Event {
            seq: 1,
            event: citation(CitationKind::Suggest, 3, 4, "the replacement"),
        });
        let sets = panel.take_marks();
        let mark = &sets[0].marks[0];
        assert_eq!(mark.kind, crate::marks::MarkKind::Suggest);
        assert_eq!(mark.after().as_deref(), Some("the replacement"));

        panel.remember_outcomes(&[(mark.id.clone(), Outcome::Accepted)]);
        panel.input = "and now?".to_string();
        panel.send();
        let acts = panel.take_acts();
        let [Act::Prompt { context, .. }] = acts.as_slice() else {
            panic!("one prompt: {acts:?}");
        };
        let context = context.as_ref().expect("the outcome rides along");
        assert_eq!(
            context.outcomes,
            vec![MarkOutcome {
                label: "notes.txt:3-4".to_string(),
                outcome: "accepted".to_string(),
            }]
        );
        assert!(panel.outcomes.is_empty(), "said once, not twice");
    }

    /// A newer reply's highlights replace an older reply's on the same file; its hunks are work and
    /// stay, which is why they are a source of their own.
    #[test]
    fn a_newer_reply_replaces_the_highlights_and_keeps_the_hunks() {
        let mut panel = opened();
        panel.apply(&Frame::Event {
            seq: 1,
            event: citation(CitationKind::Note, 3, 4, "ok: fine"),
        });
        assert!(panel.take_marks()[0].replaces.is_empty(), "the first one");
        panel.apply(&Frame::Event {
            seq: 2,
            event: Event::Citations {
                item_id: "it-2".to_string(),
                citations: vec![
                    Citation {
                        label: "notes.txt:3-4".to_string(),
                        path: "notes.txt".to_string(),
                        start: 3,
                        end: 4,
                        lines: vec![],
                        error: None,
                        kind: CitationKind::Suggest,
                        body: "better".to_string(),
                    },
                    Citation {
                        label: "notes.txt:3-4".to_string(),
                        path: "notes.txt".to_string(),
                        start: 3,
                        end: 4,
                        lines: vec![],
                        error: None,
                        kind: CitationKind::Cite,
                        body: String::new(),
                    },
                ],
            },
        });
        let sets = panel.take_marks();
        let hunk = sets
            .iter()
            .find(|s| s.source == "reply:it-2:hunk")
            .expect("the suggestion is its own set");
        assert!(hunk.replaces.is_empty(), "a hunk replaces nothing");
        let highlight = sets
            .iter()
            .find(|s| s.source == "reply:it-2")
            .expect("the citation is a set of its own");
        assert_eq!(
            highlight.replaces,
            vec!["reply:it-1".to_string()],
            "the older reply's highlights go"
        );
    }

    /// An edit a call made is a hunk already on disk: `before` is what a revert puts back.
    #[test]
    fn a_call_s_edits_are_marks_named_by_the_call() {
        let mut panel = opened();
        panel.apply(&Frame::Event {
            seq: 1,
            event: Event::ToolFinished {
                turn_id: "t".to_string(),
                call_id: "c-9".to_string(),
                outcome: "ok".to_string(),
                why: String::new(),
                bytes: None,
                edits: vec![ToolEdit {
                    path: "notes.txt".to_string(),
                    before_start: 3,
                    after_start: 3,
                    before: "one".to_string(),
                    after: "a line".to_string(),
                }],
            },
        });
        let sets = panel.take_marks();
        assert_eq!(sets.len(), 1);
        assert_eq!(sets[0].source, "call:c-9");
        assert_eq!(sets[0].path, PathBuf::from("/w/notes.txt"));
        let mark = &sets[0].marks[0];
        assert_eq!(mark.kind, crate::marks::MarkKind::Edit);
        assert_eq!(mark.before().as_deref(), Some("one"));
        assert_eq!((mark.line, mark.end_line), (3, 3));
    }
}
