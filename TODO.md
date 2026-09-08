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
