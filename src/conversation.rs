//! The conversation's vocabulary, shared by the panel and by a build without it: the data
//! types a host or an embedder still handles (the place chips, the marks, the session memory),
//! never the pane. `panel` re-exports all of it, so `crate::panel::Chip` is one path either way.

use std::path::{Path, PathBuf};

use crate::letibot::{ContextPos, ContextRange, ContextRef, EditorContext};
#[cfg(feature = "agent")]
use crate::markdown::{IncrementalMarkdown, MarkdownView};
use crate::marks::Mark;
use crate::send::{Point, Selection, SendEvent};
use crate::style::Role;

/// What the panel is doing, in the words its top edge shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PanelState {
    /// Nothing asked for yet: the panel has never looked for a daemon.
    #[default]
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
    /// The person's words, and what they pointed at beside them. `context` is `None` when there is
    /// nothing to carry, which is what a daemon written before 71 is sent.
    Prompt {
        text: String,
        context: Option<EditorContext>,
    },
    /// The same words on the queue lane: the daemon runs them after the running turn ends.
    PromptQueued {
        text: String,
        context: Option<EditorContext>,
    },
    /// Stop the running turn.
    Interrupt,
}

/// One place waiting to ride the next prompt: the panel's chip. The file is absolute and the
/// positions are the editor's own, so nothing here has to know where the session's workspace is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chip {
    pub file: PathBuf,
    pub cursor: Point,
    pub selection: Option<Selection>,
    /// The buffer differs from the file on disk, so the text sent is the one the daemon cannot read.
    pub unsaved: bool,
    pub lang: String,
}

impl Chip {
    /// The chip a place makes, or `None` for an unnamed buffer: a mark needs a file to land on.
    pub fn of(send: &SendEvent, lang: &str) -> Option<Chip> {
        Some(Chip {
            file: send.path.clone()?,
            cursor: send.cursor,
            selection: send.selection.clone(),
            unsaved: send.modified,
            lang: lang.to_string(),
        })
    }

    /// `article.md:41-47 · unsaved`: what the composer's chip row and the user's row say.
    pub fn label(&self, folder: &Path) -> String {
        let shown = self
            .file
            .strip_prefix(folder)
            .unwrap_or(&self.file)
            .display()
            .to_string();
        let range = match &self.selection {
            Some(s) if s.start.line != s.end.line => format!(":{}-{}", s.start.line, s.end.line),
            Some(s) => format!(":{}", s.start.line),
            None => format!(":{}", self.cursor.line),
        };
        let unsaved = if self.unsaved { " · unsaved" } else { "" };
        format!("{shown}{range}{unsaved}")
    }

    /// The `ContextRef` the daemon renders: the path as an absolute one, the range in characters.
    pub fn reference(&self) -> ContextRef {
        let point = |p: Point| ContextPos {
            line: p.line as u32,
            column: p.column as u32,
        };
        ContextRef {
            file: self.file.display().to_string(),
            range: self.selection.as_ref().map(|s| ContextRange {
                start: point(s.start),
                end: point(s.end),
            }),
            cursor: Some(point(self.cursor)),
            text: self
                .selection
                .as_ref()
                .map(|s| s.text.clone())
                .unwrap_or_default(),
            unsaved: self.unsaved,
            lang: self.lang.clone(),
            title: String::new(),
        }
    }
}

/// Marks one source set on one file, handed to the host to land on a buffer: the editor holds the
/// text a mark is anchored in, and the panel holds only what the daemon said about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Markset {
    pub path: PathBuf,
    /// `reply:<item_id>` or `call:<call_id>`: who set it, so a host replaces only its own.
    pub source: String,
    /// Sources that name the same lines from an older reply, to clear first: highlights do not pile
    /// up, while hunks stay until they are decided.
    pub replaces: Vec<String>,
    pub marks: Vec<Mark>,
}

/// A reply as it is being written: the text, and the Markdown the panel renders it with while the
/// deltas land, so a frame costs the tail rather than the whole reply.
#[derive(Default)]
pub struct Reply {
    pub text: String,
    #[cfg(feature = "agent")]
    pub(crate) md: IncrementalMarkdown,
    #[cfg(feature = "agent")]
    pub(crate) view: MarkdownView,
}

impl Reply {
    #[cfg(feature = "agent")]
    pub(crate) fn push(&mut self, delta: &str) {
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
    /// Anything that is not prose: a turn's end, a refusal — one line.
    Note(String),
    /// A tool call: verb and target, its outcome and duration, its payload folded with a
    /// line count, the failure's reason wrapped rather than cut.
    #[cfg(feature = "agent")]
    Tool(Box<crate::agent::tool_row::ToolRow>),
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
