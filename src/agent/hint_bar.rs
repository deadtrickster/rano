//! **The hint bar**: the keys that do something right now.
//!
//! Ported from letibot's `crates/tui/src/ui/hint_bar.rs`. The composer owns the first half and
//! changes it after the first Esc or Ctrl+C — that is how anyone finds out a double-tap exists.
//! The head owns the second half, which is its own keys; which half applies is [`HintMode`].
//!
//! # Whose sentences these are
//!
//! A tail names a host's chords, and only the host knows what its chords do. So a host that
//! dispatches its keys from tables should generate the tail from those tables and hand it to
//! [`HintBar::line_with_tail`] (or [`centred`]), which lays it out the way [`HintBar::line`]
//! does. The built-in sentences ([`HintMode::tail`]) are defaults for a host that has no
//! tables of its own.
//!
//! letibot (since its R69) generates the tail for every pane and for the conversation, and
//! reads only these modes' built-in sentences: `QuitCard`, `Detached`, `Pick`, `Deciding`,
//! `Editor`, `EditorBehind` — the cards and the editor pane, whose keys are rano's. The other
//! modes' sentences are kept, corrected, for other hosts.
//!
//! **A pane that draws through [`crate::agent::tree_list`] has its own tail already**, and a
//! host that opens one should pass that rather than a mode's sentence: `hint_text()` on the
//! session picker, the subagents pane and the jobs pane is the very sentence the pane draws
//! under its rows, computed from the row the cursor is on, so the bar and the pane cannot
//! disagree about what Enter does where.

use crate::render::{Line, Span};
use crate::style::Role;

/// What the head is showing, which decides what its keys do. The host picks the variant in
/// letibot's order of precedence (the quit card first, then a lost link, then whichever pane
/// or card is open, else the conversation).
///
/// The pane modes (`Reading` through `Jobs`) and `Conversation` describe a host's own chords
/// and panes; a host whose keys differ supplies its own tail with
/// [`HintBar::line_with_tail`] rather than relying on these.
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
            // The footer said "ctrl+c again to exit" here, which stopped being true the moment
            // the second press started opening a card instead: a third press now CLOSES it. A
            // hint that names the wrong key is worse than none.
            HintMode::QuitCard => "1/2 or ↑↓ then enter · esc stays",
            // **The one thing the keys cannot do while the link is down**, said where the keys
            // are described: enter is held rather than sent. Everything else still works, which
            // is the point of keeping the head up.
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
            // **The pane's own keys, and the two that leave it.** The editor's own bar is two
            // rows above this one and names its keys; this row names what the HOST adds: the
            // way to the composer, the send that lands there, and the way out. `^X` is the
            // editor's own exit, kept because it asks about unsaved edits.
            HintMode::Editor => {
                "ctrl-] to the composer · alt-s sends your place to it · alt-p the change · ctrl-q closes (asks to save)"
            }
            HintMode::EditorBehind => {
                "ctrl-] back to the editor · enter sends · the editor keeps your place · /help"
            }
            // **What a chord says must be what the chord does** (R40). This bar read `ctrl-t
            // long output` while `/t`, which DOES the conversation-wide unfold, was on the bar
            // nowhere. **The pair is adjacent on purpose**: `ctrl-v` and `/t` are the two things
            // a reader confuses, so the bar states both, in its own nouns: one result against
            // all of them.
            //
            // **`ctrl-n clears notes`, not `ctrl-n notes`**: the chord retires every note, and a
            // bar that named it like the pane chords around it invited a reader to press it to
            // look (letibot R67 item 9).
            //
            // **`tab completes /commands` gave up its space, and it is the one that should.**
            // The bar is over capacity by construction at 80 columns, so *which* entries are
            // visible is a decision: what gives way is the entry that answers before it is ever
            // named (Tab on a half-typed `/models` completes, unasked). `/help` stays, because
            // it is the index.
            HintMode::Conversation => {
                "ctrl-s sessions · ctrl-n clears notes · ctrl-t todos · ctrl-g subagents · ctrl-r thinking · ctrl-v newest result · /t all tool rows · ctrl-q jobs · ctrl-p hold · /help"
            }
        }
    }
}

/// The bottom bar's view model.
///
/// letibot fills it from: `editor` ← `editor.hint(now_ms, palette)` (the composer's own
/// double-tap hint, already a line), `mode` ← the first of `quit_card`, `detached()`,
/// `help || stats`, `picker`, `pick`, `todos_pane`, `config_pane`, `subagents_pane`,
/// `job_out`, `queue_open`, `queue_pane`, `jobs_pane`, `!open.is_empty()` that holds.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct HintBar {
    /// The composer's half. Ignored under [`HintMode::QuitCard`]: while the card is up there is
    /// no double-tap left to learn — the card IS the second press — and the editor's "ctrl+c
    /// again to exit" would name a key that now closes the card instead.
    pub editor: Line,
    pub mode: HintMode,
}

impl HintBar {
    /// The bar at `w` columns: the composer's half, then [`HintMode::tail`], centred.
    pub fn line(&self, w: usize) -> Line {
        self.line_with_tail(self.mode.tail(), w)
    }

    /// **The bar with a host's own tail** in place of [`HintMode::tail`]'s sentence, laid out
    /// exactly as [`HintBar::line`] lays out the built-in one: the composer's half first (still
    /// dropped under [`HintMode::QuitCard`]), a `·` between the halves, centred, and an
    /// over-long bar keeping its head.
    ///
    /// For a host that generates its keys' sentences from the tables its keys are dispatched
    /// from, so the bar cannot name a chord that does something else in that host. `mode`
    /// still decides whether the composer's half is shown; its sentence is not used. An empty
    /// `tail` draws the composer's half alone, with no separator after it.
    pub fn line_with_tail(&self, tail: &str, w: usize) -> Line {
        let editor = self.mode != HintMode::QuitCard && self.editor.width() > 0;
        let head = if editor { Some(&self.editor) } else { None };
        centred(head, tail, w)
    }
}

/// **The bar's layout, for any two halves**: `head` (its spans patched with its line style),
/// then ` · ` and `tail` in [`Role::Faint`], centred in `w` columns and cut to them.
///
/// [`HintBar::line`] and [`HintBar::line_with_tail`] are this; it is public for a host whose
/// bar has a first half that is not the composer's (a popup's own keys, say).
pub fn centred(head: Option<&Line>, tail: &str, w: usize) -> Line {
    let mut line = Line::default();
    let head = head.filter(|h| h.width() > 0);
    // The separator belongs between two halves, not in front of one: with the editor's
    // half suppressed the bar used to open with a bare `·`.
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
    // **Centred — the operator's ask of 2026-10-04: *"please center the keymap bottom
    // line"*.** Measured on the visible width. **An over-long bar centres to itself** (pad
    // 0) and keeps the head, which is the half naming the first keys.
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

    /// letibot `app/tests/screen.rs::the_keymap_line_is_centred`.
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

    /// letibot `app/tests/screen.rs` / `subagents.rs`: after the first press the composer's
    /// half says `again`, and the head's half follows it after a separator.
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

    /// **An editor pane names the way back to whichever side does not have the keys**, so a
    /// reader who lands in either knows the one chord that crosses.
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

    /// letibot `app/tests/transcript.rs::the_notes_chord_is_advertised_where_it_can_be_seen_and_reaches_nothing_else`
    /// (the bar half): at 80 columns `ctrl-n clears notes` is visible and `ctrl-s` still opens
    /// it. It says what the chord does — it clears them — not a pane it would open.
    #[test]
    fn the_notes_chord_is_visible_at_80_columns() {
        let s = HintBar::default().line(80).plain();
        assert!(s.contains("ctrl-n clears notes"), "{s}");
        assert!(!s.contains("ctrl-n notes"), "{s}");
        assert!(s.contains("ctrl-s sessions"), "{s}");
    }

    /// **A host's tail is laid out exactly as the built-in one**: the same bytes and roles
    /// when the host hands back the mode's own sentence, at every width, with and without the
    /// composer's half — so a host that generates its tail loses nothing of the centring.
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
