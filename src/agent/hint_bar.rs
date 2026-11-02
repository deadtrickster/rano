//! The hint bar: the keys that do something right now.
//!
//! The composer owns the first half and changes it after the first Esc or Ctrl+C — that is how anyone
//! finds out a double-tap exists. The head owns the second half; which half applies is [`HintMode`].
//!
//! A tail names a host's chords, and only the host knows what its chords do, so a host that dispatches
//! from tables generates the tail from them and hands it to [`HintBar::line_with_tail`]. A pane that
//! draws through [`crate::agent::tree_list`] already has its own tail — `hint_text()` is the very
//! sentence the pane draws — and that is what a bar should carry, so the two cannot disagree.

use crate::render::{Line, Span};
use crate::style::Role;

/// What the head is showing, which decides what its keys do. The host picks the variant in order of
/// precedence: the quit card first, then a lost link, then whichever pane or card is open, else the
/// conversation. A host whose keys differ supplies its own tail rather than relying on these.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HintMode {
    QuitCard,
    Detached,
    /// `/help` or `/stats`.
    Reading,
    SessionPicker,
    /// A settings card's pick (a row number switches).
    Pick,
    Todos,
    Config,
    Subagents,
    /// A job's output, open from the jobs pane.
    JobOutput,
    /// A queue entry, open from the queue pane.
    QueueEntry,
    Queue,
    Jobs,
    /// A decision card is open.
    Deciding,
    /// The host's editor pane is open and has the keyboard.
    Editor,
    /// The host's editor pane is open and the composer has the keyboard.
    EditorBehind,
    #[default]
    Conversation,
}

impl HintMode {
    /// The head's half.
    pub fn tail(self) -> &'static str {
        match self {
            // The footer used to say "ctrl+c again to exit", which stopped being true the moment the
            // second press opened a card instead: a hint naming the wrong key is worse than none.
            HintMode::QuitCard => "1/2 or ↑↓ then enter · esc stays",
            // The one thing the keys cannot do while the link is down, said where the keys are:
            // everything else still works, which is the point of keeping the head up.
            HintMode::Detached => {
                "no daemon connection · enter holds your line · this head keeps trying"
            }
            HintMode::Reading => "esc closes this",
            HintMode::SessionPicker => "type a number to switch · /new [title] · esc closes",
            HintMode::Pick => "a row number switches · ↑↓ then enter · or type a name · esc closes",
            HintMode::Todos => {
                "↑↓ moves · enter reads a row whole · tab unfolds or toggles it · pgup/pgdn and the wheel scroll · esc closes"
            }
            HintMode::Config => "arrows move · enter changes a row marked ✎ · esc closes",
            HintMode::Subagents => {
                "↑↓ moves · enter (or o) opens a subagent, or unfolds finished · p reads its output · esc closes"
            }
            HintMode::JobOutput => "↑↓ scroll · → next page · ← back · esc back to jobs",
            HintMode::QueueEntry => "↑↓ scroll · esc back to the queue",
            HintMode::Queue => {
                "↑↓ moves · enter opens the entry: its ask, the gate's words, the reviewer's verdict · esc closes"
            }
            HintMode::Jobs => "↑↓ moves · enter reads a job, or unfolds finished · esc closes",
            HintMode::Deciding => {
                "a row number answers · ↑↓ then enter · or type an option · /help"
            }
            // The pane's own keys, and the two that leave it: the editor's own bar names its keys, and
            // this row names what the host adds — the way to the composer, the send, and the way out.
            HintMode::Editor => {
                "ctrl-] to the composer · alt-s sends your place to it · alt-p the change · ctrl-q closes (asks to save)"
            }
            HintMode::EditorBehind => {
                "ctrl-] back to the editor · enter sends · the editor keeps your place · /help"
            }
            // What a chord says must be what the chord does: the pair is adjacent on purpose, because
            // `ctrl-v` and `/t` are the two a reader confuses — one result against all of them. `ctrl-n
            // clears notes` names the act, not a pane it would open, and `tab` gave up its space because
            // it answers before it is ever named.
            HintMode::Conversation => {
                "ctrl-s sessions · ctrl-n clears notes · ctrl-t todos · ctrl-g subagents · ctrl-r thinking · ctrl-v newest result · /t all tool rows · ctrl-q jobs · ctrl-p hold · /help"
            }
        }
    }
}

/// The bottom bar's view model.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct HintBar {
    /// The composer's half. Ignored under [`HintMode::QuitCard`]: the card is the second press, and the
    /// editor's "ctrl+c again to exit" would name a key that now closes it.
    pub editor: Line,
    pub mode: HintMode,
}

impl HintBar {
    /// The bar at `w` columns: the composer's half, then [`HintMode::tail`], centred.
    pub fn line(&self, w: usize) -> Line {
        self.line_with_tail(self.mode.tail(), w)
    }

    /// The bar with a host's own tail in place of [`HintMode::tail`]'s sentence, laid out exactly as
    /// [`HintBar::line`] lays out the built-in one: the composer's half first, a `·` between the halves,
    /// centred, and an over-long bar keeping its head. An empty `tail` draws the composer's half alone.
    pub fn line_with_tail(&self, tail: &str, w: usize) -> Line {
        let editor = self.mode != HintMode::QuitCard && self.editor.width() > 0;
        let head = if editor { Some(&self.editor) } else { None };
        centred(head, tail, w)
    }
}

/// The bar's layout, for any two halves: `head` (its spans patched with its line style), then ` · ` and
/// `tail` in [`Role::Faint`], centred in `w` columns and cut to them. Public for a host whose bar has a
/// first half that is not the composer's.
pub fn centred(head: Option<&Line>, tail: &str, w: usize) -> Line {
    let mut line = Line::default();
    let head = head.filter(|h| h.width() > 0);
    // The separator belongs between two halves, not in front of one: with the editor's half suppressed
    // the bar used to open with a bare `·`.
    if let Some(head) = head {
        line.spans.extend(head.spans.iter().map(|s| Span {
            content: s.content.clone(),
            style: head.style.patch(&s.style),
        }));
        if !tail.is_empty() {
            line.push(Span::role(format!(" · {tail}"), Role::Faint));
        }
    } else if !tail.is_empty() {
        line.push(Span::role(tail.to_string(), Role::Faint));
    }
    // Centred, on the visible width. An over-long bar centres to itself and keeps the head, which is the
    // half naming the first keys.
    let pad = w.saturating_sub(line.width()) / 2;
    if pad > 0 {
        line.spans.insert(0, Span::raw(" ".repeat(pad)));
    }
    crate::render::text::truncate_owned(line, w)
}

impl crate::render::Widget for HintBar {
    fn render(&self, area: crate::render::Rect, buf: &mut crate::render::Buffer) {
        if !area.is_empty() {
            let w = area.width as usize;
            buf.set_line(area.x, area.y, &self.line(w), w);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::testing::role_of;

    #[test]
    fn the_keymap_line_is_centred() {
        let bar = HintBar::default();
        for w in [40usize, 60, 80, 100, 120, 210] {
            let l = bar.line(w);
            let s = l.plain();
            let left = s.chars().take_while(|c| *c == ' ').count();
            let right = w.saturating_sub(l.width());
            assert!(left.abs_diff(right) <= 1, "w={w}: {left} vs {right}: {s:?}");
        }
        // And the over-long case keeps its head rather than losing both ends to the middle.
        let narrow = bar.line(20);
        assert_eq!(narrow.width(), 20, "{:?}", narrow.plain());
        assert!(narrow.plain().starts_with("ctrl-s"), "{:?}", narrow.plain());
    }

    /// After the first press the composer's half says `again`, and the head's half follows it after a
    /// separator.
    #[test]
    fn the_editors_half_leads_and_the_quit_card_stands_alone() {
        let bar = HintBar {
            editor: Line::styled("ctrl+c again to exit", Role::Attention),
            mode: HintMode::Conversation,
        };
        let l = bar.line(400);
        assert!(
            l.plain()
                .trim_start()
                .starts_with("ctrl+c again to exit · ctrl-s")
        );
        assert_eq!(role_of(&l, "again"), Some(Role::Attention));
        assert_eq!(role_of(&l, "ctrl-s"), Some(Role::Faint));
        let quit = HintBar {
            mode: HintMode::QuitCard,
            ..bar
        };
        let s = quit.line(80).plain();
        assert!(!s.contains("again"), "{s}");
        assert_eq!(s.trim(), "1/2 or ↑↓ then enter · esc stays");
    }

    /// An editor pane names the way back to whichever side does not have the keys.
    #[test]
    fn the_editor_pane_names_the_chord_that_crosses_to_the_other_side() {
        let on = HintBar {
            mode: HintMode::Editor,
            ..HintBar::default()
        };
        let s = on.line(200).plain();
        assert!(s.contains("ctrl-] to the composer"), "{s}");
        assert!(s.contains("ctrl-q closes"), "{s}");
        let behind = HintBar {
            mode: HintMode::EditorBehind,
            ..HintBar::default()
        };
        assert!(
            behind
                .line(200)
                .plain()
                .contains("ctrl-] back to the editor")
        );
    }

    /// At 80 columns `ctrl-n clears notes` is visible and says what the chord does, not a pane it would
    /// open.
    #[test]
    fn the_notes_chord_is_visible_at_80_columns() {
        let s = HintBar::default().line(80).plain();
        assert!(s.contains("ctrl-n clears notes"), "{s}");
        assert!(!s.contains("ctrl-n notes"), "{s}");
        assert!(s.contains("ctrl-s sessions"), "{s}");
    }

    /// A host's tail is laid out exactly as the built-in one: the same bytes and roles at every width,
    /// with and without the composer's half.
    #[test]
    fn a_hosts_tail_is_laid_out_as_the_built_in_one() {
        let editor = Line::styled("ctrl+c again to exit", Role::Attention);
        for mode in [HintMode::Conversation, HintMode::Todos, HintMode::QuitCard] {
            for ed in [Line::default(), editor.clone()] {
                let bar = HintBar { editor: ed, mode };
                for w in [10usize, 20, 40, 80, 120, 300] {
                    assert_eq!(
                        bar.line_with_tail(mode.tail(), w),
                        bar.line(w),
                        "{mode:?} w={w}"
                    );
                }
            }
        }
    }

    /// A tail the host made up is centred, follows the composer's half after one separator,
    /// keeps its head when it is too long, and is dropped with no stray `·` when empty.
    #[test]
    fn a_custom_tail_is_centred_and_follows_the_composers_half() {
        let tail = "↑↓ moves · enter reads it · esc closes";
        let bar = HintBar::default();
        for w in [40usize, 41, 80, 121] {
            let l = bar.line_with_tail(tail, w);
            let s = l.plain();
            assert_eq!(s.trim(), tail);
            let left = s.chars().take_while(|c| *c == ' ').count();
            let right = w - l.width();
            assert!(left.abs_diff(right) <= 1, "w={w}: {left} vs {right}: {s:?}");
            assert_eq!(role_of(&l, "moves"), Some(Role::Faint));
        }
        let narrow = bar.line_with_tail(tail, 12);
        assert_eq!(narrow.plain(), "↑↓ moves · …");

        let with = HintBar {
            editor: Line::styled("esc again to clear", Role::Attention),
            mode: HintMode::Conversation,
        };
        let l = with.line_with_tail(tail, 200);
        assert_eq!(l.plain().trim(), format!("esc again to clear · {tail}"));
        assert_eq!(role_of(&l, "again"), Some(Role::Attention));
        assert_eq!(
            with.line_with_tail("", 200).plain().trim(),
            "esc again to clear"
        );
        assert_eq!(HintBar::default().line_with_tail("", 20).plain().trim(), "");

        // The quit card still drops the composer's half under a custom tail.
        let quit = HintBar {
            mode: HintMode::QuitCard,
            ..with
        };
        assert_eq!(quit.line_with_tail("1 or 2", 80).plain().trim(), "1 or 2");
    }

    /// [`centred`] with a head of the host's own (a popup's keys) is the same layout.
    #[test]
    fn centred_takes_any_head() {
        let head = Line::styled("the diff: esc closes it", Role::Faint);
        let l = centred(Some(&head), "a row number answers", 60);
        assert_eq!(
            l.plain().trim(),
            "the diff: esc closes it · a row number answers"
        );
        let bar = HintBar {
            editor: head.clone(),
            mode: HintMode::Deciding,
        };
        assert_eq!(
            centred(Some(&head), HintMode::Deciding.tail(), 90),
            bar.line(90)
        );
        assert_eq!(centred(None, "x", 5).plain(), "  x");
    }
}
