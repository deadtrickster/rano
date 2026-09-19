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
