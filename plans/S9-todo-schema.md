# S9 — A todo schema in bare rano (org-ish, over markdown)

Status: design, for ruling. Not implemented.
Written 2026-09-29, off a conversation the operator had about leticl's todos pane.

## What this is for, stated as the constraint it must satisfy

leticl wants the repo's `TODO.md` editable in its head. It stopped because
`read-todo-md` is lossy — it parses lines into rows and re-renders from those
rows, so a save would reformat prose, `Deps:` indentation, wrapping and
trailing whitespace on a file other people have open.

**The design answer is that there is no re-render, because there is no inverse
function.** Nothing here turns a model back into markdown. The document is the
only source text that exists; the schema holds *byte ranges into it*, and every
operation produces an `Edit` — a single `(range, replacement)` splice that the
caller applies. Prose between items, indentation shape, wrapping and trailing
whitespace are not represented and therefore cannot be lost. That is not a
promise about care; it is a consequence of there being no code that writes a
document.

Corollary, and it is the architectural line: **the file is the source of truth
and the wire is a projection.** `TodoEntry` is flat (`content`, `status`) and
cannot carry a heading tree or prose, so markdown → wire is total and wire →
markdown is not attempted. Anyone who tries to round-trip the wire back into a
file will reproduce the bug leticl stopped to avoid.

## The measurements this rests on

Taken in this tree, 2026-09-29, `cargo test --test markdown_grammar -- --nocapture`.

**1. The grammar already models everything structural.** One query string gets
headings, sections, items and checkbox states with exact byte ranges:

```
(atx_heading) @heading
(list_item) @item
(task_list_marker_checked) @checked
(task_list_marker_unchecked) @unchecked
```

`Stream::captures(query)` (`syntax.rs:1807`) runs it and returns
`Capture { name, start, end }` — byte offsets. On a sample document:

```
  heading    0..10   "# Project\n"
  heading   49..56   "## Now\n"
     item   57..73   "- [ ] open item\n"
unchecked   59..62   "[ ]"
     item   73..89   "- [x] done item\n"
  checked   75..78   "[x]"
```

The **marker range is exact and 3 bytes** — that is the whole toggle.

**2. `section` nests and carries the subtree span**, which makes folding and
subtree move free rather than specified:

```
document [0..235]
  section [0..235]
    atx_heading [0..10]
    paragraph [11..48]
    section [49..189]           <- ## Now, and everything under it
      atx_heading [49..56]
      list [57..126]
      section [126..189]        <- ### Deep, nested inside
    section [189..235]
```

A fold is `section.start..section.end` and changes **zero bytes**. A subtree
move is a cut of exactly that range and an insert elsewhere. Neither needs a
"find where the heading's body ends" scan, and no line-counting.

**3. [X] is recognised, and all three bullet styles are.**

```
task_list_marker_checked   2..5   "[X]"     (from "- [X] capital")
list_marker_star           0..2   "* "
list_marker_dot            0..3   "1. "
```

**4. THE FINDING — the grammar is SILENT about a marker it does not know, and
this is the whole reason the schema is not just a query.** `- [-] partial` and
`- [~] whatever` parse as ordinary list items: no ERROR node, no missing node,
no capture, `has_error == false`.

```
- [-] partial
  list_item     0..14   "- [-] partial\n"
    list_marker_minus  0..2  "- "
    paragraph          2..14 "[-] partial\n"
      inline            2..13
        [               2..3
        -               3..4
        ]               4..5
```

Read that as the failure mode it is: a typo'd marker renders as an ordinary row
and **nothing anywhere reports it**. This is exactly the "a set which silently
accepts an unknown member is the expensive failure" case, except the silent
acceptance is in the *grammar*, not in a table of ours — so the schema is the
only thing that can catch it, and a diagnostic is not optional.

It also means a query **cannot** express the third state. There is no node to
capture. That is the one place where the schema reads bytes the grammar did not
hand it a node for, and it reads them at a position the grammar *did* give
(`paragraph.start` is exactly where a marker would have been), then validates
the shape. Reported as a finding rather than routed around: **a query cannot
express `[-]`.**

## Decisions

### 1. How much org-mode

**In** — the three that the tree above makes cheap or free:

- **Three-state checkbox**, org's own: `[ ]` pending, `[-]` in progress, `[X]`
  done. The three states are not a coincidence of org; see the wire decision
  below.
- **Heading fold** — `section` range, zero bytes.
- **Subtree move** — the same range, one splice.

**Out, with the price stated:**

- **TODO/DONE keywords on headings** (`## TODO fix this`). This is the one that
  looks free and is not. It is a *second* status vocabulary in the same file, so
  it needs a precedence rule the moment a heading says `DONE` and an item under
  it says `[ ]` — and org has a real answer (`TODO` keywords are on the
  *heading*, checkboxes are on the *item*, and a parent with children is
  computed from them). That computation is a spec, and the wire carries one
  status per entry, so the two vocabularies would have to be reconciled before
  sync. Left out until something consumes it.
- **Scheduled dates / deadlines** (`SCHEDULED: <2026-09-29>`). Not free: needs a
  timestamp type, a reader for org's time syntax, and answers to timezone and
  repeat questions. Nothing in this repo or in the daemon's todo shape has a
  place to put a date.
- **Tags** (`:work:urgent:`) and **priority** (`[#A]`). Cheap to read, but each
  is a namespace with its own precedence questions and no consumer. Deferred
  rather than refused.

### 2. The set is closed, and an unknown member is refused by name

`{ Pending, InProgress, Completed }`. An unrecognised marker character produces
a diagnostic that **names the character and the byte offset**. No coercion to
Pending, no "treat as a plain item" — that is precisely what the grammar already
does, and it is the bug.

### 3. The boundary with harnessd's `TodoEntry`: MATCH

Read in `letibot-profiles/crates/sessionlog/src/event.rs:88`:

```rust
pub enum TodoStatus { Pending, InProgress, Completed }
pub struct TodoEntry { pub content: String, pub status: TodoStatus }
```

**Correction to the brief I was given: there is no `by` field.** `TodoEntry` is
`{content, status}`. (There are `by: String` fields in that file, at lines 179,
677 and 787, but they belong to other events — `Decider`-carrying ones.) So the
"distinguishes the operator's rows from the model's" distinction is not in this
struct, and if it is needed it lives somewhere else and should be pointed at
before this design leans on it.

That leaves a clean answer: **rano's three states ARE the wire's three states**,
same names, same meaning. Matching, not extending. This is why org's `[-]`
matters — it is the third state that makes a 1:1 map possible at all; a
two-state checkbox (`[ ]`/`[x]`, which is all the grammar gives you) cannot
express `InProgress` and would force a keyword extension.

## The API

New module `src/todo.rs`, exported from `lib.rs` — **this is the requirement
that shapes the placement.** `lib.rs` exports `buffer encoding rows syntax
width`; `editor`, `ui`, `keys` and `bindings` are binary-only. A schema that
landed behind `main.rs` would be unreachable from leticl and the entire reason
for the work would evaporate. So: no `Buffer`, no `Editor`, no crossterm, no
`Frame`. `&str` in, ranges and edits out.

```rust
pub enum State { Pending, InProgress, Completed }

pub struct Item {
    pub state: State,
    pub marker: Range<usize>,      // the exact 3 bytes, "[ ]" / "[x]" / "[-]"
    pub item: Range<usize>,        // the whole list_item
    pub text: Range<usize>,        // content after the marker
    pub heading: Option<usize>,    // index into headings
    pub line: usize,               // 0-based, for a diagnostic
    pub col: usize,                // char col, for a diagnostic
}

pub struct Heading {
    pub level: usize,              // '#' count
    pub line: usize,
    pub title: Range<usize>,
    pub section: Range<usize>,     // the whole subtree, nested sections included
}

pub struct Doc { src: String, items: Vec<Item>, headings: Vec<Heading> }

/// One splice. The ONLY way anything here changes a document.
pub struct Edit { pub range: Range<usize>, pub replacement: String }
impl Edit { pub fn apply(&self, src: &str) -> String; }

impl Doc {
    pub fn parse(src: &str) -> Doc;
    pub fn items(&self) -> &[Item];
    pub fn headings(&self) -> &[Heading];

    /// The primitive: set an item's state. A 3-byte splice, always.
    pub fn set_state(&self, i: usize, s: State) -> Edit;
    /// The common case: Pending <-> Completed.
    pub fn toggle(&self, i: usize) -> Edit;
    /// Cycle Pending -> InProgress -> Completed -> Pending (org's C-c C-t).
    pub fn cycle(&self, i: usize) -> Edit;
    /// Nothing to change: a fold is a range, not an edit.
    pub fn fold_span(&self, h: usize) -> Range<usize>;
    /// Move a heading's subtree. One cut, one insert.
    pub fn move_subtree(&self, h: usize, to: usize) -> Edit;
    /// Schema violations, in the shape `syntax_errors` already returns so the
    /// existing gutter can render them: (line, col, end_col, message).
    pub fn check(&self) -> Vec<(usize, usize, usize, String)>;
}
```

`check` reports, at minimum: an unknown marker character, and (if the schema
requires it) a checkbox under no heading. Neither is reported by the grammar.

## The test that must be written first

Not "the toggle worked". **"The file after equals the file before except for the
bytes I meant to change."**

```
fn a_toggle_changes_exactly_three_bytes()      // and nothing else, anywhere
fn every_state_change_is_a_three_byte_splice() // Pending/InProgress/Completed, all 9 ordered pairs
fn a_fold_changes_no_bytes()                   // the span is returned, the text is not
fn a_subtree_move_is_the_sections_lines()      // cut == section range, byte for byte
fn parse_apply_reparse_agrees()                // the edit did what it claimed in the model too
fn an_unknown_marker_is_refused_by_name()      // [-], [~], [x ] each named, none coerced
fn prose_indentation_and_trailing_space_survive_untouched()
```

The last one is leticl's actual complaint and should carry a fixture with
deliberately awkward content — two spaces after a period, a `Deps:` block,
hard-wrapped lines, trailing whitespace — so the assertion is that they come out
identical rather than that nobody happened to touch them.

## On reusing the tree, and one place I am not

`Stream::captures` re-parses: `Stream` is its own document and its own tree.
The editor's `Highlighter` has the file's tree already but exposes no arbitrary
query (`classes` runs the language's fixed query). So `todo::Doc` currently
costs one extra parse of the todo file.

That is fine and should be said out loud rather than discovered later: a
TODO.md is kilobytes, and the schema is needed on **open, save and toggle** —
not per keystroke. If it is ever wanted on the editing hot path, the right move
is to add a query method to `Highlighter` so it uses the tree it already has,
not to parse twice per frame. Deliberately not doing that now: it would put a
second consumer inside the frame path for a case nobody has asked for.

## What is deliberately not here

- No renderer. See the top: there is no inverse function, which is the point.
- No second parser for markdown. Every range comes from a query or from a
  validated read at a grammar-anchored offset (the one `[-]` case).
- No editor integration in this step. `ui.rs`/`editor.rs` know nothing about
  todos yet; the schema has to exist and be reachable from the library first,
  and leticl is the first consumer.
