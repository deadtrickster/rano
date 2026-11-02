//! What the terminal on the other end can do beyond drawing cells: seven
//! switches decided once here, each on only for a terminal known to speak it —
//! one that does not may print the sequence as garbage. `TERM=xterm-ghostty`
//! or `TERM_PROGRAM=ghostty` gets six of the seven ([`Features::GHOSTTY`]);
//! `RANO_TERM_FEATURES` overrides either way — `all`, `none`, or a comma list
//! where `-name` removes one (the old `LETIBOT_TERM_FEATURES` still works).
//! Inside tmux nothing is on unless the override says so: tmux mangles the
//! sequences, and `TERM` names tmux.

/// The seven, as switches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Features {
    /// Desktop notifications (OSC 9) and focus reporting (`?1004`), which says when to send one.
    pub notify: bool,
    /// The tab/titlebar progress bar (OSC 9;4).
    pub progress: bool,
    /// The kitty keyboard protocol (`CSI > 1 u`): Shift+Enter, an Esc never half an arrow key.
    pub keys: bool,
    /// Hyperlinks (OSC 8).
    pub links: bool,
    /// Writing the system clipboard (OSC 52).
    pub clipboard: bool,
    /// Inline images; which protocol carries them is [`Features::proto`].
    pub images: bool,
    /// Asking the terminal its background colour (OSC 11), to choose a light or dark palette.
    pub background: bool,
    /// How a picture is delivered when [`Features::images`] is on. A field of
    /// this value, not a second detection, so the two cannot disagree.
    pub proto: crate::term::Proto,
}

const NAMES: [&str; 7] = [
    "notify",
    "progress",
    "keys",
    "links",
    "clipboard",
    "images",
    "background",
];

impl Features {
    pub const ALL: Features = Features {
        notify: true,
        progress: true,
        keys: true,
        links: true,
        clipboard: true,
        images: true,
        background: true,
        proto: crate::term::Proto::Kitty,
    };

    /// What Ghostty gets with no override: everything but `progress` — Ghostty
    /// animates an indeterminate bar for the whole of a turn.
    pub const GHOSTTY: Features = Features {
        progress: false,
        ..Features::ALL
    };

    /// From this process's environment. Asks tmux's `allow-passthrough` only
    /// where it can change the answer (see [`crate::term::proto`]).
    pub fn detect() -> Features {
        let mut env = crate::term::proto::Env::now();
        let mut f = Features::of_env(&env, &words());
        if !f.images {
            // A name promises nothing over ssh; the terminal's own answer does.
            // One query, at startup, before input.
            f.proto = crate::term::Proto::detect(&env, true);
            // tmux is asked only when it can change the answer.
            if f.proto == crate::term::Proto::Kitty && env.tmux {
                env.ask_passthrough();
                f.proto = if env.passthrough == Some(true) {
                    crate::term::Proto::KittyPassthrough
                } else {
                    crate::term::Proto::None
                };
            }
            f.images = f.proto.speaks();
        }
        f
    }

    /// The decision from its four inputs — a function so it can be tested
    /// without the environment. `images` is the protocol's answer, not a
    /// switch; with no `Env`, tmux is assumed off.
    pub fn from_env(term: &str, term_program: &str, in_tmux: bool, wanted: &str) -> Features {
        let env = crate::term::proto::Env {
            term: term.to_string(),
            term_program: term_program.to_string(),
            tmux: in_tmux || term.starts_with("screen") || term.starts_with("tmux"),
            ..crate::term::proto::Env::default()
        };
        Features::of_env(&env, wanted)
    }

    /// The decision, from the environment as facts.
    fn of_env(env: &crate::term::proto::Env, wanted: &str) -> Features {
        let ghostty =
            env.term == "xterm-ghostty" || env.term_program.eq_ignore_ascii_case("ghostty");
        let mut f = if ghostty && !env.tmux {
            Features::GHOSTTY
        } else if env.tmux {
            // OSC 52 is the one sequence tmux passes along, so it stays on and
            // `Terminal::copy` must not decline it.
            Features {
                clipboard: true,
                ..Features::default()
            }
        } else {
            Features::default()
        };
        f.images = f.images || crate::term::proto::Proto::of(env, true).speaks();
        let words: Vec<&str> = wanted
            .split(',')
            .map(str::trim)
            .filter(|w| !w.is_empty())
            .collect();
        for word in &words {
            match *word {
                "all" => f = Features::ALL,
                "none" => f = Features::default(),
                w => {
                    let (on, name) = match w.strip_prefix('-') {
                        Some(n) => (false, n),
                        None => (true, w.strip_prefix('+').unwrap_or(w)),
                    };
                    if let Some(slot) = f.slot(name) {
                        *slot = on;
                    }
                }
            }
        }
        // Protocol last, after the override: naming `images` where nothing was
        // detected asserts kitty, the one placeholder protocol.
        f.proto = if !f.images {
            crate::term::Proto::None
        } else if crate::term::proto::Proto::of(env, true).speaks() {
            crate::term::proto::Proto::of(env, true)
        } else if names_images(&words) {
            if env.tmux {
                crate::term::Proto::KittyPassthrough
            } else {
                crate::term::Proto::Kitty
            }
        } else {
            crate::term::Proto::None
        };
        f
    }

    fn slot(&mut self, name: &str) -> Option<&mut bool> {
        Some(match name {
            "notify" => &mut self.notify,
            "progress" => &mut self.progress,
            "keys" => &mut self.keys,
            "links" => &mut self.links,
            "clipboard" => &mut self.clipboard,
            "images" => &mut self.images,
            "background" => &mut self.background,
            _ => return None,
        })
    }

    /// The names that are on, for `/status` and the start-up line.
    pub fn names(&self) -> Vec<&'static str> {
        let mut me = *self;
        NAMES
            .iter()
            .copied()
            .filter(|n| me.slot(n).is_some_and(|s| *s))
            .collect()
    }
}

/// Did the override name pictures — `images`, `+images` or `all`, not `-images`/`none`.
fn names_images(words: &[&str]) -> bool {
    words
        .iter()
        .any(|w| *w == "all" || *w == "images" || *w == "+images")
}

/// The rano name when set (even empty — a deliberate no-override), the old name otherwise.
pub fn override_words(rano: Option<String>, letibot: Option<String>) -> String {
    rano.or(letibot).unwrap_or_default()
}

fn words() -> String {
    override_words(
        std::env::var("RANO_TERM_FEATURES").ok(),
        std::env::var("LETIBOT_TERM_FEATURES").ok(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::term::Proto;

    #[test]
    fn ghostty_gets_all_but_the_progress_bar_and_nothing_else_gets_any_by_default() {
        let f = Features::from_env("xterm-ghostty", "", false, "");
        assert_eq!(f, Features::GHOSTTY);
        assert!(!f.progress, "the progress bar is off by default");
        assert_eq!(
            f.names(),
            vec![
                "notify",
                "keys",
                "links",
                "clipboard",
                "images",
                "background"
            ]
        );
        assert_eq!(
            Features::from_env("xterm-256color", "ghostty", false, ""),
            Features::GHOSTTY
        );
        assert_eq!(
            Features::from_env("xterm-ghostty", "", false, "+progress"),
            Features::ALL
        );
        assert_eq!(
            Features::from_env("xterm-ghostty", "", false, "progress"),
            Features::ALL
        );
        assert_eq!(
            Features::from_env("xterm-ghostty", "", false, "all"),
            Features::ALL
        );
        for (term, prog) in [
            ("xterm-256color", "Apple_Terminal"),
            ("xterm-256color", ""),
            ("dumb", ""),
        ] {
            assert_eq!(
                Features::from_env(term, prog, false, ""),
                Features::default(),
                "{term} {prog}"
            );
        }
        for (term, prog) in [
            ("xterm-kitty", ""),
            ("xterm-256color", "WezTerm"),
            ("xterm-256color", "iTerm.app"),
        ] {
            assert_eq!(
                Features::from_env(term, prog, false, "").names(),
                vec!["images"],
                "{term} {prog}"
            );
        }
        assert_eq!(
            Features::from_env("xterm-kitty", "", false, "-images").names(),
            Vec::<&str>::new()
        );
        let tmux = Features::from_env("xterm-ghostty", "ghostty", true, "");
        assert_eq!(tmux.names(), vec!["clipboard"]);
        assert_eq!(
            tmux,
            Features {
                clipboard: true,
                ..Features::default()
            }
        );
        assert_eq!(
            Features::from_env("screen-256color", "", false, "").names(),
            vec!["clipboard"]
        );
        assert_eq!(
            Features::from_env("tmux-256color", "", true, "").names(),
            vec!["clipboard"]
        );
    }

    #[test]
    fn pictures_and_their_protocol_agree() {
        assert_eq!(
            Features::from_env("xterm-ghostty", "", false, "").proto,
            Proto::Kitty
        );
        assert_eq!(
            Features::from_env("xterm-256color", "iTerm.app", false, "").proto,
            Proto::Iterm
        );
        assert_eq!(
            Features::from_env("xterm-256color", "Apple_Terminal", false, "").proto,
            Proto::None
        );
        // Insisting `all` where nothing was detected asserts kitty.
        let all = Features::from_env("xterm-256color", "", false, "all");
        assert!(all.images);
        assert_eq!(all.proto, Proto::Kitty);
        assert_eq!(
            Features::from_env("xterm-256color", "", false, "images").proto,
            Proto::Kitty
        );
        assert_eq!(
            Features::from_env("xterm-256color", "", true, "images").proto,
            Proto::KittyPassthrough
        );
        assert_eq!(
            Features::from_env("xterm-kitty", "", false, "-images").proto,
            Proto::None
        );
        assert_eq!(
            Features::from_env("xterm-ghostty", "", false, "none").proto,
            Proto::None
        );
        let tmux = Features::from_env("xterm-ghostty", "ghostty", true, "");
        assert!(!tmux.images);
        assert_eq!(tmux.proto, Proto::None);
    }

    #[test]
    fn the_override_adds_removes_and_replaces() {
        let f = Features::from_env("xterm-ghostty", "", false, "-keys");
        assert!(!f.keys && f.notify && f.images);
        let f = Features::from_env("xterm-256color", "", false, "links,clipboard");
        assert_eq!(f.names(), vec!["links", "clipboard"]);
        assert_eq!(
            Features::from_env("xterm-ghostty", "", false, "none"),
            Features::default()
        );
        // `all` under tmux is delivered as the wrapped form.
        assert_eq!(
            Features::from_env("xterm-256color", "", true, "all"),
            Features {
                proto: Proto::KittyPassthrough,
                ..Features::ALL
            }
        );
        assert_eq!(
            Features::from_env("xterm-256color", "", false, "all"),
            Features::ALL
        );
        assert_eq!(override_words(None, Some("all".into())), "all");
        assert_eq!(
            override_words(Some("none".into()), Some("all".into())),
            "none"
        );
        assert_eq!(override_words(Some(String::new()), Some("all".into())), "");
        assert_eq!(override_words(None, None), "");
        // An unknown word is ignored rather than refusing to start a head.
        assert_eq!(
            Features::from_env("xterm-256color", "", false, "sparkles"),
            Features::default()
        );
    }
}
