# S9 — TODO.md in bare rano: built

Status: **implemented** — `src/todo.rs` (563 lines, 360 code), exported from
`lib.rs`, with `tests/todo_schema.rs` (13 tests) and `tests/markdown_grammar.rs`
(10 tests, the grammar facts). Written 2026-09-29.

Scope, per the operator: *"when i say orgmodish - i mainly think of hierarchy and
auto done for the whole subtree."* Hierarchy and cascade. No DAG — no ids, no
`dependsOn`; the operator is taking that himself.

## The spec, read rather than summarised

**They are two different specs, and I read both.**

- `todomd/todo.md` — the whole repo is two files. **Two states**: `- [ ]` and
  `- [x]`, and checkboxes are *optional*. Columns are sections; the completed
  column's name must contain `✓` or `[x]`. Metadata at the end of the title:
  `~3d #type @name yyyy-mm-dd`, and a ticket id like `[JIRA-345]`.
- `todo-md/todo-md` — **three states**, and this is the target:
  *"The tasks themself are one liners that start with either `'- [ ] '`,
  `'- [-] '` or `'- [x] '`."* Plus *"Each subheader is a todo section"*,
  `@USERNAME` assignment and `#TAG` tags, and a `# TODO` header.

**`[-]` means DECLINED.** The spec's four states are *"open / declined / done /
deleted (removed from the document)"* — the fourth is the absence of a line, not
a marker. It is **not** "in progress", which is the reading the third state
usually gets (Obsidian uses `[/]` for that, and `[-]` for cancelled).

That distinction is load-bearing for the wire mapping, below.

## The finding: the format has a state the grammar cannot name

`tree-sitter-md` has `task_list_marker_checked` and `task_list_marker_unchecked`
— **two**. The standard has **three**. So:

- `- [-]` parses as an ordinary list item whose paragraph happens to begin with
  brackets. No ERROR node, no missing node, no capture, `has_error == false`.
- No query can express it: there is no node to capture.
- So `check()` is required **by the format**, not merely by the cascade. This was
  found by trying the query, which is the only way it could have been found.

The schema reads the declined marker with a **validated read at a
grammar-anchored offset**: a list item's own `list_marker_*` node ends exactly
where a task marker would begin, so the offset comes from the tree even though
the contents do not. That is the one place the schema reads bytes the grammar
gave it no node for, and it is reported rather than routed around with a
hand-rolled line scan.

`Doc::check()` also names a marker the standard does not define at all — `[/]`,
`[~]` — reporting it in the tuple shape the editor's `syntax_errors` already
returns, so the existing gutter renders it.

## Decision: `up` is written, as an auto-`[x]` on the parent

The operator ruled `up` in, because **the file is the transport**: harnessd's
state follows the file, so a fact that lives only in a renderer never crosses the
wire. The rule is not "never write" but **"never leave it wrong"**.

Then the spec settled the *form*, and it is auto-`[x]`, not org's `[1/3]` cookie:
the standard defines three markers and no cookie, so a cookie would be an
extension beyond the spec, while a derived marker on a parent stays inside it and
is what a plain markdown reader — a `git diff`, GitHub, another agent's `read` —
already understands. The burden for the cookie moved, and it did not earn its
extension in this document's shape.

The rule, in the standard's own vocabulary:

- every sub-task done → the parent is done
- every sub-task declined → the parent is declined
- otherwise → the parent is open

A leaf task's marker is the operator's and is never touched. The mixed case
resolving to *open* is deliberate: leaving a parent `[x]` above an unchecked
child is the file disagreeing with itself. Declining a parent is done as a
cascade, so its children move with it and the rule does not fight it.

## The byte-identity property, restated for ancestors

The original requirement was *"the file after equals the file before except the
bytes I meant to change"*. With `up` written, a child toggle legitimately
rewrites its **ancestor chain**, so the intended set is
`{toggled markers} ∪ {ancestor markers whose text changes}`.

Both halves are asserted. The rule that makes it safe is that **a marker already
at the target state yields no edit at all** — asserted by `a_cascade_is_idempotent`
(a second cascade produces an empty edit list) and by
`an_ancestor_already_correct_produces_no_edit`. Without it, every child toggle
would dirty the whole path to the root and a byte-identical save would rewrite
the file.

One correction the tests made to my own first draft: an **edit's range** is
always three bytes, but the **diff** need not be — `[ ]` → `[x]` moves one byte,
because only the middle one changes. So the assertion is *containment*: every
differing byte lies inside a named range, and the document does not change
length. The equality form I wrote first was wrong.

## Decision: ids are neither supported nor precluded

No `id`, no `dependsOn`, no DAG. What makes that safe to add later is that this
module **reads only the marker** and never rewrites a task's text: the `~3d #tag
@name` tail is exposed as a byte range ([`Item::text`]) and left alone, so an
`id:abc123 dependsOn:def456` tail can be added without this module changing.
Asserted by `an_id_in_the_tail_is_left_alone`.

## What the wire gets, and what it does not — the boundary is UNCLEAN

`TodoEntry` is `{content, status, by}`, with
`TodoStatus { Pending, InProgress, Completed }` and
`TodoBy { Model, Operator }` (`letibot/wt-resume-chain/crates/sessionlog/src/event.rs`).

**Two axes, and neither closes:**

1. **Status maps two of three, with a gap on BOTH sides.** The file has
   `open`/`declined`/`done`; the wire has `Pending`/`InProgress`/`Completed`. So
   `Open ↔ Pending` and `Done ↔ Completed`, but **`Declined` has no wire state,
   and `InProgress` has no file state.** A sync must decide what to do with each
   — drop, widen the enum, or refuse — and that decision is upstream of this
   module.
2. **Authorship is absent and cannot be added.** `by` distinguishes the
   operator's rows from the model's because they share one list. A markdown line
   records nothing about who wrote it, so markdown → wire must assign `by` as a
   *policy*, and wire → markdown loses it. (The spec's `@USERNAME` is
   **assignment**, a different axis — it says who a task is for, not who wrote
   it.) And two variants cannot express the third writer the operator's own
   design now has: their `$EDITOR`, a `git pull`, another agent.

This is a **view of a file**, and it carries what the file carries.

## The API

`src/todo.rs`, exported from `lib.rs`. No `Buffer`, no `Editor`, no crossterm:
`&str` in, byte ranges and edits out, so an embedder can drive it. That placement
is a hard requirement — `editor`, `ui`, `keys` and `bindings` are binary-only, so
a schema behind `main.rs` would be unreachable from leticl.

```rust
pub enum State { Open, Declined, Done }          // the spec's three
pub struct Item { state, marker, item, text, parent, depth, line, col, heading }
pub struct Heading { level, line, section, title }
pub struct Edit { range, replacement }
pub fn apply(src: &str, edits: &[Edit]) -> String

impl Doc {
    pub fn parse(src: &str) -> Doc;              // never fails
    pub fn items(&self) -> &[Item];
    pub fn headings(&self) -> &[Heading];
    pub fn items_in(&self, heading: usize) -> Vec<usize>;
    pub fn progress(&self, heading: usize) -> (usize, usize);
    pub fn set_subtree(&self, item: Option<usize>, s: State) -> Vec<Edit>;  // down + up
    pub fn derive_up(&self) -> Vec<Edit>;                                   // up only
    pub fn subtree(&self, item: usize) -> Option<Range<usize>>;
    pub fn check(&self) -> Vec<(usize, usize, usize, String)>;
}
```

## The second caller: the model completing a task

The write-back ruling means the surgical toggle gains a caller that is not a
keystroke. Measured in `tests/todo_writeback.rs` rather than assumed:

**The non-interactive path needs no interactive state.** `src/todo.rs` contains
no cursor, viewport, scroll, `Buffer`, `Editor`, `Frame`, `ui::`, `editor::`,
`Instant` or `now()` — one `&str` in, one `String` out. So nothing has to be
faked to call it from harnessd.

**But addressing is a real gap, and it is the caller's to solve.** The API
addresses a task by **index into `items`**, and a wire `TodoEntry` carries a
`content` string. So a non-interactive caller must map content → index itself,
and two things make that worse than it looks:

- **The file's text is not the wire's content.** Measured: the raw text of
  `- [ ] Add readme file with newline #example` is
  `" Add readme file with newline #example"` — leading space, and the whole
  metadata tail still attached. A wire content is a trimmed title with the tail
  split off. `trim()` alone is not enough.
- **It is ambiguous.** Two tasks with the same title give two hits, and the
  module cannot know which the model meant. Taking the first is a silent wrong
  write.

Neither is fixed here, deliberately: `content → item` is a *policy* question
(which of two identical titles, and how to strip a tail whose format is the
spec's, not ours), and the module is right to know only about the file. What the
finding gives the caller is the shape of the problem and a test that pins the
caveats.

**Two bugs the second-caller tests found in this module, both real:**

1. **`Item::text` covered the subtree, not the line.** For a parent it returned
   `" Parent ~3d …\n  - [ ] sub one\n  - [x] sub two\n\n"`. The field is
   documented as the task's own text and was nothing of the kind — and for a
   content-matching caller it was worse than useless, since a blob containing the
   children matches almost anything. Now it ends at the item's own newline; the
   subtree stays available as `Item::item`, which is what a cascade needs.
2. **`derive_up` ran on the parsed states, not the produced ones.** Measured:
   completing a parent's last child left the parent **open**, because the derived
   pass still saw the child as it had been. `set_subtree` now computes both passes
   against the states the cascade would produce. This is precisely why
   `set_subtree` exists rather than being a documented two-call sequence — the
   doc comment claimed the right thing and the code did the other.

Both were found by driving the operation the way a non-interactive caller would,
which is the argument for having written the file at all.

## Editor integration (landed 2026-09-30)

`src/todo_ctrl.rs`, binary-side, and the mapping it owns:

- `todo` speaks **byte offsets into `Buffer::text()`**; the editor speaks
  `(row, char col)`. Those differ the moment a line holds a multibyte character,
  so `row_chars` converts by walking the row rather than by adding, and it is
  asserted through the real `Editor`.
- The conversion is computed **once, before any edit**, which is sound only
  because every edit is three ASCII bytes for three. `apply_todo_edits` still
  applies descending by offset, so a future non-length-preserving edit cannot
  silently corrupt the ones after it.
- Each action is **one undo step** (`ActionKind::Todo`), including a section
  cascade that rewrote dozens of markers.
- `check()` rides the existing 300 ms diagnostic debounce, so the todo parse
  (2.1 ms on a 200-item file) is never on the frame path — the same discipline
  the highlight window uses.
- The binary uses the **library's** `todo` module rather than compiling a second
  copy, so there is one implementation of the format.

Measured on a real file through the real UI: `M-C` on a `## Now` heading ticked
its two tasks and nothing else, the diff was those two lines, the file stayed
140 bytes, and the `- [/]` line was reported in red in the gutter.

## Size

**~250 → ~1 200 lines.** `src/todo.rs` is 605 lines (about 390 non-comment),
`tests/todo_schema.rs` 422 and `tests/todo_writeback.rs` 169; the earlier
`tests/markdown_grammar.rs` (525) pins the grammar facts that both the schema and
the highlighter depend on.

The growth over the original estimate is the spec being fixed rather than
invented — the third state, the validated read for it, sections, the diagnostic
that only `check()` can produce, and the ancestor pass.

## Deliberately absent

- **No renderer and no inverse function.** That is the design: prose, the
  metadata tail, wrapping and trailing whitespace are never represented, so they
  cannot be lost.
- **No staleness apparatus.** Out of date is not invalid; the next touch fixes
  it. `check()` is for what the format *cannot express*, not for what somebody
  typed wrong.
- ~~No editor integration yet.~~ **Landed**: `src/todo_ctrl.rs` wires the schema
  into the editor — M-T ticks the task on the line, M-C the section (or the
  task's subtree), M-X declines — and `check()` feeds the gutter that already
  renders diagnostics. The byte-to-row mapping is the binary-side half and is
  asserted directly (`pos_of_byte`), because it is the one place the two
  coordinate systems could disagree silently.
- **No second parser.** Every range is a grammar node or a validated read at a
  grammar-given offset.
