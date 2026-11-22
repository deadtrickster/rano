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

/// The recording's printable text with escape sequences dropped, its tail only: what a failure
/// needs to say is whether the preview opened.
fn visible(raw: &str) -> String {
    let mut out = String::new();
    let mut chars = raw.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            match chars.next() {
                Some('[') => {
                    while let Some(&n) = chars.peek() {
                        chars.next();
                        if ('@'..='~').contains(&n) {
                            break;
                        }
                    }
                }
                Some(']' | '_' | 'P') => {
                    while let Some(n) = chars.next() {
                        if n == '\x07' || (n == '\x1b' && chars.next_if_eq(&'\\').is_some()) {
                            break;
                        }
                    }
                }
                _ => {}
            }
        } else if !c.is_control() || c == ' ' {
            out.push(c);
        }
    }
    let squeezed: String = out.split_whitespace().collect::<Vec<_>>().join(" ");
    let tail = squeezed.chars().count().saturating_sub(600);
    squeezed.chars().skip(tail).collect()
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
    // tmux checks TERM against terminfo when it starts, and the gate reded for hours on
    // "missing or unsuitable terminal: xterm-ghostty": the entry is on some boxes and not
    // others. The test carries its own: a copy of xterm-256color under the name the recipe
    // uses, compiled into the test's temp dir, with TERMINFO pointed at it. Whatever the
    // host has installed, the private server starts.
    let terminfo = dir.join("terminfo");
    let src = dir.join("ghostty.src");
    let _ = std::fs::write(
        &src,
        String::from_utf8_lossy(
            &Command::new("sh")
                .arg("-c")
                .arg("infocmp -x tmux-direct 2>/dev/null || infocmp -x xterm-direct 2>/dev/null || infocmp -x xterm-256color")
                .output()
                .expect("infocmp")
                .stdout,
        )
        .into_owned(),
    );
    // Rename whatever source answered to the name the recipe uses. tmux-direct carries
    // Smulx/Setulc — styled underlines — without which the placement's 24-bit underline
    // is silently dropped no matter what the colours say.
    let src_text = std::fs::read_to_string(&src).unwrap_or_default();
    let _ = std::fs::write(
        &src,
        src_text
            .replace("tmux-direct", "xterm-ghostty")
            .replace("xterm-direct", "xterm-ghostty")
            .replace("xterm-256color", "xterm-ghostty"),
    );
    let _ = Command::new("tic")
        .args(["-x", "-o"])
        .arg(&terminfo)
        .arg(&src)
        .status();
    // The recipe cc@rano used: a private server whose client is recorded by `script`.
    let inner = format!(
        "stty rows 40 cols 120; tmux -L {sock} -f {} new-session -s x '{bin} README.md'",
        conf.display()
    );
    let mut child = Command::new("script")
        .args(["-qfc", &inner, raw.to_str().expect("a path")])
        .env_remove("TMUX")
        // The terminfo the test compiled itself: the recipe's terminal name works on every
        // box, without depending on the host having ghostty's entry installed.
        .env("TERMINFO", terminfo.display().to_string())
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
        // The refusal said only "never came up"; the gate reded three times on it without
        // a word of WHY. tmux's own stderr is the diagnosis — a socket problem, a config
        // problem, a permissions problem each read differently — so it rides the panic.
        let said = Command::new("tmux")
            .args(["-L", &sock, "list-sessions"])
            .stderr(Stdio::piped())
            .output();
        let why = match said {
            Ok(o) => String::from_utf8_lossy(&o.stderr).trim().to_string(),
            Err(e) => format!("tmux would not even run: {e}"),
        };
        let recorded = std::fs::read(&raw)
            .map(|b| String::from_utf8_lossy(&b).trim().to_string())
            .unwrap_or_default();
        panic!(
            "the private tmux server never came up: {why}{}",
            if recorded.is_empty() {
                String::new()
            } else {
                format!(
                    "; the recording holds: {}",
                    &recorded[..recorded.len().min(400)]
                )
            }
        );
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
    // M-p as one key: `Escape` and `p` are two writes, and a lone ESC read between them is Esc.
    std::thread::sleep(Duration::from_millis(500));
    let _ = Command::new("tmux")
        .args(["-L", &sock, "send-keys", "-t", "x", "M-p"])
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
        "the upload reached the client; {} bytes recorded, the screen read: {}",
        log.len(),
        visible(&text)
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
