# S9 — A todo schema over markdown: hierarchy and cascade

Status: design, for ruling. Schema not implemented; the grammar facts below are
pinned by tests that run (`tests/markdown_grammar.rs`, 10 tests).
Written 2026-09-29. Scope narrowed by the operator: *"when i say orgmodish - i
mainly think of hierarchy and auto done for the whole subtree."*

The one boundary this design does NOT close is authorship against the wire's
`TodoEntry` — see Decision 3, which is stated as unclean on purpose.

## Scope, and what that rules out

**In:** hierarchy (headings and nested bullets), and marking a subtree done in
one act.

**Out, deliberately, and not as options to be picked up later:** priorities
(`[#A]`), scheduling and deadlines (`SCHEDULED:`), tags (`:work:`), TODO/DONE
keywords on headings, agenda, clocking. The operator's narrowing removed them
from the ask, and each is its own namespace with its own precedence questions.
Recorded here only so nobody reads their absence as an oversight.

## The grammar: no new dependency, and none should be added

`tree-sitter-md` is already in `Cargo.toml` and already parses every markdown
file rano opens. **`tree-sitter-org` must not be taken**: it would be a second
grammar and a second file format for one job, and the file the operator actually
wants edited is the repo's `TODO.md`, which is markdown.

What the grammar gives, read from its node types and then pinned by test:

- **Checkbox state is a node type**, not a pattern: `task_list_marker_checked`
  and `task_list_marker_unchecked`. No regex, no line scanning.
- **Hierarchy is native, in two shapes that nest into each other.** `section`
  contains `section` (heading depth as a real tree, not a count of `#`);
  `list_item` contains `list` (nested bullets as a real tree). So "the subtree"
  is one node's range, whichever shape the checklist is written in.
- **`[X]` is checked** — the capital X is not a different state.
- **All four bullet styles hold a marker**: `list_marker_minus`, `_star`,
  `_plus`, `_dot`.

So the cascade is range surgery over query results: find each
`task_list_marker_*` inside the subtree's range, rewrite its own three bytes.

**Byte-identical by construction**, and that is not a claim about care — it is
that every edit lands inside a range the tree handed us, every state is exactly
three bytes, and no code writes a document. leticl had to hand-build source-line
tracking to *approximate* this.

There is no renderer here and no inverse function: prose, indentation, wrapping
and trailing whitespace are not represented, so they cannot be lost. The file is
the source of truth; the wire is a projection.

### The finding: a query cannot express an unknown marker

`- [-]`, `- [~]` and `- []` parse as **ordinary list items** — no ERROR node, no
missing node, no capture, `has_error == false`. A typo'd marker renders as a
plain row and nothing anywhere reports it.

So the schema needs `check()` with a diagnostic, and this is the one place it
reads bytes at a grammar-anchored offset instead of a captured range: a marker
would have begun exactly at the item's `paragraph.start`, which is where to look.
Reported as a finding rather than routed around with a hand-rolled line scan.

(Org's third state `[-]` is not expressible as a query for the same reason. It
is kept in the schema for wire parity — see below — but **it is not part of this
feature**: a cascade needs only done and not-done.)

## Decision 1 — down is a command, up is a derived fact, never written

Agreed with the ruling, and the reasoning holds: storing `up` lets the file
disagree with itself (a parent `[x]` above an unchecked child) and re-marks a
line the operator just unmarked.

**What I measured that sharpens it.** The open question was the cost of
computing `up`. It cannot be per frame:

| document | parse + query |
|---|---|
| 200 items, 11 KB | **2.13 ms** |
| 1 000 items, 56 KB | 9.10 ms |
| 4 000 items, 229 KB | 35.17 ms |

Against rano's measured frame cost of **139 µs**, the smallest of those is 15× a
whole frame, and it grows linearly with the document — which is the §16.0 sin
this project has spent two phases removing.

**So: computed per edit, cached, and read by the renderer. Never per frame.**
A toggle is one keystroke; 2.13 ms there is imperceptible, and it is the same
order as the 2.5 ms keystroke already measured on a 2 MB file. The 4 000-item
case (35 ms per edit) is a real cost and is named rather than hidden; bounding
it means recomputing only the ancestor chain of the edited line, which needs the
previous parse for ancestry and is deferred until something is slow.

### Decision 1a — and this is the part that is easy to get wrong

**Derived progress must not be rendered in the checkbox.** If `up` is displayed
where the stored marker lives, then a parent whose children are all done reads
`[x]` on screen while the file says `[ ]` — and the operator has the same
"editor fighting me" symptom the ruling exists to prevent, moved from storage
into rendering. Worse, it is invisible: the file and the screen disagree and
only one of them is real.

Org's own answer is two affordances: the checkbox is the stored state, the
cookie (`[2/3]`, `[/]`) is the derived progress, and they are in different
places.

Cheapest good place for ours: **the line-number gutter**, which already renders
one fact per row (diagnostic severity, by colour) and costs no document bytes
at all. A subtree's progress marker on the heading's row is the same shape as
what the gutter already does. Cost to name: the cached progress has to reach
`ui.rs`, which means it lives in `BufferState` or is passed into `draw`.

If that is not wanted, the fallback is simpler and costs nothing: **do not
render `up` at all** and compute nothing per frame. `down` alone is the whole
of "auto done for the whole subtree"; `up` is a convenience on top, and it is
the half with the whole cost.

## Decision 2 — the set is closed; an unknown member is refused by name

`{ Pending, InProgress, Completed }`. An unrecognised marker character produces
a diagnostic naming the character and offset — never coerced to Pending, which
is precisely what the grammar already does and is the bug.

## Decision 3 — the boundary with harnessd's `TodoEntry`: UNCLEAN, and it does not close

The wire carries a THIRD axis this schema cannot model: **authorship**. Read at
`letibot/wt-resume-chain/crates/sessionlog/src/event.rs:99` and `:105-114`:

```rust
pub enum TodoBy { #[default] Model, Operator }

pub struct TodoEntry {
    pub content: String,
    pub status: TodoStatus,
    #[serde(default)]
    pub by: TodoBy,
}
```

The storage twin is `letibot_tokencore::store::TodoItem::by`, the same two
variants with the same default, and the reason is in its own comment: the two
authors *"share one list and a format, and this field is the whole of the
difference between them."*

So:

- **Status: MATCH, and that part is clean.** My three names are the wire's three
  names with the same meaning. This remains the one reason the third state stays
  in the schema even though a cascade only needs two.
- **Authorship: ABSENT, and it cannot be added.** A `[ ]` line in a markdown
  file says nothing about who wrote it. There is no org convention for it and no
  place in a checkbox to put it. Consequences, stated rather than discovered
  later:
  - **markdown → wire:** every row's `by` must be assigned by the sync layer as
    a *policy* ("the file is the operator's"), not read as a fact. The file does
    not hold that fact.
  - **wire → markdown:** `by` is lost. If the model's rows and the operator's
    rows both live in `TODO.md`, they become one indistinguishable list — and
    `set_operator_todos`'s "replace the operator's half and leave the model's
    alone" has nothing to key on.
- **Two variants are already insufficient for the operator's own design**, before
  this schema adds anything: a third writer — their `$EDITOR`, a `git pull`,
  another agent — is neither `Model` nor `Operator`. Not mine to solve; recorded
  so whoever builds the sync knows which axis they are missing.

**The boundary, honestly stated: this schema is a VIEW of a file, and it carries
what the file carries.** The wire needs one thing the file does not have, so the
sync is lossy in one direction and invented in the other, and the design that
reconciles that is upstream of this module. Stating it as "clean" would have
hidden exactly the work somebody else has to do.

### The failed search, filed

My previous message claimed the opposite — *"there is no `by` field; `TodoEntry`
is `{content, status}`"* — and it was wrong. Filing it, because an absence claim
is worth exactly the search that failed and nothing more:

- **Search:** `grep -rn "TodoEntry" ~/Projects --include=*.rs -l | head -10`, then
  `sed -n '75,115p'` on ONE hit,
  `letibot-profiles/crates/sessionlog/src/event.rs` — and the claim was made from
  that one file.
- **What it found:** in *that* worktree `TodoEntry` really is `{content, status}`.
  The claim was true of the file I read.
- **Why it failed — the scope, not the pattern:** `letibot` is a multi-worktree
  repo (`letibot-profiles`, `wt-todo`, `wt-resume-chain`, `wt-edit`, `wt-cat`)
  whose worktrees sit on different branches, and the field exists on exactly one
  of them. Counted: `TodoBy` occurs 3 times in `wt-resume-chain` and **0 times in
  each of the other four**. I read one worktree's copy and generalised to "the
  wire".
- **A refinement worth keeping, because it changes what to guard against.** The
  first diagnosis I was given was that the search missed the field because `by`
  is typed `TodoBy` rather than `String`, so a pattern looking for `by: String`
  could not match it. That is not what happened: my pattern was `pub by`, which
  *does* match `pub by: TodoBy`, and re-running it in `letibot-profiles` still
  returns only the three unrelated `by: String` fields — because in that file
  they are all there is. **The load-bearing scope was the branch, and a worktree
  looks exactly like the repo.** That is harder to notice than a grep pattern: a
  pattern is visible in the command you wrote, and a checkout is not visible at
  all.

## The API, and where it must live

New module `src/todo.rs`, exported from `lib.rs`.

**This placement is a hard requirement, not a preference.** `lib.rs` exports
`buffer encoding rows syntax width`; `editor`, `ui`, `keys` and `bindings` are
binary-only. A schema behind `main.rs` is unreachable from leticl, and the reason
for the work evaporates. So: no `Buffer`, no `Editor`, no crossterm, no `Frame`.
`&str` in, ranges and edits out.

```rust
pub enum State { Pending, InProgress, Completed }

pub struct Item {
    pub state: State,
    pub marker: Range<usize>,      // the exact 3 bytes
    pub item: Range<usize>,        // the whole list_item
    pub heading: Option<usize>,    // index into headings
    pub line: usize, pub col: usize,
}

pub struct Heading { pub level: usize, pub line: usize,
                     pub section: Range<usize> }   // the whole subtree

pub struct Doc { /* src, items, headings, cached progress */ }

/// One splice. The ONLY way anything here changes a document.
pub struct Edit { pub range: Range<usize>, pub replacement: &'static str }
impl Edit { pub fn apply(&self, src: &str) -> String; }

impl Doc {
    pub fn parse(src: &str) -> Doc;

    /// DOWN: mark a subtree done. One Edit per marker that CHANGES — a
    /// cascade over an already-done subtree returns an empty vec, which is
    /// what makes it idempotent and keeps the undo stack honest.
    pub fn set_subtree_done(&self, span: Range<usize>, s: State) -> Vec<Edit>;

    /// UP, as a fact: done/total under this node. Never written.
    pub fn progress(&self, span: Range<usize>) -> (usize, usize);

    /// The subtree of the item or heading at `line`, for a key binding.
    pub fn subtree_at(&self, line: usize) -> Option<Range<usize>>;

    /// Schema violations, in the shape `syntax_errors` already returns, so the
    /// existing gutter renders them: (line, col, end_col, message).
    pub fn check(&self) -> Vec<(usize, usize, usize, String)>;
}
```

## The tests, written before the implementation

The hard requirement was that the first test not be "the toggle worked" but "the
file after equals the file before except for the bytes I meant to change" — run
twice. Those exist and pass, against the real grammar:

- `a_cascade_changes_only_marker_bytes` — asserts the containment the cascade
  rides on (nested items inside the parent's range, the next section's outside),
  then checks **every differing byte index is inside a marker range** and every
  other byte is identical.
- `a_cascade_is_idempotent` — a second cascade over the result produces **no
  edits at all**.
- `a_section_is_a_cascade_target_too` — the same over a heading's subtree.
- `the_marker_is_exactly_three_bytes_at_a_grammar_given_offset`,
  `every_bullet_style_can_hold_a_marker`, `a_capital_x_is_checked`,
  `a_section_is_the_whole_subtree`, `a_heading_carries_its_level_marker`,
  `an_unknown_marker_is_silent_in_the_grammar`.

Writing these first paid twice, and both times the test was right and I was
wrong: the cascade helper returned every marker in the span rather than the ones
it would rewrite, so a second run emitted three no-op edits — **the edit set is
part of the contract, not an implementation detail**. That is now asserted.

Still to write when the module exists: `prose_indentation_and_trailing_space_survive_untouched`,
over a fixture with two spaces after a period, a `Deps:` block, hard-wrapped
lines and trailing whitespace — leticl's actual complaint, asserted rather than
assumed.

## What is deliberately absent

- **Authorship.** The schema carries no `by` and cannot: a markdown file does not
  record who wrote a line. This is a property of the medium rather than a choice
  to defer, and Decision 3 states what it costs the sync in both directions.
- No renderer, no inverse function. That is the design, not a gap.
- No second parser for markdown; every range is from a query or a validated read
  at a grammar-anchored offset.
- No editor integration yet. `ui.rs`/`editor.rs` know nothing about todos; the
  schema exists and is reachable from the library first, and leticl is the first
  consumer.
- `up` rendering is proposed (gutter) but not required; `down` stands alone.
