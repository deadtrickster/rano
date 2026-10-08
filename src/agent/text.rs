//! **Small text rules the agent widgets share**: sanitising foreign text, durations and
//! byte counts a person reads, shortening a subject, and a few line helpers.
//!
//! Ported from letibot's `ui/transcript/call.rs`, `ui/paint.rs`, `ui/transcript/item.rs`,
//! `ui/render.rs` and `letibot_ui::progress`; the comments are theirs.

use std::borrow::Cow;

use crate::render::{Line, Span, Style};
use crate::style::Role;
use crate::width::text as wt;

pub use crate::width::text::{truncate as trim_to, width as visible_width, wrap};

/// **Text a host did not author, made safe to measure**: escape sequences removed whole,
/// every other control character a space, newlines kept.
///
/// The buffer already refuses to put an escape or a control into a cell, so this is not
/// what keeps a terminal safe — it is what keeps the *measurement* honest: a `\r` or a tab
/// left in a target would be counted by the wrapper and then not drawn, and a row measured
/// one way and drawn another is a row that misaligns. A space rather than a deletion,
/// because dropping it would silently reflow the line (letibot's `without_control`).
pub fn clean(s: &str) -> Cow<'_, str> {
    if !s.chars().any(|c| c != '\n' && wt::is_control(c)) {
        return Cow::Borrowed(s);
    }
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        match c {
            // `ESC` introduces a sequence; the sequence goes with it. **Whole**: the first
            // version of letibot's sanitiser turned the `ESC` into a space and left `[31m`
            // on the line — five columns of visible garbage where the terminal measured
            // none, and a wrap that counted them.
            '\u{1b}' => match it.peek().copied() {
                Some('[') => {
                    it.next();
                    skip_csi(&mut it);
                }
                Some(']') => {
                    it.next();
                    skip_osc(&mut it);
                }
                // Any other two-byte sequence (`ESC ( B`, `ESC =`): one more character
                // goes with it, if there is one.
                Some('\n') | None => {}
                Some(_) => {
                    it.next();
                }
            },
            // The C1 forms mean the same with no `ESC` in front: `CSI` (U+009B) and `OSC`
            // (U+009D) introduce, `ST` (U+009C) is a bare terminator.
            '\u{9b}' => skip_csi(&mut it),
            '\u{9d}' => skip_osc(&mut it),
            '\u{9c}' => {}
            '\n' => out.push('\n'),
            c if wt::is_control(c) => out.push(' '),
            c => out.push(c),
        }
    }
    Cow::Owned(out)
}

/// A `CSI` sequence's body, the introducer already consumed: `0x20..=0x3f` are parameters
/// and intermediates, and one byte in `0x40..=0x7e` ends it.
fn skip_csi(it: &mut std::iter::Peekable<std::str::Chars<'_>>) {
    while it.peek().is_some_and(|c| ('\u{20}'..='\u{3f}').contains(c)) {
        it.next();
    }
    if it.peek().is_some_and(|c| ('\u{40}'..='\u{7e}').contains(c)) {
        it.next();
    }
}

/// An `OSC` string, the introducer already consumed: it ends at `BEL` or at `ST`, and one
/// that never ends takes the rest of the line with it — which is what a terminal would do
/// with it too. A newline ends the line it was on, so it ends the string here.
fn skip_osc(it: &mut std::iter::Peekable<std::str::Chars<'_>>) {
    while let Some(&c) = it.peek() {
        if c == '\n' {
            return;
        }
        it.next();
        if c == '\u{7}' {
            return;
        }
        if c == '\u{1b}' {
            if it.peek() == Some(&'\\') {
                it.next();
            }
            return;
        }
    }
}

/// [`clean`] for text that must be one line: a newline becomes a space too.
pub fn clean_line(s: &str) -> String {
    clean(s).replace('\n', " ")
}

/// A duration a person reads: `412ms`, `4.2s`, `3m07s`, `1h04m`.
pub fn duration(ms: u64) -> String {
    if ms < 1000 {
        return format!("{ms}ms");
    }
    let s = ms / 1000;
    if s < 60 {
        return format!("{:.1}s", ms as f64 / 1000.0);
    }
    let (m, s) = (s / 60, s % 60);
    if m < 60 {
        return format!("{m}m{s:02}s");
    }
    format!("{}h{:02}m", m / 60, m % 60)
}

/// **How long there is, as a ladder rather than a number** (§1.6).
///
/// A countdown that ticks for five minutes is furniture and one that ticks for ten
/// seconds is a pressure nobody asked for; both are avoided by **changing the unit
/// with the time left**, so the reader gets the precision the moment is worth:
///
/// ```text
///  300s -> "expires in 5 min"      whole minutes, changing once a minute
///  181s -> "expires in 4 min"      rounded UP: never claim less time than there is
///  120s -> "expires in 2 min"      the boundary is inclusive
///  119s -> "1m59s left"            seconds from here, where the number is acted on
///   59s -> "59s left"              whole seconds under a minute — `47s`, never `47.0s`
///    0s -> "0s left"
/// ```
///
/// **Minutes round up and seconds do not.** A card must never claim less time than
/// there is. **It never goes negative**, and that is structural rather than a guard: the
/// input is a remaining time, and a caller with a deadline in the past passes zero (see
/// the decision card for the case where a past deadline needs a sentence of its own).
///
/// **Why the ladder and not just a format**: the finest rung is one whole second, so a
/// card needs one frame a second and not the ten a spinner needs.
pub fn countdown(remaining_ms: u64) -> String {
    // **The minutes are rounded from the MILLISECONDS, not from the seconds.**
    // `remaining_ms / 1000` first would floor 180,001 ms to 180 s and then call that
    // three minutes — a card claiming a second less than there is.
    if remaining_ms >= 120_000 {
        return format!("expires in {} min", remaining_ms.div_ceil(60_000));
    }
    let secs = remaining_ms / 1000;
    if secs >= 60 {
        return format!("{}m{:02}s left", secs / 60, secs % 60);
    }
    format!("{secs}s left")
}

/// A byte count a person can read. `8192` is a number to decode; `8.0 KB` is not.
pub fn bytes_human(n: u64) -> String {
    const K: u64 = 1024;
    match n {
        0..=1_023 => format!("{n} B"),
        _ if n < K * K => format!("{:.1} KB", n as f64 / K as f64),
        _ if n < K * K * K => format!("{:.1} MB", n as f64 / (K * K) as f64),
        _ => format!("{:.1} GB", n as f64 / (K * K * K) as f64),
    }
}

/// A braille spinner frame for `elapsed_ms`, driven off a clock the caller chose so a
/// replayed session animates the way the live one did.
pub fn spinner(elapsed_ms: u64) -> char {
    const FRAMES: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
    FRAMES[((elapsed_ms / 80) % FRAMES.len() as u64) as usize]
}

/// The first sentence of a refusal's reasoning, capped.
///
/// Layer A's `basis` is written for the model: it names every construct it could
/// not resolve, one indented paragraph each, and ends with the instruction to
/// re-issue. The operator needs the first clause of that — *what happened* — and
/// nothing else, because the rest is already in front of them as the tool result.
///
/// Cut at the first sentence end, then hard-capped: a "sentence" written without a
/// full stop is still not a paragraph a status line should carry. Borrows when the
/// first sentence is already in the input, which it usually is.
pub fn first_sentence(basis: &str) -> Cow<'_, str> {
    let line = basis.lines().next().unwrap_or("").trim();
    let end = line.find(". ").map(|i| i + 1).unwrap_or(line.len());
    let s = &line[..end];
    const CAP: usize = 140;
    if s.chars().count() <= CAP {
        return Cow::Borrowed(s);
    }
    let cut: String = s.chars().take(CAP).collect();
    Cow::Owned(format!("{}…", cut.trim_end()))
}

/// A result envelope's marker line: `<<<TOOL_ERROR 5ebfdef6>>>`, `<<<END_OK …>>>`.
///
/// Matched by SHAPE rather than against a list of kinds, so a kind added to the
/// envelope does not start leaking here on the day it lands. A body line that happens
/// to look like one cannot exist: the envelope rewrites every `<<<` in a payload to
/// `< < <` precisely so its own markers are unforgeable.
pub fn is_envelope(line: &str) -> bool {
    let l = line.trim();
    l.starts_with("<<<") && l.ends_with(">>>") && l.len() > 6
}

/// Drop a leading line-number gutter — `     1| ` — from one line of tool output.
///
/// Only ever applied to a **one-line preview inlaid on a header**, never to a
/// body: a body's gutter is how a reader refers to a line, and taking it away
/// there would lose a fact. On a header it is `1|` before the only line there is,
/// which is three columns saying "this is line one of one".
///
/// A prefix match rather than a parse of any tool's format. It matches what
/// `read` emits and nothing that is not shaped exactly like it; a tool whose
/// output happens to begin `12| ` gets three columns back and loses nothing.
pub fn strip_gutter(l: &str) -> String {
    let t = l.trim_start();
    let digits = t.len() - t.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    match t[digits..].strip_prefix("| ") {
        Some(rest) if digits > 0 => rest.trim_end().to_string(),
        _ => l.trim().to_string(),
    }
}

/// Shorten a path to `max` columns by eating its **left**, at a separator.
///
/// `…/worktrees/agent-a19da2/crates/tui`, not `~/Projects/letibot/.claud…`. A path
/// is recognised by where it ends; truncating from the right of a deep tree leaves
/// every session on this box looking identical.
///
/// Not [`crate::render::text::ellipsise_left`], which cuts at a cell and only nudges to
/// a separator nearby: this one drops *whole* segments.
pub fn ellipsise_left(s: &str, max: usize) -> String {
    if visible_width(s) <= max || max < 2 {
        return s.to_string();
    }
    // **At a separator, not at a character.** `…/1f0655c6-…/scratchpad` was the
    // operator's example and it is two lies in twenty-two columns: the first
    // ellipsis says a prefix was dropped, which is true, and the second says a
    // directory has a shorter name than it does, which is not — and neither
    // segment can be pasted back into a shell. Dropping *whole* segments leaves a
    // suffix that is a real path, which is what a person compares against.
    //
    // `match_indices` runs left to right, so the first candidate that fits is the
    // longest suffix that fits.
    if s.contains('/') {
        for (i, _) in s.match_indices('/') {
            let cand = format!("…{}", &s[i..]);
            if visible_width(&cand) <= max {
                return cand;
            }
        }
    }
    // A single segment longer than the whole allowance, or no separator at all.
    // Then there is nothing to cut on and the characters are all there is.
    let keep = max - 1;
    let mut out = String::new();
    let mut cols = 0usize;
    for c in s.chars().rev() {
        let cw = visible_width(&c.to_string());
        if cols + cw > keep {
            break;
        }
        out.push(c);
        cols += cw;
    }
    format!("…{}", out.chars().rev().collect::<String>())
}

/// Shorten a tool call's subject to `max` columns, cutting at the end a reader
/// does not need.
///
/// A **path** loses its left, at a separator: `…/crates/tui/src/app.rs` is still
/// a file you can recognise and `crates/tui/src/ap…` is not. Anything else — a
/// regex, a command line, a glob — loses its **right**, because those are read
/// from the start and the first token is the one that says what it is.
///
/// The test for "path" is a separator **and no glob metacharacter**. Measured at
/// 60 columns: `**/*.{md,json,toml,yaml,yml} 40` has a slash in it and cutting
/// its left gave `…json,toml,yaml,yml} 40`, which has lost the fact that it is a
/// glob at all. Cutting its right gives `**/*.{md,json,tom…`, which has not.
pub fn shorten_subject(s: &str, max: usize) -> String {
    if visible_width(s) <= max {
        return s.to_string();
    }
    // A glob metacharacter, or a quote — a display target quotes any argument
    // containing whitespace, so a leading `"` is how prose announces itself.
    // Measured: an `ask_code` call whose subject was a sentence with `src/` in the
    // middle of it left-cut to `…/ is responsible for, how main.rs, editor.rs,
    // and…`, which has thrown away the question and kept its tail.
    let not_a_path = s.contains(['*', '?', '{', '[', '"']);
    if s.contains('/') && !not_a_path {
        ellipsise_left(s, max)
    } else {
        trim_to(s, max)
    }
}

/// **Does the row's header already name the file its diff is of?** Then the diff's own name
/// line (the first row of an edit view) says it a second time — the operator, on
/// `▸ Wrote …/pr-body-align.md · ok` with the same path on the line under it: *"why two
/// times?"*. That line exists for the card whose header could NOT name the file (a call id in
/// its place — *"sometimes your Edited card doesnt have file name"*, 2026-10-05), and it stays
/// for that one. A relative target the excerpt's absolute path ends in is the same file.
pub fn header_names_the_file(target: &str, path: &str) -> bool {
    let t = target.trim();
    !t.is_empty() && (t == path || path.ends_with(&format!("/{t}")))
}

/// Set `lines` one step in. Empty rows stay empty: trailing spaces on a blank
/// line are invisible until something copies them.
pub fn step_in(lines: Vec<Line>, n: usize) -> Vec<Line> {
    if n == 0 {
        return lines;
    }
    let pad = " ".repeat(n);
    lines
        .into_iter()
        .map(|mut l| {
            if l.width() > 0 {
                l.spans.insert(0, Span::raw(pad.clone()));
            }
            l
        })
        .collect()
}

/// A line of `(text, role)` parts.
pub fn parts<S: Into<String>>(ps: impl IntoIterator<Item = (S, Role)>) -> Line {
    Line::new(ps.into_iter().map(|(s, r)| Span::role(s, r)).collect())
}

/// One line of text in one role.
pub fn one(s: impl Into<String>, r: Role) -> Line {
    Line::new(vec![Span::role(s, r)])
}

/// One line of text in one style.
pub fn styled(s: impl Into<String>, st: Style) -> Line {
    Line::new(vec![Span::styled(s, st)])
}

/// `s` wrapped to `w` columns, each row one line in role `r`.
pub fn wrapped(s: &str, w: usize, r: Role) -> Vec<Line> {
    wrap(s, w).into_iter().map(|l| one(l, r)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// letibot `sanitize::an_escape_sequence_goes_whole_and_never_leaves_its_body_behind`,
    /// with the C1 spellings: not one byte of a sequence's body is left as text, a lone
    /// control is a space, and the newlines stay.
    #[test]
    fn an_escape_sequence_goes_whole_and_never_leaves_its_body_behind() {
        let hostile = "a\u{1b}[31mred\u{1b}[0m \u{1b}[8mdim \u{1b}[2J clear \
                       \u{1b}[?1002h mouse \u{1b}]0;title\u{7} \u{9b}31m \u{9c} \u{7f} end\nnext";
        let safe = clean(hostile);
        assert_eq!(safe, "ared dim  clear  mouse      end\nnext");
        assert_eq!(
            clean("a\u{1b}[31mred\u{1b}[0m \u{1b}[8mdim \u{1b}[2Jclear"),
            "ared dim clear"
        );
        assert_eq!(clean("tab\there"), "tab here");
        assert!(matches!(clean("plain\ntext"), Cow::Borrowed(_)));
    }

    /// letibot `app/tests/commands.rs::a_path_is_shortened_at_a_separator_and_a_pattern_is_not`.
    #[test]
    fn a_path_is_shortened_at_a_separator_and_a_pattern_is_not() {
        assert_eq!(
            ellipsise_left("/home/dead/Projects/letibot/crates/tui", 22),
            "…/letibot/crates/tui",
            "whole segments, and the longest suffix that fits"
        );
        assert_eq!(
            shorten_subject("crates/tui/src/app.rs", 14),
            "…/src/app.rs",
            "a path loses its left"
        );
        assert_eq!(
            shorten_subject("^pub (fn|struct|enum)", 12),
            "^pub (fn|st…",
            "a pattern loses its right — it is read from the start"
        );
        assert_eq!(
            shorten_subject("**/*.{md,json,toml,yaml}", 12),
            "**/*.{md,js…",
            "and so does a glob, slash or no slash"
        );
        assert_eq!(
            shorten_subject("\"what is src/main.rs for\"", 12),
            "\"what is sr…",
            "a quoted sentence is prose with a slash in it, not a path"
        );
        // A single segment with no separator to cut on falls back to characters
        // rather than returning something wider than it was asked for.
        assert!(visible_width(&ellipsise_left("averylongsinglesegment", 10)) <= 10);
    }

    /// letibot `app/tests/tools.rs::an_edit_card_names_its_file_once` (its predicate half).
    #[test]
    fn a_header_names_the_file_when_the_path_ends_in_it() {
        assert!(header_names_the_file("a.rs", "a.rs"));
        assert!(header_names_the_file("src/a.rs", "/w/src/a.rs"));
        assert!(!header_names_the_file("a.rs", "/w/ba.rs"));
        assert!(!header_names_the_file("", "a.rs"));
    }

    /// letibot `app/tests/transcript.rs::a_reason_that_is_a_document_folds_to_its_first_sentence`.
    #[test]
    fn a_reason_that_is_a_document_folds_to_its_first_sentence() {
        assert!(is_envelope("<<<TOOL_ERROR 5ebfdef6>>>"));
        assert!(is_envelope("  <<<END_TOOL_ERROR 5ebfdef6>>>  "));
        assert!(
            !is_envelope("< < <TOOL_ERROR 5ebfdef6>>>"),
            "a neutralised body line"
        );
        assert!(!is_envelope("error: could not find `Cargo.toml`"));

        let doc = "this command's meaning does not exist yet, so nothing can decide \
                   about it. The grammar read 315 bytes and could not resolve:\n  \
                   parameter_expansion at 1:2 decides the assignment";
        let gist = first_sentence(doc);
        assert_eq!(
            gist,
            "this command's meaning does not exist yet, so nothing can decide about it."
        );
        assert!(!gist.contains("parameter_expansion"));
        // A reason that IS a sentence is left exactly alone.
        let one = "the workspace has no writable backend";
        assert_eq!(first_sentence(one), one);
    }

    /// letibot `letibot_ui::progress` tests: the countdown's ladder.
    #[test]
    fn a_countdown_changes_unit_with_the_time_left_and_never_overstates_the_pressure() {
        assert_eq!(countdown(300_000), "expires in 5 min");
        assert_eq!(countdown(181_000), "expires in 4 min");
        assert_eq!(countdown(180_001), "expires in 4 min");
        assert_eq!(countdown(121_000), "expires in 3 min");
        assert_eq!(countdown(120_000), "expires in 2 min");
        assert_eq!(countdown(119_000), "1m59s left");
        assert_eq!(countdown(65_000), "1m05s left");
        assert_eq!(countdown(60_000), "1m00s left");
        assert_eq!(countdown(59_000), "59s left");
        assert_eq!(countdown(47_000), "47s left");
        assert_eq!(countdown(0), "0s left");
    }

    #[test]
    fn durations_and_sizes_read_as_a_person_says_them() {
        assert_eq!(duration(0), "0ms");
        assert_eq!(duration(4_200), "4.2s");
        assert_eq!(duration(187_000), "3m07s");
        assert_eq!(duration(3_840_000), "1h04m");
        assert_eq!(bytes_human(214), "214 B");
        assert_eq!(bytes_human(8192), "8.0 KB");
        assert_eq!(bytes_human(480_000), "468.8 KB");
    }

    #[test]
    fn a_gutter_is_dropped_only_from_a_line_shaped_like_one() {
        assert_eq!(strip_gutter("     1| /target"), "/target");
        assert_eq!(strip_gutter("| not a gutter"), "| not a gutter");
        assert_eq!(strip_gutter("  plain  "), "plain");
    }

    #[test]
    fn foreign_text_keeps_its_newlines_and_loses_everything_else() {
        assert_eq!(clean("a\x1b[31mb\rc\nd"), "ab c\nd");
        assert!(matches!(clean("plain"), Cow::Borrowed(_)));
        assert_eq!(clean_line("a\nb"), "a b");
    }

    #[test]
    fn stepping_in_leaves_a_blank_row_blank() {
        let out = step_in(vec![Line::raw("x"), Line::default()], 2);
        assert_eq!(out[0].plain(), "  x");
        assert_eq!(out[1].plain(), "");
    }
}
