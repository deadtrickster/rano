# S6 — Features & I/O (F1 config, F3 auto-indent, F6 filter, D7 async exec, E1 paste, E2 panic hook)

Grounded to code as of 2026-09-06: main.rs @ 1528 lines, crossterm **0.29.0** (Cargo.lock:180).
Verified in local registry source `crossterm-0.29.0/src/event.rs`: `Event::Paste(String)` (:562),
`EnableBracketedPaste` (:421), `DisableBracketedPaste` (:441) — E1 uncertainty resolved, both exist.

**Shared assumption / dependency:** main.rs has **no `#[cfg(test)] mod` today** (tests exist only in
buffer.rs/lsp.rs/syntax.rs). The editor test harness (`test_ed()`, `press()`, per PLAN.md "Test harness
conventions", created in Phase B) must exist before any S6 red test. If S1/B has not landed yet, create
the harness in main.rs as part of the first S6 item and note it.

---

## F1 — Config file

### Anchors
- `main()` main.rs:1459-1490 — loads config, passes to `Editor::new`.
- `Editor::new` main.rs:108-148 — gains `config` param; struct field in Editor block 79-105.
- No `mod` decl list change beyond `mod config;` (main.rs:1-4).
- PLAN.md F1 said "~/.config/rano/config.toml OR .ranorc?" → **resolved: XDG path only, no .ranorc fallback.**

### Design
- New `src/config.rs`, no new deps:
```rust
#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    pub tab_width: usize,      // 8
    pub auto_indent: bool,     // false
    pub line_numbers: bool,    // false
    pub multibuffer: bool,     // false
}
impl Default for Config { /* the values above */ }
pub fn config_path() -> PathBuf  // $XDG_CONFIG_HOME else $HOME/.config, then "rano/config.toml"
pub fn load() -> Config          // load_from(config_path())
pub(crate) fn load_from(path: &Path) -> Config // read_to_string err -> Default; else parse_config
fn parse_config(text: &str) -> Config          // pure
```
- `parse_config`: per line — trim; skip empty and `#`-comment lines; strip inline comment at first
  `#`; must contain `=` else skip; `key = value` with trimmed sides; keys matched exactly
  (`tab_width`, `auto_indent`, `line_numbers`, `multibuffer`); unknown keys ignored; bad values
  (`tab_width` not usize, bools not `true`/`false`) keep default. `tab_width = 0` rejected (keeps
  default 8 — 0 breaks F2 display math).
- `load_from` takes `&Path` (not owned) so the missing-file test can pass a nonexistent temp path
  without env-var mutation (env::set_var in tests is racy with parallel tests).

### Tests (red | green)
- RED `parse_config("")` == defaults.
- RED `parse_config("tab_width = 4\nauto_indent = true")` → (4, true, false, false).
- RED partial: only `line_numbers = true` → rest default.
- RED invalid: `tab_width = abc` → 8; `auto_indent = yes` → false; `tab_width = 0` → 8.
- RED comments/unknown: `# hi`, `theme = dark`, `tab_width = 2 # inline` → 2.
- RED `load_from(temp_dir.join("nonexistent"))` == defaults.
- GREEN once impl lands; malformed file (directory passed as path / unreadable) → defaults via Err arm.

### Touches
- NEW src/config.rs. `mod config;` + `use config::Config` in main.rs.
- `Editor` field `pub config: Config`; `Editor::new(buf, config)` (all existing call sites + future
  test harness `test_ed` pass `Config::default()`).
- `main()` main.rs:1465: `let config = config::Config::load();` → `run(buf, read_lines, config)` or
  set on `ed` inside `run` before loop (prefer: `Editor::new(buf, config)`).
- F2/F4/F8 consume `ed.config` later; nothing else reads it in S6.

### Risks
- `Editor::new` signature change breaks every test-helper construction — mechanical, done in same commit.
- HOME unset in weird environments → `config_path()` falls back to `PathBuf::from(".config")` joined
  path; `load` simply gets defaults. Acceptable.

---

## F3 — Auto-indent

### Anchors
- `newline()` main.rs:369-377; `Buffer::newline(row, col)` buffer.rs:110-114 (drains right half into
  new row at `row+1`).
- `begin_action(ActionKind::Newline)` main.rs:370.

### Design
- In `newline()`, before `buf.newline`: if `self.config.auto_indent`, compute
  `indent: Vec<char> = self.buf.lines[row].iter().take_while(|c| c.is_whitespace()).copied().collect()`.
- After `buf.newline(row, col)`: if non-empty indent, `self.buf.lines[row + 1].splice(0..0, indent)`
  (the new row currently holds the drained right side; indent goes in front of it).
- `self.cursor = Pos { row: row + 1, col: indent.len() }` (col 0 when disabled/empty indent).
- Undo coalescing: **confirmed no change needed.** Newline is its own `ActionKind::Newline`; under the
  current snapshot impl consecutive Newlines coalesce via `last_kind` (acceptable, unchanged); under
  S1's D3 region refactor the coalescing whitelist is Insert/Backspace/Delete only → Newline is
  always its own step. Both impls give correct single-Enter behavior; no F3 code touches undo.

### Tests (red | green)
- RED: `test_ed("    foo")`, cursor EOL (0,7), `press(Enter, NONE)` → `lines[1] == "    "`,
  cursor (1,4).
- RED: default (auto_indent off) unchanged → `lines[1] == ""`, cursor (1,0).
- RED: indent with tabs: `"\t\tfoo"` Enter EOL → `lines[1] == "\t\t"`, cursor (1,2).
- RED: cursor mid-line `"  hello"` cursor (0,3) Enter → `lines[1] == "  llo"` (indent + right half),
  cursor (1,2).
- Green when all pass.

### Touches
- src/main.rs `newline()` only (plus config field from F1).

### Risks
- `delete_selection_if_any()` runs first (main.rs:371) — selection case: row/col may change before
  indent computed; compute indent AFTER the deletion from the post-deletion cursor row (spec order:
  delete → recompute row/col → capture indent → split). Covered by mid-line test.

---

## F6 — Filter region through command (M-|)

### Anchors
- Alt-key arm main.rs:1235-1252 (add `KeyCode::Char('|')`).
- `PromptKind` enum main.rs:42-55; Enter dispatch main.rs:1379-1407; `prompt_label` ui.rs:269-283.
- `normalize` main.rs:1443-1449; `Buffer::cut_range` buffer.rs:117 (not used here), region text built
  from `self.buf.lines[a.row..=b.row]`.
- TODO.md §5 calls it `^|` — **stale wording**; nano 8 binds **M-|**; we bind `Char('|') + ALT` per
  this plan. C2's bindings.rs will make the bar/help/README agree.

### Design
- `PromptKind::FilterCmd` variant + label `"Filter: "` in ui.rs. (Bar/help entries deferred to C2.)
- Alt arm: `Char('|') => if self.mark.is_none() { self.flash("No mark set") } else { self.prompt = Some(Prompt{ kind: FilterCmd, text: String::new(), cursor: 0 }) }`.
- `do_filter(&mut self, cmd: &str)`:
  1. `cmd` empty → return. Mark still set? If user cancelled mark meanwhile → flash, return.
  2. `(a, b) = normalize(mark, cursor)`; region = **whole rows `a.row..=b.row`** (nano filters
     complete lines; PLAN.md F6 wavered between "exact region [a,b)" and rows — resolved: rows).
  3. `input: String = rows joined with '\n' + trailing '\n'`.
  4. Run **sync** for v1 (deliberate: filter is modal, short-lived, and D7's ExecJob has no
     stdin-writing story; upgrade path noted below):
     `Command::new("sh").arg("-c").arg(cmd).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()`,
     then **write `input` on a spawned thread** (`let h = thread::spawn(move || child_stdin.write_all(input))`)
     and `child.wait_with_output()` — writing inline before reading stdout deadlocks on input > 64K
     pipe buffer; the thread is 5 lines and removes the hazard.
  5. Non-zero exit → flash `Exit {code}: {first stderr line}` (stderr via from_utf8_lossy, first
     non-empty line), region untouched, **no undo step recorded** (begin_action happens only on
     success). spawn error → flash `Error: {e}`.
  6. Success: `begin_action(ActionKind::Filter)` — **new ActionKind variant** (always its own step,
     never coalesces); region rows for the S1/D3 step = `a.row..=b.row` (before), after = the output
     rows. Splice `self.buf.lines.splice(a.row..=b.row, out_lines)`; empty stdout → one empty row.
     `cursor = Pos{ row: a.row, col: 0 }`, `mark = None`, `edit_invalidate()`,
     flash `[ Filtered {n} line{s} ]` (n = out_lines.len()).
- **Sync-vs-D7 decision (coordination with D7 agent):** filter v1 uses sync `Command` + stdin thread;
  it does NOT reuse ExecJob. Upgrade path (post-F8/D7): extend ExecJob with `stdin_tx:
  Sender<String>` + `replace_range: Option<(usize, usize)>` and route both Exec and Filter through
  `exec_poll`. Not worth it now — filter blocks the UI only for the command's duration, same as nano.

### Tests (red | green)
- RED: `test_ed("hello\nworld")`, `ed.mark = Some(Pos{row:0,col:0})`, cursor (0,5) (rows 0..=0),
  prompt FilterCmd `tr a-z A-Z` + Enter → `lines[0] == "HELLO"`, `lines[1] == "world"`,
  cursor (0,0), mark None.
- RED: same setup, one `M-U` → `lines[0] == "hello"` (single undo step).
- RED: no mark, M-| → status flash "No mark set", no prompt.
- RED: failing command (`false`) → flash contains "Exit", buffer unchanged, `ed.undo` unchanged
  (no step recorded).
- RED: multi-row: mark (0,0), cursor (1,3), filter `sort` on `"b\na"` → `"a","b"`.
- RED: `prompt_label(FilterCmd) == "Filter: "` (ui test or moved to bindings later).
- Green: real `tr`/`sort` binaries — tests are Linux-only (env already is; lsp tests already assume
  rust-analyzer installed).

### Touches
- main.rs: PromptKind, ActionKind::Filter, alt arm, `do_filter`, Enter arm.
- ui.rs: `prompt_label` arm (compiler-forced).
- No Cargo.toml change (std::process/thread only).

### Risks
- stdin deadlock if the writer thread is forgotten — test with a large region (64K+) optional but
  recommended (RED first: inline-write version hangs/fails).
- Command inheriting terminal? No — all three pipes set; child can't touch raw mode.
- Filter + D3 refactor (S1): `begin_action` signature may become `begin_action(kind, row_range)` —
  write `do_filter` so the region capture is one call site; adapt mechanically when S1 lands.

---

## D7 — Async exec

### Anchors
- `do_exec` main.rs:929-967 — **sync `Command::output()` at :933 is REMOVED entirely** (TODO §3:
  "do_exec blocks event loop indefinitely").
- Run loop main.rs:1503-1524 — `ed.exec_poll()` next to `ed.lsp_poll()` (:1523).
- `status_text` main.rs:161-178 — new "Running" priority.
- `start_exec` main.rs:921-927; PromptKind::Exec Enter arm main.rs:1404.
- PLAN.md D6 (dirty-flag draw) already anticipates "exec job finished" as a redraw trigger —
  `exec_poll` returning bool when a job completes satisfies it.

### Design
- New `src/exec.rs`:
```rust
pub struct ExecJob {
    pub child: std::process::Child,
    pub rx: mpsc::Receiver<String>,      // full stdout, sent by reader thread
    pub err_rx: mpsc::Receiver<String>,  // full stderr, sent by reader thread
    pub cmd: String,
    pub insert_row: usize,               // row index in the SPAWNING pane's buffer (see F8 note)
    pub pane: usize,                     // 0 pre-F8; target pane once F8 lands
}
pub fn spawn_job(cmd: &str, insert_row: usize, pane: usize) -> io::Result<ExecJob>
// sh -c cmd; stdout+stderr piped; one thread each: read_to_string → send.
```
- `Editor.exec_job: Option<ExecJob>` — **global, single active job** (rationale in F8 section).
- `do_exec(cmd)`: empty → return; `exec_job.is_some()` → flash "Command already running", return;
  `insert_row = cursor.row + 1`; `spawn_job` → Ok: store, flash `Running: {cmd}`; Err: flash.
  The old sync block (:933-962) deleted. Note pre-existing quirk it fixes: old code called
  `edit_invalidate()` even on failure paths (:961) — new code invalidates only on insert.
- `exec_poll(&mut self) -> bool` (redrew?): `take` job; `child.try_wait()`:
  - `None` → put back, return false.
  - `Some(status)`: `rx.recv()` (reader thread has finished writing → cannot block); code 0 &&
    stdout non-empty → build lines (`text.lines()`, empty→one empty row, same as :938-941),
    `begin_action(ActionKind::Exec)` **region = `insert_row..insert_row` (empty before, n lines
    after — the D3 contract agreed with S1: UndoStep{start=insert_row, before=[], after=n rows};
    S1's undo/redo splice must tolerate `before.len() == 0`)**, `insert_lines_at(insert_row, lines)`,
    `cursor = (insert_row, 0)`, edit_invalidate, flash `Ran: {cmd}`; code 0 && empty → flash `Ran:`;
    non-zero → flash `Exit {code}: {first err_rx line}`. Return true.
- `status_text` priority becomes: flash > `exec_job.is_some() → "Running: {cmd}"` > lsp_status >
  loc (C3 will rename show_loc→loc_until — rebase trivially). This ordering is a **decision**;
  PLAN.md D7 only said "Running… while active".
- run loop: `if ed.exec_poll() { /* dirty = true under D6 */ }` each tick.

### Tests (red | green)
- RED: `test_ed("")`, `ed.do_exec("printf hi")` → `exec_job.is_some()`, loop
  `{ sleep(10ms); if !ed.exec_poll() continue }` bounded 200 iters → `lines == ["", "hi"]`,
  cursor (1,0), flash "Ran: printf hi".
- RED: one M-U → `lines == [""]`.
- RED: failing job `ed.do_exec("false")` → poll to completion → flash contains "Exit code",
  buffer unchanged, no undo step.
- RED: `status_text()` is Some containing "Running:" while job active (spawn `sleep 0.3`).
- RED: second `do_exec` while first active → flash "already running", first job untouched.
- Unit: `exec::spawn_job("printf hi", 0, 0)` + manual try_wait loop inserts nothing by itself
  (insertion is Editor's job) — covered by the editor-level tests.

### Touches
- NEW src/exec.rs; `mod exec;` main.rs. Editor field + `do_exec` rewrite + `exec_poll` + status_text
  + run loop. Cargo.toml unchanged (std only).

### Risks
- **S1/D3 dependency:** empty-region UndoStep (`before == []`) must be supported by the region
  refactor; if S1's `finish_step` derives `after` from `before.len()` it records zero after-rows.
  Coordinate: Exec/Filter/ReadFile-style actions pass explicit after rows. Flagged to S1.
- `recv()` after try_wait is safe only because the reader thread always sends (even empty) — spec
  threads send unconditionally; document in exec.rs.
- Zombie risk: job running at quit → child outlives editor (inherits terminal? No — pipes; but
  stdout thread may block forever on a never-closing pipe). Mitigation: on `ed.quit` break, drop
  `exec_job` (Child dropped ≠ killed; acceptable v1, note in code comment-free plan: optional
  `child.kill()` in cleanup path at :1525).
- F8 interaction: `insert_row` indexes the buffer of the pane that spawned — post-F8 the job
  completes into `panes[job.pane]`; if that pane was closed meanwhile, drop output + flash.

---

## E1 — Bracketed paste

### Anchors
- Run loop main.rs:1512-1521 — `_ => {}` at :1519 drops `Event::Paste` today (TODO §4).
- Terminal setup main.rs:1493-1496 (`execute!(EnterAlternateScreen)` at :1494); cleanup :1525-1526.
- `paste()` main.rs:464-488 (inline branch is the template: `merge_inline` + `insert_lines_at`).
- `Buffer::merge_inline` buffer.rs:188-195, `insert_lines_at` buffer.rs:175-185.
- crossterm 0.29.0 **verified**: `Event::Paste(String)`, `EnableBracketedPaste`,
  `DisableBracketedPaste` all exist (registry event.rs:562/421/441). No Cargo change.

### Design
- `fn paste_text(&mut self, s: &str)`:
  1. `s` empty → return (no undo step).
  2. `begin_action(ActionKind::Paste)` — **exactly one step** (kind already exists, main.rs:71).
  3. `delete_selection_if_any()` inside the same step (nano replaces selection on paste).
  4. frags: `s.split('\n')`, trim a single trailing `'\r'` per frag (CJK-safe: chars), →
     `Vec<Vec<char>>`.
  5. `let c = self.cursor;` `merge_inline(c.row, c.col, &frags[0])`; if `frags.len() > 1`
     `insert_lines_at(c.row + 1, frags[1..].to_vec())`.
  6. Cursor at end: single frag → `(c.row, c.col + frags[0].len())`; multi →
     `(c.row + frags.len() - 1, last_frag.len())`.
  7. `edit_invalidate()`.
- D3 region contract (coordinate with S1): recorded region rows = if mark active,
  `min(a.row, c.row)..=max(b.row, c.row)` (deletion + insertion span), else `c.row..=c.row`;
  row-count delta = `frags.len() - 1`. One step either way; the RED test below is impl-agnostic
  (passes on snapshot undo too).
- run loop: `Event::Paste(t) => ed.paste_text(&t)` in the `_ => {}` arm's place; keep
  `KeyEventKind` filter semantics (Paste is not a Key event, unaffected).
- Setup :1494 → `execute!(stdout, EnterAlternateScreen, EnableBracketedPaste)`; cleanup :1525 →
  `DisableBracketedPaste` before `LeaveAlternateScreen`. E2's panic hook also sends
  `DisableBracketedPaste` (harmless if not enabled).

### Tests (red | green)
- RED: `test_ed("")`, `ed.paste_text("ab\ncd")` → `lines == ["ab","cd"]`, cursor (1,2).
- RED: one M-U → `lines == [""]` (single step).
- RED: `paste_text("x\r\ny")` → no `\r` in lines.
- RED: paste with selection: "abcd", mark (0,0), cursor (0,2), `paste_text("z")` → "zd", one M-U →
  "abcd".
- RED: trailing newline `paste_text("q\n")` → `lines == ["q",""]`, cursor (1,0).
- Green: run-loop arm is untestable headlessly — covered by `paste_text` unit tests + manual check.

### Touches
- main.rs: `paste_text`, Event::Paste arm, two `execute!` sites. No ui.rs, no Cargo.toml.

### Risks
- Multi-byte/UTF-8 is safe (chars, not bytes). Huge pastes: one undo step keeps snapshot cheap
  pre-D3; region step keeps it cheap post-D3.
- If the terminal never sent the enable (SSH/screen quirks), Paste events simply never arrive —
  no behavior regression vs today.

---

## E2 — Panic hook

### Anchors
- `run()` main.rs:1492-1496 (setup), cleanup :1525-1526. TODO §4 "Panic guard".

### Design
- In `run()`, immediately after the alt-screen setup succeeds and **before** `Terminal::new` /
  `Editor::new` (so a panic during init is also covered):
```rust
let prev = std::panic::take_hook();
std::panic::set_hook(Box::new(move |info| {
    let _ = disable_raw_mode();
    let _ = execute!(io::stdout(), LeaveAlternateScreen, DisableBracketedPaste);
    prev(info);
}));
```
- Chains the previous hook (default or any already-installed), restores terminal, then reports the
  panic normally. `DisableBracketedPaste` included since E1 lands in the same phase (no-op if E1
  deferred — it's just an escape sequence).
- Cleanup at :1525 stays as-is; hook is process-global, no restore needed.

### Tests (red | green)
- None (task: "No test"). Manual check: `RUST_BACKTRACE=1 rano` + induce panic (e.g. temporarily) —
  terminal must be usable afterwards.

### Touches
- main.rs `run()` only.

### Risks
- If panic fires before setup completes, hook still runs disable/leave on a non-raw terminal — both
  are `let _ =` best-effort, harmless.
- Hook captures stdout; a panic inside another thread's panic prints interleaved — acceptable.

---

## F8 cross-cutting decisions (for the multi-buffer agent)

- **Cutbuffer global, not per-pane.** `cut`/`cut_line` stay on `Editor` (already global today,
  main.rs:87-88). Justification: nano's cutbuffer is deliberately global across buffers
  (nano's global `cutbuffer`), and the killer workflow is ^K lines in file A → ^R new buffer → ^U.
  Per-pane cutbuffers would break muscle memory and buy nothing; PLAN.md F8 already places `cut` in
  the global Editor struct — confirmed, keep it.
- **Config global.** `config` lives on `Editor`, shared by all panes; it is startup defaults
  (F4's `show_line_numbers` toggle is a separate runtime field seeded FROM config, not a config
  mutation — no config write-back, ever, in v1).
- **exec_job global, single active job.** One terminal, one subprocess; a queue adds UI states for
  no value (refuse while running). BUT `insert_row` is a row index into a specific buffer →
  `ExecJob.pane: usize` (0 pre-F8) and completion targets `panes[pane]`; pane closed before
  completion → discard output + flash. Filter (F6) is sync and modal, so it never races pane
  switches.
- **G1 module placement.** `exec.rs` and `config.rs` already standalone by then (no move needed).
  `paste_text` and `do_filter` are Editor action methods → `editor.rs`; `PromptKind::FilterCmd` →
  `prompt.rs` (with its label); `ActionKind::Filter/Paste/Exec` → `undo.rs` enum; editor-level tests
  (harness incl. paste/filter/exec tests) move to `editor.rs` per PLAN.md conventions.

## Contradictions / coordination notes (vs PLAN.md & TODO.md)

1. TODO §5 says filter is `^|`; actual binding is M-| (nano 8). Docs fixed by C2, not here.
2. PLAN F1's ".ranorc?" alternative is dropped — XDG path only (decision above).
3. PLAN F6's region wording is internally inconsistent ("exact region [a,b)" vs "rows a..=b") —
   resolved to whole rows `a.row..=b.row` (nano semantics).
4. PLAN D7's status priority is unspecified vs lsp/loc — decided: flash > Running > lsp > loc;
   rebases cleanly on C3's `loc_until`.
5. Old `do_exec` calls `edit_invalidate()` on failure paths (main.rs:961) — bug; D7's rewrite
   invalidates only on insert (behavior change, covered by "failing job" red test).
6. Ordering: PLAN.md sequence is D3 (S1) → D7 → E1 → F1/F3/F6. S6 items only *need* the test
   harness (Phase B) and F1-before-F3; every undo-related red test is written impl-agnostic so S6
   can proceed before/after S1's D3 without rework.
7. `Editor::new` gains a `config` parameter in F1 — S1/F8 test helpers must be updated in the same
   commit as F1 (mechanical).
