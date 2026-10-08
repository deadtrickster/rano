//! **The cards that ask the person for something typed**: a provider key, a terminal's
//! answer, a prompt, a secret (masked).
//!
//! Ported from letibot's `crates/tui/src/ui/cards/asks.rs`. Each card is the question; the
//! text is typed into the composer's field below it (masked for a key and a secret — see
//! [`super::composer::masked`] — in the open for a prompt), so none of these draws what is
//! being typed.
//!
//! Registers: letibot painted the headlines `sgr::YELLOW` and the rest `sgr::DIM`. Here the
//! headline is [`Role::Attention`] (yellow, bold: it exists to interrupt — it needs a person)
//! and the rest [`Role::Faint`].

use crate::render::Line;
use crate::style::Role;

use super::text::{clean, clean_line, countdown, one, trim_to, wrap};

fn faint(s: &str, w: usize) -> Line {
    one(trim_to(s, w), Role::Faint)
}

fn headline(s: &str, w: usize) -> Line {
    one(trim_to(s, w), Role::Attention)
}

/// **The key-ask card**: what is asking, and where the key goes.
///
/// letibot fills it from `App::key_ask` (`provider`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct KeyAsk {
    pub provider: String,
}

impl KeyAsk {
    pub fn lines(&self, w: usize) -> Vec<Line> {
        let provider = clean_line(&self.provider);
        let mut out = vec![headline(
            &format!("{provider} needs a key this box does not hold"),
            w,
        )];
        for l in wrap(
            &format!(
                "paste the {provider} key below — shown as dots, sent once to the daemon, stored at \
                 mode 600; it never enters the conversation or the transcript. Enter stores it \
                 and takes the row; Esc cancels"
            ),
            w,
        ) {
            out.push(one(l, Role::Faint));
        }
        out
    }
}

/// **The card that asks before a pane is ended** — the daemon's own question about killing
/// something, and never the program's question about itself.
///
/// # The words are the whole of the separation
///
/// The prompt card and this one can never be on the screen together, but a person has to know
/// which one they are looking at *by reading it*, so the registers are as different as two
/// cards can be: **this one names the program** and says what ending it does; **it quotes
/// nothing the program wrote** (a confirmation that showed that text would look like the
/// program's question); and **it spells both keys and says which is the default**, because a
/// destructive confirmation that leaves the reader to guess is a trap.
///
/// letibot fills it from `TermAsk::line` (the program's command line).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TermAsk {
    pub line: String,
}

impl TermAsk {
    pub fn lines(&self, w: usize) -> Vec<Line> {
        vec![
            headline(
                &format!("? end the pane — {} is running", clean_line(&self.line)),
                w,
            ),
            faint("  ending it kills the program and everything it started", w),
            // Not Enter: that key is the prompt card's and the composer's, and a stray one must
            // not be able to kill a program.
            faint(
                "  y ends it  ·  any other key (esc) leaves it running — that is the default",
                w,
            ),
        ]
    }
}

/// **The prompt card: a command of the operator's own is waiting for a line.**
///
/// The headline names the thing that is certain — **the command the operator typed** — because
/// the daemon cannot know which process in a pipeline asked: `sudo apt install mc` is three
/// programs and the question is the third one's. The program's own last line goes under it,
/// quoted and never parsed.
///
/// **No deadline, and that is not an omission**: a command blocked on its stdin is blocked
/// until somebody answers it, and a countdown invented here would be a number nobody measured.
/// **Not masked, and not a secret**: the composer draws the line in the open. **And it says the
/// manual way in** (`!send`), which needs no reading of the pipe at all.
///
/// letibot fills it from `PromptAsk { command, question, job }`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PromptAsk {
    pub command: String,
    /// The program's own last line. `None`: it has written nothing yet — a `read` blocked
    /// before its first byte — and the card says so rather than showing an empty line.
    pub question: Option<String>,
    /// The job handle, so the row can be found in `/jobs`.
    pub job: String,
}

impl PromptAsk {
    pub fn lines(&self, w: usize) -> Vec<Line> {
        let mut out = vec![headline("? your command is asking", w)];
        for l in wrap(
            &format!("run: {}", clean_line(&self.command)),
            w.saturating_sub(2),
        ) {
            out.push(one(format!("  {l}"), Role::Faint));
        }
        match self
            .question
            .as_deref()
            .map(clean)
            .as_deref()
            .map(str::trim)
        {
            Some(q) if !q.is_empty() => {
                for l in wrap(q, w.saturating_sub(4)) {
                    out.push(one(format!("  │ {l}"), Role::Faint));
                }
            }
            _ => out.push(faint("  │ (it has not written anything yet)", w)),
        }
        out.push(faint(
            &format!(
                "  enter sends it · esc puts the card away · `!send LINE` also works · {}",
                clean_line(&self.job)
            ),
            w,
        ));
        out
    }
}

/// **The password card**: what is asking, for which command, and the two keys.
///
/// **Two askers share this card**: sudo, through the askpass helper, and the daemon asking for
/// a provider's API key, which sends no command. The headline says which; the masking, the keys
/// and the countdown are the same.
///
/// letibot fills it from `SecretAsk { prompt, command, deadline }`, with
/// `remaining_ms = deadline.saturating_sub(now_ms)` — saturating, so an expired one reads `0s
/// left` rather than counting backwards.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SecretAsk {
    /// The asker's own words (`[sudo] password for dead:`).
    pub prompt: String,
    /// Empty for a key ask.
    pub command: String,
    pub remaining_ms: u64,
}

impl SecretAsk {
    pub fn lines(&self, w: usize) -> Vec<Line> {
        // **The same countdown the gate card draws** (§1.6): one function, so the two cards
        // cannot disagree about how long there is.
        let left = countdown(self.remaining_ms);
        let key = self.command.is_empty();
        // **And it says the thing ONCE.** The operator, having just answered one: *"the ask is
        // ugly as hell"*. The old first line read `sudo wants a password — [sudo] password for
        // dead:` — the same fact twice, in the same weight, trailing off a colon.
        let mut out = vec![headline(
            if key {
                "? an API key is needed"
            } else {
                "? sudo wants a password"
            },
            w,
        )];
        // **sudo's own words**, which name the account — faint and indented.
        for l in wrap(clean(&self.prompt).trim(), w.saturating_sub(2)) {
            out.push(one(format!("  {l}"), Role::Faint));
        }
        // The command keeps its own rows — the thing being authorised. `run:` names it, where
        // the old `for:` named nothing. A key ask has no command, so no row.
        if !key {
            for l in wrap(
                &format!("run: {}", clean_line(&self.command)),
                w.saturating_sub(2),
            ) {
                out.push(one(format!("  {l}"), Role::Faint));
            }
        }
        // **Short enough to survive a narrow terminal whole, keys first.** The old sentence ran
        // past a hundred columns and a trim cuts from the end — which is where the countdown
        // lives. Forty-two columns, so the countdown survives even a 44-column frame.
        out.push(faint(
            &format!("  enter sends it · esc refuses · {left}"),
            w,
        ));
        out
    }
}

super::lines_widget!(KeyAsk);
super::lines_widget!(TermAsk);
super::lines_widget!(PromptAsk);
super::lines_widget!(SecretAsk);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::testing::{drawn, plain, role_of};

    /// letibot `app/tests/asks.rs::the_key_ask_is_the_secret_card_worded_for_a_key` (the card).
    #[test]
    fn the_key_ask_is_the_secret_card_worded_for_a_key() {
        let card = SecretAsk {
            prompt: "deepseek needs an API key. It is saved to /h/.config/letibot/providers.toml"
                .into(),
            command: String::new(),
            remaining_ms: 600_000,
        };
        let s = plain(&card.lines(100)).join("\n");
        assert!(s.contains("? an API key is needed"), "{s}");
        assert!(s.contains("deepseek needs an API key"), "{s}");
        assert!(
            !s.contains("sudo"),
            "a key ask must not read as sudo's: {s}"
        );
        assert!(!s.contains("run:"), "there is no command to name: {s}");
    }

    /// letibot `app/tests/asks.rs::the_password_card_is_a_card_and_says_it_once`.
    #[test]
    fn the_password_card_is_a_card_and_says_it_once() {
        let card = SecretAsk {
            prompt: "[sudo] password for dead: ".into(),
            command: "sudo apt install x".into(),
            remaining_ms: 47_000,
        };
        let lines = card.lines(80);
        let s = plain(&lines).join("\n");
        assert!(lines[0].plain().starts_with("? "), "no marker: {s}");
        assert_eq!(role_of(&lines[0], "sudo"), Some(Role::Attention));
        assert_eq!(s.matches("sudo wants a password").count(), 1, "{s}");
        assert!(s.contains("[sudo] password for dead:"), "{s}");
        assert!(s.contains("run: sudo apt install x"), "{s}");
        // **The keys survive a narrow terminal along with the countdown.**
        for w in [44usize, 60, 80, 200] {
            let rows = card.lines(w);
            let last = rows.last().unwrap();
            assert!(last.width() <= w, "w={w}: {:?}", last.plain());
            let l = last.plain();
            assert!(
                l.contains("enter sends it") && l.contains("esc refuses"),
                "w={w}: {l}"
            );
            assert!(l.contains("left"), "w={w}: the countdown was cut: {l}");
        }
    }

    /// letibot `app/tests/asks.rs::the_password_cards_countdown_is_seconds_and_not_a_second_epoch`.
    #[test]
    fn the_password_cards_countdown_is_the_ladder() {
        let at = |ms| {
            plain(
                &SecretAsk {
                    prompt: "p".into(),
                    command: "c".into(),
                    remaining_ms: ms,
                }
                .lines(80),
            )
            .join("\n")
        };
        assert!(at(120_000).contains("expires in 2 min"));
        assert!(at(90_000).contains("1m30s left"));
        assert!(at(59_000).contains("59s left"));
        assert!(at(47_000).contains("47s left") && !at(47_000).contains("47.0"));
        assert!(at(0).contains("0s left"));
    }

    /// letibot `app/tests/asks.rs::a_prompt_card_owns_the_keys_shows_what_is_typed_and_never_becomes_a_secret`
    /// (the card).
    #[test]
    fn a_prompt_card_names_the_command_quotes_the_program_and_offers_send() {
        let card = PromptAsk {
            command: "sudo apt install mc".into(),
            question: Some("Do you want to continue? [Y/n] ".into()),
            job: "j7".into(),
        };
        let s = plain(&card.lines(100)).join("\n");
        assert!(s.contains("your command is asking"), "{s}");
        assert!(s.contains("run: sudo apt install mc"), "{s}");
        assert!(s.contains("  │ Do you want to continue? [Y/n]"), "{s}");
        assert!(s.contains("!send"), "{s}");
        assert!(s.contains("j7"), "the job handle is named: {s}");
        let silent = PromptAsk {
            question: None,
            ..card
        };
        assert!(
            plain(&silent.lines(100))
                .join("\n")
                .contains("(it has not written anything yet)")
        );
    }

    /// letibot `app/tests/asks.rs::a_question_from_the_run_takes_the_screen_back_from_the_confirmation`
    /// (the card's words): the confirmation names the program, quotes nothing, spells the keys.
    #[test]
    fn the_pane_confirmation_names_the_program_and_both_keys() {
        let rows = drawn(
            &TermAsk {
                line: "htop".into(),
            },
            100,
            3,
        );
        assert_eq!(rows[0], "? end the pane — htop is running");
        assert!(rows[2].contains("y ends it") && rows[2].contains("that is the default"));
    }

    #[test]
    fn the_key_card_says_where_the_key_goes() {
        let rows = KeyAsk {
            provider: "deepseek".into(),
        }
        .lines(60);
        assert_eq!(
            rows[0].plain(),
            "deepseek needs a key this box does not hold"
        );
        assert!(plain(&rows).join(" ").contains("mode 600"));
        for r in &rows {
            assert!(r.width() <= 60);
        }
    }
}
