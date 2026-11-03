//! TODO.md — the markdown todo standard, as a schema over byte ranges: `# TODO` first,
//! sections from subheaders, tasks `'- [ ] '` / `'- [-] '` / `'- [x] '`, and `[-]` means
//! DECLINED. Every operation yields an [`Edit`] splice of three bytes, never a rewrite.

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
    /// not define it. `[X]` is accepted as done: the grammar treats it as
    /// checked, so refusing it would report a file that renders correctly.
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
    /// The task's own text: from the marker's end to the end of its own line, not the item's end —
    /// a `list_item` covers its nested children, so a title taken from `n.end` would swallow every
    /// sub-task. Kept as a range: this module never rewrites it.
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

/// Apply several edits to one source, from the end backwards so an earlier
/// edit cannot move a later one's offsets. The ranges never overlap.
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
    /// Marker-shaped ranges that are NOT a state the standard defines. Kept
    /// separately from `items`: such a line has no marker node and no state, so
    /// without this list the violation would be unrepresentable.
    suspects: Vec<Range<usize>>,
}

impl Doc {
    /// Parse `src`. Never fails: a non-TODO.md parses to no items, and anything
    /// the standard does not define is reported by [`Doc::check`] rather than
    /// refused here.
    pub fn parse(src: &str) -> Doc {
        let line_starts = line_starts(src);

        let mut stream = Stream::new(Lang::Markdown);
        stream.push(src);
        let root = stream.root();

        let mut items = Vec::new();
        let mut headings = Vec::new();
        let mut suspects = Vec::new();
        if let Some(root) = &root {
            // Pre-order traversal: `items` comes out in document order and a
            // parent's index is always lower than its children's, which is what
            // makes `parent` an index at all.
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

    /// Tasks belonging to `heading` — every task in its section, nested sub-tasks included,
    /// excluding tasks that belong to a nested section.
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

    /// Down: set `item` and every task inside it to `state`, then bring the ancestors into line;
    /// `None` cascades over the whole document. One edit per marker whose bytes actually change,
    /// and the `up` pass reads the states the cascade PRODUCED, or a parent stays open after its
    /// last child is completed.
    pub fn set_subtree(&self, item: Option<usize>, state: State) -> Vec<Edit> {
        let span = match item {
            Some(i) => match self.items.get(i) {
                Some(it) => it.item.clone(),
                None => return Vec::new(),
            },
            None => 0..self.src.len(),
        };
        self.set_span(span, state)
    }

    /// Down over an arbitrary span, then the ancestors. The span is what makes a HEADING a
    /// target: a `section` range covers all the tasks under it, so the same operation that
    /// ticks a subtree ticks a whole section. The `up` pass keeps `[x]` off an open child.
    pub fn set_span(&self, span: Range<usize>, state: State) -> Vec<Edit> {
        // The states the document WOULD have, so the two passes agree about
        // what they are looking at.
        let mut effective: Vec<State> = self.items.iter().map(|it| it.state).collect();
        for (i, it) in self.items.iter().enumerate() {
            if it.item.start >= span.start && it.item.end <= span.end {
                effective[i] = state;
            }
        }

        let mut edits = Vec::new();
        for (i, it) in self.items.iter().enumerate() {
            self.push_state(&mut edits, it, effective[i]);
        }
        edits.extend(self.derive_from(&effective));
        dedup(&mut edits);
        edits
    }

    /// The section of the heading on `line`, as a span — or `None` when there is no heading there.
    pub fn section_at(&self, line: usize) -> Option<Range<usize>> {
        self.headings
            .iter()
            .find(|h| h.line == line)
            .map(|h| h.section.clone())
    }

    /// Up, written to the file. A parent's marker is derived from its children: all done → done,
    /// all declined → declined, otherwise → open. A leaf's marker is the operator's and is never
    /// touched, and mixed resolving to open is deliberate — `[x]` above an open child disagrees.
    pub fn derive_up(&self) -> Vec<Edit> {
        let parsed: Vec<State> = self.items.iter().map(|it| it.state).collect();
        self.derive_from(&parsed)
    }

    /// The `up` pass over a given set of states. A child scan per parent (O(tasks²)) — fine
    /// for a todo file, and kept obvious rather than indexed.
    fn derive_from(&self, states: &[State]) -> Vec<Edit> {
        let mut edits = Vec::new();
        for (i, it) in self.items.iter().enumerate() {
            let kids: Vec<usize> = (0..self.items.len())
                .filter(|&j| self.items[j].parent == Some(i))
                .collect();
            let Some(&first_kid) = kids.first() else {
                continue;
            };
            let first = states[first_kid];
            let uniform = kids.iter().all(|&j| states[j] == first);
            let want = match (uniform, first) {
                (true, State::Done) => State::Done,
                (true, State::Declined) => State::Declined,
                // Uniformly open, or mixed: open either way.
                _ => State::Open,
            };
            self.push_state(&mut edits, it, want);
        }
        edits
    }

    /// The subtree a task owns, for a key binding.
    pub fn subtree(&self, item: usize) -> Option<Range<usize>> {
        self.items.get(item).map(|it| it.item.clone())
    }

    /// What the standard cannot express, in the tuple shape the editor's `syntax_errors`
    /// returns. One violation: a bracketed three-byte marker that is none of `[ ]`, `[-]`,
    /// `[x]` — the grammar cannot report it and no query can capture it.
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

/// Line start byte offsets, one entry for an empty document.
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

/// Walk the tree, collecting sections and tasks — a struct so the walk's own state has a name.
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
                // Not a task, but if it LOOKS like one — a marker the standard
                // does not define — record it: the grammar will not.
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

/// A bracketed three-byte range after a list marker that is NOT a defined state. The item's
/// own list marker ends exactly where a task marker would begin, so the offset comes from the
/// grammar even though the contents do not.
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

/// Build an [`Item`] from a `list_item`, or `None` when the line is not a task. The marker
/// is found two ways: the grammar's nodes for the two states it has, and a validated read for
/// `[-]`, which has no node. A list item with no marker is not a task.
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

    let text_start = marker.end;
    // The task's own line: `n.end` includes nested children for a parent.
    let text_end = src[text_start.min(n.end)..n.end]
        .find('\n')
        .map(|o| text_start + o)
        .unwrap_or(n.end);

    Some(Item {
        state,
        marker: marker.clone(),
        item: n.start..n.end,
        text: text_start..text_end,
        parent: None,
        depth: 0,
        line: 0,
        col: 0,
        heading: None,
    })
}

/// Drop edits naming a range more than once, keeping the last (the derived value —
/// computed from the states the cascade produced — wins).
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
