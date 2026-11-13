//! The panel, driven the way the operator drives it: the real binary on a pty, a scripted daemon on
//! the other end of a socket, and keys written into the terminal.
//!
//! What this asserts is the package's "done when": `alt-i` opens the panel, a streamed reply appears,
//! `ctrl-]` crosses the keyboard between the two surfaces, Esc gives it back to the editor, and below
//! the width limit the focused surface takes the whole frame. Nothing here reaches a daemon: the peer
//! is a `UnixListener` this test owns, answering the frames the panel sends.

use std::fs::File;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::fd::{FromRawFd, OwnedFd};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::Value;

/// Long enough that a failure to appear is the program's, short enough that a hang is not a wait.
const PATIENCE: Duration = Duration::from_secs(10);

/// The file the editor has open: its words are what tells the editor's column from the panel's.
const LINE: &str = "pack my box with five dozen liquor jugs";

/// The session the scripted daemon holds, which the panel's top edge names.
const TITLE: &str = "the scripted one";

/// A temporary folder the test owns and removes.
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> TempDir {
        // This folder holds a unix socket, and a socket's path has to fit in `SUN_LEN` (108 bytes) —
        // the kernel refuses the rest with "path must be shorter than SUN_LEN", which is what the
        // gate's landing tree did to this test. So the name is short and filesystem-safe, and when
        // `TMPDIR` is already long enough to matter the folder goes to `/tmp`, which is the one case
        // where the environment's temp folder cannot be honoured.
        let base = std::env::temp_dir();
        let base = if base.as_os_str().len() > 40 {
            PathBuf::from("/tmp")
        } else {
            base
        };
        let dir = base.join(format!("rp-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("a temporary folder");
        TempDir(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The frames the panel has sent, in order, for the assertions about what it actually said.
type Heard = Arc<Mutex<Vec<Value>>>;

/// A daemon on a socket, scripted: a list with one session, a `Hello` for the attach, and a turn of
/// three deltas for the first prompt.
fn scripted_daemon(socket: &Path, heard: Heard) -> std::thread::JoinHandle<()> {
    let listener = UnixListener::bind(socket).expect("a socket to listen on");
    std::thread::spawn(move || {
        let Ok((stream, _)) = listener.accept() else {
            return;
        };
        let mut out = stream.try_clone().expect("a writer");
        let mut lines = BufReader::new(stream).lines();
        while let Some(Ok(line)) = lines.next() {
            let Ok(frame) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            heard.lock().expect("the transcript").push(frame.clone());
            let answer: Vec<String> = match frame["frame"].as_str().unwrap_or_default() {
                "list_sessions" => vec![format!(
                    r#"{{"frame":"sessions","current":"s-1","sessions":[
                        {{"session_id":"s-1","title":"{TITLE}","created_ms":0,"live":true}}]}}"#
                )],
                "attach" => vec![format!(
                    r#"{{"frame":"hello","protocol_version":70,"session_id":"s-1","head_id":"h-1",
                        "dropped":0,"resumed_from":null,
                        "snapshot":{{"session_id":"s-1","seq":0,"dropped":0,"items_dropped":0,
                                     "items":[],"decisions":[]}},
                        "wiring":{{"model":"m"}},
                        "sessions":[{{"session_id":"s-1","title":"{TITLE}","created_ms":0,
                                      "live":true}}]}}"#
                )],
                "prompt" => vec![
                    r#"{"frame":"accepted","client_request_id":"r","seq":1,"note":"queued"}"#
                        .to_string(),
                    r#"{"frame":"event","seq":2,"event":"turn_started","turn_id":"t","model":"m"}"#
                        .to_string(),
                    r#"{"frame":"event","seq":3,"event":"delta","turn_id":"t","target":"text","text":"streamed "}"#
                        .to_string(),
                    r#"{"frame":"event","seq":4,"event":"delta","turn_id":"t","target":"text","text":"reply."}"#
                        .to_string(),
                    r#"{"frame":"event","seq":5,"event":"turn_finished","turn_id":"t","ends_turn":true}"#
                        .to_string(),
                ],
                _ => Vec::new(),
            };
            for line in answer {
                if writeln!(out, "{}", line.replace('\n', " ").trim()).is_err() {
                    return;
                }
            }
            if out.flush().is_err() {
                return;
            }
        }
    })
}

/// The binary on a pty of its own, and what it has drawn so far.
struct Ran {
    master: File,
    child: Child,
    /// Everything the terminal was told, in order: a frame's cells arrive as they change.
    seen: Arc<Mutex<String>>,
}

impl Ran {
    /// Start `rano <file>` in `cwd`, at `cols`×`rows`, on a pty.
    fn new(cwd: &Path, socket: &Path, config: &Path, cols: u16, rows: u16) -> Ran {
        let (master, slave) = openpty(cols, rows);
        let child = Command::new(env!("CARGO_BIN_EXE_rano"))
            .arg("notes.txt")
            .current_dir(cwd)
            .env("LETIBOT_SOCKET", socket)
            .env("XDG_CONFIG_HOME", config)
            .stdin(Stdio::from(slave.try_clone().expect("a standard input")))
            .stdout(Stdio::from(slave.try_clone().expect("a standard output")))
            .stderr(Stdio::from(slave))
            .spawn()
            .expect("the binary starts");
        let seen = Arc::new(Mutex::new(String::new()));
        let mut reading = master.try_clone().expect("a reader");
        let into = Arc::clone(&seen);
        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            while let Ok(n) = reading.read(&mut buf) {
                if n == 0 {
                    break;
                }
                into.lock()
                    .expect("the screen")
                    .push_str(&String::from_utf8_lossy(&buf[..n]));
            }
        });
        Ran {
            master,
            child,
            seen,
        }
    }

    /// Where the screen's text is now: what a later assertion reads only the new part of.
    fn mark(&self) -> usize {
        self.seen.lock().expect("the screen").len()
    }

    /// The rows written since `from`.
    fn since(&self, from: usize) -> String {
        let seen = self.seen.lock().expect("the screen");
        seen.get(from..).unwrap_or_default().to_string()
    }

    /// Wait until what was drawn since `from` holds `needle`.
    fn wait_for(&self, from: usize, needle: &str) -> bool {
        let deadline = Instant::now() + PATIENCE;
        while Instant::now() < deadline {
            if self.since(from).contains(needle) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        false
    }

    /// Keys into the terminal.
    fn send(&mut self, keys: &[u8]) {
        self.master.write_all(keys).expect("keys to the terminal");
        self.master.flush().expect("flushed");
    }

    /// Resize the pty, which is what a terminal emulator does when its window is dragged.
    fn resize(&mut self, cols: u16, rows: u16) {
        let ws = libc::winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        let ok = unsafe { libc::ioctl(fd_of(&self.master), libc::TIOCSWINSZ, &ws) };
        assert_eq!(ok, 0, "the pty takes a new size");
    }
}

impl Drop for Ran {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn fd_of(f: &File) -> i32 {
    use std::os::fd::AsRawFd;
    f.as_raw_fd()
}

/// A pty of `cols`×`rows`, as the terminal half and the program half.
fn openpty(cols: u16, rows: u16) -> (File, File) {
    let mut master: libc::c_int = -1;
    let mut slave: libc::c_int = -1;
    let ws = libc::winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    let ok = unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null(),
            &ws,
        )
    };
    assert_eq!(ok, 0, "a pty");
    // Owned from here: a leak per test would be a descriptor nobody closes.
    let master: OwnedFd = unsafe { OwnedFd::from_raw_fd(master) };
    let slave: OwnedFd = unsafe { OwnedFd::from_raw_fd(slave) };
    (File::from(master), File::from(slave))
}

/// The one test the package names: open, stream, cross, leave, zoom.
#[test]
fn the_panel_opens_streams_crosses_and_zooms() {
    let dir = TempDir::new("pty");
    std::fs::write(dir.path().join("notes.txt"), format!("{LINE}\n")).expect("a file to read");
    let config = TempDir::new("config");
    let socket = dir.path().join("daemon.sock");
    let heard: Heard = Arc::new(Mutex::new(Vec::new()));
    let daemon = scripted_daemon(&socket, Arc::clone(&heard));

    let mut ran = Ran::new(dir.path(), &socket, config.path(), 120, 30);
    // The editor is up and its file is on the glass before any key is sent, so the pty is in raw
    // mode and a key is a key rather than a line.
    assert!(ran.wait_for(0, "liquor"), "the editor drew its file");

    // ---- alt-i opens the panel, which attaches and names its session ----
    let from = ran.mark();
    ran.send(b"\x1bi");
    assert!(
        ran.wait_for(from, TITLE),
        "the panel opened on the session: {:?}",
        ran.since(from)
    );
    let attach = heard
        .lock()
        .expect("the transcript")
        .iter()
        .find(|f| f["frame"] == "attach")
        .cloned()
        .expect("an attach");
    assert_eq!(attach["kind"], "editor");
    assert_eq!(attach["session_id"], "s-1");
    assert_eq!(
        attach["caps"]["can_decide"].as_bool(),
        Some(false),
        "a panel that draws no card must not say it can answer one"
    );

    // ---- typing in the composer sends, and the reply streams into the rows ----
    let from = ran.mark();
    ran.send(b"ping\r");
    assert!(
        ran.wait_for(from, "reply."),
        "the streamed reply is on the glass: {:?}",
        ran.since(from)
    );
    let prompt = heard
        .lock()
        .expect("the transcript")
        .iter()
        .find(|f| f["frame"] == "prompt")
        .cloned()
        .expect("a prompt");
    assert_eq!(prompt["text"], "ping");
    assert!(
        prompt.get("client_request_id").is_some(),
        "a prompt names itself"
    );

    // ---- ctrl-] crosses: the next letter goes into the file, not the composer ----
    let from = ran.mark();
    ran.send(b"\x1d");
    ran.send(b"x");
    assert!(
        ran.wait_for(from, "xpack"),
        "the editor has the keyboard: {:?}",
        ran.since(from)
    );
    let prompts = heard
        .lock()
        .expect("the transcript")
        .iter()
        .filter(|f| f["frame"] == "prompt")
        .count();
    assert_eq!(
        prompts, 1,
        "the editor's letters were never sent as prompts"
    );

    // ---- ctrl-] back: the letters land in the composer, which is one span of its own ----
    let from = ran.mark();
    ran.send(b"\x1d");
    ran.send(b"yip");
    assert!(
        ran.wait_for(from, "yip"),
        "the composer has the keyboard: {:?}",
        ran.since(from)
    );
    assert!(
        !ran.since(from).contains("yippack") && !ran.since(from).contains("xyippack"),
        "and the file never saw it: {:?}",
        ran.since(from)
    );

    // ---- Esc gives it back: the letter after it reaches the file again ----
    let from = ran.mark();
    ran.send(b"\x1b");
    std::thread::sleep(Duration::from_millis(50));
    ran.send(b"q");
    assert!(
        ran.wait_for(from, "xqpack"),
        "Esc returned the keyboard to the editor: {:?}",
        ran.since(from)
    );

    // ---- below the width limit one surface takes the frame, and ctrl-] swaps which ----
    ran.resize(90, 30);
    let from = ran.mark();
    // The keyboard is the editor's here, so the editor takes the frame: its file, no panel.
    assert!(
        ran.wait_for(from, "liquor"),
        "the editor has the whole narrow frame: {:?}",
        ran.since(from)
    );
    let from = ran.mark();
    ran.send(b"\x1d");
    assert!(
        ran.wait_for(from, TITLE),
        "the panel has the whole narrow frame: {:?}",
        ran.since(from)
    );
    assert!(
        !ran.since(from).contains("liquor"),
        "and the editor is not drawn beside it: {:?}",
        ran.since(from)
    );

    drop(ran);
    let _ = daemon.join();
}
