//! **The todos pane** — the session's plan and who wrote each line, beside the repository's
//! `TODO.md` read-only — and **the new-todo card**.
//!
//! Ported from letibot's `ui/panes/todos.rs` and `ui/cards/todo.rs`. The plan is the
//! session's; the file is the project's, and the pane says which is which on the screen,
//! because the model is reminded of the first and never told about the second.
//!
//! The cursor walks the rows that can be acted on — the `[+]` row, the person's own rows,
//! and the file's items — and the pane records where each landed ([`PaneLines`]), so a click
//! and the arrows read the same record (letibot's *"mouse doesnt click"*).

use crate::render::{Line, Span};
use crate::style::Role;

use super::pane::{self, PaneLines};
use super::text::{clean_line, one};

/// A checklist mark: `[ ]` open, `[~]` doing, `[x]` done, `[p]` set aside.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TodoMark {
    Open,
    Doing,
    Done,
    Postponed,
}

impl TodoMark {
    /// A markdown checklist line's mark and text: `- [ ] text`, `* [x] text`. `[~]` and `[-]`
    /// are both *doing*, and `[X]` is *done*: the spellings people write.
    pub fn of(line: &str) -> Option<(TodoMark, &str)> {
        let t = line.trim_start();
        let rest = t.strip_prefix("- ").or_else(|| t.strip_prefix("* "))?;
        let (boxed, text) = rest.split_at_checked(3)?;
        let mark = match boxed {
            "[ ]" => TodoMark::Open,
            "[x]" | "[X]" => TodoMark::Done,
            "[~]" | "[-]" => TodoMark::Doing,
            _ => return None,
        };
        Some((mark, text.trim()))
    }

    pub fn glyph(self) -> &'static str {
        match self {
            TodoMark::Open => "[ ]",
            TodoMark::Doing => "[~]",
            TodoMark::Done => "[x]",
            TodoMark::Postponed => "[p]",
        }
    }

    /// The mark as a span: an open one plain, the others in the register that says where
    /// they are.
    pub fn span(self) -> Span {
        match self {
            TodoMark::Open => Span::raw(self.glyph()),
            TodoMark::Doing => Span::role(self.glyph(), Role::Pending),
            TodoMark::Done => Span::role(self.glyph(), Role::Success),
            TodoMark::Postponed => Span::role(self.glyph(), Role::Faint),
        }
    }
}

/// One line of the session's plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TodoLine {
    pub mark: TodoMark,
    pub content: String,
    /// The person wrote it (`you`), rather than the model: it is numbered for `/todo` and it
    /// is a stop.
    pub mine: bool,
    /// Its number among the person's rows, for `/todo postpone N`.
    pub number: usize,
    /// The job it waits on, if any.
    pub waits_on: Option<String>,
    /// The cursor is on it.
    pub cursor: bool,
}

/// One row of the repository's `TODO.md`, as the host parsed it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RepoRow {
    pub indent: usize,
    pub mark: Option<TodoMark>,
    pub text: String,
    /// The item's detail, shown when it is unfolded.
    pub body: Vec<String>,
    /// An item (a stop) rather than a section heading.
    pub item: bool,
    /// The cursor is on it.
    pub cursor: bool,
    /// It is unfolded.
    pub open: bool,
}

/// The todos pane's facts.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TodosPane {
    /// Rows still open (pending or in progress).
    pub open: usize,
    pub postponed: usize,
    /// The cursor is on the `[+]` row.
    pub on_add: bool,
    pub todos: Vec<TodoLine>,
    /// `None` until the file has been read.
    pub repo: Option<Vec<RepoRow>>,
}

impl TodosPane {
    /// Every row of the pane, each stop's first row recorded, cut to `w`.
    pub fn content(&self, w: usize) -> PaneLines {
        let mut out = PaneLines::new();
        out.push(pane::title(match self.postponed {
            0 => format!("todos — {} open", self.open),
            n => format!("todos — {} open · {n} postponed", self.open),
        }));
        out.blank();
        out.push(pane::faint(
            "  this session — the plan, and who wrote each line:",
        ));
        out.push_stop(Line::new(vec![
            Span::raw(format!("  {} ", pane::mark(self.on_add))),
            Span::role("[+]", Role::Strong),
            Span::raw(" "),
            Span::role(
                "add todo item — enter opens the card, or /todo TEXT",
                Role::Strong,
            ),
        ]));
        if self.todos.is_empty() {
            out.push(pane::faint(
                "    none yet. The model writes them with todo_write; `/todo TEXT` adds yours.",
            ));
        }
        for t in &self.todos {
            let number = if t.mine {
                format!("{:>2}  ", t.number)
            } else {
                "    ".to_string()
            };
            let who = if t.mine { "you" } else { "model" };
            let waiting = match &t.waits_on {
                Some(handle) => format!(" · waits on {}", clean_line(handle)),
                None => String::new(),
            };
            let l = Line::new(vec![
                Span::raw(format!("  {} {number}", pane::mark(t.cursor))),
                t.mark.span(),
                Span::raw(format!(" {}  ", clean_line(&t.content))),
                Span::role(format!("— {who}"), Role::Faint),
                // Painted even when empty: the faint run's open and close are on the row
                // whether or not the row waits on anything, as letibot's row always had.
                Span::role(waiting, Role::Faint),
            ]);
            if t.mine {
                out.push_stop(l);
            } else {
                out.push(l);
            }
        }
        out.blank();
        out.push(pane::faint(
            "  the model sees these and is reminded of them; it can mark one done, and cannot \
             remove yours",
        ));
        out.blank();
        out.push(pane::faint(
            "  the repo's TODO.md — what the project intends; not the model's plan:",
        ));
        match &self.repo {
            None => out.push(pane::faint("    not read yet — close and reopen the pane.")),
            Some(rows) => {
                if rows.is_empty() {
                    out.push(pane::faint("    no sections found."));
                }
                for r in rows {
                    let pad = " ".repeat(r.indent.saturating_sub(2));
                    let cursor = if r.cursor { "▸ " } else { "  " };
                    let more = if !r.body.is_empty() && !r.open {
                        " ···"
                    } else {
                        ""
                    };
                    let l = match r.mark {
                        Some(m) => Line::new(vec![
                            Span::raw(format!("{pad}{cursor}")),
                            m.span(),
                            Span::raw(format!(" {}{more}", clean_line(&r.text))),
                        ]),
                        None => pane::faint(format!("{pad}{cursor}{}", clean_line(&r.text))),
                    };
                    if r.item {
                        out.push_stop(l);
                    } else {
                        out.push(l);
                    }
                    if r.open {
                        for b in &r.body {
                            out.push(pane::faint(format!("{pad}        {}", clean_line(b))));
                        }
                    }
                }
            }
        }
        out.blank();
        out.push(pane::faint(
            "  the file itself is in the workspace; this pane never writes it, and the model is \
             never told about it — nothing here is a task it has been given.",
        ));
        out.push(pane::faint(
            "  ↑↓ moves (or click a row) · enter on [+] adds, on your row toggles it, on a repo \
             item unfolds · esc closes",
        ));
        out.push(pane::faint(
            "  `[p]` is a row you set aside — it stays on the board and the model still sees it:",
        ));
        out.push(pane::faint(
            "  the check stops asking about it · `/todo postpone N` · `/todo resume N`",
        ));
        out.trimmed(w)
    }
}

/// Which field of the new-todo card has the keyboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TodoField {
    #[default]
    Title,
    Detail,
    When,
}

/// **The new-todo card**: the three fields, which one is being typed into, and the keys.
///
/// The field with the keyboard shows what is being typed (the host passes the live text in
/// its place), in the strong register; the others are faint.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TodoCard {
    pub title: String,
    pub detail: String,
    /// A job handle: the row comes up again when that job is not running.
    pub when: String,
    pub focus: TodoField,
}

impl TodoCard {
    pub fn lines(&self, w: usize) -> Vec<Line> {
        let field = |key: &str, which: TodoField, value: &str, empty: &str| {
            let head = Span::role(format!("  {key:<7} "), Role::Faint);
            let body = if value.is_empty() {
                Span::role(empty.to_string(), Role::Faint)
            } else if self.focus == which {
                Span::role(clean_line(value), Role::Strong)
            } else {
                Span::role(clean_line(value), Role::Faint)
            };
            Line::new(vec![head, body])
        };
        let mut out = vec![one("adding a todo item", Role::Strong), Line::default()];
        out.push(field("title", TodoField::Title, &self.title, "(empty)"));
        out.push(field("detail", TodoField::Detail, &self.detail, "(empty)"));
        out.push(field(
            "when",
            TodoField::When,
            &self.when,
            "(waits on nothing)",
        ));
        out.push(Line::default());
        for (k, why) in [
            ("tab", "moves between the fields"),
            ("enter", "adds it to the session's plan, marked as yours"),
            ("esc", "cancels, and adds nothing"),
        ] {
            out.push(Line::new(vec![
                Span::role(format!("  {k:<7}"), Role::Faint),
                Span::raw(why),
            ]));
        }
        out.push(Line::default());
        out.push(one(
            "  `when` is a JOB handle: the row is filed now and comes up again when that job is \
             not running. A job this daemon has never heard of counts as ended, which is what a \
             restart looks like.",
            Role::Faint,
        ));
        out.push(one(
            "  the model sees these and is reminded of them; it can mark one done, and cannot \
             remove yours",
            Role::Faint,
        ));
        out.into_iter()
            .map(|l| crate::render::text::truncate(&l, w))
            .collect()
    }
}

super::lines_widget!(TodoCard);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::testing::{plain, role_of};
    use crate::style::Palette;

    fn todo(mark: TodoMark, content: &str, mine: bool, number: usize) -> TodoLine {
        TodoLine {
            mark,
            content: content.into(),
            mine,
            number,
            waits_on: None,
            cursor: false,
        }
    }

    /// The `[+]` row and the person's own rows are stops; the model's rows are not.
    #[test]
    fn the_add_row_and_your_rows_are_stops_and_the_models_are_not() {
        let p = TodosPane {
            open: 2,
            postponed: 1,
            on_add: true,
            todos: vec![
                todo(TodoMark::Doing, "write the port", false, 0),
                todo(TodoMark::Open, "review it", true, 1),
            ],
            repo: Some(vec![RepoRow {
                indent: 8,
                mark: Some(TodoMark::Done),
                text: "ship".into(),
                item: true,
                ..RepoRow::default()
            }]),
        };
        let c = p.content(120);
        let rows = plain(&c.lines);
        assert_eq!(rows[0], "todos — 2 open · 1 postponed");
        assert_eq!(
            rows[3],
            "  ▸ [+] add todo item — enter opens the card, or /todo TEXT"
        );
        assert_eq!(rows[4], "        [~] write the port  — model");
        assert_eq!(rows[5], "     1  [ ] review it  — you");
        let ship = rows.iter().position(|l| l.contains("ship")).unwrap();
        assert_eq!(c.stop_rows, vec![3, 5, ship]);
        assert_eq!(role_of(&c.lines[4], "[~]"), Some(Role::Pending));
    }

    /// letibot's bytes: the empty `waits on` run is still painted.
    #[test]
    fn a_row_waiting_on_nothing_keeps_its_empty_run() {
        let p = TodosPane {
            todos: vec![todo(TodoMark::Open, "x", true, 1)],
            repo: None,
            ..TodosPane::default()
        };
        assert_eq!(
            p.content(120).lines[4].to_ansi(Palette::Colour),
            "     1  [ ] x  \x1b[2m— you\x1b[0m\x1b[2m\x1b[0m"
        );
    }

    /// A repo item with a body shows `···` folded and its body unfolded.
    #[test]
    fn a_repo_item_unfolds_its_body() {
        let mut r = RepoRow {
            indent: 8,
            mark: Some(TodoMark::Open),
            text: "port".into(),
            body: vec!["the panes".into()],
            item: true,
            cursor: true,
            open: false,
        };
        let pane = |r: &RepoRow| TodosPane {
            repo: Some(vec![r.clone()]),
            ..TodosPane::default()
        };
        let rows = plain(&pane(&r).content(120).lines);
        assert!(
            rows.iter().any(|l| l == "      ▸ [ ] port ···"),
            "{rows:#?}"
        );
        r.open = true;
        let rows = plain(&pane(&r).content(120).lines);
        assert!(rows.iter().any(|l| l == "      ▸ [ ] port"));
        assert!(rows.iter().any(|l| l == "              the panes"));
    }

    #[test]
    fn a_checklist_line_reads_its_mark() {
        assert_eq!(TodoMark::of("- [ ] a"), Some((TodoMark::Open, "a")));
        assert_eq!(TodoMark::of("  * [X] b"), Some((TodoMark::Done, "b")));
        assert_eq!(TodoMark::of("- [-] c"), Some((TodoMark::Doing, "c")));
        assert_eq!(TodoMark::of("- c"), None);
    }

    /// The field with the keyboard is strong, an empty one says what empty means.
    #[test]
    fn the_card_shows_which_field_is_being_typed() {
        let c = TodoCard {
            title: "port the panes".into(),
            focus: TodoField::Title,
            ..TodoCard::default()
        };
        let lines = c.lines(100);
        assert_eq!(lines[2].plain(), "  title   port the panes");
        assert_eq!(role_of(&lines[2], "port"), Some(Role::Strong));
        assert_eq!(lines[4].plain(), "  when    (waits on nothing)");
    }
}
