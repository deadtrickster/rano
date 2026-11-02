# rano TODO

Open work only. Done items are removed; git history keeps them. Gates: `cargo test`,
`cargo clippy --all-targets -- -D warnings`, `cargo fmt --all -- --check`.

Conventions: editor tests use `test_ed`/`press`/`lines` (`src/ed_tests.rs:23`). In
`handle_prompt_key`, alt and ctrl arms come before the modifier-blind `Char`/`Left` arms,
otherwise `M-b`, `M-f` and the rest type letters.

## Crashes and data loss

- Completion popup underflows on a narrow terminal. `src/ui.rs:630` `w - 2 - label - tag` and
  `src/ui.rs:588` `clamp(10, width.min(60))` panic under 10 columns. Use `saturating_sub` and a
  safe clamp. Test a long label at width 8 and 20.
- Saves are not atomic. `src/editor.rs:3216` is a plain `fs::write`, so a crash or full disk
  truncates the file. Write `<file>.tmp`, flush, copy the mode, `rename`, then update
  `disk`/`name`. Test that the mode is kept and no tmp file is left.
- A second `^T` orphans the first child. `src/exec_ctrl.rs:28` overwrites `exec_job` and
  `ExecJob` has no `Drop`. Kill and wait the old job, and kill a running job on quit.
- `^T` and `M-|` have no timeout, and `M-|` blocks the UI on `wait_with_output`
  (`src/exec_ctrl.rs:131`). Move the filter onto the `ExecJob` poll path with a deadline.
  `run_filter` (`src/exec_ctrl.rs:225`) already has the kill-on-deadline shape.

## Editing correctness

- Replace uses the literal `Buffer::find_next`/`find_all` (`src/search_ctrl.rs:168`, `:201`,
  `:268`) and ignores the `M-C`/`M-R` toggles that search honours through `search::Matcher`
  (`src/search_ctrl.rs:69`). Decided earlier: replace stays literal. Then at least flash a notice
  when replace starts with `search_regex` on. Revisit driving replace from `Matcher`.
- Interactive replace never ends. `Buffer::find_next` wraps (`src/buffer.rs:470`), so `n` on the
  last match returns to the first. Remember the start and stop with "Replaced N" on reaching it.
- ReplaceAsk drops the prompt on any non-char key and leaves `self.replace` set (bare `return;`
  at `src/prompt.rs:199`). Restore `p` as the Confirm* arms do, or cancel the replace.
- Completion insert can put a raw `\n` in a row. `insert_str_plain` (`src/editor.rs:1538`)
  calls `insert_char` per char. Split on `\n` as paste does.
- `^R` into a non-empty buffer overwrites its `crlf` and `encoding` with the inserted file's
  (`src/editor.rs:3899`). Keep the buffer's. Decide whether mixed EOLs are refused or normalised.
- `justify` counts chars, not display columns, and drops the indent (`src/editor.rs:4254`).
- `sort_lines` sorts whole rows for a partial selection (`src/editor.rs:4305`). Sort only fully
  covered rows. It also lowercases two `String`s per comparison (`src/editor.rs:4317`). Compute
  the key once per row. `Buffer::sort_rows_by` (`src/buffer.rs:177`) needs a keyed variant.
- `match_bracket` knows 3 pairs, looks ahead 5 chars and ignores strings and comments
  (`src/editor.rs:2762`). Skip string and comment nodes using the parse, fall back to the
  innermost enclosing pair.
- `do_goto` keeps every digit, so `1,2` goes to 12 (`src/editor.rs:2861`). Parse `N[,C]` and
  flash on anything else.

## LSP

- The completion queue is popped once per response (`src/editor.rs:1613`), so an unanswered
  request shifts every later answer. Key it by request id (`completion_ids`, `src/lsp.rs:77`)
  and prune that on timeout.
- Server stderr is discarded (`src/lsp.rs:568` `Stdio::null()`). Pipe it to a ring buffer and a
  file under `RANO_LSP_LOG`, and show the last line when the server dies.
- `initialize` declares only diagnostics and sync (`src/lsp.rs:659`). Declare `completion`,
  `definition`, `hover`, `workDoneProgress`. Send `didClose`. Surface `showMessage`/`logMessage`
  (dropped at `src/lsp.rs:500`).
- Every server request gets a `null` answer (`src/lsp.rs:714`). Answer by method: `[]` for
  registerCapability, `[null,...]` for configuration, the real folders for workspaceFolders.
- `didChange` sends the whole document (`src/lsp.rs:694`). Send ranges when the server allows.
- `LspClient::shutdown` runs on the UI thread and waits up to 2 s: on close
  (`src/lsp_ctrl.rs:73`) and when a stale handshake client is dropped (`src/lsp_ctrl.rs:142`,
  `src/lsp.rs:797`). Hand it to a background thread.
- `M-.` blocks for up to 3 s (`src/editor.rs:1782`). Make it async like completion.
- `raw_log` opens the log file per message (`src/lsp.rs:253`). Open it once.

### Completion junk on the repo path

Typing `text.` in `src/prompt.rs` inside the rano checkout makes rust-analyzer return the
path-fallback list (`self::`, `crate::`, locals). A copy of the repo at another path returns
`str` members. Server-side and path-dependent. The retry and junk-parking (`src/editor.rs:1634`)
only work around it.

- Diff the initialize/didOpen streams of a failing and a working run (`RANO_LSP_RAW`): `rootUri`,
  `workspaceFolders`, capabilities.
- Capture RA's logs with `RUST_LOG=info` once stderr is kept (above).
- Copy the repo to a sibling and to an unrelated dir: does the junk follow the parent or the path?
- Suspects: flycheck contention on `target/`, inotify exhaustion, VFS root discovery above home.
- After the root cause, retune or drop the retry.

## UX

- `-` as the file argument should read stdin. Today it opens a file named `-`.
- No `^Z` suspend. Leave the alt screen, stop, restore on SIGCONT.
- `NO_COLOR` is not honoured.
- Bad config lines are ignored silently. Warn after startup.
- Keymaps drive dispatch (`src/keys.rs:193`, `src/keymap.rs`), but the config cannot rebind a key.
- Search prompt label is a static `"Search: "` (`src/prompt.rs:51`). Show `[case]`/`[re]`.
- Help does not list prompt-local keys: Up/Down history, Tab completion, `M-b`/`M-f` word motion,
  `M-C`/`M-R` (`src/help.rs`).
- Mouse support is not verified live (`tmux send-keys -M`).

## Features

- Select line, select all.
- LSP go-to-symbol, rename, format file or region (gofmt-on-save is the only formatter).
- Undo history browser, undo count in the status line.
- Session restore (`~/.local/state/rano/session.json`, `--no-session`).
- Folding headings in `TODO.md`. Heading motion (`src/todo_ctrl.rs:285`) was chosen instead,
  because `wrap_prefix`, `visual_pos`, `adjust_scroll`, the gutter and mouse-to-row assume every
  row is visible. Middle option: collapse a section to its heading and direct children.

## Previews (`M-p`)

- No decoder in the binary for machines with no converter on PATH (`image::CONVERTERS`).
  `Source::Picture` and `sync_image` take one unchanged.
- CSV/JSON table preview: one more `Source` variant (`src/diffview.rs:44`), no new dependency.
- No key for a larger picture. The box is capped by `image_box` (`src/term/graphics.rs:37`)
  and 30 rows in `image_cells`. Undecided.
- Animated pictures show their first frame (`{in}[0]`, `src/image.rs:100`).

## Log mode

- Log search has no caller in the editor (`src/lib.rs:177`). `logsearch::scan` needs keys (`^W`
  from a tail searches backward) and a jump that decodes only the screenful it lands on.
- Replace logsearch's row pre-pass over `[0, from)` (`src/logsearch.rs:319`) with a lookup into
  `LogTail`'s sparse index.
- Use `memchr` in `logtail` (`src/logtail.rs:168`) and logsearch's literal matcher.
- `LogTail`/`LogPane` have no caller in rano (`src/lib.rs:182`). The tail pane belongs to the host.

## Large files

Principle: size does not limit editability. No size threshold, no read-only or "too big" mode.
Piece table dropped: the chunked store gives O(1) lookup and flat edits.

- Stage B: swap `Buffer::lines` from `Rows` to the file-backed `RowStore`. `src/buffer.rs:39` is
  still eager `lines: Rows`. A 193 MB file sits at ~977 MiB resident against ~121 MiB lazy.
  Decided: the loader becomes a byte-range indexer. `LoadMsg::Rows(Vec<Vec<char>>)`
  (`src/loader.rs:197`, `:348`, `:505`) becomes `LoadMsg::Ranges(Vec<(u64, u64)>)`, fed to
  `RowStore::begin`/`ingest` (`src/rows.rs:207`, `:233`). Rejected: `RowStore::open` from
  `from_file` (O(bytes) scan, dead screen on 2 GB).
- The swap lands in one commit with the loader rewrite: ~53 `self.lines` uses in `buffer.rs`,
  ~58 `row()`/`row_opt()` reads needing `ensure`, 8 `buf.rows()` iterations, 3 `row_mut()`.
  Decided: prepare-then-read (`ensure(a, b)` then borrow), not `RefCell`. O(document) callers
  (`rows_range`, `rows_vec`, `sort_rows_by`, export, search, undo capture) call
  `materialize_all()` first.
- Invalid UTF-8 past the sniffed 64 KiB surfaces as `io::Error(InvalidData)` from `read_row` on
  render, not as `LoadMsg::Failed`. Render must report it, not draw an empty row.
- `indent_unit` (`src/config.rs:83`, `detect_indent`) walks every row on Enter/Tab/Backspace,
  ~13 ms at 2.6M rows, and blocks the swap. Decided: a per-chunk histogram of leading widths in
  the row store, which can land before the swap.
- Wrap table from the index for narrow rows: `ceil(char_count / view_w)` from byte counts,
  narrow/wide from lead bytes via `width::is_wide_cp` (`src/width.rs:99`).
- Undo stores whole rows: `UndoStep.before/after: Vec<Vec<char>>` (`src/editor.rs:93`), filled by
  `begin_action` (`:1338`) and `current_after` (`:1418`). About 31 ms of a 33.5 ms keystroke on a
  20.9 MB single row. Store `(row, char_range, text)` deltas. `coalesce_kind` tests "both steps
  touched one row". Add a property test first: N random edits, undo all, byte-identical.
- The wrap prefix (`src/bufstate.rs:84`) is re-summed from the edited row down on every edit in
  `ensure_wrap_prefix` (`src/editor.rs:697`), 1.7 ms at 2.6M rows. Use a Fenwick tree read by
  `buf_row_of_visual`, `seg_count` and the renderer, or turn wrap off above a row cap.
- Windowed highlight edges are approximate: a construct opening above the window is coloured as
  if it opened inside it (`src/syntax.rs:1438`). Margins 200 rows / 4,000 cols
  (`src/editor.rs:989`). Undecided: accept with a status note, widen backwards until the first row
  parses without `ERROR`, or seed from a real parse once every mutation goes through `Buffer`.
- Whole-document paths rebuild text from chars via `Buffer::text` (`src/buffer.rs:278`): the
  diagnostics parse (`src/syntax.rs:1722`) and LSP didChange (`src/editor.rs:1578`), ~24 ms at
  30 MB. Hand the parser the file's bytes.

After the swap:

- Eviction is built (`RowStore::with_budget`, `src/rows.rs:125`). Derive the budget from
  `text_h` plus margin and measure worst-case frame time with a tiny cache.
- Windowed search: `Matcher::find_all` takes `&[Vec<char>]` (`src/search.rs:34`). Rewrite as a
  chunked byte scan that finds matches across chunk boundaries. Differential-test against the
  current one.
- Incremental index: first chunk on open, extend on scroll, `[N% indexed]` in the status line.
- A file changed under a lazy buffer: compare size and mtime, reindex on mismatch.
- Tests: `search_finds_a_match_spanning_a_chunk_boundary`,
  `scrolling_back_and_forth_does_not_regrow_the_cache`, `the_index_extends_without_rereading`,
  `an_edit_is_the_same_cost_at_any_size`.
- Open question: `RowStore` holds 40 B/row of index plus 40 B/row of wrap geometry, ~7 GB for a
  2 GiB file of 22-byte lines. Paying only for what is visible needs a sparse block index (one
  entry per 256 KiB), visible-row wrap geometry and an edit overlay. Not designed.
- Unexplained ~54 B/row resident on short-line files. Suspect: the wrap table. Untested.

## Streaming parse engine (`syntax::Stream`)

- `Stream::push` (`src/syntax.rs:2234`) is O(document) per push on markdown, so a stream is
  O(N²): tree-sitter does not reuse a token while markdown's external scanner is active. Parse
  only the growing tail via `set_included_ranges` and splice, or settle closed blocks out of the
  input. `per_push_cost_stays_flat` (`src/syntax.rs:4009`, ignored) fails until then.
- `Stream::spans` (`src/syntax.rs:2392`) walks the whole text per call. Use
  `QueryCursor::set_byte_range` over changed rows, widened to overlapping nodes, when an editor or
  a long fence needs it.
- `` ` ```console ` `` in a paragraph gets a few cells in the literal colour (the inline grammar is
  looser than CommonMark). Decided: leave it.
- `Lang::from_token("console")` returns `None` on purpose (`src/syntax.rs:4102`). It changes only
  if letibot rules `console` is shell.

## Plugins

Engine is wasmtime (decided). No file watching or mtime polling (decided).

- Load-time probe: call each hook once on 64 KiB, print time, ms/MiB and fuel, `SLOW` above
  250 ms/MiB. `Plugin::last_fuel` (`src/plugin.rs:297`) is recorded but unused.
- Per-call counters: flag a call over 50 ms once per session per hook, disable the hook after 3.
  The counter survives a reload. Never fail a save on slowness.
- `M-x plugin-reload` (`src/commands.rs` has no plugin command): read, compile, probe, then swap.
  On failure keep the old module and say so once.
- Bus service, config `bus = true`: name `dev.rano.Editor`, object `/dev/rano/Editor`, interface
  `dev.rano.Plugin1`, methods `Reload()`, `Status()`, `Open(path)`, shipped as a builtin script.
  Measure `zbus` against a Unix socket with the same three messages first.
- Ship the dired as a builtin. `builtin::ALL` (`src/builtin.rs:50`) has only `MANUAL` and `PDF`.
  The Rust `dirview` stays the fallback (decided).
- Load `scripts/*.wasm` beside `built-in/`, in name order. Startup loads only `builtin::ALL`
  (`src/editor.rs:3458`).

## Agent UI (`src/agent/tree_list.rs`)

- `monitors.rs`, `queue.rs` and `config.rs` keep their own renderers because the list has no
  sections or blank lines. If wanted, add a section field and a "detail only while picked" field.
- `queue.rs` draws no hint when the queue is empty and never cuts its rows to `w`.

## Todo sync (host side, not rano)

- Wire status mapping: `Declined` has no wire state, `InProgress` has no file state, and a
  markdown line records no author. Decide drop, widen the enum, or refuse. Where the shared
  file's hash at copy time lives is undecided.
- `content -> item` addressing: `Item::text` keeps a leading space and the `~3d #tag @name` tail,
  and duplicate titles are ambiguous. Pinned by `tests/todo_writeback.rs`.

## Tests, CI, repo

- Binary integration tests: nothing in `tests/` spawns `CARGO_BIN_EXE_rano` (`--help`, unknown
  flag, unreadable file, atomic save).
- Proptest over random edit sequences: undo restores the text, redo is its exact inverse, the
  cursor stays in bounds.
- CI: `cargo doc --no-deps`, `cargo deny` or `cargo audit`.
- Cargo.toml has no `description` or `rust-version`. No CHANGELOG. Add `docs/ARCHITECTURE.md`
  (module map, event loop to draw).
