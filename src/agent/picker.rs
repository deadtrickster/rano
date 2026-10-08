//! **The pickers**: the sessions a daemon holds, and a setting's values.
//!
//! Ported from letibot's `ui/panes/picker.rs` (`picker_lines`, `setting_picker_lines`). The
//! host does the enumeration — which sessions are listed, in which order, at which depth,
//! and which families are unfolded is session logic — and hands the rows over in drawing
//! order; this draws them.

use crate::agent::pane::{self, PaneLines};
use crate::agent::text::clean_line;
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

/// The session picker's view model.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionPicker {
    pub rows: Vec<SessionRow>,
    /// The cursor, an index into `rows`; clamped when drawn.
    pub selected: usize,
}

impl SessionPicker {
    /// Every row, each session a stop. letibot truncates this to the room rather than
    /// windowing it; a host may do either with the result.
    pub fn content(&self, w: usize) -> PaneLines {
        let mut out = PaneLines::new();
        out.push(pane::title("sessions in this daemon"));
        out.blank();
        if self.rows.is_empty() {
            out.push(pane::faint(
                "  none listed yet — the daemon has not answered, or this head is \
                 replaying a recorded log and has no daemon to ask.",
            ));
        }
        // **And the number's field is as wide as the list is long.** `{:>2}` is right for
        // nine-row lists and wrong for a box whose header reads `1/106`: row 100 renders `100`
        // in a two-wide field, so **every row from 100 on sits one column right of every row
        // before it** — MEASURED (rows 3–99 at column 6, rows 100–104 at column 7). `max(2)`
        // keeps a short list's frame exactly as it was.
        let digit_w = self.rows.len().to_string().len().max(2);
        let cursor = self.selected.min(self.rows.len().saturating_sub(1));
        for (at, s) in self.rows.iter().enumerate() {
            // The same ladder the decision prompt draws: the mark IS the thing Enter takes,
            // and the row it sits on is inverse. The session this head is in keeps its bold
            // name, so "where am I" and "what Enter takes" stay two readable facts even when
            // they are different rows.
            let picked = at == cursor;
            // **The fold is its own glyph beside the mark, and its COLUMN IS RESERVED either
            // way** — the session tree is why. An empty string for a childless row saved one
            // column, and MEASURED on a root with two children of which only one has a child:
            //
            // ```text
            //   ▸+  1  this conversation      name at col 7    depth 0, has children
            //      -  2  the first child       name at col 9    depth 1, has a child
            //         3  the grandchild        name at col 10   depth 2, no children
            //       4  the second child        name at col 8    depth 1, NO children
            // ```
            //
            // **Two siblings at ONE depth, one column apart**, so the name column was a
            // function of *has children* and not of *depth* — a list that cannot be read as a
            // tree. With the column reserved the step is the indent's own two columns at every
            // level. What it costs: a screen with no children on it moves one column right.
            //
            // **The fold is `+`/`-`, and it stopped being a triangle on purpose.** It was
            // `▾`/`▸` — the shapes the cursor is made of — so a PICKED row of a folded
            // conversation drew `▸▸`, two identical glyphs doing two different jobs, and the
            // operator's report is exactly that.
            let fold = match s.children {
                None => " ",
                Some(true) => "-",
                Some(false) => "+",
            };
            let indent = "  ".repeat(s.depth.min(3));
            let left = Line::new(vec![
                Span::raw(format!(
                    "{indent}{}{fold} {:>digit_w$}  ",
                    pane::mark(picked),
                    at + 1
                )),
                Span::role(
                    clean_line(&s.name),
                    if s.here { Role::Strong } else { Role::Plain },
                ),
            ]);
            // Busy is the fact a picker exists to show: switching away from a running turn is
            // fine — the daemon keeps generating — and switching *into* one is how you go back
            // and watch it.
            let mut facts: Vec<String> = Vec::new();
            if s.generating {
                facts.push("generating".into());
            }
            if !s.live {
                // The whole difference between a row that costs one keystroke and a row that
                // costs a resume. Said in a word rather than implied by an absent "generating".
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
            let right = Line::new(vec![Span::role(
                facts.join(" · "),
                if s.generating {
                    Role::Pending
                } else {
                    Role::Faint
                },
            )]);
            out.push_stop(pane::split_row(pane::picked(left, picked), right, w));
            // The full id under **every** row, not only the named ones: the sessions whose id
            // you might actually need to type — the unnamed ones — were the ones showing a
            // truncation. The workspace goes on the same line: for a stored session it is the
            // only thing on the row that says what the conversation was *about*.
            let under = if s.workspace.is_empty() {
                format!("      {}", clean_line(&s.session_id))
            } else {
                format!(
                    "      {}  {}",
                    clean_line(&s.session_id),
                    clean_line(&s.workspace)
                )
            };
            out.push(pane::faint(under));
        }
        out.blank();
        out.push(pane::faint(
            "  ↑↓ moves · enter switches · or type a number or part of a name and press \
             enter · /new [title] makes one · /rename NAME names this one · esc closes",
        ));
        out.push(pane::faint(
            "  switching does not stop anything: a turn keeps running in the session you \
             left, and it is still there when you come back.",
        ));
        out.trimmed(w)
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
        let p = SessionPicker { rows, selected: 0 };
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
                },
                SessionRow {
                    live: false,
                    ..row(0, "old", None)
                },
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
}
