//! **One tree list: selectable rows, a fold column, numbers, facts and a hint that
//! follows the cursor** — drawn from data, so the behaviour lives in the data.
//!
//! # Why this exists
//!
//! rano had three hand-written renderers for the same shape — the session picker
//! ([`crate::agent::picker`]), the subagents pane ([`crate::agent::subagents`]) and the jobs
//! pane ([`crate::agent::jobs`]) — and letibot had a fourth that drew a plugin's tree. Each
//! one grew its own fold column, its own numbering, its own right-aligned facts, its own
//! second line, and **its own hard-coded footer**. That footer is the defect this module was
//! written for: the session picker's said *enter switches* on a group row, where Enter folds,
//! because the sentence was written once for the list rather than read off the row the cursor
//! is on. A list whose rows are data and whose keys are data cannot say the wrong thing about
//! a row: the hint is the **selected row's own [`Action`]s**.
//!
//! The same reason makes it the boundary a plugin can cross. A pane hook answers with data,
//! never with drawing code, so the shape it answers with has to be the shape the renderer
//! takes — one [`Row`] carrying an id, its cells, its role, its second line, its actions and
//! its children, which is exactly what letibot's C3 `Row` carries. A host maps one onto the
//! other with a plain function and the plugin gets the same list, the same marks and the same
//! cursor keys as a built-in pane, without knowing a terminal exists.
//!
//! # The shape, and the numbers in it
//!
//! ```text
//! {depth indent}{mark}{fold} {number}  {cells…}            {facts…}
//! {detail column}{detail}
//! ```
//!
//! * **The fold column is one column, reserved at every depth** — `-` open, `+` closed, a
//!   blank for a leaf — because the alternative measured as a broken tree: a childless row's
//!   blank fold was dropped and two siblings at one depth landed one column apart, so the name
//!   column was a function of *has children* rather than of *depth*. The session picker's
//!   `SessionPicker::content` doc carries the measurement.
//! * **The number field is as wide as the list is long** (`max(2)`, so a short list's frame
//!   does not move), and a row may opt out: a group header is not a number you can type.
//! * **The mark is the row Enter takes** ([`pane::mark`]), and the row it sits on is inverse
//!   ([`pane::picked`]) — `where the cursor is` and `what Enter takes` are the same row here
//!   because the cursor only ever rests on a stop.
//! * **A row with no `id` is not a stop**: a heading, a group's legend and a row's second line
//!   are rows the cursor passes over. The stops are recorded as they are drawn
//!   ([`PaneLines::push_stop`]) so the host's click mapping and its arrows read one record.
//! * **The hint is the selected row's actions, then the caller's fixed keys.** The part that
//!   changes with the cursor comes first, because that is the part the eye has to catch: a
//!   reader who has learnt the fixed half should not have to hunt for the word that moved.
//!
//! Everything here is the caller's data: this module knows nothing about sessions, children,
//! jobs or plugins, and never writes a sentence about any of them.

use crate::render::{Line, Span};
use crate::style::Role;

use super::pane::{self, PaneLines};
use super::text::clean_line;

/// One run of a row's text, and what it means.
///
/// **On the wire a cell is the string it is when it means the row's own role**, and an object
/// `{"text": …, "role": …}` when it means something else — which is exactly C3's
/// `cells: [String]`, so a plugin's tree deserialises straight into a [`Row`] rather than
/// through a mapping function that has to exist in every host.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct Cell {
    pub text: String,
    /// The role this run is drawn in. `None` takes the row's own [`Row::role`], and then
    /// [`Role::Plain`] — so a caller says *this whole row is faint* once, and one cell of it
    /// something else.
    pub role: Option<Role>,
}

impl<'de> serde::Deserialize<'de> for Cell {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Cell, D::Error> {
        #[derive(serde::Deserialize)]
        #[serde(untagged)]
        enum Wire {
            Text(String),
            Full { text: String, role: Role },
        }
        Ok(match Wire::deserialize(d)? {
            Wire::Text(text) => Cell { text, role: None },
            Wire::Full { text, role } => Cell {
                text,
                role: Some(role),
            },
        })
    }
}

impl Cell {
    /// A cell in the row's own role.
    pub fn new(text: impl Into<String>) -> Cell {
        Cell {
            text: text.into(),
            role: None,
        }
    }

    /// A cell that says what it is regardless of its row.
    pub fn role(text: impl Into<String>, r: Role) -> Cell {
        Cell {
            text: text.into(),
            role: Some(r),
        }
    }
}

/// **A key that does something on the row it is on**, and what it does — `enter` / `switches`.
///
/// This is the whole of the hint's behaviour: the sentence under the list is composed from
/// these, so a group row's `enter` / `folds it away` and a session row's `enter` / `switches`
/// are two answers from one renderer, and neither can be hard-coded wrong.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Action {
    pub key: String,
    pub label: String,
}

impl Action {
    pub fn new(key: impl Into<String>, label: impl Into<String>) -> Action {
        Action {
            key: key.into(),
            label: label.into(),
        }
    }
}

/// **One row of the list**, in the order it is drawn.
///
/// `id` is the whole of stop-ness: a row with one is a place the cursor can rest and a click
/// can land, and a row without is a line the list passes over (a heading, an empty state, a
/// legend). Every other field is optional and absent means *not there* rather than *zero*: an
/// absent number is not `0`, an absent detail is not an empty line.
///
/// `children` is the nested form a plugin answers with; [`flatten`] turns it into the flat
/// rows this list draws. A caller that already holds a flat list — the session picker's
/// host-side tree walk — sets [`Row::depth`] itself and leaves `children` empty.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Row {
    /// **What the row is**, as the host names it: a session id, a job handle. `None` is not a
    /// stop.
    pub id: Option<String>,
    /// How deep in the tree, in indent steps — 0 for a root.
    pub depth: usize,
    /// The number drawn in the list's field, or `None` for a row that opts out of numbering.
    pub number: Option<usize>,
    /// `Some(true)` has children and is open, `Some(false)` has children and is closed,
    /// `None` is a leaf. Only read when the list reserves a fold column.
    pub open: Option<bool>,
    /// The role the whole row is drawn in, unless a cell says its own.
    pub role: Option<Role>,
    /// The left text, joined by one space.
    pub cells: Vec<Cell>,
    /// The right-aligned text, joined by ` · ` — aligned to the list's edge, and dropped when
    /// it and the left together do not fit.
    pub facts: Vec<Cell>,
    /// The faint lines under the row, each at [`Row::detail_column`].
    pub detail: Vec<String>,
    /// Where the detail lines start, when this row is not at the list's own column — the jobs
    /// pane's `finished` group sits at 7 and a job's facts at 9.
    pub detail_column: Option<usize>,
    /// The keys that act on this row, for the hint line when the cursor is on it.
    pub actions: Vec<Action>,
    /// The nested form: [`flatten`] turns these into rows of their own, one level deeper.
    pub children: Vec<Row>,
}

impl Row {
    /// **The row made a stop**: the cursor can rest on it and a click can land on it.
    pub fn stop(mut self, id: impl Into<String>) -> Row {
        self.id = Some(id.into());
        self
    }

    /// This row is number `n` in the list's field.
    pub fn number(mut self, n: usize) -> Row {
        self.number = Some(n);
        self
    }
}

/// **A nested tree as the flat rows the list draws**: depth-first, a parent before its
/// children, each child one indent step deeper.
///
/// **A folded row's children are not drawn** — its `open` is `Some(false)` and they stay in
/// the model — and a row that has children and did not say whether it is open is open, which
/// is the reading that loses nothing: a plugin's tree arrives expanded, and a caller that
/// means a group folded says `open: Some(false)`.
pub fn flatten(rows: &[Row]) -> Vec<Row> {
    fn walk(rows: &[Row], depth: usize, out: &mut Vec<Row>) {
        for r in rows {
            let mut flat = r.clone();
            flat.depth = depth;
            flat.children = Vec::new();
            if !r.children.is_empty() && flat.open.is_none() {
                flat.open = Some(true);
            }
            let folded = flat.open == Some(false) && !r.children.is_empty();
            out.push(flat);
            if !folded {
                walk(&r.children, depth + 1, out);
            }
        }
    }
    let mut out = Vec::new();
    walk(rows, 0, &mut out);
    out
}

/// **The list**: rows, the cursor, and the frame around them.
///
/// A caller hands this its rows and the state it holds (which row the cursor is on, which
/// groups are open — already folded into the rows it built) and gets back [`PaneLines`]. The
/// frame is data too, because the panes that share this shape do not share their margins:
/// the session picker's second line starts at column 6, the subagents pane's at 7, and their
/// footers at 2 and 4.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TreeList {
    /// The frame's title, drawn strong on the first row; empty draws none.
    pub title: String,
    /// The rows, in drawing order. [`flatten`] builds this from a nested tree.
    pub rows: Vec<Row>,
    /// **The cursor, an index into the STOPS** — the rows with an `id` — not into `rows`;
    /// clamped when drawn. Same convention as [`PaneLines::row_of`].
    pub selected: usize,
    /// Drawn faint under the title when there are no rows. Empty draws nothing.
    pub empty: String,
    /// Reserve the one-column fold column. A list with no tree in it turns this off and its
    /// rows begin one column earlier.
    pub fold_column: bool,
    /// The column a row's detail lines start at, unless the row sets its own.
    pub detail_column: usize,
    /// The left margin of the empty state, the hint and the notes.
    pub margin: usize,
    /// The fixed half of the hint line — the keys that hold wherever the cursor is. Drawn
    /// after the selected row's own actions.
    pub hint: Vec<String>,
    /// Sentences drawn under the hint, each on a faint row of its own.
    pub notes: Vec<String>,
}

impl TreeList {
    /// How many rows the cursor can rest on.
    pub fn stops(&self) -> usize {
        self.rows.iter().filter(|r| r.id.is_some()).count()
    }

    /// **The row the cursor is on**, clamped the way the drawing clamps it: the table can
    /// move under the cursor (a child settling, a session going away), and an arrow pressed
    /// against a shorter list must land on a row rather than on an index that no longer
    /// exists.
    pub fn selected_row(&self) -> Option<&Row> {
        let at = self.selected.min(self.stops().saturating_sub(1));
        self.rows.iter().filter(|r| r.id.is_some()).nth(at)
    }

    /// **The number field's width**: as wide as the list is long, and never narrower than
    /// two, so a nine-row list's frame is the frame it always had. Zero when no row is
    /// numbered at all — then there is no field to reserve.
    pub fn number_width(&self) -> usize {
        if self.rows.iter().any(|r| r.number.is_some()) {
            self.rows.len().to_string().len().max(2)
        } else {
            0
        }
    }

    /// **The hint line**: the selected row's own actions, then the caller's fixed keys.
    pub fn hint_line(&self) -> Option<String> {
        let mut items: Vec<String> = Vec::new();
        if let Some(r) = self.selected_row() {
            for a in &r.actions {
                items.push(format!("{} {}", clean_line(&a.key), clean_line(&a.label)));
            }
        }
        items.extend(self.hint.iter().map(|h| clean_line(h)));
        if items.is_empty() {
            return None;
        }
        Some(format!("{}{}", " ".repeat(self.margin), items.join(" · ")))
    }

    /// Every row of the list, each stop's first row recorded, cut to `w`.
    pub fn content(&self, w: usize) -> PaneLines {
        let mut out = PaneLines::new();
        let margin = " ".repeat(self.margin);
        if !self.title.is_empty() {
            out.push(pane::title(clean_line(&self.title)));
            out.blank();
        }
        if self.rows.is_empty() && !self.empty.is_empty() {
            out.push(pane::faint(format!("{margin}{}", clean_line(&self.empty))));
        }
        let digits = self.number_width();
        let cursor = self.selected.min(self.stops().saturating_sub(1));
        let mut at = 0usize;
        for r in &self.rows {
            // **A row without an id is not a stop**, so it is neither a place the cursor can
            // be nor a place a click can land: it is drawn, and that is all it is.
            let is_stop = r.id.is_some();
            let picked = is_stop && at == cursor;
            if is_stop {
                at += 1;
            }
            let line = self.row_line(r, picked, digits, w);
            if is_stop {
                out.push_stop(line);
            } else {
                out.push(line);
            }
            let column = r.detail_column.unwrap_or(self.detail_column);
            let pad = " ".repeat(column);
            for d in &r.detail {
                out.push(pane::faint(format!("{pad}{}", clean_line(d))));
            }
        }
        let hint = self.hint_line();
        if hint.is_some() || !self.notes.is_empty() {
            out.blank();
            if let Some(h) = hint {
                out.push(pane::faint(h));
            }
            for n in &self.notes {
                out.push(pane::faint(format!("{margin}{}", clean_line(n))));
            }
        }
        out.trimmed(w)
    }

    /// One row: the lead (indent, mark, fold, number), the cells, then the facts at the edge.
    fn row_line(&self, r: &Row, picked: bool, digits: usize, w: usize) -> Line {
        // The fold is one column and its column is reserved whether or not this row has a
        // glyph for it: that is what keeps a name's column a function of its depth.
        let fold = if self.fold_column {
            match r.open {
                None => " ",
                Some(true) => "-",
                Some(false) => "+",
            }
        } else {
            ""
        };
        let number = match (r.number, digits) {
            (Some(n), d) if d > 0 => format!("{n:>d$}  "),
            _ => String::new(),
        };
        let lead = format!(
            "{}{}{} {}",
            "  ".repeat(r.depth),
            pane::mark(picked),
            fold,
            number
        );
        let mut left = Line::new(vec![Span::raw(lead)]);
        for (i, c) in r.cells.iter().enumerate() {
            if i > 0 {
                left.push(Span::raw(" "));
            }
            left.push(Span::role(clean_line(&c.text), self.role_of(r, c)));
        }
        if r.facts.is_empty() {
            return pane::picked(left, picked);
        }
        let mut right = Line::new(Vec::new());
        for (i, f) in r.facts.iter().enumerate() {
            if i > 0 {
                right.push(Span::raw(" · "));
            }
            right.push(Span::role(clean_line(&f.text), self.role_of(r, f)));
        }
        pane::split_row(pane::picked(left, picked), right, w)
    }

    /// A cell's role: its own, else the row's, else plain.
    fn role_of(&self, r: &Row, c: &Cell) -> Role {
        c.role.or(r.role).unwrap_or(Role::Plain)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::testing::{plain, role_of};
    use crate::width::text as wt;

    fn row(depth: usize, open: Option<bool>, name: &str) -> Row {
        Row {
            depth,
            open,
            cells: vec![Cell::new(name)],
            ..Row::default()
        }
    }

    fn list(rows: Vec<Row>) -> TreeList {
        TreeList {
            rows,
            fold_column: true,
            ..TreeList::default()
        }
    }

    /// The column `name` starts at, measured on the drawn text.
    fn column(drawn: &[String], name: &str) -> usize {
        let l = drawn.iter().find(|l| l.contains(name)).expect(name);
        wt::width(&l[..l.find(name).unwrap()])
    }

    /// **The fold column is reserved at every depth.** The defect it prevents is measured in
    /// the session picker's own doc: two siblings at ONE depth landing one column apart
    /// because one of them had children, which is a list that cannot be read as a tree.
    #[test]
    fn the_fold_column_is_reserved_at_every_depth() {
        let t = list(vec![
            row(0, Some(true), "root open").stop("root"),
            row(1, Some(false), "child closed").stop("closed"),
            row(1, None, "child leaf").stop("leaf"),
            row(2, None, "grandchild").stop("grand"),
        ]);
        let drawn = plain(&t.content(80).lines);
        // The two siblings of one depth share a column, whatever their fold.
        assert_eq!(
            column(&drawn, "child closed"),
            column(&drawn, "child leaf"),
            "{drawn:#?}"
        );
        assert_eq!(
            column(&drawn, "grandchild"),
            column(&drawn, "child leaf") + 2
        );
        assert_eq!(
            column(&drawn, "child leaf"),
            column(&drawn, "root open") + 2
        );
        // And the glyphs: `-` open, `+` closed, a blank that is still a column for a leaf.
        assert!(drawn[0].starts_with("▸- root open"), "{drawn:#?}");
        assert!(drawn[1].starts_with("   + child closed"), "{drawn:#?}");
    }

    /// The number's field is as wide as the list is long, and never narrower than two:
    /// `1/106` in a two-wide field put every row from 100 one column right of the rows
    /// before it (the session picker's measurement).
    #[test]
    fn the_number_field_is_as_wide_as_the_list_is_long() {
        let numbered = |n: usize| {
            let rows = (1..=n)
                .map(|i| {
                    row(0, None, &format!("row {i}"))
                        .number(i)
                        .stop(format!("r{i}"))
                })
                .collect();
            plain(&list(rows).content(60).lines).join("\n")
        };
        // Nine rows: a two-wide field, the frame a short list always had.
        assert!(numbered(9).contains("   1  row 1\n"), "{}", numbered(9));
        assert!(numbered(10).contains("   1  row 1\n"), "{}", numbered(10));
        assert!(numbered(10).contains("  10  row 10"), "{}", numbered(10));
        // A hundred rows: three columns, and rows 1 and 99 start their text in one column.
        let hundred = numbered(100);
        assert!(hundred.contains("    1  row 1\n"), "{}", hundred);
        assert!(hundred.contains("  100  row 100"), "{}", hundred);
        let drawn: Vec<String> = hundred.lines().map(str::to_string).collect();
        assert_eq!(column(&drawn, "row 1"), column(&drawn, "row 99"));
    }

    /// **A row that opts out of numbering gets no field at all** — a group header is not a
    /// number anyone can type, and the cells after it start where the number would have.
    #[test]
    fn a_row_that_opts_out_of_numbering_is_drawn_without_one() {
        let t = list(vec![
            Row {
                open: Some(false),
                cells: vec![Cell::new("finished (2)")],
                ..Row::default()
            },
            row(0, None, "one").stop("a").number(1),
            row(0, None, "two").stop("b").number(2),
        ]);
        let drawn = plain(&t.content(60).lines);
        assert_eq!(drawn[0], " + finished (2)", "{drawn:#?}");
        assert_eq!(drawn[1], "▸   1  one", "{drawn:#?}");
        assert_eq!(drawn[2], "    2  two", "{drawn:#?}");
        // A list with no numbers at all reserves no column.
        let unnumbered = list(vec![row(0, None, "only").stop("a")]);
        assert_eq!(plain(&unnumbered.content(60).lines)[0], "▸  only");
    }

    /// **The hint is the selected row's own actions**, so a group row and a row that opens
    /// something cannot say the same words: this is the defect (`enter switches` on a group
    /// line, where Enter folds) written as a test.
    #[test]
    fn the_hint_is_computed_from_the_row_the_cursor_is_on() {
        let mut t = list(vec![
            Row {
                id: Some("session".into()),
                cells: vec![Cell::new("this conversation")],
                actions: vec![Action::new("enter", "switches")],
                ..Row::default()
            },
            Row {
                id: Some("finished".into()),
                open: Some(false),
                cells: vec![Cell::new("finished (2)")],
                actions: vec![Action::new("enter", "folds it away")],
                ..Row::default()
            },
        ]);
        t.margin = 2;
        t.hint = vec!["↑↓ moves".into(), "esc closes".into()];
        let hint = |t: &mut TreeList, sel: usize| {
            t.selected = sel;
            plain(&t.content(60).lines)
                .into_iter()
                .find(|l| l.contains("↑↓ moves"))
                .expect("a hint line")
        };
        assert_eq!(hint(&mut t, 0), "  enter switches · ↑↓ moves · esc closes");
        assert_eq!(
            hint(&mut t, 1),
            "  enter folds it away · ↑↓ moves · esc closes"
        );
        // A row with no actions says only the fixed half.
        t.rows[1].actions.clear();
        assert_eq!(hint(&mut t, 1), "  ↑↓ moves · esc closes");
        // And an empty list still says the fixed half rather than nothing at all.
        t.rows.clear();
        assert_eq!(hint(&mut t, 0), "  ↑↓ moves · esc closes");
    }

    /// **A row with no id is not a stop**: it is drawn, it takes no mark, the cursor never
    /// lands on it, and the stop record skips it.
    #[test]
    fn a_row_with_no_id_is_drawn_but_is_not_a_stop() {
        let t = list(vec![
            Row {
                cells: vec![Cell::new("a heading")],
                ..Row::default()
            },
            row(0, None, "first").stop("a"),
            row(0, None, "second").stop("b"),
        ]);
        let c = t.content(60);
        let drawn = plain(&c.lines);
        assert_eq!(c.stop_rows, vec![1, 2], "{drawn:#?}");
        assert!(!drawn[0].starts_with('▸'), "a heading takes no mark");
        assert_eq!(t.stops(), 2);
        // The cursor is an index into the stops, so stop 1 is the third row.
        assert_eq!(
            t.selected_row().map(|r| r.id.clone()),
            Some(Some("a".into()))
        );
    }

    /// The second line is faint and starts at the list's column, or at the row's own when a
    /// row says so — the jobs pane's group sits at 7 and a job's facts at 9.
    #[test]
    fn the_second_line_sits_where_the_list_or_the_row_says() {
        let mut t = TreeList {
            detail_column: 6,
            ..list(vec![
                Row {
                    id: Some("a".into()),
                    cells: vec![Cell::new("a row")],
                    detail: vec!["the first line".into(), "the second".into()],
                    ..Row::default()
                },
                Row {
                    id: Some("b".into()),
                    cells: vec![Cell::new("another")],
                    detail: vec!["further right".into()],
                    detail_column: Some(9),
                    ..Row::default()
                },
            ])
        };
        t.fold_column = false;
        let drawn = plain(&t.content(60).lines);
        assert_eq!(drawn[1], "      the first line");
        assert_eq!(drawn[2], "      the second");
        assert_eq!(drawn[4], "         further right");
        assert_eq!(
            role_of(&t.content(60).lines[1], "the first line"),
            Some(Role::Faint)
        );
    }

    /// The nested form a plugin answers with: depth comes from the nesting, a folded row's
    /// children stay in the model and off the screen, and an id makes a stop.
    #[test]
    fn a_nested_tree_flattens_to_depths_and_a_folded_row_hides_its_children() {
        let tree = vec![Row {
            id: Some("root".into()),
            cells: vec![Cell::new("root")],
            children: vec![
                Row {
                    cells: vec![Cell::new("kept")],
                    ..Row::default()
                },
                Row {
                    open: Some(false),
                    cells: vec![Cell::new("shut")],
                    children: vec![row(0, None, "hidden")],
                    ..Row::default()
                },
            ],
            ..Row::default()
        }];
        let flat = flatten(&tree);
        let names: Vec<String> = flat
            .iter()
            .map(|r| r.cells[0].text.clone())
            .collect::<Vec<_>>();
        assert_eq!(names, vec!["root", "kept", "shut"]);
        assert_eq!(flat[1].depth, 1);
        assert_eq!(flat[2].open, Some(false), "a row with children is foldable");
        assert_eq!(
            flat[0].open,
            Some(true),
            "and open unless it said otherwise"
        );
        let drawn = plain(&list(flat.clone()).content(60).lines);
        assert_eq!(drawn[0], "▸- root", "{drawn:#?}");
        assert_eq!(drawn[1], "     kept");
        assert_eq!(drawn[2], "   + shut");
        // The root and the group are stops; the "kept" row has no id and is not.
        assert!(!drawn.iter().any(|l| l.contains("hidden")));
    }

    /// **The C3 row crosses the boundary with nothing lost**: id, cells, role, detail,
    /// actions and children arrive as one [`Row`], which is what letibot maps a plugin's
    /// tree onto.
    #[test]
    fn a_plugins_tree_row_arrives_whole() {
        let json = r#"{
            "id": "s-1",
            "cells": ["[~]", "port the widgets"],
            "role": "pending",
            "detail": ["s-1a2b · role coder"],
            "actions": [{"key": "enter", "label": "opens it"}],
            "children": [
                {"cells": ["a child"], "role": "plain"}
            ]
        }"#;
        let r: Row = serde_json::from_str(json).expect("a C3 row");
        assert_eq!(r.id.as_deref(), Some("s-1"));
        assert_eq!(r.role, Some(Role::Pending));
        assert_eq!(r.cells[1].text, "port the widgets");
        assert_eq!(r.cells[1].role, None, "a bare string takes the row's role");
        assert_eq!(r.detail, vec!["s-1a2b · role coder".to_string()]);
        assert_eq!(r.actions[0], Action::new("enter", "opens it"));
        assert_eq!(r.children.len(), 1);
        // A cell with a role of its own is the object form, and it survives the round trip.
        let one = serde_json::to_string(&Cell::role("x", Role::Success)).expect("a cell");
        assert_eq!(one, r#"{"text":"x","role":"success"}"#);
        assert_eq!(
            serde_json::from_str::<Cell>(&one).unwrap(),
            Cell::role("x", Role::Success)
        );
        let flat = flatten(std::slice::from_ref(&r));
        assert_eq!(flat.len(), 2);
        assert_eq!(flat[1].depth, 1);
        // And it draws: the row's role reaches its cells that name none.
        let c = list(flat).content(60);
        let drawn = plain(&c.lines);
        assert!(drawn[0].contains("[~] port the widgets"), "{drawn:#?}");
        assert_eq!(
            role_of(&c.lines[0], "port the widgets"),
            Some(Role::Pending)
        );
        assert_eq!(role_of(&c.lines[2], "a child"), Some(Role::Plain));
    }
}
