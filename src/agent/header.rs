//! **The header**: the session, the model, the context, the clock.
//!
//! Ported from letibot's `crates/tui/src/ui/header.rs` (`header_line`) and the fitting half of
//! `crates/tui/src/gitfield.rs` (`git_fit`, the per-segment colours). Which model the header
//! names, which usage it reads and whether a turn is running are the host's decisions (letibot's
//! comments on those live with the code that decides them); this draws what it is handed.
//!
//! ```text
//!   the cache question  ~/Projects/letibot (main⇡2!1)   2/4 · glm-5.3-flash · 41.2k ctx · 92% cached · 45 tok/s · 12.3s · 1.2k out
//! ```

use crate::render::{Line, Span, Style};
use crate::style::Role;

use super::text::{clean_line, ellipsise_left, visible_width};

/// **The style a git segment is painted with** — one per segment, chosen to say what the
/// segment SAYS rather than to be pretty (leticl's `+git-styles+`, via letibot's `GitRole`).
///
/// A branch is green when the tree is clean and yellow when it is not — the one fact a person
/// reads at a glance — staged work is green, unstaged is yellow, conflicts are red and bold
/// because nothing else on that row is a demand, and untracked files are dim because they are
/// usually noise.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitMark {
    BranchClean,
    BranchDirty,
    Behind,
    Ahead,
    Stash,
    Action,
    Conflict,
    Staged,
    Unstaged,
    Untracked,
}

impl GitMark {
    /// letibot painted these with raw SGR (`git_paint`); here each is the role whose look is
    /// that colour, so a palette still decides: green = [`Role::Success`], yellow =
    /// [`Role::Pending`] (plain yellow, not `Attention`'s bold), cyan = [`Role::Code`],
    /// magenta = [`Role::Keyword`], red = [`Role::Failure`], dim = [`Role::Faint`]. The two
    /// bold ones (action, conflicts) add the attribute, as letibot spelled a second SGR.
    pub fn style(self) -> Style {
        match self {
            GitMark::BranchClean | GitMark::Staged => Style::of(Role::Success),
            GitMark::BranchDirty | GitMark::Unstaged => Style::of(Role::Pending),
            GitMark::Behind | GitMark::Ahead => Style::of(Role::Code),
            GitMark::Stash => Style::of(Role::Keyword),
            GitMark::Action => Style::of(Role::Keyword).bold(),
            GitMark::Conflict => Style::of(Role::Failure).bold(),
            GitMark::Untracked => Style::of(Role::Faint),
        }
    }
}

/// The columns the row wraps a git field in: the `" ("` before it and the `")"` after it.
const GIT_WRAP: usize = 3;

/// The columns between the left half and the tail: the one gap rule of this row.
const GAP: usize = 2;

/// The tail's facts, by the rank they are dropped in: lowest first.
const DROP_OUT: u8 = 0;
const DROP_POSITION: u8 = 1;
const DROP_CACHED: u8 = 2;
const DROP_MODEL: u8 = 3;
const DROP_DURATION: u8 = 4;
const DROP_RATE: u8 = 5;
const DROP_SPEND: u8 = 6;
const DROP_CTX: u8 = 7;

/// **The longest PREFIX of the pieces that fits in ROOM columns** — leticl's `%git-fit`.
///
/// ROOM is the whole field's budget, wrapping included: a field of pieces `p` is exactly
/// `" (" + p + ")"`, so it is drawn when `3 + width(p) <= room` and not one column later.
///
/// The field degrades by DELETION, like every other thing on this row: the branch is the floor
/// and the marks fall off its right in gitstatus's own order, so a narrow screen loses `?4` and
/// not the branch. A field dropped whole is the behaviour this replaces.
pub fn git_fit(pieces: &[(String, GitMark)], room: usize) -> &[(String, GitMark)] {
    let mut used = GIT_WRAP;
    let mut n = 0;
    for piece in pieces {
        let w = visible_width(&piece.0);
        if used + w > room {
            break;
        }
        used += w;
        n += 1;
    }
    &pieces[..n]
}

/// The prompt's size, and how much of it the cache saved.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Context {
    /// Prompt tokens.
    pub tokens: u64,
    /// Of those, cached.
    pub cached: u64,
    /// Whether `cached` is a measurement. A row that carries the size but not the fraction
    /// shows the size and says nothing about the cache.
    pub cache_measured: bool,
}

/// The header's view model.
///
/// letibot fills it from: `name` ← `session_label(session_id)`; `subagent_of` ←
/// `parent_session().map(session_label)`; `workspace` ← `tilde(wiring.workspace)`; `git` ← the
/// pieces `apply_git_format` rendered (`App::git`); `position` ← (the current root's index + 1,
/// the number of root sessions); `model` ← whichever of the settings row and the turn was
/// heard later; `spent_micros` ← `spent_micros` when `spent_seen`; `context` ← the live
/// prefill when a turn has one, else the last usage; `tok_per_s`, `out_tokens` ← the last
/// round's usage and timings; `duration_ms` ← the running turn's elapsed, else the last
/// round's `wall_ms`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Header {
    pub name: String,
    pub subagent_of: Option<String>,
    /// The workspace as it should read (`~` for `$HOME` already applied). Empty: none.
    pub workspace: String,
    /// The repository's segments, already rendered through the format in force. Empty: no
    /// reading, and nothing is drawn — never a blank that reads like a clean tree.
    pub git: Vec<(String, GitMark)>,
    /// (this conversation, of how many). `at` 0 when the current one is not in the list.
    pub position: (usize, usize),
    pub model: String,
    /// The money meter, in micro-USD; `None` for free and unpriced alike.
    pub spent_micros: Option<u64>,
    pub context: Option<Context>,
    /// The last round's decode rate; `None` when nothing was measured.
    pub tok_per_s: Option<f64>,
    pub duration_ms: Option<u64>,
    pub out_tokens: Option<u64>,
}

/// `1234567` becomes `1.23M`, `12345` becomes `12.3k`.
///
/// Not a thousands separator: a status line has no room for one, and `40.1k` is
/// read faster than `40,132` when the digits past the first three are noise.
pub fn thousands(n: u64) -> String {
    match n {
        0..=9_999 => n.to_string(),
        10_000..=999_999 => format!("{:.1}k", n as f64 / 1000.0),
        _ => format!("{:.2}M", n as f64 / 1_000_000.0),
    }
}

/// A duration as the header has always written it: `412 ms`, `4.2s`, `3m07s`
/// (letibot's `dur_human`, which differs from the cards' `duration` by its space).
pub fn dur_human(ms: u64) -> String {
    if ms < 1000 {
        format!("{ms} ms")
    } else if ms < 60_000 {
        format!("{:.1}s", ms as f64 / 1000.0)
    } else {
        format!("{}m{:02}s", ms / 60_000, (ms % 60_000) / 1000)
    }
}

/// The `model` settings row, as the header shows it: `local (glm-5.3-flash)` is
/// `glm-5.3-flash`, and anything else (a provider pair) is itself.
pub fn header_model(value: &str) -> String {
    match value
        .strip_prefix("local (")
        .and_then(|v| v.strip_suffix(')'))
    {
        Some(alias) => alias.to_string(),
        None => value.to_string(),
    }
}

impl Header {
    /// The right-hand facts in DISPLAY order, each with the rank it is dropped in, lowest first.
    ///
    /// The operator's ruling: the out count goes first, *"hide session counter too"*, *"and %
    /// cached"*, *"hide model too, the most important part is path and git"*. So the order is
    /// `N out`, the position counter, `N% cached`, the model, the duration, `tok/s`, the spend,
    /// and last `N ctx` — every one of them before the path or the git field gives a column.
    fn facts(&self) -> Vec<(String, u8)> {
        let mut right = Vec::new();
        // Shown for one session too: "1/1" is a fact — this daemon holds one session and you
        // are in it — and the one untitled session is otherwise the case with no identity at all.
        right.push((
            format!("{}/{}", self.position.0, self.position.1.max(1)),
            DROP_POSITION,
        ));
        if !self.model.is_empty() {
            right.push((clean_line(&self.model), DROP_MODEL));
        }
        // **The meter.** Beside the token count, because that is where the question "what is
        // this costing me" is already being asked.
        if let Some(m) = self.spent_micros {
            right.push((format!("${:.4}", m as f64 / 1_000_000.0), DROP_SPEND));
        }
        if let Some(c) = self.context.filter(|c| c.tokens > 0) {
            right.push((format!("{} ctx", thousands(c.tokens)), DROP_CTX));
            // A percentage nobody measured is refused.
            if c.cache_measured {
                right.push((
                    format!("{:.0}% cached", c.cached as f64 * 100.0 / c.tokens as f64),
                    DROP_CACHED,
                ));
            }
        }
        // A rate nobody measured is refused: `0 tok/s` would be a number nobody took.
        if let Some(r) = self.tok_per_s {
            right.push((format!("{r:.0} tok/s"), DROP_RATE));
        }
        // **While a turn runs the duration is the turn's, and only the duration** — the host
        // decides which (letibot: two clocks for one turn, `Responding · 151s` beside `4.5s`,
        // read as a clock). This draws what it is given.
        // The host decides whether there is a duration: a running turn's elapsed is shown
        // from its first millisecond (`0 ms`), and an idle head's last wall time only when
        // one was measured.
        if let Some(ms) = self.duration_ms {
            right.push((dur_human(ms), DROP_DURATION));
        }
        if let Some(n) = self.out_tokens.filter(|n| *n > 0) {
            right.push((format!("{} out", thousands(n)), DROP_OUT));
        }
        right
    }

    /// The header row.
    ///
    /// # It degrades by deletion, one field at a time
    ///
    /// The first version dropped the **whole** right half when the two halves did not both
    /// fit, with nothing to say it had. Every field now goes on its own, in an order the
    /// operator set: *"the most important part is path and git"*.
    ///
    /// 1. The tail's facts go by rank, not by position: `N out`, the position counter
    ///    (`1/130`), `N% cached`, the model, the duration, `tok/s`, the spend, and last
    ///    `N ctx`. The survivors keep their display order.
    /// 2. Only once the tail is empty does the path shorten, from its left (the end of a path
    ///    is the part that identifies it), and below its floor of 8 columns it goes.
    /// 3. Then the git marks fall off the right, then the branch.
    /// 4. Past that the row is truncated. It is never wider than `w`; the name is never
    ///    dropped by fitting.
    ///
    /// Measured at 156 columns: a 7-column name, a 25-column path and a 95-column tail left 27
    /// columns of padding, and the 23-column `(fix-main-pgx-timeout)` was not drawn — the room
    /// sum counted the gap twice and the wrap's `)` twice. Every width below is exactly what is
    /// pushed into the line: the left half, one [`GAP`] of two columns, the tail.
    pub fn line(&self, w: usize) -> Line {
        let name = clean_line(&self.name);
        // **One label, and only for a subagent: `subagent of <parent>`.** A screen showing
        // somebody else's conversation looked like a screen showing one's own: *"when I
        // \"Enter\" Subagent it is like completely switching session with just one piece of
        // info - a Label that it is a subagent"*. Same faint register, same field.
        let of = self
            .subagent_of
            .as_ref()
            .map(|parent| format!("  subagent of {}", clean_line(parent)));
        let path = (!self.workspace.is_empty()).then(|| clean_line(&self.workspace));
        // The git field lives beside the workspace: no workspace, no field.
        let git: &[(String, GitMark)] = if path.is_some() { &self.git } else { &[] };
        let git_cols = |pieces: &[(String, GitMark)]| {
            if pieces.is_empty() {
                0
            } else {
                GIT_WRAP + pieces.iter().map(|p| visible_width(&p.0)).sum::<usize>()
            }
        };

        let head_cols = visible_width(&name) + of.as_deref().map_or(0, visible_width);
        let full_left =
            head_cols + path.as_deref().map_or(0, |p| 2 + visible_width(p)) + git_cols(git);
        let tail_cols = |right: &[(String, u8)]| {
            if right.is_empty() {
                0
            } else {
                GAP + right.iter().map(|f| visible_width(&f.0)).sum::<usize>()
                    + 3 * (right.len() - 1)
            }
        };
        // Drop the tail's facts by rank until the whole left half and the tail fit.
        let mut right = self.facts();
        while !right.is_empty() && full_left + tail_cols(&right) > w {
            let lowest = (0..right.len())
                .min_by_key(|&i| right[i].1)
                .expect("non-empty");
            right.remove(lowest);
        }
        let tail_cols = tail_cols(&right);
        let tail = right
            .into_iter()
            .map(|f| f.0)
            .collect::<Vec<_>>()
            .join(" · ");

        // **The header is FACTS, and neither half of it is a sentence** (R51 item 10).
        //
        // The name used to open with `▌` in the user-accent register, and that glyph is how both
        // heads say *a person said this* — so on the row the reader crosses on every return to
        // the field, the session's own name read as somebody's message. The operator: *"the
        // project directory and session name are pinned in the first row with the same blue bar
        // we use for my messages. very confusing. just make both gray and remove the bar."*
        // **Deleted, not recoloured**: a grey bar is still a bar and still makes the claim. And
        // the name loses `Strong` as well — one quiet register for the header.
        let mut line = Line::default();
        line.push(Span::role(name, Role::Faint));
        if let Some(of) = of {
            line.push(Span::role(of, Role::Faint));
        }
        let mut left_cols = head_cols;
        // **The workspace's repository, in gitstatus's own segments and colours**, beside the
        // path it is a fact about. It keeps as much of itself as fits with no path at all, so
        // the path gives way before any git piece does; the marks fall off the right before
        // the branch. The parens are the row's own faint, so the field still reads as one thing.
        let fit = git_fit(git, w.saturating_sub(head_cols + tail_cols));
        if let Some(path) = path {
            // The workspace fills whatever is left, shortened from its *left*.
            let room = w.saturating_sub(head_cols + git_cols(fit) + tail_cols + 2);
            if room >= 8 {
                let shown = ellipsise_left(&path, room);
                left_cols += 2 + visible_width(&shown);
                line.push(Span::role(format!("  {shown}"), Role::Faint));
            }
        }
        if !fit.is_empty() {
            line.push(Span::role(" (", Role::Faint));
            for (text, mark) in fit {
                line.push(Span::styled(clean_line(text), mark.style()));
            }
            line.push(Span::role(")", Role::Faint));
            left_cols += git_cols(fit);
        }
        // At least the one gap, even when the row is about to be truncated.
        let pad = w
            .saturating_sub(left_cols + visible_width(&tail))
            .max(if tail.is_empty() { 0 } else { GAP });
        line.push(Span::raw(" ".repeat(pad)));
        line.push(Span::role(tail, Role::Faint));
        if line.width() > w {
            return crate::render::text::truncate_owned(line, w);
        }
        line
    }
}

impl crate::render::Widget for Header {
    fn render(&self, area: crate::render::Rect, buf: &mut crate::render::Buffer) {
        if !area.is_empty() {
            buf.set_line(
                area.x,
                area.y,
                &self.line(area.width as usize),
                area.width as usize,
            );
        }
    }
}

/// **What the terminal's window title says**: the session's name and the folder, so a tab is
/// told apart by the conversation in it. `titled` says whether `label` is a title rather than a
/// short id: a name leads; a short id does not — before the first message the folder is the
/// more useful word to find a tab by. No session yet: the program's own name.
pub fn window_title(label: Option<&str>, titled: bool, folder: &str) -> String {
    let Some(label) = label else {
        return "letibot".to_string();
    };
    match (titled, folder.is_empty()) {
        (_, true) => label.to_string(),
        (true, false) => format!("{label} · {folder}"),
        (false, false) => format!("{folder} · {label}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::testing::{drawn, role_of};

    fn base() -> Header {
        Header {
            name: "the cache question".into(),
            position: (1, 2),
            ..Header::default()
        }
    }

    /// letibot `app/tests/screen.rs::the_header_carries_the_workspace_branch_and_nothing_when_there_is_none`.
    #[test]
    fn the_header_carries_the_workspace_branch_and_nothing_when_there_is_none() {
        let mut h = base();
        h.workspace = "~/Projects/letibot".into();
        h.git = vec![
            ("main".into(), GitMark::BranchDirty),
            ("⇡2".into(), GitMark::Ahead),
            ("!1".into(), GitMark::Unstaged),
        ];
        let l = h.line(200);
        let s = l.plain();
        assert!(
            s.contains("(main⇡2!1)"),
            "the segments are on the row, gitstatus's own glyphs: {s}"
        );
        assert!(s.contains("letibot"), "{s}");
        assert_eq!(role_of(&l, "main"), Some(Role::Pending));
        assert_eq!(role_of(&l, "⇡2"), Some(Role::Code));

        // **Absent draws nothing at all**: no marks, no placeholder.
        let mut b = h.clone();
        b.git.clear();
        assert!(
            !b.line(200).plain().contains('('),
            "{}",
            b.line(200).plain()
        );
        // And a workspace that is not set draws none of it either.
        let mut c = h.clone();
        c.workspace.clear();
        assert!(
            !c.line(200).plain().contains("main"),
            "{}",
            c.line(200).plain()
        );
    }

    /// letibot `app/tests/screen.rs::a_metered_turn_puts_its_cost_on_the_header_and_the_total_accumulates`
    /// (the drawing half: the host sums; free and unpriced are both no number).
    #[test]
    fn a_metered_session_puts_its_cost_on_the_header() {
        let mut h = base();
        assert!(!h.line(200).plain().contains('$'));
        h.spent_micros = Some(33 + 12_345);
        assert!(
            h.line(200).plain().contains("$0.0124"),
            "{}",
            h.line(200).plain()
        );
    }

    /// letibot `app/tests/turn.rs` — the header degrades by deletion. It used to keep `1/2`
    /// longest and `41.2k ctx` from 80 columns up; by the operator's ruling the path and the
    /// git field now outrank every tail fact, so the 80-column left half keeps its path whole
    /// and the tail goes first, by rank.
    #[test]
    fn a_narrow_header_drops_its_tail_before_its_path() {
        let mut h = base();
        h.workspace = "~/Projects/letibot/.claude/worktrees/agent-a19da2/crates/tui".into();
        h.model = "glm-5.3-flash".into();
        h.context = Some(Context {
            tokens: 41_233,
            cached: 38_100,
            cache_measured: true,
        });
        h.out_tokens = Some(200);
        for w in [40usize, 60, 80, 110, 200] {
            let l = h.line(w);
            let s = l.plain();
            assert!(l.width() <= w, "w={w}: {s}");
            assert!(s.contains("crates/tui"), "w={w}: {s}");
            if w >= 80 {
                assert!(s.contains(&h.workspace), "w={w}: {s}");
            }
        }
        // 18 + 62 + gap: 28 columns of tail, and `glm-5.3-flash · 41.2k ctx` is 25.
        let s = h.line(110).plain();
        assert!(s.ends_with("  glm-5.3-flash · 41.2k ctx"), "{s}");
        assert_eq!(
            h.line(80).plain().trim_end(),
            format!("the cache question  {}", h.workspace)
        );
    }

    /// letibot `app/tests/turn.rs::the_turns_numbers_live_in_the_header_and_an_ordinary_ending_has_no_footer`
    /// (the header half): 1200 tokens over 2000 ms is 600 tok/s.
    #[test]
    fn the_turns_numbers_live_in_the_header() {
        let mut h = base();
        h.context = Some(Context {
            tokens: 41_233,
            cached: 38_100,
            cache_measured: true,
        });
        h.tok_per_s = Some(1_200.0 * 1000.0 / 2_000.0);
        h.duration_ms = Some(12_300);
        h.out_tokens = Some(1_200);
        let s = h.line(200).plain();
        for want in ["600 tok/s", "12.3s", "1200 out", "41.2k ctx", "92% cached"] {
            assert!(s.contains(want), "{want}: {s}");
        }
        // A size without a measured fraction says nothing about the cache.
        h.context.as_mut().unwrap().cache_measured = false;
        assert!(!h.line(200).plain().contains("cached"));
    }

    /// letibot `app/tests/render.rs::the_header_has_no_bar_and_no_emphasis`.
    #[test]
    fn the_header_has_no_bar_and_no_emphasis() {
        let mut h = base();
        h.workspace = "~/x".into();
        let l = h.line(100);
        assert!(!l.plain().contains('▌'), "{}", l.plain());
        for sp in &l.spans {
            let roles: Vec<Role> = sp.style.roles().collect();
            assert!(
                !roles.contains(&Role::Strong) && !roles.contains(&Role::UserAccent),
                "{sp:?}"
            );
        }
        assert_eq!(role_of(&l, "the cache question"), Some(Role::Faint));
        assert_eq!(drawn(&h, 100, 1)[0].trim_end(), l.plain().trim_end());
    }

    #[test]
    fn the_model_row_names_the_alias_and_the_title_leads_with_a_name() {
        assert_eq!(header_model("local (glm-5.3-flash)"), "glm-5.3-flash");
        assert_eq!(
            header_model("deepseek/deepseek-flash"),
            "deepseek/deepseek-flash"
        );
        assert_eq!(window_title(None, false, "x"), "letibot");
        assert_eq!(window_title(Some("one"), true, "letibot"), "one · letibot");
        assert_eq!(
            window_title(Some("s1a2"), false, "letibot"),
            "letibot · s1a2"
        );
    }

    #[test]
    fn git_fit_keeps_the_branch_and_drops_marks_from_the_right() {
        let p = vec![
            ("main".to_string(), GitMark::BranchClean),
            ("?4".to_string(), GitMark::Untracked),
        ];
        assert_eq!(git_fit(&p, 100).len(), 2);
        // " (main?4)" is exactly 9 columns: it fits in 9 and loses `?4` at 8.
        assert_eq!(git_fit(&p, 9).len(), 2);
        assert_eq!(git_fit(&p, 8).len(), 1);
        // " (main)" is exactly 7.
        assert_eq!(git_fit(&p, 7).len(), 1);
        assert_eq!(git_fit(&p, 6).len(), 0);
        assert_eq!(git_fit(&p, 4).len(), 0);
    }

    /// The header measured live at 156 columns, where 27 columns of padding sat beside a
    /// 23-column branch field that was not drawn.
    fn measured() -> Header {
        Header {
            name: "pg-noop".into(),
            workspace: "~/Projects/dbtest/pg-noop".into(),
            git: vec![
                ("fix-main-pgx-timeout".into(), GitMark::BranchDirty),
                ("!3".into(), GitMark::Unstaged),
                ("?2".into(), GitMark::Untracked),
            ],
            position: (1, 130),
            model: "deepseek/deepseek-flash".into(),
            spent_micros: Some(605_100),
            context: Some(Context {
                tokens: 719_300,
                cached: 704_914,
                cache_measured: true,
            }),
            tok_per_s: Some(83.0),
            duration_ms: Some(6_200),
            out_tokens: Some(514),
            ..Header::default()
        }
    }

    fn tail_of(h: &Header) -> String {
        h.facts()
            .into_iter()
            .map(|f| f.0)
            .collect::<Vec<_>>()
            .join(" · ")
    }

    #[test]
    fn the_measured_header_draws_its_branch_at_156_columns() {
        let mut h = measured();
        assert_eq!(visible_width(&tail_of(&h)), 95, "the tail as measured");
        // As measured: the branch, 23 columns wrapped, beside the whole path and the whole tail
        // (7 + 27 + 23 + 2 + 95 = 154).
        h.git.truncate(1);
        let l = h.line(156);
        let s = l.plain();
        assert!(l.width() <= 156, "{s}");
        assert!(
            s.contains("~/Projects/dbtest/pg-noop (fix-main-pgx-timeout)"),
            "{s}"
        );
        assert!(s.ends_with("514 out"), "nothing had to be dropped: {s}");
        // With marks the field is 27 columns and 158 do not fit: the out count goes, not git.
        let l = measured().line(156);
        let s = l.plain();
        assert!(l.width() <= 156, "{s}");
        assert!(
            s.contains("~/Projects/dbtest/pg-noop (fix-main-pgx-timeout!3?2)"),
            "{s}"
        );
        assert!(s.ends_with("6.2s"), "{s}");
    }

    #[test]
    fn the_git_field_fits_in_exactly_its_columns_and_not_one_fewer() {
        // name 1 + "  ~/abcdef" 10 + " (main?4)" 9 = 20; + gap 2 + "1/2" 3 = 25.
        let h = Header {
            name: "n".into(),
            workspace: "~/abcdef".into(),
            git: vec![
                ("main".into(), GitMark::BranchClean),
                ("?4".into(), GitMark::Untracked),
            ],
            position: (1, 2),
            ..Header::default()
        };
        assert_eq!(h.line(25).plain(), "n  ~/abcdef (main?4)  1/2");
        // One fewer: the tail goes, all of it, before the path or git give a column.
        assert_eq!(h.line(24).plain(), "n  ~/abcdef (main?4)    ");
        assert_eq!(h.line(20).plain(), "n  ~/abcdef (main?4)");
        // Then the path, below its floor of 8.
        assert_eq!(h.line(19).plain(), "n (main?4)         ");
        // The whole field in exactly its 9 columns, and `?4` off the right at 8.
        assert_eq!(h.line(10).plain(), "n (main?4)");
        assert_eq!(h.line(9).plain(), "n (main) ");
        assert_eq!(h.line(8).plain(), "n (main)");
        assert_eq!(h.line(7).plain(), "n      ");
    }

    /// The operator's order: out, position, cached, model, duration, tok/s, spend, ctx — and
    /// only then the path, the marks, the branch. The left half, whole, is
    /// 7 + 27 + 27 = 61 columns.
    #[test]
    fn the_header_drops_its_facts_in_the_operators_order() {
        let h = measured();
        let left = "pg-noop  ~/Projects/dbtest/pg-noop (fix-main-pgx-timeout!3?2)";
        assert_eq!(visible_width(left), 61);
        let steps = [
            "1/130 · deepseek/deepseek-flash · $0.6051 · 719.3k ctx · 98% cached · 83 tok/s · 6.2s · 514 out",
            "1/130 · deepseek/deepseek-flash · $0.6051 · 719.3k ctx · 98% cached · 83 tok/s · 6.2s",
            "deepseek/deepseek-flash · $0.6051 · 719.3k ctx · 98% cached · 83 tok/s · 6.2s",
            "deepseek/deepseek-flash · $0.6051 · 719.3k ctx · 83 tok/s · 6.2s",
            "$0.6051 · 719.3k ctx · 83 tok/s · 6.2s",
            "$0.6051 · 719.3k ctx · 83 tok/s",
            "$0.6051 · 719.3k ctx",
            "719.3k ctx",
            "",
        ];
        let need = |tail: &str| {
            61 + if tail.is_empty() {
                0
            } else {
                GAP + visible_width(tail)
            }
        };
        for (k, tail) in steps.iter().enumerate() {
            let w = need(tail);
            assert_eq!(
                h.line(w).plain(),
                format!("{left}  {tail}").trim_end(),
                "w={w}"
            );
            if let Some(next) = steps.get(k + 1) {
                let pad = w - 1 - 61 - visible_width(next);
                assert_eq!(
                    h.line(w - 1).plain(),
                    format!("{left}{}{next}", " ".repeat(pad)),
                    "w={}",
                    w - 1
                );
            }
        }
        // The tail is empty and the path and git are whole at 61; at 60 the path shortens.
        let s = h.line(60).plain();
        assert!(
            s.starts_with("pg-noop  …/dbtest/pg-noop (fix-main-pgx-timeout!3?2)"),
            "{s}"
        );
        // 7 + 2 + 8 + 27 = 44 is the path's floor; at 43 it is gone and git is whole.
        assert!(
            h.line(44).plain().starts_with("pg-noop  "),
            "{}",
            h.line(44).plain()
        );
        assert!(
            h.line(43)
                .plain()
                .starts_with("pg-noop (fix-main-pgx-timeout!3?2)"),
            "{}",
            h.line(43).plain()
        );
        // Then the marks from the right, then the branch.
        assert_eq!(h.line(34).plain(), "pg-noop (fix-main-pgx-timeout!3?2)");
        assert_eq!(h.line(33).plain(), "pg-noop (fix-main-pgx-timeout!3) ");
        assert_eq!(h.line(30).plain(), "pg-noop (fix-main-pgx-timeout)");
        // With no git field left to keep, the path takes back the columns it can use.
        assert_eq!(h.line(29).plain(), "pg-noop  …/dbtest/pg-noop    ");
    }

    #[test]
    fn the_header_never_exceeds_its_width() {
        let h = measured();
        let mut sub = h.clone();
        sub.subagent_of = Some("the parent".into());
        for h in [h, sub] {
            for w in 0..=220 {
                let l = h.line(w);
                assert!(l.width() <= w, "w={w}: {}", l.plain());
            }
        }
    }
}
