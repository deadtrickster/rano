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
    /// Inline images — see [`Features::proto`] for *which* protocol said so.
    pub images: bool,
    /// Asking the terminal its background colour (OSC 11), to choose a light or dark palette.
    pub background: bool,
    /// **How a picture is delivered**, when [`Features::images`] says one may be: the terminal's
    /// protocol, or [`Proto::None`](crate::term::Proto::None).
    ///
    /// It is a field of this value rather than a second detection call, because the two answers
    /// come from one environment and must not be able to disagree — a renderer that believed
    /// *pictures are on* and a protocol that said *none* is a head drawing placeholders at a
    /// terminal that fills none of them.
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

    /// **What Ghostty gets with no override**: everything but `progress`. A busy tab's bar is
    /// indeterminate, and Ghostty animates that for the whole of a turn — motion in the corner of
    /// the eye that says nothing a glance at the tab would not.
    pub const GHOSTTY: Features = Features {
        progress: false,
        ..Features::ALL
    };

    /// From this process's environment.
    ///
    /// **The one question this asks outside the process is tmux's `allow-passthrough`**, and only
    /// where it can change the answer: inside a multiplexer, in front of a terminal that speaks
    /// the kitty protocol. The spawn is a `tmux show-options` round trip, and what it buys is the
    /// difference between a picture and a sequence tmux swallows without saying so — see
    /// [`crate::term::proto`].
    pub fn detect() -> Features {
        let mut env = crate::term::proto::Env::now();
        let mut f = Features::of_env(&env, &words());
        if !f.images {
            // **Ask when the name said nothing.** Over ssh `TERM` is conservative and
            // `TERM_PROGRAM` does not travel, so a name is the one thing a remote session cannot
            // promise; the terminal's own answer is. One query, at startup, before input.
            f.proto = crate::term::Proto::detect(&env, true);
            // tmux is asked only when it can change the answer: a terminal that said no is a
            // picture that does not happen, and a process buys nothing.
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

    /// The decision, from the four inputs that make it — a function so it can be tested
    /// without touching the environment.
    ///
    /// **`images` is not a switch of its own**: it is [`crate::term::Proto::of`]'s answer for this
    /// environment, because *does this terminal take a picture* is a question about the terminal
    /// and not about this program. Ghostty, kitty, WezTerm and iTerm2 speak one of the two
    /// protocols and get it; Terminal.app does not and does not. tmux is the exception it has
    /// always been — nothing crosses it without `allow-passthrough`, which [`Features::detect`]
    /// asks tmux for and this function, having no `Env` with the answer in it, assumes off.
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
            // One sequence survives a multiplexer, and it is the one copying needs: OSC 52 is
            // what tmux exists to pass along, to the outer terminal or to its own paste buffer,
            // so emitting it is never wrong and `Terminal::copy` must not decline it.
            Features {
                clipboard: true,
                ..Features::default()
            }
        } else {
            Features::default()
        };
        // **Pictures are the one extra that is the terminal's to grant, not this file's** — see
        // this function's doc. Asked last, so the override below still has the final word.
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
        // **And the protocol, last, because the override has had its say by now.** The operator
        // naming `images` (or `all`) for a terminal this build cannot place is an assertion about
        // that terminal, and the one protocol that draws *by placeholder* — the only one a frame
        // this program diffs can keep — is kitty. Everywhere else the names decide.
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

/// **Did the override name pictures at all** — `images`, `+images` or `all`, and not
/// `-images`/`none`, which turn them off outright.
fn names_images(words: &[&str]) -> bool {
    words
        .iter()
        .any(|w| *w == "all" || *w == "images" || *w == "+images")
}

/// The override's words: the rano-neutral name when it is set (even to empty, which is a
/// deliberate "no override"), the letibot name otherwise.
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
            ("xterm-256color", ""),
            ("dumb", ""),
        ] {
            assert_eq!(
                Features::from_env(term, prog, false, ""),
                Features::default(),
                "{term} {prog}"
            );
        }
        // **Except pictures, which are the terminal's to grant** — kitty and WezTerm speak the
        // graphics protocol and said so in `TERM`/`TERM_PROGRAM`, so `images` is on for them and
        // for nothing else here. See `crate::term::proto`.
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
        // And `-images` takes it away from a terminal that speaks it perfectly.
        assert_eq!(
            Features::from_env("xterm-kitty", "", false, "-images").names(),
            Vec::<&str>::new()
        );
        // Under tmux every sequence is eaten or mangled except OSC 52, which tmux forwards
        // or keeps in its paste buffer; pictures cross only with `allow-passthrough`, which
        // `from_env` cannot know, so it claims none and `Features::detect` asks.
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

    /// **The features and the picture protocol are one answer, not two** — a value that says
    /// *pictures* beside a protocol that says *none* is a head drawing placeholders a terminal
    /// never fills.
    #[test]
    fn pictures_and_their_protocol_agree() {
        // The names decide where they can.
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
        // **The operator insisting is an assertion about a terminal this build cannot place**, and
        // the one protocol that draws by placeholder is kitty.
        let all = Features::from_env("xterm-256color", "", false, "all");
        assert!(all.images);
        assert_eq!(all.proto, Proto::Kitty);
        assert_eq!(
            Features::from_env("xterm-256color", "", false, "images").proto,
            Proto::Kitty
        );
        // Under tmux that insistence is the wrapped form, because nothing else crosses.
        assert_eq!(
            Features::from_env("xterm-256color", "", true, "images").proto,
            Proto::KittyPassthrough
        );
        // And `-images`/`none` take the protocol with them.
        assert_eq!(
            Features::from_env("xterm-kitty", "", false, "-images").proto,
            Proto::None
        );
        assert_eq!(
            Features::from_env("xterm-ghostty", "", false, "none").proto,
            Proto::None
        );
        // tmux in front of a kitty terminal, with no answer from tmux: no pictures and no
        // protocol, which is the honest pair (`Features::detect` is the one that asks).
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
        // **`all` under tmux is everything, delivered as the wrapped form** — the one picture
        // protocol that crosses a multiplexer, which is not what a direct terminal gets.
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
