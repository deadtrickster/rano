//! A fatal signal on a real rano, in a real pty: the recovery file is written, the exit is the
//! signal's own status, and the terminal is left restored rather than raw.

use std::os::unix::io::{FromRawFd, RawFd};
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// The pty pair, libc's `openpty` as the editor's own terminal uses it.
fn openpty() -> Option<(RawFd, RawFd)> {
    let mut master = 0;
    let mut slave = 0;
    let ws = libc::winsize {
        ws_row: 24,
        ws_col: 80,
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
    (ok == 0).then_some((master, slave))
}

fn tempdir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "rano_sighup_{}_{}_{:?}",
        tag,
        std::process::id(),
        Instant::now()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Read whatever the pty has for `wait` and append it: a deadline, never an iteration count.
fn drain(fd: RawFd, text: &mut String, wait: Duration) {
    let deadline = Instant::now() + wait;
    let mut buf = [0u8; 4096];
    loop {
        let n = unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) };
        if n > 0 {
            text.push_str(&String::from_utf8_lossy(&buf[..n as usize]));
        }
        if Instant::now() >= deadline {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// The child's exit status, the pty's output, and whether the pty is back to cooked mode.
struct Exit {
    status: i32,
    restored: bool,
    out: String,
}

/// Run rano on `file` in `dir`, type the marker, send `sig`, and reap with a deadline.
fn run_to_signal(dir: &PathBuf, file: Option<&PathBuf>, sig: i32) -> Option<Exit> {
    let (master, slave) = openpty()?;
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_rano"));
    if let Some(f) = file {
        cmd.arg(f);
    }
    cmd.current_dir(dir)
        .env("TERM", "xterm-256color")
        .env_remove("TMUX")
        .stdin(unsafe { Stdio::from_raw_fd(libc::dup(slave)) })
        .stdout(unsafe { Stdio::from_raw_fd(libc::dup(slave)) })
        .stderr(unsafe { Stdio::from_raw_fd(libc::dup(slave)) });
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    let child = cmd.spawn().ok()?;
    // The slave is the child's now; the parent only reads the master, and never blocks on it:
    // a blocking read would sit past every deadline the waits below are built on.
    unsafe {
        libc::close(slave);
        libc::fcntl(master, libc::F_SETFL, libc::O_NONBLOCK);
    }
    let pid = child.id() as i32;
    let mut out = String::new();
    // The WHOLE first frame before the typing: the status row is its last part, and raw mode
    // is fully on by then. Typing on the first escape bytes raced the enter sequence and the
    // line discipline flushed the keystrokes away.
    let start = Instant::now();
    while !out.contains("Ln 1, Col 1") && start.elapsed() < Duration::from_secs(30) {
        drain(master, &mut out, Duration::from_millis(50));
    }
    unsafe {
        let bytes = b"RECOVERYZZZ";
        libc::write(master, bytes.as_ptr().cast(), bytes.len());
    }
    // Let the keystrokes land; the marker on the glass is the editor's own drawing of them.
    let start = Instant::now();
    while !out.contains("RECOVERYZZZ") && start.elapsed() < Duration::from_secs(30) {
        drain(master, &mut out, Duration::from_millis(50));
    }
    assert!(
        out.contains("RECOVERYZZZ"),
        "the marker landed in the buffer before the signal: {out}"
    );
    unsafe { libc::kill(pid, sig) };
    let mut status = -1;
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(30) {
        let mut st = 0;
        if unsafe { libc::waitpid(pid, &mut st, libc::WNOHANG) } == pid {
            status = st;
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(status != -1, "rano exited within the deadline: {out}");
    // The pty's own flags say whether the restore ran: raw mode leaves ICANON and ECHO off.
    let mut tio: libc::termios = unsafe { std::mem::zeroed() };
    unsafe { libc::tcgetattr(master, &mut tio) };
    let restored = (tio.c_lflag & (libc::ICANON | libc::ECHO)) == (libc::ICANON | libc::ECHO);
    unsafe { libc::close(master) };
    Some(Exit {
        status,
        restored,
        out,
    })
}

#[test]
fn sighup_writes_the_recovery_file_and_restores_the_pty() {
    let dir = tempdir("named");
    let file = dir.join("note.txt");
    std::fs::write(&file, "base\n").unwrap();
    let Some(exit) = run_to_signal(&dir, Some(&file), libc::SIGHUP) else {
        eprintln!("no pty here: the signal test is skipped");
        return;
    };
    assert!(
        !libc::WIFSIGNALED(exit.status),
        "the handler ran and exited itself, not the signal's death"
    );
    assert_eq!(libc::WEXITSTATUS(exit.status), 128 + libc::SIGHUP);
    assert!(exit.restored, "the terminal is not left raw");
    let recovery = dir.join("note.txt.save");
    let text = std::fs::read_to_string(&recovery).unwrap_or_else(|e| {
        panic!(
            "{} was written: {e}; screen: {}",
            recovery.display(),
            exit.out
        )
    });
    assert!(
        text.contains("RECOVERYZZZ") && text.contains("base"),
        "the buffer's own text, typed prefix and all: {text:?}"
    );
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        "base\n",
        "a recovery is not a save"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn sigterm_recovers_an_unnamed_buffer_to_rano_save() {
    let dir = tempdir("unnamed");
    let Some(exit) = run_to_signal(&dir, None, libc::SIGTERM) else {
        eprintln!("no pty here: the signal test is skipped");
        return;
    };
    assert_eq!(libc::WEXITSTATUS(exit.status), 128 + libc::SIGTERM);
    let recovery = dir.join("rano.save");
    let text = std::fs::read_to_string(&recovery).unwrap_or_else(|e| {
        panic!(
            "{} was written: {e}; screen: {}",
            recovery.display(),
            exit.out
        )
    });
    assert!(text.contains("RECOVERYZZZ"), "{text:?}");
    let _ = std::fs::remove_dir_all(&dir);
}
