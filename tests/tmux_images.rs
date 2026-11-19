//! The tmux path end to end: a private tmux server with a recorded client, a real rano drawing
//! `screenshot.png` through README's preview, and the client's own stream as the evidence.
//!
//! Skipped when tmux or `script` is not installed: the recipe needs a multiplexer and a pty.

use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn have(prog: &str, arg: &str) -> bool {
    Command::new(prog)
        .arg(arg)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Poll until `ok()` or the deadline; true when it answered.
fn until(deadline: Duration, mut ok: impl FnMut() -> bool) -> bool {
    let start = Instant::now();
    loop {
        if ok() {
            return true;
        }
        if start.elapsed() > deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn a_picture_reaches_a_recording_tmux_client() {
    if !have("tmux", "-V") || !have("script", "--version") {
        eprintln!("skipped: tmux or script is not installed");
        return;
    }
    let dir = std::env::temp_dir().join(format!("rano-tmux-image-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a temp dir");
    let conf = dir.join("conf");
    std::fs::write(
        &conf,
        "set -g allow-passthrough on\nset -as terminal-features 'xterm-ghostty:RGB:sync:clipboard'\n",
    )
    .expect("a conf");
    let raw = dir.join("raw.log");
    let sock = format!("rano-t{}", std::process::id());
    let bin = env!("CARGO_BIN_EXE_rano");
    // The recipe cc@rano used: a private server whose client is recorded by `script`.
    let inner = format!(
        "stty rows 40 cols 120; tmux -L {sock} -f {} new-session -s x '{bin} README.md'",
        conf.display()
    );
    let mut child = Command::new("script")
        .args(["-qfc", &inner, raw.to_str().expect("a path")])
        .env_remove("TMUX")
        .env("TERM", "xterm-ghostty")
        // Nothing of the private server's tree may hold this process's stdout: a surviving
        // tmux or rano would keep `cargo test`'s pipe open and hang the run after the test ends.
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("script");
    // 60 s, not 20: the gate runs this beside a whole parallel test step, and on that
    // load a private tmux server can take past 20 s to answer — the tip went red twice
    // on exactly that deadline. The test's own frame wait below says the same thing:
    // "on a loaded machine it starts slower than any fixed sleep".
    let up = until(Duration::from_secs(60), || {
        Command::new("tmux")
            .args(["-L", &sock, "list-sessions"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    });
    if !up {
        let _ = child.kill();
        let _ = child.wait();
        panic!("the private tmux server never came up");
    }
    // Wait until rano has drawn its frame before sending a key: on a loaded machine it starts
    // slower than any fixed sleep, and a chord sent into a starting editor is a chord lost.
    let drawn = until(Duration::from_secs(60), || {
        std::fs::read(&raw)
            // rano's own status row: contiguous bytes, where the key bar's text is split by the
            // reverse-video sequences around each key.
            .map(|b| b.windows(11).any(|w| w == b"Ln 1, Col 1"))
            .unwrap_or(false)
    });
    assert!(drawn, "rano drew its frame before the key was sent");
    // M-p, which is Escape then p.
    std::thread::sleep(Duration::from_millis(500));
    let _ = Command::new("tmux")
        .args(["-L", &sock, "send-keys", "-t", "x", "Escape", "p"])
        .status();
    // Wait for the client's stream to carry the picture, then stop the server.
    // Best effort: the recording is flushed when `script` exits, so the read below is what
    // decides; this only shortens the wait when the stream does arrive early.
    let _seen = until(Duration::from_secs(20), || {
        std::fs::read(&raw)
            .map(|b| {
                b.windows(8).any(|w| w == b"\x1b_Ga=t")
                    && b.windows(11).any(|w| w == b"\x1b_Ga=p,U=1")
            })
            .unwrap_or(false)
    });
    let _ = Command::new("tmux")
        .args(["-L", &sock, "kill-server"])
        .status();
    // `script` ends when the pty it records closes; rano holds that pty, so give it a moment and
    // then end the recorder rather than waiting on a server that may already be gone.
    let ended = until(Duration::from_secs(5), || {
        child.try_wait().map(|s| s.is_some()).unwrap_or(true)
    });
    if !ended {
        let _ = child.kill();
    }
    let _ = child.wait();
    let mut log = Vec::new();
    if let Ok(mut f) = std::fs::File::open(&raw) {
        let _ = f.read_to_end(&mut log);
    }
    let text = String::from_utf8_lossy(&log);
    assert!(
        text.contains("\x1b_Ga=t"),
        "the upload reached the client; {} bytes recorded",
        log.len()
    );
    assert!(text.contains("\x1b_Ga=p,U=1"), "the placement reached it");
    assert!(
        text.contains("58:2::0:0:1") || text.contains("58;2;0;0;1"),
        "the placeholder rows name the placement in 24 bits, not a palette index"
    );
    assert!(
        !text.contains("58;5;1m"),
        "and never as a palette index tmux would rewrite"
    );
}
