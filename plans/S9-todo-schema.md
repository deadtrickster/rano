# S9 — A todo schema over markdown: hierarchy, cascade, and a written `up`

Status: design, for ruling. Schema not implemented; the grammar facts are pinned
by tests that run (`tests/markdown_grammar.rs`, 11 tests).
Written 2026-09-29. Scope narrowed by the operator: *"when i say orgmodish - i
mainly think of hierarchy and auto done for the whole subtree."*

`up` is IN and it is WRITTEN to the file. The file is the transport: harnessd's
state follows the file, so a fact that lives only in a renderer never crosses the
wire. The rule is not "never write" but **"never leave it wrong"**.

The boundary against the wire's `TodoEntry` does not close (authorship) — see
Decision 3, stated as unclean on purpose.

## Scope

**In:** hierarchy (headings and nested bullets); marking a subtree done in one
act; and a derived progress value written into the file.

**Out, by the operator's narrowing:** priorities (`[#A]`), scheduling and
deadlines, tags, TODO/DONE keywords on headings, agenda, clocking. Recorded only
so nobody reads their absence as an oversight.

**Out by decision, this turn:** any staleness apparatus. A stale cookie is *out
of date, not invalid* — org does not defend against a hand-edited file either,
and neither does any text format. `check()` is for what the format **cannot
express**, not for what somebody typed wrong.

## The grammar: no new dependency

`tree-sitter-md` is already in `Cargo.toml`. **`tree-sitter-org` must not be
taken** — a second grammar and a second file format for a job whose subject is a
markdown `TODO.md`. Pinned by test:

- **Checkbox state is a node type**: `task_list_marker_checked`,
  `task_list_marker_unchecked`. No regex, no line scanning.
- **Hierarchy is native in two shapes that nest into each other.** `section`
  contains `section`; `list_item` contains `list`. A subtree is one node range in
  either shape.
- **`[X]` is checked**; all four bullet styles hold a marker.

The marker is exactly three bytes at a grammar-given offset. That is the toggle,
and it is why a cascade is byte-identical **by construction** rather than by
care: every edit lands inside a range the tree handed us, and no code writes a
document. leticl had to hand-build source-line tracking to approximate this.

### The finding: a query cannot express a marker the grammar does not know

`- [-]`, `- [~]`, `- []` parse as **ordinary list items** — no ERROR node, no
missing node, no capture, `has_error == false`. A typo renders as a plain row and
nothing reports it. So `check()` is required, and it is the one place the schema
reads bytes at a grammar-anchored offset (a marker would begin exactly at the
item's `paragraph.start`).

With `up` written this is structural rather than a gap, and for a precise reason:
the schema now has states the grammar cannot name. A **container's** partiality
is the cookie's job (`[2/3]`); an **item's** in-progress state is `[-]`'s, and it
is what maps onto the wire's `InProgress`. Those are two levels, not one, and
`[-]` is needed for the item level.

## Decision 1 — `up` is written, as a COOKIE on the heading, and never auto-`[x]`

This reverses the earlier "never written" recommendation. The operator's reason
stands: the file is how anything downstream finds out.

**What the file actually looks like, measured.** `rano/TODO.md`: **49 headings,
67 checkbox items, items flat directly under headings** — and a heading carries
no marker of its own:

```
## 1. Crash bugs
- [x] Prompt input panics on multibyte text: ...
- [x] `move_left` at BOL sets cursor past EOL: ...
```

So "auto-`[x]` on the parent" has no parent checkbox to write. On this shape it
would mean inventing a checkbox on the heading line, and **GitHub renders
task-list checkboxes only on list items, never on a heading** — so `## X [x]`
shows literal text, the same visual class as `[3/3]`, while being a poorer
answer: it cannot say *two of three*, and a derived `[x]` on a parent is a state
that looks toggleable and is not (unchecking it is re-derived on the next child
edit).

**Chosen: the cookie, org's form, `[done/total]`, at the end of the heading.**

1. **It never lies about a partial subtree.** `[2/3]` is honest; a derived `[x]`
   on a partly-done parent is either wrong or `[ ]`, and `[ ]` hides progress.
2. **It presents no false affordance** — nobody tries to click `[2/3]`.
3. **It maps onto the wire's three states exactly**: `[0/n]` → `Pending`,
   partial → `InProgress`, `[n/n]` → `Completed`. That is the operator's actual
   requirement — the file carries the fact harnessd needs, in a form that
   projects onto `TodoStatus`.
4. **It costs the renderer nothing.** The cookie is text on the heading row and
   rano already draws that line; there is **no gutter work**. (This corrects the
   direction I was given: `up` being in moves the estimate, but *down* in
   `todo.rs` — not up in `ui.rs`.)

**What the option I dropped would have bought**, honestly: for a checklist
written as **nested bullets** rather than under headings, auto-`[x]` on the
parent genuinely wins — a `- [x] parent` renders as a ticked box in GitHub and
most editors, where `[2/3]` is only text, and the parent marker already exists so
nothing has to be invented. That is a real case and a detectable one (which
container the item sits in). It is not *this* file, so the cookie is the design's
answer and the nested-bullet case is recorded as where the other one wins.

### The insert rule: update-if-present, NEVER insert

A cookie is maintained on a heading that already has one, and is never added to a
heading that does not.

The number that decides it: `rano/TODO.md` has **49 headings and no cookies**. If
a toggle inserted a cookie, the first `[x]` on a shared, committed file would
rewrite **49 heading lines** — the exact diff-nobody-asked-for that leticl
stopped before building to avoid. With update-if-present, a first toggle on an
uncookied file produces **exactly one edit**, identical to the pre-`up`
behaviour, and adopting the feature is a deliberate act: write `[2/3]` once and
it stays right from then on.

This is org's posture too — the file declares what it wants maintained.

## Decision 1a — nobody double-renders the cookie

`leticl`'s pane **already computes and displays this value**, at
`src/panes.lisp:922-927`: `:mark (%todo-rollup marks)` with
`:text "~a  [~a/~a]"`. And `%todo-rollup` (`:876-881`) is org's rule verbatim,
in their own words — *"every child done makes the parent done; any child started
makes it started; otherwise open"* — over their three marks `:open`/`:doing`/
`:done`, which are the wire's three states.

Two consequences, and the second is a bug report for the letibot side:

- Writing the cookie makes the pane's render-time rollup **persistable** rather
  than a coincidence: file and pane agree by construction.
- But if the pane keeps *generating* the cookie text while the file also
  contains one, it renders **`Section [2/3]  [2/3]`**. leticl must read the
  cookie out of the heading text instead of appending its own. Flagged here
  because a divergence between two implementations of one convention is exactly
  what both ends are being asked to catch early.

## The byte-identity assertion, restated

The hard requirement was *"the file after equals the file before except the bytes
I meant to change."* With `up` written, a child toggle **legitimately rewrites its
ancestor chain**, so the assertion becomes:

> **only the bytes the rule required changed, and nothing else in the file moved.**

The intended set is now `{the toggled marker} ∪ {ancestor cookies whose text
changes}`, and both halves are named rather than counted.

**The no-op rule becomes more load-bearing, not less.** An ancestor whose cookie
is already correct must produce **no edit at all**, or every child toggle dirties
the whole path to the root, the undo stack fills with steps that change nothing,
and a byte-identical save rewrites the file. Asserted, not assumed.

## The API

New module `src/todo.rs`, exported from `lib.rs`.

**Placement is a hard requirement.** `lib.rs` exports `buffer encoding rows
syntax width`; `editor`, `ui`, `keys`, `bindings` are binary-only, so a schema
behind `main.rs` is unreachable from leticl and the reason for the work
evaporates. No `Buffer`, no `Editor`, no crossterm, no `Frame`: `&str` in,
ranges and edits out.

```rust
pub enum State { Pending, InProgress, Completed }

pub struct Item {
    pub state: State,
    pub marker: Range<usize>,      // the exact 3 bytes
    pub item: Range<usize>,        // the whole list_item
    pub line: usize, pub col: usize,
}

/// A statistics cookie on a heading: `[2/3]`. Only ever maintained, never added.
pub struct Cookie { pub span: Range<usize>, pub done: usize, pub total: usize }

pub struct Heading {
    pub level: usize,
    pub line: usize,
    pub section: Range<usize>,     // the whole subtree, nested sections included
    pub cookie: Option<Cookie>,    // as found in the file, if the file has one
}

/// One splice. The ONLY way anything here changes a document.
pub struct Edit { pub range: Range<usize>, pub replacement: String }

pub struct Doc { /* src, items, headings, cached progress */ }

impl Doc {
    pub fn parse(src: &str) -> Doc;

    /// DOWN, plus the ancestor cookies it implies. Every edit in the returned
    /// vec changes bytes: a marker already at the target state, and an ancestor
    /// cookie already correct, produce NOTHING.
    pub fn set_subtree_done(&self, span: Range<usize>, s: State) -> Vec<Edit>;

    /// UP as a fact: done/total under this node.
    pub fn progress(&self, span: Range<usize>) -> (usize, usize);

    /// The subtree of the item or heading at `line`, for a key binding.
    pub fn subtree_at(&self, line: usize) -> Option<Range<usize>>;

    /// What the format cannot express: an unknown marker character, a checkbox
    /// with no list, the `[-]` the grammar cannot name. Same tuple shape
    /// `syntax_errors` already returns, so the existing gutter renders it.
    /// NOT for a cookie that disagrees with its children — that is arithmetic
    /// that is behind, and doing the arithmetic fixes it.
    pub fn check(&self) -> Vec<(usize, usize, usize, String)>;
}
```

## The tests

Already written and passing against the real grammar (the first three):

- `a_cascade_changes_only_marker_bytes` — asserts the containment the cascade
  rides on, then that **every differing byte index is inside a marker range** and
  every other byte is identical.
- `a_cascade_is_idempotent` — a second cascade over the result yields **no
  edits at all**.
- `a_section_is_a_cascade_target_too`.
- `the_marker_is_exactly_three_bytes_at_a_grammar_given_offset`,
  `every_bullet_style_can_hold_a_marker`, `a_capital_x_is_checked`,
  `a_section_is_the_whole_subtree`, `a_heading_carries_its_level_marker`,
  `an_unknown_marker_is_silent_in_the_grammar`,
  `a_cookie_sits_inside_the_heading_range` — the assumption Decision 1 rests on:
  `[2/3]` is inside the heading's own range (so it is findable by looking at the
  heading, not by scanning lines) and the grammar gives it no node, which is why
  reading it is a validated read at a grammar-anchored offset.

Required before the module is done:

- `a_toggle_on_an_uncookied_file_is_one_edit` — the 49-heading property: no
  cookie is ever inserted, so the first toggle edits one marker and nothing else.
- `a_toggle_updates_only_the_ancestor_cookies_that_change` — three levels, one
  number moves; the second ancestor's cookie is already right and yields no edit.
- `an_ancestor_cookie_already_correct_produces_no_edit` — the no-op rule, which
  is what keeps a toggle from dirtying the path to the root.
- `prose_indentation_and_trailing_space_survive_untouched` — over a fixture with
  two spaces after a period, a `Deps:` block, hard-wrapped lines and trailing
  whitespace. leticl's actual complaint, asserted rather than assumed.
- `a_fence_inside_a_blockquote_is_a_fence` exists already on the grammar side and
  guards the fixture that started this.

Writing these first has already paid twice, both times with the test right and me
wrong: the cascade helper returned every marker in the span rather than the ones
it would rewrite, so a second run emitted three no-op edits. **The edit set is
part of the contract.**

## Revised size

The old estimate was ~250 lines. **Revised: ~350-450 in `src/todo.rs`**, plus the
five tests above.

The growth is cookie support — parsing an existing cookie off a heading line,
computing done/total for its section, and assembling ancestor edits only where
the text changes — and it is all in the **schema**, not the renderer. The gutter
work I expected to be implied by `up` does **not** exist: the cookie is text on a
line rano already draws.

## What is deliberately absent

- **Authorship.** The schema carries no `by` and cannot: a markdown file does not
  record who wrote a line. Decision 3 states what that costs the sync.
- **Staleness apparatus.** Out of date is not invalid; the next touch fixes it.
- No renderer and no inverse function. That is the design, not a gap.
- No second parser; every range is a query result or a validated read at a
  grammar-anchored offset.
- No editor integration yet: the schema exists and is reachable from the library
  first, and leticl is the first consumer.
- Auto-`[x]` on a list-item parent. Recorded above as the better choice *only*
  where the checklist is nested bullets rather than under headings.

## Decision 3 — the boundary with the wire's `TodoEntry`: UNCLEAN

Read at `letibot/wt-resume-chain/crates/sessionlog/src/event.rs:99` and
`:105-114`, confirmed verbatim:

```rust
pub enum TodoBy { #[default] Model, Operator }

pub struct TodoEntry {
    pub content: String,
    pub status: TodoStatus,
    #[serde(default)]
    pub by: TodoBy,
}
```

The storage twin is `letibot_tokencore::store::TodoItem::by`, whose own comment
gives the reason: the two authors *"share one list and a format, and this field is
the whole of the difference between them."*

- **Status: MATCH.** My three names are the wire's three, same meaning — and the
  cookie projects onto them (`[0/n]`/partial/`[n/n]`).
- **Authorship: ABSENT and not addable.** A `[ ]` line records nothing about who
  wrote it. **markdown → wire** must assign `by` as a *policy*, not read it as a
  fact. **wire → markdown** loses it, and `set_operator_todos`'s "replace the
  operator's half and leave the model's alone" then has nothing to key on.
- **Two variants cannot express the third writer** the operator's own design now
  has: their `$EDITOR`, a `git pull`, another agent. Not mine to solve; recorded
  so the sync builder knows which axis is missing.

This schema is a **view** of a file and carries what the file carries. The wire
needs one thing the file does not have, and the design that reconciles that is
upstream of this module.

### The failed search, filed

An earlier message claimed the opposite — *"there is no `by` field"* — and was
wrong. Filed because an absence claim is worth exactly the search that failed:

- **Search:** `grep -rn "TodoEntry" ~/Projects --include=*.rs -l | head -10`, then
  `sed -n '75,115p'` on **one** hit,
  `letibot-profiles/crates/sessionlog/src/event.rs`, and the claim was made from
  that one file.
- **What it found:** in *that* worktree `TodoEntry` really is `{content, status}`.
  The claim was true of the file I read.
- **Why it failed — the scope, not the pattern:** `letibot` is a multi-worktree
  repo whose worktrees sit on different branches, and the field exists on exactly
  one. `TodoBy` occurs 3 times in `wt-resume-chain` and **0 times in each of the
  other four**. I read one worktree's copy and generalised to "the wire".
- **A refinement, because it changes what to guard against.** The proposed
  diagnosis was that a pattern looking for `by: String` could not match a field
  typed `TodoBy`. That is not what happened: my pattern was `pub by`, which *does*
  match `pub by: TodoBy`, and re-running it in `letibot-profiles` still returns
  only the three unrelated fields — because there they are all there is. **The
  load-bearing scope was the branch, and a worktree looks exactly like the
  repo.** Harder to notice than a pattern: a pattern is visible in the command
  you wrote, a checkout is not visible at all.
