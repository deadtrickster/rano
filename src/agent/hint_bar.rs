//! **The hint bar**: the keys that do something right now.
//!
//! Ported from letibot's `crates/tui/src/ui/hint_bar.rs`. The composer owns the first half and
//! changes it after the first Esc or Ctrl+C — that is how anyone finds out a double-tap exists.
//! The head owns the second half, which is its own keys; which half applies is [`HintMode`].

use crate::render::{Line, Span};
use crate::style::Role;

/// What the head is showing, which decides what its keys do. The host picks the variant in
/// letibot's order of precedence (the quit card first, then a lost link, then whichever pane
/// or card is open, else the conversation).
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
                "↑↓ moves · enter or tab unfolds · pgup/pgdn and the wheel scroll · esc closes"
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
            // **What a chord says must be what the chord does** (R40). This bar read `ctrl-t
            // long output` while `/t`, which DOES the conversation-wide unfold, was on the bar
            // nowhere. **The pair is adjacent on purpose**: `ctrl-v` and `/t` are the two things
            // a reader confuses, so the bar states both, in its own nouns: one result against
            // all of them.
            //
            // **`tab completes /commands` gave up its space, and it is the one that should.**
            // The bar is over capacity by construction at 80 columns, so *which* entries are
            // visible is a decision: what gives way is the entry that answers before it is ever
            // named (Tab on a half-typed `/models` completes, unasked). `/help` stays, because
            // it is the index.
            HintMode::Conversation => {
                "ctrl-s sessions · ctrl-n notes · ctrl-t todos · ctrl-g subagents · ctrl-r thinking · ctrl-v newest result · /t all tool rows · ctrl-q jobs · ctrl-p hold · /help"
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
    pub fn line(&self, w: usize) -> Line {
        let tail = self.mode.tail();
        let mut line = Line::default();
        let editor = self.mode != HintMode::QuitCard && self.editor.width() > 0;
        // The separator belongs between two halves, not in front of one: with the editor's
        // half suppressed the bar used to open with a bare `·`.
        if editor {
            line.spans.extend(self.editor.spans.iter().map(|s| Span {
                content: s.content.clone(),
                style: self.editor.style.patch(&s.style),
            }));
            line.push(Span::role(format!(" · {tail}"), Role::Faint));
        } else {
            line.push(Span::role(tail, Role::Faint));
        }
        // **Centred — the operator's ask of 2026-10-04: *"please center the keymap bottom
        // line"*.** Measured on the visible width. **An over-long bar centres to itself** (pad
        // 0) and keeps the head, which is the half naming the first keys.
        let pad = w.saturating_sub(line.width()) / 2;
        if pad > 0 {
            line.spans.insert(0, Span::raw(" ".repeat(pad)));
        }
        crate::render::text::truncate(&line, w)
    }
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

    /// letibot `app/tests/transcript.rs::the_notes_chord_is_advertised_where_it_can_be_seen_and_reaches_nothing_else`
    /// (the bar half): at 80 columns `ctrl-n notes` is visible and `ctrl-s` still opens it.
    #[test]
    fn the_notes_chord_is_visible_at_80_columns() {
        let s = HintBar::default().line(80).plain();
        assert!(s.contains("ctrl-n notes"), "{s}");
        assert!(s.contains("ctrl-s sessions"), "{s}");
    }
}
