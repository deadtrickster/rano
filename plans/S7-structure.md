# S7 — Structure: new languages (F7), bindings single source (C2), multi-buffer (F8), module split (G1), version/CI/ignore (G2–G4)

Grounded to the code as of 2026-09-06 (main.rs 1528 lines, Editor at main.rs:79-105).
Execution order per PLAN.md: C2 runs in Phase C; F7 in Phase F (before F8); F8 last
before Phase G; G1–G4 close out. All line anchors in this file refer to the
*current* tree unless marked "post-X"; every item MUST re-grep its anchors at
execution time (cheaper than trusting this file).

---

## F7 — New languages: Python, C, JSON

### Anchors
- `Cargo.toml:6-14` dependency list (tree-sitter 0.27, rust 0.24, go 0.25, bash 0.25).
- `src/syntax.rs:14-19` `Lang` enum; `:21-28` `language()`; `:30-37` `query()`;
  `:40-48` `detect()`; `:1` module doc ("rust, go and bash").
- `src/lsp.rs:78-88` `command_for`; `:90-96` `language_id`.
- `src/main.rs:241-245` `lsp_sync` root-marker match (`Rust→Cargo.toml`,
  `Go→go.mod`, `Bash→dir`).
- `src/syntax.rs:247-254` `detects_languages` test — line 252 asserts
  `detect("x.py") == None`; must flip to `Some(Lang::Python)` in this item.
- `src/syntax.rs:274-282` `highlights_go` — the pattern new tests mirror.

### Design
Cargo.toml += (versions verified, see Tests/Risks):
```toml
tree-sitter-python = "0.25"   # 0.25.0 — LANGUAGE + HIGHLIGHTS_QUERY
tree-sitter-c      = "0.24"   # 0.24.2 — LANGUAGE + HIGHLIGHT_QUERY  (singular!)
tree-sitter-json   = "0.24"   # 0.24.8 — LANGUAGE + HIGHLIGHTS_QUERY
```
Verified against crates.io API: all three latest versions declare
`tree-sitter-language ^0.1` as a *normal* dependency (the `LANGUAGE` API, same
mechanism as rust 0.24.2 / go 0.25.0 / bash 0.25.1 in Cargo.lock:1497-1530).
A throwaway crate (`/tmp/opencode/tscheck`) with `tree-sitter = "0.27"` plus
these three compiled clean, `LANGUAGE.into()` → `tree_sitter::Language` works,
and a real `Parser::parse` of each language produced error-free trees — ABI
compatible with tree-sitter 0.27. Export-name gotcha: `tree_sitter_c` exposes
`HIGHLIGHT_QUERY` (no `S`), matching `tree_sitter_bash`'s spelling
(syntax.rs:34); python and json use `HIGHLIGHTS_QUERY`.

syntax.rs changes:
- `Lang::{Python, C, Json}` variants.
- `language()`: `tree_sitter_python::LANGUAGE.into()`, `tree_sitter_c::LANGUAGE.into()`,
  `tree_sitter_json::LANGUAGE.into()`.
- `query()`: Python → `tree_sitter_python::HIGHLIGHTS_QUERY`,
  C → `tree_sitter_c::HIGHLIGHT_QUERY`, Json → `tree_sitter_json::HIGHLIGHTS_QUERY`.
- `detect()`: `"py" | "pyw" => Python`, `"c" | "h" => C`, `"json" => Json`.
- Module doc line 1: "rust, go, bash, python, c and json".

lsp.rs changes:
- `command_for`: `Python → ("pylsp", &[])`, `C → ("clangd", &[])`,
  `Json → ("vscode-json-language-server", &["--stdio"])`.
- `language_id`: `"python"`, `"c"`, `"json"`.

main.rs `lsp_sync` root markers (main.rs:241-245):
```rust
syntax::Lang::Python => lsp::find_project_root(&dir, "pyproject.toml"),
syntax::Lang::C      => lsp::find_project_root(&dir, "compile_commands.json"),
syntax::Lang::Json   => dir,
```
`find_project_root` (lsp.rs:100-110) already falls back to `start` when no
marker is found, so the "fallback dir" behavior is automatic. Note: a Python
file inside a project that also has `setup.py`/`requirements.txt` but no
`pyproject.toml` roots at the file's dir — acceptable; pylsp doesn't need the
root to be exact.

### Tests
Red (all in existing `mod tests`, mirroring `highlights_go` syntax.rs:274-282):
- `highlights_python`: `buf_named("t.py", "def fn():\n    return 1\n")` →
  `style_at(0,0)` is Some ("def" keyword), `style_at(1,4)` is Some ("return").
- `highlights_c`: `buf_named("t.c", "int main(void) { return 0; }\n")` →
  `style_at(0,0)` Some ("int" keyword), `style_at(0,4)` Some ("main" function).
- `highlights_json`: `buf_named("t.json", "{\"key\": 1}\n")` → `style_at(0,1)`
  Some ("key" property).
- `detects_languages`: change line 252 to `x.py → Some(Lang::Python)`; add
  `x.pyw → Python`, `x.c → C`, `x.h → C`, `x.json → Json`.
- lsp.rs `language_id_and_command_for`: assert
  `language_id(Lang::Python) == "python"`, `language_id(Lang::C) == "c"`,
  `language_id(Lang::Json) == "json"`; assert `command_for` arg tuples for the
  three langs (second tuple element: `[]`, `[]`, `["--stdio"]`; do NOT assert
  the resolved binary path — `find_bin` is environment-dependent).
Red first, then implement, then green. No server e2e tests (pylsp/clangd not
guaranteed installed; rust_diagnostics_flow remains the only e2e).

### Touches
Cargo.toml (+3 deps), Cargo.lock (regenerated), src/syntax.rs, src/lsp.rs,
src/main.rs (lsp_sync arms only). No src/ restructure.

### Risks
- Version drift: a future 0.26/0.25.1 of python etc. could change the API.
  Caret specs pin `<0.next-major` only. **Build verification required in-repo**:
  after editing Cargo.toml run `cargo build`; if a grammar fails to compile or
  lacks `LANGUAGE`, fall back within the verified list (python 0.23.6,
  c 0.23.4, json 0.23.0 — all also tree-sitter-language-based per crates.io).
- `h` → C is a guess for headers; `.hpp` deliberately NOT mapped (no C++ crate).
- pylsp/clangd/vscode-json-language-server may be absent on user machines —
  behavior matches today's missing-server path (flash on spawn failure,
  main.rs:249).
- json queries can be noisy (punctuation styles) — theme() fallback handles
  dotted/unmatched captures (syntax.rs:70-74); no theme changes planned.

---

## C2 — Bindings single source (src/bindings.rs)

### Anchors
- `src/ui.rs:24-51` `bar_items()` — the 24-entry table (ALREADY correct: says
  `^F Where Is`, `^B Where Was`, `M-B Previous`, `M-F Next`).
- `src/ui.rs:192-230` help overlay — STALE: lines 196-199/209 say `^W Where Is`,
  `^W searches`, `^Y to the previous` (code binds ^F/^B search, M-B/M-F match
  motion); line 202 says "F6 Do Command" (code: F6 Execute).
- `src/ui.rs:141` `let items = bar_items();`; `src/ui.rs:19` TITLE_LEFT (G2's).
- README.md:25-51 keys table — stale (^W/^Y/^B-prev-word/^F-forward-char);
  rewritten in Phase G, not here.
- Alt-arm collisions to respect: main.rs:1247 `M-D => prev_word`,
  main.rs:1248 `M-N => next_word` (see Conflicts).

### Design
New `src/bindings.rs`:
```rust
pub struct Binding { pub key: &'static str, pub label: &'static str }
pub static BAR: [Binding; 24] = [ /* exact copy of ui.rs:25-50 tuples */ ];
pub fn help_lines() -> Vec<String>; // overlay content
```
- `help_lines()` builds the nano-4-column key grid **from BAR at runtime**
  (`format!("{:<4} {:<12}", b.key, b.label)` into fixed 4-per-row cells) plus
  static prose lines (movement/editing hints, mark/copy prose, search prose
  rewritten to `^F searches; ^B searches backwards; M-B/M-F jump to the
  previous/next match`, the "Press any key" line). Return `Vec<String>` —
  runtime formatting from BAR means `&'static str` (PLAN.md's sketch) is not
  achievable; deviation noted.
- ui.rs: delete `bar_items()` (ui.rs:24-51); `draw` uses `bindings::BAR`
  (field access `b.key`/`b.label` at ui.rs:157); help overlay (ui.rs:192-230)
  becomes `let text = bindings::help_lines();` keeping the existing
  yellow-bold "Press any key" special case (match on content prefix).

FINAL intended binding set — the table + help + README-regeneration procedure
are owned HERE; behavior rows are owned by the item that implements them.
An item adds its BAR row(s) + help prose as part of its own work; C2 never
advertises a key whose behavior doesn't exist yet.

| Key | Label | BAR row | Owner | Notes |
|---|---|---|---|---|
| ^G | Help | yes (C2) | existing | |
| ^X | Exit | yes (C2) | existing | |
| ^O | Write Out | yes (C2) | existing | |
| ^R | Read File | yes (C2) | existing | F8 changes behavior (new pane) |
| ^F | Where Is | yes (C2) | existing | |
| ^B | Where Was | yes (C2) | existing | |
| ^\ | Replace | yes (C2) | existing | also ^\ arrives as ^4 (main.rs:1266) |
| ^K | Cut | yes (C2) | existing | |
| ^U | Paste | yes (C2) | existing | |
| ^T | Execute | yes (C2) | existing | |
| ^J | Justify | yes (C2) | existing | |
| ^C | Location | yes (C2) | existing | C3 makes it auto-clear |
| ^/ | Go To Line | yes (C2) | existing | arrives as ^7 too (main.rs:1269) |
| M-U / M-E | Undo / Redo | yes (C2) | existing | |
| M-A / ^A | Set Mark | yes (C2) | existing | |
| M-6 | Copy | yes (C2) | existing | |
| M-] | To Bracket | yes (C2) | existing | |
| M-B / M-F | Previous / Next match | yes (C2) | existing | |
| ^Left / ^Right | Prev / Next Word | yes (C2) | existing | ◂/▸ glyphs ui.rs:48-49 |
| M-D | Next Diag | added by E5 | **E5** | displaces M-D=prev_word |
| M-\| | Filter | added by F6 | **F6** | |
| M-> / M-< | Next / Prev Buffer | added by F8 | **F8** | |
| M-N | Line Numbers | added by F4 | **F4** | displaces M-N=next_word |
| M-C / M-R | case / regex toggle | help prose only (F5) | **F5** | prompt-local, not in BAR |
| Tab / M-b / M-f / ^Left/^Right | prompt word-motion/completion | help prose only (E4) | **E4** | prompt-local |

README regeneration procedure (executed in Phase G final, owned by C2's spec):
1. Regenerate the `## Keys` table from `bindings::BAR` (key, label columns) —
   manually or via a throwaway `cargo test` print; 2. append the prompt-local
   bindings (M-C/M-R/Tab/M-b/M-f) and feature sections (config, filter,
   buffers, regex search, new languages) by hand; 3. optional green test:
   `include_str!("../README.md")` contains every `BAR[i].key` literal.

### Tests
Red (new `#[cfg(test)] mod tests` in bindings.rs; ui.rs has no test mod yet —
test lives next to the source it pins):
- `help_is_accurate`: join `help_lines()` → must NOT contain `"^W"` and must
  contain `"^F Where Is"` and `"^B Where Was"`. Red against the current inline
  ui.rs text (196-209 has ^W/^Y) — so this test must live somewhere it can run
  pre-move: put the RED test temporarily in ui.rs's new test mod asserting the
  same properties against what `draw` renders (or, simpler, write the test in
  bindings.rs and accept that "red" is demonstrated by running the equivalent
  assertions against the old ui.rs strings before deleting them). Concrete red
  protocol: (1) add `ui::help_text_still_stale` test asserting the CURRENT
  overlay contains "^W" (green, documents the bug), (2) implement bindings.rs,
  (3) replace with the real assertions above (red → implement swap → green),
  (4) delete the staleness test.
- `bar_matches_help`: every `BAR` key/label pair appears in `help_lines()`
  output (keeps the two surfaces in lockstep forever).
- `bar_count`: `BAR.len() == 24` at C2 time (update the constant as later items
  append rows; the assert keeps column math honest).
Green: `cargo test` full suite; visual check `cargo run` bar/help unchanged
except corrected keys.

### Touches
NEW src/bindings.rs; src/ui.rs (delete bar_items, overlay text, one import);
main.rs `mod bindings;`. Later items append rows: E5 (M-D), F4 (M-N), F5
(prose), F6 (M-|), F8 (M->/M-<). Phase G rewrites README per procedure.

### Risks
- The bar layout math (ui.rs:134-172) is column-major and handles any
  `BAR.len()`; odd counts leave one blank cell — verify visually at 80 cols
  with 24 items (fits: total = min(24, ((80+40)/20)*2) = 24).
- help_lines() width: prose lines are hand-wrapped; no test on wrapping, only
  content. Terminal narrower than the longest help line just clips (Paragraph).
- M-D/M-N collisions resolved in favor of E5/F4 — until those land, prev_word/
  next_word keep M-D/M-N; the BAR must NOT contain M-D/M-N rows before then
  (dead-key advertising). Guarded by `bar_count` + review.

---

## F8 — Multi-buffer refactor (THE big one)

### Anchors (current tree — re-grep after B..F7 land; they WILL have shifted)
- `main.rs:22-77` Flash/SearchState/ReplaceState/PromptKind/Prompt/ActionKind.
- `main.rs:79-105` Editor struct; `:102-104` private undo/redo/last_kind.
- `main.rs:108-148` Editor::new; `:145-146` hl.refresh + lsp_sync at startup.
- `main.rs:150-210` clamp_cursor/flash/status_text/tick_status/adjust_scroll/
  edit_invalidate; `:212-299` LSP block; `:301-357` undo/redo/delete_selection;
  `:359-420` char input; `:422-517` cut/paste/copy; `:519-725` movement+bracket;
  `:718-725` toggle_mark; `:727-898` search+replace; `:901-917` goto;
  `:919-967` exec; `:969-1084` write/read/backup; `:1086-1121` quit;
  `:1123-1184` justify/sort; `:1186-1220` styling; `:1222-1441` key dispatch.
- Free fns: `normalize` main.rs:1443-1449, `plural` :1451-1457,
  `main` :1459-1490, `run` :1492-1528.
- `ui.rs:53-231` draw — reads ed.buf/cursor/mark/scroll/char_style at
  ui.rs:64-88, 175-189.
- Planned fields landing before F8 (from other items): C3 `loc_until`,
  D3 `UndoStep` undo/redo, D4 `lsp_dirty`/`lsp_last_send` + `lsp_flush`,
  D5 `lsp_starting`, D7 `exec_job`, E3 `scroll_x` + `adjust_scroll_x`,
  E4 `search_hist`/`exec_hist`, F1 `config`, F4 `show_line_numbers`,
  F5 SearchState regex/case fields.

### Design

Structs (full target):
```rust
pub struct Pane {
    pub buf: Buffer,                  // was Editor.buf
    pub hl: syntax::Highlighter,      // per-buffer style grid
    pub cursor: Pos,
    pub scroll: usize,
    pub scroll_x: usize,              // from E3
    pub mark: Option<Pos>,
    pub loc_until: Option<Instant>,   // from C3
    pub undo: Vec<UndoStep>,          // from D3 (was Editor undo, private)
    pub redo: Vec<UndoStep>,
    pub last_kind: Option<ActionKind>,
    pub search: SearchState,          // from F5: query, matches, current,
                                      //   backwards, regex, case_sensitive
    pub lsp_diags: Vec<lsp::Diagnostic>,
    pub lsp_dirty: bool,              // from D4
    pub lsp_last_send: Instant,       // from D4
}
pub struct Editor {
    pub panes: Vec<Pane>,
    pub active: usize,
    pub cut: Vec<Vec<char>>,          // shared cutbuffer
    pub cut_line: bool,
    pub config: Config,               // F1
    pub prompt: Option<Prompt>,
    pub help: bool,
    pub status: Option<Flash>,
    pub quit: bool,
    pub pending_write: Option<PathBuf>,
    pub quit_after_save: bool,
    pub replace: Option<ReplaceState>,   // replace trio stays global:
    pub replace_pos: Option<Pos>,        // operates on the active pane
    pub replace_count: usize,
    pub exec_job: Option<ExecJob>,       // D7
    pub lsp: Option<lsp::LspClient>,     // ONE server = active pane's doc
    pub lsp_starting: Option<mpsc::Receiver<Result<LspClient, String>>>, // D5
    pub text_w: usize,
    pub text_h: usize,
    pub show_line_numbers: bool,         // F4
    pub search_hist: Vec<String>,        // E4
    pub exec_hist: Vec<String>,          // E4
}
impl Editor {
    pub fn pane(&self) -> &Pane { &self.panes[self.active] }
    pub fn pane_mut(&mut self) -> &mut Pane { &mut self.panes[self.active] }
}
```
`hl` goes into Pane (PLAN.md's F8 sketch omits it — deviation, deliberate:
the style grid is per-buffer; keeping it global would force a re-parse on
every switch and lose nothing but correctness). `search` per pane because
matches index into that buffer's text. `lsp` stays global: LspClient tracks
exactly one open doc (lsp.rs doc comment) — it serves whichever pane is
active; switching restarts it.

Complete field catalog (current main.rs line → destination):

| Field | Now | F8 dest | Rationale |
|---|---|---|---|
| buf | 80 | Pane.buf | per-buffer |
| hl | 81 | Pane.hl | per-buffer grid |
| lsp | 82 | Editor (global) | single active-doc server |
| lsp_diags | 83 | Pane.lsp_diags | per-doc; stale in inactive panes (ok) |
| cursor | 84 | Pane.cursor | |
| scroll | 85 | Pane.scroll | |
| mark | 86 | Pane.mark | |
| cut | 87 | Editor (global) | nano's shared cutbuffer |
| cut_line | 88 | Editor (global) | |
| search | 89 | Pane.search | matches are per-buffer |
| prompt | 90 | Editor (global) | one prompt at a time |
| help | 91 | Editor (global) | |
| status | 92 | Editor (global) | |
| show_loc → loc_until (C3) | 93 | Pane.loc_until | describes active cursor |
| quit | 94 | Editor (global) | |
| pending_write | 95 | Editor (global) | |
| quit_after_save | 96 | Editor (global) | |
| replace | 97 | Editor (global) | runs on active pane |
| replace_pos | 98 | Editor (global) | |
| replace_count | 99 | Editor (global) | |
| text_w | 100 | Editor (global) | viewport, not buffer |
| text_h | 101 | Editor (global) | |
| undo (priv) | 102 | Pane.undo (Vec<UndoStep> post-D3) | |
| redo (priv) | 103 | Pane.redo | |
| last_kind (priv) | 104 | Pane.last_kind | coalescing is per-buffer |
| panes / active | — | NEW | |
| config (F1) | Editor | Editor (global) | |
| exec_job (D7) | Editor | Editor (global) | |
| lsp_starting (D5) | Editor | Editor (global) | |
| lsp_dirty / lsp_last_send (D4) | Editor | Pane | per-doc debounce |
| scroll_x (E3) | Editor | Pane | |
| show_line_numbers (F4) | Editor | Editor (global) | UI toggle |
| search_hist / exec_hist (E4) | Editor | Editor (global) | |

Complete method catalog (main.rs line → classification):

| Method | Now | Class | Port notes |
|---|---|---|---|
| new | 108 | Editor (rewritten) | builds `panes: vec![Pane::new(buf)]`, active 0, then hl.refresh+lsp_sync as today |
| clamp_cursor | 150 | pane() | |
| flash | 154 | Editor (global) | untouched |
| status_text | 161 | Editor, reads pane() | status + pane().loc_until + lsp_status |
| tick_status | 180 | Editor, pane_mut() | clears global status + ACTIVE pane's expired loc_until (C3 semantics) |
| adjust_scroll | 188 | pane_mut() | + E3 adjust_scroll_x same |
| edit_invalidate | 202 | Editor, pane_mut() | pane.buf.modified/search/hl (+D4: pane.lsp_dirty=true); D4 removes the direct `l.change` |
| lsp_sync | 216 | Editor (mixed) | clone pane().buf.name to a local first; writes self.lsp; clears pane_mut().lsp_diags; root markers per F7 |
| lsp_poll | 255 | Editor (mixed) | self.lsp.poll() → pane_mut().lsp_diags; D5 adoption of lsp_starting |
| lsp_status | 267 | Editor, reads pane() | diags + cursor |
| lsp_flush (D4) | — | Editor (mixed) | pane dirty/last_send + global lsp |
| begin_action | 308 | pane_mut() | post-D3 signature |
| undo / redo | 319/332 | pane_mut() | |
| delete_selection_if_any | 345 | pane_mut() | |
| insert_char / newline / backspace / delete_at | 361-420 | pane_mut() | newline gains F3 auto-indent |
| cut | 424 | mixed | pane buf/cursor/mark + GLOBAL cut/cut_line; locals pattern (below) |
| paste | 464 | mixed | reads global cut; writes pane |
| copy | 492 | mixed | pane → global cut |
| delete_char_cut | 506 | mixed | pane + global cut |
| paste_text (E1) | — | pane_mut() | |
| move_left/right/up/down/home/end | 521-561 | pane_mut() | B2 fix included |
| page_up / page_down | 563-573 | pane_mut() | take text_h arg (global) |
| prev_word / next_word / next_line / prev_line | 575-643 | pane_mut() | M-D/M-N arms per Conflicts |
| match_bracket | 647 | pane_mut() + flash | |
| find_matching | 677 | pane() (&self) | |
| toggle_mark | 720 | pane_mut() | |
| start_search / start_search_backward | 729/743 | Editor (mixed) | read pane().search; write global prompt |
| do_search | 756 | Editor (mixed) | pane search+buf; flash (moves to search.rs in G1) |
| next_match / prev_match / jump_to_match | 783-803 | pane_mut() | |
| start_replace | 807 | Editor (global prompt) | |
| next_replace_ask / answer_replace_ask | 815/844 | Editor (mixed) | global replace trio + pane buf/cursor; D1 replace_all_from inside 'a' arm |
| start_goto / do_goto | 903/911 | Editor / pane_mut() | do_goto sets pane cursor + loc_until |
| start_exec / do_exec | 921/929 | Editor (global prompt) / mixed | D7: spawn_job global, exec_poll inserts into pane |
| filter (F6) | — | Editor (mixed) | pane region + flash |
| start_write / do_write | 971/985 | Editor (global) | reads pane().buf.name |
| save_to | 1002 | mixed | pane buf.name/modified/hl + global quit_after_save + lsp_sync |
| start_read / do_read | 1023/1031 | Editor / mixed | do_read: `if config.multibuffer && active pane not empty-unnamed → push Pane, active=len-1, lsp_sync, flash "Read N lines [i/n]" else today's insert path` |
| start_backup / do_backup | 1060/1076 | Editor (global) | reads pane().buf.name |
| try_quit | 1088 | Editor (mixed) | EXTENDED: quit only if NO pane modified; else ConfirmSave; 'y' saves active, and if other panes still modified flash "N other modified buffer(s)" instead of quitting (re-press ^X to confirm each) |
| answer_save | 1100 | Editor (mixed) | via save_to |
| justify | 1125 | mixed | read text_w into local, then pane_mut; F2 display widths |
| sort_lines | 1167 | pane_mut() | D2 cached key |
| char_style | 1188 | pane() (&self) | pure per-pane read (search/mark/hl + E5 diags); Editor keeps forwarding `pub fn char_style` so ui.rs call sites survive |
| current_match_range | 1212 | pane() | |
| handle_key | 1224 | Editor (dispatch) | + M->/M-< arms in the alt match |
| switch_pane (NEW) | — | Editor | next/prev: active = (active±1) % len; lsp_sync(); flash `"[{}/{}] {}"` with name or "(scratch)"; NO state save (panes live in RAM) |
| handle_prompt_key | 1324 | Editor (global) | prompt/replace/histories/pending_write; E4 word-motion |
| normalize / plural | 1443/1451 | free fn | pub(crate) at G1 |

ui.rs changes: every `ed.buf` → `ed.pane().buf` (name/modified/lines at
ui.rs:64-69, 78-79), `ed.cursor` → `ed.pane().cursor` (183-186), `ed.mark` →
`ed.pane().mark` (70), `ed.scroll` → `ed.pane().scroll` (78); `ed.char_style`
stays (Editor forwards). D6's `line_to_spans` keeps `&Editor` and uses
`ed.pane()` internally. Title bar could show `[i/n]` — NOT in scope; the
switch flash carries the index.

Borrow-checker pattern for mixed methods (the one real hazard — see Risks):
```rust
// BAD (post-refactor): pane_mut() borrows all of self
// let p = self.pane_mut(); self.cut = p.buf.cut_range(..);
// GOOD: end the pane borrow before touching globals
let (cut, cur, cl) = { let p = self.pane_mut(); (p.buf.cut_range(a,b), p.cursor, p.clamped()) };
self.cut = cut; /* ... */
```
and for edit_invalidate-style "read pane, then global lsp":
`let text = self.pane().buf.text(); if let Some(l) = self.lsp.as_mut() { l.change(&text); }`
(D4 makes this moot but the pattern applies to lsp_sync/lsp_poll).

### Tests
Red (written BEFORE the refactor where the Editor API suffices):
- `switch_flash_next_prev`: test_ed + press(ALT+'>') → status_text() contains
  "[2/2]"; red now (M-> unbound, no flash). With two panes pre-seeded via a
  test-only `ed.panes.push(...)` — only possible post-struct; so this test
  lands in two steps: `switch_flash_red` (single-pane, red pre-refactor) then
  the full version post-refactor.
Green/protective:
- ALL existing editor tests must pass UNCHANGED through the refactor — they
  go through `test_ed()`/`press()` (planned by others, PLAN.md:381-390), so
  only the helpers change: `test_ed` unchanged (Editor::new wraps in a Pane);
  `lines(ed)` helper reads `ed.pane().buf.lines`.
- New post-refactor tests: `panes_cursor_independent` (move in p0, switch,
  p1.cursor == (0,0)); `panes_modified_independent` (insert in p0, p1.buf.modified
  == false, p0 true); `panes_undo_independent` (type in p0, switch, M-U in p1
  is a no-op, p0 still undoes); `switch_flash_index` ("[1/2]"/"[2/2]" on M-</M->);
  `switch_restarts_lsp` (two named .rs panes in tmpdir; after switch,
  ed.lsp.doc_path == pane1 name — requires lsp; mark `#[ignore]`-style env
  guard like rust_diagnostics_flow, or assert only lsp_diags cleared +
  lsp None for a scratch pane: prefer the scratch-pane variant, no server needed);
  `read_new_pane_when_multibuffer` (config.multibuffer=true, do_read →
  panes.len()==2, active==1, content matches); `read_inserts_when_single`
  (regression: multibuffer=false → today's behavior); `try_quit_other_modified`
  (p1 modified, ^X on unmodified p0 → ConfirmSave appears, not quit).

### Migration ORDER (mechanical, `cargo check` between every step)
1. **Pre-flight**: full suite green; record baseline. Add `switch_flash_red`
   (red) — documents the missing binding.
2. **Introduce the structs**: define `Pane` (all per-pane fields above),
   restructure `Editor` to `{panes, active, globals}` (delete the per-pane
   fields), add `pane()`/`pane_mut()`, rewrite `Editor::new` +
   a `Pane::new(buf)` (hl.refresh inside). `cargo check` → wall of errors;
   that list is the work queue.
3. **Pure per-pane methods** via pane()/pane_mut(): movement (521-643),
   toggle_mark, undo/redo/begin_action/delete_selection (post-D3 forms),
   char input (361-420), find_matching, current_match_range, char_style,
   sort_lines, next/prev_match, jump_to_match, adjust_scroll(_x), do_goto,
   clamp_cursor. `cargo check`.
4. **Mixed methods** with the locals pattern: cut/paste/copy/delete_char_cut,
   start_search*/do_search, start_replace/next_replace_ask/answer_replace_ask,
   justify, save_to, do_write/do_read/do_backup/start_write/start_backup,
   try_quit/answer_save, start_goto/start_exec/start_exec prompt fns,
   do_exec/exec_poll, match_bracket. `cargo check`.
5. **LSP + status glue**: lsp_sync, lsp_poll, lsp_status, lsp_flush,
   edit_invalidate, tick_status, status_text. `cargo check` — should compile.
6. **ui.rs**: pane()-relative reads (6 call-site clusters). `cargo test` →
   FULL suite green. This is the refactor-complete checkpoint: zero behavior
   change, single pane, `panes.len() == 1` always.
7. **Behavior**: M->/M-< arms + switch_pane + lsp_sync-on-switch + flash;
   do_read multibuffer branch; try_quit all-panes rule. Flip
   `switch_flash_red` to the full switch tests; add the new green tests above.
8. **Full suite + `cargo clippy`**; update PLAN.md STATE.

### Touches
src/main.rs (restructure + ~15 new/changed methods), src/ui.rs (~8 read sites),
tests (helpers + ~8 new). Nothing else — deliberately after D/E/F1-F7 so their
fields arrive once, in their final home. G1's module split happens AFTER this;
F8 does not pre-split files.

### Risks
- Borrow-checker friction in mixed methods is the main time sink; the locals
  pattern resolves all known cases. If a method resists (cut's multi-step
  body), split it into `fn cut_pane(&mut Pane, ...) -> (Vec<Vec<char>>, ...)`
  returning globals-by-value.
- `undo`/`redo`/`last_kind` are currently PRIVATE (main.rs:102-104) — making
  them Pane-pub is fine (same module until G1); tests reach them via press()
  anyway.
- Inactive panes' lsp_diags/lsp_dirty go stale (no server attached) — cosmetic
  only; diags are re-pulled fresh from the new server after each switch.
- try_quit all-panes rule changes ^X semantics in single-buffer mode only when
  other panes exist — single-pane behavior byte-identical (suite guards it).
- Highlighter is not Clone — Pane must not derive Clone (it doesn't need to).
- This refactor invalidates every main.rs line anchor in other agents' plans;
  see Conflicts.

---

## G1 — Module split

### Anchors
- Post-F8 main.rs (symbol-based, NOT line-based — F8 just moved everything):
  Editor/Pane/Flash/SearchState/ReplaceState/Prompt/PromptKind/ActionKind,
  all `impl Editor` blocks, normalize, plural, main, run, mod decls at top.
- Existing satellite modules: bindings.rs (C2), config.rs (F1), exec.rs (D7),
  undo.rs (D3 — if D3 put UndoStep there, G1 only verifies).

### Design — end-state map (other agents' code lands here; G1 is mechanical)

| Module | Contents | Written by (already, per plan) |
|---|---|---|
| main.rs | `mod` decls, `main()` (arg parse), `run()` loop, panic hook, event dispatch loop | E2 (hook) |
| editor.rs | Editor, Pane, Flash, ActionKind, normalize, plural, all action methods (movement, undo wrappers, cut/paste, justify, sort, goto, write/read/backup, quit, diagnostics jump, filter, switch_pane), `handle_key` | B2, C3, D1*, D3*, D4*, D5*, E1, E3, E5, F3, F4, F6, F8 |
| search.rs | SearchState, Matcher (F5), search/replace methods (start_search*, do_search, next/prev_match, jump_to_match, replace_all_from, current_match_range) | D1, F5 |
| prompt.rs | Prompt, PromptKind, handle_prompt_key, prompt_insert/prompt_backspace (B1 helpers), expand_tilde, complete_path, history, `pub(crate) fn prompt_label` (moved out of ui.rs:269-283) | B1, E4 |
| undo.rs | UndoStep, UNDO_LIMIT | D3 |
| exec.rs | ExecJob, spawn_job, exec_poll | D7 |
| bindings.rs | Binding, BAR, help_lines | C2 |
| config.rs | Config, parse_config | F1 |
| buffer.rs / syntax.rs / lsp.rs / ui.rs | unchanged layout | — |

(*D1/D4/D5 land in main.rs per their plans, then move with editor.rs/search.rs
at G1.)

`pub(crate)` requirements: `Editor`, `Pane`, `Flash`, `ActionKind`, `Prompt`,
`PromptKind`, `SearchState`, `Matcher`, `UndoStep`, `normalize`, `plural`,
`prompt_label`, `Editor::handle_key`, `Editor::char_style`, `Editor::status_text`,
`Editor::new`, plus every Editor field ui.rs touches (already `pub` — keep
`pub(crate)` minimum). Free fns main/run stay private in main.rs.
editor_test mod (test_ed/press/lines) moves main.rs → editor.rs.

Mechanical move order (verbatim moves, no logic edits, `cargo check` after
each): 1. undo.rs → 2. exec.rs, config.rs, bindings.rs (verify they're already
out) → 3. prompt.rs (Prompt/PromptKind/handle_prompt_key/B1+e4 helpers;
re-point ui.rs's prompt_label import) → 4. search.rs (SearchState/Matcher +
search/replace impl block) → 5. editor.rs (Editor/Pane/Flash/ActionKind/
normalize/plural/everything else of the impl) → 6. main.rs slims to mod decls +
main + run + panic hook; move the ed_tests mod to editor.rs → 7. `cargo fmt`,
full suite, clippy.

### Tests
Green-only (pure code motion): full suite identical before/after each step.
Add one canary: `mod smoke { #[test] fn modules_exist() }` is unnecessary —
the compiler is the test. No new behavior tests.

### Touches
NEW src/{editor,search,prompt,undo}.rs (exec/bindings/config exist); main.rs
gutted; ui.rs import fix (prompt_label); pub(crate) sweep.

### Risks
- Circular imports: none possible (same crate, plain `use crate::...`).
- `pub` → `pub(crate)` churn can break test mods — tests live in the same
  module tree, unaffected.
- Do NOT rename/refactor anything while moving (temptation risk); clippy fixes
  are a separate commit after the suite is green.

---

## G2 — Version from Cargo metadata

### Anchors
`src/ui.rs:19` `const TITLE_LEFT: &str = "  rano 0.1.0";`; ui.rs:236-267
`title_line` (uses `TITLE_LEFT.len()` — byte len; version is ASCII so len ==
char count; safe).

### Design
`const TITLE_LEFT: &str = concat!("  rano ", env!("CARGO_PKG_VERSION"));`

### Tests
Green: existing/Phase-A title_line test extended — `title_line(40, "", false, "")`
starts_with `"  rano 0.1.0"` (Cargo.toml version = 0.1.0).

### Touches
src/ui.rs:19 (one line), test.

### Risks
None. Bump version later and the title follows.

---

## G3 — CI workflow

### Anchors
Repo root (no .github/ today); lsp.rs e2e test needs rust-analyzer on PATH;
G4 marks it `#[ignore]` so CI runs it explicitly.

### Design
`.github/workflows/ci.yml`:
```yaml
name: CI
on: [push, pull_request]
jobs:
  test:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
        with:
          components: rustfmt, clippy
      - run: rustup component add rust-analyzer
      - run: cargo fmt -- --check
      - run: cargo clippy -- -D warnings
      - run: cargo test
      - run: cargo test -- --ignored
```

### Tests
None locally; validate by pushing and watching the run (or `act` if available).

### Touches
NEW .github/workflows/ci.yml.

### Risks
- `cargo clippy -D warnings` may surface lints from earlier phases — fix them
  in this item (separate commit), don't weaken the flag.
- `cargo test -- --ignored` runs the 20 s rust-analyzer e2e on every CI run —
  acceptable; it's the point of G4's split.
- dtolnay/rust-toolchain@stable + `rustup component add rust-analyzer` both
  needed (toolchain action doesn't install rust-analyzer by default).

---

## G4 — Ignore the slow LSP e2e test

### Anchors
`src/lsp.rs:442-484` `rust_diagnostics_flow` (the 20 s rust-analyzer e2e).

### Design
`#[ignore = "20 s rust-analyzer e2e; run with: cargo test -- --ignored"]` on
the test fn (keep the in-body server-absent skip too — harmless).

### Tests
Green: `cargo test` no longer runs it (27 → 27 visible, 1 ignored);
`cargo test -- --ignored` runs exactly it (plus any other ignored tests).

### Touches
src/lsp.rs (one attribute).

### Risks
None. Order: land G4 before G3 so CI's `cargo test` step is fast from day one.

---

## Conflicts (F8 invalidates other agents' anchors; cross-item contradictions)

1. **Anchor invalidation by F8**: every main.rs line anchor in B1 (1408-1431),
   B2 (521-529), C3 (1275), D1 (874), D2 (1177-1181), D3 (306-343, 345-357),
   D4 (202-210), D5 (247-250), D6 (main.rs 1503-1524), D7 (933), E1 (1513-1520),
   E3 (1507), E4 (981, 1067), E5 (1188-1210, alt arm 1239-1249), F2 (1125-1165),
   F3 (369-377), F5 (29-34), F6, and G1 (whole file) is dead after F8. Since
   execution order runs B..F7 BEFORE F8, those items are safe on first
   execution — but every item MUST re-grep its anchors at execution time
   anyway (B..F7 also shift each other). Items that run after F8 and must
   locate code by symbol, not line: **G1** (entire split), **G2** (ui.rs:19 is
   F8-stable — ui.rs only gets ed.pane() read changes), G3/G4 (unaffected).
   F8 itself must re-verify Editor@79-105 and the section anchors after F7.
2. **M-D collision**: main.rs:1247 binds M-D → prev_word ("nano alternate");
   E5 wants M-D → next diagnostic. Decision: **E5 wins**; prev_word keeps
   ^Left only. C2's BAR must not list M-D until E5 lands.
3. **M-N collision**: main.rs:1248 binds M-N → next_word; F4 wants M-N →
   line numbers. Decision: **F4 wins**; next_word keeps ^Right only.
4. **README vs code**: README.md:30-31/42/50-51 (^W/^Y/^B-prev-word/^F-
   forward-char/^L-refresh) and help overlay ui.rs:196-209 are stale. C2 fixes
   help; README rewrite is Phase G (C2 owns the procedure, G executes it).
5. **PLAN.md contradictions found**:
   - PLAN F8 Pane sketch omits `hl` — must be per-pane (this plan adds it).
   - PLAN C2 `help_lines() -> Vec<&'static str>` — impossible when built from
     BAR at runtime; returns `Vec<String>`.
   - PLAN F7 "HIGHLIGHTS_QUERY for all three" — `tree_sitter_c` is
     `HIGHLIGHT_QUERY` (singular, verified).
   - PLAN F8 "save nothing on switch" — kept; but try_quit needed an
     all-panes rule PLAN doesn't mention (specified here).
   - PLAN G3 says "cargo fmt --check" — correct invocation is
     `cargo fmt -- --check`.
6. **F8 vs F4 line numbers**: gutter width depends on the ACTIVE pane's row
   count (per-pane gutter) — F4's `gutter_width` call site must use
   `ed.pane().buf.lines.len()` post-F8. F4 lands before F8; F8's ui.rs step
   (migration step 6) owns adjusting that call site.
