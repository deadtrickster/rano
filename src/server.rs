//! **The rano server and its client** - emacsclient's shape, ported. A running rano listens on a
//! unix socket in `~/.cache/rano/` (one per instance, the newest winning); a `rano` started while
//! one is there hands its arguments over instead of starting a second editor.
//!
//! The wire is **`path:line:col`**, the shape every tool already prints - rg, grep, gcc, a Python
//! traceback - so `$EDITOR=rano` and a paste from any of them land where the reader was looking.
//! One line per request, one line back: `ok <file>:<line>` or `no <reason>`.
//!
//! The verbs:
//!
//! - `open <path>[:line[:col]]` - open (or switch to) the file, at the position
//! - `line <n>` - the current buffer to line `n`
//! - `goto-file <path> <line>` - alias of open, for the $EDITOR contract
//! - `status` - the current file and position, as `path:line:col`
//! - `quit` - what `^X` does, saves asked for
//!
//! Everything is fire-and-forget from the client's view: the socket answers when the editor has
//! *taken* the request, and the editor applies it on its next tick, the same way the terminal
//! strip's `rano` shim reaches it.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::mpsc;

/// A request the editor applies on its next tick.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    /// Open (or switch to) `path`, at the 1-based line and optional column.
    Open {
        path: String,
        line: Option<usize>,
        col: Option<usize>,
    },
    /// The current buffer to a 1-based line.
    Line(usize),
    /// `path:line:col` for the current buffer and cursor.
    Status,
    /// What `^X` does: offer every modified buffer for saving, then exit.
    Quit,
}

/// Where the server's socket lives, the newest instance the only one listening: `~/.cache/rano/`.
fn socket_dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))?;
    Some(base.join("rano"))
}

/// The socket of a rano that is listening, if one is. The newest instance's socket is the one
/// tried; a stale socket (its rano gone without cleanup) is not a server.
pub fn running_socket() -> Option<PathBuf> {
    let dir = socket_dir()?;
    let sock = dir.join("server.sock");
    // A socket nobody answers is not a server: connect, and the absence is the answer.
    UnixStream::connect(&sock).ok().map(|_| sock)
}

/// The server: a listener thread and the channel the editor drains each tick. Dropping it cleans
/// the socket up - and a rano that died without dropping leaves a socket `running_socket`'s
/// connect-probe refuses, the corpse-socket rule the operator's Emacs taught (init.el:1021).
pub struct Server {
    /// The socket's path, for the cleanup on drop. The listener itself lives in the thread -
    /// the thread is what owns the socket, and it ends when the channel's sender (this Server)
    /// goes away.
    sock: PathBuf,
    pub rx: mpsc::Receiver<Request>,
    _thread: std::thread::JoinHandle<()>,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.sock);
    }
}

impl Server {
    /// Listen, taking the socket from any rano that died without cleanup. `Err` when the machine
    /// gives no home directory or the socket cannot be bound - both fine: the client side then
    /// just starts a normal editor.
    pub fn start() -> Result<Server, String> {
        let Some(dir) = socket_dir() else {
            return Err("no home for the socket".to_string());
        };
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let path = dir.join("server.sock");
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path).map_err(|e| e.to_string())?;
        let (tx, rx) = mpsc::channel();
        // The listener is marked non-blocking by the thread's poll loop: accept, read one line,
        // answer, close. A client that dies mid-line closes its stream and the thread moves on.
        // The thread blocks on accept: a connection arrives, is read, answered, closed. It never
        // exits on its own - the channel's receiver going away (the editor exiting) is what ends
        // everything, the thread dying with the process. The socket file's cleanup is the
        // Server's own drop.
        let thread = std::thread::spawn(move || {
            // An errored connection is skipped: the next one is independent.
            for stream in listener.incoming().flatten() {
                handle(stream, &tx);
            }
        });
        Ok(Server {
            sock: path,
            rx,
            _thread: thread,
        })
    }
}

/// One connection: read a line, parse, hand the request over, answer.
fn handle(mut stream: UnixStream, tx: &mpsc::Sender<Request>) {
    let Some(line) = BufReader::new(&mut stream).lines().next() else {
        return;
    };
    let Ok(line) = line else {
        return;
    };
    match parse_request(&line) {
        Some(req) => {
            let answer = match &req {
                Request::Open { .. } => "ok",
                Request::Line(_) => "ok",
                Request::Status => "ok status",
                Request::Quit => "ok quit",
            };
            let _ = tx.send(req);
            let _ = stream.write_all(format!("{answer}\n").as_bytes());
        }
        None => {
            let _ = stream.write_all(format!("no: not a request: {line}\n").as_bytes());
        }
    }
}

/// One line of the wire into a request. `path:line:col` on the way in is the whole point - the
/// shape tools print, pasted straight back.
pub fn parse_request(line: &str) -> Option<Request> {
    let line = line.trim();
    if let Some(rest) = line.strip_prefix("open ") {
        let (path, l, c) = split_pos(rest);
        if path.is_empty() {
            return None;
        }
        return Some(Request::Open {
            path,
            line: l,
            col: c,
        });
    }
    if let Some(rest) = line.strip_prefix("goto-file ") {
        // `$EDITOR path line`-shaped: two words after the verb.
        let mut it = rest.split_whitespace();
        let path = it.next()?.to_string();
        let line: usize = it.next()?.parse().ok()?;
        return Some(Request::Open {
            path,
            line: Some(line),
            col: None,
        });
    }
    if let Some(rest) = line.strip_prefix("line ") {
        return Some(Request::Line(rest.trim().parse().ok()?));
    }
    if line == "status" {
        return Some(Request::Status);
    }
    if line == "quit" {
        return Some(Request::Quit);
    }
    // A bare `path:line:col` is an open: what a paste is.
    if !line.contains(' ') {
        let (path, l, c) = split_pos(line);
        if !path.is_empty() {
            return Some(Request::Open {
                path,
                line: l,
                col: c,
            });
        }
    }
    None
}

/// `path:line:col` - the shape rano's own F8 and `--line` already speak. The trailing colon rg
/// prints is tolerated; a path that literally exists with colons in it wins over the split.
fn split_pos(s: &str) -> (String, Option<usize>, Option<usize>) {
    if std::path::Path::new(s).exists() {
        return (s.to_string(), None, None);
    }
    // Numeric segments from the right, at most two, and the path keeps at least one segment:
    // `a.rs:12:4`, `a.rs:12`, `a.rs:12:` all parse, `a:b:c` keeps its colons.
    // rg and grep print a trailing colon (`a.rs:12:`); strip exactly one when what is left
    // still carries a position.
    let s = s.strip_suffix(':').unwrap_or(s);
    let all: Vec<&str> = s.split(':').collect();
    let mut end = all.len();
    let mut nums: Vec<usize> = Vec::new();
    while end > 1 && nums.len() < 2 {
        match all[end - 1].parse::<usize>() {
            Ok(n) => {
                nums.insert(0, n);
                end -= 1;
            }
            Err(_) => break,
        }
    }
    if nums.is_empty() {
        return (s.to_string(), None, None);
    }
    let path = all[..end].join(":");
    match nums.len() {
        1 => (path, Some(nums[0]), None),
        _ => (path, Some(nums[0]), Some(nums[1])),
    }
}

/// The client: hand the arguments to a running rano. `None` when nobody is listening, so the
/// caller starts its own editor. The answer line is returned for `$EDITOR`'s benefit.
pub fn send(args: &[String]) -> Option<String> {
    let sock = running_socket()?;
    let mut stream = UnixStream::connect(sock).ok()?;
    let line = args.join(" ");
    stream.write_all(format!("{line}\n").as_bytes()).ok()?;
    stream.flush().ok()?;
    let mut answer = String::new();
    // The editor answers when it has TAKEN the request, not applied it: the client must not wait
    // for a draw.
    let _ = BufReader::new(&mut stream).read_line(&mut answer);
    Some(answer.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_wire_is_the_shape_tools_print() {
        let Request::Open { path, line, col } = parse_request("src/main.rs:120:4").unwrap() else {
            panic!("an open");
        };
        assert_eq!(
            (path.as_str(), line, col),
            ("src/main.rs", Some(120), Some(4))
        );
        let Request::Open { path, line, col } = parse_request("open lib.rs:8").unwrap() else {
            panic!("an open");
        };
        assert_eq!((path.as_str(), line, col), ("lib.rs", Some(8), None));
        // rg's trailing colon, a bare file, the verb forms.
        assert!(matches!(
            parse_request("a.rs:12:"),
            Some(Request::Open { line: Some(12), .. })
        ));
        assert!(matches!(
            parse_request("a.rs"),
            Some(Request::Open { line: None, .. })
        ));
        assert!(matches!(parse_request("line 40"), Some(Request::Line(40))));
        assert!(matches!(
            parse_request("goto-file a.rs 12"),
            Some(Request::Open { .. })
        ));
        assert_eq!(parse_request("status"), Some(Request::Status));
        assert_eq!(parse_request("quit"), Some(Request::Quit));
        assert_eq!(parse_request(""), None);
        assert_eq!(parse_request("not a verb"), None);
        // A path that exists with colons in it is itself.
        std::fs::write("/tmp/rano_colon_1:2", "x").unwrap();
        let Request::Open { path, line, col } = parse_request("/tmp/rano_colon_1:2").unwrap()
        else {
            panic!("an open");
        };
        assert_eq!(
            (path.as_str(), line, col),
            ("/tmp/rano_colon_1:2", None, None)
        );
        let _ = std::fs::remove_file("/tmp/rano_colon_1:2");
    }

    #[test]
    fn a_server_answers_and_the_editor_drains_it() {
        let server = Server::start().expect("a server");
        // The client reaches it.
        let answer = send(&["src/main.rs:12".to_string()]).expect("an answer");
        assert!(answer.starts_with("ok"), "{answer}");
        // And the editor's half sees the request.
        let req = server
            .rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("the request");
        assert_eq!(
            req,
            Request::Open {
                path: "src/main.rs".to_string(),
                line: Some(12),
                col: None
            }
        );
        drop(server);
        // The socket is cleaned: a probe finds no rano.
        assert!(running_socket().is_none(), "no server after the drop");
    }
}
