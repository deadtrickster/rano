# Brief: `syntax::Stream` — a generic append-only incremental parse engine

For the agent working in this repo. The consumer is external (a TUI conversation
renderer in another project); this brief is self-contained — everything needed is here
and in this repo. Read `src/syntax.rs` first: the new type sits next to `Highlighter` and
follows its conventions.

## 0. Why

The consumer streams a growing markdown document: it receives text in small deltas
(tens of pushes per second, one per model token) and re-renders after each push. It needs
three things this repo does not have:

1. **Incremental re-parse.** `Highlighter::refresh` and `Highlighter::classes` full-reparse
   per call (the full-reparse rule at `syntax.rs:625-628` is deliberate for the *editor*,
   whose lines get shorter). Full-reparse per push is O(N²) over the stream: each push
   re-parses the whole document, so the total work grows with the square of the number of
   pushes. The budget here is the µs range per push as the document grows to ~200 KB;
   §3.5 measures whether the incremental engine meets it.
2. **The tree as plain data.** The consumer walks the tree to build its layout model and
   must not depend on the `tree-sitter` crate: the public API returns this crate's own
   types — the same rule as `classes()` returning `Vec<Vec<Option<String>>>` rather than
   nodes.
3. **A second grammar over selected ranges.** Markdown is parsed with two grammars (block
   + inline; `tree-sitter-md`'s README, "Standalone usage"): the inline grammar parses
   only the byte ranges the block tree marks as inline content. The engine must support
   this generically — it is tree-sitter's `set_included_ranges`, and the same mechanism
   covers HTML-embeds-JS, C-embeds-asm, and the rest.

The engine is **generic over `Lang`**. Markdown is the first consumer, not the shape of
the API: no markdown knowledge in the engine, no markdown module.

## 1. Constraints

- **Additive only.** Do not change `Highlighter`, `refresh`, `classes`, `style_at`,
  `syntax_errors`, `detect`, any existing query, or the editor's behaviour. `Stream` is a
  new type in `src/syntax.rs`.
- **No new dependencies.** Do not bump the `tree-sitter` (0.27) or `tree-sitter-md`
  (0.5.3) pins in `Cargo.toml`.
- **Do not enable `tree-sitter-md`'s `parser` feature.** It is optional and pulls
  `tree-sitter 0.26` as a direct dependency — a second tree-sitter C runtime in the link,
  colliding with the 0.27 this crate uses. That is exactly the class of problem the
  vendored-dockerfile comment at `syntax.rs:19-27` documents. Use only the crate's
  constants: `LANGUAGE`, `INLINE_LANGUAGE`, `HIGHLIGHT_QUERY_BLOCK`,
  `HIGHLIGHT_QUERY_INLINE`.
- **No `tree-sitter` types in the public API.** `Node` and `Capture` below are this
  crate's own structs. Private fields may hold `Parser`/`Tree`/`Query`.

## 2. What to build

### 2.1 One grammar registration

`Lang::MarkdownInline` → `tree_sitter_md::INLINE_LANGUAGE`, next to `Lang::Markdown`
(`syntax.rs:73`). Its `query()` arm is `tree_sitter_md::HIGHLIGHT_QUERY_INLINE`. This is a
grammar registration, not markdown logic.

### 2.2 The type

```rust
/// An append-only document, incrementally parsed.
///
/// The editor's `Highlighter` full-reparses because editor lines get shorter; this
/// type only ever grows, which is the case tree-sitter's incremental re-parse is safe
/// for (a pure-append edit never shortens a line — see the module note, §4).
pub struct Stream {
    /* private: Parser, Option<Tree>, String src, pending included ranges,
       cached query + its (Lang, String) key, parse_calls */
}

impl Stream {
    /// Infallible: every `Lang` has a valid grammar. If `set_language` ever fails,
    /// the stream is inert: `push` is a no-op, `root()` is `None` — the same
    /// "empty on failure" convention as `classes()`.
    pub fn new(lang: Lang) -> Self;

    /// Append `delta` to the end and re-parse. With a previous tree: edit the tree
    /// for the pure-append range and pass it to `parse`, so the unchanged prefix is
    /// reused. Without: a fresh parse.
    pub fn push(&mut self, delta: &str);

    pub fn src(&self) -> &str;

    /// The current tree as this crate's own type. `None` before the first push or on
    /// an inert stream.
    pub fn root(&self) -> Option<Node>;

    /// Restrict the next parse to these byte ranges (tree-sitter's
    /// `set_included_ranges`). The generic form of "a second grammar over selected
    /// parts of the document". An empty slice means the whole document (tree-sitter's
    /// convention). Ranges must be sorted and non-overlapping, as tree-sitter
    /// requires; sort them here rather than trusting the caller.
    pub fn set_included_ranges(&mut self, ranges: &[(usize, usize)]);

    /// Run a query over the current tree: one `Capture` per capture, in document
    /// order. The query compiles once per (language, text) and is cached, the same
    /// way `Highlighter` caches per language. `Vec::new()` on an inert stream or a
    /// query that fails to compile.
    pub fn captures(&mut self, query: &str) -> Vec<Capture>;

    pub fn parse_calls(&self) -> u64;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Node {
    pub kind: String,     // "atx_heading", "fenced_code_block", "strong_emphasis", …
    pub start: usize,     // byte offsets into `src()`
    pub end: usize,
    pub has_error: bool,
    pub is_missing: bool,
    pub children: Vec<Node>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Capture {
    pub name: String,
    pub start: usize,
    pub end: usize,
}
```

### 2.3 Implementation notes (verified against `tree-sitter` 0.27.0)

- **The edit.** `Tree::edit` takes an `InputEdit` with byte offsets **and** `Point`s
  (`{ row, column }`):
  ```rust
  InputEdit {
      start_byte: n, old_end_byte: n, new_end_byte: m,
      start_position: p(n), old_end_position: p(n), new_end_position: p(m),
  }
  ```
  where `n` is the old length, `m` the new, and `p(off)` = (count of `\n` in
  `src[..off]`, `off` minus the offset just after the last `\n`). Keep the source-end
  `Point` incrementally (two integer updates per push) rather than rescanning.
- **`set_included_ranges`** takes `&[Range]`, where `Range { start_byte, end_byte,
  start_point, end_point }` — convert each byte offset with the same `p()` helper.
  Store the pending ranges on the struct and apply them to the parser **before** the
  next `parse` call.
- **`root()`** is a recursive walk: `kind()`, `start_byte()`, `end_byte()`,
  `has_error()`, `is_missing()`, `children()`. Build a fresh `Node` tree per call — the
  consumer calls it once per push and the trees are small (a 200 KB markdown document is
  a few thousand nodes).
- **`captures()`**: factor the capture iteration out of the private walk that fills
  `build_classes`'s grid if it fits cleanly; otherwise a small `QueryCursor` walk.
  Capture name = `capture_name(index)`.
- **Counters.** `parse_calls` increments per `parse()` call. Do **not** keep a
  "bytes re-parsed" counter: tree-sitter does not expose how much of the prefix it
  reused, and any proxy (e.g. `src.len()` per call) is quadratic by construction and
  would answer the wrong question. The not-quadratic question is answered by **time**,
  in the test (§3.5).

### 2.4 The two-pass reference (read, do not link)

The `tree-sitter-md` crate ships a `MarkdownParser` doing the block+inline passes,
behind its `parser` feature — which §1 forbids enabling. Read it as the reference in the
cargo registry source (`tree-sitter-md-0.5.3/bindings/rust/parser.rs`,
`parse_with_options`); the details that matter:

- The inline content lives in block-tree nodes of kind **`inline` and
  `pipe_table_cell`** (table cells hold inline content too).
- For each such node, the included ranges are the node's range **split around its named
  children**: the named children (e.g. an `inline_code` span inside the inline node) are
  *excluded* from the inline grammar's ranges; the unnamed text between them is
  included. (The block grammar already parsed those children; the inline grammar must
  not re-parse them.)
- The reference does one parse per inline node, each with its own old tree. This engine
  instead does **one parse over all ranges** on one stream — the consumer sets the
  combined, sorted, non-overlapping range list per push. Whether that keeps the
  incremental reuse honest is exactly what §3.5 measures.

## 3. Tests

In the existing test module of `src/syntax.rs` (or a `#[cfg(test)] mod stream_tests`
next to it). All must pass with `cargo test -p rano` alongside the existing suite.

1. **Streaming == full parse, two grammars.** For `Lang::Rust` and `Lang::Markdown`:
   take a document (≥ 2 KB each; the markdown one must contain a heading, a paragraph
   with `**bold**` and `` `code` ``, a fenced block, a list and a pipe table), parse it
   whole (one push) and in 7 random-sized chunks (seeded — the test must be
   reproducible); assert the `root()` trees are equal (`Node` derives `PartialEq`).
   This is the property the consumer's renderer is built on.
2. **The two passes.** A markdown primary stream + an inline secondary stream
   (`Lang::MarkdownInline`): after each push to the primary, collect the included ranges
   from the primary's `root()` per §2.4 (kinds `inline` and `pipe_table_cell`, split
   around named children, sorted), `set_included_ranges` on the secondary, push the same
   delta, and assert the secondary's `root()` covers exactly those ranges and contains
   the expected kinds (`strong_emphasis`, `inline_code`, `link`) at the right byte
   offsets.
3. **Error recovery.** Rust: push `fn main() { let x = ` → `root()` has an error node;
   push `1; }` → no error node, and the prefix of the tree (the function header) is
   unchanged. Markdown: push an unterminated fence → the last block carries an
   error/missing node; push the closing fence → clean.
4. **Included ranges change mid-stream.** The §3.2 scenario *is* this (the range list
   grows at the tail on every push while earlier ranges are unchanged); assert it stays
   correct through the whole run, not just the final state.
5. **The measurement** (the number this brief exists for). For `Lang::Markdown` and
   `Lang::Rust`: build a ~100 KB document, push it in 1,000 appends, record per-push
   wall time; print the median and p95 of the first 100 pushes and of the last 100, plus
   the total. **Assert not-quadratic**: p95(last 100) < 5 × median(first 100) — per-push
   cost flat as the document grows. Run it for the secondary stream of §3.2 as well,
   while its included ranges grow. If the assertion fails, that is the result: report
   the number, do not paper over it.

## 4. Module doc

On `Stream` (and one line in the `syntax` module doc): the append-only contract, and why
this does not hit the full-reparse rule documented at `syntax.rs:625-628` — that rule
exists because the *editor* re-parses buffers whose lines get shorter, and incremental
reuse then leaks byte offsets from the old source; a pure-append edit never shortens a
line, so the reuse is sound.

## 5. Exit criteria

- `cargo test -p rano` green: the existing suite untouched and passing, plus §3.
- The §3.5 measurement printed for both languages, in the test output.
- `cargo build -p rano` (the editor binary) still builds; the editor path is
  behaviourally unchanged (verify by diff — you did not touch it).
- No new dependencies in `Cargo.toml`; the `tree-sitter` / `tree-sitter-md` pins
  unchanged; the `parser` feature not enabled.

## 6. What the consumer does with it (context, not your work)

So the API's shape makes sense: the consumer holds one `Stream` per growing document —
a conversation reply, pushed per token delta, tens of pushes per second, growing to
~200 KB over a long reply. Per push it reads `root()` and rebuilds its block layout
(headings, code fences, lists, tables), and holds a second `Stream`
(`Lang::MarkdownInline`) whose included ranges are the inline-content ranges of the
first, for bold/italic/code/link styling. Settled (no longer growing) documents are one
big push. The per-push cost budget is the µs range; §3.5 guards it.
