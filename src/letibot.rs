//! rano's own letibot client: the socket, the NDJSON frames, and the reply folded out of them.
//! Not letibot's crate, deliberately: linking `letibot-sessionlog` would pin the two repositories
//! to each other, and its typed enums fail on a variant this build never heard of — a fault in the
//! middle of a turn. So a line is read as JSON, the panel's subset is given a type here (field for
//! field from letibot's `crates/sessionlog/src/{protocol.rs,event.rs}`), and an unknown frame or
//! event tag is counted and skipped while the stream carries on (`docs/rano-integration.md` §3.2).

use std::io::{BufRead, BufReader, Write};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::thread::JoinHandle;
use std::time::Duration;

use serde::Serialize;
use serde_json::Value;

/// The protocol this build was written against. rano sends it and reads the daemon's from `Hello`:
/// the daemon logs a difference and serves anyway, so this is a fact to state, not a gate to pass.
///
/// 71 added `Prompt.context`, 72 `Citation.kind` and `body`; both are defaulted on the wire, and
/// this build reads and writes both, so it speaks the newer one.
pub const PROTOCOL_VERSION: u32 = 72;

/// The head kind a panel attaches as.
pub const KIND_EDITOR: &str = "editor";

/// The feature string that says this head is an editor panel. Free-form and forward compatible on
/// letibot's side, so a daemon that has never heard of it ignores it.
pub const FEATURE_EDITOR_V1: &str = "editor_v1";

/// The feature string for the queue lane: this head may send `"delivery":"queue"` on a prompt.
/// A daemon that did not echo it in `Features.accepted` steers every prompt, so the head must not
/// promise a queue there — `Client::prompt_queued` refuses by name instead.
pub const FEATURE_QUEUE_PROMPT: &str = "queue_prompt_v1";

/// Which lane a prompt takes. `Steer` is what every prompt took before the lane existed: the
/// running turn reads it at its next step. `Queue` waits for the turn to end and opens its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Lane {
    #[default]
    Steer,
    Queue,
}

impl Lane {
    /// The wire word, where the lane needs one at all: a steer is the frame without the field,
    /// so a daemon that never heard of `delivery` reads exactly what it always did.
    fn word(self) -> Option<&'static str> {
        match self {
            Lane::Steer => None,
            Lane::Queue => Some("queue"),
        }
    }
}

/// The variable naming a socket, taken as authoritative when set — the launcher's own rule
/// (`bridge::daemon::resolve`), and the one way two heads can be pointed at one daemon.
pub const SOCKET_VAR: &str = "LETIBOT_SOCKET";

/// The launcher, whose `--where --json` is how rano asks which daemon a folder belongs to.
const LAUNCHER: &str = "letibot";

/// Who this connection says it is: what `Attach` carries, and what makes a card reach this head.
/// A head that cannot answer must say so — `can_decide: false` — or a question routed to it waits
/// out its deadline and times out: an answer given for a fake reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Seat {
    pub kind: String,
    pub identity: String,
    pub can_decide: bool,
    /// How many events the daemon may buffer for this head before demoting it to a resync.
    pub queue: usize,
    pub features: Vec<String>,
}

impl Default for Seat {
    fn default() -> Seat {
        Seat {
            kind: KIND_EDITOR.to_string(),
            identity: "rano".to_string(),
            can_decide: true,
            // letibot's own `Caps::default`: a slow head may ask for more, the daemon clamps.
            queue: 1024,
            features: vec![
                FEATURE_EDITOR_V1.to_string(),
                FEATURE_QUEUE_PROMPT.to_string(),
            ],
        }
    }
}

/// One file beside a prompt, as the composer's chip: a path the daemon reads, never the bytes —
/// both processes are on one box, and what the head stat'ed is not what the daemon must read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Attachment {
    pub kind: AttachmentKind,
    pub path: String,
    #[serde(default)]
    pub media_type: String,
    #[serde(default)]
    pub bytes_len: u64,
    #[serde(default)]
    pub name: String,
}

/// Image or file: which one decides whether the model is handed it or told its path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AttachmentKind {
    Image,
    File,
}

/// A question's answer: a choice, a note on that choice, typed words, or a deliberate abstention.
/// An abstention is not permission to proceed, so it is a field rather than an empty answer.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Answer {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub option: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub free: Option<String>,
    #[serde(skip_serializing_if = "is_false")]
    pub abstain: bool,
}

/// Why `abstain` is elided when false, spelled out: serde's attribute should not need the reader to
/// work out which `Not` impl was meant.
fn is_false(b: &bool) -> bool {
    !*b
}

/// Head → daemon, the subset the panel sends. Byte-identical to letibot's `ClientFrame`: the tag
/// is `frame`, and a variant whose payload is a struct has that struct's fields inline.
#[derive(Debug, Serialize)]
#[serde(tag = "frame", rename_all = "snake_case")]
enum Out<'a> {
    Attach {
        protocol_version: u32,
        session_id: &'a str,
        since_seq: u64,
        kind: &'a str,
        identity: &'a str,
        caps: Caps,
    },
    Ack {
        seq: u64,
        rendered: u64,
        filtered: u64,
    },
    Resync,
    Detach,
    Prompt {
        client_request_id: &'a str,
        expected_seq: u64,
        text: &'a str,
        attachments: &'a [Attachment],
        /// Absent rather than empty, so a daemon written before 71 reads exactly what it always did.
        #[serde(skip_serializing_if = "Option::is_none")]
        context: Option<&'a EditorContext>,
        /// `"queue"` on the queue lane; absent for a steer, which is every prompt before the lane.
        #[serde(skip_serializing_if = "Option::is_none")]
        delivery: Option<&'static str>,
    },
    Interrupt {
        client_request_id: &'a str,
        expected_seq: u64,
        reason: &'a str,
    },
    Answer {
        client_request_id: &'a str,
        req_id: &'a str,
        option_id: &'a str,
        #[serde(skip_serializing_if = "Option::is_none")]
        pattern: Option<&'a str>,
        #[serde(skip_serializing_if = "Option::is_none")]
        note: Option<&'a str>,
    },
    AnswerQuestion {
        client_request_id: &'a str,
        req_id: &'a str,
        answer: Answer,
    },
    ListSessions,
    NewSession {
        client_request_id: &'a str,
        title: &'a str,
        workspace: &'a str,
    },
    Switch {
        session_id: &'a str,
        since_seq: u64,
    },
    FetchRows {
        session_id: &'a str,
        from: usize,
        count: usize,
    },
}

/// What a head can do and wants — letibot's `Caps`, minus the fields whose default is what this
/// head means (`row_bodies: false`: the panel does not draw a body off its announcement).
#[derive(Debug, Serialize)]
struct Caps {
    queue: usize,
    can_decide: bool,
    features: Vec<String>,
}

/// One session in a list: the picker's row, not the whole of letibot's `SessionBrief`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Session {
    pub id: String,
    pub title: String,
    pub created_ms: u64,
    /// Whether this daemon holds a hub for it. A session only in the store has to be resumed
    /// before it can be switched to.
    pub live: bool,
}

/// The answer to `Attach`: always first, and once per attachment — `Switch` answers with one too.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Hello {
    pub protocol_version: u32,
    pub session_id: String,
    pub head_id: String,
    /// Events that fell off the scrollback before this head attached. Present and zero: the
    /// disclosure is the field, and silence about it reads as "you missed nothing".
    pub dropped: u64,
    /// Where a resume's gap starts, when this attach was a resume.
    pub resumed_from: Option<u64>,
    /// The seq the snapshot is true as of, when there was one: the next event is `seq + 1`.
    pub seq: Option<u64>,
    pub sessions: Vec<Session>,
    /// The conversation itself, on a snapshot attach: the rows, the open cards, the live turn.
    pub snapshot: Option<Snapshot>,
}

/// The conversation as an attach handed it over: the recent transcript rows, the cards still
/// open, and the turn in flight. Absent on a resume served from the scrollback, whose gap
/// follows as events instead.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Snapshot {
    pub items: Vec<SnapshotRow>,
    pub open_decisions: Vec<DecisionRequest>,
    pub turn: Option<TurnSnapshot>,
}

/// One transcript row: sessionlog's `SnapshotItem`. `item` is honestly `None` when the daemon
/// saw the row's event but never reconciled its content — the panel skips such a row rather
/// than drawing an empty message.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SnapshotRow {
    pub item_id: String,
    pub kind: String,
    pub item: Option<TranscriptItem>,
}

/// As much of a transcript row's content (`TranscriptItem`, tagged by `type`) as the panel
/// draws. Reasoning, offers and segment marks are `Other`: folded by design, or nothing here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TranscriptItem {
    /// A prompt. `speaker` is `operator`, `agent`, or a peer's handle: a job's completion or a
    /// peer's words are never drawn as the operator's own.
    User {
        text: String,
        speaker: String,
    },
    Assistant {
        text: String,
    },
    /// A settled call. Unlike the live wire, a replayed row CARRIES its payload, so the row's
    /// line count is real. No inline/full byte counts exist here: `bytes` stays unset.
    ToolResult {
        call_id: String,
        name: String,
        outcome: String,
        why: String,
        payload: String,
        edits: Vec<ToolEdit>,
    },
    Notice {
        text: String,
    },
    Other {
        kind: String,
    },
}

/// The turn in flight, from the snapshot. Its streaming text is not a transcript row until the
/// turn ends, so a head attaching mid-turn reads it here. The snapshot carries no start
/// timestamp: the status line's "started before this head attached" is the honest clock.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TurnSnapshot {
    pub running: bool,
    pub text: String,
    pub calls: Vec<CallSnapshot>,
}

/// A call the running turn has made. `running` covers proposed and started: neither has a
/// settled transcript row yet, and drawing it as settled would claim an outcome nobody said.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CallSnapshot {
    pub call_id: String,
    pub name: String,
    pub target: String,
    pub running: bool,
    /// The daemon's own `ToolStarted` timestamp, Unix ms; 0 while only proposed. The settled
    /// row's duration is measured from this, not from when this head attached.
    pub started_ts: u64,
}

/// Which channel a delta belongs to. `ToolCall` carries the model's raw call markup, so it is never
/// prose; a target from a newer daemon stays named rather than being folded into the reply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeltaTarget {
    Text,
    Reasoning,
    ToolCall,
    Other(String),
}

/// What a line-addressed fence is for, which is its label's first word: a bare range shows the
/// lines, `note` says something about them, `suggest` offers a replacement.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum CitationKind {
    #[default]
    Cite,
    Note,
    Suggest,
}

/// One cited range: the fence a reply labelled `path:start-end`, and the lines behind it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Citation {
    /// The fence's label as the model wrote it, which is how a head matches fence to citation.
    pub label: String,
    pub path: String,
    pub start: usize,
    pub end: usize,
    pub lines: Vec<String>,
    /// Why the lines are not here, or not all here. `None` when every cited line was read.
    pub error: Option<String>,
    pub kind: CitationKind,
    /// A `note`'s sentence or a `suggest`'s replacement: empty for a bare citation.
    pub body: String,
}

/// Both sides of one file a tool call changed, as the daemon bounded it to the differing region.
/// These are `edit` marks: the change is already on disk, and `before` is what a revert puts back.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ToolEdit {
    /// Relative to the session's workspace, or absolute.
    pub path: String,
    /// 1-based line of the old file that `before` starts at.
    pub before_start: usize,
    /// 1-based line of the new file that `after` starts at.
    pub after_start: usize,
    pub before: String,
    pub after: String,
}

/// What the operator pointed at, carried beside the words rather than pasted into them. The daemon
/// renders it, so a head and the daemon's own spelling of the block can never disagree.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct EditorContext {
    /// `rano`, `browser` or `acp`: who captured it, said on the block's first line.
    pub origin: String,
    pub refs: Vec<ContextRef>,
    /// The other files open in the editor, names only.
    pub open: Vec<OpenBuffer>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ask: Option<ContextAsk>,
    /// What became of the marks an earlier reply suggested.
    pub outcomes: Vec<MarkOutcome>,
}

impl EditorContext {
    /// Nothing to say beside the words: a prompt with no chips, no open files and no outcomes is
    /// sent without a context at all, which is what a daemon written before 71 understands.
    pub fn is_empty(&self) -> bool {
        self.refs.is_empty()
            && self.open.is_empty()
            && self.ask.is_none()
            && self.outcomes.is_empty()
    }
}

/// One place: a file with a range, a whole file, or a page.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ContextRef {
    /// An absolute path, or the URL a browser captured.
    pub file: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub range: Option<ContextRange>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor: Option<ContextPos>,
    /// The selection as the editor has it, empty for a bare place.
    pub text: String,
    /// The buffer differs from the file on disk, so the text sent is the one the daemon cannot read.
    pub unsaved: bool,
    pub lang: String,
    /// A browser page's title, empty for a file.
    pub title: String,
}

/// A span of a [`ContextRef`]: lines and columns 1-based, columns counted in characters.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct ContextRange {
    pub start: ContextPos,
    pub end: ContextPos,
}

/// A place in a buffer, spelled as rano's `SendEvent` spells it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct ContextPos {
    pub line: u32,
    pub column: u32,
}

/// One of the editor's other buffers: a name and two facts, never its text.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct OpenBuffer {
    pub file: String,
    pub unsaved: bool,
    pub visible: bool,
}

/// A quick ask: the preset the operator pressed and the shape of answer wanted back.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ContextAsk {
    pub preset: String,
    pub answer: AnswerMode,
}

/// What a quick ask wants: an answer in the conversation, a note to keep, or a suggested edit.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AnswerMode {
    #[default]
    Chat,
    Notes,
    Suggest,
}

impl AnswerMode {
    /// The wire's word for rano's own answer mode.
    pub fn of(answer: crate::ask::Answer) -> AnswerMode {
        match answer {
            crate::ask::Answer::Chat => AnswerMode::Chat,
            crate::ask::Answer::Notes => AnswerMode::Notes,
            crate::ask::Answer::Suggest => AnswerMode::Suggest,
        }
    }
}

/// What the operator did with a mark a reply suggested: `accepted`, `rejected` or `reverted`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct MarkOutcome {
    /// The range the mark named, as the model knows it: `article.md:41-47`.
    pub label: String,
    pub outcome: String,
}

/// One rung of a permission's ladder.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DecisionOption {
    pub option_id: String,
    pub label: String,
}

/// A card: a permission or a question the session is waiting on, addressed by `req_id`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DecisionRequest {
    pub req_id: String,
    /// `permission` or `question` — one mechanism, differing in `kind` and in which payload field
    /// a person reads.
    pub kind: String,
    pub call_id: Option<String>,
    pub summary: String,
    /// The one thing being decided about: the command a `bash` call would run, the path a write
    /// would take.
    pub target: String,
    /// The adjudication ladder, for a permission. Empty for a question.
    pub options: Vec<DecisionOption>,
    /// The model's own options, for a question: plain text in the person's vocabulary.
    pub choices: Vec<String>,
    /// Why the model is stuck, in one line.
    pub because: String,
    /// Unix millis. `None` is *wait forever*.
    pub deadline: Option<u64>,
}

/// A session event, as much of one as the panel draws.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    TurnStarted {
        turn_id: String,
        model: String,
    },
    Delta {
        turn_id: String,
        target: DeltaTarget,
        text: String,
    },
    /// A tool call starting.
    ToolStarted {
        turn_id: String,
        call_id: String,
        name: String,
        /// `read | write | exec | network`, from the tool's schema.
        access: String,
    },
    /// A tool call ending. `outcome` is the word (`ok`, `failed`, `denied`, `timeout`, …) and `why`
    /// its reason when it has one: both are what a one-line tool row says. `edits` is every file a
    /// file-editing call changed, both sides of each, and becomes `edit` marks on those lines.
    ToolFinished {
        turn_id: String,
        call_id: String,
        outcome: String,
        why: String,
        edits: Vec<ToolEdit>,
        /// The result's inline and full sizes in bytes, when the daemon reports them. The
        /// payload itself is not on this wire; the row can still say how big it was.
        bytes: Option<(u64, u64)>,
    },
    /// One round's ending. `ends_turn` says whether the whole prompt is over, because only the
    /// daemon knows what follows a round.
    TurnFinished {
        turn_id: String,
        ends_turn: bool,
    },
    TurnInterrupted {
        turn_id: String,
        reason: String,
    },
    TurnFailed {
        turn_id: String,
        error: String,
    },
    Citations {
        item_id: String,
        citations: Vec<Citation>,
    },
    DecisionRequested(DecisionRequest),
    /// A card was answered — by whichever head could decide, so every other head stops naming
    /// it. The daemon emits it as `decision_answered` with the request's id; without it a shown
    /// ask could only ever be cleared by the next turn.
    DecisionAnswered {
        req_id: String,
    },
    /// An event whose tag this build does not know. Delivered, because a panel may want to say so,
    /// and never fatal.
    Unknown {
        tag: String,
    },
}

/// Daemon → head, the subset the panel acts on. Not `Eq`: an unknown row is still named
/// `serde_json::Value`, which carries a float and cannot be that.
#[derive(Debug, Clone, PartialEq)]
pub enum Frame {
    Hello(Hello),
    Sessions {
        current: String,
        /// The id `NewSession` created, when that is what this answers.
        created: Option<String>,
        sessions: Vec<Session>,
    },
    /// A page of the session's rows, as the daemon sent them: the panel draws the rows it knows and
    /// ignores the rest, which is what keeps a new row kind from blanking the pane.
    RowsFetched {
        from: usize,
        rows: Vec<Value>,
    },
    Event {
        /// The envelope's seq: the highest one this head has consumed, which is what a command's
        /// `expected_seq` states.
        seq: u64,
        event: Event,
    },
    /// The queue overflowed or a resume gap was too large. Not an error: the panel resets to the
    /// snapshot at `seq` and carries on from there.
    Resync {
        reason: String,
        dropped: u64,
        seq: u64,
    },
    Accepted {
        client_request_id: String,
        seq: u64,
        note: String,
    },
    Rejected {
        client_request_id: String,
        reason: String,
        expected_seq: u64,
        actual_seq: u64,
    },
    /// The daemon is going away. Detach is not abort; this is the case that is.
    Bye {
        reason: String,
    },
    /// What the daemon accepted of this head's `Caps.features`, once per attach. A feature
    /// missing here must not be used: a daemon that predates it drops the field in silence.
    Features {
        accepted: Vec<String>,
    },
    /// A frame whose tag this build does not know, or a line that was not JSON at all — the tag is
    /// empty in that second case. The stream carries on, and [`Client::skipped`] counts these.
    Unknown {
        tag: String,
    },
}

impl Frame {
    /// Whether this is a frame or event this build skipped: the number a panel says out loud so a
    /// newer daemon is visible rather than silently half-drawn.
    pub fn is_unknown(&self) -> bool {
        matches!(
            self,
            Frame::Unknown { .. }
                | Frame::Event {
                    event: Event::Unknown { .. },
                    ..
                }
        )
    }
}

/// The reply being streamed: deltas appended in the order they arrived, never joined — they are
/// fragments. Thinking and raw tool-call markup have channels of their own because the panel draws
/// them differently, and a target this build does not know is not prose.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Reply {
    pub text: String,
    pub thinking: String,
    pub markup: String,
}

impl Reply {
    /// Fold one frame in, answering whether it changed anything: a panel draws on a change and does
    /// nothing on the other frames, which are most of them.
    pub fn fold(&mut self, frame: &Frame) -> bool {
        let Frame::Event {
            event: Event::Delta { target, text, .. },
            ..
        } = frame
        else {
            return false;
        };
        match target {
            DeltaTarget::Text => self.text.push_str(text),
            DeltaTarget::Reasoning => self.thinking.push_str(text),
            DeltaTarget::ToolCall => self.markup.push_str(text),
            DeltaTarget::Other(_) => return false,
        }
        true
    }
}

/// A connected head, with one thread reading its socket so the caller never blocks on the wire.
/// Frames arrive on [`Client::frame`] in the order the daemon sent them; one this build does not
/// know is delivered as [`Frame::Unknown`] and counted, never dropped silently and never fatal.
#[derive(Debug)]
pub struct Client {
    out: UnixStream,
    frames: Receiver<Frame>,
    reader: Option<JoinHandle<()>>,
    /// The highest seq the reader has taken off the socket: what a command's `expected_seq` states.
    seq: Arc<AtomicU64>,
    skipped: Arc<AtomicU64>,
    /// This head's identity, which prefixes its request ids.
    identity: String,
    requests: u64,
    /// Whether the daemon echoed [`FEATURE_QUEUE_PROMPT`] in `Features.accepted`: the one fact
    /// that says a prompt may take the queue lane. Written by the reader thread, read when a
    /// prompt asks for the lane.
    queue_prompt: Arc<AtomicBool>,
}

impl Client {
    /// Connect to a daemon socket. Discovery is [`socket`]'s job, not this one's.
    pub fn connect(socket: &Path) -> std::io::Result<Client> {
        Client::on(UnixStream::connect(socket)?)
    }

    /// A client on a stream somebody else opened: `connect`'s second half, and how a test drives a
    /// scripted peer through `UnixStream::pair()`.
    pub fn on(stream: UnixStream) -> std::io::Result<Client> {
        let reading = stream.try_clone()?;
        let seq = Arc::new(AtomicU64::new(0));
        let skipped = Arc::new(AtomicU64::new(0));
        let queue_prompt = Arc::new(AtomicBool::new(false));
        let (tx, frames) = mpsc::channel();
        let reader = std::thread::Builder::new()
            .name("rano-letibot".to_string())
            .spawn({
                let seq = Arc::clone(&seq);
                let skipped = Arc::clone(&skipped);
                let queue_prompt = Arc::clone(&queue_prompt);
                move || pump(reading, tx, seq, skipped, queue_prompt)
            })?;
        Ok(Client {
            out: stream,
            frames,
            reader: Some(reader),
            seq,
            skipped,
            identity: Seat::default().identity,
            requests: 0,
            queue_prompt,
        })
    }

    /// Announce this head. The `Hello` that answers arrives as a frame like any other, because the
    /// panel must not block on it and a `Switch` answers with one too.
    pub fn attach(&mut self, session: &str, since_seq: u64, seat: &Seat) -> std::io::Result<()> {
        self.identity.clone_from(&seat.identity);
        self.write(&Out::Attach {
            protocol_version: PROTOCOL_VERSION,
            session_id: session,
            since_seq,
            kind: &seat.kind,
            identity: &seat.identity,
            caps: Caps {
                queue: seat.queue,
                can_decide: seat.can_decide,
                features: seat.features.clone(),
            },
        })
    }

    /// A user message, queueing it behind a running turn the way the daemon does rather than
    /// refusing it. Answers the request id its `Accepted` or `Rejected` is keyed by.
    pub fn prompt(&mut self, text: &str) -> std::io::Result<String> {
        self.prompt_with_context(text, &[], None)
    }

    /// [`Client::prompt`] with files and the editor's context beside it. `context` is `None` for a
    /// prompt that only has words, which is what a daemon older than 71 is sent.
    pub fn prompt_with_context(
        &mut self,
        text: &str,
        attachments: &[Attachment],
        context: Option<&EditorContext>,
    ) -> std::io::Result<String> {
        self.prompt_in_lane(text, attachments, context, Lane::Steer)
    }

    /// A prompt on a chosen lane. `Queue` waits for the running turn to end and opens its own;
    /// it is refused by name when this daemon did not echo [`FEATURE_QUEUE_PROMPT`], since a
    /// daemon that predates the field would steer it — the opposite of what was asked.
    pub fn prompt_in_lane(
        &mut self,
        text: &str,
        attachments: &[Attachment],
        context: Option<&EditorContext>,
        lane: Lane,
    ) -> std::io::Result<String> {
        let delivery = match lane {
            Lane::Steer => None,
            Lane::Queue if self.queue_prompt.load(Ordering::Relaxed) => lane.word(),
            Lane::Queue => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::Unsupported,
                    "this daemon has no queue lane: every prompt steers",
                ));
            }
        };
        let id = self.next_id();
        let context = context.filter(|c| !c.is_empty());
        self.write(&Out::Prompt {
            client_request_id: &id,
            expected_seq: self.seq(),
            text,
            attachments,
            context,
            delivery,
        })?;
        Ok(id)
    }

    /// Whether the daemon let this head use the queue lane.
    pub fn queue_prompt_accepted(&self) -> bool {
        self.queue_prompt.load(Ordering::Relaxed)
    }

    /// Stop the running turn. Idempotent, and issuable by any attached head.
    pub fn interrupt(&mut self, reason: &str) -> std::io::Result<String> {
        let id = self.next_id();
        self.write(&Out::Interrupt {
            client_request_id: &id,
            expected_seq: self.seq(),
            reason,
        })?;
        Ok(id)
    }

    /// Answer an open permission by option id: `allow_once`, `allow_session`, `allow_always`,
    /// `reject_once`, `reject_always`, as the card listed them.
    pub fn answer(&mut self, req_id: &str, option_id: &str) -> std::io::Result<String> {
        self.answer_with(req_id, option_id, None, None)
    }

    /// [`Client::answer`] with the two things an answer may carry: a glob that widens an *always
    /// allow*, and the words a `deny_and_tell` wants the model to read.
    pub fn answer_with(
        &mut self,
        req_id: &str,
        option_id: &str,
        pattern: Option<&str>,
        note: Option<&str>,
    ) -> std::io::Result<String> {
        let id = self.next_id();
        self.write(&Out::Answer {
            client_request_id: &id,
            req_id,
            option_id,
            pattern,
            note,
        })?;
        Ok(id)
    }

    /// Answer an open question: a choice, a note, typed words, both, or an abstention. There is no
    /// vocabulary here for a permission — that is [`Client::answer`].
    pub fn answer_question(&mut self, req_id: &str, answer: &Answer) -> std::io::Result<String> {
        let id = self.next_id();
        self.write(&Out::AnswerQuestion {
            client_request_id: &id,
            req_id,
            answer: answer.clone(),
        })?;
        Ok(id)
    }

    /// The read mark, at the seq this client has seen: send it AFTER the batch is drawn, never on
    /// receipt, so a crash costs a duplicate rather than a silence.
    pub fn ack(&mut self, rendered: u64, filtered: u64) -> std::io::Result<()> {
        self.ack_at(self.seq(), rendered, filtered)
    }

    /// The read mark at a seq the caller tracks itself.
    pub fn ack_at(&mut self, seq: u64, rendered: u64, filtered: u64) -> std::io::Result<()> {
        self.write(&Out::Ack {
            seq,
            rendered,
            filtered,
        })
    }

    /// Ask for a fresh snapshot after a [`Frame::Resync`], whose rows the panel redraws.
    pub fn resync(&mut self) -> std::io::Result<()> {
        self.write(&Out::Resync)
    }

    /// Every session this daemon holds, answered with [`Frame::Sessions`].
    pub fn list_sessions(&mut self) -> std::io::Result<()> {
        self.write(&Out::ListSessions)
    }

    /// Make a session without moving to it: a head that wants one ready for later should not have
    /// to leave the one it is in, so `Switch` is a second frame.
    pub fn new_session(&mut self, title: &str, workspace: &str) -> std::io::Result<String> {
        let id = self.next_id();
        self.write(&Out::NewSession {
            client_request_id: &id,
            title,
            workspace,
        })?;
        Ok(id)
    }

    /// Move this connection to another session on the same socket. `since_seq = 0` takes a
    /// snapshot; anything else resumes.
    pub fn switch(&mut self, session: &str, since_seq: u64) -> std::io::Result<()> {
        self.write(&Out::Switch {
            session_id: session,
            since_seq,
        })
    }

    /// A page of a session's rows by ordinal, oldest first. Fewer rows than asked for means the
    /// walk stopped, not that the conversation is shorter.
    pub fn fetch_rows(&mut self, session: &str, from: usize, count: usize) -> std::io::Result<()> {
        self.write(&Out::FetchRows {
            session_id: session,
            from,
            count,
        })
    }

    /// Leave without saying the daemon is going away.
    pub fn detach(&mut self) -> std::io::Result<()> {
        self.write(&Out::Detach)
    }

    /// The next frame, waiting at most `within`. `None` is the peer gone (or the wait over): the
    /// editor is never blocked by either, and a caller reading frames needs no exclusive borrow.
    pub fn frame(&self, within: Duration) -> Option<Frame> {
        self.frames.recv_timeout(within).ok()
    }

    /// The next frame if one is already here, for a loop that has other work to do.
    pub fn try_frame(&self) -> Option<Frame> {
        self.frames.try_recv().ok()
    }

    /// The highest seq this head has seen, as a command's `expected_seq` states it. Zero means
    /// nothing has been seen, which the daemon never reads as stale.
    pub fn seq(&self) -> u64 {
        self.seq.load(Ordering::Relaxed)
    }

    /// Frames and events skipped because this build does not know them, since the connection
    /// opened.
    pub fn skipped(&self) -> u64 {
        self.skipped.load(Ordering::Relaxed)
    }

    /// A request id for the next command: the head's identity and a counter, unique per connection.
    fn next_id(&mut self) -> String {
        self.requests += 1;
        format!("{}-{}", self.identity, self.requests)
    }

    /// One frame out, flushed: a head that renders in real time cannot wait for a buffer to fill,
    /// and a daemon waiting on an unflushed frame is the same defect as one that warms up in
    /// silence.
    fn write(&mut self, frame: &Out<'_>) -> std::io::Result<()> {
        let mut line = Vec::with_capacity(256);
        serde_json::to_writer(&mut line, frame).map_err(std::io::Error::other)?;
        line.push(b'\n');
        self.out.write_all(&line)?;
        self.out.flush()
    }
}

impl Drop for Client {
    /// Shut the socket down so the reader's `read` returns instead of waiting for a peer that is
    /// still there, then wait for it: a thread still holding a receiver is a thread leaked.
    fn drop(&mut self) {
        let _ = self.out.shutdown(Shutdown::Both);
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

/// Read the socket line by line until the peer closes, delivering frames as they land. A line this
/// build cannot read is counted and skipped rather than ending the stream: a new letibot variant
/// must not blank the panel, and a closed socket is a detach, not an error.
fn pump(
    stream: UnixStream,
    frames: mpsc::Sender<Frame>,
    seq: Arc<AtomicU64>,
    skipped: Arc<AtomicU64>,
    queue_prompt: Arc<AtomicBool>,
) {
    let mut reader = BufReader::new(stream);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match reader.read_until(b'\n', &mut buf) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        // Lossy rather than strict: one byte of bad UTF-8 in one frame costs that frame, not the
        // conversation.
        let line = String::from_utf8_lossy(&buf);
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let frame = parse(line);
        // The lane is the daemon's own promise, so its echo is read the moment it lands rather
        // than left for a panel to notice.
        if let Frame::Features { accepted } = &frame {
            let yes = accepted.iter().any(|f| f == FEATURE_QUEUE_PROMPT);
            queue_prompt.store(yes, Ordering::Relaxed);
        }
        if let Some(seen) = frame.seq() {
            seq.fetch_max(seen, Ordering::Relaxed);
        }
        if frame.is_unknown() {
            skipped.fetch_add(1, Ordering::Relaxed);
        }
        if frames.send(frame).is_err() {
            break;
        }
    }
}

impl Frame {
    /// The seq this frame proves the head has consumed, where it has one.
    fn seq(&self) -> Option<u64> {
        match self {
            Frame::Event { seq, .. } => Some(*seq),
            Frame::Hello(Hello { seq, .. }) => *seq,
            Frame::Resync { seq, .. } => Some(*seq),
            _ => None,
        }
    }
}

/// One line into a frame: the tolerant half of this module. A tag this build does not know becomes
/// [`Frame::Unknown`], a missing field reads as its empty value, and a line that is not JSON is
/// unknown with no tag. Nothing here fails: a refusal mid-turn is worse than a fact we cannot use.
pub(crate) fn parse(line: &str) -> Frame {
    let Ok(v) = serde_json::from_str::<Value>(line) else {
        return Frame::Unknown { tag: String::new() };
    };
    let tag = text(&v, "frame");
    match tag.as_str() {
        "hello" => Frame::Hello(Hello {
            protocol_version: uint(&v, "protocol_version") as u32,
            session_id: text(&v, "session_id"),
            head_id: text(&v, "head_id"),
            dropped: uint(&v, "dropped"),
            resumed_from: number(&v, "resumed_from"),
            seq: number(at(&v, "snapshot"), "seq"),
            sessions: sessions(&v),
            snapshot: snapshot(at(&v, "snapshot")),
        }),
        "sessions" => Frame::Sessions {
            current: text(&v, "current"),
            created: non_empty(&v, "created"),
            sessions: sessions(&v),
        },
        "features" => Frame::Features {
            accepted: array(&v, "accepted")
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect(),
        },
        "rows_fetched" => Frame::RowsFetched {
            from: uint(&v, "from") as usize,
            rows: array(&v, "rows"),
        },
        "event" => Frame::Event {
            seq: uint(&v, "seq"),
            event: event(&v),
        },
        "resync" => Frame::Resync {
            reason: text(&v, "reason"),
            dropped: uint(&v, "dropped"),
            seq: uint(at(&v, "snapshot"), "seq"),
        },
        "accepted" => Frame::Accepted {
            client_request_id: text(&v, "client_request_id"),
            seq: uint(&v, "seq"),
            note: text(&v, "note"),
        },
        "rejected" => Frame::Rejected {
            client_request_id: text(&v, "client_request_id"),
            reason: text(&v, "reason"),
            expected_seq: uint(&v, "expected_seq"),
            actual_seq: uint(&v, "actual_seq"),
        },
        "bye" => Frame::Bye {
            reason: text(&v, "reason"),
        },
        _ => Frame::Unknown { tag },
    }
}

/// A card, from a `decision_requested` event or from the snapshot's `open_decisions`: the
/// daemon's `OpenDecision` is the same fields plus its bookkeeping.
fn decision(v: &Value) -> DecisionRequest {
    DecisionRequest {
        req_id: text(v, "req_id"),
        kind: text(v, "kind"),
        call_id: non_empty(v, "call_id"),
        summary: text(v, "summary"),
        target: text(v, "target"),
        options: array(v, "options")
            .iter()
            .map(|o| DecisionOption {
                option_id: text(o, "option_id"),
                label: text(o, "label"),
            })
            .collect(),
        choices: array(v, "choices")
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect(),
        because: text(v, "because"),
        deadline: number(v, "deadline"),
    }
}

/// The snapshot in a `hello`, when this attach was a snapshot attach rather than a resume.
fn snapshot(v: &Value) -> Option<Snapshot> {
    if !v.is_object() {
        return None;
    }
    Some(Snapshot {
        items: array(v, "items").iter().map(snapshot_row).collect(),
        open_decisions: array(v, "open_decisions").iter().map(decision).collect(),
        turn: if at(v, "turn").is_object() {
            Some(turn_snapshot(at(v, "turn")))
        } else {
            None
        },
    })
}

fn snapshot_row(v: &Value) -> SnapshotRow {
    SnapshotRow {
        item_id: text(v, "item_id"),
        kind: text(v, "kind"),
        item: if at(v, "item").is_object() {
            Some(transcript_item(at(v, "item")))
        } else {
            None
        },
    }
}

fn transcript_item(v: &Value) -> TranscriptItem {
    let kind = text(v, "type");
    match kind.as_str() {
        "user" => {
            // UserPart is tagged by `kind`: text, an image, or a file reference. The last two
            // are named in brackets rather than dropped, so the prompt keeps its attachments.
            let mut texts = Vec::new();
            for part in array(v, "parts").iter() {
                match text(part, "kind").as_str() {
                    "text" => texts.push(text(part, "text")),
                    "image" => texts.push("[image]".to_string()),
                    "file_ref" => texts.push(format!("[{}]", text(part, "path"))),
                    _ => {}
                }
            }
            // Speaker is externally tagged: "operator", "agent", or {"peer":{"from":...}}.
            let speaker = match at(v, "speaker") {
                Value::String(s) => s.clone(),
                Value::Object(o) => text(o.get("peer").unwrap_or(&Value::Null), "from"),
                _ => String::new(),
            };
            TranscriptItem::User {
                text: texts.join("\n"),
                speaker,
            }
        }
        "assistant" => TranscriptItem::Assistant {
            text: text(v, "text"),
        },
        "tool_result" => {
            // ToolOutcome is tagged by `outcome`, one level in, exactly as on the live wire.
            let outcome = at(v, "outcome");
            let why = text(outcome, "reason");
            TranscriptItem::ToolResult {
                call_id: text(v, "call_id"),
                name: text(v, "name"),
                outcome: text(outcome, "outcome"),
                why: if why.is_empty() {
                    text(outcome, "why")
                } else {
                    why
                },
                payload: text(v, "payload"),
                edits: match at(v, "edits") {
                    Value::Array(items) => items.iter().map(tool_edit).collect(),
                    Value::Object(_) => vec![tool_edit(at(v, "edits"))],
                    _ => Vec::new(),
                },
            }
        }
        "notice" => TranscriptItem::Notice {
            text: text(v, "text"),
        },
        other => TranscriptItem::Other {
            kind: other.to_string(),
        },
    }
}

/// `TurnState` and `CallState` are each tagged by `state`, one object in.
fn turn_snapshot(v: &Value) -> TurnSnapshot {
    TurnSnapshot {
        running: text(at(v, "state"), "state") == "running",
        text: text(v, "text"),
        calls: array(v, "calls")
            .iter()
            .map(|c| CallSnapshot {
                call_id: text(c, "call_id"),
                name: text(c, "name"),
                target: text(c, "target"),
                running: matches!(
                    text(at(c, "state"), "state").as_str(),
                    "running" | "proposed"
                ),
                started_ts: number(c, "started_ts").unwrap_or(0),
            })
            .collect(),
    }
}

/// A session event off an `event` frame. The envelope is flattened into the frame, so the event's
/// own tag and fields sit beside `frame`, `seq`, `ts` and `session_id`.
fn event(v: &Value) -> Event {
    let tag = text(v, "event");
    match tag.as_str() {
        "turn_started" => Event::TurnStarted {
            turn_id: text(v, "turn_id"),
            model: text(v, "model"),
        },
        "delta" => Event::Delta {
            turn_id: text(v, "turn_id"),
            target: match text(v, "target").as_str() {
                "text" => DeltaTarget::Text,
                "reasoning" => DeltaTarget::Reasoning,
                "tool_call" => DeltaTarget::ToolCall,
                other => DeltaTarget::Other(other.to_string()),
            },
            text: text(v, "text"),
        },
        "tool_started" => Event::ToolStarted {
            turn_id: text(v, "turn_id"),
            call_id: text(v, "call_id"),
            name: text(v, "name"),
            access: text(v, "access"),
        },
        "tool_finished" => {
            // `ToolOutcome` is tagged by `outcome`, so the word is one level in, and a refusal's or
            // a failure's reason sits beside it.
            let outcome = at(v, "outcome");
            let why = text(outcome, "reason");
            Event::ToolFinished {
                turn_id: text(v, "turn_id"),
                call_id: text(v, "call_id"),
                outcome: if outcome.is_string() {
                    text(v, "outcome")
                } else {
                    text(outcome, "outcome")
                },
                why: if why.is_empty() {
                    text(outcome, "why")
                } else {
                    why
                },
                // How large the result was, when the daemon says: the payload itself is not on
                // this wire, so a row names the size rather than inventing a line count.
                bytes: match (
                    v.get("inline_bytes").and_then(Value::as_u64),
                    v.get("full_bytes").and_then(Value::as_u64),
                ) {
                    (Some(inline), Some(full)) => Some((inline, full)),
                    _ => None,
                },
                // One object rather than a list is what a row written before the list existed
                // carries, and a `null` is a call that changed nothing.
                edits: match at(v, "edits") {
                    Value::Array(items) => items.iter().map(tool_edit).collect(),
                    Value::Object(_) => vec![tool_edit(at(v, "edits"))],
                    _ => Vec::new(),
                },
            }
        }
        // A daemon older than the field published only endings, so an absent `ends_turn` is one:
        // the default is what keeps an old daemon's turn visible instead of never ended.
        "turn_finished" => Event::TurnFinished {
            turn_id: text(v, "turn_id"),
            ends_turn: v.get("ends_turn").and_then(Value::as_bool).unwrap_or(true),
        },
        "turn_interrupted" => Event::TurnInterrupted {
            turn_id: text(v, "turn_id"),
            reason: text(v, "reason"),
        },
        "turn_failed" => Event::TurnFailed {
            turn_id: text(v, "turn_id"),
            error: text(v, "error"),
        },
        "citations" => Event::Citations {
            item_id: text(v, "item_id"),
            citations: array(v, "citations")
                .iter()
                .map(|c| Citation {
                    label: text(c, "label"),
                    path: text(c, "path"),
                    start: uint(c, "start") as usize,
                    end: uint(c, "end") as usize,
                    lines: strings(c, "lines"),
                    error: non_empty(c, "error"),
                    // A daemon older than 72 sends neither, and a bare range is what it meant.
                    kind: match text(c, "kind").as_str() {
                        "note" => CitationKind::Note,
                        "suggest" => CitationKind::Suggest,
                        _ => CitationKind::Cite,
                    },
                    body: text(c, "body"),
                })
                .collect(),
        },
        "decision_requested" => Event::DecisionRequested(decision(v)),
        "decision_answered" => Event::DecisionAnswered {
            req_id: text(v, "req_id"),
        },
        _ => Event::Unknown { tag },
    }
}

/// A field and its null are the same answer here, which is what lets a daemon one version behind
/// be read without a version check anywhere.
fn at<'a>(v: &'a Value, key: &str) -> &'a Value {
    v.get(key).unwrap_or(&Value::Null)
}

fn text(v: &Value, key: &str) -> String {
    v.get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn uint(v: &Value, key: &str) -> u64 {
    v.get(key).and_then(Value::as_u64).unwrap_or_default()
}

/// A number that is absent rather than zero, where the two mean different things (a deadline of
/// `None` is *wait forever*).
fn number(v: &Value, key: &str) -> Option<u64> {
    v.get(key).and_then(Value::as_u64)
}

/// A string that is absent rather than empty, for the ids and the reasons where "" is no answer.
fn non_empty(v: &Value, key: &str) -> Option<String> {
    let s = text(v, key);
    (!s.is_empty()).then_some(s)
}

fn array(v: &Value, key: &str) -> Vec<Value> {
    v.get(key)
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// A list of strings, with the entries that are not one left out.
fn strings(v: &Value, key: &str) -> Vec<String> {
    array(v, key)
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect()
}

/// One element of a `tool_finished`'s `edits`.
fn tool_edit(v: &Value) -> ToolEdit {
    ToolEdit {
        path: text(v, "path"),
        before_start: uint(v, "before_start") as usize,
        after_start: uint(v, "after_start") as usize,
        before: text(v, "before"),
        after: text(v, "after"),
    }
}

fn sessions(v: &Value) -> Vec<Session> {
    array(v, "sessions")
        .iter()
        .map(|s| Session {
            id: text(s, "session_id"),
            title: text(s, "title"),
            created_ms: uint(s, "created_ms"),
            live: s.get("live").and_then(Value::as_bool).unwrap_or(false),
        })
        .collect()
}

/// What `letibot --where --json` answers: the folder, its socket, and whether a daemon is up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Where {
    pub workspace: String,
    pub socket: PathBuf,
    /// `false` is a folder with no daemon: the panel offers to start one rather than connecting to
    /// a path nothing is listening on.
    pub up: bool,
    /// The protocol the daemon speaks, when it was asked. A fact to compare with
    /// [`PROTOCOL_VERSION`], never a gate.
    pub protocol: Option<u32>,
}

impl Where {
    /// Read the launcher's answer, tolerant of the fields a later launcher adds and of the ones
    /// said with a null. A missing socket is the one error, because everything here is for
    /// connecting.
    pub fn parse(json: &str) -> Result<Where, String> {
        let v: Value = serde_json::from_str(json)
            .map_err(|e| format!("letibot --where --json is not JSON: {e}"))?;
        let socket = text(&v, "socket");
        if socket.is_empty() {
            return Err("letibot --where --json named no socket".to_string());
        }
        Ok(Where {
            workspace: text(&v, "workspace"),
            socket: PathBuf::from(socket),
            // The launcher's word for "it is up" is `live`; `up` is what an older answer said.
            // Reading only `up` made every live daemon read as absent, which is the whole of
            // "no letibot daemon for this folder" while one was running.
            up: v
                .get("up")
                .or_else(|| v.get("live"))
                .and_then(Value::as_bool)
                .unwrap_or(false),
            protocol: number(&v, "protocol").map(|n| n as u32),
        })
    }
}

/// Why there is no daemon to attach to, in the three cases a panel says differently.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NoDaemon {
    /// No `letibot` on `PATH`: one line saying so, and the panel stays shut.
    NotInstalled,
    /// No daemon for this folder, or none listening: the panel offers Enter to start one, and rano
    /// never starts `harnessd` itself — model, mode and placement are the launcher's to choose.
    NotRunning { folder: PathBuf, said: String },
    /// The launcher has no `--where` verb yet (a later letibot package adds it) or could not look.
    /// Distinct from `NotRunning`, because nothing here can start its way out of it.
    NoVerb { said: String },
}

impl std::fmt::Display for NoDaemon {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NoDaemon::NotInstalled => write!(f, "no letibot on PATH"),
            NoDaemon::NotRunning { folder, said } => {
                if said.is_empty() {
                    write!(f, "no letibot daemon for {}", folder.display())
                } else {
                    write!(f, "no letibot daemon for {}: {said}", folder.display())
                }
            }
            NoDaemon::NoVerb { said } => {
                write!(f, "cannot ask letibot where its daemon is: {said}")
            }
        }
    }
}

impl std::error::Error for NoDaemon {}

/// The socket for a folder: `$LETIBOT_SOCKET` when set, else what the launcher says.
///
/// rano asks rather than re-deriving the hash: which folder a session belongs to is one rule, and a
/// second spelling of it in the editor is the thing that rule exists to prevent.
pub fn socket(folder: &Path) -> Result<PathBuf, NoDaemon> {
    let var = std::env::var_os(SOCKET_VAR).map(|v| v.to_string_lossy().into_owned());
    resolve(folder, var.as_deref())
}

/// [`socket`] with the variable passed in, which is how a test drives both arms without touching
/// this process's environment.
pub fn resolve(folder: &Path, var: Option<&str>) -> Result<PathBuf, NoDaemon> {
    if let Some(socket) = named(var) {
        return Ok(socket);
    }
    let said = ask(folder)?;
    if !said.up {
        return Err(NoDaemon::NotRunning {
            folder: folder.to_path_buf(),
            said: String::new(),
        });
    }
    Ok(said.socket)
}

/// The socket `$LETIBOT_SOCKET` names, if it names one. Empty and blank are unset, as they are in
/// the launcher's own `resolve`: a variable set to nothing is not an address.
fn named(var: Option<&str>) -> Option<PathBuf> {
    var.map(str::trim)
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
}

/// Ask the launcher, running `letibot --where --json` in the folder: the folder IS the question,
/// since the launcher resolves a subdirectory to its repository's daemon.
pub fn ask(folder: &Path) -> Result<Where, NoDaemon> {
    ask_with(folder, Path::new(LAUNCHER))
}

/// [`ask`] against a named binary: the seam a test drives with a script, rather than putting a fake
/// `letibot` on `PATH` in a process whose environment other tests share.
pub fn ask_with(folder: &Path, launcher: &Path) -> Result<Where, NoDaemon> {
    where_with(folder, launcher, &[])
}

/// Start a daemon for a folder the launcher says has none: `letibot --where --ensure --json`. The
/// launcher owns the model, the mode and the placement, which is exactly why rano asks it rather
/// than starting `harnessd` itself.
pub fn start(folder: &Path) -> Result<Where, NoDaemon> {
    start_with(folder, Path::new(LAUNCHER))
}

/// [`start`] against a named binary, for the same reason [`ask_with`] exists.
pub fn start_with(folder: &Path, launcher: &Path) -> Result<Where, NoDaemon> {
    where_with(folder, launcher, &["--ensure"])
}

/// The launcher's `--where [--ensure] --json`, in the folder.
fn where_with(folder: &Path, launcher: &Path, extra: &[&str]) -> Result<Where, NoDaemon> {
    // A folder that moved makes `spawn` fail with the same ENOENT as a binary that is not there,
    // so it is answered here rather than read as "no letibot on PATH".
    if !folder.is_dir() {
        return Err(NoDaemon::NoVerb {
            said: format!("no such folder: {}", folder.display()),
        });
    }
    let mut args = vec!["--where"];
    args.extend_from_slice(extra);
    args.push("--json");
    let out = match std::process::Command::new(launcher)
        .args(&args)
        .current_dir(folder)
        .output()
    {
        Ok(out) => out,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(NoDaemon::NotInstalled),
        Err(e) => {
            return Err(NoDaemon::NoVerb {
                said: format!("{}: {e}", launcher.display()),
            });
        }
    };
    if !out.status.success() {
        // An exit that still printed the answer answered the question; only a refusal with no
        // answer is a launcher without the verb, which is the one case rano must not read as
        // "no daemon", because nothing here could start one.
        if let Ok(said) = Where::parse(&String::from_utf8_lossy(&out.stdout)) {
            return Ok(said);
        }
        let said = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(NoDaemon::NoVerb {
            said: if said.is_empty() {
                format!(
                    "{} {} exited {}",
                    launcher.display(),
                    args.join(" "),
                    out.status
                )
            } else {
                said
            },
        });
    }
    Where::parse(&String::from_utf8_lossy(&out.stdout)).map_err(|said| NoDaemon::NoVerb { said })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seat() -> Seat {
        Seat::default()
    }

    /// The wire is letibot's, so the frame's own spelling is asserted rather than described: a
    /// renamed field is a daemon that answers `Rejected`, and this fails first.
    #[test]
    fn the_attach_frame_is_the_daemons_own_shape() {
        let s = seat();
        let out = Out::Attach {
            protocol_version: PROTOCOL_VERSION,
            session_id: "s-1",
            since_seq: 0,
            kind: &s.kind,
            identity: &s.identity,
            caps: Caps {
                queue: s.queue,
                can_decide: s.can_decide,
                features: s.features.clone(),
            },
        };
        let v: Value = serde_json::from_str(&serde_json::to_string(&out).unwrap()).unwrap();
        assert_eq!(v["frame"], "attach");
        assert_eq!(v["protocol_version"], PROTOCOL_VERSION);
        assert_eq!(v["session_id"], "s-1");
        assert_eq!(v["since_seq"], 0);
        assert_eq!(v["kind"], "editor");
        assert_eq!(v["identity"], "rano");
        assert_eq!(v["caps"]["queue"], 1024);
        assert_eq!(v["caps"]["can_decide"].as_bool(), Some(true));
        assert_eq!(v["caps"]["features"][0], "editor_v1");
    }

    #[test]
    fn a_prompt_and_an_interrupt_carry_the_seq_and_an_id() {
        let prompt = Out::Prompt {
            client_request_id: "rano-1",
            expected_seq: 7,
            text: "why is it slow?",
            attachments: &[],
            context: None,
            delivery: None,
        };
        let v: Value = serde_json::from_str(&serde_json::to_string(&prompt).unwrap()).unwrap();
        assert_eq!(v["frame"], "prompt");
        assert_eq!(v["client_request_id"], "rano-1");
        assert_eq!(v["expected_seq"], 7);
        assert_eq!(v["text"], "why is it slow?");
        assert_eq!(v["attachments"], serde_json::json!([]));
        assert!(
            v.get("context").is_none(),
            "a prompt that points at nothing says nothing about it"
        );
        assert!(
            v.get("delivery").is_none(),
            "a steer is the frame without the lane: every prompt before the lane reads the same"
        );

        let stop = Out::Interrupt {
            client_request_id: "rano-2",
            expected_seq: 7,
            reason: "esc esc",
        };
        let v: Value = serde_json::from_str(&serde_json::to_string(&stop).unwrap()).unwrap();
        assert_eq!(v["frame"], "interrupt");
        assert_eq!(v["reason"], "esc esc");
    }

    #[test]
    fn an_answer_names_its_option_and_omits_what_it_does_not_say() {
        let out = Out::Answer {
            client_request_id: "rano-3",
            req_id: "r-9",
            option_id: "allow_once",
            pattern: None,
            note: None,
        };
        let v: Value = serde_json::from_str(&serde_json::to_string(&out).unwrap()).unwrap();
        assert_eq!(v["frame"], "answer");
        assert_eq!(v["req_id"], "r-9");
        assert_eq!(v["option_id"], "allow_once");
        assert!(v.get("pattern").is_none() && v.get("note").is_none());

        let question = Out::AnswerQuestion {
            client_request_id: "rano-4",
            req_id: "r-10",
            answer: Answer {
                option: Some(1),
                ..Answer::default()
            },
        };
        let v: Value = serde_json::from_str(&serde_json::to_string(&question).unwrap()).unwrap();
        assert_eq!(v["frame"], "answer_question");
        assert_eq!(v["answer"]["option"], 1);
        assert!(v["answer"].get("abstain").is_none());
    }

    #[test]
    fn a_hello_is_read_with_the_snapshot_seq_and_the_sessions() {
        let line = r#"{"frame":"hello","protocol_version":70,"session_id":"s-1","head_id":"h-2",
            "dropped":4,"resumed_from":null,
            "snapshot":{"session_id":"s-1","seq":41,"dropped":0,"items_dropped":0,
                        "items":[],"decisions":[],"unknown_field":true},
            "wiring":{"model":"m"},"sessions":[{"session_id":"s-1","title":"a","created_ms":9,
            "status":{"turns":1},"wiring":{},"live":true,"stored_items":3}]}"#;
        let Frame::Hello(hello) = parse(line) else {
            panic!("a hello is a hello");
        };
        assert_eq!(hello.protocol_version, 70);
        assert_eq!(hello.head_id, "h-2");
        assert_eq!(hello.dropped, 4);
        assert_eq!(hello.seq, Some(41));
        assert_eq!(hello.sessions.len(), 1);
        assert_eq!(hello.sessions[0].id, "s-1");
        assert_eq!(hello.sessions[0].title, "a");
        assert!(hello.sessions[0].live);
    }

    #[test]
    fn deltas_fold_into_text_and_thinking_apart() {
        let mut reply = Reply::default();
        let started =
            parse(r#"{"frame":"event","seq":3,"event":"turn_started","turn_id":"t","model":"m"}"#);
        // The frames that are not deltas change nothing, which is most of them.
        assert!(!reply.fold(&started));
        for line in [
            r#"{"frame":"event","seq":4,"event":"delta","turn_id":"t","target":"reasoning","text":"hmm "}"#,
            r#"{"frame":"event","seq":5,"event":"delta","turn_id":"t","target":"text","text":"the "}"#,
            r#"{"frame":"event","seq":6,"event":"delta","turn_id":"t","target":"text","text":"answer."}"#,
        ] {
            assert!(reply.fold(&parse(line)), "{line}");
        }
        assert_eq!(reply.text, "the answer.");
        assert_eq!(reply.thinking, "hmm ");
    }

    #[test]
    fn an_unknown_delta_target_is_not_prose() {
        let frame = parse(
            r#"{"frame":"event","seq":1,"event":"delta","turn_id":"t","target":"chant","text":"x"}"#,
        );
        let mut reply = Reply::default();
        assert!(!reply.fold(&frame));
        assert_eq!(reply.text, "");
        assert!(!frame.is_unknown());
    }

    #[test]
    fn a_tool_call_ends_with_its_word_and_its_reason() {
        let ok = parse(
            r#"{"frame":"event","seq":2,"event":"tool_finished","turn_id":"t","call_id":"c",
                "outcome":{"outcome":"ok"},"payload_digest":"d","inline_bytes":1,"full_bytes":1,
                "spill":null,"repairs":0}"#,
        );
        let Event::ToolFinished { outcome, why, .. } = event_of(&ok) else {
            panic!("a tool_finished is one");
        };
        assert_eq!((outcome.as_str(), why.as_str()), ("ok", ""));

        let failed = parse(
            r#"{"frame":"event","seq":2,"event":"tool_finished","turn_id":"t","call_id":"c",
                "outcome":{"outcome":"failed","reason":"no such file"}}"#,
        );
        let Event::ToolFinished { outcome, why, .. } = event_of(&failed) else {
            panic!("a tool_finished is one");
        };
        assert_eq!((outcome.as_str(), why.as_str()), ("failed", "no such file"));
    }

    #[test]
    fn a_turn_finished_without_the_field_is_an_ending() {
        let frame = parse(
            r#"{"frame":"event","seq":9,"event":"turn_finished","turn_id":"t",
                "finish_reason":"eos","usage":{},"timings":{}}"#,
        );
        let Event::TurnFinished { ends_turn, .. } = event_of(&frame) else {
            panic!("a turn_finished is one");
        };
        assert!(ends_turn);
    }

    /// The queue lane, byte for byte as letibot's `queue_lane_wire.rs` proves it from its side:
    /// `delivery` beside `text`, the exact word, and nothing else on the frame changed.
    #[test]
    fn a_queued_prompt_is_the_frame_with_delivery_queue_beside_text() {
        let prompt = Out::Prompt {
            client_request_id: "rano-3",
            expected_seq: 0,
            text: "and then land it",
            attachments: &[],
            context: None,
            delivery: Lane::Queue.word(),
        };
        let json = serde_json::to_string(&prompt).unwrap();
        let v: Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["delivery"], "queue", "{json}");
        assert_eq!(v["text"], "and then land it");
        assert_eq!(v["client_request_id"], "rano-3");
        assert_eq!(v["expected_seq"], 0);
        assert_eq!(v["attachments"], serde_json::json!([]));
        assert!(v.get("context").is_none(), "{json}");
        assert_eq!(Lane::Steer.word(), None, "a steer adds nothing");
    }

    /// The daemon's echo is the only promise: the seat asks for the lane, the `features` frame
    /// is parsed, and the client answers what the daemon accepted.
    #[test]
    fn the_seat_asks_for_the_lane_and_the_features_frame_is_read() {
        let seat = Seat::default();
        assert!(
            seat.features.iter().any(|f| f == FEATURE_QUEUE_PROMPT),
            "the seat advertises the lane: {:?}",
            seat.features
        );
        let Frame::Features { accepted } =
            parse(r#"{"frame":"features","accepted":["editor_v1","queue_prompt_v1"]}"#)
        else {
            panic!("a features frame is one");
        };
        assert!(accepted.iter().any(|f| f == FEATURE_QUEUE_PROMPT));
        let Frame::Features { accepted } = parse(r#"{"frame":"features","accepted":[]}"#) else {
            unreachable!()
        };
        assert!(accepted.is_empty());
    }

    /// Over a real socket pair: the reader thread reads the daemon's echo and the client's
    /// prompt door obeys it — refused by name before the echo, `delivery: queue` after it.
    #[test]
    fn the_queue_lane_opens_only_after_the_daemon_echoes_it() {
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::net::UnixStream;
        let (mine, theirs) = UnixStream::pair().unwrap();
        let mut client = Client::on(mine).unwrap();
        assert!(
            !client.queue_prompt_accepted(),
            "nothing is promised before the daemon says so"
        );
        let err = client
            .prompt_in_lane("first", &[], None, Lane::Queue)
            .expect_err("refused by name");
        assert!(
            err.to_string().contains("no queue lane"),
            "the refusal names the lane: {err}"
        );
        let mut peer = BufReader::new(theirs);
        let mut daemon = peer.get_ref().try_clone().unwrap();
        daemon
            .write_all(b"{\"frame\":\"features\",\"accepted\":[\"queue_prompt_v1\"]}\n")
            .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !client.queue_prompt_accepted() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(client.queue_prompt_accepted(), "the echo was read");
        let id = client
            .prompt_in_lane("and then land it", &[], None, Lane::Queue)
            .expect("the lane is open");
        assert!(id.starts_with("rano-"), "the id is the head's own: {id}");
        let mut line = String::new();
        peer.read_line(&mut line).unwrap();
        let v: Value = serde_json::from_str(line.trim()).unwrap();
        assert_eq!(v["frame"], "prompt", "{line}");
        assert_eq!(v["delivery"], "queue", "{line}");
        assert_eq!(v["text"], "and then land it");
        // And a steer on the same connection is still the frame without the field.
        let _ = client.prompt("now").unwrap();
        let mut line = String::new();
        peer.read_line(&mut line).unwrap();
        let v: Value = serde_json::from_str(line.trim()).unwrap();
        assert!(
            v.get("delivery").is_none(),
            "a steer carries no lane: {line}"
        );
    }

    #[test]
    fn a_card_carries_both_its_ladder_and_its_choices() {
        let frame = parse(
            r#"{"frame":"event","seq":5,"event":"decision_requested","req_id":"r-1",
                "kind":"permission","call_id":"c","access":"exec","summary":"run it",
                "target":"rm -rf x","detail":"","options":[
                  {"option_id":"allow_once","label":"once","kind":"allow_once"}],
                "options_note":"","choices":[],"because":"","deadline":null,
                "on_timeout":"deny"}"#,
        );
        let Event::DecisionRequested(card) = event_of(&frame) else {
            panic!("a decision_requested is one");
        };
        assert_eq!(card.req_id, "r-1");
        assert_eq!(card.kind, "permission");
        assert_eq!(card.target, "rm -rf x");
        assert_eq!(card.options[0].option_id, "allow_once");
        assert_eq!(card.deadline, None);
        // The answer is an event too, so a head that only watches can stop naming the card.
        let frame = parse(
            r#"{"frame":"event","seq":6,"event":"decision_answered","req_id":"r-1",
                "outcome":{"outcome":"selected","option_id":"allow_once"},
                "by":{"kind":"human"},"basis":"","late":false}"#,
        );
        let Event::DecisionAnswered { req_id } = event_of(&frame) else {
            panic!("a decision_answered is one");
        };
        assert_eq!(req_id, "r-1");
    }

    #[test]
    fn a_citation_keeps_the_lines_and_the_reason_they_are_missing() {
        let frame = parse(
            r#"{"frame":"event","seq":8,"event":"citations","item_id":"i-1","citations":[
                {"label":"a.rs:1-2","path":"a.rs","start":1,"end":2,"lines":["x","y"]},
                {"label":"b.rs:9-9","path":"b.rs","start":9,"end":9,"lines":[],
                 "error":"past the end"}]}"#,
        );
        let Event::Citations { citations, .. } = event_of(&frame) else {
            panic!("a citations is one");
        };
        assert_eq!(citations[0].start, 1);
        assert_eq!(citations[0].lines, vec!["x", "y"]);
        assert_eq!(citations[0].error, None);
        assert_eq!(citations[1].error.as_deref(), Some("past the end"));
    }

    /// A tag this build does not know is the case a new letibot version is: skipped, counted by
    /// the pump, and never a reason to end the stream.
    #[test]
    fn an_unknown_frame_or_event_tag_is_unknown_and_not_an_error() {
        let frame = parse(r#"{"frame":"telemetry","rows":[]}"#);
        assert_eq!(
            frame,
            Frame::Unknown {
                tag: "telemetry".to_string()
            }
        );
        assert!(frame.is_unknown());

        let event = parse(r#"{"frame":"event","seq":4,"event":"explain","turn_id":"t"}"#);
        assert_eq!(
            event,
            Frame::Event {
                seq: 4,
                event: Event::Unknown {
                    tag: "explain".to_string()
                },
            }
        );
        assert!(event.is_unknown());
    }

    #[test]
    fn a_line_that_is_not_json_is_unknown_rather_than_a_panic() {
        for line in ["not json", "[]", "{", ""] {
            let frame = parse(line);
            assert!(frame.is_unknown(), "{line}");
        }
    }

    #[test]
    fn a_rejection_says_what_the_head_was_looking_at() {
        let frame = parse(
            r#"{"frame":"rejected","client_request_id":"rano-1","reason":"stale expected_seq",
                "expected_seq":2,"actual_seq":9}"#,
        );
        let Frame::Rejected {
            client_request_id,
            expected_seq,
            actual_seq,
            ..
        } = frame
        else {
            panic!("a rejected is a rejected");
        };
        assert_eq!(client_request_id, "rano-1");
        assert_eq!((expected_seq, actual_seq), (2, 9));
    }

    /// `$LETIBOT_SOCKET` is authoritative, as it is in the launcher: the arm is checked without
    /// running anything, which is also why a test can check it at all.
    #[test]
    fn the_socket_variable_wins_over_the_launcher() {
        let folder = Path::new("/nowhere");
        assert_eq!(
            resolve(folder, Some("/run/x.sock")),
            Ok(PathBuf::from("/run/x.sock"))
        );
        // Blank and empty are unset: a variable set to nothing is not an address.
        assert_eq!(named(Some("  ")), None);
        assert_eq!(named(Some("")), None);
        assert_eq!(named(None), None);
    }

    #[test]
    fn a_where_answer_names_the_socket_and_whether_it_is_up() {
        let where_ = Where::parse(
            r#"{"workspace":"/w","socket":"/run/letibot/abc.sock","up":true,"protocol":70}"#,
        )
        .unwrap();
        assert_eq!(where_.socket, PathBuf::from("/run/letibot/abc.sock"));
        assert!(where_.up);
        assert_eq!(where_.protocol, Some(70));

        // A field a later launcher adds is not an error, and one said with a null is absent.
        let where_ =
            Where::parse(r#"{"socket":"/r/x.sock","up":false,"protocol":null,"extra":1}"#).unwrap();
        assert_eq!(where_.protocol, None);
        assert!(!where_.up);
        assert_eq!(where_.workspace, "");

        // The launcher's own word, as it actually answers today: `live`, not `up`. A parser that
        // only knows `up` reads every live daemon as absent, which is the failure this test pins.
        let live = Where::parse(
            r#"{"socket":"/run/letibot/abc.sock","live":true,"project":"x","folder":"/w"}"#,
        )
        .unwrap();
        assert!(live.up, "the launcher's `live` means up");
        assert!(live.protocol.is_none());

        assert!(Where::parse(r#"{"up":true}"#).is_err());
        assert!(Where::parse("not a where").is_err());
    }

    /// The verb is a later letibot package's, so its absence is an answer of its own kind and not a
    /// missing daemon: nothing here can start a daemon that this rano could not ask about.
    /// `ask_with` for a launcher this test just wrote. Another test thread forking while the
    /// script's write descriptor is still open makes the exec fail with ETXTBSY; that is the
    /// harness racing itself, so it is retried, and every other answer is returned as it was.
    fn ask_fresh(folder: &Path, launcher: &Path) -> Result<Where, NoDaemon> {
        for _ in 0..50 {
            match ask_with(folder, launcher) {
                Err(e) if e.to_string().contains("Text file busy") => {
                    std::thread::sleep(std::time::Duration::from_millis(20))
                }
                other => return other,
            }
        }
        ask_with(folder, launcher)
    }

    #[test]
    fn a_launcher_that_is_not_there_is_not_installed() {
        let gone = ask_with(Path::new("/"), Path::new("/nonexistent/letibot"));
        assert_eq!(gone, Err(NoDaemon::NotInstalled));
    }

    /// A launcher script is the only honest stand-in for one that is not built yet, and it is run
    /// from a path rather than through `PATH`, which this process's other tests share.
    #[test]
    fn a_where_answer_is_read_from_the_launchers_own_output() {
        use std::os::unix::fs::PermissionsExt;

        let dir = std::env::temp_dir().join(format!("rano-where-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let launcher = dir.join("letibot");
        std::fs::write(
            &launcher,
            format!(
                "#!/bin/sh\necho '{{\"workspace\":\"{}\",\"socket\":\"{}/s.sock\",\"up\":true,\"protocol\":72}}'\n",
                dir.display(),
                dir.display()
            ),
        )
        .unwrap();
        let mut perms = std::fs::metadata(&launcher).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&launcher, perms).unwrap();

        let said = ask_fresh(&dir, &launcher).unwrap();
        assert_eq!(said.socket, dir.join("s.sock"));
        assert_eq!(said.workspace, dir.display().to_string());
        assert!(said.up);
        assert_eq!(said.protocol, Some(PROTOCOL_VERSION));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A daemon that is not up is a folder to offer starting one for, not a socket to connect to.
    #[test]
    fn a_launcher_that_says_nothing_is_up_is_a_folder_with_no_daemon() {
        use std::os::unix::fs::PermissionsExt;

        let dir = std::env::temp_dir().join(format!("rano-down-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let launcher = dir.join("letibot");
        std::fs::write(
            &launcher,
            format!(
                "#!/bin/sh\necho '{{\"workspace\":\"{}\",\"socket\":\"{}/s.sock\",\"up\":false}}'\n",
                dir.display(),
                dir.display()
            ),
        )
        .unwrap();
        let mut perms = std::fs::metadata(&launcher).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&launcher, perms).unwrap();

        let said = ask_fresh(&dir, &launcher).unwrap();
        assert!(!said.up);
        assert!(
            NoDaemon::NotRunning {
                folder: dir.clone(),
                said: String::new(),
            }
            .to_string()
            .contains("no letibot daemon for")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A launcher without the verb refuses the question, and that is not "no daemon": there is
    /// nothing this side could start.
    #[test]
    fn a_launcher_without_the_verb_says_what_it_said() {
        use std::os::unix::fs::PermissionsExt;

        let dir = std::env::temp_dir().join(format!("rano-noverb-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let launcher = dir.join("letibot");
        std::fs::write(
            &launcher,
            "#!/bin/sh\necho 'no such flag: --where' >&2\nexit 2\n",
        )
        .unwrap();
        let mut perms = std::fs::metadata(&launcher).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&launcher, perms).unwrap();

        let said = ask_fresh(&dir, &launcher);
        assert_eq!(
            said,
            Err(NoDaemon::NoVerb {
                said: "no such flag: --where".to_string(),
            })
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A folder that is gone is not a missing launcher: the message says which of the two it is,
    /// and that is the difference between "install letibot" and "open a folder that exists".
    #[test]
    fn a_folder_that_is_gone_is_not_a_missing_letibot() {
        let said = ask_with(
            Path::new("/nonexistent/folder-for-a-rano-test"),
            Path::new("/bin/sh"),
        );
        let Err(NoDaemon::NoVerb { said }) = said else {
            panic!("a gone folder is not a missing launcher: {said:?}");
        };
        assert!(said.contains("no such folder"), "{said}");
    }

    /// A launcher that answers and then exits non-zero still answered: the JSON it printed is the
    /// answer, and reading it as a missing verb would hide a daemon that is right there.
    #[test]
    fn a_launcher_that_answers_and_exits_nonzero_was_still_asked() {
        use std::os::unix::fs::PermissionsExt;

        let dir = std::env::temp_dir().join(format!("rano-nonzero-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let launcher = dir.join("letibot");
        std::fs::write(
            &launcher,
            format!(
                "#!/bin/sh\necho '{{\"workspace\":\"{}\",\"socket\":\"{}/s.sock\",\"up\":true}}'\nexit 1\n",
                dir.display(),
                dir.display()
            ),
        )
        .unwrap();
        let mut perms = std::fs::metadata(&launcher).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&launcher, perms).unwrap();

        let said = ask_fresh(&dir, &launcher).unwrap();
        assert_eq!(said.socket, dir.join("s.sock"));
        assert!(said.up);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The frame a `parse` answered an event for: a small borrow so a test can match on the event
    /// of a whole frame without repeating the envelope.
    fn event_of(frame: &Frame) -> Event {
        match frame {
            Frame::Event { event, .. } => event.clone(),
            other => panic!("not an event: {other:?}"),
        }
    }
}
