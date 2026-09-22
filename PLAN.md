# rano — implementation plan (compaction-survival artifact)

> **Superseded for new work.** This file covers the 2026-09 build, which reached
> PROJECT COMPLETE (40/40, 166 tests) and whose phases A–G are all done. The
> plan for the responsiveness and large-file work is **TODO.md §16**, with the
> problems it addresses in §13–§15. This file is kept for its STATE history and
> its conventions.

Grounded to the code as of 2026-09-06. Companion to TODO.md (the what); this file
is the how. Update STATE after every phase. Source files: src/{main,buffer,lsp,
syntax,ui}.rs. main.rs is ~1528 lines; Editor struct at main.rs:79-105.

## STATE (update as work proceeds)

- [x] TODO.md written; baseline 27/27 tests green (rust-analyzer component installed)
- [x] buffer.rs replace_at already fixed (verifies match, returns bool) — per S5:
      write its tests GREEN, no revert dance
- [x] 7 plan files in plans/: S1 (I wrote it after subagent failures), S2..S7
- [x] Coordination decisions merged (see GRAPH section)
- [x] W0 done: deps, placeholder modules + mod decls, Editor::new pub(crate)
- [x] W1 done (5 parallel agents): buffer tests+CRLF (buffer.rs 15→34 tests),
      lsp did_change_params/end_col/spawn_async+tests, Lang::{Python,C,Json}+
      command_for/language_id/lsp_sync arms, leaf modules (bindings BAR+help_lines,
      config parse/load, exec ExecJob+spawn_job+try_finish, search Matcher),
      ui helpers (gutter_width/display_width/display_col/line_to_spans)+tests.
      83/83 green. 20 dead-code warnings expected until wiring.
- [x] W2 E1 done (93/93 green): ed_tests harness (test_ed/press/lines), B2 move_left
      fix, B1 prompt multibyte fix (prompt_insert/prompt_backspace), C3 loc_until,
      D2 sort_by_cached_key, save_to→file_text CRLF hook
- [x] W2 E2 done (109/109 green): region-based UndoStep + VecDeque + pending/finish_step
      coalescing (all action sites rewired), undo/redo now edit_invalidate+clamp;
      replace_all_from one-pass nano semantics; 13 protective + 3 D1 tests.
      Deviations: last_kind set in finish_step; coalesce requires prev.after.len()==1;
      newline/paste selection-span regions; no-match 'a' records no step.
- [x] W2 E3 done (117/117 green): D4 lsp_dirty/lsp_flush(300ms) debounce (clears
      flag even lsp None), D5 spawn_async + lsp_starting tag-validated adoption
      (catch-up via lsp_dirty), E5 diag underline in char_style + M-D jump_next_diag
      + word-motion rebinding (Alt+Left/Right; M-n free). 8 new tests.
- [x] W2 E4 done (126/126 green): async exec (spawn_job/exec_poll, status Running,
      no-undo failure path), paste_text + bracketed paste + Event::Paste arm,
      panic hook, F6 filter (M-|, FilterCmd, stdin thread, no-undo failure).
      9 new tests.
- [x] W2 E5 done (137/137 green): dirty-flag draw loop (tick_status/lsp_poll/lsp_flush
      -> bool, resize cache, text_w=width + justify -2), scroll_x display-col based,
      tabs in draw (text_window pairs), F4 gutter + M-N. 11 new tests. NOTE:
      rust_diagnostics_flow flaked twice under load (rust-analyzer starving) — env.
- [x] W2 E6 done (150/150 green): config field + Editor::new(buf, config) wiring
      (test call sites incl. ui.rs test ctor), F3 auto-indent (config-gated),
      expand_tilde, prompt history (3 kinds + draft + Up/Down), Tab path completion
      (complete_path), prompt word motion (M-b/M-f/Ctrl+arrows, arm order invariant
      held). 13 new tests.
- [x] W2 E7 done (154/154 green): Matcher wiring (do_search/next_match/prev_match
      cursor-relative + wrap), search_matches stored + edit_invalidate-cleared,
      invalid-regex flash, M-C/M-R Search-prompt toggles. 4 new tests.
      NOTE: main.rs now fmt-clean.
- [x] W2 E8a done (154/154 green, verified after run): BufferState refactor —
      per-buffer fields moved (buf, hl, lsp*, cursor, scroll, scroll_x, mark,
      search/SearchState, search_matches, exec_job, undo/redo/pending/last_kind);
      Editor keeps buffers: Vec<BufferState> + cur: usize + globals (cutbuffer,
      case/regex toggles, prompt, status, quit/pending_write/quit_after_save,
      replace*, text_w/h, tab_width, show_line_numbers, config, histories);
      bs()/bs_mut() accessors, all methods stayed on Editor, zero behavior change,
      tests access-path-only. 4 pre-existing bindings.rs warnings remain.
- [x] W2 E8b done (163/163): F8 OpenName (multibuffer push vs in-place replace),
      M-</M-> wrap switching with per-buffer state isolation (cursor/undo/LSP),
      title [i/n], quit cycles modified buffers. 9 new tests.
- [x] E9a done (162+1): G1 module split main.rs 3866→1442 + editor.rs/prompt.rs/
      search_ctrl.rs/lsp_ctrl.rs/exec_ctrl.rs/keys.rs; BufferState stayed in
      main.rs (avoids new dead-code lints on lsp.rs LspEvent fields — noted).
- [x] Inline finish (all gates green, 165 tests + 1 ignored, clippy -D warnings
      clean, cargo fmt applied repo-wide + fmt --check clean):
      G2 --version/-V; G4 #[ignore] rust_diagnostics_flow (suite 3.5s→0.30s);
      bindings.rs BAR/help_lines wired into ui.rs bar+help overlay (kills 4
      dead-code warnings, fixes ^W/^Y overlay drift — TODO 14); last 2 clippy
      lints fixed (lsp.rs collapsed-if, prompt.rs saturating_sub); D6 diag
      gutter markers (line numbers colored by severity, TODO 37); TODO 65
      title_line + function-bar layout tests.
- [x] G3 CI: .github/workflows/ci.yml (fmt --check, clippy --all-targets
      -D warnings, cargo test — all three verified locally first).
- [x] README rewritten from bindings.rs BAR + config.rs + lsp.rs facts.
- [x] TODO.md fully ticked (config item annotated: theme/bindings out of scope).
PROJECT COMPLETE: 40/40 TODO items, 166 tests total (165 + 1 ignored LSP e2e).


## COORDINATION DECISIONS (from S1..S7 planning)

1. Bindings final set: prev_word = Alt+Left + M-d; next_word = Alt+Right + Ctrl+Right
   (M-n freed); M-D = next diagnostic (S3); M-N = line numbers (S4); M-| = filter
   (S6); M-< / M-> = buffer switch (S7); M-C / M-R = search case/regex toggles in
   Search prompt; M-b / M-f / Ctrl+Left / Ctrl+Right = word motion in prompts.
   CRITICAL (S2): in handle_prompt_key, new alt/ctrl arms MUST precede the
   modifier-blind Char/Left arms or M-b/M-f/M-c/M-r type letters.
2. B1 repro corrected (S2 verified by rustc): bare 'é'+Backspace survives; real
   panics are "éa"+Backspace and "é"+ASCII-insert. Red tests use those.
3. D5 catch-up goes through D4's lsp_dirty/last_send (not direct change()).
   lsp_starting is tagged with doc name; adopt validates tag, stale = dropped.
4. text_w unifies to full width (S4); justify keeps old wrap width explicitly.
5. F8: `hl: Highlighter` goes into Pane (PLAN sketch omitted it); lsp/lsp_starting
   global but tag-validated; cut/cut_line, replace*, prompt, config, exec_job
   global (exec_job carries .pane); try_quit extended to all panes; gutter flag
   per-pane post-F8.
6. gutter_width(rows) = max(2, digits(rows)) + 1 → gutter_width(10)==3.
7. F6 filter v1: sync, whole rows a.row..=b.row, stdin written on a thread (64K
   pipe deadlock); no undo step on failure.
8. D7 exec_poll() -> bool; also fixes existing failure-path edit_invalidate bug
   (main.rs:961).
9. F7 crates verified compiling vs tree-sitter 0.27 (S7 scratch build):
   tree-sitter-python 0.25.0, tree-sitter-c 0.24.2, tree-sitter-json 0.24.8;
   `tree_sitter_c::HIGHLIGHT_QUERY` is SINGULAR (others HIGHLIGHTS_QUERY).
10. C2: help_lines() -> Vec<String> (not &'static str).
11. D3 finish_step must tolerate empty-region steps (before == []) — D7/E1/F6.
12. F8 invalidates ALL main.rs line anchors: every W2 executor re-greps before
    editing; G1 locates by symbol.
13. SearchState regex/case fields land early (S2) so prompt arms don't block on
    the Matcher (S5).
14. Editor::new → pub(crate) (S4 ui tests need it).

## IMPLEMENTATION GRAPH

W0 (me, now): Cargo.toml deps (regex, tree-sitter-{python,c,json}); placeholder
modules bindings.rs/config.rs/exec.rs/search.rs + mod decls in main.rs;
cargo build+test green.

W1 (PARALLEL subagents, disjoint files, nobody touches main.rs):
  P1 buffer.rs   — coverage tests + C1 CRLF                     [plans/S5]
  P2 lsp.rs      — did_change_params+tests, end_col, spawn_async[plans/S3]
  P3 syntax.rs   — F7 langs + highlight tests                   [plans/S7]
  P4 leaf mods   — search.rs/config.rs/exec.rs/bindings.rs+self-tests
                                                                  [S5,S6,S7]
  P5 ui.rs       — line_to_spans, display_width/display_col, gutter_width + tests
                                                                  [plans/S4]
  Gate: full cargo test green; P4 code compiles via W0 mod decls.

W2 (SEQUENTIAL main.rs executors; each: re-grep anchors, follow plan file, end
with full suite green):
  E1 main.rs: B1+B2+C3+D2 + save_to file_text hook              [S2,S1,S5]
  E2 main.rs: D3 undo refactor (protective tests FIRST) + D1    [S1]
  E3 main.rs: D4+D5 wiring, E5 diag char_style + M-D rebind     [S3]
  E4 main.rs: D7 exec wiring, E1 paste + bracketed paste, E2 panic hook, F6 filter [S6]
  E5 main.rs+ui.rs: D6 draw wiring, E3 scroll_x, F2/F4 wiring (M-N) [S4]
  E6 main.rs: E4 prompt upgrades + F3 auto-indent + F1 config wiring [S2,S6]
  E7 main.rs: F5 Matcher wiring                                 [S5]
  E8 main.rs+ui.rs: F8 multi-buffer                             [S7]
  E9 all:      G1 module split, G2 version, G4 #[ignore]        [S7]
Final (me): G3 CI, README rewrite, TODO.md tick, STATE update.

## Phase A — coverage tests (green) + 1 red (replace_at)

buffer.rs tests (append to `mod tests`):
- copy_range_single_row / copy_range_multi_row / copy_range_whole_lines: mirror the
  cut_range tests (buffer.rs:304-347); assert buffer unchanged after copy.
- copy_range_empty_region: a==b → out == [vec![]] (characterize current).
- insert_char tests: mid, at EOL (clamped, buffer.rs:75-79), on empty buffer.
- replace_at edges: longer replacement ("foo"→"foobar"), shorter ("foo"→"f"),
  multibyte ("héllo" replace "é"→"e"), and RED: replace_at with non-matching text
  at pos must leave buffer unchanged + return false (old impl spliced at
  `pos.col.min(len-n)` and corrupted; new impl checks match first).
- find_all_overlap: "aa" in "aaa" → [(0,0),(0,1)] (characterize; overlapping is
  intended for literal search).

lsp.rs: extract pure fn from change() (lsp.rs:321-348):
```rust
fn did_change_params(uri: &str, version: i32, text: &str) -> Value
```
(end position = last line index + UTF-16 length of last line; change() calls it).
Tests: text "a𝕏\nb\n" → end {line:2,character:0}; "abc" → {0,3}; UTF-16 astral
char counts 2 units ("a𝕏" → character 3). poll() answering test: test-only
constructor building LspClient literal with `child: Command::new("cat") piped`,
stdin = Box::new(SharedBuf(Arc<Mutex<Vec<u8>>})) (test-local Write impl), preload
rx channel with LspEvent::ServerRequest{id:5}; poll() → buffer contains
`"id":5` and `"result":null`.

ui.rs: test mod for title_line (ui.rs:236-267): basic centering (name pos >
TITLE_LEFT.len()), modified adds '*', flags near right edge (within last 8),
long name truncated without panic, width 10 narrow no panic, output len == width.

## Phase B — §1 crash bugs (red → fix)

B1 prompt multibyte panic. Root cause: handle_prompt_key (main.rs:1408-1431) uses
p.cursor as CHAR count but String::insert/remove take BYTE index. Type 'é' then
Backspace → remove(1) is not a char boundary → panic. Fix: helpers in main.rs:
```rust
fn prompt_insert(p: &mut Prompt, c: char) { // Vec<char> round-trip
    let mut v: Vec<char> = p.text.chars().collect();
    v.insert(p.cursor.min(v.len()), c); p.cursor = ...; p.text = v.into_iter().collect();
}
fn prompt_backspace(p: &mut Prompt) { /* same via remove(p.cursor-1) */ }
```
Callers: handle_prompt_key Backspace arm (main.rs:1408), Char insert arm
(main.rs:1429). Also start_write/start_backup set cursor via chars().count()
(main.rs:981,1067) — stays correct since cursor remains char-count.
RED test (editor tests, new #[cfg(test)] mod in main.rs): helper `ed_search_prompt()`
→ handle_key Char('é'), handle_key Backspace → must not panic, text == "".
Test harness: `fn test_ed(text: &str) -> Editor` building Editor::new(Buffer with
lines, name None), text_w 80, text_h 24. KeyCode::Char('é') with no modifiers.

B2 move_left BOL. main.rs:521-529: after `self.cursor.row -= 1` sets col to
`line_len(c.row)` (OLD row) → col can exceed new row len → later Backspace panics
in Buffer::backspace (buffer.rs:102 `lines[row].remove(col-1)`).
Fix: `self.cursor.col = self.buf.line_len(self.cursor.row);`
RED test: buffer "ab\ncdef", cursor (1,0), move_left → assert (0,2).
Also add clamp_cursor() after for safety.

## Phase C — §2 correctness

C1 CRLF. Buffer gains `pub crlf: bool` (default false). from_file (buffer.rs:26-35):
detect `text.contains("\r\n")` BEFORE lines() strips it; store. New method:
```rust
pub fn file_text(&self) -> String // text() but joins with \r\n when crlf
```
save_to (main.rs:1002) writes file_text(). Internal text()/LSP/syntax keep \n.
RED test: from_file on temp file "a\r\nb\r\n" → crlf true; file_text() ==
"a\r\nb\r\n"; text() == "a\nb\n". Currently: lines() strips \r → red.
Buffer::new()/test helpers set crlf: false (fix all struct literals incl.
buffer.rs:249, syntax.rs test helper uses Buffer::new).

C2 bindings single source. New src/bindings.rs:
```rust
pub struct Binding { pub key: &'static str, pub label: &'static str }
pub static BAR: [Binding; 24] = [...];        // what ui.rs:24-51 has today
pub fn help_lines() -> Vec<&'static str>;      // help overlay content, built from BAR + extra prose
```
ui.rs: bar_items() deleted, draw uses bindings::BAR; help overlay (ui.rs:192-230)
uses bindings::help_lines(). RED test: help text must NOT contain "^W" and must
contain "^F Where Is" (current help ui.rs:196-209 says ^W/^Y — red). README
rewrite happens in Phase G (after all bindings settle: ^F, ^B, M-B/M-F, M-D diag,
M-| filter, M-< / M-> buffers, M-N lines, M-C/M-R search toggles).

C3 show_loc expiry. Replace `pub show_loc: bool` with `loc_until: Option<Instant>`.
^C (main.rs:1275) sets Some(now+2s). status_text (main.rs:170-176) checks
loc_until > now. tick_status clears expired. RED test: press ^C → status_text is
Some("Line 1, Col 1"); ed.loc_until = Some(past) ; tick_status → None.

## Phase D — §3 performance

D1 replace-all one-pass + nano semantics. RED test: cursor (0,0), buffer "aaa",
replace find "aa" with "b", answer 'a' → expect "ba" (replace AT cursor, standard
left-to-right non-overlapping). Current: find_next strictly-after → "ab". RED.
New fn in main.rs (or search.rs later):
```rust
// one pass; find/with single-line only; drift-corrected same-row cols
fn replace_all_from(&mut self, find: &str, with: &str) -> usize {
    let matches = self.buf.find_all(find); // once
    let (mut drift, mut next_free) = (0isize, 0usize); let mut anchor_row = None;
    for m in matches {
        let row = m.row; let col = if anchor_row == Some(row) { (m.col as isize + drift) as usize } else { m.col };
        // skip overlaps: if row==anchor_row && col < next_free → continue
        // first match must be >= cursor? NO: 'a' semantics = from cursor: filter matches >= cursor first (col>=cursor.col when row==cursor.row, row>cursor.row, or wrap? keep: strictly from cursor, no wrap for 'a')
        if !self.buf.replace_at(Pos{row,col}, find, with) { continue; }
        anchor_row = Some(row); drift += with.chars().count() as isize - find.chars().count() as isize;
        next_free = col + with.chars().count(); self.replace_count += 1;
        self.cursor = Pos { row, col: next_free };
    }
}
```
answer_replace_ask 'a' arm (main.rs:872-889) calls it inside begin_action(Replace).
Also y/n interactive loop keeps find_next (one scan per keypress, fine).

D2 sort_by_cached_key (main.rs:1177-1181): `region.sort_by_cached_key(|l| l.iter().collect::<String>().to_lowercase())`. Green (no behavior change).

D3 undo region-based. Protective tests FIRST (green on snapshot impl):
undo_types_coalesce (type abc, M-U once → empty), undo_backspace_run,
undo_redo_roundtrip, undo_repeated_cut_coalesce (3x^K line cuts, one M-U → all
back), undo_paste, undo_replace_all_one_step, undo_limit_trims (501 steps, len 500).
Then replace (main.rs:102-104, 306-343):
```rust
struct UndoStep {
    kind: ActionKind,
    start: usize,           // row index, pre-edit coords
    before: Vec<Vec<char>>, // rows [start, start+before.len()) pre-edit
    after_start: usize,     // row index, post-edit coords
    after: Vec<Vec<char>>,  // post-edit rows
    cur_before: Pos, cur_after: Pos,
}
// undo: lines.splice(after_start..after_start+after.len(), before); cursor=cur_before
// redo: lines.splice(start..start+before.len(), after); cursor=cur_after
```
Recording pattern per action: `begin_action(kind, row_range)` snapshots before-rows;
`finish_step(delta)` fills after = lines[after_start .. after_start + before.len() + delta]
(delta = row_count change). Coalescing rules:
- Insert/Backspace/Delete: same kind AND edit stayed on the same single row AND
  existing step is single-row → update step.after[0] + cur_after (clone that line
  only). Row-crossing backspace (col 0 join) → new step.
- Cut: repeated ^K line-cuts at same cursor: extend step.before with newly removed
  pre-edit row (contiguity: next pre-edit row), after stays empty region; partial
  ^K then full ^K merges into before=[orig row], after=[].
- Others: always new step. UNDO_LIMIT trims front (now cheap: Vec::remove(0) on
  small structs; use VecDeque).
last_kind logic unchanged. delete_selection_if_any (main.rs:345-357) participates
in caller's begin/finish (it edits inside insert_char etc. — its region must be
covered by the caller's recorded range: record range = mark..cursor span ∪ cursor
row; simplest: begin_action for Insert etc. records rows min(mark_row,cursor_row)
..=max(...) when mark active, else cursor row).

D4 LSP debounce. Editor fields: `lsp_dirty: bool, lsp_last_send: Instant`.
edit_invalidate (main.rs:202-210) sets lsp_dirty=true INSTEAD of immediate change().
New `fn lsp_flush(&mut self, now: Instant)`: if dirty && now>=last+300ms →
l.change(&buf.text()), dirty=false, last_send=now. Called in run loop each tick
(main.rs:1523 area) and forced on save_to/quit. RED test: insert_char → lsp_dirty
true; lsp_flush(now+100ms) → still dirty; lsp_flush(now+400ms) → false.

D5 LSP async handshake. lsp.rs: keep spawn() (sync, used by tests); add:
```rust
pub fn spawn_async(lang, root: &Path, doc: &Path, text: &str)
    -> mpsc::Receiver<Result<LspClient, String>> // thread does spawn(), sends result
```
Editor: replace direct spawn in lsp_sync (main.rs:247-250) with storing
`lsp_starting: Option<mpsc::Receiver<Result<LspClient,String>>>`; lsp_poll adopts:
recv → Ok(client) → self.lsp=Some + client.change(current text) (catch up edits
made during handshake); Err(e) → flash. lsp_sync stops old client + clears diags
immediately as today. RED test: editor with lsp_starting preloaded Err("x") via
mpsc → lsp_poll → status flash contains "x", lsp None.

D6 ui draw. Extract from draw (ui.rs:77-90):
```rust
fn line_to_spans(chars: &[char], abs_row: usize, scroll_x: usize, max_w: usize, ed: &Editor) -> Line
```
coalescing consecutive equal styles into one Span (char_style per char, group
runs). RED test: line "abc" no styles → ONE span covering 3 chars; with selection
on middle char → 3 spans. Draw loop stops cloning lines (iterate slice
&ed.buf.lines[r][scroll_x..]). Dirty-flag draw in run (main.rs:1503-1524):
`let mut dirty = true;` draw only when dirty; set dirty on: key/paste/resize event,
tick_status()==true (make it return bool when it clears), lsp event adopted, exec
job finished, lsp_flush actually sent. Poll timeout stays 200ms.

D7 exec async. New src/exec.rs:
```rust
pub struct ExecJob { pub child: Child, pub rx: mpsc::Receiver<String>, pub err_rx: mpsc::Receiver<String>, pub cmd: String, pub insert_row: usize }
pub fn spawn_job(cmd: &str, insert_row: usize) -> io::Result<ExecJob> // sh -c, threads read stdout/stderr fully
```
Editor.exec_job: Option<ExecJob>; do_exec → spawn_job + flash "Running: cmd";
`fn exec_poll(&mut self)` each loop tick: child.try_wait() → Some(status): recv
stdout (blocking recv fine — reader finished), code 0 && non-empty →
begin_action(Exec) region step + insert_lines_at(insert_row) + flash "Ran: cmd";
non-zero → flash Exit code + first stderr line. None → keep polling.
RED test: spawn_job("printf hi", 0) + poll loop (sleep 10ms, try_wait) → inserted
"hi". Status bar shows "Running…" while active (status_text priority after flash).

## Phase E — §4 robustness/UX

E1 bracketed paste. `fn paste_text(&mut self, s: &str)`: one begin_action(Paste)
region step; split on '\n'; merge_inline first frag at cursor, insert_lines_at
rest, cursor at end of last frag. run loop: Event::Paste(t) => ed.paste_text(&t)
(main.rs:1513-1520 area); execute EnableBracketedPaste after EnterAlternateScreen,
DisableBracketedPaste before Leave (verify names in crossterm 0.29:
crossterm::event::{EnableBracketedPaste, DisableBracketedPaste}).
RED test: paste_text("ab\ncd") on empty → ["ab","cd"], cursor (1,2), one M-U → empty.

E2 panic hook. In run() before loop:
```rust
let prev = std::panic::take_hook();
std::panic::set_hook(Box::new(move |info| { let _ = disable_raw_mode(); let _ = crossterm::execute!(std::io::stdout(), LeaveAlternateScreen); prev(info); }));
```
No test (manual).

E3 horizontal scroll. Editor `pub scroll_x: usize`. `fn adjust_scroll_x(&mut self)`:
if cursor.col < scroll_x → scroll_x = cursor.col; if cursor.col >= scroll_x + text_w
→ scroll_x = cursor.col + 1 - text_w. Called next to adjust_scroll (main.rs:1507).
ui.rs text spans use window [scroll_x .. scroll_x+width) and cursor x =
cursor.col - scroll_x; char_style lookups use absolute col. RED test: line of 40
'a's, text_w=10, move_end + adjust_scroll_x → scroll_x == 31; move_home +
adjust_scroll_x → 0.

E4 prompt upgrades (all RED-first, pure where possible):
- tilde: `fn expand_tilde(p: &str) -> String` (~ → $HOME, ~/ rest). Used in
  do_write/do_read/do_backup before PathBuf::from. Test: expand_tilde("~") == home;
  "~/x" == home + "/x"; "/a" unchanged.
- history: Editor `histories: Vec<(PromptKindDiscriminant-ish, Vec<String>)>` —
  simplest: two Vec<String>: search_hist, exec_hist (+read/write share file_hist).
  In prompt mode: Up/Down cycle (hist_idx), Enter appends (non-empty, dedupe
  adjacent). RED test: search "foo" → reopen search prompt → Up prefills "foo".
- completion: `fn complete_path(prefix: &str) -> Option<(String, Vec<String>)>`
  (split at last '/', read_dir parent, filter starts_with file part, longest common
  prefix + sorted list). Tab in WriteName/ReadName/BackupName/FilterCmd prompts:
  unique → complete + trailing '/'; ambiguous → complete common prefix + flash up
  to 5 options; none → flash. RED test with std::env::temp_dir() fixture dir
  (files alpha.txt, alphabet/ dir): prefix "alp" → "alpha"; prefix "a" → common
  prefix "alpha" + 2 options.
- word motion: M-b/M-f and Ctrl+Left/Right in prompts: move p.cursor by word over
  chars().collect(). RED test: text "foo bar_baz", M-f from 0 → 4 (after "foo "),
  M-f → 12; M-b back.

E5 diagnostics. lsp::Diagnostic gains `pub end_col: usize` (parse
range.end.character, lsp.rs:161-174). char_style (main.rs:1188-1210): after
selection check, before syntax: if p inside any diag (d.line==p.row &&
d.col<=p.col<p.end_col) → base style .fg(Red).add_modifier(UNDERLINED) for
severity 1, Yellow for 2, Blue underline otherwise; search/selection still win.
M-D binding (main.rs alt arm): jump_next_diag(): next diag with line > cursor.row
else wrap to first; cursor = (line, col). RED tests: (1) char_style at diag range
has Red fg; (2) jump order 1→3→1 with diags on lines 1,3; (3) cursor col set.

## Phase F — §5 features

F1 config. New src/config.rs:
```rust
#[derive(Debug, Clone)]
pub struct Config { pub tab_width: usize /*8*/, pub auto_indent: bool /*false*/, pub line_numbers: bool /*false*/, pub multibuffer: bool /*false*/ }
impl Config { pub fn load() -> Config } // ~/.config/rano/config.toml OR .ranorc? -> XDG path; key = value lines, # comments, unknown keys ignored, bad values ignored
fn parse_config(text: &str) -> Config // pure, testable
```
RED tests: parse "tab_width = 4\nauto_indent = true" etc.; missing file → defaults.
main() loads and passes to Editor. Editor.config field.

F2 tab_width rendering. `fn display_width(chars: &[char], tab_width: usize) -> usize`
(tab advances to next multiple). ui::draw uses per-char display cols for spans
(tab char renders as spaces up to next stop); cursor x = display col of cursor.col
- scroll_x (scroll_x now in DISPLAY cols; adjust_scroll_x math uses display widths).
justify (main.rs:1125-1165) uses display width for wrapping. RED tests:
display_width(["a","b","\t"]) tab8 → 8; tab4 at col 3 → 4; pure fn tests +
scroll_x test updated to tabs. Cursor mapping fn `fn display_col(line:&[char], col:usize, tw:usize) -> usize`.

F3 auto-indent. newline() (main.rs:369-377): if config.auto_indent: indent =
leading ws chars of current line; buf.newline(); extend new row with indent;
cursor.col = indent.len(). RED test: line "    foo", cursor EOL, Enter → row1
starts "    ", col 4.

F4 line numbers. M-N toggle: Editor.show_line_numbers (init from config). Pure fn
`fn gutter_width(rows: usize) -> usize { max(2, digits(rows)) + 1 }`. draw: gutter
column right-aligned numbers, dim gray; text area x = gutter_width; cursor x +=
gutter; visible text width = width - gutter. RED test: gutter_width(9)==3,
gutter_width(10)==4.

F5 search upgrades. Cargo.toml += `regex = "1"`. SearchState (main.rs:29-34):
matches becomes `Vec<(Pos, usize)>` (pos + match len), gains `pub regex: bool,
pub case_sensitive: bool` (default true = nano). New src/search.rs:
```rust
pub enum Matcher { Literal { needle: String, case_sensitive: bool }, Re(regex::Regex) }
impl Matcher { pub fn find_all(&self, lines: &[Vec<char>]) -> Vec<(Pos, usize)> }
```
do_search builds Matcher (regex mode: Regex::new(query) — flash error on bad
pattern, keep old). Prompt bindings M-C (case toggle) / M-R (regex toggle) while
Search prompt active — flash new state, stay in prompt. current_match_range uses
stored len. Replace stays literal-only (ignore regex mode; flash notice if regex
active on ^\). RED tests: find_all case-insensitive ("Foo"/"foo" match), regex
"a.c" matches "abc", match len for regex > literal. Buffer::find_all/find_next
stay (used by replace).

F6 filter region. Binding M-| (KeyCode::Char('|') + ALT) → requires mark else
flash; PromptKind::FilterCmd; on Enter: sync Command sh -c with stdin = region
text (normalize(mark,cursor) full lines? nano filters whole marked lines incl.
partial? Use exact region [a,b) rows a..=b), stdout replaces region; one undo
step kind Filter (region step covers old rows → new rows). Exec failure → flash
exit code, region unchanged. RED test: buffer "hello\nworld", mark (0,0)-(0,5),
filter "tr a-z A-Z" → line0 "HELLO", undo restores.

F7 new langs. Cargo.toml += tree-sitter-python, tree-sitter-c, tree-sitter-json
(pick versions with tree-sitter-language LANGUAGE API; try python 0.23, c 0.24,
json 0.25 — adjust to whatever compiles against tree-sitter 0.27). syntax.rs:
Lang::{Python,C,Json} + language()/query() arms (HIGHLIGHTS_QUERY for all three;
json uses HIGHLIGHTS_QUERY too), detect: py/pyw→Python, c/h→C, json→Json.
lsp.rs command_for: Python→("pylsp",&[]), C→("clangd",&[]), Json→
("vscode-json-language-server",&["--stdio"]); language_id: python/c/json.
main.rs lsp_sync root markers: Python→"pyproject.toml" fallback dir; C→
"compile_commands.json" fallback dir; Json→dir. RED tests mirror highlights_go
(python: "def fn()" keyword; c: "int main"; json: "key" property).

F8 multi-buffer. Restructure Editor (main.rs) into per-pane state:
```rust
pub struct Pane {
    pub buf: Buffer, pub cursor: Pos, pub scroll: usize, pub scroll_x: usize,
    pub mark: Option<Pos>, pub loc_until: Option<Instant>,
    pub undo: Vec<UndoStep>, pub redo: Vec<UndoStep>, pub last_kind: Option<ActionKind>,
    pub search: SearchState, pub lsp_diags: Vec<lsp::Diagnostic>, pub lsp_dirty: bool, pub lsp_last_send: Instant,
}
pub struct Editor { pub panes: Vec<Pane>, pub active: usize, /* global: cut, cut_line, config, prompt, help, status, replace state?, exec_job, lsp (per active pane's doc), lsp_starting, text_w/h */ }
```
replace/replace_pos/replace_count stay global (replace operates on active pane).
Editor methods use `fn pane(&self) -> &Pane` / `pane_mut()`. Switch bindings:
M-> next, M-< prev (crossterm Char('>')+ALT / Char('<')+ALT): save nothing (panes
live in RAM), lsp_sync() on switch (restarts LSP for new pane's file), flash
"[buf 2/3] name". ^R when config.multibuffer: do_read targets a NEW pane
(instead of inserting into current) — read file, push Pane with cursor 0, switch
to it. Scratch pane (no name) allowed. char_style/ui read via ed.pane().
PROTECTIVE tests (write against single-pane Editor FIRST in D3/B phases so they
survive refactor): all editor tests use helpers test_ed()/press() — after
refactor only helpers change. New tests: two panes cursor independence, modified
flags independent, undo stacks independent, switch flashes index.

## Phase G — §6 code health

G1 split main.rs. Final layout:
- main.rs: mod decls, main() arg parse, run() loop, panic hook, key dispatch
  (handle_key/handle_prompt_key stay on Editor? move handle_key to editor.rs —
  Editor lives in editor.rs with actions; main.rs keeps only run loop + main()).
- editor.rs: Editor/Pane/Flash/ActionKind/normalize/plural + all action methods.
- search.rs: Matcher, SearchState, search/replace methods.
- prompt.rs: Prompt/PromptKind, handle_prompt_key, complete_path, expand_tilde,
  history.
- undo.rs: UndoStep.
- exec.rs, bindings.rs, config.rs already exist by then.
Mechanics: create files, move code verbatim (pub(crate) where needed), cargo
check after each move. No logic changes in this step.

G2 version: ui.rs `const TITLE_LEFT: &str = concat!("  rano ", env!("CARGO_PKG_VERSION"));`
Green test: title_line(40,"",false,"") starts_with("  rano 0.1.0").

G3 CI: .github/workflows/ci.yml — jobs.test: runs-on ubuntu-latest, steps:
checkout, dtolnay/rust-toolchain@stable, `rustup component add rust-analyzer`
(LSP e2e test needs it), cargo fmt -- --check, cargo clippy -- -D warnings,
cargo test -- --include-ignored? NO: LSP test gets #[ignore] (G4) and CI runs
`cargo test` + `cargo test -- --ignored` as separate step so the e2e still runs
in CI but not locally by default.

G4 `#[ignore]` on lsp::tests::rust_diagnostics_flow.

Final: cargo fmt, cargo clippy --fix, cargo test (all green), tick every
completed [x] in TODO.md, rewrite README keys table from bindings.rs + new
features (config, filter, buffers, regex search, langs), update STATE here.

## Test harness conventions (editor tests)

```rust
#[cfg(test)] mod ed_tests {
    fn test_ed(text: &str) -> Editor { /* Buffer from text, name None, Editor::new, text_w=80, text_h=24 */ }
    fn press(ed: &mut Editor, code: KeyCode, mods: KeyModifiers) { ed.handle_key(KeyEvent::new(code, mods)) }
    fn lines(ed: &Editor) -> Vec<String> { ed.pane().buf.lines → strings }  // pane() only after F8
}
```
Place in main.rs `mod tests` until G1, then editor.rs.

## Order of execution

A → B → C → D1,D2 → D3 → D4,D5 → D6,D7 → E1..E5 → F1..F7 → F8 → G.
Run `cargo test` after each item; `cargo build` after Cargo.toml changes.
Update this STATE section after each phase completes.
