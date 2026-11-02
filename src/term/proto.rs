//! Which picture protocol the terminal on the other end speaks, and what to do about the
//! multiplexer in front of it.
//!
//! Emitting the kitty protocol to a terminal that does not understand it is not a harmless
//! no-op: the bytes are typed at the operator's prompt. So the answer is decided once, here.

use std::time::Duration;

/// The kitty protocol's own capability question: a 1×1 RGB image, transferred but not
/// displayed, under [`QUERY_ID`]. A terminal that speaks it answers `OK`; one that does not
/// consumes the bytes silently.
pub const KITTY_QUERY: &[u8] = b"\x1b_Gi=107,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\";

/// The id [`KITTY_QUERY`] asks under, so a reply can be told from anything else on the wire.
pub const QUERY_ID: u32 = 107;

/// How long a capability query waits: a fifth of a second is enough, and longer is a stall at
/// startup.
pub const QUERY_TIMEOUT: Duration = Duration::from_millis(200);

/// How a picture can be put on the glass.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Proto {
    /// Nothing: the caller draws its own fallback (a name, a size, a chord to open it).
    #[default]
    None,
    /// The kitty graphics protocol, written straight to the terminal.
    Kitty,
    /// The kitty graphics protocol, DCS-wrapped for tmux (`allow-passthrough` is on).
    KittyPassthrough,
    /// iTerm2's `OSC 1337;File=`: drawn at the cursor, into cells, with no id to place it by.
    Iterm,
}

/// The environment detection reads, as facts rather than a `std::env` call per question, so the
/// decision can be tested for every terminal.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Env {
    /// `TERM`, verbatim.
    pub term: String,
    /// `TERM_PROGRAM`. Not forwarded over `ssh`, which is why `TERM` is asked first.
    pub term_program: String,
    /// `LC_TERMINAL` — iTerm2 sets it, and it survives a `TERM` that says `xterm-256color`.
    pub lc_terminal: String,
    /// `KITTY_WINDOW_ID` is set: the kitty family says so even behind a conservative `TERM`.
    pub kitty_window: bool,
    /// `TMUX` is set: there is a multiplexer between this program and the terminal.
    pub tmux: bool,
    /// What tmux said about `allow-passthrough`, when it was asked — `None` when nobody asked.
    pub passthrough: Option<bool>,
}

impl Env {
    /// This process's environment: four variables and two flags, and no question to anything
    /// outside the process.
    pub fn now() -> Env {
        let var = |k: &str| std::env::var(k).unwrap_or_default();
        Env {
            term: var("TERM"),
            term_program: var("TERM_PROGRAM"),
            lc_terminal: var("LC_TERMINAL"),
            kitty_window: !var("KITTY_WINDOW_ID").is_empty(),
            tmux: !var("TMUX").is_empty(),
            passthrough: None,
        }
    }

    /// Ask tmux whether it will pass a DCS sequence through, and record the answer. Worth only
    /// inside tmux: it costs a process and a round trip to a busy server. `on` and `all` both
    /// forward a wrapped sequence; anything else is a no.
    pub fn ask_passthrough(&mut self) {
        if !self.tmux {
            return;
        }
        let said = std::process::Command::new("tmux")
            .args(["show-options", "-gv", "allow-passthrough"])
            .output()
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());
        self.passthrough = match said.as_deref() {
            Some("on") | Some("all") => Some(true),
            Some(_) => Some(false),
            None => None,
        };
    }
}

/// The terminal's family, by the names it goes by: a pure function of two strings so a test can
/// walk the whole list.
fn kitty_family(env: &Env) -> bool {
    let prog = env.term_program.to_ascii_lowercase();
    env.kitty_window
        || env.term.contains("kitty")
        || env.term.contains("ghostty")
        || prog == "ghostty"
        || prog == "wezterm"
        || prog == "kitty"
        // WezTerm sets both; the lower-case check above already covers it, and this is the
        // spelling its own docs use.
        || prog == "rio"
}

/// iTerm2, by either name it leaves behind.
fn iterm_family(env: &Env) -> bool {
    let prog = env.term_program.to_ascii_lowercase();
    prog == "iterm.app" || env.lc_terminal.to_ascii_lowercase().contains("iterm")
}

impl Proto {
    /// Does this terminal take a picture at all — the one question a renderer usually has.
    pub fn speaks(self) -> bool {
        self != Proto::None
    }

    /// The decision, from the environment and the operator's `images` override.
    ///
    /// `images` false is the override saying no, and it is answered first.
    pub fn of(env: &Env, images: bool) -> Proto {
        if !images {
            return Proto::None;
        }
        if env.tmux {
            // One protocol crosses tmux, and only with its permission: everything else is
            // swallowed, which is a picture missing with no error anywhere.
            return match (kitty_family(env), env.passthrough) {
                (true, Some(true)) => Proto::KittyPassthrough,
                _ => Proto::None,
            };
        }
        if kitty_family(env) {
            return Proto::Kitty;
        }
        if iterm_family(env) {
            return Proto::Iterm;
        }
        Proto::None
    }

    /// [`Proto::of`], and then the capability query where the names said nothing. Asked only
    /// when it can change the answer.
    pub fn detect(env: &Env, images: bool) -> Proto {
        let named = Proto::of(env, images);
        if named.speaks() || !images {
            return named;
        }
        if iterm_family(env) {
            // Named after all, just not by the first rung — nothing to ask.
            return named;
        }
        match ask_kitty(QUERY_TIMEOUT).answered {
            Some(true) if env.tmux => match env.passthrough {
                Some(true) => Proto::KittyPassthrough,
                _ => Proto::None,
            },
            Some(true) => Proto::Kitty,
            _ => Proto::None,
        }
    }

    /// A sequence as this terminal must receive it: unchanged for a direct kitty or iTerm2
    /// terminal, DCS-wrapped for tmux, dropped for [`Proto::None`] — a byte written to a terminal
    /// that cannot read it is text at the prompt.
    ///
    /// The wrapper doubles every `ESC`, which keeps the sequence's own from ending it early.
    pub fn wrap(self, seq: &[u8]) -> Vec<u8> {
        match self {
            Proto::None => Vec::new(),
            Proto::Kitty | Proto::Iterm => seq.to_vec(),
            Proto::KittyPassthrough => {
                let mut out = Vec::with_capacity(seq.len() + 16);
                out.extend_from_slice(b"\x1bPtmux;");
                for b in seq {
                    if *b == 0x1b {
                        out.push(0x1b);
                    }
                    out.push(*b);
                }
                out.extend_from_slice(b"\x1b\\");
                out
            }
        }
    }
}

/// What a capability query came back with: `answered` is the verdict, and `other` is every byte
/// read that was not the reply — the caller's input, which must reach its decoder.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Reply {
    pub answered: Option<bool>,
    pub other: Vec<u8>,
}

/// Ask the terminal whether it speaks the kitty graphics protocol, for at most `timeout`. The
/// question goes to stdout and the answer comes back on stdin, so this only works in raw mode
/// before a host starts reading — which is why it is the last rung.
pub fn ask_kitty(timeout: Duration) -> Reply {
    let mut reply = Reply::default();
    if !write_out(KITTY_QUERY) {
        return reply;
    }
    let deadline = std::time::Instant::now() + timeout;
    let mut seen: Vec<u8> = Vec::new();
    loop {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        if left.is_zero() {
            break;
        }
        match read_in(left) {
            Some(chunk) if !chunk.is_empty() => {
                seen.extend_from_slice(&chunk);
                if let Some(answer) = kitty_answered(&seen) {
                    reply.answered = Some(answer);
                    break;
                }
            }
            // A closed stdin, or nothing within the timeout.
            _ => break,
        }
    }
    // Whatever is left after the reply (or everything, when there was none) belongs to the
    // caller's decoder.
    reply.other = match kitty_reply_span(&seen) {
        Some((_, end)) => seen[end..].to_vec(),
        None => seen,
    };
    reply
}

/// What the terminal said, out of the bytes read so far: `Some(true)` for `OK`, `Some(false)` for
/// a named refusal, `None` while the answer has not arrived.
pub fn kitty_answered(bytes: &[u8]) -> Option<bool> {
    let (what, _) = kitty_reply_span(bytes)?;
    Some(what)
}

/// The reply's own words and the byte just past it — `(true, end)` for `\x1b_Gi=107;OK\x1b\\`.
fn kitty_reply_span(bytes: &[u8]) -> Option<(bool, usize)> {
    let head = format!("\x1b_Gi={QUERY_ID};");
    let at = find(bytes, head.as_bytes())?;
    let body = at + head.len();
    let end = find(&bytes[body..], b"\x1b\\").map(|e| body + e)?;
    let said = &bytes[body..end];
    let ok = said.starts_with(b"OK");
    Some((ok, end + 2))
}

/// `needle` in `hay`, at or after the start.
fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || hay.len() < needle.len() {
        return None;
    }
    (0..=hay.len() - needle.len()).find(|i| &hay[*i..*i + needle.len()] == needle)
}

fn write_out(bytes: &[u8]) -> bool {
    use std::io::Write;
    let mut out = std::io::stdout();
    out.write_all(bytes).is_ok() && out.flush().is_ok()
}

/// One read from stdin, waiting at most `budget`: `None` on a timeout or an error, and in either
/// case nothing more is coming.
fn read_in(budget: Duration) -> Option<Vec<u8>> {
    let mut poll = libc::pollfd {
        fd: libc::STDIN_FILENO,
        events: libc::POLLIN,
        revents: 0,
    };
    let ms = budget.as_millis().min(i32::MAX as u128) as i32;
    // SAFETY: one `pollfd`, and the timeout is a plain int.
    let ready = unsafe { libc::poll(&mut poll, 1, ms) };
    if ready <= 0 {
        return None;
    }
    let mut buf = [0u8; 256];
    // SAFETY: `buf` is this function's own memory, of the length given.
    let n = unsafe { libc::read(libc::STDIN_FILENO, buf.as_mut_ptr().cast(), buf.len()) };
    if n <= 0 {
        return None;
    }
    Some(buf[..n as usize].to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(term: &str, prog: &str) -> Env {
        Env {
            term: term.into(),
            term_program: prog.into(),
            ..Env::default()
        }
    }

    /// The names, on every terminal worth knowing, and the ones this file declines to guess about.
    #[test]
    fn the_families_are_the_names_they_go_by() {
        for (term, prog) in [
            ("xterm-ghostty", ""),
            ("xterm-ghostty", "ghostty"),
            ("xterm-kitty", ""),
            ("xterm-256color", "Ghostty"),
            ("xterm-256color", "WezTerm"),
            ("screen-256color", "rio"),
        ] {
            assert_eq!(
                Proto::of(&env(term, prog), true),
                Proto::Kitty,
                "{term} {prog}"
            );
        }
        // The kitty family's own statement, behind a `TERM` that says otherwise.
        let mut e = env("xterm-256color", "");
        e.kitty_window = true;
        assert_eq!(Proto::of(&e, true), Proto::Kitty);
        // iTerm2, by either name.
        assert_eq!(
            Proto::of(&env("xterm-256color", "iTerm.app"), true),
            Proto::Iterm
        );
        let mut e = env("xterm-256color", "");
        e.lc_terminal = "iTerm2".into();
        assert_eq!(Proto::of(&e, true), Proto::Iterm);
        // And nothing else — Terminal.app, an unknown terminal, a plain `screen`.
        for (term, prog) in [
            ("xterm-256color", "Apple_Terminal"),
            ("xterm-256color", ""),
            ("screen-256color", ""),
            ("dumb", ""),
        ] {
            assert_eq!(
                Proto::of(&env(term, prog), true),
                Proto::None,
                "{term} {prog}"
            );
        }
        // The override that says no is answered before any name is read.
        for (term, prog) in [("xterm-ghostty", ""), ("xterm-256color", "iTerm.app")] {
            assert_eq!(
                Proto::of(&env(term, prog), false),
                Proto::None,
                "{term} {prog} with -images"
            );
        }
    }

    /// Inside tmux one protocol crosses, and only with `allow-passthrough`.
    #[test]
    fn tmux_takes_kitty_only_with_passthrough_and_iterm_never() {
        let mut e = env("xterm-ghostty", "ghostty");
        e.tmux = true;
        // Nobody asked tmux: the honest answer is nothing.
        assert_eq!(Proto::of(&e, true), Proto::None);
        e.passthrough = Some(false);
        assert_eq!(Proto::of(&e, true), Proto::None);
        e.passthrough = Some(true);
        assert_eq!(Proto::of(&e, true), Proto::KittyPassthrough);
        // iTerm2's protocol has no passthrough: inside tmux the answer is none, whatever tmux said.
        let mut i = env("xterm-256color", "iTerm.app");
        i.tmux = true;
        i.passthrough = Some(true);
        assert_eq!(Proto::of(&i, true), Proto::None);
        // And outside tmux the setting is not consulted at all.
        let mut d = env("xterm-ghostty", "");
        d.passthrough = Some(true);
        assert_eq!(Proto::of(&d, true), Proto::Kitty);
    }

    /// The wrapper is tmux's, byte for byte: an un-doubled inner `ESC` would end it early.
    #[test]
    fn a_wrapped_sequence_doubles_every_escape_inside_it() {
        let seq = b"\x1b_Ga=t,i=9;AAAA\x1b\\";
        let wrapped = Proto::KittyPassthrough.wrap(seq);
        assert!(wrapped.starts_with(b"\x1bPtmux;"));
        assert!(wrapped.ends_with(b"\x1b\\"));
        let inner = &wrapped[7..wrapped.len() - 2];
        assert_eq!(inner, b"\x1b\x1b_Ga=t,i=9;AAAA\x1b\x1b\\");
        // The escape count: two in the sequence, four inside the wrapper, two for the wrapper.
        assert_eq!(wrapped.iter().filter(|b| **b == 0x1b).count(), 6);
        // A direct terminal gets the bytes as they are; a terminal that cannot read them gets
        // nothing at all rather than text typed at the prompt.
        assert_eq!(Proto::Kitty.wrap(seq), seq);
        assert_eq!(Proto::Iterm.wrap(seq), seq);
        assert!(Proto::None.wrap(seq).is_empty());
        // A wrapped sequence un-doubles back to the original — the property tmux's own parser
        // applies, checked here so a change to `wrap` cannot quietly break it.
        let unwrapped: Vec<u8> = inner
            .iter()
            .enumerate()
            .filter(|(i, b)| !(**b == 0x1b && inner.get(i + 1) == Some(&0x1b)))
            .map(|(_, b)| *b)
            .collect();
        assert_eq!(unwrapped, seq);
    }

    /// The reply parser, on the shapes a terminal really sends.
    #[test]
    fn a_reply_is_ok_or_a_named_refusal_and_anything_else_is_not_an_answer() {
        assert_eq!(kitty_answered(b"\x1b_Gi=107;OK\x1b\\"), Some(true));
        assert_eq!(
            kitty_answered(b"\x1b_Gi=107;ENOTSUP:...\x1b\\"),
            Some(false)
        );
        // Somebody else's id, and a keystroke: no answer, which the caller reads as "no".
        assert_eq!(kitty_answered(b"\x1b_Gi=9;OK\x1b\\"), None);
        assert_eq!(kitty_answered(b"a"), None);
        assert_eq!(kitty_answered(b""), None);
        // The reply recognised before its terminator has arrived is not yet an answer.
        assert_eq!(kitty_answered(b"\x1b_Gi=107;OK"), None);
        // And a keystroke typed after the answer is the caller's to keep, not the reply's.
        let (what, end) = kitty_reply_span(b"\x1b_Gi=107;OK\x1b\\hi").unwrap();
        assert!(what);
        assert_eq!(&b"\x1b_Gi=107;OK\x1b\\hi"[end..], b"hi");
    }
}
