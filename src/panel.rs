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

use std::path::{Path, PathBuf};

use crate::letibot::{DeltaTarget, Event, Frame, NoDaemon, Session};
use crate::markdown::{IncrementalMarkdown, MarkdownView, RenderOptions};
use crate::render::{Line, Span};
use crate::style::Role;

/// What the panel is doing, in the words its top edge shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanelState {
    /// Nothing asked for yet: the panel has never looked for a daemon.
    Closed,
    /// Asking the launcher where the folder's daemon is, and it for its sessions.
    Connecting,
    /// The folder's sessions are on screen: sending picks one.
    Picking,
    /// Attached, and nothing is running.
    Idle,
    /// A turn is running.
    Thinking,
    /// A card is open that another head has to answer, because this one cannot.
    Waiting,
    /// Nothing to talk to. `startable` is whether Enter can start a daemon for the folder.
    Offline { startable: bool },
}

impl PanelState {
    /// The two or three words the top edge carries.
    pub fn words(self) -> &'static str {
        match self {
            PanelState::Closed | PanelState::Connecting => "connecting",
            PanelState::Picking => "pick a session",
            PanelState::Idle => "idle",
            PanelState::Thinking => "thinking",
            PanelState::Waiting => "waiting on you",
            PanelState::Offline { .. } => "no daemon",
        }
    }

    /// The role it is drawn in: work in progress is yellow, a wait is loud, a failure is red.
    pub fn role(self) -> Role {
        match self {
            PanelState::Connecting | PanelState::Thinking => Role::Pending,
            PanelState::Waiting => Role::Attention,
            PanelState::Offline { .. } => Role::Failure,
            _ => Role::Faint,
        }
    }
}

/// One thing the host must do on the wire. The panel never writes a frame itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Act {
    /// Find the folder's daemon and ask it for its sessions.
    Connect(PathBuf),
    /// Start a daemon for the folder, then connect.
    Start(PathBuf),
    /// Attach this connection to that session.
    Attach(String),
    /// Make a session in the folder; the answer's `created` names it.
    New,
    /// The person's words.
    Prompt(String),
    /// Stop the running turn.
    Interrupt,
}

/// A reply as it is being written: the text, and the Markdown the panel renders it with while the
/// deltas land, so a frame costs the tail rather than the whole reply.
#[derive(Default)]
pub struct Reply {
    pub text: String,
    md: IncrementalMarkdown,
    view: MarkdownView,
}

impl Reply {
    fn push(&mut self, delta: &str) {
        self.text.push_str(delta);
        self.md.push(delta);
    }
}

impl std::fmt::Debug for Reply {
    /// The text, and never the parser's internals: what a failed assertion wants to print.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Reply").field("text", &self.text).finish()
    }
}

/// One row of the conversation.
#[derive(Debug)]
pub enum Row {
    /// What the person said.
    User(String),
    /// What the session said, streaming while the turn runs. Boxed: a reply carries the Markdown
    /// parser's window, and every other row would pay for it.
    Reply(Box<Reply>),
    /// Anything that is not prose: a tool call, a turn's end, a refusal — one line.
    Note(String),
}

/// The panel, and the conversation it is showing.
#[derive(Debug)]
pub struct Panel {
    /// Drawn beside the text.
    pub open: bool,
    /// Holds the keyboard: the `panel` keymap mode.
    pub focus: bool,
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
    /// The file the folder was last worked out for, so the git call happens once per buffer.
    seen_name: Option<PathBuf>,
    acts: Vec<Act>,
    /// Tool calls still running: `(call_id, row, name)`.
    running: Vec<(String, usize, String)>,
    /// The conversation at the last built width, oldest first.
    built: Vec<Line>,
    built_for: Option<(usize, u64)>,
    version: u64,
}

impl Default for Panel {
    fn default() -> Self {
        Panel {
            open: false,
            focus: false,
            zoom: false,
            folder: PathBuf::new(),
            moved: None,
            session: String::new(),
            title: String::new(),
            state: PanelState::Closed,
            said: String::new(),
            rows: Vec::new(),
            input: String::new(),
            cursor: 0,
            scroll: 0,
            follow: true,
            sessions: Vec::new(),
            pick: 0,
            state_file: None,
            seen_name: None,
            acts: Vec::new(),
            running: Vec::new(),
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
        self.focus = true;
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
        self.focus = false;
        self.zoom = false;
    }

    /// Ask the host for the folder's daemon and its sessions.
    fn connect(&mut self) {
        self.state = PanelState::Connecting;
        self.session.clear();
        self.title.clear();
        self.said.clear();
        self.moved = None;
        self.acts.push(Act::Connect(self.folder.clone()));
        self.touch();
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
                self.remember();
            }
            Frame::Sessions {
                created, sessions, ..
            } => {
                self.sessions.clone_from(sessions);
                if let Some(id) = created {
                    self.attach(id);
                } else if matches!(self.state, PanelState::Connecting | PanelState::Picking) {
                    self.choose();
                }
            }
            Frame::Accepted { .. } => self.state = PanelState::Thinking,
            Frame::Rejected { reason, .. } => {
                self.state = PanelState::Idle;
                self.said = format!("the daemon refused that: {reason}");
            }
            Frame::Bye { reason } => {
                self.state = PanelState::Offline { startable: false };
                self.said = if reason.is_empty() {
                    "the daemon left".to_string()
                } else {
                    format!("the daemon left: {reason}")
                };
            }
            Frame::Resync { .. } => self.said = "the stream restarted".to_string(),
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
        self.state = PanelState::Connecting;
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
                self.state = PanelState::Idle;
                self.said = "no sessions in this folder: enter makes one".to_string();
            }
            1 => {
                let id = self.sessions[0].id.clone();
                self.attach(&id);
            }
            _ => {
                self.state = PanelState::Picking;
                self.pick = 0;
                self.follow = true;
            }
        }
        self.touch();
    }

    /// The pick took the highlighted row.
    fn pick_now(&mut self) {
        let Some(session) = self.sessions.get(self.pick).map(|s| s.id.clone()) else {
            self.said = "no session to pick".to_string();
            self.state = PanelState::Idle;
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
        self.touch();
    }

    fn event(&mut self, event: &Event) {
        match event {
            Event::TurnStarted { .. } => self.state = PanelState::Thinking,
            Event::Delta {
                target: DeltaTarget::Text,
                text,
                ..
            } => {
                if !matches!(self.rows.last(), Some(Row::Reply(_))) {
                    self.rows.push(Row::Reply(Box::default()));
                }
                if let Some(Row::Reply(reply)) = self.rows.last_mut() {
                    reply.push(text);
                }
                self.follow = true;
                self.state = PanelState::Thinking;
            }
            // Thinking and the model's raw call markup are not prose: the panel folds both.
            Event::Delta { .. } => {}
            Event::ToolStarted { call_id, name, .. } => {
                self.running
                    .push((call_id.clone(), self.rows.len(), name.clone()));
                self.rows.push(Row::Note(format!("{name} …")));
                self.follow = true;
            }
            Event::ToolFinished {
                call_id,
                outcome,
                why,
                ..
            } => {
                let said = if why.is_empty() {
                    outcome.clone()
                } else {
                    format!("{outcome}: {why}")
                };
                match self.running.iter().position(|(id, _, _)| id == call_id) {
                    Some(at) => {
                        let (_, row, name) = self.running.remove(at);
                        if let Some(Row::Note(note)) = self.rows.get_mut(row) {
                            *note = format!("{name} · {said}");
                        }
                    }
                    None => self.rows.push(Row::Note(said)),
                }
            }
            Event::TurnFinished { ends_turn, .. } => {
                // A round that is not the end of the prompt leaves the panel thinking, which is
                // what only the daemon can tell.
                self.state = if *ends_turn {
                    PanelState::Idle
                } else {
                    PanelState::Thinking
                };
            }
            Event::TurnInterrupted { reason, .. } => {
                self.rows.push(Row::Note(if reason.is_empty() {
                    "interrupted".to_string()
                } else {
                    format!("interrupted: {reason}")
                }));
                self.state = PanelState::Idle;
            }
            Event::TurnFailed { error, .. } => {
                self.rows.push(Row::Note(format!("failed: {error}")));
                self.state = PanelState::Idle;
            }
            Event::DecisionRequested(card) => {
                // This head says `can_decide: false`, so the daemon put the card in front of one
                // that can answer it; saying where is the panel's whole job here.
                self.rows.push(Row::Note(format!(
                    "{} waiting in letibot: {}",
                    card.kind, card.summary
                )));
                self.state = PanelState::Waiting;
                self.follow = true;
            }
            // A citation becomes a mark on the text, which is a later package's work.
            Event::Citations { .. } | Event::Unknown { .. } => {}
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
            _ => {
                let text = self.input.trim().to_string();
                if text.is_empty() {
                    return;
                }
                self.rows.push(Row::User(text.clone()));
                self.input.clear();
                self.cursor = 0;
                self.scroll = 0;
                self.follow = true;
                self.state = PanelState::Thinking;
                self.acts.push(Act::Prompt(text));
                self.touch();
            }
        }
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
            for row in &mut self.rows {
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
        for (i, s) in self.sessions.iter().enumerate() {
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

/// One note: a faint bullet and its words, cleaned of anything a terminal would draw wrongly.
fn note_line(note: &str) -> Line {
    Line::new(vec![
        Span::role("· ", Role::Faint),
        Span::role(crate::agent::text::clean_line(note), Role::Faint),
    ])
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

/// The folder a conversation belongs to: the git toplevel of the focused file's directory, or of
/// the directory rano was started in. The launcher resolves a subdirectory to its repository's
/// daemon, so this states the same rule once rather than deriving a socket from a path.
pub fn folder_for(file: Option<&Path>, cwd: &Path) -> PathBuf {
    let dir = match file.and_then(Path::parent) {
        Some(d) if !d.as_os_str().is_empty() => {
            if d.is_absolute() {
                d.to_path_buf()
            } else {
                cwd.join(d)
            }
        }
        _ => cwd.to_path_buf(),
    };
    toplevel(&dir).unwrap_or(dir)
}

/// The repository a directory belongs to, when it is in one.
fn toplevel(dir: &Path) -> Option<PathBuf> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let said = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!said.is_empty()).then(|| PathBuf::from(said))
}

/// The session remembered for a folder, out of the panel's own file: `folder<TAB>session`, one per
/// line. A missing or unreadable file is simply nothing remembered.
pub fn remembered_in(file: &Path, folder: &Path) -> Option<String> {
    let text = std::fs::read_to_string(file).ok()?;
    let want = folder.to_string_lossy().into_owned();
    text.lines().find_map(|line| {
        let (fold, session) = line.split_once('\t')?;
        (fold == want).then(|| session.trim().to_string())
    })
}

/// Remember `session` for `folder`, replacing what that folder had. Written whole, because the file
/// is a few lines and a partial one would drop a folder's memory for a reason nobody can see.
pub fn remember_in(file: &Path, folder: &Path, session: &str) -> std::io::Result<()> {
    let want = folder.to_string_lossy().into_owned();
    let mut kept: Vec<String> = std::fs::read_to_string(file)
        .unwrap_or_default()
        .lines()
        .filter(|l| !l.starts_with(&format!("{want}\t")))
        .map(str::to_string)
        .collect();
    kept.push(format!("{want}\t{session}"));
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(file, kept.join("\n") + "\n")
}

impl crate::editor::Editor {
    /// `alt-i`: the side surface on or off, keeping the buffers and asking nothing.
    pub fn panel_toggle(&mut self) {
        if self.panel.open {
            self.panel.close();
        } else {
            self.panel_open();
        }
    }

    /// Show the panel and give it the keyboard, without losing a conversation it already has.
    pub fn panel_open(&mut self) {
        let folder = self.panel_folder();
        self.panel.open(folder);
    }

    /// `ctrl-]`: the keyboard to the other surface, opening the panel when it is shut.
    pub fn panel_cross(&mut self) {
        if !self.panel.open {
            self.panel_open();
            return;
        }
        self.panel.focus = !self.panel.focus;
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
        self.panel.focus = false;
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
        // The pick's letters, which a composer cannot have: nothing is typed while it is up.
        if self.panel.state == PanelState::Picking && plain {
            match key.code {
                KeyCode::Char('n') => {
                    self.panel_new();
                    return true;
                }
                KeyCode::Char(c) if c.is_ascii_digit() => {
                    let at = c.to_digit(10).unwrap_or(0) as usize;
                    if at > 0 && at <= self.panel.sessions.len() {
                        self.panel.pick = at - 1;
                    }
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

    /// The panel asks the host for a connection and nothing else: it holds no socket, so every
    /// wire action is an `Act` the host translates.
    #[test]
    fn opening_asks_the_host_to_connect_and_the_session_is_attached_from_the_list() {
        let mut panel = opened();
        assert!(panel.open && panel.focus);
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
    /// and a turn's end change the state and never the prose.
    #[test]
    fn a_streamed_reply_is_one_row_and_the_state_follows_the_turn() {
        let mut panel = opened();
        panel.apply(&hello("s-1", "the one"));
        panel.take_acts();
        panel.input = "why?".to_string();
        panel.send();
        assert_eq!(panel.take_acts(), vec![Act::Prompt("why?".to_string())]);
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

    /// A tool call is one line that ends with its outcome, so a run of calls is a list a reader can
    /// scan rather than a paragraph each.
    #[test]
    fn a_tool_call_ends_on_the_line_it_started_on() {
        let mut panel = opened();
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
                why: "no such file".to_string(),
            },
        });
        let notes: Vec<&str> = panel
            .rows
            .iter()
            .filter_map(|r| match r {
                Row::Note(n) => Some(n.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(notes, vec!["bash · failed: no such file"]);
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
        ed.panel.focus = true;
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
        assert!(!ed.panel.focus, "Esc gives the keyboard back to the text");
        assert!(ed.panel.open, "and leaves the panel showing");
    }

    /// `enter` in the composer sends, and the row it leaves is the person's own words.
    #[test]
    fn the_composers_enter_sends_what_was_typed() {
        let mut ed = Editor::new(Buffer::new(), Config::default());
        ed.panel.open = true;
        ed.panel.focus = true;
        ed.panel.state = PanelState::Idle;
        ed.panel.session = "s-1".to_string();
        for c in "ping".chars() {
            ed.handle_key(held(&KeyCode::Char(c)));
        }
        assert_eq!(ed.handle_key(held(&KeyCode::Enter)), KeyOutcome::Handled);
        assert_eq!(ed.panel.take_acts(), vec![Act::Prompt("ping".to_string())]);
        assert!(matches!(ed.panel.rows.as_slice(), [Row::User(t)] if t == "ping"));
        assert!(ed.panel.input.is_empty());
    }

    /// The composer's cursor is a character and not a byte: a letter outside ASCII is typed and
    /// deleted by the character, which a byte offset would cut in half.
    #[test]
    fn a_letter_outside_ascii_is_typed_and_deleted_by_the_character() {
        let mut ed = Editor::new(Buffer::new(), Config::default());
        ed.panel.open = true;
        ed.panel.focus = true;
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
        ed.panel.focus = true;
        ed.panel_refuse("no letibot on PATH");
        assert!(!ed.panel.open && !ed.panel.focus);
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
}
