//! **The pickers**: the sessions a daemon holds, and a setting's values.
//!
//! Ported from letibot's `ui/panes/picker.rs` (`picker_lines`, `setting_picker_lines`). The
//! host does the enumeration — which sessions are listed, in which order, at which depth,
//! and which families are unfolded is session logic — and hands the rows over in drawing
//! order; this draws them.

use crate::agent::pane::{self, PaneLines};
use crate::agent::text::clean_line;
use crate::agent::tree_list::{Action, Cell, Fold, Row, TreeList};
use crate::render::{Line, Span};
use crate::style::Role;
use crate::width::text as wt;

/// One row of the session picker, in the order it is drawn.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionRow {
    /// How deep in the session tree: 0 for a conversation, 1 for its subagent, …
    pub depth: usize,
    /// The title, or a short id when it has none.
    pub name: String,
    /// The full id, drawn under every row.
    pub session_id: String,
    /// This head is in this session.
    pub here: bool,
    /// `None` when it has no children; `Some(open)` when it does.
    pub children: Option<bool>,
    /// A turn is generating in it now.
    pub generating: bool,
    /// Held live by the daemon (`false`: on disk, a resume away).
    pub live: bool,
    /// Its length in rows (the store's count when there is one); 0 is not said.
    pub rows: usize,
    /// Heads attached to it.
    pub heads: usize,
    /// The model it runs on, or empty.
    pub model: String,
    /// Its workspace as the host wants it shown (letibot writes `$HOME` as `~`), or empty.
    pub workspace: String,
}

/// **A row that is not a session**: a group header — how many rows it holds, how it folds, and
/// the sentence under it that says what Enter does.
///
/// It exists because rano had no such row, so letibot faked one as a [`SessionRow`] with an
/// empty `session_id` — and the picker's footer then said *enter switches* about a row where
/// Enter folds. A group is a stop (Enter acts on it) and it carries no number: a group line
/// is not a number anyone can type.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionGroup {
    /// What the group holds, in a word: `finished`, `on disk`, `mine`.
    pub name: String,
    /// How many rows it holds, for the `(N)` after its name.
    pub count: usize,
    /// **How it folds** — [`Fold::Fixed`] for a group that is always open (letibot's `mine`):
    /// no glyph, no fold key, and a hint that never says *folds*. Two states forced that group
    /// to say `open: true` and rano then drew a `-` and wrote *enter folds it away* on a row
    /// where Enter does nothing.
    pub fold: Fold,
    /// The faint line under the row, saying what Enter will do to it — or, for a fixed group,
    /// what the group is. Empty draws none.
    pub note: String,
}

/// One row of the session picker, in drawing order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionItem {
    Session(SessionRow),
    Group(SessionGroup),
}

impl From<SessionRow> for SessionItem {
    fn from(s: SessionRow) -> SessionItem {
        SessionItem::Session(s)
    }
}

impl From<SessionGroup> for SessionItem {
    fn from(g: SessionGroup) -> SessionItem {
        SessionItem::Group(g)
    }
}

/// The session picker's view model.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionPicker {
    pub rows: Vec<SessionItem>,
    /// The cursor, an index into `rows`; clamped when drawn.
    pub selected: usize,
}

impl SessionPicker {
    /// **The picker, as data**: every row is a [`Row`] and the list draws it.
    ///
    /// The measurements the hand-written renderer used to carry are now the primitive's own
    /// rules (`crate::agent::tree_list`): the fold column is reserved at every depth — dropping
    /// it for a leaf put two siblings at ONE depth one column apart, so the name column was a
    /// function of *has children* rather than of *depth* — the number field is as wide as the
    /// list is long (`{:>2}` is right for nine rows and wrong for a box whose header reads
    /// `1/106`: MEASURED, rows 3–99 at column 6, rows 100–104 at column 7), and the hint under
    /// the list is the selected row's own words.
    ///
    /// The depth is capped at three indents: a session tree is a conversation and its
    /// subagents, and a row four levels in would be a name pushed off the right edge to say
    /// where it is.
    fn tree(&self) -> TreeList {
        let mut sessions = 0usize;
        let rows = self
            .rows
            .iter()
            .map(|item| match item {
                SessionItem::Session(s) => {
                    sessions += 1;
                    session_row(s, sessions)
                }
                SessionItem::Group(g) => group_row(g),
            })
            .collect();
        TreeList {
            title: "sessions in this daemon".into(),
            rows,
            selected: Some(self.selected),
            empty: "none listed yet — the daemon has not answered, or this head is replaying \
                    a recorded log and has no daemon to ask."
                .into(),
            fold_column: true,
            // The full id sits at column 6, under the mark and the fold column.
            detail_column: 6,
            margin: 2,
            hint: vec![
                "↑↓ moves".into(),
                "or type a number or part of a name and press enter".into(),
                "/new [title] makes one".into(),
                "/rename NAME names this one".into(),
                "esc closes".into(),
            ],
            notes: vec![
                "switching does not stop anything: a turn keeps running in the session you \
                 left, and it is still there when you come back."
                    .into(),
            ],
        }
    }

    /// Every row, each session a stop. letibot truncates this to the room rather than
    /// windowing it; a host may do either with the result.
    pub fn content(&self, w: usize) -> PaneLines {
        self.tree().content(w)
    }

    /// **The hint row [`SessionPicker::content`] draws** — the selected row's own keys and then
    /// the picker's, margin and all. A host that puts the picker's hint somewhere else (its
    /// [`HintBar`](crate::agent::hint_bar::HintBar), or a pane of its own that opens the
    /// picker) reads it here rather than spelling the same cursor-dependent sentence a second
    /// time, which is how the two drift.
    pub fn hint_line(&self) -> Option<String> {
        self.tree().hint_line()
    }

    /// The same sentence **without the pane's left margin**, which is what
    /// [`HintBar::line_with_tail`](crate::agent::hint_bar::HintBar::line_with_tail) takes: a
    /// bar centres its own text, and a margin carried into it reads as a layout bug.
    pub fn hint_text(&self) -> Option<String> {
        self.tree().hint_text()
    }
}

/// One session, as a row: the number you type, its fold, its name, its facts, and its full id
/// underneath.
fn session_row(s: &SessionRow, number: usize) -> Row {
    // Busy is the fact a picker exists to show: switching away from a running turn is fine —
    // the daemon keeps generating — and switching *into* one is how you go back and watch it.
    let mut facts: Vec<String> = Vec::new();
    if s.generating {
        facts.push("generating".into());
    }
    if !s.live {
        // The whole difference between a row that costs one keystroke and a row that costs a
        // resume. Said in a word rather than implied by an absent "generating".
        facts.push("on disk".into());
    }
    if s.rows > 0 {
        facts.push(format!("{} rows", s.rows));
    }
    if s.heads > 0 {
        facts.push(format!(
            "{} head{}",
            s.heads,
            if s.heads == 1 { "" } else { "s" }
        ));
    }
    if !s.model.is_empty() {
        facts.push(clean_line(&s.model));
    }
    // The full id under **every** row, not only the named ones: the sessions whose id you
    // might actually need to type — the unnamed ones — were the ones showing a truncation. The
    // workspace goes on the same line: for a stored session it is the only thing on the row
    // that says what the conversation was *about*.
    let under = if s.workspace.is_empty() {
        clean_line(&s.session_id)
    } else {
        format!(
            "{}  {}",
            clean_line(&s.session_id),
            clean_line(&s.workspace)
        )
    };
    Row {
        // Every session is a stop, even one the host has no id for: the row is still a place
        // Enter takes you, and today it always was.
        id: Some(s.session_id.clone()),
        // The name's column is a function of its depth and nothing else — the fold column is
        // reserved for a leaf too, and the picture in this file's history is why.
        depth: s.depth.min(3),
        number: Some(number),
        // A session with children folds, and a session whose children are held back says so.
        open: s
            .children
            .map(|open| if open { Fold::Open } else { Fold::Closed }),
        cells: vec![Cell::role(
            clean_line(&s.name),
            if s.here { Role::Strong } else { Role::Plain },
        )],
        facts: vec![Cell::role(
            facts.join(" · "),
            if s.generating {
                Role::Pending
            } else {
                Role::Faint
            },
        )],
        detail: vec![under],
        actions: vec![Action::new("enter", "switches")],
        ..Row::default()
    }
}

/// One group header, as a row: what it holds and how many, its fold, and what Enter does to it.
fn group_row(g: &SessionGroup) -> Row {
    // **A group's fold is the group's own data.** `Fixed` is a group that is always open: it
    // carries no fold action, so the hint under it cannot name one — the sentence is the
    // picker's fixed half alone, which is what a row with no keys is entitled to say.
    let (open, actions) = match g.fold {
        Fold::Open => (
            Some(Fold::Open),
            vec![Action::new("enter", "folds it away")],
        ),
        Fold::Closed => (Some(Fold::Closed), vec![Action::new("enter", "unfolds it")]),
        Fold::Fixed => (Some(Fold::Fixed), Vec::new()),
    };
    Row {
        id: Some(g.name.clone()),
        open,
        cells: vec![Cell::role(
            format!("{} ({})", clean_line(&g.name), g.count),
            Role::Strong,
        )],
        detail: if g.note.is_empty() {
            Vec::new()
        } else {
            vec![g.note.clone()]
        },
        actions,
        ..Row::default()
    }
}

/// One value a setting can take.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SettingValue {
    pub name: String,
    /// What it means, one fact per line — or empty, for a daemon's setting whose meaning
    /// the host does not know (a gloss it invented would be documentation written wrongly).
    pub why: String,
    /// This box can take it as it is (the model card: a key is held for it). Drawn green.
    pub ready: bool,
}

/// **The setting card** — one renderer for every setting a reader chooses from (R38):
/// letibot's `/mode`, `/models`, `/verbosity` and `/diff` are four questions of one kind,
/// so they get one card.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SettingPicker {
    pub title: String,
    pub values: Vec<SettingValue>,
    /// The value in force now, marked `← now`.
    pub current: String,
    /// The cursor, an index into `values`; clamped when drawn.
    pub selected: usize,
    /// What to say when there are no values (the daemon has not named them).
    pub empty: String,
    /// What the green means, in words, when any row can be green.
    pub legend: Option<String>,
    /// What taking a value does to what is already drawn, one line each.
    pub consequence: Vec<String>,
}

impl SettingPicker {
    /// The card. The title is one row and the first value is the next one; each value is a
    /// stop, so a click is read off the record rather than off that arithmetic.
    ///
    /// * **Every value, the current one marked.** `← now` in the faint register and the value
    ///   itself bold — the split the session picker draws between its bold row and its
    ///   inverse one: *where am I* and *what does Enter take* stay two readable facts.
    /// * **What each value MEANS**, wrapped and indented under it. One fact per line: these
    ///   are not trimmed, so a sentence carrying two facts would lose the second — measured
    ///   at 110 columns, where a two-fact version read "It also become…".
    /// * **`esc` is a real answer** — it closes the card and leaves the setting alone.
    pub fn content(&self, w: usize) -> PaneLines {
        let mut out = PaneLines::new();
        out.push(pane::title(self.title.clone()));
        if self.values.is_empty() && !self.empty.is_empty() {
            out.push(pane::faint(format!("  {}", self.empty)));
        }
        let cursor = self.selected.min(self.values.len().saturating_sub(1));
        for (i, v) in self.values.iter().enumerate() {
            let here = v.name == self.current;
            let picked = i == cursor;
            // **Greened when this box can actually take the row** — the operator's ask.
            let role = if v.ready {
                Role::Success
            } else if here {
                Role::Strong
            } else {
                Role::Plain
            };
            let left = Line::new(vec![
                Span::raw(format!("{} {:>2}  ", pane::mark(picked), i + 1)),
                Span::role(clean_line(&v.name), role),
            ]);
            let right = if here {
                pane::faint("← now")
            } else {
                Line::default()
            };
            out.push_stop(crate::render::text::truncate(
                &pane::split_row(pane::picked(left, picked), right, w),
                w,
            ));
            if !v.why.is_empty() {
                for l in wt::wrap(&v.why, w.saturating_sub(9)) {
                    out.push(pane::faint(format!("        {l}")));
                }
            }
        }
        out.push(pane::faint(
            "  ↑↓ moves · enter takes · or type a name or the row number · esc leaves it alone",
        ));
        // **What the colour means, in words** — because `Palette::None` is not a monochrome
        // theme but the `--replay`, pipe and CI case, and there a colour says nothing at all.
        if let Some(l) = &self.legend {
            out.push(pane::faint(format!("  {l}")));
        }
        for line in &self.consequence {
            out.push(pane::faint(format!("  {line}")));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::testing::{plain, role_of};

    fn row(depth: usize, name: &str, children: Option<bool>) -> SessionRow {
        SessionRow {
            depth,
            name: name.into(),
            session_id: format!("s-{}", name.replace(' ', "-")),
            children,
            live: true,
            ..SessionRow::default()
        }
    }

    /// The column rule letibot's `show_the_sessions_pane_tree` instrument measured: a name's
    /// column is a function of its DEPTH alone, whatever its fold and however many rows the
    /// list has.
    #[test]
    fn a_names_column_is_its_depth_and_not_its_children() {
        let mut rows = vec![
            row(0, "this conversation", Some(true)),
            row(1, "the first child", Some(false)),
            row(2, "the grandchild", None),
            row(1, "the second child", None),
        ];
        for i in 0..100 {
            rows.push(row(0, &format!("filler {i}"), None));
        }
        let p = SessionPicker {
            rows: rows.into_iter().map(SessionItem::Session).collect(),
            selected: 0,
        };
        let drawn = plain(&p.content(96).lines);
        let col = |name: &str| {
            let l = drawn.iter().find(|l| l.contains(name)).unwrap();
            wt::width(&l[..l.find(name).unwrap()])
        };
        assert_eq!(
            col("the first child"),
            col("the second child"),
            "{drawn:#?}"
        );
        assert_eq!(col("the grandchild"), col("the first child") + 2);
        assert_eq!(
            col("this conversation"),
            col("filler 99"),
            "row 100+ in step"
        );
        let first = drawn
            .iter()
            .find(|l| l.contains("this conversation"))
            .unwrap();
        assert!(first.starts_with("▸-   1  "), "{first:?}");
    }

    /// The facts a picker exists to show: busy, on disk, length, heads, model — and the full
    /// id under every row (letibot `session.rs` assertions on `generating`).
    #[test]
    fn a_session_row_says_whether_it_is_busy_and_where_it_lives() {
        let p = SessionPicker {
            rows: vec![
                SessionRow {
                    generating: true,
                    rows: 120,
                    heads: 1,
                    model: "local".into(),
                    workspace: "~/Projects/rano".into(),
                    here: true,
                    ..row(0, "work", None)
                }
                .into(),
                SessionRow {
                    live: false,
                    ..row(0, "old", None)
                }
                .into(),
            ],
            selected: 1,
        };
        let c = p.content(100);
        let t = plain(&c.lines);
        assert!(
            t[2].ends_with("generating · 120 rows · 1 head · local"),
            "{t:?}"
        );
        assert_eq!(role_of(&c.lines[2], "generating"), Some(Role::Pending));
        assert_eq!(
            role_of(&c.lines[2], "work"),
            Some(Role::Strong),
            "where am I"
        );
        assert_eq!(t[3], "      s-work  ~/Projects/rano");
        assert!(t[4].ends_with("on disk"), "{t:?}");
        assert!(t[4].starts_with("▸"), "{t:?}");
        assert_eq!(c.stop_rows, vec![2, 4]);
        assert!(
            t.iter()
                .any(|l| l.contains("switching does not stop anything"))
        );
    }

    fn models() -> SettingPicker {
        SettingPicker {
            title: "what answers this conversation".into(),
            values: vec![
                SettingValue {
                    name: "local".into(),
                    ready: true,
                    ..SettingValue::default()
                },
                SettingValue {
                    name: "deepseek/deepseek-flash".into(),
                    ..SettingValue::default()
                },
                SettingValue {
                    name: "grok/grok-4.3".into(),
                    why: "the one this box pays for".into(),
                    ready: true,
                },
            ],
            current: "grok/grok-4.3".into(),
            selected: 2,
            empty: String::new(),
            legend: Some(
                "green: this box holds a key for it; the others ask for one when you take them"
                    .into(),
            ),
            consequence: vec!["this conversation only".into()],
        }
    }

    /// letibot `app/tests/session.rs` (the `/models` card: `← now` on the value in force)
    /// and `app/tests/scroll.rs` (the green rows and the legend that says what green means).
    #[test]
    fn the_setting_card_marks_the_value_in_force_and_says_what_green_means() {
        let c = models().content(110);
        let t = plain(&c.lines);
        assert_eq!(t[0], "what answers this conversation");
        let now = &t[c.stop_rows[2]];
        assert!(
            now.starts_with("▸  3  grok/grok-4.3") && now.ends_with("← now"),
            "{now:?}"
        );
        assert_eq!(t[c.stop_rows[2] + 1], "        the one this box pays for");
        let local = &c.lines[c.stop_rows[0]];
        assert_eq!(
            role_of(local, "local"),
            Some(Role::Success),
            "a keyed row is green"
        );
        assert_eq!(
            role_of(&c.lines[c.stop_rows[1]], "deepseek"),
            Some(Role::Plain),
            "a provider with no key is not"
        );
        assert!(
            t.iter()
                .any(|l| l.contains("green: this box holds a key for it"))
        );
        assert!(t.iter().any(|l| l.contains("this conversation only")));
        assert!(!t.iter().any(|l| l.contains("← now") && l.contains("local")));
    }

    #[test]
    fn a_setting_with_no_values_says_how_to_name_one() {
        let p = SettingPicker {
            title: "mode".into(),
            empty: "this daemon has not named its modes — `/mode NAME` still works, if you \
                    know the name."
                .into(),
            ..SettingPicker::default()
        };
        let t = plain(&p.content(140).lines);
        assert!(t[1].contains("has not named its modes"), "{t:?}");
        assert!(p.content(140).stop_rows.is_empty());
    }

    /// **A group header is a row of its own**: a fold, a count, a sentence under it, no number
    /// (a group line is not a number anyone can type) — and a hint that says what Enter does to
    /// *it*. letibot faked this row as a session with an empty id, and the footer then said
    /// "enter switches" about a row where Enter folds.
    #[test]
    fn a_group_header_folds_and_says_so_in_the_hint() {
        let mut p = SessionPicker {
            rows: vec![
                row(0, "this conversation", None).into(),
                SessionGroup {
                    name: "on disk".into(),
                    count: 3,
                    fold: Fold::Closed,
                    note: "switching to one resumes it".into(),
                }
                .into(),
                row(0, "an old one", None).into(),
            ],
            selected: 0,
        };
        let c = p.content(96);
        let t = plain(&c.lines);
        assert_eq!(t[4], " + on disk (3)", "{t:#?}");
        assert_eq!(t[5], "      switching to one resumes it", "{t:#?}");
        // The sessions keep their own numbering: a group consumes no number.
        assert_eq!(t[6], "    2  an old one", "{t:#?}");
        assert_eq!(c.stop_rows, vec![2, 4, 6], "a group is a stop");
        assert!(t.iter().any(|l| l.contains("enter switches ·")), "{t:#?}");
        // The cursor moved onto the group, and the hint followed it.
        p.selected = 1;
        let t = plain(&p.content(96).lines);
        assert!(t[4].starts_with("▸+ on disk (3)"), "{t:#?}");
        assert!(
            t.iter().any(|l| l.contains("enter unfolds it ·")),
            "a group says what Enter does to it: {t:#?}"
        );
    }

    /// **A group that cannot fold says so** — letibot's `mine` is always open — and nothing
    /// draws a fold for it or offers one: no `+`/`-`, no *folds* in the hint, and its `(N)`
    /// name still in the row that every other row's is. Two states could not say this, so the
    /// group had to claim a fold, and rano drew one where there was none.
    #[test]
    fn a_fixed_group_draws_no_fold_glyph_and_offers_no_fold_key() {
        let mut p = SessionPicker {
            rows: vec![
                SessionGroup {
                    name: "mine".into(),
                    count: 3,
                    fold: Fold::Fixed,
                    note: "the sessions this head started".into(),
                }
                .into(),
                row(0, "this conversation", None).into(),
            ],
            selected: 0,
        };
        let c = p.content(96);
        let t = plain(&c.lines);
        let group = t
            .iter()
            .find(|l| l.contains("mine (3)"))
            .expect("the group row")
            .clone();
        // The fold column is still reserved — three columns before the name, the mark and
        // the fold and the space — but nothing is drawn in it.
        assert_eq!(group, "▸  mine (3)", "{t:#?}");
        assert_eq!(t[3], "      the sessions this head started", "{t:#?}");
        // It is still a stop: the arrows reach it, and its own note is drawn under it.
        assert_eq!(c.stop_rows, vec![2, 4], "{t:#?}");
        // And no key on it folds anything, so the hint under it says none does: the sentence
        // opens with the picker's own half, not with an `enter …` for this row.
        let hint = p.hint_text().expect("a hint");
        assert!(!hint.contains("folds"), "{hint:?}");
        assert!(!hint.starts_with("enter"), "no key acts on it: {hint:?}");
        // A foldable group on the same cursor still says what Enter does to it.
        p.rows[0] = SessionGroup {
            name: "mine".into(),
            count: 3,
            fold: Fold::Open,
            note: "the sessions this head started".into(),
        }
        .into();
        assert!(p.hint_text().unwrap().contains("enter folds it away"));
    }

    /// **The picker's hint leaves as data**: a host's bar reads the sentence the picker draws
    /// instead of writing a second copy of it — the drift letibot measured (it had to spell
    /// this cursor-dependent line itself).
    #[test]
    fn the_pickers_hint_is_the_hint_it_draws() {
        let mut p = SessionPicker {
            rows: vec![
                row(0, "this conversation", None).into(),
                SessionGroup {
                    name: "on disk".into(),
                    count: 1,
                    fold: Fold::Closed,
                    note: String::new(),
                }
                .into(),
            ],
            selected: 0,
        };
        // Wide enough that `content` draws the hint whole: it cuts its rows to `w`.
        let drawn = plain(&p.content(400).lines);
        let hint_row = drawn
            .iter()
            .find(|l| l.contains("↑↓ moves"))
            .expect("a hint row");
        assert_eq!(p.hint_line().as_deref(), Some(hint_row.as_str()));
        assert_eq!(
            p.hint_text().as_deref(),
            Some(hint_row.trim_start()),
            "the bar takes the words, not the pane's frame"
        );
        assert!(p.hint_text().unwrap().starts_with("enter switches ·"));
        // The cursor moved to the group, and the host's copy of the hint moved with it.
        p.selected = 1;
        assert!(
            p.hint_text().unwrap().starts_with("enter unfolds it ·"),
            "{:?}",
            p.hint_text()
        );
        assert_eq!(
            p.hint_line().as_deref(),
            Some(
                plain(&p.content(400).lines)
                    .into_iter()
                    .find(|l| l.contains("↑↓ moves"))
                    .unwrap()
                    .as_str()
            )
        );
    }

    /// The fixture the golden pin below draws: a conversation with two children and a
    /// grandchild, a sibling with pinned facts, and enough rows that the number field is
    /// wider than two columns.
    fn golden_fixture() -> SessionPicker {
        let rows = vec![
            SessionRow {
                here: true,
                generating: true,
                rows: 42,
                heads: 1,
                model: "local".into(),
                workspace: "~/Projects/rano".into(),
                ..row(0, "this conversation", Some(true))
            },
            row(1, "the first child", Some(false)),
            SessionRow {
                live: false,
                rows: 7,
                ..row(2, "the grandchild", None)
            },
            row(1, "the second child", None),
            SessionRow {
                heads: 2,
                model: "grok/grok-4.3".into(),
                ..row(0, "a sibling", None)
            },
            row(0, "filler six", None),
            row(0, "filler seven", None),
            row(0, "filler eight", None),
            row(0, "filler nine", None),
            row(0, "filler ten", None),
        ];
        SessionPicker {
            rows: rows.into_iter().map(SessionItem::Session).collect(),
            selected: 1,
        }
    }

    /// **The body the session picker drew before the tree-list port** (R1a), pinned here
    /// first, so the port is a diff against a measurement rather than against memory.
    ///
    /// The *hint* below it is expected to change — it now follows the cursor — so this is
    /// everything above the frame's last blank row: the title, the tree, the fold column,
    /// the number field, the right-aligned facts and the full id under every row.
    const GOLDEN_BODY: &[&str] = &[
        "sessions in this daemon",
        "",
        " -  1  this conversation                                   generating · 42 rows · 1 head · local",
        "      s-this-conversation  ~/Projects/rano",
        "  ▸+  2  the first child",
        "      s-the-first-child",
        "        3  the grandchild                                                       on disk · 7 rows",
        "      s-the-grandchild",
        "      4  the second child",
        "      s-the-second-child",
        "    5  a sibling                                                         2 heads · grok/grok-4.3",
        "      s-a-sibling",
        "    6  filler six",
        "      s-filler-six",
        "    7  filler seven",
        "      s-filler-seven",
        "    8  filler eight",
        "      s-filler-eight",
        "    9  filler nine",
        "      s-filler-nine",
        "   10  filler ten",
        "      s-filler-ten",
    ];

    #[test]
    fn the_body_the_picker_drew_before_the_port_is_the_body_it_draws_after_it() {
        let drawn = plain(&golden_fixture().content(96).lines);
        let end = drawn
            .iter()
            .rposition(|l| l.is_empty())
            .expect("the frame's blank row before the hint");
        let body: Vec<&str> = drawn[..end].iter().map(String::as_str).collect();
        assert_eq!(body, GOLDEN_BODY);
    }
}
