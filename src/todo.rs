//! TODO.md — the markdown todo standard, as a schema over byte ranges.
//!
//! The target is the `todo-md` standard (`github.com/todo-md/todo-md`), read
//! rather than paraphrased. Its rules, quoted:
//!
//! - *"Every todo markdown file starts with the `# TODO` header."*
//! - *"Each subheader is a todo section."*
//! - *"The tasks themself are one liners that start with either `'- [ ] '`,
//!   `'- [-] '` or `'- [x] '`."*
//! - *"A task can be in the following states: open / declined / done /
//!   deleted (removed from the document)"* — the fourth is the absence of a
//!   line, not a marker.
//! - *"Tasks can be assigned to people using `@USERNAME` format. Tasks can be
//!   tagged using the `#TAG` format."*
//!
//! **`[-]` means DECLINED** — not "in progress", which is the reading the third
//! state usually gets, and which the wire's `InProgress` would want. The other
//! repo called TODO.md (`todomd/todo.md`) has only two states and makes
//! checkboxes optional, so the two differ; this module follows the three-state
//! one.
//!
//! # There is no re-render, and that is the design
//!
//! Nothing here turns a model back into markdown. The document is the only
//! source text; the schema holds **byte ranges into it**, and every operation
//! yields an [`Edit`] — one `(range, replacement)` splice the caller applies.
//! Prose between tasks, the `~3d #tag @name 2020-03-20` tail, wrapping and
//! trailing whitespace are never represented, so they cannot be lost. An `id`
//! or `dependsOn` field would live in that tail; this module reads only the
//! marker, so it neither supports nor precludes one.
//!
//! A state change is exactly three bytes, because every marker is three bytes
//! and the range comes from the parse tree. That is what makes a cascade
//! byte-identical **by construction** rather than by care.
//!
//! # The grammar gap, which is why [`Doc::check`] exists
//!
//! `tree-sitter-md` has `task_list_marker_checked` and
//! `task_list_marker_unchecked` — two. The standard has three. `- [-]` is not an
//! error to the grammar and is not captured: it parses as an ordinary list item
//! whose paragraph happens to begin with brackets, `has_error == false`. So a
//! declined task is invisible to every query, and naming it needs a validated
//! read at a grammar-given offset (the end of the item's own list marker). A
//! marker the standard does not define is reported by [`Doc::check`] rather than
//! silently accepted.

use crate::syntax::{Lang, Node, Stream};
use std::ops::Range;

/// A task's state. Three, exactly as the standard defines.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// `- [ ]`
    Open,
    /// `- [-]` — declined. **Not** in progress.
    Declined,
    /// `- [x]`
    Done,
}

impl State {
    /// In the order the standard lists them.
    pub const ALL: [State; 3] = [State::Open, State::Declined, State::Done];

    /// The three bytes this state is written as.
    pub fn marker(self) -> &'static str {
        match self {
            State::Open => "[ ]",
            State::Declined => "[-]",
            State::Done => "[x]",
        }
    }

    /// The state a three-byte marker spells, or `None` when the standard does
    /// not define it.
    ///
    /// `[X]` is accepted as done: the grammar treats it as checked, so refusing
    /// it would report a file that renders correctly as broken.
    pub fn of_marker(marker: &str) -> Option<State> {
        match marker {
            "[ ]" => Some(State::Open),
            "[-]" => Some(State::Declined),
            "[x]" | "[X]" => Some(State::Done),
            _ => None,
        }
    }
}

/// One task line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub state: State,
    /// The marker's exact three bytes — the whole of a state change.
    pub marker: Range<usize>,
    /// The whole `list_item`, nested sub-tasks included.
    pub item: Range<usize>,
    /// The task's own text: from the marker's end to the line's.
    ///
    /// Kept as a range, not a string, because this module never rewrites it —
    /// the `~3d #tag @name` tail is where future metadata would go.
    pub text: Range<usize>,
    /// The nearest enclosing task, when this is a sub-task.
    pub parent: Option<usize>,
    /// 0 for a top-level task, 1 for a sub-task, and so on.
    pub depth: usize,
    /// 0-based line of the marker, for a diagnostic.
    pub line: usize,
    /// Character column of the marker within its line.
    pub col: usize,
    /// Innermost section containing this task, when there is one.
    pub heading: Option<usize>,
}

/// A subheader — *"Each subheader is a todo section."*
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Heading {
    /// `#` count: 1 for `#`, 2 for `##`.
    pub level: usize,
    /// 0-based line of the heading.
    pub line: usize,
    /// The heading and everything under it, nested sections included.
    pub section: Range<usize>,
    /// The heading's own text, markers removed.
    pub title: Range<usize>,
}

/// One splice. **The only way anything in this module changes a document.**
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edit {
    pub range: Range<usize>,
    pub replacement: String,
}

impl Edit {
    /// Apply to `src`, which must be the source these offsets came from.
    pub fn apply(&self, src: &str) -> String {
        apply(src, std::slice::from_ref(self))
    }
}

/// Apply several edits to one source.
///
/// Applied from the end backwards, so an earlier edit cannot move a later one's
/// offsets. Every range this module produces is a distinct three-byte marker, so
/// the edits do not overlap and the result is order-independent.
pub fn apply(src: &str, edits: &[Edit]) -> String {
    let mut sorted: Vec<&Edit> = edits.iter().collect();
    sorted.sort_by_key(|e| std::cmp::Reverse(e.range.start));
    let mut out = src.to_string();
    for e in sorted {
        out.replace_range(e.range.clone(), &e.replacement);
    }
    out
}

/// A parsed TODO.md.
#[derive(Debug, Clone)]
pub struct Doc {
    src: String,
    items: Vec<Item>,
    headings: Vec<Heading>,
    line_starts: Vec<usize>,
    /// Marker-shaped ranges that are NOT a state the standard defines.
    ///
    /// Kept separately from `items` because such a line is not a task: there is
    /// no marker node for it and no state to give it, so it cannot be an `Item`.
    /// Without this list the violation would be unrepresentable, which is the
    /// whole reason `check` exists.
    suspects: Vec<Range<usize>>,
}

impl Doc {
    /// Parse `src`.
    ///
    /// Never fails. A document that is not a TODO.md parses to a `Doc` with no
    /// items, and anything the standard does not define is reported by
    /// [`Doc::check`] rather than refused here — this is a view of a file, and
    /// the file is allowed to contain anything.
    pub fn parse(src: &str) -> Doc {
        let line_starts = line_starts(src);

        let mut stream = Stream::new(Lang::Markdown);
        stream.push(src);
        let root = stream.root();

        let mut items = Vec::new();
        let mut headings = Vec::new();
        let mut suspects = Vec::new();
        if let Some(root) = &root {
            // The walk is a pre-order traversal of an ordered tree, so `items`
            // comes out in document order and a parent's index is always lower
            // than its children's — which is what makes `parent` an index at
            // all, and why nothing here re-derives the relationship afterwards.
            let mut walk = Walk {
                src,
                items: Vec::new(),
                headings: Vec::new(),
                suspects: Vec::new(),
            };
            walk.visit(root, None, 0, None);
            items = walk.items;
            headings = walk.headings;
            suspects = walk.suspects;
        }

        // Innermost heading per item: the one with the smallest section that
        // still contains it.
        let sections: Vec<Range<usize>> = headings.iter().map(|h| h.section.clone()).collect();
        for it in items.iter_mut() {
            let mut best: Option<usize> = None;
            for (i, s) in sections.iter().enumerate() {
                if s.start <= it.item.start && it.item.end <= s.end {
                    let better = match best {
                        None => true,
                        Some(b) => (sections[b].end - sections[b].start) > (s.end - s.start),
                    };
                    if better {
                        best = Some(i);
                    }
                }
            }
            it.heading = best;
        }

        let mut doc = Doc {
            src: src.to_string(),
            items,
            headings,
            line_starts,
            suspects,
        };
        doc.stamp_positions();
        doc
    }

    /// The source this was parsed from.
    pub fn src(&self) -> &str {
        &self.src
    }

    pub fn items(&self) -> &[Item] {
        &self.items
    }

    pub fn headings(&self) -> &[Heading] {
        &self.headings
    }

    /// Tasks belonging to `heading` — every task in its section, nested
    /// sub-tasks included, excluding tasks that belong to a nested section.
    pub fn items_in(&self, heading: usize) -> Vec<usize> {
        self.items
            .iter()
            .enumerate()
            .filter(|(_, it)| it.heading == Some(heading))
            .map(|(i, _)| i)
            .collect()
    }

    /// Done and total under `heading`.
    pub fn progress(&self, heading: usize) -> (usize, usize) {
        let idx = self.items_in(heading);
        let done = idx
            .iter()
            .filter(|&&i| self.items[i].state == State::Done)
            .count();
        (done, idx.len())
    }

    /// **Down.** Set `item` and every task inside it to `state`, then bring the
    /// ancestors into line.
    ///
    /// `None` cascades over the whole document. Returns one edit per marker
    /// whose bytes actually change: **a marker already at `state` yields no
    /// edit at all**, which is what keeps a toggle from dirtying the whole path
    /// to the root and makes a second cascade a no-op.
    pub fn set_subtree(&self, item: Option<usize>, state: State) -> Vec<Edit> {
        let span = match item {
            Some(i) => match self.items.get(i) {
                Some(it) => it.item.clone(),
                None => return Vec::new(),
            },
            None => 0..self.src.len(),
        };

        let mut edits = Vec::new();
        for it in &self.items {
            if it.item.start >= span.start && it.item.end <= span.end {
                self.push_state(&mut edits, it, state);
            }
        }
        edits.extend(self.derive_up());
        dedup(&mut edits);
        edits
    }

    /// **Up, written to the file.** Every task that has sub-tasks has its own
    /// marker derived from them:
    ///
    /// - every sub-task done → the parent is done
    /// - every sub-task declined → the parent is declined
    /// - otherwise → the parent is open
    ///
    /// A leaf task's marker is the operator's and is never touched. The result
    /// uses only the standard's three markers, so a plain markdown reader — a
    /// `git diff`, GitHub, another agent's `read` — sees the roll-up.
    ///
    /// The mixed case resolving to *open* is deliberate: a parent with one done
    /// child and one open child is open, and the alternative — leaving the
    /// parent `[x]` above an unchecked child — is the file disagreeing with
    /// itself. Declining a parent is therefore done as a [`Self::set_subtree`]
    /// cascade, so its children move with it and this rule does not fight it.
    pub fn derive_up(&self) -> Vec<Edit> {
        let mut edits = Vec::new();
        for (i, it) in self.items.iter().enumerate() {
            let mut kids = self.items.iter().filter(|k| k.parent == Some(i)).peekable();
            if kids.peek().is_none() {
                continue;
            }
            let kids: Vec<&Item> = kids.collect();
            let want = if kids.iter().all(|k| k.state == State::Done) {
                State::Done
            } else if kids.iter().all(|k| k.state == State::Declined) {
                State::Declined
            } else {
                State::Open
            };
            self.push_state(&mut edits, it, want);
        }
        edits
    }

    /// The subtree a task owns, for a key binding.
    pub fn subtree(&self, item: usize) -> Option<Range<usize>> {
        self.items.get(item).map(|it| it.item.clone())
    }

    /// What the standard cannot express, in the tuple shape the editor's
    /// `syntax_errors` already returns, so the existing gutter renders it.
    ///
    /// One violation: a bracketed three-byte marker that is neither `[ ]`,
    /// `[-]` nor `[x]` — an unknown state, such as Obsidian's `[/]`. The
    /// grammar cannot report it (markdown has no error here) and no query can
    /// capture it (there is no node), so this is the only place it can be named.
    pub fn check(&self) -> Vec<(usize, usize, usize, String)> {
        let at = |off: usize| -> (usize, usize) {
            let line = self
                .line_starts
                .partition_point(|o| *o <= off)
                .saturating_sub(1);
            let start = self.line_starts.get(line).copied().unwrap_or(0);
            (line, self.src[start..off].chars().count())
        };
        let mut out = Vec::new();
        for r in &self.suspects {
            let Some(candidate) = self.src.get(r.clone()) else {
                continue;
            };
            let (line, col) = at(r.start);
            out.push((
                line,
                col,
                col + candidate.chars().count(),
                format!(
                    "unknown task marker {candidate:?}: TODO.md defines \"[ ]\", \"[-]\" and \"[x]\""
                ),
            ));
        }
        out.sort();
        out
    }

    fn push_state(&self, edits: &mut Vec<Edit>, it: &Item, state: State) {
        if it.state == state {
            return;
        }
        edits.push(Edit {
            range: it.marker.clone(),
            replacement: state.marker().to_string(),
        });
    }

    fn stamp_positions(&mut self) {
        let src = &self.src;
        let starts = &self.line_starts;
        let at = |off: usize| -> (usize, usize) {
            let line = starts.partition_point(|o| *o <= off).saturating_sub(1);
            let start = starts.get(line).copied().unwrap_or(0);
            (line, src[start..off].chars().count())
        };
        for it in self.items.iter_mut() {
            let (l, c) = at(it.marker.start);
            it.line = l;
            it.col = c;
        }
        for h in self.headings.iter_mut() {
            h.line = at(h.section.start).0;
        }
    }
}

/// Line start byte offsets. One entry for an empty document.
fn line_starts(src: &str) -> Vec<usize> {
    let mut v = vec![0usize];
    for (i, b) in src.bytes().enumerate() {
        if b == b'\n' {
            v.push(i + 1);
        }
    }
    v
}

fn child_starting_with<'a>(n: &'a Node, prefix: &str) -> Option<&'a Node> {
    n.children.iter().find(|c| c.kind.starts_with(prefix))
}

/// Walk the tree, collecting sections and tasks.
///
/// A struct rather than a pile of `&mut` parameters, so the walk's own state has
/// a name and adding a collection later does not change every signature.
struct Walk<'a> {
    src: &'a str,
    items: Vec<Item>,
    headings: Vec<Heading>,
    suspects: Vec<Range<usize>>,
}

impl Walk<'_> {
    fn visit(&mut self, n: &Node, parent: Option<usize>, depth: usize, heading: Option<usize>) {
        // A `section` carries a heading. Its range is the heading and everything
        // under it, nested sections included — so a section IS a subtree.
        let mut here = heading;
        if n.kind == "section"
            && let Some(h) = n.children.iter().find(|c| c.kind == "atx_heading")
        {
            let level = (1..=6)
                .find(|l| child_starting_with(h, &format!("atx_h{l}_marker")).is_some())
                .unwrap_or(1);
            let title = h
                .children
                .iter()
                .find(|c| c.kind == "inline")
                .map(|c| c.start..c.end)
                .unwrap_or(h.end..h.end);
            self.headings.push(Heading {
                level,
                line: 0,
                section: n.start..n.end,
                title,
            });
            here = Some(self.headings.len() - 1);
        }

        let mut this_parent = parent;
        let mut this_depth = depth;
        if n.kind == "list_item" {
            match item_of(n, self.src) {
                Some(item) => {
                    let idx = self.items.len();
                    self.items.push(Item {
                        parent,
                        depth,
                        ..item
                    });
                    this_parent = Some(idx);
                    this_depth = depth + 1;
                }
                // Not a task. If it nevertheless LOOKS like one — a bracketed
                // three-byte marker the standard does not define — record it:
                // the grammar will not, and there is no `Item` to carry it.
                None => {
                    if let Some(r) = suspect_marker(n, self.src) {
                        self.suspects.push(r);
                    }
                }
            }
        }

        for c in &n.children {
            self.visit(c, this_parent, this_depth, here);
        }
    }
}

/// A bracketed three-byte range after a list marker that is NOT a defined
/// state, if the line has one.
///
/// This is the validated read that names a violation: the item's own list
/// marker ends exactly where a task marker would begin, so the offset comes from
/// the grammar even though the contents do not.
fn suspect_marker(n: &Node, src: &str) -> Option<Range<usize>> {
    let bullet = child_starting_with(n, "list_marker")?;
    let at = bullet.end;
    let candidate = src.get(at..at + 3)?;
    let b: Vec<char> = candidate.chars().collect();
    if b.len() == 3 && b[0] == '[' && b[2] == ']' && State::of_marker(candidate).is_none() {
        Some(at..at + 3)
    } else {
        None
    }
}

/// Build an [`Item`] from a `list_item`, or `None` when the line is not a task.
///
/// The marker is found two ways, and they are complementary:
///
/// - the grammar's nodes, for the two states it has;
/// - a **validated read** for `[-]`, which has no node — the item's own list
///   marker ends exactly where a task marker would begin.
///
/// A list item with no marker is not a task. (The other TODO.md standard makes
/// checkboxes optional; this one does not, and a bare bullet is prose.)
fn item_of(n: &Node, src: &str) -> Option<Item> {
    let bullet = child_starting_with(n, "list_marker")?;
    let (state, marker) = if let Some(c) = n
        .children
        .iter()
        .find(|c| c.kind == "task_list_marker_checked")
    {
        (State::Done, c.start..c.end)
    } else if let Some(c) = n
        .children
        .iter()
        .find(|c| c.kind == "task_list_marker_unchecked")
    {
        (State::Open, c.start..c.end)
    } else {
        let at = bullet.end;
        let candidate = src.get(at..at + 3)?;
        (State::of_marker(candidate)?, at..at + 3)
    };

    Some(Item {
        state,
        marker: marker.clone(),
        item: n.start..n.end,
        text: marker.end..n.end,
        parent: None,
        depth: 0,
        line: 0,
        col: 0,
        heading: None,
    })
}

/// Drop edits naming a range more than once, keeping the last.
///
/// [`Doc::set_subtree`] and [`Doc::derive_up`] can both name a marker: a parent
/// inside the cascaded subtree whose children also changed. The derived value
/// wins, because it was computed from the states the cascade produced.
fn dedup(edits: &mut Vec<Edit>) {
    let mut seen: Vec<Range<usize>> = Vec::new();
    let mut out: Vec<Edit> = Vec::new();
    for e in edits.drain(..).rev() {
        if seen.contains(&e.range) {
            continue;
        }
        seen.push(e.range.clone());
        out.push(e);
    }
    out.reverse();
    *edits = out;
}
