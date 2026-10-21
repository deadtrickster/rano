//! **What the terminal on the other end can do beyond drawing cells.**
//!
//! The operator, 2026-10-07, on Ghostty: *"what ghostty specific things we can have?"* and then
//! *"i want all 7 you listed"*. Seven terminal features, each a sequence a terminal that does
//! not know it may print as garbage or act on wrongly — so each is switched on only for a
//! terminal known to speak it, and the set is one value decided once, here, rather than seven
//! `TERM` checks scattered through the head.
//!
//! # Detection
//!
//! `TERM=xterm-ghostty` (which survives `ssh` when the terminfo is installed on the far side —
//! the Linux box case) or `TERM_PROGRAM=ghostty` (local only; ssh does not forward it) turns on
//! six of the seven ([`Features::GHOSTTY`]) — all but the tab's progress bar, which the operator
//! turned off by default (2026-10-14): Ghostty draws a busy one as a bar sweeping side to side
//! for as long as a turn runs, and `+progress` brings it back. Nothing else does by default: kitty, WezTerm and iTerm2 speak some of them, each
//! a different some, and a guess about which is a garbled screen for somebody.
//!
//! `RANO_TERM_FEATURES` overrides the guess either way (`LETIBOT_TERM_FEATURES`, the name this
//! had in letibot, is still read when the new one is unset, so a shell profile set up for
//! letibot keeps working after letibot draws through rano): `all`, `none`, or a comma list of
//! the names below (`notify,progress,keys,links,clipboard,images,background`). A `-name`
//! removes one from whatever the detection gave, so `-keys` on Ghostty turns the kitty keyboard
//! off and leaves the rest.
//!
//! **Inside tmux nothing is on** unless the override says so: tmux eats or mangles most of
//! these unless its own passthrough is configured, and `TERM` there names tmux, not the
//! terminal outside it.

/// The seven, as switches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Features {
    /// Desktop notifications (OSC 9) and focus reporting (`?1004`), which says when to send one.
    pub notify: bool,
    /// The tab/titlebar progress bar (OSC 9;4).
    pub progress: bool,
    /// The kitty keyboard protocol (`CSI > 1 u`): Shift+Enter, an Esc that is never half an
    /// arrow key.
    pub keys: bool,
    /// Hyperlinks (OSC 8).
    pub links: bool,
    /// Writing the system clipboard (OSC 52).
    pub clipboard: bool,
    /// Inline images (the kitty graphics protocol).
    pub images: bool,
    /// Asking the terminal its background colour (OSC 11), to choose a light or dark palette.
    pub background: bool,
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
    };

    /// **What Ghostty gets with no override**: everything but `progress`. A busy tab's bar is
    /// indeterminate, and Ghostty animates that for the whole of a turn — motion in the corner of
    /// the eye that says nothing a glance at the tab would not.
    pub const GHOSTTY: Features = Features {
        progress: false,
        ..Features::ALL
    };

    /// From this process's environment.
    pub fn detect() -> Features {
        let var = |k: &str| std::env::var(k).unwrap_or_default();
        Features::from_env(
            &var("TERM"),
            &var("TERM_PROGRAM"),
            !var("TMUX").is_empty(),
            &override_words(
                std::env::var("RANO_TERM_FEATURES").ok(),
                std::env::var("LETIBOT_TERM_FEATURES").ok(),
            ),
        )
    }

    /// The decision, from the four inputs that make it — a function so it can be tested
    /// without touching the environment.
    pub fn from_env(term: &str, term_program: &str, in_tmux: bool, wanted: &str) -> Features {
        let ghostty = term == "xterm-ghostty" || term_program.eq_ignore_ascii_case("ghostty");
        let mut f = if ghostty && !in_tmux {
            Features::GHOSTTY
        } else if in_tmux || term.starts_with("screen") || term.starts_with("tmux") {
            // **One sequence survives a multiplexer, and it is the one copying needs.**
            // tmux eats or mangles everything else here — progress bars, notifications,
            // links, inline images — and the note that used to sit in this table said so
            // and stopped there. But OSC 52 is not like the rest: it is the sequence tmux
            // exists to pass along. With `set-clipboard on` it forwards it to the outer
            // terminal, and with the default `external` it keeps it in its own paste
            // buffer. Emitting it is therefore never wrong, and declining was: dragging a
            // selection in rano inside byobu copied nothing, silently, because
            // `Terminal::copy` returns early when this flag is off.
            Features {
                clipboard: true,
                ..Features::default()
            }
        } else {
            Features::default()
        };
        for word in wanted.split(',').map(str::trim).filter(|w| !w.is_empty()) {
            match word {
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

/// The override's words: the rano-neutral name when it is set (even to empty, which is a
/// deliberate "no override"), the letibot name otherwise.
pub fn override_words(rano: Option<String>, letibot: Option<String>) -> String {
    rano.or(letibot).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

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
        // And it is one word away, either way it is asked for.
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
            ("xterm-kitty", ""),
            ("xterm-256color", "WezTerm"),
            ("xterm-kitty", ""),
        ] {
            assert_eq!(
                Features::from_env(term, prog, false, ""),
                Features::default(),
                "{term} {prog}"
            );
        }
        // tmux in front of Ghostty: the sequences would be eaten or mangled — **except the
        // clipboard's**, which tmux forwards (or keeps in its paste buffer). That one flag,
        // and nothing else.
        let tmux = Features::from_env("xterm-ghostty", "ghostty", true, "");
        assert_eq!(tmux.names(), vec!["clipboard"]);
        assert_eq!(
            tmux,
            Features {
                clipboard: true,
                ..Features::default()
            }
        );
        // And the same for a terminal that only says `screen`, which is what byobu leaves
        // in `TERM` when it is not saying `tmux`.
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
    fn the_override_adds_removes_and_replaces() {
        let f = Features::from_env("xterm-ghostty", "", false, "-keys");
        assert!(!f.keys && f.notify && f.images);
        let f = Features::from_env("xterm-256color", "", false, "links,clipboard");
        assert_eq!(f.names(), vec!["links", "clipboard"]);
        assert_eq!(
            Features::from_env("xterm-ghostty", "", false, "none"),
            Features::default()
        );
        assert_eq!(
            Features::from_env("xterm-256color", "", true, "all"),
            Features::ALL
        );
        // The old name still works, and the new one wins when both are set.
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
