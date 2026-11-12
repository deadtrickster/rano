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
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::thread::JoinHandle;
use std::time::Duration;

use serde::Serialize;
use serde_json::Value;

/// The protocol this build was written against. rano sends it and reads the daemon's from `Hello`:
/// the daemon logs a difference and serves anyway, so this is a fact to state, not a gate to pass.
pub const PROTOCOL_VERSION: u32 = 70;

/// The head kind a panel attaches as.
pub const KIND_EDITOR: &str = "editor";

/// The feature string that says this head is an editor panel. Free-form and forward compatible on
/// letibot's side, so a daemon that has never heard of it ignores it.
pub const FEATURE_EDITOR_V1: &str = "editor_v1";

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
            features: vec![FEATURE_EDITOR_V1.to_string()],
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

/// One cited range: the empty fence a reply labelled `path:start-end`, and the lines behind it.
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
    /// its reason when it has one: both are what a one-line tool row says.
    ToolFinished {
        turn_id: String,
        call_id: String,
        outcome: String,
        why: String,
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
        let (tx, frames) = mpsc::channel();
        let reader = std::thread::Builder::new()
            .name("rano-letibot".to_string())
            .spawn({
                let seq = Arc::clone(&seq);
                let skipped = Arc::clone(&skipped);
                move || pump(reading, tx, seq, skipped)
            })?;
        Ok(Client {
            out: stream,
            frames,
            reader: Some(reader),
            seq,
            skipped,
            identity: Seat::default().identity,
            requests: 0,
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
        self.prompt_with(text, &[])
    }

    /// [`Client::prompt`] with files beside it.
    pub fn prompt_with(
        &mut self,
        text: &str,
        attachments: &[Attachment],
    ) -> std::io::Result<String> {
        let id = self.next_id();
        self.write(&Out::Prompt {
            client_request_id: &id,
            expected_seq: self.seq(),
            text,
            attachments,
        })?;
        Ok(id)
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
fn parse(line: &str) -> Frame {
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
        }),
        "sessions" => Frame::Sessions {
            current: text(&v, "current"),
            created: non_empty(&v, "created"),
            sessions: sessions(&v),
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
                    lines: array(c, "lines")
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect(),
                    error: non_empty(c, "error"),
                })
                .collect(),
        },
        "decision_requested" => Event::DecisionRequested(DecisionRequest {
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
        }),
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
            up: v.get("up").and_then(Value::as_bool).unwrap_or(false),
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
    // A folder that moved makes `spawn` fail with the same ENOENT as a binary that is not there,
    // so it is answered here rather than read as "no letibot on PATH".
    if !folder.is_dir() {
        return Err(NoDaemon::NoVerb {
            said: format!("no such folder: {}", folder.display()),
        });
    }
    let out = match std::process::Command::new(launcher)
        .args(["--where", "--json"])
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
                    "{} --where --json exited {}",
                    launcher.display(),
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
        assert_eq!(v["protocol_version"], 70);
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
        };
        let v: Value = serde_json::from_str(&serde_json::to_string(&prompt).unwrap()).unwrap();
        assert_eq!(v["frame"], "prompt");
        assert_eq!(v["client_request_id"], "rano-1");
        assert_eq!(v["expected_seq"], 7);
        assert_eq!(v["text"], "why is it slow?");
        assert_eq!(v["attachments"], serde_json::json!([]));

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

        assert!(Where::parse(r#"{"up":true}"#).is_err());
        assert!(Where::parse("not a where").is_err());
    }

    /// The verb is a later letibot package's, so its absence is an answer of its own kind and not a
    /// missing daemon: nothing here can start a daemon that this rano could not ask about.
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
                "#!/bin/sh\necho '{{\"workspace\":\"{}\",\"socket\":\"{}/s.sock\",\"up\":true,\"protocol\":70}}'\n",
                dir.display(),
                dir.display()
            ),
        )
        .unwrap();
        let mut perms = std::fs::metadata(&launcher).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&launcher, perms).unwrap();

        let said = ask_with(&dir, &launcher).unwrap();
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

        let said = ask_with(&dir, &launcher).unwrap();
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

        let said = ask_with(&dir, &launcher);
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

        let said = ask_with(&dir, &launcher).unwrap();
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
