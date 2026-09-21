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
  no injection grammar runs, so a ```rust block is cyan, not Rust), raw HTML
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
