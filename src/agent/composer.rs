//! **The composer's frame**: the box the person types in — its edges and the facts inlaid in
//! them, the walls around the text, the caret's place — and the completion row over it.
//!
//! Ported from letibot's `crates/tui/src/ui/composer.rs` (`box_edge`, `composer_rows`,
//! `completions_line`, `shell_completions_line`) and the two edges in `ui/screen.rs`
//! (`box_top`, `box_bottom`). **The text editing is not here**: the host's editor lays its text
//! out into lines and a caret, and hands both over; this puts the field around them.
//!
//! ```text
//! ╭──────────────────────────────── 1 subagent running · 2 jobs running ─╮
//! │ › why did the cache miss                                              │
//! ╰──────────────────────────────────────────────────────── ⚠ · holding ─╯
//! ```

use crate::render::{Line, Span, Style};
use crate::style::Role;

use super::text::{trim_to, visible_width};

/// One edge of the box, with a legend inlaid at the left and one pinned to the right.
///
/// `╰────────── ⚠ · holding ─╯`. A legend rather than a decoration **when there is something to
/// say**: an edge with nothing to say renders plain, because a row of attention paid for ever
/// for a fact read once is the mistake the composer's top border already made once. The right
/// legend yields room to the left one, yields itself by truncation next, and is dropped before
/// the border is allowed to wrap.
///
/// The legends keep their own roles (the alarm in attention, a count in pending) and the border
/// is [`Role::Faint`] around them — in letibot a painted legend's reset restored the terminal
/// default rather than the border's grey, so the border had to reopen itself after each one; a
/// span cannot leak, so that defect has no place to happen here.
pub fn box_edge(w: usize, open: char, close: char, left: &Line, right: &Line) -> Line {
    let w = w.max(4);
    let inner = w - 2;
    // **The edge is one faint register, and the legends are inlaid in it**: the frame's
    // own glyphs are plain text under the line's faint style, and a legend's spans are its
    // own looks over it. That is letibot's edge exactly — `p.painted(Faint, …)` around the
    // whole edge, a legend that closes and re-opens the faint — and it is the cells a
    // buffer draws: a legend in the pending register reads dim yellow, as the string did.
    let mut out = Line {
        spans: vec![Span::raw(open.to_string())],
        style: Style::of(Role::Faint),
    };
    let mut left_cols = 0;
    if left.width() > 0 && inner >= 10 {
        out.push(Span::raw("─ "));
        let l = crate::render::text::truncate(left, inner - 4);
        left_cols = 3 + l.width();
        out.spans.extend(spans_of(&l));
        out.push(Span::raw(" "));
    }
    let mut right_spans = Vec::new();
    let mut right_cols = 0;
    if right.width() > 0 && inner >= 10 {
        // letibot measured `+ 2` here and spent three (` `, ` ─`), so a full legend ran one
        // column past the edge; the frame's trim hid it. Measured exactly here.
        let room = inner.saturating_sub(left_cols + 3);
        if room >= 4 {
            let r = crate::render::text::truncate(right, room);
            right_cols = r.width() + 3;
            right_spans.push(Span::raw(" "));
            right_spans.extend(spans_of(&r));
            right_spans.push(Span::raw(" ─"));
        }
    }
    let fill = inner.saturating_sub(left_cols + right_cols);
    out.push(Span::raw("─".repeat(fill)));
    out.spans.extend(right_spans);
    out.push(Span::raw(close.to_string()));
    out
}

/// A line's spans with the line's own style patched in, so they can join another line.
fn spans_of(l: &Line) -> Vec<Span> {
    l.spans
        .iter()
        .map(|s| Span {
            content: s.content.clone(),
            style: l.style.patch(&s.style),
        })
        .collect()
}

/// The jobs fact on the top edge: `2 jobs running · 1 to a file`, or nothing while none runs.
/// `running` counts running jobs and `to_a_file` the running ones redirected to a file — the
/// unwatchable one is named before the reader opens the pane.
pub fn jobs_fact(running: usize, to_a_file: usize) -> Option<String> {
    if running == 0 {
        return None;
    }
    let mut out = format!(
        "{running} job{} running",
        if running == 1 { "" } else { "s" }
    );
    if to_a_file > 0 {
        out.push_str(&format!(" · {to_a_file} to a file"));
    }
    Some(out)
}

/// **The composer box's top edge**, carrying what this session has running: its live subagents
/// and its background jobs.
///
/// letibot fills it from: `subagents_running` ← subagents that are not `is_finished()` (the one
/// lifecycle predicate the subagents pane's active group is built from, so the edge and the pane
/// cannot disagree — *"an agent is alive from spawn until it has finished"*); `jobs_running` /
/// `jobs_to_a_file` ← the job table's running rows and those with a `redirect`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BoxTop {
    pub subagents_running: usize,
    pub jobs_running: usize,
    pub jobs_to_a_file: usize,
}

impl BoxTop {
    pub fn line(&self, w: usize) -> Line {
        let mut facts: Vec<String> = Vec::new();
        let n = self.subagents_running;
        if n > 0 {
            facts.push(format!(
                "{n} subagent{} running",
                if n == 1 { "" } else { "s" }
            ));
        }
        if let Some(jobs) = jobs_fact(self.jobs_running, self.jobs_to_a_file) {
            facts.push(jobs);
        }
        let right = if facts.is_empty() {
            Line::default()
        } else {
            Line::styled(facts.join(" · "), Role::Pending)
        };
        box_edge(w, '╭', '╮', &Line::default(), &right)
    }
}

/// **The composer box's bottom edge**, carrying the alarm, where the reader is in the
/// conversation, and the visibility rung.
///
/// letibot fills it from: `alarmed` ← `alarmed()`; `holding` ← `!following()`; `rung` ←
/// `rung_state()`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BoxBottom {
    pub alarmed: bool,
    /// **The viewport's state, where the reader's eye already crosses** (R36). Drawn **only when
    /// it is holding**, because following is the ordinary state and owes the reader nothing — a
    /// marker that is always on is furniture.
    pub holding: bool,
    /// **The rung, when it is the one that hides things** (R37): drawn only when it is news.
    /// What it buys is the reader who switched and then forgot.
    pub rung: Option<String>,
}

impl BoxBottom {
    pub fn line(&self, w: usize) -> Line {
        let mut right = Line::default();
        // The separator is the edge's own text: it sits in the edge's faint register (a
        // string host writes it bare between the pieces, as letibot's edge did).
        let sep = |l: &mut Line| {
            if l.width() > 0 {
                l.push(Span::raw(" · "));
            }
        };
        if self.alarmed {
            right.push(Span::role("⚠", Role::Attention));
        }
        if self.holding {
            sep(&mut right);
            right.push(Span::role("holding", Role::Pending));
        }
        if let Some(r) = &self.rung {
            sep(&mut right);
            right.push(Span::role(super::text::clean_line(r), Role::Attention));
        }
        box_edge(w, '╰', '╯', &Line::default(), &right)
    }
}

/// A masked field's one line: a dot per character, and the caret after the last one. The text
/// itself is never handed over, not even to compute a width — the host passes the count. A
/// secret and a provider key are both drawn this way; a prompt card's line for a program's
/// stdin is drawn **in the open**, through the ordinary editor, and that difference is the
/// whole of what keeps the two channels apart.
pub fn masked(chars: usize) -> (Vec<Line>, (usize, usize)) {
    (vec![Line::raw("•".repeat(chars))], (0, chars))
}

/// **The field**: the editor's laid-out lines, inside the walls, scrolled to the caret.
///
/// The host's editor lays out the text at [`ComposerField::inner`] columns and passes the lines
/// and the caret's `(row, col)` within them. This puts a wall on each side and pads to the full
/// width, so the row is a *field* and not a line of text that happens to be at the bottom.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ComposerField {
    pub lines: Vec<Line>,
    pub caret: (usize, usize),
}

impl ComposerField {
    /// The columns a boxed field gives its text at width `w`: two walls and a space each side.
    pub fn inner(w: usize) -> usize {
        w.saturating_sub(4)
    }

    /// The rows to draw (at most `max_rows`, at least one) and the caret's `(row, col)` within
    /// them — **the host places the terminal's caret from this**, offset by the row the field
    /// is drawn at (computed once, after every row above the box is laid out: leticl added a
    /// status row above the box and its caret stayed one row up — *"cursor now goes above the
    /// text i type lol"*).
    ///
    /// Scrolled to the row being edited, never to the top: a composer taller than the rows it
    /// was given must still show the caret, or the person is typing somewhere they cannot see.
    pub fn rows(&self, w: usize, max_rows: usize, boxed: bool) -> (Vec<Line>, usize, usize) {
        let inner = Self::inner(w);
        let (crow, ccol) = self.caret;
        let n = self.lines.len().max(1);
        let show = max_rows.clamp(1, n);
        let start = crow.saturating_sub(show - 1).min(n - show);
        // `Role::Faint`, not a grey: bright black lands within a hair of the background on
        // several light themes; the attribute de-emphasises whatever the reader has chosen.
        let wall = Span::role("│", Role::Faint);
        let mut out = Vec::with_capacity(show);
        for i in start..start + show {
            let body = self.lines.get(i).cloned().unwrap_or_default();
            out.push(if boxed {
                let mut l = Line::new(vec![wall.clone(), Span::raw(" ")]);
                l.spans
                    .extend(spans_of(&crate::render::text::fit(&body, inner + 1)));
                l.push(wall.clone());
                l
            } else {
                crate::render::text::truncate_owned(body, w)
            });
        }
        let col = if boxed { ccol + 2 } else { ccol };
        (out, crow.saturating_sub(start), col)
    }
}

/// One candidate on the completion row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub text: String,
    /// **A proposal, not a fact** — the model's suggestion rather than a command this session
    /// ran. Drawn with a leading `~`, because a line that looks like the operator typed it and
    /// did not is the same class of lie as an unattributed quote. The mark is display-only: a
    /// Tab fills the composer with the line itself, never the marked form.
    pub proposed: bool,
}

/// The columns the row puts between two candidates — `  ·  `.
pub const SEPARATOR_COLS: usize = 5;

/// **The live completion row** shown above the composer while a `/command` or a `!` line is
/// being typed. letibot fills it with the `/` command matches (`/name hint`), or for a `!`
/// line: the file names a Tab found, else the history's candidates (plain, a fact) then the
/// model's for this prefix and position (`proposed`). Empty: no row content — the row itself is
/// a slot the fit ladder reserves, so its height does not follow its content.
///
/// **Only as many candidates as fit are joined.** Measured at 10 ms a frame on a history of two
/// thousand matching commands when every one was cloned and joined and then trimmed away.
pub fn completions_line(items: &[Candidate], w: usize) -> Option<Line> {
    let mut parts: Vec<String> = Vec::new();
    let mut used = 2usize; // the row's own leading indent
    for c in items {
        if used >= w {
            break;
        }
        let shown = if c.proposed {
            format!("~{}", c.text)
        } else {
            c.text.clone()
        };
        used += visible_width(&shown) + SEPARATOR_COLS;
        parts.push(super::text::clean_line(&shown));
    }
    if parts.is_empty() {
        return None;
    }
    Some(Line::styled(
        trim_to(&format!("  {}", parts.join("  ·  ")), w),
        Style::of(Role::Faint),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::testing::{plain, role_of};

    #[test]
    fn an_edge_with_nothing_to_say_is_plain_and_exactly_the_width() {
        for w in [4usize, 10, 40, 80] {
            let l = BoxTop::default().line(w);
            assert_eq!(l.width(), w);
            assert!(l.plain().starts_with('╭') && l.plain().ends_with('╮'));
            assert!(!l.plain().contains(' '));
        }
    }

    /// letibot `app/tests/subagents.rs::a_running_subagent_is_counted_on_the_top_border_and_a_done_one_is_not`
    /// and `app/tests/jobs.rs` (`jobs_line` and its place on the top edge).
    #[test]
    fn running_work_is_counted_on_the_top_edge() {
        let top = BoxTop {
            subagents_running: 1,
            jobs_running: 2,
            jobs_to_a_file: 1,
        };
        let l = top.line(100);
        let s = l.plain();
        assert!(s.starts_with('╭') && s.ends_with("─╮"), "{s}");
        assert!(
            s.contains(" 1 subagent running · 2 jobs running · 1 to a file ─╮"),
            "{s}"
        );
        assert_eq!(l.width(), 100);
        assert_eq!(role_of(&l, "subagent"), Some(Role::Pending));
        assert_eq!(jobs_fact(1, 0).as_deref(), Some("1 job running"));
        assert_eq!(jobs_fact(0, 0), None);
        assert!(!BoxTop::default().line(100).plain().contains("running"));
    }

    /// letibot's bytes: one faint run, the legend closing and re-opening it.
    #[test]
    fn an_edge_is_one_faint_register_with_its_legend_inlaid() {
        let l = BoxTop {
            subagents_running: 1,
            ..BoxTop::default()
        }
        .line(30);
        assert_eq!(
            l.to_ansi_inside(crate::style::Palette::Colour),
            "╭─────── \x1b[33m1 subagent running\x1b[0m\x1b[2m ─╮"
        );
        assert_eq!(l.style, Style::of(Role::Faint));
    }

    #[test]
    fn the_bottom_edge_says_only_what_is_news() {
        let b = BoxBottom {
            alarmed: true,
            holding: true,
            rung: Some("conversation".into()),
        };
        let l = b.line(60);
        assert!(
            l.plain().ends_with(" ⚠ · holding · conversation ─╯"),
            "{}",
            l.plain()
        );
        assert_eq!(role_of(&l, "⚠"), Some(Role::Attention));
        assert_eq!(role_of(&l, "holding"), Some(Role::Pending));
        assert_eq!(role_of(&l, "─"), Some(Role::Faint));
        assert!(!BoxBottom::default().line(60).plain().contains("holding"));
        // A legend too long for the edge is cut, never wrapped.
        for w in [8usize, 14, 20] {
            assert_eq!(b.line(w).width(), w.max(4), "w={w}");
        }
    }

    /// letibot `app/tests/screen.rs::the_composer_is_a_field_with_a_caret_in_it_and_no_prose`
    /// (the field half): walls, no prose, and the caret two columns in past the text.
    #[test]
    fn the_composer_is_a_field_with_a_caret_in_it_and_no_prose() {
        let empty = ComposerField {
            lines: vec![Line::raw("› ")],
            caret: (0, 2),
        };
        let (rows, r, c) = empty.rows(80, 3, true);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].width(), 80);
        assert!(rows[0].plain().starts_with("│ › ") && rows[0].plain().ends_with('│'));
        assert_eq!((r, c), (0, 4));
        let typed = ComposerField {
            lines: vec![Line::raw("› why did the cache miss")],
            caret: (0, 2 + "why did the cache miss".len()),
        };
        let (_, _, c) = typed.rows(80, 3, true);
        assert_eq!(c, 4 + "why did the cache miss".len());
    }

    /// A composer taller than its rows scrolls to the caret, not to the top.
    #[test]
    fn a_tall_composer_shows_the_row_being_edited() {
        let f = ComposerField {
            lines: (0..10).map(|i| Line::raw(format!("line {i}"))).collect(),
            caret: (8, 3),
        };
        let (rows, r, _) = f.rows(40, 3, true);
        assert_eq!(rows.len(), 3);
        assert!(rows[r].plain().contains("line 8"), "{:?}", plain(&rows));
        let (rows, _, c) = f.rows(40, 3, false);
        assert_eq!(c, 3, "an unboxed field has no wall to step over");
        assert_eq!(rows[0].plain(), "line 6");
    }

    #[test]
    fn a_secret_is_dots_and_the_caret_follows_them() {
        let (lines, caret) = masked(4);
        assert_eq!(lines[0].plain(), "••••");
        assert_eq!(caret, (0, 4));
    }

    /// letibot `app/tests/composer.rs` — the model's candidates are marked `~`, the history's
    /// are not, and the row stops collecting at its width.
    #[test]
    fn a_proposed_candidate_wears_its_provenance() {
        let items = vec![
            Candidate {
                text: "! cargo test".into(),
                proposed: false,
            },
            Candidate {
                text: "! cargo build".into(),
                proposed: true,
            },
        ];
        let l = completions_line(&items, 100).unwrap();
        assert_eq!(l.plain(), "  ! cargo test  ·  ~! cargo build");
        assert_eq!(role_of(&l, "cargo"), Some(Role::Faint));
        assert!(completions_line(&[], 100).is_none());
        let many: Vec<Candidate> = (0..2000)
            .map(|i| Candidate {
                text: format!("! cargo {i}"),
                proposed: false,
            })
            .collect();
        assert!(completions_line(&many, 40).unwrap().width() <= 40);
    }
}
