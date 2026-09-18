# rano — improvement TODO

Fresh audit of the tree (2026-09, post "PROJECT COMPLETE"). `TODO.md` is the
finished historical list; this file is the new work, ordered by risk.
Line refs are from the current files. Everything below was read in the source,
not inferred from the docs — where the docs disagree with the code, that is
called out in §7.

Gates to keep green while working: `cargo test`, `cargo clippy --all-targets -- -D warnings`,
`cargo fmt --all -- --check`.

---

## 1. P0 — crashes and data loss

- [ ] **Title bar underflows `usize` on a long file name in a narrow terminal.**
  `ui.rs:482`: `let mut pos = (TITLE_LEFT.len() + region_end - nl) / 2;` with
  `region_end = width - 8`. When the name is longer than `width + 4` chars the
  subtraction wraps → panic in debug, out-of-bounds `s[pos + i]` in release.
  Repro: `rano <90-char path>` in a 40-column terminal. Fix: compute in `i64`
  or `saturating_sub`, and clamp `pos` to `s.len()`. Add a `title_line` test at
  widths 10/20/40 with a 60-char name (ui.rs:662 only tests width 80).
- [ ] **Completion popup underflows when a label doesn't fit the popup width.**
  `ui.rs:307`: `let pad = w - 2 - label.chars().count() - kind_tag(it.kind).len();`.
  `w` is clamped to the terminal width (`ui.rs:281`) but `label` is truncated to
  `label_w` (max label length, capped at 40), so `w < 2 + label + tag` is
  reachable — e.g. a 20-column terminal with a 20-char `const` item
  (`kind_tag` = 5 chars, `ui.rs:63`). Panic in debug, `" ".repeat(huge)` abort in
  release. Fix: `let pad = w.saturating_sub(2 + label_w_chars + tag_len);`.
  Add a TestBackend draw test with a long label + narrow backend.
- [ ] **Terminal is left in raw mode / alt screen when the event loop errors.**
  `main.rs:235-272` uses `?` on `terminal.size()`, `terminal.draw()`,
  `event::poll()`, `event::read()` inside the loop, so an error returns from
  `run()` and skips the restore at `main.rs:273-279`. The panic hook
  (`main.rs:212-217`) only covers panics. Fix: run the loop in an inner closure,
  capture the result, always restore, then return the result. Also make the
  restore itself infallible (`let _ =` on `disable_raw_mode()` before the
  `execute!`, so one failure can't skip the other).
- [ ] **Saves are not atomic — README claims they are.**
  `editor.rs:1540` is a plain `fs::write(&path, text)`; a crash/full disk mid
  write truncates the user's file. `TODO.md:12` is ticked and `README.md:126`
  promises "`file.tmp` + rename", but the refactor lost it. Fix: write
  `<file>.tmp` (or `.rano-tmp`), `flush`, copy the existing file's mode,
  `fs::rename`. Keep `buf.name`/`modified` updates after the rename succeeds.
  Add a test that saves twice over an existing file and asserts no `.tmp` is
  left behind and the mode is preserved.
- [ ] **A second `^T` orphans the first command's process.**
  `exec_ctrl.rs:29` overwrites `bs.exec_job` with the new job; the old `ExecJob`
  is dropped, and `Child` does not kill on drop → the child keeps running with
  its output silently discarded. Same at quit: a running job is dropped, not
  killed. Fix: `kill()` + `wait()` the previous job before replacing, and kill
  any running job on quit; or queue jobs.
- [ ] **No timeout on external commands.** `exec.rs` / `exec_ctrl.rs` wait
  forever; `^T sleep 1000` leaves "Running:" indefinitely and `M-|` freezes the
  whole UI (see next item). Add a configurable deadline (default e.g. 30 s) that
  kills the child and flashes.

## 2. P1 — correctness

- [ ] **Replace ignores the regex / case toggles that search honours.**
  `search_ctrl.rs:168` and `:201`/`:267` go through `Buffer::find_next` /
  `find_all` (literal, case-sensitive), while `do_search` (`search_ctrl.rs:68`)
  builds a `search::Matcher`. So `M-R` + regex search finds matches that replace
  cannot. Fix: drive replace from the same `Matcher` (needs a
  `Matcher::replace_all` that returns `(Pos, len, replacement)`), and make
  `replace_all_from` work on multi-char `find` lengths from the matcher.
- [ ] **Replace never terminates: it wraps silently and re-asks forever.**
  `Buffer::find_next` (`buffer.rs:244-250`) wraps to the first match, so
  answering `n` at the last match jumps back to the first one and the y/n/a/q
  loop never ends. Fix: remember the first-asked position, stop (and flash
  "Replaced N occurrences") when it comes back around; optionally flash
  "reached buffer top, wrapping" once, like nano.
- [ ] **`quit_after_save` leaks across a cancelled prompt.**
  `editor.rs:1706-1718`: answering `y` to "Save modified buffer?" on an unnamed
  buffer sets `quit_after_save = true` and opens the File Name prompt; pressing
  `Esc` there (`prompt.rs:99`, `:322-329`) drops the prompt but leaves the flag
  set — the next unrelated `^O` quits the editor. Same via
  `ConfirmOverwrite` → `n`. Fix: clear `quit_after_save` (and `pending_write`)
  whenever the WriteName/ConfirmOverwrite prompt is cancelled or answered "no".
- [ ] **Completion insert can inject a raw `\n` char into a line.**
  `editor.rs:528-542` (`insert_str_plain`) inserts every char via
  `Buffer::insert_char`, which has no newline case, so an LSP `insertText` /
  flattened snippet containing `\n` (`lsp.rs:298 strip_snippet` keeps bodies)
  puts a literal control char inside `Vec<char>` and breaks rendering and
  column math. Fix: route through the same path as `paste_text`
  (`editor.rs:1187`) — split on `\n`, merge head, insert the rest as rows.
- [ ] **`uri_to_path` does not percent-decode.** `lsp.rs:286-294` only strips
  `file://` (documented as "does not percent-encode" because `path_to_uri`
  doesn't encode) — but *server*-sent URIs are encoded, so jump-to-definition
  into `/home/me/my file.rs` opens `/home/me/my%20file.rs`. Fix: decode `%XX`
  (and `+`? no — only `%XX`) in `uri_to_path`; keep `path_to_uri` encoding too
  for symmetry. Test with a space and a non-ASCII path.
- [ ] **`^R` (read file) ignores CRLF.** `editor.rs:1589-1595` uses
  `text.lines()`, which strips `\r`, and never touches `buf.crlf`, so reading a
  CRLF file into an LF buffer silently normalises the inserted lines (and the
  reverse mixes EOLs). Fix: detect per-insert and either reject mixed endings or
  keep the buffer's flag and re-attach `\r` on save (document the choice).
- [ ] **Non-UTF-8 files are unreadable, and fatal at startup.**
  `buffer.rs:29` `fs::read_to_string` → `main.rs:193-196` prints and `exit(1)`.
  Fix: `fs::read` + lossy-decode with a "not valid UTF-8, edited lossily" flash
  and a flag that disables save-without-rename (or refuse with a clear message
  instead of a stack of bytes).
- [ ] **No check that the file changed on disk.** `save_to` overwrites blindly;
  `from_file` records no mtime/size. Fix: store the mtime+size at load/save, and
  on save (and on `^R`) warn + confirm when the on-disk file moved.
- [ ] **`justify` measures chars, not display columns, and drops indentation.**
  `editor.rs:1760-1803`: `cur.len()` vs a display-column `width`, tabs count as
  1, and the paragraph's leading whitespace is flattened by `split_whitespace`.
  Fix: use `ui::display_width`, preserve the first line's indent (nano's
  behaviour), and don't justify inside code blocks (or gate on a config flag).
- [ ] **`sort_lines` sorts whole rows for a partial selection.**
  `editor.rs:1806-1812` takes `a.row..=b.row` regardless of columns; the
  unselected tail of the last row moves. Fix: sort only fully covered rows, or
  operate on the selected text and re-splice. Add `reverse`/`unique` variants
  later.
- [ ] **`match_bracket` handles 3 pair types and ignores strings/comments.**
  `editor.rs:1397-1427` (`[('(',')'),('{','}'),('[',']')]`, 5-char lookahead,
  full-buffer scan in `find_matching`). Fix: add `<`/`>`? (no — better: use the
  tree-sitter parse to skip string/comment nodes), and prefer the innermost
  enclosing pair when the cursor is not on a bracket.
- [ ] **`do_goto` accepts digits from anywhere.** `editor.rs:1496` filters all
  digits, so `Go To Line: 1,2` → 12 and `L5` → 5. Fix: parse a trimmed
  `[0-9]+` (accept `row,col` explicitly if wanted), flash on garbage.
- [ ] **Completion request queue can desync.** `editor.rs:599-611` pops one
  entry per response; a dropped/never-answered request leaves a stale front
  entry that mis-attributes the next response. `completion_retries`
  (`editor.rs:618`) only resets on a good response or popup close, so after 40
  parks the junk filter is off until the popup closes. Fix: key the queue by
  LSP request id (the client already tracks `completion_ids`, `lsp.rs:77`), and
  decay the retry counter when the context changes.
- [ ] **`M-<key>` depends on the terminal's ALT flag.** `keys.rs:65-94` only
  matches `KeyModifiers::ALT`; terminals that send `ESC` `u` as two events will
  clear the mark (`keys.rs:140`) and then type `u`. Fix: buffer a lone `Esc`
  with a short timeout and re-dispatch it as a Meta binding (crossterm exposes
  the raw keyCode; kitty-protocol terminals can also be requested).

## 3. P2 — performance

- [ ] **`all_diags()` is cloned + sorted once per rendered character.**
  `editor.rs:280-286` clones and sorts the merged diagnostic list; `char_style`
  calls it for every cell (`editor.rs:1841`) and the gutter loop calls it once
  per visible row (`ui.rs:227`). A 2000-cell frame with 100 diagnostics does
  ~2000 clones + sorts. Fix: build the merged, sorted list once per draw (or
  cache it in `BufferState` with a dirty flag) and pass a slice/`&[Diagnostic]`
  into `char_style`; index by row with a `BTreeMap<usize, _>` or a
  partition-point lookup.
- [ ] **Every keystroke re-parses the whole file and rebuilds the whole style
  grid.** `edit_invalidate` (`editor.rs:268-276`) → `hl.refresh`
  (`syntax.rs:112-161`) → `Buffer::text()` (full-string alloc, `buffer.rs:60`) +
  full `parser.parse` + a `Vec<Vec<Style>>` the size of the buffer. The comment
  at `syntax.rs:126` rules out incremental parsing because a reused tree panicked
  — the actual fix is to call `Tree::edit(Input::Edit{…})` with the byte range of
  each edit before `parse(&new_source, Some(&old_tree))`. Do that, and keep the
  style grid per-line so only touched lines are rebuilt.
- [ ] **`indent_unit()` scans the entire buffer on every Backspace and Newline.**
  `editor.rs:291-315` (called at `editor.rs:971` and `:1021`), including a sort
  and a GCD over every indented line. Fix: compute once per file open / language
  change, cache on `BufferState`, invalidate on full-buffer ops.
- [ ] **`didChange` sends the whole document.** `lsp.rs:430-447` +
  `lsp_ctrl.rs:28` (`l.change(&bs.buf.text())`) — full text every 300 ms while
  typing, plus a full text alloc per flush. Fix: honour the server's
  `syncKind` and send incremental ranges (rano already knows the edit regions
  from its undo steps).
- [ ] **`LspClient::shutdown()` runs on the UI thread.** `lsp_ctrl.rs:61-63`
  calls it synchronously; it waits up to 2 s for the shutdown response
  (`lsp.rs:661-669`). Switching buffers / F8 / definition jumps across file types
  can freeze the UI for seconds. Fix: move the old client to the background
  thread and shut it down there.
- [ ] **`M-.` blocks the UI for up to 3 s.** `editor.rs:767-772` uses the
  synchronous `definition()` with a 3 s timeout. Fix: make it fire-and-forget
  like completion (route the response through `LspEvent`) with a "Finding
  definition…" status.
- [ ] **`M-|` filter blocks the event loop.** `exec_ctrl.rs:129`
  `child.wait_with_output()` — a slow `sort`/`clang-format` on a big region
  stops drawing and ignores keys. Fix: reuse the `ExecJob` async machinery
  (reader threads + `exec_poll`) for filter; keep the "no edit on failure" rule.
- [ ] **`raw_log` opens the log file for every message.** `lsp.rs:162-172`
  reads the env var and opens/appends per message. Fix: resolve once, keep an
  `Option<File>` on the client (or a `thread_local` writer).
- [ ] **`Buffer::find_all` is O(lines × len × needle) and `find_next` scans the
  whole buffer.** `buffer.rs:225-250`. Fix: use `memmem`/`str::find` on a
  per-line `String`, or make replace use `search::Matcher` (see §2) which can
  reuse one compiled regex.
- [ ] **Trim the dependency tree / add a release profile.** `serde`
  (`Cargo.toml:17`, with `features = ["derive"]`) is unused as a direct
  dependency — nothing in `src/` mentions `Serialize`/`Deserialize`; only
  `serde_json` is used. Dropping it removes the `derive` feature rano forces on
  the graph (`serde` itself still arrives via `ratatui-core`, `Cargo.lock:1013`).
  Bigger win: `ratatui` default features pull `ratatui-termwiz` → `termwiz` →
  `wezterm-*`/`pest`/`nix`, plus `ratatui-termina` and `palette`
  (`Cargo.lock:988-1019`) for backends rano never uses — try
  `ratatui = { version = "0.30", default-features = false, features = ["crossterm"] }`
  (verify the exact feature names for 0.30) and measure build time. Add
  `[profile.release] lto = "fat", codegen-units = 1, strip = true,
  panic = "abort"` (verify the panic hook still restores the terminal — with
  `panic = "abort"` it does not, so keep `unwind` or install the hook
  differently).

## 4. P3 — robustness and UX

- [ ] **Help overlay clips on short terminals.** `bindings::help_lines()`
  (`bindings.rs:59-105`) emits ~22 lines; `ui.rs:445-466` renders them from row
  0 with no paging, so anything under ~24 rows loses the tail (including
  "Press any key to continue"). Fix: paginate (`^G`/Space cycles) or render a
  scrollable region; also document mouse, paste, prompt history and completion
  keys, which the help text never mentions.
- [x] **Wheel scrolls by moving the cursor.** `editor.rs:920-929` — you cannot
  look ahead in a file without moving the edit point, and `PgDn` then behaves
  oddly. Fix: scroll `bs.scroll` independently when the cursor would leave the
  viewport, nano-style. *(Done: `Editor::wheel` moves `bs.scroll` and pins the
  cursor to the viewport edge only when the scroll would push it out of view;
  test `mouse_wheel_scrolls_view_not_cursor`.)*
- [ ] **Colours assume a dark background.** `syntax.rs:63-87` is a fixed
  One-Dark palette; on a light terminal the default fg and the palette collide.
  Fix: honour `NO_COLOR`, sniff `COLORFGBG` (or query the background), and add a
  `theme = "dark" | "light" | "none"` config key.
- [ ] **No hover / signature help, no `didSave`, no `didClose`.**
  `lsp.rs:539-577` declares only `publishDiagnostics` + `synchronization`
  capabilities (`lsp.rs:548-554`) — completion and definition are used but never
  declared, and `window/workDoneProgress` is not requested (which is why the
  "RA sends no `$/progress`" note in `TODO.md:96` is unsurprising). Fix: declare
  `completion` (+ `completionItem.snippetSupport: false`), `definition`, `hover`,
  and `window.workDoneProgress`; send `didClose` when a buffer goes away; surface
  `window/showMessage` + `window/logMessage` into the status line instead of
  dropping them (`lsp.rs:402-404`).
- [ ] **LSP stderr is discarded, so server failures are undiagnosable.**
  `lsp.rs:458` `Stdio::null()`. This is the blocker for the open
  `TODO.md §8` completion-junk investigation. Fix: pipe stderr to a ring buffer
  and, when `RANO_LSP_LOG` is set, to a file; show the last line in the status
  when the server dies.
- [ ] **Server requests are answered with a bare `null`.** `lsp.rs:601-602`.
  `client/registerCapability`, `workspace/configuration` and
  `workspace/workspaceFolders` expect structured results; some servers retry or
  error out. Fix: answer per method (`[]` for registerCapability/unregister,
  `[null,…]` for configuration, the real folders for workspaceFolders).
- [ ] **`completion_ids` grows without bound** if a server never answers
  (`lsp.rs:77`, insert at `:619`, removal only on a matching response). Fix:
  prune ids older than the queue cap / on timeout.
- [ ] **Unknown CLI flags are treated as file names.** `main.rs:182` takes
  `args.first()`, so `rano --writ` opens a new buffer literally named `--writ`.
  Fix: reject unknown `--`-prefixed args with a usage message; add `+N`
  (start at line), multiple files (one buffer each), and `-` for stdin.
- [ ] **`--help` is one line.** `main.rs:175`. Document the flags, the key
  model, the config path and the debug env vars (`RANO_LSP_RAW`, and the
  `RANO_LSP_LOG` proposed above).
- [ ] **Config parsing is stringly and silently permissive.** `config.rs:52-95`
  ignores every bad line, doesn't accept quoted values (`tab_width = "4"`), and
  *clamps* rather than falling back as `README.md:89` claims. Fix: accept quoted
  values, and flash/`eprintln` a warning listing ignored lines (the editor can
  show it after startup).
- [ ] **No suspend (`^Z`), no `M-\`` style "focus follows", no clipboard.**
  Add `^Z` (SIGSTOP + leave alt screen, restore on foreground) and OSC-52 copy
  (`M-6` → clipboard) for remote sessions.

## 5. P4 — features worth having next

- [ ] Multi-cursor or at least `M-A` select-line / select-all, which makes the
  existing cut/copy/replace paths much more useful.
- [ ] `go to symbol` / `find references` / `rename symbol` over LSP (the client
  already has request plumbing in `lsp.rs:506-516`).
- [ ] Format file / region (`textDocument/formatting`, or `sh -c` fallback).
- [ ] Undo history browser (`M-U` list), plus per-buffer undo already exists —
  surface the count in the status line.
- [ ] Session restore: reopen the buffers that were open last quit (a small
  `~/.local/state/rano/session.json`), with a `--no-session` flag.
- [x] More languages: tree-sitter grammars for JS/TS (+TSX), Markdown, TOML,
  YAML, HTML, CSS, Lua, Ruby, PHP, Java, Make, Dockerfile, INI, diff,
  Elisp, Scheme, SQL and Clojure (`syntax.rs` `Lang` is the only place to
  touch, plus `lsp.rs` `command_for` for servers). Two grammars are not
  crates: Dockerfile's C sources are vendored under `vendor/` (its crate
  binds tree-sitter 0.20 and collides at link time), and Markdown uses the
  maintained `tree-sitter-md` block grammar with a rano-owned query (the
  inline grammar is a separate tree, which rano's no-injection engine
  cannot run).
- [ ] Configurable key bindings — `bindings::BAR` (`bindings.rs:15-47`) is
  display-only today; dispatch is a separate hardcoded match in `keys.rs`, so
  "one binding table" (`README.md:98`) is only half true. Making the table the
  dispatch source also enables the drift test below.

## 6. P5 — code health, tests, repo

- [ ] **Move `BufferState` out of `main.rs`** (`main.rs:41-108`) into
  `src/buffer_state.rs` (or `src/state.rs`), and move `mod ed_tests`
  (`main.rs:284-1908`, ~1600 of the file's 1908 lines) into `src/tests/` or
  `tests/`. `main.rs` should be `main` + `run` only.
- [ ] **Add integration tests for the binary.** No `tests/` directory exists:
  `--version`, `--help`, unknown-flag handling, "file does not exist" and
  "unreadable file" paths, and a `save_to` atomicity test are all untestable
  today. Use `std::process::Command` on `env!("CARGO_BIN_EXE_rano")`.
- [ ] **Property tests for the edit core.** `proptest` (dev-dependency) over
  random edit sequences asserting the invariants the undo machinery depends on:
  `undo` after any sequence returns to the original text, `redo` is the exact
  inverse, `cursor` is always in bounds, `lines` is never empty. This is where
  the region-based undo (`editor.rs:334-505`) is most fragile.
- [ ] **Drift test: every `BAR` key is actually dispatched.** Parse
  `bindings::BAR` and assert `keys::handle_key` reacts to each binding (a table
  of `(KeyCode, KeyModifiers)` → expected effect). Closes the gap the
  `bindings.rs:111-143` tests leave open (they only compare help text to bar).
- [ ] **Test the arithmetic edges that are currently panicking**: `title_line`
  narrow/long-name, completion popup narrow/long-label, `adjust_scroll_x` with a
  1-column viewport, `gutter_width` vs `view_w == 0`.
- [ ] **CI** (`.github/workflows/ci.yml`): add `Swatinem/rust-cache@v2`, a
  second job on `macos-latest` (the Linux-only assumptions in `exec.rs:24`
  `sh -c` and `lsp.rs:100-107` `~/.cargo/bin` deserve a compile-level check),
  `cargo clippy --all-targets --all-features -- -D warnings`, `cargo doc
  --no-deps`, and a `cargo deny`/`cargo audit` step. Consider a nightly
  `cargo build --release` artifact upload.
- [ ] **Repo metadata**: no `LICENSE`, no `description`/`license`/
  `rust-version` in `Cargo.toml`, no `CHANGELOG.md`, no `rustfmt.toml`
  (`bindings.rs:14` has a `#[rustfmt::skip]` that a table-formatted file would
  not need).
- [ ] **Archive the planning artifacts.** `PLAN.md` says "PROJECT COMPLETE" and
  cites line numbers that no longer exist (`PLAN.md:5` says main.rs is ~1528
  lines); `plans/S1..S7` are similarly historical. Move them to `docs/history/`
  and keep a short `docs/ARCHITECTURE.md` (module map + data flow:
  event loop → `handle_key` → `edit_invalidate` → `hl.refresh`/`lsp_dirty` →
  `draw`) so new contributors do not read stale anchors as truth.
- [ ] **`TODO.md §8`**: keep the open rust-analyzer completion-junk item alive
  but time-box it — the parking/retry hack (`editor.rs:612-631`,
  `completion_retry_poll`) is a workaround whose root cause is still unknown, and
  step one (capture RA's stderr, `lsp.rs:458`) is the same fix as the LSP-log
  item above. Do that first; the retry may turn out to be unnecessary.

## 7. Docs vs code drift (fix while you are in the file)

| Claim | Where | Reality |
|---|---|---|
| "Atomic saves: writes go to `file.tmp` + rename" | `README.md:126` | `fs::write`, `editor.rs:1540` |
| "Version from `env!("CARGO_PKG_VERSION")`" (TODO §6 ticked) | `TODO.md:53` | hardcoded `"  rano 0.1.0"`, `ui.rs:38` |
| "out-of-range values fall back to the defaults" | `README.md:89` | clamped to 1..=16, `config.rs:71-75` |
| "one binding table … cannot drift" | `README.md:98` | table drives bar/help only; dispatch is `keys.rs` |
| Replace respects regex/case toggles (implied by the key table) | `README.md:36` | literal + case-sensitive, `search_ctrl.rs:168` |
| "`^T` … timeout or async" (TODO §3 ticked) | `TODO.md:29` | `^T` is async; `M-|` still blocks, `exec_ctrl.rs:129` |
| "`M-B`/`M-F` jump to previous/next match" | `README.md:34` | correct, but only after a search has run; no "wrap" notice |

## 8. Suggested order

1. §1 items 1-3 (three panics, each ~10 lines + a test).
2. Atomic save + `quit_after_save` leak + orphaned exec children (data loss).
3. §3 items 1-3 (per-char `all_diags`, per-keystroke full re-parse,
   `indent_unit`) — these are what make rano feel slow on real files.
4. Replace/regex unification and the replace wrap loop (§2 items 1-2), since
   they share one `Matcher` refactor.
5. §6 tests + CI, before adding features, so the remaining work stays cheap.
