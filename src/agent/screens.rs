//! **The frame's one-line sentences and its two empty screens**: what the head draws while
//! it is still asking the daemon, what a session that has said nothing looks like, the
//! held-view and pane-behind sentences, the notices, and the system and segment rows.
//!
//! Ported from the painting left in letibot's `ui/screen.rs`, `ui/transcript/window.rs` and
//! `ui/transcript/item.rs`. Each is small, and each is here so that every row on the screen
//! comes out of one renderer: the head says what is true and this says how it looks.

use crate::render::{Line, Span};
use crate::style::Role;

use super::loading::{CAT_SLOT, cat_frame, centred};
use super::text::{clean, duration, one, trim_to, wrap};

/// How long an attach may go unanswered before the screen says how to get out.
pub const ATTACH_IMPATIENT: u64 = 2_000;

/// **The wait, as a walking cat at the centre of the conversation** — drawn while the head
/// is still asking the daemon for the session, rather than the empty-session banner, which is
/// a claim about the session a head that has not been answered is in no position to make.
///
/// Dead centre: `room / 2` blank rows above, each row centred. **The cat is padded to the
/// widest frame**, so it is a fixed thing whose expression changes rather than one that slides
/// left and right; it is **alone** on its row, so "centred" is about the cat. A pawprint trail
/// under it, so a still frame still reads as going somewhere. Past [`ATTACH_IMPATIENT`] it
/// says how to get out: under that it is noise on a wait usually over before it is read.
pub fn attaching(elapsed_ms: u64, room: usize, w: usize) -> Vec<Line> {
    let cat = format!("{cat:<CAT_SLOT$}", cat = cat_frame(elapsed_ms));
    let mut rows: Vec<Line> = vec![Line::default(); room / 2];
    rows.push(centred(&cat, w));
    rows.push(Line::default());
    rows.push(centred("· · · · ›", w));
    rows.push(Line::default());
    rows.push(centred("asking the daemon for this session", w));
    rows.push(Line::default());
    rows.push(centred(&duration(elapsed_ms), w));
    if elapsed_ms >= ATTACH_IMPATIENT {
        rows.push(Line::default());
        rows.push(centred(
            "the daemon has not answered. ctrl-c twice, or wait",
            w,
        ));
    }
    rows
}

/// **A session that has said nothing yet**: the name, and the three sentences that say what
/// to do and what closing the window does. An empty screen with a status line under it is
/// indistinguishable from a head that attached to the wrong socket.
pub fn opening() -> Vec<Line> {
    vec![
        one("letibot", Role::Strong),
        Line::default(),
        one(
            "attached, and this session has said nothing yet. Type a question and press enter.",
            Role::Faint,
        ),
        one(
            "The turn runs in the daemon: closing this window does not stop it, and \
             reattaching picks it up.",
            Role::Faint,
        ),
        Line::default(),
        one("/help lists the keys.", Role::Faint),
    ]
}

/// **The disclosure of a held view** (R36): the state, and the act that undoes it — the
/// reader who cannot tell pinned from following scrolls to find out, which is the affordance
/// failing.
pub fn holding(behind: usize) -> Line {
    one(
        format!(
            "── holding your place · {behind} line(s) below arrive underneath and do not move \
             this · ↑↓ pgup/pgdn move · ↓ to the bottom or esc follows again"
        ),
        Role::Pending,
    )
}

/// A pane this head is not drawing, still running, and the two verbs that do something about
/// it — drawn while it is true and gone the moment it is not.
pub fn pane_behind(line: &str, w: usize) -> Line {
    one(
        trim_to(
            &format!(
                "a pane is running: {} — `!term` attaches, `!term close` ends it",
                super::text::clean_line(line)
            ),
            w,
        ),
        Role::Pending,
    )
}

/// A notice the head is saying for a moment: `· sentence`, in the notice's own colour.
pub fn notice(text: &str, w: usize) -> Line {
    one(trim_to(&format!("· {text}"), w), Role::Keyword)
}

/// A sentence that owns the keyboard while it is up (the mode card's confirmation): wrapped,
/// not trimmed — it is read, not glanced at — in the attention register.
pub fn asking(text: &str, w: usize) -> Vec<Line> {
    wrap(text, w)
        .into_iter()
        .map(|l| one(l, Role::Attention))
        .collect()
}

/// A row the system wrote into the conversation: its origin, then its text, faint.
pub fn system_row(origin: &str, text: &str, w: usize) -> Vec<Line> {
    let mut out = vec![one(format!("system ({origin})"), Role::Faint)];
    out.extend(
        wrap(&clean(text), w)
            .into_iter()
            .map(|l| one(l, Role::Faint)),
    );
    out
}

/// A segment mark: `─── label ───`, faint.
pub fn segment_mark(label: &str) -> Line {
    one(format!("─── {label} ───"), Role::Faint)
}

/// **A call the model asked for that nothing came back for** — `→ Read foo.rs · no result`,
/// stepped in with the working, in the attention register: `→` means exactly "asked for,
/// nothing came back", which is a fact worth a row.
pub fn unanswered_call(verb: &str, subject: &str, step: usize) -> Line {
    Line::new(vec![
        Span::raw(" ".repeat(step)),
        Span::role(format!("→ {verb} {subject} · no result"), Role::Attention),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::testing::plain;
    use crate::style::Palette;

    /// The cat is centred and fixed-width, and the way out appears only once it is late.
    #[test]
    fn the_attach_wait_is_a_centred_cat_and_says_the_way_out_when_late() {
        let early = plain(&attaching(500, 10, 60));
        assert_eq!(early[..5].iter().filter(|l| l.is_empty()).count(), 5);
        assert!(early[5].trim().starts_with("(=^"), "{early:#?}");
        assert!(!early.iter().any(|l| l.contains("has not answered")));
        let late = plain(&attaching(3_000, 10, 60));
        assert!(late.iter().any(|l| l.contains("has not answered")));
        assert!(late.iter().any(|l| l.trim() == "3.0s"));
    }

    #[test]
    fn the_sentences_have_their_registers() {
        assert_eq!(
            notice("saved", 80).to_ansi(Palette::Colour),
            "\x1b[35m· saved\x1b[0m"
        );
        assert!(
            holding(3)
                .plain()
                .starts_with("── holding your place · 3 line(s)")
        );
        assert_eq!(opening()[0].plain(), "letibot");
        assert_eq!(segment_mark("compacted").plain(), "─── compacted ───");
        assert_eq!(
            unanswered_call("Read", "TODO.md", 2).to_ansi(Palette::Colour),
            "  \x1b[1;33m→ Read TODO.md · no result\x1b[0m"
        );
        assert_eq!(
            plain(&system_row("Harness", "a\x1b[31mb", 80)),
            vec!["system (Harness)", "ab"]
        );
    }
}
