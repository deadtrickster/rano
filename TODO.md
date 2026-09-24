# rano TODO

## 1. Crash bugs
- [x] Prompt input panics on multibyte text: `p.text.insert/remove(p.cursor, …)` mixes
  char-count cursor with String byte-index API (main.rs:1408–1431). Type `é` then
  Backspace → panic. Fix: keep prompt text as `Vec<char>` or byte-offset cursor.
- [x] `move_left` at BOL sets cursor past EOL: uses `line_len(c.row)` (old row) at
  main.rs:527; next Backspace panics in `Buffer::backspace` (buffer.rs:102).
  Fix: `line_len(self.cursor.row)`.

## 2. Correctness / data integrity
- [x] Atomic save: write `file.tmp` + rename instead of `fs::write` (main.rs:1005).
- [x] Preserve CRLF line endings (load detect + save with same EOL).
- [x] Docs drift: README + help overlay say `^W`/`^Y`, code binds `^F`/`^B`/M-B/M-F.
  Single source of truth for bindings (table used by bar, help, README).
- [x] `show_loc` (`^C`) is sticky — auto-clear after a few seconds.

## 3. Performance
- [x] Replace-all is O(N²): `find_next` → `find_all` per match (main.rs:874). Collect
  matches once, splice last→first.
- [x] Undo: replace 500 full-buffer snapshots with edit-based undo
  (kind, pos, removed, inserted); keep coalescing.
- [x] LSP: debounce `didChange` (send on idle) or incremental sync; stop allocating
  full `buf.text()` per keystroke.
- [x] LSP handshake blocks UI up to 15 s (lsp.rs:296) — spawn on background thread.
- [x] `ui::draw`: stop cloning visible lines + per-char Spans (ui.rs:79–84); coalesce
  style runs; draw only when dirty.
- [x] `sort_lines`: `sort_by_cached_key` instead of per-comparison String allocs.
- [x] `do_exec` blocks event loop indefinitely (main.rs:933) — timeout or async.

## 4. Robustness / UX
- [x] Handle `Event::Paste` (bracketed paste) — currently dropped (main.rs:1513).
- [x] Panic guard: restore terminal (raw mode off, leave alt screen) on panic.
- [x] Horizontal scroll (or soft-wrap) for long lines.
- [x] Prompts: filename Tab-completion, history (search/exec), `~` expansion,
  word-motion.
- [x] Diagnostics: gutter markers using unused `Diagnostic.col`; next/prev-diagnostic
  jump.

## 5. Features (roadmap)
- [x] Config file (tab width, auto-indent, line numbers, multibuffer; theme and
  custom bindings out of scope for v1)
- [x] Auto-indent on newline
- [x] Line-number gutter (nano M-N)
- [x] Regex / case-toggle search
- [x] Filter region through command (nano `^|`)
- [x] More tree-sitter langs (Python, C, JSON)
- [x] Multiple buffers

## 6. Code health
- [x] Split main.rs (~3.9k → 1.4k lines): `editor.rs`, `prompt.rs`, `search_ctrl.rs`,
  `lsp_ctrl.rs`, `exec_ctrl.rs`, `keys.rs` (+ the W1 leaf modules).
- [x] Version from `env!("CARGO_PKG_VERSION")` instead of hardcoded TITLE_LEFT (ui.rs:19).
- [x] CI: fmt + clippy + test workflow.
- [x] Mark 20 s `rust_diagnostics_flow` LSP test `#[ignore]`.

## 7. Test coverage gaps (baseline 2026-09-06: 27 tests, all pass after installing
    the rust-analyzer rustup component; zero tests for main.rs / ui.rs; no tests/
    dir, no CI)
- [x] Editor tests: undo/redo coalescing, cut/paste/copy semantics, replace state
  machine (y/n/a/q), search state, prompt editing (regression for §1 bug 1),
  movement incl. BOL wrap (regression for §1 bug 2), `char_style` priority.
- [x] `copy_range`, `insert_char`, `replace_at` edges, overlapping `find_all`.
- [x] syntax: multibyte offsets in `build_styles`, language switch on one Highlighter,
  `theme()` fallback.
- [x] lsp: `change()` UTF-16 payload, `poll()` server-request answering.
- [x] ui: `title_line`, function-bar layout math (pure functions).

## 9. streaming parse engine (`syntax::Stream`)

From `plans/S8-streaming-engine.md` (= `../streaming-engine-brief.md`): an
external consumer (a TUI conversation renderer) streams a growing markdown
document, tens of pushes per second, and re-renders per push. `Highlighter`
full-reparses per call, which is O(N²) over such a stream; the engine must be
**append-only and incremental**, **generic over `Lang`**, and must hand back
**plain data** (no `tree-sitter` types in the public API). Markdown's block +
inline two-grammar split is the first consumer, not the shape of the API.

- [x] `Lang::MarkdownInline` → `tree_sitter_md::INLINE_LANGUAGE`, query
  `HIGHLIGHT_QUERY_INLINE` (a grammar registration, not markdown logic).
- [x] `pub struct Stream` + `Node` + `Capture` in `src/syntax.rs`:
  `new(lang)`, `push(delta)`, `src()`, `root() -> Option<Node>`,
  `set_included_ranges(&[(usize, usize)])`, `captures(query) -> Vec<Capture>`,
  `parse_calls() -> u64`. (`Node` also carries `named: bool` — tree-sitter's
  `is_named()`. The brief's field list has no way to tell an anonymous token
  from a named node, and §2.4's "split around the named children" step is
  unimplementable without it.)
- [x] `push` edits the previous tree for the pure-append range (`InputEdit`
  with byte offsets *and* `Point`s) and re-parses; the source-end `Point` is
  kept incrementally.
- [x] `set_included_ranges` stores the ranges and applies them to the parser
  before the next `parse` (sorted + merged here rather than trusting the
  caller; empty = whole document).
- [x] Tests: streaming == full parse (`Node` equality) for Rust + Markdown;
  the two passes (block stream feeding an inline stream's ranges); error
  recovery; included ranges changing mid-stream; the measurement.
- [x] Constraints: additive only, no new dependencies, `tree-sitter-md`'s
  `parser` feature not enabled. `lsp.rs` gained one match arm per new enum
  variant (both behaviour-neutral: no server, id `"markdown"`).
- [ ] **OPEN: incremental reuse is not an asymptotic win — the brief's µs
  budget misses by 100×.** Measured (release, 1,000 pushes of ~100 bytes):
  markdown 829 µs → 9.5 ms per push (**11× worse at the end**, so the suite's
  not-quadratic bound of 5× fails), Rust 42 µs → 305 µs (7×), two-pass
  pipeline 5.4 ms → 88.8 ms (16×). Per-push cost is linear in the document
  (~106 ns per document byte for markdown, ~3.5 for Rust), so a stream is
  O(N²) in total; `Tree::edit` is ~0.3 µs and not the cost. Cause found in
  tree-sitter's source, not in the driver: `ts_parser__can_reuse_first_leaf`
  (`parser.c`) refuses to reuse a token when the current parse state admits
  external tokens, and markdown's block grammar has a 48-state external
  scanner active in nearly every block state — so a markdown push re-lexes
  essentially the whole document. Rust's states mostly do not, hence 3% of a
  full parse. Fixes would be at the consumer or the grammar level: parse only
  the growing tail via `set_included_ranges` and splice trees, or settle
  closed blocks out of the parser's input. Numbers and the criterion are
  pinned in `syntax.rs`'s `stream_tests` (`an_incremental_push_beats_a_full_reparse`
  by default; `per_push_cost_stays_flat` ignored, and it fails).
- [ ] Also from the brief, as written vs what the grammars do: §3.3's markdown
  half ("an unterminated fence carries an error/missing node") is not true of
  `tree-sitter-md` — CommonMark closes a fence at EOF, so the block is
  complete and error-free; the test pins that instead. §3.2's construct names
  are `code_span` and `inline_link` in the inline grammar, not `inline_code`
  and `link`.
- [x] **The hub API**, added 2026-09-20 for the consumer that asked for it
  (`letibot`'s `TODO.md` R18: *"rely on rano as much as possible"*):
  - `Lang::from_token` — an info-string token to a `Lang`. `detect` takes a
    *path*, and a markdown fence hands over `rust`/`tsx`/`sh`, so without this
    nothing outside rano could be coloured from an info string at all. Its own
    table, not `detect`'s: `mk` is Make as an extension and not a token, `sh`
    is bash as a token and `/bin/sh` as a path.
  - `Lang::name` — the name a reader sees, and every one of them answers
    `from_token` back (`name_tests::every_name_is_a_token_the_table_knows`
    holds the pair together).
  - `Stream::spans` — flat `Span { row, start, end, name }` records, the
    highlighting answer for a **renderer**. The per-character grid
    (`Highlighter::classes`, kept for an editor interrogating a cursor) costs
    one `Vec` per line whatever the caller does with it, and that dominated
    the walk: measured at ~1.9 µs per line before the refactor, 1.34 ms for
    7.7 KB in 700 lines against 0.46 ms for 9 KB in one.
  - `for_each_capture` and `build_styles`/`build_classes` now take `&str`
    rather than a per-line `Vec<Vec<char>>`, with an ASCII fast path (byte
    index == char index). 700 lines 1.34 → **0.88 ms**; one 9 KB line
    0.46 → **0.18 ms**; a Rust fence grown a token at a time, push + walk,
    584 → **468 µs** per push at the end of 5.4 KB, markdown 396 → 264 µs at
    8.4 KB. `--ignored` tests print all of these.
- [ ] **OPEN: the walk is still O(text) per call, so a whole document is
  quadratic over a stream.** Measured above: ~0.1 µs per byte, ~1.1 µs per
  line. Fine for a code fence (a frame's budget at fence sizes, which is what
  letibot needed and why this shipped), not fine for an editor repainting a
  200 KB file per keystroke. The fix is the range-limited walk an editor
  wants anyway — `QueryCursor::set_byte_range` over the changed rows, with the
  range widened back to the start of any node that overlaps it so a construct
  spanning lines keeps its scope. That is exactly what neovim and helix do,
  and it is a real piece of work rather than a tweak: the widening is where
  the correctness lives. File it when an editor user complains, or when
  `letibot` streams a fence long enough to matter.
- [ ] **OPEN: the markdown grammars parse at ~250-400 ns/byte, which is what
  makes a large transcript slow to open.** Measured 2026-09-20, release, on
  letibot's own stored transcripts:

      markdown block grammar      133 ns/byte   (555 KB -> 74 ms)
      markdown inline grammar     250 ns/byte   (450 KB -> 133 ms)
      Rust, for scale               ~3 ns/byte  (the `Stream` measurement above)

  Both grammars run external scanners (the block one has 48 states — see the
  reuse note above), and that is where the constant comes from; tree-sitter
  itself is not the problem. So the markdown half of rano costs ~20x the Rust
  half per byte, and a 4-6 MB transcript — letibot has sessions that size —
  takes 1.6-2.7 s to lex, which the operator sees as a head that "does nothing"
  and then paints its history.

  **Fixed so far**: the query cache (`highlight_query`) removed the part of it
  that was our bug — 8.7-10.2 ms per `Query::new`, paid per code fence and per
  diff excerpt before. What is left is the parse itself, and the levers are
  consumer-side: lex only what is drawn (a bounded viewport rather than the
  whole transcript), or cache the lexed model across restarts. Neither belongs
  in this crate — rano parses what it is handed — but the number does, because
  it bounds what a consumer can afford to hand over.

## 10. OPEN: completion junk on the repo path (paused 2026-09-08)

Symptom: typing `text.` in rano's own `src/prompt.rs` (~line 86, inside
`record_history`) pops a *path-fallback* list (`self::`, `crate::`,
`super::`, locals) instead of `str` members (`is_empty`, …). Same code at
`/tmp/rano_t` (rsync of the repo, blank line inserted after prompt.rs:88,
`git init`ed) completes correctly.

Established experimentally (rust-analyzer 1.98.0, 2026-08-18 build):
- Not edition/let-chains: minimal 2024 files with the same let-chain
  (prompt.rs:89-93 shape) complete to members.
- Not the file: full prompt.rs in a scratch crate completes to members.
- Not Cargo.toml deps: rano's Cargo.toml + minimal src → members.
- Not `mod ed_tests`, not `.git` (rano_t works with git), not column-0
  parsing (user's line is indented).
- Headless `rust-analyzer diagnostics /home/dead/Projects/rano/src/prompt.rs`
  finishes clean in ~5 s — analysis itself is fine.
- **Decisive**: two tmux sessions, same rano binary, byte-identical
  completion requests (position line 88 char 5 in both), 30 s warm-up —
  `/home/dead/Projects/rano` → junk, `/tmp/rano_t` → members. The junk is
  server-side and workspace-path-dependent. Raw logs: `/tmp/comp_fail.log`
  (junk) vs `/tmp/comp_ok.log` (members); rano writes LSP traffic to
  `$RANO_LSP_RAW` as `[->]`/`[<-]` lines.
- Earlier "poison line in editor.rs" bisects were confounded by truncation
  artifacts and timing noise; do not trust them. The timing-race theory
  (members appear only after ~15-20 s) was also misleading — the real path
  never flips even after 90 s.
- RA sends no `$/progress`/`serverStatus` notifications, so readiness can't
  be observed directly; rano discards RA stderr (lsp.rs:458
  `Stdio::null()`), so RA logs are currently invisible.

Done this round (uncommitted, gates green: 197 tests + clippy + fmt):
- `adjust_scroll_x` rewritten with min/max (editor.rs:244) — behaviour
  equivalent except a degenerate zero-width viewport now clamps to 1.
- Completion fallback-junk parking + retry: `Editor.completion_retry` /
  `completion_retries`, dot-context detection (empty prefix after `.`) with
  `self::`/`crate::`/`super::` label signature, `completion_retry_poll`
  wired into the main loop (700 ms × 40), test
  `completion_fallback_junk_parks_popup_and_retries`. Works mechanically
  (re-requests fire) but cannot fix the repo path while RA answers junk
  indefinitely there.

Next steps:
- [ ] Diff the full `[->]` initialize/didOpen streams in comp_fail.log vs
  comp_ok.log (only completion requests were compared so far) — check
  `rootUri`, `workspaceFolders`, capabilities.
- [ ] Capture RA's own logs: point lsp.rs:458 stderr at a file (env is
  inherited, so `RUST_LOG=info rano …` works once stderr isn't nulled) or
  drive RA with a small python fake-client script against
  /home/dead/Projects/rano and watch `RUST_LOG=info`.
- [ ] Path-isolation tests: copy the repo to /home/dead/Projects/rano2
  (same parent) and to /home/rano_t2 — does the junk follow the parent
  dir or the exact path?
- [ ] Suspect list for the path dependence: flycheck/proc-macro-build
  contention with the running `target/release/rano` binary inside the
  analyzed workspace; inotify watch exhaustion on the big target/ dir;
  RA root/VFS discovery picking up extra roots above /home/dead.
- [ ] After root cause: keep or retune the retry (cap/interval), decide
  whether the junk-parking stays as belt-and-braces.
- [ ] Still pending from earlier rounds: live-verify mouse support
  (`tmux send-keys -M`); all rounds uncommitted — commit only on request.

## 11. Markdown rendering and display width (from the head-parity read, done)

Reading `../head-parity-2026-09-21.md` for the letibot/leticl parity work turned
up two things on rano's own side. Both are fixed; the numbers are here because
the next reader will want to price the trade, not rediscover it.

- [x] **`width`: a character is not a column.** `ui::display_width` counted
  every character as one column, so rano had the doc's §2.1 defect by
  construction: a CJK or emoji line wrapped at twice its true width (the
  terminal showed ~half the line and the rest never reached the screen), the
  cursor landed in the wrong cell, a click mapped to the wrong character, and
  a wrap boundary could split a combining mark from its base. `src/width.rs`
  now implements char widths (0/1/2), cluster grouping (base + combining
  marks, ZWJ sequences, paired regional indicators) and a greedy segmenter
  that never splits a cluster. Verified end-to-end in a tmux pane: a 170-char
  CJK paragraph (400 columns) wrapped to rows of 196 and 144 columns and the
  text was recovered character for character — 170/170 — where the old code
  would have dropped most of it.
  - Not UAX #11/#29: the ranges a code assistant meets are listed by hand,
    and a miss costs one column. No generated table, no new dependency.
  - The per-frame cost work stands: 77/118/118/117/117 µs on the five cases
    from the earlier benchmark, and a 20 × 500k-row wide-character document
    scrolled deep is 113 µs, because wrapped rows now render from the wrap
    table's per-segment char range instead of rescanning the line.
- [x] **Markdown is two grammars.** The block query cannot see emphasis, code
  spans or links — they are nodes of the *inline* grammar — so everything
  inside a paragraph was plain, while nano colours bold, italic, code and
  links. `Highlighter::refresh`/`classes` now run a second pass: the inline
  grammar over the byte ranges the block tree marks as inline content
  (`markdown_inline_ranges`, split around the named children the block
  grammar already parsed), with both passes overlaying one grid. Fence bodies
  and indented code are one `text.literal` run (nano colours the whole fence;
  no injection grammar runs, so a `` ```rust `` block is cyan, not Rust), raw HTML
  is a tag, `~~struck~~` is struck through, and link text and its destination
  share the link colour — the distinctions nano's markdown mode draws.
  Cost, measured (release, 60 KB): 46 ms against 40 ms for a Rust file of the
  same size, so the inline parse is ~15% on top of the block pass.
- [x] **`(block_continuation) @punctuation.bracket` is gone.** That node
  spans the leading whitespace of a *continuation* line, so the indent of one
  bullet's second line painted while its neighbour's did not: two adjacent
  identical lines looked different (the doc's §1.1/§1.4 example showed it as
  rows 15 vs 16). A test pins that continuation indentation is not painted.

Left alone on purpose:

- **C4 in the parity doc — a `console` fence.** rano returns `None` for the
  `console` info string, deliberately (it is the archetypal unknown-fence
  token, and its own test names it so). The doc records that head A inherits
  that choice and head B colours `console` as shell. If the ruling goes the
  other way it is a one-line change in rano's token table, but it is a ruling
  about the two heads, not a defect here.
- **Inline code inside a heading** now reads as code (cyan) rather than as
  heading text. That is the two passes composing; nano's markdown mode does
  the same, and it is what makes `` `letibot-tui` `` legible inside a heading.

### 11a. The inline pass parses one node at a time (a regression from 11)

Handing the inline grammar every node's ranges in one call is wrong, not just
slow. tree-sitter reads the ranges it is given as ONE concatenated stream, so
a backtick that cannot close inside its own node pairs with one in a later
paragraph. On the 33 KB parity document one stray `` ` ```console ` `` became
a 12 KB code span and painted everything from line 294 to the end of the file
cyan — 300-odd rows, no structure left.

Measured: 0 of the document's 440 inline ranges leak when parsed alone; the
combined parse leaks. Fixed by parsing per node, which is what
`tree-sitter-md`'s own `MarkdownParser` does and what the brief's §2.4
described as the reference — read there as a performance question, and it is
a correctness one. Cost, measured (release, 60 KB): 46 ms → 49–56 ms against
40 ms for a Rust file of the same size.

- [x] `markdown_inline_node_ranges` (grouped by node) drives the pass;
  `markdown_inline_ranges` (flattened) stays test-only.
- [x] Three tests, two of which fail on the combined parse.
- [ ] **Known rough edge, the grammar's and now confined to one node:** for a
  paragraph containing `` ` ```console ` `` the inline grammar reads the run
  more loosely than CommonMark, so a couple of cells in that paragraph take
  the literal colour (visible around lines 293/294/304 of the parity doc).
  Before the inline pass the region was plain; nano colours none of it either
  (its rule needs a backtick-free run). Fixing it means either a range check
  on the capture or dropping the inline pass for a node whose ranges contain
  adjacent backtick runs — not worth the complexity for two cells, but it is
  the one place the two heads' fence story (C4) is visibly inconsistent.

## 12. Typing latency on large files (measured 2026-09-22)

The concern was typing on heavy syntaxes, minified JS (one enormous line) and
200 MB+ files. Measured per keystroke, before → after, on this machine
(release, 120x40 viewport):

    file                        size    before      after
    7 MB Rust                   7.0 MiB  1452 ms      1.4 ms
    30 MB log (no syntax)      29.2 MiB   182 ms      0.57 ms
    4.8 MB single-line text     4.8 MiB    33 ms       33 ms
    4 MB minified JS (1 line)   3.9 MiB  1110 ms     1108 ms

- [x] **Capture walk was `#captures × log(#lines)`.** It binary-searched a
  multi-megabyte line-start array per capture. Captures arrive in document
  order, so a forward-only cursor replaces both searches. ~2× on its own.
- [x] **Highlight is per-viewport above 2 MiB** (`syntax::LARGE_BUFFER`):
  visible rows plus `editor::HIGHLIGHT_MARGIN` (200) either side, parsed as a
  slice, with the tree and style grid slice-relative. This is the change that
  took the 7 MB case from 1452 ms to 1.4 ms.
- [x] **Wrap table is incremental**: an edit that names its row re-measures
  that row and re-sums arithmetically. `wrap_dirty_row` carries the hint and
  the row count is re-checked before it is trusted. This took the 30 MB case
  from 182 ms to 0.57 ms.
- [x] `rano --export {html,ansi,markdown,text}` — the feature, and the
  instrument that isolates parse+colorise from draw.
- [x] `src/bench.rs` — the harness, so the numbers can be re-taken.


### 12a. Hundreds of megabytes, minified files, and what research said

Researched 2026-09-22, operator-instigated. Three findings changed the design:

1. **`ts_parser_set_included_ranges` parses a PORTION of a document "but still
   return a syntax tree whose ranges match up with the document as a whole"**
   (tree-sitter's *Advanced Parsing*). The tree keeps absolute offsets, so a
   partial parse needs no re-basing.
2. **Helix parses only the visible text** and has filed the consequence as a
   known trade (`helix#2285`: a definition off screen is not highlighted).
   Same family: `helix#3072` asks for exactly this on large files.
3. **VS Code goes further and skips tokenisation outright for long lines**
   (`editor.maxTokenizationLineLength`), and disables it wholesale for large
   files (`editor.largeFileOptimizations`).

The operator's framing is the design: **parsing does not imply colouring, and
for a huge file you colour only the visible part plus a margin.** A row-shaped
window is not enough, because a row can be megabytes.

What landed:

- [x] **The window is a CHAR range, not a row range** (`syntax::Window`: `rows`
  plus `cols` bounding the first and last row).
- [x] **Highlighting is lazy, once per FRAME** (`Editor::ensure_highlight`),
  so a burst of keystrokes costs one highlight, and a huge file's first frame
  is the viewport window rather than the document.
- [x] **Opening is windowed too** — `BufferState::new` no longer highlights.
  Measured (read + first highlight): 7 MB Rust **35 ms**, 30 MB log **58 ms**,
  193 MB **387 ms** (299 of it the read).
- [x] **`width::is_simple_prefix`** — the wrap table asked "is this row
  ordinary?" by scanning every character (193M at open). ASCII-and-not-a-tab
  answers conservatively: the 193 MB first highlight **1.1 s → 88 ms**.
- [x] **A quadratic in markdown's inline pass**, found by a test written in
  the same commit: it handed the parser the whole source per node, and
  tree-sitter walks the excluded input to reach each range, so 2,400
  paragraphs cost **9.8× for 4× the text**. Slicing each node's own bytes:
  **4.0×**, dead linear.

Measured after all of it (release, median of 3, edit point mid-document —
`bench_breakdown` prints exactly this):

    file                       rows      edit only   edit+highlight   of which
    minified20 (20.9 MB)          1       33.5 ms        41.2 ms       undo 17.0 ms
    minified.js (4.1 MB)          1        800 µs         6.0 ms       undo 239 µs
    oneline.txt (5.0 MB)          1        1.1 ms         2.7 ms       undo 242 µs
    big.log (30.6 MB)       400,000        264 µs         253 µs       re-sum 240 µs
    huge200.log (193 MB)  2,600,000        2.0 ms         1.8 ms       re-sum 1.7 ms
    big.rs (7.3 MB)         360,000        222 µs         1.1 ms       re-sum 209 µs

For scale, the same keystroke before any of this work: 7 MB Rust **1452 ms**,
4 MB minified JS **1108 ms**, 30 MB log **182 ms**.

## 13. Six costs left, to take one at a time

Six, and they are not independent: **13.6 (edit is a viewport) subsumes 13.1
(undo) and 13.4 (the read model)**, because all three are the same question —
what does the editor hold in memory, and what does it fetch. Deciding 13.6
first would settle the other two; deciding them first risks doing the work
twice. The rest are independent.

Three are on a keystroke, two on a read, one on a window's edges. They are
different problems on different shapes of file, so they are separate
decisions. Each has: what you see, the measurement, the mechanism with its
code location, the options and what they cost, and what would settle it.

### 13.1 The undo snapshot clones the row — twice per keystroke

**What you see.** Typing in a file that is one enormous line lags. Editing
any ordinary file is unaffected.

**Measurement.** 20.9 MB single-row file, one keystroke: **33.5 ms** of which
the before-snapshot is **17.0 ms** (measured by taking
`lines[r..r+1].to_vec()` directly, which is what `begin_action` does) and the
buffer's own splice is **2.5 ms**. The remaining ~14 ms is the same clone
again from `current_after` — same code shape, same row, *not* separately
instrumented, so treat it as inferred rather than measured. On the 4.1 MB
single-row file the before-clone alone is 239 µs of an 800 µs edit.

**Mechanism.** `UndoStep` stores whole rows:
`before: Vec<Vec<char>>`, `after: Vec<Vec<char>>` — *editor.rs:47-56*.
`begin_action` fills `before` with `bs.buf.lines[first..last].to_vec()`
(*editor.rs:810*); `finish_step` fills `after` via `current_after`, which does
`self.bs().buf.lines[after_start..end].to_vec()` (*editor.rs:901*). A
one-character insertion passes `first..last` = one row, so the "region" the
comment promises is a whole row — megabytes when the row is megabytes.

**Options.**

1. **Store a delta instead of rows.** `before`/`after` become
   `(row, char_range, text)` — the characters an edit actually touched. The
   undo application splices them back. This makes a keystroke O(typed text)
   rather than O(row), which is what it should be.
   *Cost:* the coalescing rules (`coalesce_kind`, `apply_coalesce`,
   *editor.rs:846-885*) are written against whole rows — runs of
   insert/backspace on one row merge by comparing row vectors. Coalescing a
   *run* of deltas into one is the real work: merge adjacent ranges, keep the
   earliest `before` slice and the latest `after` slice. Larger diff, but the
   invariants are local.
2. **Bound the snapshot by size**: keep full rows under a threshold and
   deltas above it. Two code paths for one invariant — the reason to prefer 1.
3. **Leave it.** 17 ms on a 20 MB single line; a 200 MB single line would be
   ~170 ms per keystroke, which is not typing.

**What would settle it.** Whether coalescing wants to be delta-shaped anyway.
It currently special-cases `prev.before.len() == 1 && prev.after.len() == 1`
(*editor.rs:851-856*) — i.e. "both steps touched one row" — which is a fuzzy
proxy for "these are adjacent edits". Deltas would say it exactly.

**Risk.** Undo/redo is the one place a bug loses a user's text. The undo tests
(`ed_tests`) cover coalescing, cut/paste and replace; they should be extended
with a property test — apply N random edits, undo them all, assert the buffer
is byte-identical — before this changes.

### 13.2 The wrap prefix is re-summed from the edited row down

**What you see.** Typing in a file with millions of lines costs a few
milliseconds per key. Nothing visible; it is the floor under everything else.

**Measurement.** 193 MB, 2.6M rows: **1.7 ms** of a 2.0 ms edit. 30.6 MB,
400k rows: **240 µs** of 264 µs. big.rs, 360k rows: **209 µs**. The cost
tracks the row count (6.5× rows → 7× time), not the bytes.

**Mechanism.** `wrap_prefix[r]` is the total visual-row count before buffer
row `r`, so an edit on row `k` invalidates every entry after `k`. The
incremental path re-measures row `k` and then re-sums the rest
(*editor.rs:383-388*): a loop over `row..n` doing one add each. Correct and
cheap per row; `n` is 2.6 million.

**Options.**

1. **A Fenwick tree (binary indexed tree) over per-row segment counts.**
   Point update on the edited row, prefix sum on read — O(log n) for both.
   The table is already rebuilt wholesale when rows are inserted or removed,
   so only the point-update path changes.
   *Cost:* ~40 lines and a new invariant; `wrap_prefix` is read by
   `buf_row_of_visual` (binary search), `seg_count`, and the renderer, so
   those would read through the tree instead of the array.
2. **Leave the array and pay O(rows).** 1.7 ms at 2.6M rows. A 2 GB file of
   short lines (~26M rows) would be ~17 ms per key — noticeable.
3. **Cap the table.** Above some row count, drop soft wrap entirely and
   scroll horizontally. Honest (nano has no wrap at 26M lines either) and
   much simpler than a Fenwick tree, but it takes the feature away at exactly
   the size where reading long lines matters most.

**What would settle it.** Whether a Fenwick tree is warranted for the file
sizes actually opened here. The largest log on this box is 193 MB (2.6M
rows, 1.7 ms); the point where it hurts is ~10× that.

**Risk.** Low, and contained: the wrap table is fully covered by tests
(`the_incremental_wrap_table_agrees_with_a_full_rebuild`,
`a_row_that_stops_wrapping_is_still_measured_once`, the `wrap_prefix_*`
tests), and any mistake shows as a wrong scroll position, not as data loss.

### 13.3 A window's edges are approximate, and `--export` is whole-document

**What you see.** Two different things, both consequences of the same design,
so one decision:

- A construct that opens *outside* the highlight window and closes *inside* is
  coloured as though it opened inside. A block comment started 300 rows above
  the viewport reads as code until the window catches up with a scroll.
  Measured by `a_window_can_be_part_of_a_single_enormous_row`, which prints the
  count rather than hiding it: **4 of 64 probe columns** differ at the window's
  32-column edges.
- `rano --export html huge.log` parses and walks the whole document. That is
  what an export *is*, so it is not a defect — but it means the export path
  still carries the O(document) cost the editor no longer has.

**Mechanism.** The window is `LARGE_BUFFER` = 2 MiB (*syntax.rs:745*) with
`HIGHLIGHT_MARGIN` = 200 rows, `COL_MARGIN` = 4,000 columns and
`LONG_ROW` = 8,000 (*editor.rs:583-599*). The margins are the whole mitigation
for the approximation: at 200 rows the edges are off screen at any realistic
terminal height. `syntax_errors` returns empty for a windowed highlight
(*syntax.rs*, documented), so tree-sitter diagnostics are LSP-only on big
files.

**Options.**

1. **Accept it, and say so in the UI.** The margins already make it invisible
   in use; a status-line note ("windowed highlight") would make it honest for
   someone who scrolls fast enough to see it.
2. **Seed the window from a real parse.** Parse the *first* window at open,
   keep that tree, and extend it incrementally as the viewport moves (edit the
   tree for the inserted text, re-parse with `set_included_ranges` only if the
   construct at the boundary actually changed). This is the correct fix, and
   it is the `Stream` design that already exists in `syntax.rs` — but see
   §12's note: a wrong `InputEdit` panics inside tree-sitter, so it depends on
   every buffer mutation going through `Buffer` first.
3. **Widen the margin adaptively**: keep extending the window backwards until
   the first row of the window parses with no `ERROR` node, bounded by a
   maximum. Cheap to try, no new invariant, and it fixes exactly the block
   comment case — but it costs a parse per extension, and a document whose
   first row is genuinely unparseable would extend to the cap every time.

**What would settle it.** Whether anyone actually sees the approximation. The
margin is 200 rows; a block comment longer than 400 rows with code visible
above and below it is the case. If that never happens in practice, option 1
plus a documented limit is the right cost.

**Risk.** Option 2 is the risky one and it is the one that would remove the
trade rather than hide it. It should not be attempted without the `InputEdit`
correctness work in §12 (every mutation funnelled through `Buffer`), because a
wrong edit description is a panic, not a wrong colour.

### 13.4 How a file gets read, and what it costs

**What you see.** Nothing, until a file is big enough that the memory matters
— and one hard limitation: **rano cannot open a file that is not valid
UTF-8.** A latin-1 log, a file with one stray byte, a binary: "cannot read
…: stream did not contain valid UTF-8", and that is the end of it.

**Measurement.** `Buffer::from_file` is four steps, and `bench_read_phases`
times each (release, median of 3):

| file | size | read + UTF-8 | CRLF scan | chars convert | total |
|---|---|---|---|---|---|
| big.rs | 7.0 MiB | 312 µs | 115 µs | 32.7 ms | 27.6 ms |
| big.log | 29.2 MiB | 2.4 ms | 551 µs | 75.9 ms | 81.5 ms |
| huge200.log | 184.2 MiB | 51.9 ms | 4.5 ms | **506.9 ms** | 568.5 ms |

And the memory, from `/proc/self/status` either side of the read:

| file | size | chars | RSS delta | bytes/char |
|---|---|---|---|---|
| big.rs | 7.0 MiB | 6,975,560 | 44.6 MiB | **6.71** |
| big.log | 29.2 MiB | 30,182,170 | 130.1 MiB | 4.52 |
| huge200.log | 184.2 MiB | 190,595,760 | **835.1 MiB** | 4.59 |

**Mechanism.** `Buffer::from_file`, *buffer.rs:36-52*:

1. `fs::read_to_string(path)` — the whole file into a `String`, validated as
   UTF-8. Reading the bytes is **11%** of the cost, and this is the line that
   refuses a non-UTF-8 file outright.
2. `text.contains("\r\n")` — a second full pass to remember the line ending.
   **1%.**
3. `text.lines().map(|l| l.chars().collect())` — every line becomes a
   `Vec<char>`. **89% of the read**: 507 ms of 568 ms at 184 MB.
4. Stored as `Vec<Vec<char>>`.

The memory is arithmetic, not a mystery: `size_of::<char>() == 4` and
`size_of::<Vec<char>>() == 24` per *line*. A row of `n` characters costs
`4n + 24 +` allocator overhead, so a one-byte-per-character file needs at
least 4× its size to be editable. The measured 6.71 bytes/char for `big.rs` is
the 24-byte per-line header dominating: those rows average 19 characters, so
the header is 56% of the row's cost. `big.log`'s 73-character rows pay 4.52.

**The architectural note.** The same text exists in three representations: it
is read as **bytes**, decoded to **chars** for the buffer, and then
*re-encoded* to **bytes** for the parser — `buf.text()` does
`l.iter().collect::<String>()` per line on every re-highlight (*buffer.rs:88*,
43 `chars().collect()` sites in the tree). That round trip is why `buf.text()`
appears at 24 ms for a 30 MB file in §12's table, and why a windowed highlight
still pays it.

**Options.**

1. **A per-line ASCII fast path: `Bytes(Vec<u8>) | Chars(Vec<char>)`.** 4×
   less memory on everything ASCII — which is nearly all source and logs —
   with indexing still O(1) behind a match, and the non-ASCII lines keep the
   current behaviour exactly. This mirrors `width::is_simple_prefix`, already
   in the tree for the same reason. *Cost:* every `line[i]` becomes a match:
   12 direct index sites and 8 iterator sites (*editor.rs*, *ui.rs*,
   *search*.rs, *syntax.rs*).
2. **`String` per line with a byte offset.** 1 byte per character, but a
   char-column lookup becomes O(col) — and the codebase indexes by **char
   column** everywhere (cursor, rows, ranges). This would slow the hot paths
   to save memory, which is the wrong trade for an editor.
3. **One `String` for the file, a line-start table, and a per-line char-offset
   index.** The most compact, and it makes `buf.text()` free — the parser
   would read the original bytes instead of a re-encoding. Biggest change:
   `&[Vec<char>]` is the shape the highlighter and the whole UI are written
   against.
4. **`String::from_utf8_lossy` instead of `read_to_string`.** Not a perf
   fix — it makes the non-UTF-8 refusal go away (replacement characters where
   the bytes were not text, which is what every other editor does). Small,
   separate, and arguably the more user-visible bug of the two.

**What would settle it.** Whether rano is expected to open files where 4.6×
the size does not fit in RAM. At 184 MB that is 835 MB resident, which is
fine; at 2 GB it is 9 GB, which is not. If "hundreds of megabytes" is the
ceiling, nothing here needs doing beyond option 4 — the honest answer is that
the `Vec<char>` model is a deliberate choice for O(1) char indexing and it
costs 4×.

**Risk.** Options 1–3 touch the buffer's core representation, which every
module reads: a mistake is a rendering or editing bug across the board, not a
local one. Option 4 is three lines and its risk is showing `U+FFFD` where a
file was not really text.

### 13.5 Encodings: rano opens UTF-8 or nothing

**What you see.** A file that is not valid UTF-8 cannot be opened at all:
"cannot read …: stream did not contain valid UTF-8", and nothing else happens.
No dialog, no lossy view, no way in. Verified on real bytes — of six common
shapes of the same small document, **three are refused**:

| file | bytes | what a detection ladder says | rano today |
|---|---|---|---|
| utf8.txt | 47 | UTF-8 (validates) | opens |
| utf8-bom.txt | 50 | UTF-8 (BOM) | opens, **BOM kept as U+FEFF** |
| utf16le-bom.txt | 86 | UTF-16LE (BOM) | **refused** |
| utf16be-bom.txt | 86 | UTF-16BE (BOM) | **refused** |
| latin1.txt | 42 | legacy → detection needed | **refused** |
| cp1252.txt | 42 | legacy → detection needed | **refused** |

The BOM row is a second, quieter bug: a UTF-8 BOM *validates*, so the file
opens — with `U+FEFF` as the first character of the buffer. It is zero-width
(so invisible) but it is a real column: `Home` goes to before it, a click on
the first visible character lands one column right of it, and a regex anchored
at `^` sees it. It is preserved on save, which is correct; it is not *handled*.

**Mechanism.** `fs::read_to_string(path)?` — *buffer.rs:37*. One line, and its
`InvalidData` error is the whole story: no BOM check before it, no fallback
after it. `Buffer` has `crlf: bool` (*buffer.rs:15*) but nothing for encoding.

**Detection needs a prefix, not the file** — worth stating because this section
first implied otherwise, which made encoding look like it forces an eager read.
Measured (`bench_detect_from_prefix`): deciding from the first 64 KiB costs
**8 µs on a 193 MB file against 46 ms to scan the whole thing**, and it agrees
with the whole-file answer on all seven shapes tried, from UTF-16-with-BOM to
latin-1 to CJK. The ladder's third rung is designed for this: `chardetng`'s own
docs say *"If you want to perform detection on just the prefix of a longer
stream, do not pass `last=true`"*, and its `feed` returns whether any non-ASCII
byte has been seen at all — which is the signal the first two rungs want
anyway. So the encoding step is a 64 KiB read, not a pass.

**The ladder that is standard.** Three rungs, cheapest first:

1. **BOM.** Definitive, six comparisons: `EF BB BF` UTF-8, `FF FE` UTF-16LE,
   `FE FF` UTF-16BE, `FF FE 00 00` UTF-32LE, `00 00 FE FF` UTF-32BE. A
   reference implementation is a dozen lines and no dependency.
2. **Valid UTF-8 → UTF-8.** The whole file validating as UTF-8 is
   overwhelming evidence for real text; this is what every editor does.
3. **Otherwise, guess.** This is the only rung that needs help, and the
   standard tooling is `chardetng` (Mozilla's detector, the one behind
   Firefox's "repair text encoding") paired with `encoding_rs` (the WHATWG
   decoders, same author — Henri Sivonen). `chardetng` is a statistical
   detector for legacy content; `encoding_rs` does the decoding, including
   the round-trip encoders.

**Options.**

1. **BOM ladder + strip-and-remember the BOM.** No dependency, ~40 lines.
   Fixes UTF-16 (which is entirely defined by its BOM in practice) and the
   U+FEFF column, and still refuses BOM-less legacy. Remembers the BOM so a
   save puts it back.
2. **Add `unicode-bom`-style handling plus a lossy fallback.** No dependency
   either: decode legacy bytes as **windows-1252** (the superset of latin-1
   that browsers assume) and replace the rest with `U+FFFD`. The file opens
   and the user is not confused — but the replacement characters are *in the
   buffer*, so a save writes them back and **silently corrupts the file**.
   That is the trap in this design, and it is why option 2 is not enough on its
   own: it needs `Buffer.encoding` set and a re-encode on save, at which point
   it is option 3 without the detector.
3. **The full ladder: BOM, then UTF-8, then `chardetng` + `encoding_rs`, with
   `Buffer.encoding` remembered and a re-encode on save.** Non-UTF-8 files
   open correctly *and* round-trip. Two new dependencies (the brief's
   no-new-deps rule was scoped to the `Stream` work, not to this), both
   well-established and small; `encoding_rs` is already in most trees
   indirectly.

**What would settle it.** Whether opening a latin-1 log is something the
operator wants. If yes, option 3; if "UTF-8 and UTF-16 only" is the answer,
option 1 is 40 lines and no dependency. The one thing that is not defensible
is the status quo, because the failure is silent-ish and total.

**Risk.** Option 1 is small and self-contained. Option 3's risk is the save
path: writing a file back in the wrong encoding corrupts it, so the encoding
has to travel with the buffer (like `crlf` already does) and the save path has
to honour it. `--export` would also need to decide what it emits (UTF-8 is
right for HTML/ANSI output).

### 13.6 Edit is a viewport: load a window, not the file

**What you see.** Nothing — until it is the reason memory runs out. And it is
the biggest single number in this document: **a 184 MB file costs 835 MB
resident**, because opening decodes all of it eagerly.

**Measurement.** `bench_read` and `bench_line_index`, release, median of 3:

| | 184 MB file |
|---|---|
| today: read + decode every line | **554 ms**, **835 MB RSS** |
| scan the bytes for newlines only | **36.8 ms**, **19.8 MB** |

The byte scan is 15× faster and the index is **42× smaller**. That is the whole
argument, and it is why the operator's framing is the right one: **edit is a
viewport, so load the minimum that makes the top of the file look right.**

**The sources agree, independently.** VS Code reached the same conclusion from
their own reimplementation (§13.7): "Finding and caching line breaks is much
faster than splitting the file into an array of strings." And their sharpest
lesson is about rano's existing hot path: they tuned `insert`/`delete`/`search`
and found "none of those optimizations mattered. The hottest method was
`getLineContent`" — invoked by the view *and the tokenizer*. In rano that is
`line_to_spans` and the capture walk, which is why §12 was about the highlight
and not the data structure. It also means any change here is judged by what it
does to line lookup, not to editing: making lookup worse to make editing better
would be trading the hot path for the cold one.

**Mechanism, and the one fact that makes it possible.** `Buffer` is
`lines: Vec<Vec<char>>` (*buffer.rs:11-12*) and `from_file` fills every row
before returning (*buffer.rs:40*). To decode only a window you first have to
know **which byte each row starts at** — and that can be answered *without
decoding at all*:

> In UTF-8, a `0x0A` byte is always a newline. Multi-byte sequences use only
> lead bytes `C2`–`F4` and continuation bytes `80`–`BF`, so no `0x0A` can occur
> inside one.

Verified over every codepoint (0 whose UTF-8 encoding contains `0x0A`, and the
same for `0x0D`), so a byte scan finds exactly the line breaks. That is what
`bench_line_index` measures: 36.8 ms and 19.8 MB for 184 MB of log, against
554 ms and 835 MB to decode it.

**Options, and they are a staging rather than a menu.**

1. **Read the file in pieces, index it in bytes, decode only the window.**
   A `u64` line-start table built from a positional read, then decode the
   visible rows into `Vec<char>` — *the same `Window` the highlighter already
   takes* (`.rows`/`.cols`), applied to loading instead of colouring. Opening
   a 184 MB file then costs ~20 MB and ~40 ms, and scrolling is bounded.
   **Editing needs an answer**, which is option 2.

   **Not `mmap`, and this is a correction.** I originally wrote "mmap + index",
   which is the obvious reading of the numbers and the wrong one. Sublime
   wrote a whole article about why, *Use mmap With Care*, after shipping it in
   Sublime Merge — four independent caveats, quoted in §13.7: any read can
   raise `SIGBUS` (a network mount dropping, another process truncating the
   file), so you need a signal handler, and `setjmp`/`longjmp` out of one is
   undefined behaviour "especially on MacOS" — `sigsetjmp`/`siglongjmp` plus
   `SA_NODEFER` and thread-local state; on Windows the mapping *locks the file
   against deletion* even with `FILE_SHARE_DELETE`, which is why Sublime
   releases mappings on idle; and signal handlers are global, so any library
   that installs its own (Breakpad) breaks the arrangement, with "no nice
   solution" and "only unsatisfying workarounds". Their own conclusion: "In
   hindsight it's difficult to justify using `mmap` over `pread`" — and their
   benchmark said `pread` was "around ⅔ as fast as `mmap`", i.e. the whole
   benefit for two-thirds the speed and none of the hazards. Their advice is
   exactly this option: "copy only the portions of the file that you require
   into memory".

   *Cost:* `Buffer` becomes an enum or gains a "not loaded" state for rows
   outside the materialised window; every `lines[r]` read has to be able to
   say "not here". That is the same shape of change as 13.4's ASCII fast path,
   and they would fight if done separately. It also wants a bounded cache with
   eviction, not just a window — scrolling backwards through 184 MB must not
   re-read from the start each time.
2. **Materialise on first edit.** The window is loaded; the moment a keystroke
   lands in a huge file, fall back to reading the whole thing as today. Simple,
   honest, and it fixes the case that actually happens — *reading* a 184 MB log
   — while leaving editing exactly as it is (835 MB, and it works).
   *Cost:* one `if` at the edit boundary, plus a pause the first time. The
   user is not confused, because nothing they can see is different.
3. **A piece table over the original file and an edit buffer.** The real
   answer: the file's bytes stay on disk (or mapped), edits append to an
   in-memory buffer, and the document is a list of nodes pointing at one or
   the other. **This is what VS Code did** and their numbers are the same
   shape as ours: their old line array used 600 MB for a 35 MB / 13.7M-line
   file — "roughly 20 times the initial file size", i.e. the same 4×-plus-
   metadata problem — and the piece tree brought memory "very close to the
   original file size". Their design adds three things ours would need too:
   per-node line-break caches, a red-black tree with subtree metadata for
   O(log n) lookup, and multiple buffers because a single buffer capped out.

   **And the part that argues against it** — the half of the VS Code post read
   second, recorded in §13.7: their own benchmark found "TA DA, we found the
   Achilles heel of piece tree. A large file, with 1000s of edits, will lead to
   thousands or tens of thousands of nodes … that is significantly more than
   `O(1)` which the line array enjoyed." Line lookup becomes `O(log n)` in the
   number of **edits**, where rano's array is `O(1)`. Their mitigation was to
   consider "a normalization step, where we would recreate buffers and nodes if
   certain conditions such as a high number of nodes are met" — a second
   mechanism to get back what the first gave up. They also warn that CRLF in a
   tree took "several attempts until I had a solution that was correct and
   fast", which rano avoids entirely with `crlf: bool`.

   **A second independent objection, from the author of xi-editor** (read in
   full for §13.7, and this is why the survey was worth doing): "many good
   editors have been written using piece tables, but I'm not a huge fan;
   performance is very good when first opening the file, but degrades over
   time." Two editors, independently, found the same shape of decay — and
   neither chose the piece table in the end. A rope is what xi, Helix and
   Lapce use.

   *Cost:* a rewrite of `Buffer` and the save path, plus a normalization pass,
   and it makes the hottest operation asymptotically worse by their own
   profiling. The undo model (13.1) sits on top of it, so those two want doing
   together or not at all.
4. **Just reduce the per-line overhead.** `Vec<Vec<char>>` pays 24 bytes of
   header *per line* — 56% of a 19-character row, which is what §13.4's 6.71
   bytes/char for `big.rs` is made of. A flat `Vec<char>` plus a line-start
   index removes it and is a much smaller change than 3. **But it does not
   touch the 4-bytes-per-char cost**, which is the one that says 184 MB →
   835 MB, so it is a 30% fix for the same amount of surgery. It is not a
   substitute for 1–3.

**What would settle it.** Whether the operator wants to *edit* a
hundreds-of-megabytes file, or only read one. Reading is the case that happens
(logs, dumps, generated files) and options 1+2 fix it completely for ~20 MB
and 40 ms. Editing a 184 MB file is the case that justifies a piece table, and
it is a rewrite. My reading of "the minimum that makes the user happy and not
confused" is options 1+2: the top of the file is right, scrolling works, typing
works, and the memory ceiling moves from "about a gigabyte" to "about the size
of what you are looking at".

**Risk.** Option 1 changes the shape every module reads (`&[Vec<char>]` is
what the UI, the highlighter and the search are written against), so it wants
doing deliberately and with the load-on-demand path covered by tests before
the eager path comes out. Option 3 subsumes 13.1 and 13.4 and should be
decided before either is implemented, or the work is done twice.

### 13.6a The survey: five containers, and where this section was wrong

Reading properly (§13.7) changed the recommendation here twice, so the evidence
is set out rather than summarised. xi-editor's author enumerates the options as
"contiguous string, gapped buffer, array of lines, piece table, and rope", and
that is the frame:

| container | memory | line lookup | search | non-local edit | who uses it |
|---|---|---|---|---|---|
| **array of lines** (rano today) | 4× + 24 B/line | **O(1)** | slice-local | O(row) | VS Code until 1.21 |
| contiguous string | ~1× | O(1) | slice | O(document) | fine "under a megabyte or so" |
| gap buffer | ~1×, "always in ideal state" | O(log n) via metrics tree | **7× faster than a rope** | O(n) gap move | Emacs, decades |
| piece table | ~1× initially | O(log n) in EDITS | poor | O(log n) | VS Code today |
| rope | ~1× ideal, worse after edits | O(log n) | **7× slower** | O(log n) | xi, Helix, Lapce |

What the sources say, in their own words:

- **rano's model is a named failure mode.** On the array of lines: "has
  performance failure modes, most notably very long lines." That is the
  minified-JS problem, diagnosed independently of our measurements.
- **The piece table is not the answer.** VS Code measured its own decay ("the
  Achilles heel of piece tree … thousands or tens of thousands of nodes") and
  xi's author says the same from the other side: "performance is very good when
  first opening the file, but degrades over time." §13.6's option 3 called it
  "the real answer"; on the evidence it is not.
- **The rope's cost lands exactly on rano's hot paths.** Line lookup is
  `O(log n)` instead of `O(1)`, and search over a rope is `7× slower` than over
  a contiguous buffer — "searching 1 GB text, the gap buffer runs in 35 ms,
  which is around 7x faster than the next fastest rope (~250 ms)", because the
  regex crate needs a slice. rano searches (`^W`, M-F, `search.rs`).
- **And the sharpest observation, which is rano's case exactly**: "the larger a
  file is the less likely I am to be editing it, but the more likely I am to be
  searching it."
- **A gap buffer is a serious contender** — ~1× memory, fastest search, no
  fragmentation — with O(n) gap moves (22 ms to move across 1 GB, ~100 ms to
  resize at 1 GB) as the price for non-local edits.
- **Both alternatives store UTF-8 bytes**, so the metrics tree gives `O(log n)`
  char indexing — meaning a rope or gap buffer **costs rano the `O(1)` char
  column indexing the UI, cursor and search are all built on.** That is the
  part the "just use a rope" reflex misses.

**Revised recommendation.** rano's hot operations are line lookup, char-column
indexing and search; its non-local edits (sort, replace-all) are rare, and big
files are read, not typed into. That set argues *against* a rope — it makes two
of the three hot operations asymptotically worse. So:

1. **For memory, the ASCII fast path (13.4 option 1) is the best value**: it
   keeps `O(1)` indexing, takes ASCII source and logs from 4 bytes/char to 1,
   and is ~20 call sites rather than a rewrite. It does not fix the 24-byte
   per-line header as cleanly, which is a smaller follow-on.
2. **A gap buffer is the alternative worth pricing properly** if the fast path
   proves insufficient — 1× memory and the fastest search, paid for with O(n)
   non-local edits and the loss of `O(1)` char indexing.
3. **Loading (this section) is a separate decision from representation** (13.4),
   and it is the one with the clearest win: positional reads and a byte index,
   measured at 15× faster and 42× smaller.

**What would settle it, honestly.** Not more reading. rano has `bench.rs`;
what is missing is the *operation mix* — how much of a frame is line lookup
versus search versus edit on real files. Measuring that on the current model
would say whether `O(1)` line lookup is worth defending, and the answer decides
between 1 and 2.

### 13.7 Sources, and what was actually read

The research behind §13 was done 2026-09-22. Recorded here because a finding
without an address cannot be checked, and because the distinction between
"read the page" and "saw a search snippet" decides how much weight a claim can
carry.

**Read in full** (fetched and read end to end):

- Tree-sitter, *Advanced Parsing* —
  <https://tree-sitter.github.io/tree-sitter/using-parsers/3-advanced-parsing.html>
  The `set_included_ranges` documentation quoted in §12a is from here: "create
  a syntax tree based on the text in certain *ranges* of a file", and the Go
  binding's phrasing "parse only a *portion* of a document but still return a
  syntax tree whose ranges match up with the document as a whole". Also the
  editing section (`ts_tree_edit` before re-parsing with the old tree), which
  is the API §13.3's option 2 would need.
- VS Code, *Text Buffer Reimplementation*, Peng Lyu, 2018-03-23 —
  <https://code.visualstudio.com/blogs/2018/03/23/text-buffer-reimplementation>
  The primary source for §13.6's option 3. Read in two sittings — the first
  200 lines, then the rest — and the second half changed what I recorded, see
  below.

- Sublime HQ, *Use mmap With Care* —
  <https://www.sublimetext.com/blog/articles/use-mmap-with-care>
  Read in full. The four caveats quoted in §13.6 option 1 (SIGBUS on a network
  drop or truncation; `setjmp`/`longjmp` from a handler being undefined
  behaviour "especially on MacOS"; Windows holding a lock that blocks deletion;
  signal handlers colliding with Breakpad) and the `pread` recommendation are
  from here. Published after they shipped `mmap` in Sublime Merge and "found
  it considerably more difficult than we had first thought".
- Raph Levien, *xi-editor retrospective* —
  <https://raphlinus.github.io/xi/2020/06/27/xi-retrospective.html>
  Read in full. The five-container taxonomy, "array of lines has performance
  failure modes, most notably very long lines", the piece-table objection
  ("degrades over time"), "my favorite aspect of the rope … is its excellent
  worst-case performance", and "in Rust, a rope is the sweet spot" are all from
  the *The rope* section. Note the author's stake: he wrote `xi-rope`, so his
  preference is not disinterested — the piece-table objection is corroborated
  by VS Code's own benchmark, which is why it carries.
- Troy Hinckley, *Text showdown: Gap Buffers vs Ropes* —
  <https://coredumped.dev/2023/08/09/text-showdown-gap-buffers-vs-ropes/>
  Read in full, and this is the only source with real numbers rather than
  prose: memory overhead per container, the 1 GB search figures (35 ms gap
  buffer against ~250 ms best rope), the 22 ms/100 ms gap-move and resize
  costs, and the conclusion "gap buffers are better for searching and memory
  usage, but ropes are better at non-local editing patterns". The author is
  reimplementing Emacs in Rust and says so, so read his gap-buffer result with
  that in mind; the benchmark repository is linked and reproducible.
- `ropey` crate metadata — <https://crates.io/api/v1/crates/ropey>
  **Read the head, not all 626 lines**: the crate record and version list, not
  every release entry. What it says: 1.6.1 is the newest stable (2023-10-18),
  2.0.0-beta.1 exists (2025-08-02), 11.87 M downloads total, MIT, 8,542 lines
  of Rust. Recorded because "use a rope" needs a crate that is actually
  maintained.

**Saw only as search snippets** (the claim is quoted from the snippet, not
verified against the page):

- VS Code large-file handling — <https://github.com/microsoft/vscode/issues/30243>
  ("over 30MB or over 300K lines will be considered a large file"), and
  <https://code.visualstudio.com/updates/v1_15> ("by disabling certain
  features for large files, for example tokenization, line guides, and
  wrapping or folding, we were able to optimize memory usage, in some cases,
  by as much as 50%").
- `editor.maxTokenizationLineLength` — the setting that skips tokenisation for
  long lines, and <https://github.com/microsoft/vscode/issues/240918>, that it
  is *not* honoured for some large files.
- Helix — <https://github.com/helix-editor/helix/issues/2285> (highlighting
  only works when the definition is on screen — the visible-only trade),
  <https://github.com/helix-editor/helix/issues/3072> ("very slow *editing* of
  large files when tree-sitter is used … like 2 seconds on 50K lines"), and
  <https://github.com/helix-editor/helix/issues/338> (disabling tree-sitter on
  >100 MB files; "tree-sitter/tree-sitter#222").
- `chardetng` — <https://docs.rs/chardetng/> and
  <https://docs.rs/chardetng/latest/chardetng/struct.EncodingDetector.html>.
  The comparative claims quoted in §13.5 ("more accurate than ICU, more
  complete than chardet, more explainable and modifiable than
  compact_enc_det") are the crate's own marketing, from its docs front page —
  not an independent benchmark.
- Editor memory comparisons —
  <https://github.com/levivilet/lvce-memory-benchmark> (note: not consulted
  for any claim above; recorded because it came back in the search and
  someone wanting numbers should start there).
- The rope-crate landscape — <https://crates.io/crates/crop> and
  <https://github.com/josephg/editing-traces>. crop's README compares itself,
  Jumprope and Ropey on editing traces and says "as of April 2023 there are (to
  my knowledge) 3 rope crates that are still actively maintained". Two years
  old and self-interested (it is crop's own README announcing crop as fastest),
  so it is recorded as a pointer, not relied on.
- Zed's rope and SumTree — <https://zed.dev/blog/zed-decoded-rope-sumtree>.
  Listed because Zed is the other modern Rust editor with a custom text
  structure, and §13.6a's table does not include it. Not read; its own
  large-file issue (<https://github.com/zed-industries/zed/issues/4701>, "Open
  a 4G file, it is stuck for a very long time, and the memory consumption is
  very high") came back in the same search and suggests a rope alone does not
  settle the hundreds-of-megabytes case.

**What the second half of the VS Code post added** (and one thing it took
away):

1. **The piece tree's real weakness is line lookup, not editing.** "TA DA, we
   found the Achilles heel of piece tree. A large file, with 1000s of edits,
   will lead to thousands or tens of thousands of nodes … that is
   significantly more than `O(1)` which the line array enjoyed." Line lookup
   is `O(log N)` where `N` is the number of *edits*, not the file size. So the
   piece table is not free for a reader; it trades memory for line-lookup
   speed, and rano's current line array is `O(1)`. That belongs in §13.6's
   option 3 and it was not in the first draft.
2. **"The most important lesson this reimplementation taught me is to always
   do real world profiling."** They tuned `insert`/`delete`/`search` and found
   "none of those optimizations mattered. The hottest method was
   `getLineContent`" — invoked by the view *and the tokenizer*. That is
   exactly rano's hot path, and exactly why §12 was about the highlight rather
   than the data structure.
3. **They name the trap rano is in.** "Our text model used to assume that the
   buffer is stored in an array and we frequently use `getLineContent` even
   though sometimes it wasn't necessary. For example, if we just want to know
   the character code of the first character of a line, we used a
   `getLineContent` first and then did `charCodeAt` … This is wasteful."
   rano's `buf.text()` per re-highlight is the same mistake at document scale:
   re-encoding every line to hand the parser a `String` — 24 ms at 30 MB
   (§12's table) — when the parser could read the bytes.
4. **CRLF in a tree is "a programmer's nightmare"**: "for every modification,
   we need to check if it splits a Carriage Return/Line Feed (CRLF) sequence,
   or if it creates a new CRLF sequence". rano sidesteps this entirely —
   `crlf: bool` records one decision for the whole file and the save path
   re-applies it. Worth keeping in mind before any piece-table work: this is
   the part that took them "several attempts".
5. **Their opening benchmark is the same claim as §13.6's**, independently:
   "Finding and caching line breaks is much faster than splitting the file
   into an array of strings." That is the byte scan versus decode, which is
   the measurement that makes §13.6 possible.

**Why this section kept changing.** §13.6's first draft recommended mmap and
called a piece table "the real answer"; both were written from one post read
three-quarters of the way through. The survey contradicts both — mmap for
reasons Sublime documented four of, the piece table for reasons two editors
measured independently. The lesson recorded here rather than privately: a
recommendation needs more than one source, and a source needs reading to the
end. Both were corrected in place rather than quietly dropped.

**One caveat on that post's numbers.** The memory, opening-time and
editing-time comparisons are *images* (`memoryusage.webp`, `fileopen.webp`,
`write.webp`, `read.webp`). The qualitative claims are in the prose and are
quoted above; the actual figures were not read, so no number from that post is
cited in this document. The one figure quoted — their old line array using
"around 600MB" for a 35 MB, 13.7-million-line file — is in the prose, and it
is the reason §13.6 says "some 20 times the initial file size".

## 14. Responsiveness: the event loop, and why not io_uring

Asked 2026-09-22: could `io_uring` get rid of the UI freezes? Researched, and
measured. The answer is no, and the reason is that the freezes are not what
`io_uring` addresses — but the measurements did find the real freeze, and it is
a wide one.

### 14.1 The freeze, measured

`bench_cold_open` — wall clock from "process started" to "first frame painted",
which is what `rano huge.log` makes you wait through:

| file | size | today (read on the UI thread) | with the read on a worker |
|---|---|---|---|
| big.rs | 7.0 MiB | 71.5 ms | **142 µs** |
| big.log | 29.2 MiB | 75.8 ms | **207 µs** |
| huge200.log | 184.2 MiB | **575 ms** | **261 µs** |

Nothing is on screen — no frame, no chrome, no running event loop, so not even
`^C` is handled — until the whole file has been read, decoded, indexed,
highlighted and drawn. At 184 MB that is **575 ms of a dead terminal**, and a
2 GB file would be ~6 s. With the read hoisted off the thread the window is up
(and the event loop live) in **261 µs**, a 2,200× reduction in time to first
useful pixel.

### 14.2 Why `io_uring` does not fix it

Three reasons, in decreasing order of decisiveness.

1. **It would not remove the freeze, only shorten it.** The freeze is
   "everything on the critical path is on the UI thread". `io_uring` is a
   different *mechanism* for the same blocking read — the thread still waits
   for the same bytes. `bench_cold_open` shows the read is not the binding cost
   anyway: at 184 MB, 51.9 ms is the read and **~500 ms is the decode** (§13.4).
   Making the read asynchronous while decoding 190M characters on the UI thread
   would move 575 ms to ~520 ms. The fix is a worker thread, whatever the syscall.

2. **Its benefit is concurrency, and rano issues one read.** `io_uring`'s win is
   submitting many operations without a syscall each and completing them out of
   order. rano's load is one file, read in sequence, then decodes. §13.6's
   loading design is *positional reads of the rows you are looking at* — a
   handful of sequential `pread`s per scroll, which a plain thread issues
   perfectly well. There is no queue to fill.

3. **It is a security liability, and this is Google's position, not folklore.**
   From §14.5: 60% of submissions to Google's Vulnerability Rewards Program
   were `io_uring` exploits, ~$1M paid out on them, and Google's own conclusion —
   *"we currently consider it safe only for use by trusted components"*. They
   disabled it in ChromeOS, in Android (seccomp-bpf), and on their production
   servers. A terminal editor is not a trusted component in Google's sense; it
   is the program a user points at a file somebody else wrote.

Also worth knowing before reaching for the crate ecosystem: the Rust wrappers
are not in good shape. `tokio-uring` is the obvious one and the forum thread
records "there haven't been many releases in recent years … changelog hasn't
been updated for any release since 2022"; `monoio` and `compio` are
thread-per-core *runtimes*, i.e. adopt a whole reactor to read one file. All
three are snippet-level claims (§14.5), and all three are moot given (1)–(3).

### 14.3 The fix, hand-rolled

**Corrected: see §14.6.** This section first claimed the fix was "one thread,
one channel", with a sketch that ended in `rx.recv()`. The operator's objection
was exact — *"you yourself telling me even ^C is blocked, so at least some
scheduling is a must here"* — and the sketch had the same bug one level down:
`recv()` moves the block off the read and onto the wait, leaving the loop and
`^C` just as dead. A thread is necessary and not sufficient.

What survives from here is the *pattern*, which is the tree's own (below), and
the four decisions the sketch needed; what changed is that they are
requirements of a scheduler rather than details of a thread. §14.6 states it.

No new dependency, and no new pattern either — **the tree already does this
twice**: `lsp.rs` spawns the handshake on a thread and adopts the client when it
lands (`lsp_starting: Option<(String, Receiver<...>)>`), and `exec.rs` drains
each pipe on its own reader thread and sends exactly one message per channel.
Both are polled, never waited on:

```rust
// `lsp_poll` — the shape the loader wants, rather than `recv()`
if let Some((tag, rx)) = self.bs_mut().lsp_starting.take() {
    match rx.try_recv() {                    // <- try_recv, so the loop runs
        Ok(Ok(client)) => { /* adopt, if not stale */ }
        Ok(Err(e))     => { /* flash the error */ }
        Err(TryRecvError::Empty) => { /* put it back, try next frame */ }
    }
}
```

What it needs beyond the sketch, stated because each is a decision:

- **A visible "loading" state**, not a blank screen. The operator's own words
  from §13.6 are the criterion — *the minimum that makes the user happy and not
  confused*. A blank frame is confusing; "reading huge.log…" is not.
- **The parse is the next cost to move.** Once the read is off-thread, the
  window's first highlight (71 ms at 184 MB) and the wrap table are on the
  frame's critical path. Same treatment: measure, then decide. §12's numbers
  say the *windowed* highlight is ~1 ms, so this may need nothing.
- **Ordering.** A read that lands after the user has started typing must not
  clobber the edit; the adoption needs to be refused if `modified` is set, the
  way a stale LSP handshake is refused by tag today.
- **Failure.** A read error must arrive as a message and become a status line,
  not an `eprintln!` from a thread nobody is reading.

None of this needs a runtime — but it does need **scheduling**, which is §14.6.

### 14.4 What `io_uring` would change, if anything ever did

Recorded so the question can be re-asked without redoing the research. Three
conditions would have to hold together, and none does today:

1. **Many concurrent operations**, not one file read sequentially — e.g. a
   project-wide search that reads thousands of files, or a reader that
   pipelines the next N windows while you scroll. rano has neither.
2. **The decode off the critical path anyway.** `io_uring` shortens the wait; it
   does not remove the work. Since ~90% of the load is decode (§13.4), the wait
   is not the problem.
3. **A wrapper that is not a whole runtime**, and a security posture where the
   kernel interface is acceptable. Neither is available today.

If a rewrite ever happens, the shape to reach for is a `io_uring`-style ring
*inside* the loader — submission/completion without a thread per file — which is
what §13.6's "bounded cache with eviction" would want if scrolling were
pipelined. That is a long way off, and it is not what fixes the freeze.

### 14.5 Sources for §14

- Google's restriction of `io_uring`, via Phoronix —
  <https://www.phoronix.com/news/Google-Restricting-IO_uring> — **read in full**.
  Source of the 60%, the ~$1M, "safe only for use by trusted components", and
  the ChromeOS/Android/GKE/production-server list.
- `io_uring`, Wikipedia — <https://en.wikipedia.org/wiki/Io_uring> — touched
  only for the kernel-side summary; the Google claims above are cited from
  Phoronix, not from here.
- Rust wrapper status — <https://users.rust-lang.org/t/status-of-tokio-uring/114481>
  and <https://zread.ai/bytedance/monoio/30-comparing-with-tokio-and-glmmio>
  — **search snippets only**, so the "no releases since 2022" and the
  runtime-versus-library distinction are recorded as leads to check, not as
  facts relied on. They are moot for the decision either way.
- The local facts, which are first-hand: `kernel.io_uring_disabled = 0` on this
  box (kernel 7.0.0-31-generic), so the interface is available — the objection
  is not that it would fail here, it is that it does not address the freeze.
- `bench_cold_open`, in `src/bench.rs`, is the measurement — reproducible, and
  it prints the empty-frame variant beside the blocking one so the comparison
  is visible rather than argued.

### 14.6 Scheduling is the requirement — a thread is not enough

The objection, and it is right: *"you yourself telling me even `^C` is blocked,
so at least some scheduling is a must here."* §14.3's first sketch ended in
`rx.recv()`. That moves the block from the read to the wait and leaves the loop
and `^C` exactly as dead as before — the same bug, one level down. What is
needed is not a worker but **a scheduler**, and the good news is that rano
already has one; the loader just has to join it.

**Two structural facts about the current code.**

1. **The loop does not exist during the load.** `main()` reads the file and
   *then* calls `run(buf, …)` (*main.rs:275-295*). So no amount of scheduling
   inside the loop helps until the open moves into it. This is the change.
2. **The loop is already a scheduler** — `ed.lsp_poll()`, `ed.lsp_flush()`,
   `ed.completion_retry_poll()`, `ed.exec_poll()` (*main.rs:428-432*) are all
   polled once per iteration and all return "did anything change", which is
   precisely the contract a loader wants. And the only blocking call in the
   whole loop is `event::poll(200 ms)` — the idle wait, which is where the
   loop *should* block, because a keystroke ends it.

**What the scheduler has to do, stated as four requirements** (each has a
measurement or a failure attached, so none is decorative):

1. **Run from the first iteration, with no buffer yet.** The editor holds a
   `Loading` state, draws immediately, and adopts rows as they arrive.
2. **Never wait on the job.** `try_recv()`, exactly as `lsp_poll` does — put the
   receiver back on `Empty` and try again next frame.
3. **Bound the work per iteration.** This is the requirement that a naive
   hand-rolled version gets wrong: `while let Ok(chunk) = rx.try_recv() { adopt }`
   is unbounded, and a worker faster than the loop turns adoption itself into
   the freeze. Adoption needs a budget — N rows or a time slice per frame —
   which is what makes the loop's worst-case iteration bounded rather than
   merely usually-short.
4. **Be cancellable, promptly.** One `AtomicBool` the worker checks per chunk.
   Set by `^C` (quit), by an edit (the user is typing; do not clobber), and by
   opening another file.

**And the payload should be chunks, not one buffer** — which is what turns
"responsive" into "content appears immediately". Measured, `bench_first_screen`:

| file | first screenful | bytes read | whole file |
|---|---|---|---|
| big.rs 7 MB | **59 µs** | 64 KiB | 27.7 ms |
| big.log 30 MB | **35 µs** | 64 KiB | 93.5 ms |
| huge200.log 193 MB | **37 µs** | 64 KiB | 516.6 ms |

**37 µs to the first screenful of a 193 MB file, against 516 ms for the whole
thing** — 14,000× — because the top of the file needs one chunk, not the file.
That is §13.6's "load the minimum that makes the user happy" as a number, and
it means the loading design and the window design are the same design: decode
what is on screen, fetch the rest as it is asked for.

Cancellation latency measured in the same test: **~5 ms**, bounded by the chunk
size and the check interval rather than by the file — a 193 MB read abandons in
5 ms because the flag is checked per 64 KiB chunk rather than once at the end.
A single `read_to_string` cannot be abandoned at all.

**Why this is still not a runtime.** The scheduler is: one loop that already
exists, one job state polled per iteration, one `AtomicBool`, one channel. What
`tokio` would add is a reactor, a work-stealing executor and a dependency tree
to move chunks across a boundary that rano already crosses four times per
iteration with `std::sync::mpsc`. The operator's "we will not use tokio, hand
roll" is not a purist preference here — it is the smaller design.

**What this changes about §14's earlier claim.** §14.1's "575 ms → 261 µs"
compared a blocking read against a thread that was *waited on*, so it overstated
what a worker alone buys. The honest numbers are:

| | today | thread, waited on | scheduled, chunked |
|---|---|---|---|
| first frame with chrome | 575 ms | 261 µs | **261 µs** |
| first text on screen | 575 ms | 575 ms | **37 µs** |
| `^C` works after | 575 ms | 575 ms | **first iteration** |
| cancels a 193 MB read in | n/a | never | **~5 ms** |

The middle column is why the objection was correct: a thread buys the frame and
nothing a user would notice.

## 15. Lazy loading: the design, prototyped

§13.6 concluded that loading a window is what the memory problem needs and left
the shape open. This is the shape, with the core prototyped and **verified
against the eager path on every row** rather than argued. The prototype is in
`src/bench.rs` (`bench_lazy_is_correct_and_cheap`, the `lazy` module) and it is
a test, not prose.

### 15.1 The two tiers

**Tier 1 — always resident: the index.** One pass over the file's *bytes*,
recording where each row starts and whether it is ASCII. No `char` is
constructed. `Vec<u64>` of starts plus one bit per row.

**Tier 2 — on demand: decoded rows.** A bounded cache of `Vec<char>` rows,
filled by positional reads (`pread`/`read_at` — in `std`, no dependency) and
evicted when it grows past a budget.

Measured, against the eager load of the same files:

| file | rows | index build | index memory | decode a window |
|---|---|---|---|---|
| big.rs 7 MB | 360,000 | 2.9 ms | 3.1 MB | 9 µs first / 5 µs deep |
| big.log 30 MB | 400,000 | 8.7 ms | 16 KB | 9 µs / 6 µs |
| **huge200.log 193 MB** | **2,600,000** | **54 ms** | **19.8 MB** | **13 µs / 6 µs** |
| cjk-check.md | 11 | 6 µs | — | 5 µs / 1 µs |

Against the eager path's **568 ms and 835 MB** for that 193 MB file: opening
costs 54 ms instead of 568 (10×), resident memory 19.8 MB instead of 835
(**42×**), and *any* window — first screen or the middle of the file — costs
~10 µs. "Deep" is as cheap as "first", which is what makes scrolling work.

**And it is correct**, which is the claim that had to be earned: all
**2,600,000 rows decode identical to `Buffer::from_file`'s**, compared row by
row in the test, and the one bug the prototype had was exactly here — a phantom
empty final row on every file, from mishandling a trailing newline. That is the
argument for prototyping before designing further.

### 15.2 The five facts it rests on

1. **In UTF-8, a `0x0A` byte is always a newline** — verified over every
   codepoint (§13.6). So the index needs no decoding.
2. **Char count per row is derivable from bytes for EVERY UTF-8 row**, not just
   ASCII ones: it is the number of bytes that are not continuation bytes
   (`b & 0xC0 != 0x80`). Verified: `chars_from_bytes == chars` for ASCII,
   Latin-1, Greek, Cyrillic, CJK, kana, Hangul and emoji alike.
3. **The property that matters is "every character is one column", not "the row
   is ASCII"** — see §15.6, which corrects this section. A Latin-1, Greek or
   Cyrillic row is non-ASCII and wraps *exactly* like ASCII; a CJK one does
   not. The per-row flag is therefore **narrow vs wide**, derived from the
   leading bytes: `0xC2..=0xDF` is always one column (all of U+0080..U+07FF is
   narrow), and only the 3- and 4-byte leads need the codepoint assembled —
   arithmetic on at most four bytes, no `char`, no allocation.
4. **Therefore wrap segments are known from bytes alone** for a narrow row:
   `ceil(char_count / view_w)`, exactly. Verified exact on all 2.6M rows of the
   193 MB log, and on 6 of the 11 rows of the CJK file — the other 5 being the
   wide ones, which are decoded to measure rather than guessed.
5. **Rows always start on a char boundary** (because `\n` cannot be inside a
   multi-byte sequence), so a positional read can decode any row without
   hunting for a boundary.

Fact 4 is the one that makes this more than "a cache of rows": it means the
**wrap table can be built from the index**, which is what scrolling, `M-\`, the
cursor and mouse hit-testing all read. Without it, lazy rows and the wrap table
would fight. And since fact 3 covers non-ASCII narrow text, that table is exact
for far more than source code.

### 15.3 What it costs, honestly

- **The API is the bulk of the work, not the loader.** 193 sites touch
  `Buffer::lines` and 60 index it. `lines` has to become private, with
  `row(r)` / `rows(a..b)` / `row_count()` replacing it, and the compiler finds
  the callers. That is mechanical but it is not small, and it is the reason this
  deserves to be staged rather than started.
- **O(document) operations want `materialize_all()`**, and there is no point
  pretending otherwise: `indent_unit` (scans every row for its indent),
  `sort_lines`, `justify`, replace-all, save, `--export`, and a full-buffer
  regex search. Those are O(document) *anyway*; materializing makes it explicit
  rather than accidental, and they are not what a person does to a 193 MB log.
- **An index build still reads the whole file** (54 ms for 193 MB). It need not:
  the index is append-only in the forward direction, so it can be built
  incrementally — index the first chunk, extend as you scroll, and opening
  costs one chunk (**37 µs**, §14.6) instead of 54 ms. That is the same
  "viewport" discipline applied one layer down, and it is cheap because the
  structure already grows only at the end.
- **Encodings bound it.** The byte-index trick *requires* UTF-8: in UTF-16 a newline is two bytes,
  and byte offsets are not char boundaries. So lazy loading applies to UTF-8
  and everything detected otherwise takes the eager path (§13.5). Worth stating
  because it makes 13.5 a prerequisite in a way it was not: today UTF-16 is
  refused, and after lazy loading it would be the one encoding that behaves
  differently.
- **Search must become a byte scan.** `search.rs` iterates `lines`, so a lazy
  buffer forces it to stream chunks and handle matches spanning a chunk
  boundary. That is *better* than the current in-memory scan (it is a scan over
  bytes, with no `Vec<char>`), but it is a rewrite of that module, not a tweak.
- **Eviction is required, not optional.** Without a cache budget, scrolling
  through the whole file re-materializes all 835 MB and the win is only the
  open. With one, the ceiling is the budget.
- **The file can change underneath.** Size and mtime are the cheap check; on a
  mismatch, reindex. `RANO_`-style refresh-on-focus would be the natural place.
- **The status line has to say so.** The operator's criterion from §13.6 is "not
  confused": `[12% indexed]` is honest, a blank screen is not.

### 15.4 What this subsumes

- **§13.4's memory problem, without touching the row representation.** The
  `Vec<char>` 4-bytes-per-char cost stops being a function of the file and
  becomes a function of the cache budget — a window of 40 rows costs 40 rows'
  worth of `char`s whatever the file's size. The ASCII fast path (§13.4 option
  1) is still worth having for the *index* and for small files, but it is no
  longer the thing that decides whether a 2 GB file is openable. That is a
  substantial simplification of 13.4's options.
- **§13.6's options 1 and 2**, which this is: option 1 done properly (with
  eviction and a real API) plus option 2 as the edit story.
- **Not §13.1 or §13.2.** The undo snapshot still clones the row it touched, and
  the wrap prefix still re-sums. Lazy loading changes *what is resident*, not
  how an edit is recorded. Those stay separate.

### 15.5 Staging, in the order the dependencies allow

1. **§14.6, the scheduled loader.** Independent of all of this, small, and it
   removes the freeze (575 ms → 37 µs to first text). Do it first because it is
   the part a user feels and it needs no API change.
2. **Chunked reads + `materialize_all` on first edit.** Lazy rows for reading,
   eager on editing. Fixes reading a 193 MB file completely, and "nothing the
   user can see is different" for editing. This is where the 42× memory win
   lands.
3. **The API change** (`lines` private, `row`/`rows`), driven by the compiler.
   Stage 2 wants this anyway to know what to materialize; doing it as its own
   step keeps the diff reviewable.
4. **Eviction, windowed search, incremental index.** The polish that makes the
   ceiling a budget rather than "whatever you scrolled through".
5. **A piece table, only if editing hundreds-of-megabytes files is a real goal.**
   It subsumes §13.1 and stage 3 of this, and §13.6a's survey is the argument
   against doing it for any other reason.

**What would settle stage 2 vs 5.** Whether the operator ever *edits* a file big
enough to need this, or only reads one. Logs, dumps and generated files are
read; that is the case stage 2 covers for ~20 MB and 54 ms, and it is the case
the measurements were taken on.

### 15.6 Two corrections, and the loop closed

Both from the operator, both measured, both changing the design.

**"ASCII is not that special."** True, and §15.2 as first written was built on
`is_ascii` per row, which was the wrong property. Two things are true that the
word "ASCII" was hiding:

- **Char count is derivable from bytes for every UTF-8 row** — it is the count
  of non-continuation bytes. Verified equal to the real char count for ASCII,
  Latin-1, Greek, Cyrillic, CJK, kana, Hangul and emoji.
- **The property that matters for wrapping is "one column per character", and
  Latin-1, Greek and Cyrillic satisfy it while being non-ASCII.** A Cyrillic
  row wraps *exactly* like an ASCII one. So the flag is **narrow vs wide**,
  decided from leading-byte ranges: `0xC2..=0xDF` is always one column, and
  only 3- and 4-byte leads need the codepoint assembled. `is_wide_cp` is now
  exposed from `width.rs` so the byte scan and the renderer cannot disagree
  about what is wide.

The prototype is corrected accordingly, and the numbers are unchanged where it
matters — **62 ms and 19.8 MB** for the 193 MB file (against 54 ms and 19.8 MB
for the ASCII-only version), because counting chars costs nothing for a row
whose count equals its byte length, so the counts are stored **sparsely** (only
for rows that are not pure ASCII — empty for every source file and log here).
What improved is coverage: char counts exact on all 2.6M rows, and segment
counts exact on all 2.6M *narrow* rows rather than declining every non-ASCII
one. The CJK file now reports 6 narrow of 11, where the ASCII-only version
claimed none.

**"You don't have to scan the whole file usually to detect encoding."** Also
true, and §13.5 implied otherwise. Measured: 8 µs for a 64 KiB prefix against
46 ms for the whole file, agreeing on every case tried. Amended there.

**What that closes.** With both corrections, **no step of opening a file needs
the whole file**:

| step | cost | scope |
|---|---|---|
| first frame (chrome, event loop live) | 261 µs | nothing |
| detect encoding from a 64 KiB prefix | 8 µs | one chunk |
| index the first chunk, extend as you scroll | ~37 µs | one chunk |
| decode the visible rows | ~10 µs | one window |
| full index (only if you must know the row count) | 62 ms | whole file |

The last row is the one to notice: it is the only O(file) step left, and it is
avoidable — the index grows only at the end, so it can be extended chunk by
chunk, and `[12% indexed]` in the status line is honest about the rest (§15.3).
That makes open genuinely O(chunk): read 64 KiB, sniff it, index it, draw it.

## 16. Implementation plan

The *how*, for the work §13–§15 identified. Phase 1 is worth shipping on its
own and is unblocked; nothing here waits on a decision.

Conventions, inherited from `PLAN.md`: run `cargo test`, `cargo fmt --all --
--check` and `cargo clippy --all-targets -- -D warnings` after every item; a
phase is done only when all three are green and the phase's own tests exist;
update the phase's state line as it lands.

### 16.0 Size does not influence editability

This section began with a question — *does rano need to edit a huge file, or
only read one?* — and the operator's answer removed it: **size does not
influence editability.** An editor does not get to declare a file too big to
edit, so there is no read-only mode to design for.

That answer invalidated the phase that followed it. "Materialise the whole file
on first edit" is a cliff *and* a lie: it advertises editing and then charges
835 MB the moment a character is typed. It is exactly the behaviour the
principle forbids. So the question below is not "may we avoid editing", it is
**which edit operations currently scale with the file, and what removes them.**

Measured (`bench_edit_scaling`), worst case — edit at row 0 of a 2.6M-row file:

| operation | 400k rows | 2.6M rows | scales with the file? |
|---|---|---|---|
| type a character | 0.1 µs | 0.0 µs | no — tracks the row |
| **insert a line** (Enter) | 154 µs | **1.5 ms** | **yes** |
| **delete a line** | 153 µs | **1.3 ms** | **yes** |
| **join lines** (Backspace at BOL) | 153 µs | **1.2 ms** | **yes** |
| undo's snapshot | 0.1 µs | 0.1 µs | no — tracks the row (but see §13.1: on ONE huge row it is O(row)) |

Typing is already size-independent. Every *line-structure* edit is not: it is a
`memmove` of 2.6M `Vec` headers, ~20 MB, per keystroke. At 2 GB that is ~15 ms
per Enter, and holding Enter queues them.

**And the fix is not a piece table.** Measured (`bench_chunked_fixes_the_scaling`):
a **chunked** row store — rows in chunks of 1,024 — makes the same insert
**0.2 µs, flat at every size**, while keeping line lookup O(1) (chunk index +
offset), which §13.6a's survey says the piece table gives up. The insert becomes
a memmove of one chunk instead of the whole file.

So Phase 3 is a *chunked, copy-on-write* row store, and Phase 5 (piece table)
drops off the plan entirely — not because editing huge files is out of scope,
but because this design serves it without §13.6a's two objections (lookup
O(log n), search 7× slower).


### 16.0a State: what has landed (2026-09-22)

| phase | state | evidence |
|---|---|---|
| 1 loader | **done** | content on screen 120 ms after launch on the 193 MB log (was 575 ms of blank); `^C` exits during the load; 20 tests |
| 2 encodings | **done** | all six shapes: UTF-16 opens, latin-1 and cp1252 open (were refused), the UTF-8 BOM is stripped rather than being a column; 12 tests plus loader/load_ctrl coverage |
| 3 store | **built and tested**, not yet wired | `src/rows.rs`: chunked + copy-on-write, 12 tests, exposed by the library target |
| 3 migration | **not started** | 192 `lines` call sites; see the warning below |
| 4 eviction/search/index | pending | needs 3 |

**The migration is the part that cannot be half-done.** `Buffer::lines` is a
public field read by every module, and a partly-migrated tree does not compile —
so it is one commit, and the plan's ordering (make it private behaviour-
preserving *first*, put the store behind it *second*) is what keeps that commit
reviewable.

**And it has one problem the store alone does not solve**, found while building
it and worth knowing before starting: several callers iterate every row
(`ensure_wrap_prefix`, `build_styles`, `search`, `export`), and with lazy decode
iteration needs `&mut` to decode — which is the whole point, but it means those
callers cannot simply be pointed at accessors. Each needs a decision:

- **the wrap table** should read the INDEX rather than the rows (byte lengths
  are known without decoding, and for a narrow row the char count is the byte
  count — §15 fact 4). This is the one that makes scrolling work at any size,
  and it is the reason the index exists.
- **the highlighter** already takes a window (§12), so it needs its rows
  materialised for that window only — which is what `ensure` is for.
- **search and export** are O(document) by nature; they call
  `materialize_all()` and say so, rather than pretending otherwise.

That is real design work, not a mechanical port, which is why this is stage 3
rather than part of the store.

### 16.1 Phase 1 — the scheduled loader (§14.6). Ship this alone.

**Deliverable.** `rano huge.log` puts a frame on screen and starts handling keys
within ~261 µs, shows text as soon as it has a screenful (**37 µs**), responds
to `^C` from the first iteration, and cancels an in-flight 184 MB read in
**~5 ms** when interrupted.

**Files.**

- `src/main.rs` — `main()` stops reading. `run()` takes a path, not a `Buffer`
  (*main.rs:295*, *353*). The size check and the `read_lines` flash move in.
- `src/loader.rs` (new) — `LoadJob`:
  ```
  pub struct LoadJob {
      rx: Receiver<Chunk>,           // Ok(rows) | Err(io::Error)
      cancel: Arc<AtomicBool>,       // set by ^C, by an edit, by a new open
      rows_read: usize,
  }
  impl LoadJob {
      pub fn spawn(path: PathBuf) -> io::Result<Self>;   // one thread
      pub fn poll(&mut self, budget: usize) -> Adopted;  // try_recv, bounded
      pub fn cancel(&self);
  }
  ```
  Chunks arrive already split into rows and already encoding-normalised, so the
  loop never sees raw bytes. 64 KiB reads, matching the measurement.
- `src/editor.rs` — a `Loading` state: the editor holds rows-so-far plus a flag,
  so `ui::draw` can paint a status line and the buffer's own rows.
- `src/main.rs` loop — `dirty |= ed.load_poll()` beside `lsp_poll`/`exec_poll`
  (*main.rs:428-432*), which is the contract those already satisfy.

**The four requirements from §14.6, each with its test:**

1. Frame before rows → `a_loading_editor_draws_a_frame_and_its_status`.
2. `try_recv`, never `recv` → `the_loop_iteration_never_blocks_on_the_loader`
   (poll a job whose worker is asleep).
3. **Bounded adoption** → `adoption_is_budgeted_so_a_fast_worker_cannot_stall_the_loop`
   (feed 10,000 chunks with a 100-row budget and assert the loop yields).
4. Prompt cancellation → `cancelling_stops_the_reader_within_a_chunk` (assert the
   worker sees the flag and exits; the 5 ms bound is in `bench_first_screen`).

Plus: `a_read_error_becomes_a_status_line`, `an_edit_during_the_load_is_not_clobbered`.

**Gate.** All three gates, and `bench_cold_open` shows the same or better
numbers. **Rollback** is trivial: the loader is additive until `main()` stops
reading, so the change is one call site.

**Size.** ~400 lines with tests, mostly new file. One `run()` signature change.

### 16.2 Phase 2 — encodings (§13.5). Unblocks Phase 3.

**Deliverable.** A UTF-16 file opens; a UTF-8 BOM is stripped and remembered
rather than becoming a column; a latin-1 file either opens (lossy, with
`encoding` recorded so a save round-trips) or is refused with a message that
says why.

**Files.**

- `src/buffer.rs` — `pub enum Encoding { Utf8, Utf8Bom, Utf16Le, Utf16Be,
  Latin1 }` beside `crlf` (*buffer.rs:15*); `from_file` does BOM sniff → prefix
  validate → (optional) detector; `file_text` re-encodes (*buffer.rs:104*).
- `src/syntax.rs` — `LARGE_BUFFER`'s windowed path already assumes UTF-8; the
  ladder is what makes that assumption checkable, so a non-UTF-8 file must take
  the eager path (documented at the call site).
- Optional dep: `encoding_rs` + `chardetng` for the third rung only.

**Tests.** The six-shape table from §13.5 as a test, not a table: UTF-8,
UTF-8+BOM, UTF-16LE/BE+BOM, latin-1, cp1252 → each either opens with the right
`Encoding` or refuses with the right message; plus a round-trip (read, write,
read) per encoding.

**Gate.** Three gates; `bench_detect_from_prefix` unchanged (8 µs).

**Size.** ~250 lines. The save path is the risk: a wrong re-encode corrupts a
file, so `Encoding` travels with the buffer exactly as `crlf` does.

### 16.3 Phase 3 — chunked, copy-on-write rows (§15 + §16.0). The big one.

**Deliverable.** A 193 MB file opens in **62 ms / 19.8 MB** instead of
568 ms / 835 MB; **every edit stays size-independent** (Enter 0.2 µs at 2.6M
rows, not 1.5 ms); every row renders identically to today (verified, not
asserted). No size threshold anywhere: there is no "too big" mode.

**The store, and why each piece is there** — both parts measured:

- **Chunked**: rows in chunks of 1,024, so a structural edit shifts one chunk
  (0.2 µs) instead of the file (1.5 ms), and lookup stays O(1). This is §16.0.
- **Copy-on-write rows**: a row is `FromFile { byte_range }` until it is edited,
  then `Owned(Vec<char>)`. Reading decodes from the file on demand and caches;
  editing promotes one row to owned. **This is what removes the cliff** — there
  is no materialise-on-edit step, because typing in a row is the same operation
  at every file size, and that is the whole point of §16.0.
- The prototype for each half is already written and verified
  (`bench_lazy_is_correct_and_cheap`, `bench_chunked_fixes_the_scaling`), so
  this phase is promotion, not research.

**Files.**

- `src/rows.rs` (new) — `RowStore`: the chunked index (`Vec<Chunk>`, each chunk
  a `Vec<Row>`), `row(r)`, `rows(a..b)`, `insert`, `remove`, the decode cache
  with eviction, and `materialize_all` for the O(document) callers. Both
  prototypes live here as their tests.
- `src/buffer.rs` — `lines` becomes private. `row(r) -> &[char]`,
  `rows(a..b) -> Rows<'_>`, `row_count()`, `materialize_all()`. **192 call
  sites** (`buffer.rs` 38, `editor.rs` 87, `syntax.rs` 31, `main.rs` 19,
  `ui.rs` 8, rest ≤4). The compiler is the checklist.
- **The borrow problem, which is the real API question.** Today `row(r)` can
  return `&[char]` because everything is materialised. With lazy decode there is
  nothing to borrow from until the row is decoded. Two ways out, and the plan
  picks the second:
  1. interior mutability (`RefCell`) so `row()` can fill the cache on access —
     convenient, but it puts a runtime borrow check on the hottest path;
  2. **prepare-then-read**: the frame asks the store to ensure rows `[a, b)` are
     resident, then borrows. That is the same discipline as §12's highlight
     window and §14.6's adoption budget, so it is a pattern the codebase already
     has rather than a new one. `row(r)` for an un-resident row returns a
     decoded-and-cached reference from the prepare step, and the O(document)
     callers call `materialize_all()` first.
- `src/editor.rs` — the wrap table reads the chunk index for narrow rows (§15
  fact 4), so it needs no decoding.

**Tests**, in the order that makes them useful:

1. `every_row_matches_the_eager_decode` — the prototype's check: decode all
   rows, compare to `Buffer::from_file`. This is the test that found the
   phantom-final-row bug, and it is the one that makes the store trustworthy.
2. `structural_edits_keep_the_numbering` — insert/remove at 0, mid, end; every
   row still reads back identical. Also from the prototype.
3. **`an_edit_is_the_same_cost_at_any_size`** — the §16.0 principle as a test:
   the same insert at row 0 of a 400k-row and a 2.6M-row buffer, asserted not to
   scale. This is the test that would fail on `Vec<Vec<char>>` today.
4. `wrap_segments_from_the_index_match_the_decoded_ones` — exact on narrow rows.
5. `eviction_bounds_resident_rows` — scroll the whole file, assert the budget.
6. `materialize_all_is_idempotent_and_leaves_the_file_readable`.
7. `a_multibyte_row_decodes_from_a_positional_read` — the boundary case.

**Gate.** Three gates; the existing 313 tests pass untouched except where they
poke `lines`; `bench_edit_scaling` and `bench_lazy` both show flat behaviour.
Behaviour parity is the bar.

**Size.** ~1,200 lines plus ~200 mechanical call sites. **Staged as two
commits**, and the order matters: first make `lines` private and fix the
callers mechanically (behaviour-preserving, provably — the existing tests are
the proof), *then* put the chunked copy-on-write store behind the new API.
A half-migrated tree will not compile, so this is all-or-nothing per commit.

**Risk.** The API change touches every module. Mitigated by the two-commit
order, and by the fact that the first commit changes no behaviour at all.

### 16.4 Phase 4 — eviction, windowed search, incremental index (§15.3)

**Deliverable.** Scrolling far and back is cheap; `^W` works on a file too big
to hold; opening costs one chunk rather than one pass.

**Files.** `src/rows.rs` (cache policy), `src/search.rs` (rewrite to a chunked
byte scan — currently 1 `.lines` site, but it becomes a scan with match
crossover handling), `src/editor.rs` (extend the index as scrolling discovers
more).

**Tests.** `search_finds_a_match_spanning_a_chunk_boundary`;
`scrolling_back_and_forth_does_not_regrow_the_cache`;
`the_index_extends_without_rereading`.

**Size.** ~400 lines. **Risk.** Search is the module whose semantics are easiest
to get subtly wrong (regex over chunk boundaries); it wants a differential test
against the current whole-buffer implementation on the existing search tests.

### 16.5 The piece table: dropped from the plan

§16.0 removed the question that Phase 5 existed to answer. A piece table's win
is O(log n) structural edits *and* bounded memory; §16.3 gets **both** — 0.2 µs
inserts and file-backed rows — while keeping line lookup **O(1)** and search on
contiguous memory, which are the two things §13.6a's survey found a piece table
(or a rope) gives up. So the honest conclusion is not "not yet"; it is that the
shape it was being considered for is served better by chunking the array we
already have.

Recorded rather than deleted, because the reasoning is the valuable part: two
editors independently measured a piece table's decay, and a third
(xi) argued for a rope on worst-case grounds — but both of those comparisons
were against an *unchunked* line array. Chunking the array removes the decay
those sources describe. The survey still matters: it is the argument against
reaching for a rope for any other reason.

**If it ever returns**, the trigger is a measurement, not an opinion: a
structural-edit cost that chunking does not remove, or a row count where the
chunk index itself becomes the memmove (the crossover is around 10^8 rows, at
which point the file is ~4 GB of one-line rows and the problem is different).
### 16.6 Off the critical path

Independent, each small, none blocking the phases above:

- **§13.1 undo deltas** — 17 ms per keystroke on a 20 MB single row. Wants
  Phase 3's API anyway (the row is the thing being cloned), so doing it after
  Phase 3 is cheaper than before. Its own risk: undo bugs lose text, so a
  property test (N random edits, undo all, compare) precedes it.
- **§13.2 wrap-prefix Fenwick tree** — 1.7 ms per keystroke at 2.6M rows.
  Self-contained, well-tested area, low risk. Can go any time.
- **§13.3 window edges** — accept and document, or seed from a real tree. Not
  urgent; the margin already makes it invisible in use.

### 16.7 Order, and what ships when

| # | phase | ships | size | blocked by |
|---|---|---|---|---|
| 1 | scheduled loader | frozen `rano huge.log` fixed | ~400 | nothing |
| 2 | encodings | UTF-16 and latin-1 open | ~250 | nothing |
| 3 | chunked, copy-on-write rows | 193 MB in 62 ms / 19.8 MB, and edits flat at any size | ~1400 | 2 |
| 4 | eviction, search, incremental index | ceiling is a budget | ~400 | 3 |
| — | *(piece table)* | *dropped — §16.5* | — | — |

Phase 1 is worth shipping even if nothing else follows: it is small, additive,
and it is the change a user notices.

### 16.8 Risks, and what makes each one survivable

- **The `lines` API change is wide.** Mitigated by ordering (compiler-driven,
  behaviour-preserving commit first) and by every existing test continuing to
  pass.
- **The save path is where an encoding bug loses data.** Mitigated by
  `Encoding` on the buffer beside `crlf`, and a round-trip test per encoding
  before anything else uses it.
- **Undo is where a bug loses text.** Mitigated by keeping §13.1 off the
  critical path and putting a property test in front of it.
- **Search semantics over chunks.** Mitigated by differential testing against
  the current implementation, which stays until the new one agrees.
- **Lazy loading makes every frame depend on eviction.** A cache miss during a
  frame is a disk read; if the budget is small relative to the viewport, that is
  a stall. Mitigated by deriving the budget from `text_h` (a viewport plus
  margin, the same number §12 already uses) rather than a constant, and by
  measuring worst-case frame time with a deliberately tiny cache.
- **The chunked store's chunk size is a real constant to choose.** Too small and
  the chunk index becomes the memmove; too large and an insert shifts too much.
  1,024 rows measured 0.2 µs for an insert at row 0 — flat across 360k, 400k and
  2.6M rows — so the choice is not delicate, but it should be asserted rather
  than assumed: `an_edit_is_the_same_cost_at_any_size` is the test that catches
  it regressing.
- **Copy-on-write rows make two representations legal at once.** Every reader
  must work for both, or a file that has been edited behaves differently from
  one that has not. Mitigated by the row API returning `&[char]` either way —
  the store hides which case it is — and by test 1 decoding *after* edits as
  well as before.

### 16.0b Opening at a position, and a quadratic load

Added 2026-09-22, on the operator's request for `--line`/`--column`.

**The command line is parsed properly now.** `-l/--line N`, `-c/--column N`,
`=value` forms, `--export`, `--help`, `--version`, and a real parser instead of
`args.first()`. That positional form had a latent bug: `rano --line 42 main.rs`
would have made `"42"` the file. Bad input is refused with a message and exit 2
rather than guessed — a missing value, a non-number, a negative line, an unknown
option, two files, an unknown export format.

**A position is applied when its row arrives**, not at startup. With the loader a
row a million lines in exists long after the first frame, so clamping to what
had arrived would open the file at the wrong place and look like the feature was
broken. Verified on the 193 MB log: `--line 1000000` waits, then centres on
`Ln 1000000`.

**Centring is in VISUAL rows**, because with soft wrap a long line above the
target occupies several — using the buffer row would put the view off by exactly
that. And it cannot invent blank space: centring the last line of a 500-line file
in a 20-row viewport clamps to the end, which is the correct answer rather than a
failure.

**And making it work found a quadratic load**, which is the part worth keeping.
Every adopted batch bumped `edit_gen`, invalidating the wrap table, and the next
frame rebuilt it over EVERY row so far — ~800M row measurements across a 2.6M-line
file. Diagnosed by measuring rather than guessing: the file is page-cached at
**17 GB/s** and Python decodes it in 0.66 s, so the ~8 s it was taking was
obviously not I/O. The table now EXTENDS from the join for an append
(`BufferState::wrap_extend_from`, set by the loader, which only ever appends),
with `the_extended_wrap_table_agrees_with_a_full_rebuild` proving extend ≡
rebuild across eight batches — a wrong prefix is wrong scrolling, so that test
matters more than the speed.

    before:  315k rows/s   (sleep-paced, and rebuilding the table per batch)
    after:   5.6M rows/s   (184 MB / 2.6M rows in 461 ms, 419 MB/s through the
                            loop, against 42 ms for a bare read — 10.9x the read)

Two other fixes fell out: the loop's 8 ms idle wait was PACING the load (a
saturated loader now waits 0, so the budget bounds one iteration rather than the
throughput), and `bench_load_throughput` exists so this cannot regress unnoticed.
