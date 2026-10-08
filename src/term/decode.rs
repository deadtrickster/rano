//! **Bytes from the terminal, read as events** — the legacy encodings, the kitty keyboard,
//! SGR mouse, focus reports and the strings a terminal sends back (OSC, APC).
//!
//! Ported from letibot's `crates/tui/src/backend/decode.rs`. What changed is the output:
//! letibot decoded straight into its app's `Key` (`0x0b` was `KillToEnd`); this decodes into
//! [`Event`], which says what was pressed and leaves what it means to the app — see
//! `super::event`'s header. Everything about *reading* the bytes is letibot's and kept: the
//! incomplete-tail contract, bracketed paste first, Esc's resolution, the kitty keyboard put
//! back into the legacy bytes it stands for.
//!
//! # Control bytes, and the ones a raw terminal frees
//!
//! A C0 byte is Ctrl plus the letter it is the control form of, with the exceptions every
//! terminal makes: `0x08`/`0x7f` are Backspace, `0x09` Tab, `0x0d`/`0x0a` Enter, `0x1b` Esc.
//! Several of these are tty controls in cooked mode and free in raw mode, because `cfmakeraw`
//! clears what listens for them: Ctrl+S/Ctrl+Q (XOFF/XON, `IXON`), Ctrl+V (literal-next,
//! `IEXTEN`), Ctrl+Z (suspend, `ISIG`), Ctrl+C (interrupt, `ISIG`). So each arrives here as a
//! key like any other.
//!
//! `0x1c`–`0x1f` are Ctrl+`\`, Ctrl+`]`, Ctrl+`^`, Ctrl+`_`. letibot's decoder had no arm for
//! `0x1c` and ate it, which its pane relied on — the way out of a full-screen program is a key
//! the program never receives, and it is found on the raw stream (`Terminal::raw_input`),
//! before anything is forwarded. Here it decodes to Ctrl+`\`; an app that reserves it maps it
//! to nothing, which is where that decision belongs.

use super::event::{Event, KeyCode, KeyEvent, Mods, MouseButton, MouseEvent, MouseKind};

/// Is a bracketed paste open at the end of `b`?
///
/// The signal that says "keep reading": a paste's terminator is the only precise
/// evidence that more bytes are on their way, and without it a 40 KB paste is
/// decided by a 100 ms timeout in the middle of somebody's stack trace.
pub fn paste_open(b: &[u8]) -> bool {
    let start = rfind(b, PASTE_START);
    let end = rfind(b, PASTE_END);
    match (start, end) {
        (Some(s), Some(e)) => s > e,
        (Some(_), None) => true,
        _ => false,
    }
}

const PASTE_START: &[u8] = b"\x1b[200~";
const PASTE_END: &[u8] = b"\x1b[201~";

fn rfind(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.len() > hay.len() {
        return None;
    }
    (0..=hay.len() - needle.len())
        .rev()
        .find(|&i| &hay[i..i + needle.len()] == needle)
}

/// Decode a read buffer into events.
///
/// Convenience over [`decode_prefix`] for a caller with a complete buffer — a
/// test, or a replay script. Anything trailing and incomplete is decoded as
/// best it can be, which is what "this is all there is" means.
pub fn decode(b: &[u8]) -> Vec<Event> {
    decode_prefix(b, true).0
}

fn key(code: KeyCode, mods: Mods) -> Event {
    Event::Key(KeyEvent::new(code, mods))
}

/// A single byte below `0x80` that is not `ESC`, as the key it is.
fn byte_key(c: u8) -> Option<KeyEvent> {
    Some(match c {
        b'\r' | b'\n' => KeyEvent::plain(KeyCode::Enter),
        0x09 => KeyEvent::plain(KeyCode::Tab),
        0x7f | 0x08 => KeyEvent::plain(KeyCode::Backspace),
        0x00 => KeyEvent::ctrl(' '),
        0x01..=0x1a => KeyEvent::ctrl((b'a' + c - 1) as char),
        0x1c => KeyEvent::ctrl('\\'),
        0x1d => KeyEvent::ctrl(']'),
        0x1e => KeyEvent::ctrl('^'),
        0x1f => KeyEvent::ctrl('_'),
        0x20..=0x7e => KeyEvent::char(c as char),
        _ => return None,
    })
}

/// Decode as much of `b` as is unambiguously complete, and say how many bytes
/// that was.
///
/// The returned count is the contract: everything past it is an **incomplete
/// tail** — a UTF-8 sequence cut in half, a CSI whose final byte has not
/// arrived, a bracketed paste still open — and the caller carries it into the
/// next read. letibot's first decoder had no such notion, so a read that ended
/// mid-character failed `from_utf8` and the arm dropped the bytes without a
/// word. A person pasting an error message is not an edge case.
///
/// `force` decodes the tail anyway, for the last read of a stream and for the
/// ceiling on the carry.
pub fn decode_prefix(b: &[u8], force: bool) -> (Vec<Event>, usize) {
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        // Bracketed paste, first: everything inside it is content, including
        // bytes that would otherwise be keys. This is what stops a pasted
        // newline from submitting a prompt.
        if b[i..].starts_with(PASTE_START) {
            let body = i + PASTE_START.len();
            match find(&b[body..], PASTE_END) {
                Some(k) => {
                    out.push(Event::Paste(
                        String::from_utf8_lossy(&b[body..body + k]).into_owned(),
                    ));
                    i = body + k + PASTE_END.len();
                    continue;
                }
                None if force => {
                    out.push(Event::Paste(
                        String::from_utf8_lossy(&b[body..]).into_owned(),
                    ));
                    return (out, b.len());
                }
                None => return (out, i),
            }
        }
        let c = b[i];
        if c == 0x1b {
            match escape(&b[i..], force) {
                Step::Emit(e, n) => {
                    out.extend(e);
                    i += n;
                }
                Step::Incomplete => return (out, i),
            }
            continue;
        }
        if c < 0x80 {
            out.extend(byte_key(c).map(Event::Key));
            i += 1;
            continue;
        }
        match utf8_at(b, i) {
            Utf8::Char(ch, n) => {
                out.push(Event::Key(KeyEvent::char(ch)));
                i += n;
            }
            // The read ended mid-character. This is the byte-losing bug: hold it,
            // do not decode it.
            Utf8::Short if !force => return (out, i),
            Utf8::Short => return (out, b.len()),
            // Not UTF-8 at all. Skipping one byte resynchronises without stalling
            // the stream on it forever.
            Utf8::Bad => i += 1,
        }
    }
    (out, b.len())
}

enum Utf8 {
    Char(char, usize),
    Short,
    Bad,
}

fn utf8_at(b: &[u8], i: usize) -> Utf8 {
    let len = utf8_len(b[i]);
    if i + len > b.len() {
        return Utf8::Short;
    }
    match std::str::from_utf8(&b[i..i + len]) {
        Ok(t) => t.chars().next().map_or(Utf8::Bad, |ch| Utf8::Char(ch, len)),
        Err(_) => Utf8::Bad,
    }
}

enum Step {
    /// An event — or nothing, for a sequence recognised and deliberately ignored —
    /// and how many bytes it took.
    Emit(Option<Event>, usize),
    /// The sequence has not finished arriving.
    Incomplete,
}

/// Decode one escape sequence at the head of `b`, which starts with `0x1b`.
fn escape(b: &[u8], force: bool) -> Step {
    let esc = Step::Emit(Some(key(KeyCode::Esc, Mods::NONE)), 1);
    if b.len() == 1 {
        // A lone ESC is Esc. It is genuinely ambiguous — every arrow key starts
        // this way — and it is resolved in favour of the key a person pressed on
        // purpose: an Esc that waits for a disambiguating read is an Esc that
        // arrives a frame late, and a double Esc (letibot's interrupt) is two.
        return esc;
    }
    match b[1] {
        // Two of them. At a 100 ms read they usually arrive in the same buffer —
        // consuming both as one Alt+Esc would eat a deliberate double Esc.
        0x1b => esc,
        b'[' => csi(b, force),
        // **A string the terminal sent back**: OSC (`ESC ]`) for the background colour asked
        // at enter, APC (`ESC _`) for a kitty graphics reply. Consumed whole up to its
        // terminator — BEL or ST — and never typed: a reply is not a keystroke.
        b']' | b'_' => match string_end(&b[2..]) {
            Some((body, used)) => Step::Emit(
                if b[1] == b']' {
                    background_reply(&b[2..2 + body])
                } else {
                    None
                },
                2 + used,
            ),
            None if force => esc,
            None => Step::Incomplete,
        },
        // SS3: the application-cursor-mode arrows, which is what a terminal sends
        // after `smkx`, and F1–F4.
        b'O' => {
            if b.len() < 3 {
                return if force { esc } else { Step::Incomplete };
            }
            let k = match b[2] {
                b'A' => Some(KeyCode::Up),
                b'B' => Some(KeyCode::Down),
                b'C' => Some(KeyCode::Right),
                b'D' => Some(KeyCode::Left),
                b'H' => Some(KeyCode::Home),
                b'F' => Some(KeyCode::End),
                b'P' => Some(KeyCode::F(1)),
                b'Q' => Some(KeyCode::F(2)),
                b'R' => Some(KeyCode::F(3)),
                b'S' => Some(KeyCode::F(4)),
                _ => None,
            };
            Step::Emit(k.map(|k| key(k, Mods::NONE)), 3)
        }
        // **Alt+key**: ESC before the key. Alt+Enter is the one that has to work — a
        // terminal cannot report Shift+Enter at all without the kitty protocol, so it is
        // how a newline that does not submit gets typed.
        c if c < 0x80 => match byte_key(c) {
            Some(k) => Step::Emit(Some(key(k.code, k.mods | Mods::ALT)), 2),
            None => Step::Emit(None, 2),
        },
        _ => match utf8_at(b, 1) {
            Utf8::Char(ch, n) => Step::Emit(Some(key(KeyCode::Char(ch), Mods::ALT)), 1 + n),
            Utf8::Short if !force => Step::Incomplete,
            Utf8::Short | Utf8::Bad => esc,
        },
    }
}

/// The modifier field of a CSI key (`1;5C` → Ctrl): the wire carries `1 + bits`.
fn csi_mods(params: &[u8]) -> Mods {
    let m = params
        .split(|c| *c == b';')
        .nth(1)
        .and_then(|f| std::str::from_utf8(f).ok())
        .and_then(|f| f.split(':').next()?.parse::<u8>().ok())
        .unwrap_or(1);
    Mods::from_bits(m.saturating_sub(1))
}

/// Decode one CSI sequence: `ESC [`, parameters, intermediates, a final byte.
fn csi(b: &[u8], force: bool) -> Step {
    let mut i = 2;
    while i < b.len() && (0x30..=0x3f).contains(&b[i]) {
        i += 1;
    }
    let params_end = i;
    while i < b.len() && (0x20..=0x2f).contains(&b[i]) {
        i += 1;
    }
    if i >= b.len() {
        // The final byte has not arrived. Holding is the whole point: decoding
        // `\x1b[` as an Esc and a `[` is how half an arrow key becomes typed
        // punctuation in the middle of a prompt.
        return if force {
            Step::Emit(Some(key(KeyCode::Esc, Mods::NONE)), 1)
        } else {
            Step::Incomplete
        };
    }
    let fin = b[i];
    let n = i + 1;
    let params = &b[2..params_end];
    let mods = csi_mods(params);
    let k = |c| Some(key(c, mods));
    let e = match fin {
        // **The kitty keyboard** (`CSI code ; mods u`), put back into the legacy bytes it
        // stands for and decoded as those — so every binding already written for the legacy
        // spelling means the same key under the protocol, and the only new facts are the two
        // the protocol exists for: Shift+Enter is a key, and Esc is never half of something.
        b'u' => kitty_key(params),
        // Focus reports (`?1004`): the window gained or lost focus.
        b'I' if params.is_empty() => Some(Event::FocusGained),
        b'O' if params.is_empty() => Some(Event::FocusLost),
        b'A' => k(KeyCode::Up),
        b'B' => k(KeyCode::Down),
        b'C' => k(KeyCode::Right),
        b'D' => k(KeyCode::Left),
        b'H' => k(KeyCode::Home),
        b'F' => k(KeyCode::End),
        b'Z' => Some(key(KeyCode::BackTab, mods | Mods::SHIFT)),
        // xterm's modified F1–F4: `CSI 1 ; mods P`.
        b'P' => k(KeyCode::F(1)),
        b'Q' => k(KeyCode::F(2)),
        b'R' => k(KeyCode::F(3)),
        b'S' => k(KeyCode::F(4)),
        b'~' => match params.split(|c| *c == b';').next().unwrap_or(b"") {
            b"1" | b"7" => k(KeyCode::Home),
            b"2" => k(KeyCode::Insert),
            b"3" => k(KeyCode::Delete),
            b"4" | b"8" => k(KeyCode::End),
            b"5" => k(KeyCode::PageUp),
            b"6" => k(KeyCode::PageDown),
            b"11" => k(KeyCode::F(1)),
            b"12" => k(KeyCode::F(2)),
            b"13" => k(KeyCode::F(3)),
            b"14" => k(KeyCode::F(4)),
            b"15" => k(KeyCode::F(5)),
            b"17" => k(KeyCode::F(6)),
            b"18" => k(KeyCode::F(7)),
            b"19" => k(KeyCode::F(8)),
            b"20" => k(KeyCode::F(9)),
            b"21" => k(KeyCode::F(10)),
            b"23" => k(KeyCode::F(11)),
            b"24" => k(KeyCode::F(12)),
            _ => None,
        },
        // SGR mouse (`?1006`): `ESC [ < b ; x ; y M` on press and motion, `m` on release.
        // Every report is decoded — a report nobody acts on must never become typed
        // punctuation — and the app decides which it wants. Selecting text stays the
        // terminal's own Shift+drag, which mouse tracking does not take away.
        b'M' | b'm' if params.first() == Some(&b'<') => sgr_mouse(&params[1..], fin == b'm'),
        _ => None,
    };
    Step::Emit(e, n)
}

/// `b ; x ; y` of an SGR mouse report. Coordinates are 1-based on the wire, 0-based here.
fn sgr_mouse(params: &[u8], release: bool) -> Option<Event> {
    let mut fields = params.split(|c| *c == b';').map(|f| {
        std::str::from_utf8(f)
            .ok()
            .and_then(|f| f.parse::<u16>().ok())
    });
    let btn = fields.next()??;
    let x = fields.next().flatten().unwrap_or(1).saturating_sub(1);
    let y = fields.next().flatten().unwrap_or(1).saturating_sub(1);
    // Bits 2–4 of the button are shift, meta, control; 5 is motion; 6 the wheel.
    let mut mods = Mods::NONE;
    if btn & 4 != 0 {
        mods = mods | Mods::SHIFT;
    }
    if btn & 8 != 0 {
        mods = mods | Mods::ALT;
    }
    if btn & 16 != 0 {
        mods = mods | Mods::CTRL;
    }
    let button = match btn & 3 {
        0 => Some(MouseButton::Left),
        1 => Some(MouseButton::Middle),
        2 => Some(MouseButton::Right),
        _ => None,
    };
    let kind = if btn & 64 != 0 {
        if release {
            return None;
        }
        match btn & 3 {
            0 => MouseKind::WheelUp,
            1 => MouseKind::WheelDown,
            2 => MouseKind::WheelLeft,
            _ => MouseKind::WheelRight,
        }
    } else if release {
        MouseKind::Release(button?)
    } else if btn & 32 != 0 {
        match button {
            Some(b) => MouseKind::Drag(b),
            None => MouseKind::Move,
        }
    } else {
        MouseKind::Press(button?)
    };
    Some(Event::Mouse(MouseEvent { kind, x, y, mods }))
}

/// `CSI code ; mods u` → the event it means. See the arm in [`csi`].
fn kitty_key(params: &[u8]) -> Option<Event> {
    let (code, bits) = kitty_fields(params)?;
    let mods = Mods::from_bits(bits);
    // The one key the legacy spelling cannot carry: Enter keeps its shift.
    if code == 13 && mods.shift() {
        return Some(key(KeyCode::Enter, mods));
    }
    if code == 27 && bits == 0 {
        return Some(key(KeyCode::Esc, Mods::NONE));
    }
    let legacy = kitty_legacy(code, bits)?;
    let (events, _) = decode_prefix(&legacy, true);
    events.into_iter().next()
}

/// The code point and the modifier bits (shift 1, alt 2, ctrl 4) of a `CSI … u`. The wire
/// carries `1 + bits`, and a missing modifier field is none.
fn kitty_fields(params: &[u8]) -> Option<(u32, u8)> {
    let text = std::str::from_utf8(params).ok()?;
    let mut fields = text.split(';');
    // `code:shifted:base` — only the first is this key.
    let code: u32 = fields.next()?.split(':').next()?.parse().ok()?;
    let mods: u8 = match fields.next() {
        Some(m) => m.split(':').next()?.parse::<u8>().ok()?.saturating_sub(1),
        None => 0,
    };
    Some((code, mods))
}

/// What a terminal without the protocol sends for this key, or `None` for a key it has no
/// spelling for (the keypad's private-use codes).
fn kitty_legacy(code: u32, mods: u8) -> Option<Vec<u8>> {
    let (alt, ctrl) = (mods & 2 != 0, mods & 4 != 0);
    let mut out = Vec::new();
    if alt {
        out.push(0x1b);
    }
    match code {
        13 => out.push(b'\r'),
        9 => out.push(b'\t'),
        27 => out.push(0x1b),
        127 => out.push(0x7f),
        8 => out.push(0x08),
        c => {
            let ch = char::from_u32(c)?;
            if ctrl && ch.is_ascii() && (ch == ' ' || ('@'..='~').contains(&ch)) {
                out.push((ch.to_ascii_uppercase() as u8) & 0x1f);
            } else if (0xe000..=0xf8ff).contains(&c) {
                return None;
            } else {
                let mut buf = [0u8; 4];
                out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
            }
        }
    }
    Some(out)
}

/// **The raw stream with every `CSI … u` rewritten to its legacy bytes**, for a pane whose
/// program never asked for the protocol. Everything else passes through untouched.
///
/// A program that owns the screen reads the bytes the terminal sent, and an [`Event`] is a
/// lossy reading of them (`ESC [ A` and `ESC O A` are both Up; a paste has lost its markers),
/// so re-encoding events would be a keymap in front of a terminal. Under the kitty keyboard,
/// though, the raw stream is in a spelling the program never asked for — Ctrl-C arrives as
/// `CSI 99;5u` — so it is put back into what a terminal without the protocol would have sent.
pub fn legacy_bytes(raw: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(raw.len());
    let mut i = 0;
    while i < raw.len() {
        if raw[i..].starts_with(b"\x1b[") {
            let mut j = i + 2;
            while j < raw.len() && (0x30..=0x3f).contains(&raw[j]) {
                j += 1;
            }
            if j < raw.len() && raw[j] == b'u' {
                let params = &raw[i + 2..j];
                let legacy = kitty_fields(params).and_then(|(code, mods)| {
                    if code == 13 && mods & 1 != 0 {
                        Some(vec![b'\r'])
                    } else {
                        kitty_legacy(code, mods)
                    }
                });
                if let Some(bytes) = legacy {
                    out.extend_from_slice(&bytes);
                }
                i = j + 1;
                continue;
            }
        }
        out.push(raw[i]);
        i += 1;
    }
    out
}

/// Where an OSC/APC string ends: its body's length and the bytes used including the
/// terminator (BEL, or ST = `ESC \`). `None` while it has not finished arriving.
fn string_end(b: &[u8]) -> Option<(usize, usize)> {
    for (i, &c) in b.iter().enumerate() {
        if c == 0x07 {
            return Some((i, i + 1));
        }
        if c == 0x1b && b.get(i + 1) == Some(&b'\\') {
            return Some((i, i + 2));
        }
    }
    None
}

/// `11;rgb:RRRR/GGGG/BBBB` → whether the background is light. Any other OSC is dropped.
fn background_reply(body: &[u8]) -> Option<Event> {
    let text = std::str::from_utf8(body).ok()?;
    let rgb = text.strip_prefix("11;")?.strip_prefix("rgb:")?;
    let mut chans = rgb.split('/').map(|h| {
        // 1–4 hex digits, scaled to 0..=1.
        let v = u32::from_str_radix(h, 16).ok()?;
        let max = (1u32 << (4 * h.len() as u32)) - 1;
        Some(v as f64 / max as f64)
    });
    let (r, g, b) = (chans.next()??, chans.next()??, chans.next()??);
    // Relative luminance, the sRGB weights; past half is a light background.
    let lum = 0.2126 * r + 0.7152 * g + 0.0722 * b;
    Some(Event::Background { light: lum > 0.5 })
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.len() > hay.len() {
        return None;
    }
    (0..=hay.len() - needle.len()).find(|&i| &hay[i..i + needle.len()] == needle)
}

fn utf8_len(b: u8) -> usize {
    match b {
        0x00..=0x7f => 1,
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        _ => 4,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(code: KeyCode) -> Event {
        key(code, Mods::NONE)
    }
    fn ctrl(c: char) -> Event {
        Event::Key(KeyEvent::ctrl(c))
    }
    fn alt(code: KeyCode) -> Event {
        key(code, Mods::ALT)
    }
    fn ch(c: char) -> Event {
        Event::Key(KeyEvent::char(c))
    }

    #[test]
    fn arrows_and_control_keys_decode() {
        assert_eq!(decode(b"\x1b[A"), vec![k(KeyCode::Up)]);
        assert_eq!(decode(b"\x1b[B"), vec![k(KeyCode::Down)]);
        assert_eq!(decode(b"\x03"), vec![ctrl('c')]);
        assert_eq!(decode(b"\r"), vec![k(KeyCode::Enter)]);
        assert_eq!(decode(b"\x7f"), vec![k(KeyCode::Backspace)]);
        assert_eq!(decode(b"\x1b"), vec![k(KeyCode::Esc)]);
        assert_eq!(decode(b"\x18"), vec![ctrl('x')]);
        // `0x0e` had no arm in letibot's first decoder and was eaten silently — a key that
        // did nothing rather than a key bound to nothing. Every C0 byte is a key here.
        assert_eq!(decode(b"\x0e"), vec![ctrl('n')]);
        assert_eq!(decode(b"\x00"), vec![ctrl(' ')]);
        for c in 0x01u8..=0x1a {
            if matches!(c, 0x08 | 0x09 | 0x0a | 0x0d) {
                continue;
            }
            assert_eq!(decode(&[c]), vec![ctrl((b'a' + c - 1) as char)], "{c:#x}");
        }
    }

    #[test]
    fn a_read_that_ends_mid_utf8_loses_nothing() {
        // letibot's bug, as an assertion: a read that split a character failed the whole
        // decode and the arm returned nothing, silently. Pasting a stack trace with an
        // arrow or a box-drawing character in it lost bytes.
        let whole = "héllo → wörld ⣿".as_bytes();
        for cut in 1..whole.len() {
            let (a, b) = whole.split_at(cut);
            let (mut keys, used) = decode_prefix(a, false);
            // Whatever was not decodable is carried, never dropped.
            let mut rest = a[used..].to_vec();
            rest.extend_from_slice(b);
            keys.extend(decode_prefix(&rest, true).0);
            let text: String = keys
                .iter()
                .map(|e| match e {
                    Event::Key(KeyEvent {
                        code: KeyCode::Char(c),
                        mods,
                    }) if mods.is_empty() => *c,
                    other => panic!("{other:?}"),
                })
                .collect();
            assert_eq!(text, "héllo → wörld ⣿", "split at {cut}");
        }
    }

    #[test]
    fn an_escape_sequence_split_across_two_reads_is_one_key_not_typed_punctuation() {
        // A lone ESC is the one deliberate exception: genuinely ambiguous, and resolved
        // in favour of the key a person pressed on purpose.
        assert_eq!(decode_prefix(b"\x1b", false), (vec![k(KeyCode::Esc)], 1));
        assert_eq!(decode(b"\x1b\x1b"), vec![k(KeyCode::Esc), k(KeyCode::Esc)]);

        let whole = b"\x1b[1;5C";
        for cut in 2..whole.len() {
            let (keys, used) = decode_prefix(&whole[..cut], false);
            assert!(keys.is_empty(), "cut {cut}: {keys:?}");
            assert_eq!(used, 0, "the partial sequence must be carried, not eaten");
        }
        assert_eq!(decode(whole), vec![key(KeyCode::Right, Mods::CTRL)]);
    }

    #[test]
    fn sgr_mouse_reports_decode_whole_and_split_ones_are_carried() {
        let m = |kind, x, y, mods| Event::Mouse(MouseEvent { kind, x, y, mods });
        assert_eq!(
            decode(b"\x1b[<64;10;5M"),
            vec![m(MouseKind::WheelUp, 9, 4, Mods::NONE)]
        );
        assert_eq!(
            decode(b"\x1b[<65;10;5M"),
            vec![m(MouseKind::WheelDown, 9, 4, Mods::NONE)]
        );
        assert_eq!(
            decode(b"\x1b[<0;3;4M"),
            vec![m(MouseKind::Press(MouseButton::Left), 2, 3, Mods::NONE)]
        );
        assert_eq!(
            decode(b"\x1b[<0;3;4m"),
            vec![m(MouseKind::Release(MouseButton::Left), 2, 3, Mods::NONE)]
        );
        assert_eq!(
            decode(b"\x1b[<32;3;4M"),
            vec![m(MouseKind::Drag(MouseButton::Left), 2, 3, Mods::NONE)]
        );
        assert_eq!(
            decode(b"\x1b[<35;3;4M"),
            vec![m(MouseKind::Move, 2, 3, Mods::NONE)]
        );
        assert_eq!(
            decode(b"\x1b[<18;1;1M"),
            vec![m(MouseKind::Press(MouseButton::Right), 0, 0, Mods::CTRL)]
        );
        // A report cut mid-sequence is carried, not eaten.
        let whole = b"\x1b[<65;1;1M";
        for cut in 2..whole.len() {
            let (keys, used) = decode_prefix(&whole[..cut], false);
            assert!(keys.is_empty(), "cut {cut}: {keys:?}");
            assert_eq!(used, 0, "the partial report must be carried, not eaten");
        }
        assert_eq!(decode(whole).len(), 1);
    }

    #[test]
    fn a_bracketed_paste_is_one_event_and_its_newlines_are_not_enter() {
        let mut b = b"\x1b[200~".to_vec();
        b.extend_from_slice(b"line one\nline two\nline three");
        b.extend_from_slice(b"\x1b[201~");
        assert_eq!(
            decode(&b),
            vec![Event::Paste("line one\nline two\nline three".into())]
        );
        // …and while the terminator has not arrived, nothing is consumed: the
        // read loop is still waiting for the rest of the paste.
        let open = &b[..b.len() - 3];
        assert!(paste_open(open));
        assert_eq!(decode_prefix(open, false), (Vec::new(), 0));
    }

    #[test]
    fn the_keys_an_editor_needs_all_decode() {
        for (bytes, want) in [
            (&b"\x1b[C"[..], k(KeyCode::Right)),
            (b"\x1b[D", k(KeyCode::Left)),
            (b"\x1bOC", k(KeyCode::Right)),
            (b"\x1b[H", k(KeyCode::Home)),
            (b"\x1b[3~", k(KeyCode::Delete)),
            (b"\x1b[2~", k(KeyCode::Insert)),
            (b"\x1b[5~", k(KeyCode::PageUp)),
            (b"\x1b[6~", k(KeyCode::PageDown)),
            (b"\x1b[1;5D", key(KeyCode::Left, Mods::CTRL)),
            (b"\x1b[1;2A", key(KeyCode::Up, Mods::SHIFT)),
            (b"\x1b[3;3~", key(KeyCode::Delete, Mods::ALT)),
            (b"\x1b[Z", key(KeyCode::BackTab, Mods::SHIFT)),
            (b"\x1bOP", k(KeyCode::F(1))),
            (b"\x1b[15~", k(KeyCode::F(5))),
            (b"\x1b[24~", k(KeyCode::F(12))),
            (b"\x1b[1;5P", key(KeyCode::F(1), Mods::CTRL)),
            (b"\x1b\r", alt(KeyCode::Enter)),
            (b"\x1bb", alt(KeyCode::Char('b'))),
            (b"\x1b\x7f", alt(KeyCode::Backspace)),
            (b"\x1b\x01", key(KeyCode::Char('a'), Mods::CTRL | Mods::ALT)),
            (b"\x09", k(KeyCode::Tab)),
            (b"\x1c", ctrl('\\')),
            (b"\x1f", ctrl('_')),
        ] {
            assert_eq!(decode(bytes), vec![want.clone()], "{bytes:?}");
        }
        assert_eq!(decode("\x1bé".as_bytes()), vec![alt(KeyCode::Char('é'))]);
        // Alt plus half a character is held, like any other incomplete tail.
        assert_eq!(decode_prefix(b"\x1b\xc3", false), (Vec::new(), 0));
    }

    #[test]
    fn a_multibyte_paste_survives() {
        assert_eq!(
            decode("héllo".as_bytes()),
            vec![ch('h'), ch('é'), ch('l'), ch('l'), ch('o')]
        );
    }

    /// **The kitty keyboard means the keys the legacy spelling already meant**, plus the
    /// two it exists for: Shift+Enter is a key, and Esc is Esc.
    #[test]
    fn kitty_keys_decode_to_their_legacy_meaning() {
        let one = |b: &[u8]| {
            let (k, used) = decode_prefix(b, false);
            assert_eq!(used, b.len(), "{b:?} not consumed whole");
            k
        };
        assert_eq!(one(b"\x1b[13;2u"), vec![key(KeyCode::Enter, Mods::SHIFT)]);
        assert_eq!(one(b"\x1b[27u"), vec![k(KeyCode::Esc)]);
        assert_eq!(one(b"\x1b[99;5u"), decode(b"\x03"));
        assert_eq!(one(b"\x1b[118;5u"), vec![ctrl('v')]);
        assert_eq!(one(b"\x1b[98;3u"), vec![alt(KeyCode::Char('b'))]);
        // A plain key the protocol chose to report anyway.
        assert_eq!(one(b"\x1b[97u"), vec![ch('a')]);
        // The keypad's private-use codes have no legacy spelling, and are not typed.
        assert_eq!(one(b"\x1b[57399u"), Vec::<Event>::new());
        // An arrow is still the legacy CSI under flag 1.
        assert_eq!(one(b"\x1b[A"), vec![k(KeyCode::Up)]);
    }

    #[test]
    fn focus_reports_and_the_background_reply_are_events_not_text() {
        let (k, _) = decode_prefix(b"\x1b[I\x1b[O", false);
        assert_eq!(k, vec![Event::FocusGained, Event::FocusLost]);
        // Ghostty answers OSC 11 with four hex digits a channel, terminated by ST or BEL.
        let (k, used) = decode_prefix(b"\x1b]11;rgb:ffff/ffff/ffff\x1b\\x", false);
        assert_eq!(k, vec![Event::Background { light: true }, ch('x')]);
        assert_eq!(used, 26, "the reply and the key after it, all consumed");
        let (k, _) = decode_prefix(b"\x1b]11;rgb:1e1e/1e1e/2e2e\x07", false);
        assert_eq!(k, vec![Event::Background { light: false }]);
        // Half a reply is held, not typed.
        let (k, used) = decode_prefix(b"\x1b]11;rgb:ff", false);
        assert!(k.is_empty() && used == 0, "{k:?} {used}");
        // A kitty graphics reply (APC) is swallowed whole.
        let (k, _) = decode_prefix(b"\x1b_Gi=1;OK\x1b\\", false);
        assert!(k.is_empty(), "{k:?}");
    }

    /// **A pane's program reads legacy bytes**, whatever spelling the terminal used — and
    /// letibot's way out of a pane, `ctrl-\` (`0x1c`), is still the byte it scans for.
    #[test]
    fn a_pane_gets_legacy_bytes_under_the_kitty_keyboard() {
        assert_eq!(legacy_bytes(b"ls\x1b[99;5u"), b"ls\x03".to_vec());
        assert_eq!(legacy_bytes(b"\x1b[92;5u"), vec![0x1c]);
        assert_eq!(legacy_bytes(b"\x1b[27u"), vec![0x1b]);
        assert_eq!(legacy_bytes(b"\x1b[13;2u"), b"\r".to_vec());
        // Everything that is not a `CSI … u` passes untouched.
        assert_eq!(
            legacy_bytes(b"\x1b[A\x1b[1;5C"),
            b"\x1b[A\x1b[1;5C".to_vec()
        );
        // `0x1c` cannot be part of a character or the final byte of a CSI, so a raw scan
        // for it is exact and not a guess about where a sequence ends.
        assert!(!(0x40..=0x7e).contains(&0x1c));
    }
}
