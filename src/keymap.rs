//! Keys and keymaps, emacs-shaped.
//!
//! A [`Key`] is one chord (`C-x`, `M-p`, `<f8>`, `RET`); a [`Keymap`] maps keys
//! to a command's name or to another keymap — a **prefix** — so `M-t t` is the
//! key `M-t` leading to a map whose `t` names a command. What is active is a
//! [`stack`](lookup) of keymaps, most specific first (a mode's map above the
//! global one), and a key sequence resolves in the first map that knows it.
//!
//! The maps hold command **names**, not functions: what a name does is the
//! host's (the editor's command table, or an embedding host's own). That keeps
//! this module pure data — a host can build a map, stack it over the editor's,
//! print it, or ask which keys reach a command, without any of the editor.
//!
//! # Notation
//!
//! Emacs's, for reading and writing: `C-` control, `M-` meta (alt), `S-` shift
//! (for keys that are not characters), `C-M-x`; named keys in angle brackets
//! (`<f8>`, `<left>`, `<pgdn>`, `<delete>`) and the four emacs spells it out
//! (`RET`, `TAB`, `ESC`, `DEL` for backspace, `SPC`). A sequence is keys joined
//! by spaces: `M-t t`. [`Key::nano`] writes the same key the way nano's bar does
//! (`^X`, `M-U`, `F8`).

use crate::term::{KeyCode, KeyEvent};

/// One chord: a key and its modifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Key {
    pub code: KeyCode,
    pub ctrl: bool,
    pub alt: bool,
    /// Only for keys that are not characters (`S-<tab>`); a character's case
    /// is the shift.
    pub shift: bool,
}

impl Key {
    pub const fn plain(code: KeyCode) -> Key {
        Key {
            code,
            ctrl: false,
            alt: false,
            shift: false,
        }
    }

    pub const fn ctrl(c: char) -> Key {
        Key {
            code: KeyCode::Char(c),
            ctrl: true,
            alt: false,
            shift: false,
        }
    }

    pub const fn meta(c: char) -> Key {
        Key {
            code: KeyCode::Char(c),
            ctrl: false,
            alt: true,
            shift: false,
        }
    }

    /// A terminal key event as a key, with the terminal's own spellings undone:
    /// `C-/` arrives as `C-_` (the byte 0x1f, which is also what `C-7` sends),
    /// some terminals spell `C-\` as `C-4`, and a character's shift is already
    /// its case.
    pub fn from_event(e: &KeyEvent) -> Key {
        let ctrl = e.mods.ctrl();
        let alt = e.mods.alt();
        let mut shift = e.mods.shift();
        let code = match e.code {
            KeyCode::Char('4') if ctrl => KeyCode::Char('\\'),
            KeyCode::Char('7') | KeyCode::Char('_') if ctrl => KeyCode::Char('/'),
            // A control letter arrives lower-case whatever shift says.
            KeyCode::Char(c) if ctrl && c.is_ascii_uppercase() => {
                KeyCode::Char(c.to_ascii_lowercase())
            }
            KeyCode::BackTab => {
                shift = true;
                KeyCode::Tab
            }
            c => c,
        };
        if matches!(code, KeyCode::Char(_)) {
            shift = false;
        }
        Key {
            code,
            ctrl,
            alt,
            shift,
        }
    }

    /// The same key with a letter's case folded to lower — what `M-U` falls
    /// back to when only `M-u` is bound (nano's Meta keys ignore case).
    pub fn folded(self) -> Key {
        match self.code {
            KeyCode::Char(c) if c.is_ascii_uppercase() => Key {
                code: KeyCode::Char(c.to_ascii_lowercase()),
                ..self
            },
            _ => self,
        }
    }

    /// A plain printable character: what is typed, not a command, unless a
    /// map binds it.
    pub fn printable(self) -> Option<char> {
        match self.code {
            KeyCode::Char(c) if !self.ctrl && !self.alt && !c.is_control() => Some(c),
            _ => None,
        }
    }

    /// Parse one chord in emacs notation.
    pub fn parse(s: &str) -> Option<Key> {
        // A placeholder: every path below either sets the code or returns None.
        let mut k = Key::plain(KeyCode::Esc);
        let mut rest = s;
        loop {
            // A modifier prefix is a letter and a dash with something after it
            // (`C--` is control-minus).
            if rest.len() > 2 && rest.as_bytes()[1] == b'-' {
                match rest.as_bytes()[0] {
                    b'C' => k.ctrl = true,
                    b'M' => k.alt = true,
                    b'S' => k.shift = true,
                    _ => break,
                }
                rest = &rest[2..];
                continue;
            }
            break;
        }
        k.code = match rest {
            "RET" => KeyCode::Enter,
            "TAB" => KeyCode::Tab,
            "ESC" => KeyCode::Esc,
            "DEL" => KeyCode::Backspace,
            "SPC" => KeyCode::Char(' '),
            _ if rest.starts_with('<') && rest.ends_with('>') && rest.len() > 2 => {
                let name = &rest[1..rest.len() - 1];
                match name {
                    "left" => KeyCode::Left,
                    "right" => KeyCode::Right,
                    "up" => KeyCode::Up,
                    "down" => KeyCode::Down,
                    "home" => KeyCode::Home,
                    "end" => KeyCode::End,
                    "pgup" | "prior" => KeyCode::PageUp,
                    "pgdn" | "next" => KeyCode::PageDown,
                    "delete" => KeyCode::Delete,
                    "insert" => KeyCode::Insert,
                    "backspace" => KeyCode::Backspace,
                    "tab" => KeyCode::Tab,
                    "return" => KeyCode::Enter,
                    "escape" => KeyCode::Esc,
                    f if f.starts_with('f') => KeyCode::F(f[1..].parse().ok()?),
                    _ => return None,
                }
            }
            _ => {
                let mut cs = rest.chars();
                let c = cs.next()?;
                if cs.next().is_some() {
                    return None;
                }
                KeyCode::Char(c)
            }
        };
        if matches!(k.code, KeyCode::Char(_)) {
            k.shift = false;
        }
        Some(k)
    }

    /// Emacs notation: `C-x`, `M-p`, `C-M-a`, `<f8>`, `RET`, `S-<tab>`.
    pub fn emacs(&self) -> String {
        let mut s = String::new();
        if self.ctrl {
            s.push_str("C-");
        }
        if self.alt {
            s.push_str("M-");
        }
        if self.shift {
            s.push_str("S-");
        }
        s.push_str(&match self.code {
            KeyCode::Enter => "RET".into(),
            KeyCode::Tab => "TAB".into(),
            KeyCode::Esc => "ESC".into(),
            KeyCode::Backspace => "DEL".into(),
            KeyCode::Char(' ') => "SPC".into(),
            KeyCode::Char(c) => c.to_string(),
            KeyCode::Left => "<left>".into(),
            KeyCode::Right => "<right>".into(),
            KeyCode::Up => "<up>".into(),
            KeyCode::Down => "<down>".into(),
            KeyCode::Home => "<home>".into(),
            KeyCode::End => "<end>".into(),
            KeyCode::PageUp => "<pgup>".into(),
            KeyCode::PageDown => "<pgdn>".into(),
            KeyCode::Delete => "<delete>".into(),
            KeyCode::Insert => "<insert>".into(),
            KeyCode::F(n) => format!("<f{n}>"),
            other => format!("<{other:?}>").to_lowercase(),
        });
        s
    }

    /// nano's notation, for its bar: `^X`, `M-U`, `F8`, arrows as glyphs.
    pub fn nano(&self) -> String {
        let base = match self.code {
            KeyCode::Char(c) if self.ctrl || self.alt => c.to_ascii_uppercase().to_string(),
            KeyCode::Char(' ') => "Space".into(),
            KeyCode::Char(c) => c.to_string(),
            KeyCode::F(n) => format!("F{n}"),
            KeyCode::Left => "\u{25c2}".into(),
            KeyCode::Right => "\u{25b8}".into(),
            KeyCode::Up => "\u{25b4}".into(),
            KeyCode::Down => "\u{25be}".into(),
            KeyCode::Enter => "Enter".into(),
            KeyCode::Tab => "Tab".into(),
            KeyCode::Esc => "Esc".into(),
            KeyCode::Backspace => "Bsp".into(),
            KeyCode::Delete => "Del".into(),
            KeyCode::Home => "Home".into(),
            KeyCode::End => "End".into(),
            KeyCode::PageUp => "PgUp".into(),
            KeyCode::PageDown => "PgDn".into(),
            other => format!("{other:?}"),
        };
        match (self.ctrl, self.alt) {
            (true, true) => format!("^M-{base}"),
            (true, false) => format!("^{base}"),
            (false, true) => format!("M-{base}"),
            (false, false) => base,
        }
    }
}

/// Parse a key sequence: chords separated by spaces (`M-t t`).
pub fn parse_seq(s: &str) -> Option<Vec<Key>> {
    s.split_whitespace().map(Key::parse).collect()
}

/// A sequence in emacs notation.
pub fn seq_emacs(keys: &[Key]) -> String {
    keys.iter().map(Key::emacs).collect::<Vec<_>>().join(" ")
}

/// A sequence in nano notation.
pub fn seq_nano(keys: &[Key]) -> String {
    keys.iter().map(Key::nano).collect::<Vec<_>>().join(" ")
}

/// What a key leads to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Entry {
    /// A command, by name.
    Command(String),
    /// A prefix: the next key is looked up here.
    Prefix(Keymap),
}

/// Keys to entries, in the order they were bound (which is the order cards and
/// the bar show them in).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Keymap {
    /// What the map is called when it is shown: `"global"`, `"todo"`, `"diff"`.
    pub name: String,
    entries: Vec<(Key, Entry)>,
}

impl Keymap {
    pub fn new(name: &str) -> Keymap {
        Keymap {
            name: name.to_string(),
            entries: Vec::new(),
        }
    }

    /// Bind `seq` (emacs notation, `"M-t t"`) to `command`, creating prefix
    /// maps on the way. A later binding of the same sequence replaces the
    /// earlier one. Panics on notation it cannot parse or on a sequence that
    /// would go through a key already bound to a command: a keymap is written
    /// by hand, and either is a typo to catch at once.
    pub fn bind(&mut self, seq: &str, command: &str) -> &mut Keymap {
        let keys = parse_seq(seq).unwrap_or_else(|| panic!("bad key sequence {seq:?}"));
        assert!(!keys.is_empty(), "empty key sequence");
        self.bind_keys(&keys, command, seq);
        self
    }

    fn bind_keys(&mut self, keys: &[Key], command: &str, seq: &str) {
        let (first, rest) = (keys[0], &keys[1..]);
        if rest.is_empty() {
            match self.entries.iter_mut().find(|(k, _)| *k == first) {
                Some((_, e)) => *e = Entry::Command(command.to_string()),
                None => self
                    .entries
                    .push((first, Entry::Command(command.to_string()))),
            }
            return;
        }
        if !self.entries.iter().any(|(k, _)| *k == first) {
            let name = first.emacs();
            self.entries
                .push((first, Entry::Prefix(Keymap::new(&name))));
        }
        match self.entries.iter_mut().find(|(k, _)| *k == first) {
            Some((_, Entry::Prefix(m))) => m.bind_keys(rest, command, seq),
            _ => panic!("{seq:?} goes through {}, which is a command", first.emacs()),
        }
    }

    /// Name a prefix map (what its card is titled).
    pub fn name_prefix(&mut self, seq: &str, name: &str) -> &mut Keymap {
        let keys = parse_seq(seq).unwrap_or_else(|| panic!("bad key sequence {seq:?}"));
        let mut map = self;
        for k in keys {
            map = match map.entries.iter_mut().find(|(e, _)| *e == k) {
                Some((_, Entry::Prefix(m))) => m,
                _ => panic!("{seq:?} is not a prefix"),
            };
        }
        map.name = name.to_string();
        map
    }

    /// The entry for one key, with a letter's case folded when only the other
    /// case is bound.
    pub fn get(&self, key: Key) -> Option<&Entry> {
        let find = |k: Key| self.entries.iter().find(|(e, _)| *e == k).map(|(_, v)| v);
        find(key).or_else(|| (key.folded() != key).then(|| find(key.folded())).flatten())
    }

    /// Keys and entries, in binding order.
    pub fn entries(&self) -> impl Iterator<Item = (&Key, &Entry)> {
        self.entries.iter().map(|(k, e)| (k, e))
    }

    /// Every (sequence, command) reachable from this map, depth first.
    pub fn flatten(&self) -> Vec<(Vec<Key>, String)> {
        let mut out = Vec::new();
        self.flatten_into(&mut Vec::new(), &mut out);
        out
    }

    fn flatten_into(&self, at: &mut Vec<Key>, out: &mut Vec<(Vec<Key>, String)>) {
        for (k, e) in &self.entries {
            at.push(*k);
            match e {
                Entry::Command(c) => out.push((at.clone(), c.clone())),
                Entry::Prefix(m) => m.flatten_into(at, out),
            }
            at.pop();
        }
    }
}

/// What a key sequence means in a stack of keymaps.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Lookup<'a> {
    /// A whole command.
    Command(&'a str),
    /// A prefix: more keys are needed, and this map says which.
    Prefix(&'a Keymap),
    /// Nothing, in any map.
    Unbound,
}

/// Resolve `seq` in `stack`, most specific map first: the first map in which
/// the sequence leads anywhere decides — so a mode's map shadows the global
/// one key by key, as emacs's minor and major mode maps do.
pub fn lookup<'a>(stack: &[&'a Keymap], seq: &[Key]) -> Lookup<'a> {
    for map in stack {
        let mut m: &Keymap = map;
        let mut found = None;
        for (i, k) in seq.iter().enumerate() {
            match m.get(*k) {
                Some(Entry::Command(c)) if i + 1 == seq.len() => {
                    found = Some(Lookup::Command(c.as_str()));
                }
                Some(Entry::Prefix(p)) if i + 1 == seq.len() => found = Some(Lookup::Prefix(p)),
                Some(Entry::Prefix(p)) => {
                    m = p;
                    continue;
                }
                _ => {}
            }
            break;
        }
        if let Some(f) = found {
            return f;
        }
    }
    Lookup::Unbound
}

/// Every sequence that reaches `command` in `stack` and is not shadowed by a
/// map above — emacs's `where-is`. Shortest first.
pub fn where_is(stack: &[&Keymap], command: &str) -> Vec<Vec<Key>> {
    let mut out: Vec<Vec<Key>> = Vec::new();
    for map in stack {
        for (seq, c) in map.flatten() {
            if c == command
                && lookup(stack, &seq) == Lookup::Command(command)
                && !out.contains(&seq)
            {
                out.push(seq);
            }
        }
    }
    out.sort_by_key(Vec::len);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::term::Mods;

    fn ev(code: KeyCode, m: Mods) -> KeyEvent {
        KeyEvent::new(code, m)
    }

    #[test]
    fn notation_round_trips() {
        for s in [
            "C-x", "M-p", "C-M-a", "<f8>", "RET", "TAB", "S-TAB", "DEL", "SPC", "<left>",
            "M-<left>", "<pgdn>", "<delete>", "M-<", "C-\\", "C-/", "M-?", "ESC", "x",
        ] {
            let k = Key::parse(s).unwrap_or_else(|| panic!("{s}"));
            assert_eq!(k.emacs(), s, "{s}");
        }
        assert_eq!(Key::parse("C--"), Some(Key::ctrl('-')));
        assert_eq!(
            parse_seq("M-t t").unwrap(),
            vec![Key::meta('t'), Key::parse("t").unwrap()]
        );
        assert_eq!(Key::parse("<nope>"), None);
        assert_eq!(Key::parse("ab"), None);
    }

    #[test]
    fn nano_notation_is_the_bars() {
        assert_eq!(Key::ctrl('x').nano(), "^X");
        assert_eq!(Key::meta('u').nano(), "M-U");
        assert_eq!(Key::parse("<f8>").unwrap().nano(), "F8");
        assert_eq!(Key::parse("C-<left>").unwrap().nano(), "^\u{25c2}");
    }

    /// The terminal's spellings come out as the keys a person pressed.
    #[test]
    fn terminal_events_normalise() {
        let c = Mods::CTRL;
        assert_eq!(Key::from_event(&ev(KeyCode::Char('4'), c)), Key::ctrl('\\'));
        assert_eq!(Key::from_event(&ev(KeyCode::Char('7'), c)), Key::ctrl('/'));
        assert_eq!(Key::from_event(&ev(KeyCode::Char('_'), c)), Key::ctrl('/'));
        let shifted = ev(KeyCode::Char('<'), Mods::ALT | Mods::SHIFT);
        assert_eq!(Key::from_event(&shifted), Key::meta('<'));
        assert_eq!(
            Key::from_event(&ev(KeyCode::BackTab, Mods::SHIFT)).emacs(),
            "S-TAB"
        );
        assert_eq!(
            Key::from_event(&ev(KeyCode::Char('a'), Mods::NONE)).printable(),
            Some('a')
        );
        assert_eq!(
            Key::from_event(&ev(KeyCode::Char('a'), c)).printable(),
            None
        );
    }

    fn decoded(bytes: &[u8]) -> Key {
        match &crate::term::decode(bytes)[..] {
            [crate::term::Event::Key(k)] => Key::from_event(k),
            other => panic!("{bytes:?} -> {other:?}"),
        }
    }

    /// **The keymap reads the terminal decoder's keys as the keys it binds.** The
    /// spellings checked are the ones a decoder could get wrong for an emacs map:
    /// `0x1c` is Ctrl+`\` and `0x1f` (Ctrl+`/` on most keyboards) is Ctrl+`_`, which
    /// must land on `C-\` and `C-/`; a NUL is `C-SPC`; Shift+Tab arrives as its own
    /// sequence; and the kitty keyboard's `CSI 99;5u` is plain `C-c`.
    #[test]
    fn the_keymap_sees_the_keys_the_decoder_reports() {
        assert_eq!(decoded(b"\x18"), Key::ctrl('x'));
        assert_eq!(decoded(b"\x1bx"), Key::meta('x'));
        assert_eq!(decoded(b"\x1c"), Key::ctrl('\\'));
        assert_eq!(decoded(b"\x1f"), Key::ctrl('/'));
        assert_eq!(decoded(b"\x00"), Key::ctrl(' '));
        assert_eq!(decoded(b"A"), Key::plain(KeyCode::Char('A')));
        let tab = decoded(b"\x1b[Z");
        assert_eq!((tab.code, tab.shift), (KeyCode::Tab, true));
        let up = decoded(b"\x1b[1;2A");
        assert_eq!((up.code, up.shift, up.ctrl), (KeyCode::Up, true, false));
        assert_eq!(decoded(b"\x1b[99;5u"), Key::ctrl('c'));
    }

    #[test]
    fn prefixes_nest_and_resolve() {
        let mut g = Keymap::new("global");
        g.bind("C-o", "save")
            .bind("M-t t", "todo-tick")
            .bind("M-t x", "todo-decline");
        g.name_prefix("M-t", "todo");
        let seq = |s: &str| parse_seq(s).unwrap();
        assert_eq!(lookup(&[&g], &seq("C-o")), Lookup::Command("save"));
        match lookup(&[&g], &seq("M-t")) {
            Lookup::Prefix(m) => {
                assert_eq!(m.name, "todo");
                assert_eq!(m.entries().count(), 2);
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(
            lookup(&[&g], &seq("M-t x")),
            Lookup::Command("todo-decline")
        );
        assert_eq!(lookup(&[&g], &seq("M-t q")), Lookup::Unbound);
        assert_eq!(lookup(&[&g], &seq("C-o x")), Lookup::Unbound);
        // nano's Meta keys ignore case.
        assert_eq!(
            lookup(&[&g], &[Key::meta('T'), Key::parse("t").unwrap()]),
            Lookup::Command("todo-tick")
        );
    }

    #[test]
    fn a_mode_map_shadows_the_global_one_key_by_key() {
        let mut g = Keymap::new("global");
        g.bind("n", "self-insert").bind("C-x", "exit");
        let mut diff = Keymap::new("diff");
        diff.bind("n", "next-conflict");
        let stack = [&diff, &g];
        assert_eq!(
            lookup(&stack, &[Key::parse("n").unwrap()]),
            Lookup::Command("next-conflict")
        );
        assert_eq!(lookup(&stack, &[Key::ctrl('x')]), Lookup::Command("exit"));
        assert_eq!(
            where_is(&stack, "self-insert"),
            Vec::<Vec<Key>>::new(),
            "shadowed"
        );
        assert_eq!(where_is(&stack, "exit"), vec![vec![Key::ctrl('x')]]);
    }

    #[test]
    fn where_is_lists_every_route_shortest_first() {
        let mut g = Keymap::new("global");
        g.bind("M-t t", "tick")
            .bind("<f9>", "tick")
            .bind("C-o", "save");
        let routes: Vec<String> = where_is(&[&g], "tick")
            .iter()
            .map(|s| seq_emacs(s))
            .collect();
        assert_eq!(routes, vec!["<f9>", "M-t t"]);
    }

    #[test]
    #[should_panic(expected = "which is a command")]
    fn binding_through_a_command_is_a_typo() {
        let mut g = Keymap::new("global");
        g.bind("C-o", "save").bind("C-o x", "other");
    }
}
