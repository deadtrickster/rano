//! **Which picture protocol the terminal on the other end speaks**, and what to do about the
//! multiplexer in front of it.
//!
//! [`crate::term::graphics`] writes the kitty protocol; it says nothing about whether the terminal
//! on the other end will understand a byte of it. Emitting it to a terminal that does not is not a
//! harmless no-op: the bytes are typed at the prompt the operator is looking at. So the answer is
//! decided once, here, and every renderer asks it rather than testing `TERM` again.
//!
//! # Two protocols, and why one is not enough
//!
//! * **Kitty** (`\x1b_G…`, the graphics protocol) — Ghostty, kitty, WezTerm, Konsole and others.
//!   It is the one that can draw *by placeholders*: the terminal keeps the bytes and fills cells
//!   this program draws, so a frame that scrolls, resizes or re-arranges those cells costs the
//!   image's bytes exactly once ([`crate::term::graphics::image_rows`]).
//! * **iTerm2** (`OSC 1337 ; File=…`) — iTerm2, and nothing else is worth guessing about. It has
//!   no image id and no placement: the bytes are drawn at the cursor, into cells, and a redraw of
//!   those cells is a redraw of the picture. That difference is why the caller is told both *what*
//!   the terminal speaks and *how* to deliver it, rather than one bool.
//!
//! # Detection is a ladder, cheapest and most certain first
//!
//! 1. **A name.** `TERM=xterm-kitty` or `TERM_PROGRAM=ghostty` is the terminal saying what it is,
//!    and it costs nothing. `KITTY_WINDOW_ID` is the same statement from a terminal that has set
//!    `TERM` to something conservative.
//! 2. **Inside tmux, whether tmux will pass a sequence through** — a kitty sequence is
//!    DCS-wrapped for tmux ([`Proto::wrap`]) and tmux forwards it **only with
//!    `allow-passthrough` on** (tmux ≥ 3.3; `all` also forwards it from a pane that is not the
//!    active one). With the setting off, tmux swallows it and the picture is missing with no
//!    error anywhere, so the honest answer is [`Proto::None`] — the caller draws its fallback.
//!    iTerm2's protocol has no passthrough at all and is never claimed inside tmux.
//! 3. **A capability query**, for a terminal that named nothing: the protocol's own
//!    `a=q` question, answered `OK` by a terminal that speaks it ([`ask_kitty`]), with a short
//!    timeout because this is a guess and not a measurement.
//!
//! # And the operator's override wins
//!
//! `RANO_TERM_FEATURES` (or `LETIBOT_TERM_FEATURES`) with `-images` means *do not draw pictures*,
//! whatever the detection believes; with `+images` it means *draw them anyway*, which is the only
//! way to try a terminal this file has never heard of. [`crate::term::Features::images`] is the
//! decided form of that, and [`Proto::of`] takes it as an input rather than reading the
//! environment a second time — one decision, one place.

use std::time::Duration;

/// **The kitty protocol's own capability question**: a 1×1 RGB image, transferred but not
/// displayed (`a=q`, `t=d`), under the id [`QUERY_ID`]. A terminal that speaks the protocol
/// answers `\x1b_Gi=107;OK\x1b\\`; one that does not answers nothing at all — the bytes are not
/// `ESC`-prefixed text it knows, so it consumes them silently.
pub const KITTY_QUERY: &[u8] = b"\x1b_Gi=107,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\";

/// The id [`KITTY_QUERY`] asks under, so a reply can be told from anything else on the wire.
pub const QUERY_ID: u32 = 107;

/// **How long a capability query waits.** A terminal on the far side of an `ssh` session answers
/// in single-digit milliseconds; a fifth of a second is one frame's worth of latency, so a
/// terminal that has not answered by then is one this program will not wait for.
pub const QUERY_TIMEOUT: Duration = Duration::from_millis(200);

/// **How a picture can be put on the glass.**
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Proto {
    /// Nothing: the caller draws its own fallback (a name, a size, a chord to open it).
    #[default]
    None,
    /// The kitty graphics protocol, written straight to the terminal.
    Kitty,
    /// The kitty graphics protocol, DCS-wrapped for tmux (`allow-passthrough` is on) — see
    /// [`Proto::wrap`].
    KittyPassthrough,
    /// iTerm2's `OSC 1337;File=`: drawn at the cursor, into cells, with no id to place it by.
    Iterm,
}

/// **The environment detection reads**, as facts rather than a `std::env` call per question —
/// so the decision can be tested for every terminal on a box that is none of them.
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
    /// **What tmux said about `allow-passthrough`**, when it was asked — `None` when nobody asked
    /// (no tmux, or a caller that would rather not spawn one).
    pub passthrough: Option<bool>,
}

impl Env {
    /// This process's environment. Reads four variables and sets two flags; asks no questions of
    /// anything outside the process (see [`Env::ask_passthrough`] for the one that does).
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

    /// **Ask tmux whether it will pass a DCS sequence through**, and record the answer. Only
    /// meaningful inside tmux, and only worth asking then: a `tmux show-options -gv
    /// allow-passthrough` costs a process and a round trip to a server that is usually busy.
    ///
    /// `on` and `all` both forward a wrapped sequence; `all` additionally forwards it from a pane
    /// that is not the active one, which is not this program's case but is not a reason to
    /// refuse either. Anything else — `off`, or an option this tmux is too old to have — is a
    /// no.
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

/// **The terminal's family, by the names it goes by** — the first rung of the ladder, and a pure
/// function of two strings so a test can walk the whole list.
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
    /// **Does this terminal take a picture at all** — the one question a renderer usually has.
    pub fn speaks(self) -> bool {
        self != Proto::None
    }

    /// **The decision, from the environment and the operator's `images` override** — no query, no
    /// process: everything here is a fact already in hand.
    ///
    /// `images` false is the override saying no, and it is answered first: a terminal that speaks
    /// a protocol beautifully is still not to be sent one.
    pub fn of(env: &Env, images: bool) -> Proto {
        if !images {
            return Proto::None;
        }
        if env.tmux {
            // **One protocol crosses tmux, and only with its permission.** iTerm2's `OSC 1337`
            // is swallowed by tmux whatever the setting, and an unwrapped kitty sequence is
            // swallowed without it.
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

    /// [`Proto::of`], and then **the capability query where the names said nothing** — the third
    /// rung. A terminal that answered `OK` is asked to draw; one that did not is not.
    ///
    /// The query is only asked when it can change the answer: a named terminal needs no
    /// question, and a caller that has said *no pictures* must not have one written into its
    /// terminal either.
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

    /// **A sequence as this terminal must receive it**: unchanged for a direct kitty or iTerm2
    /// terminal, DCS-wrapped for tmux, and *dropped* for [`Proto::None`] — a byte written to a
    /// terminal that cannot read it is text typed at the operator's prompt.
    ///
    /// The wrapper is tmux's: `ESC Ptmux;`, then the sequence **with every `ESC` doubled**, then
    /// `ESC \`. tmux un-doubles and forwards it. Doubling is what keeps the sequence's own `ESC`
    /// bytes from ending the wrapper early, which is the whole reason the format exists.
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

/// **What a capability query came back with**: `answered` is the protocol's own verdict, and
/// `other` is every byte read that was not the reply — the caller's input, which must be fed to
/// its decoder and not dropped on the floor because a picture was being negotiated.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Reply {
    pub answered: Option<bool>,
    pub other: Vec<u8>,
}

/// **Ask the terminal whether it speaks the kitty graphics protocol**, for at most `timeout`.
///
/// The question goes to **stdout** and the answer comes back on **stdin**, which is why this is
/// only usefully callable before a host starts reading input, and in raw mode: a terminal in
/// canonical mode does not deliver the reply until a newline it will never send. The two facts
/// are the reason detection prefers the names, and why this is the last rung.
///
/// Anything read that is not part of the answer is returned in [`Reply::other`] — a keystroke
/// typed during the wait is the operator's, not this function's to swallow.
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

/// **What the terminal said**, out of the bytes read so far: `Some(true)` for `OK`, `Some(false)`
/// for a refusal the protocol names (`ENOTSUP`), `None` while the answer has not arrived — or
/// when what arrived was something else entirely, which is not an answer.
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

/// **One read from stdin, waiting at most `budget` for a byte** — `None` on a timeout or an
/// error, which the caller treats the same way: nothing more is coming.
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

    /// **The names, on every terminal worth knowing** — and the ones this file declines to guess
    /// about, which is the half that keeps a picture out of somebody's prompt.
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
        // **And nothing else** — Terminal.app, an unknown terminal, a plain `screen`.
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

    /// **Inside tmux one protocol crosses, and only with `allow-passthrough`** — the setting whose
    /// absence is a picture missing with no error anywhere.
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

    /// **The wrapper is tmux's, byte for byte**: `ESC Ptmux;`, the sequence with every `ESC`
    /// doubled, `ESC \`. An un-doubled inner `ESC` would end the wrapper at the first byte of the
    /// sequence it is carrying.
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

    /// **The reply parser**, on the shapes a terminal really sends: an `OK`, a refusal, a reply
    /// for somebody else's id, and a keystroke that is not a reply at all.
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
