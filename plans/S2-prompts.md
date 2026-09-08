# S2 — prompts: safety, expiry, upgrades, search toggles

Grounded to src/main.rs @ 1528 lines, src/ui.rs @ 284 lines (2026-09-06).
Scope: TODO §1 bug 1 (B1), §2 show_loc (C3), §4 prompt upgrades (E4), §5 search
toggles prompt side (F5). NOT in scope: find_all/Matcher core (S5 owns it),
FilterCmd prompt kind (F6 owns it), Pane refactor (F8), file split (G1).

Verified panic repro (rustc-checked, corrects TODO.md wording): typing `é` then
Backspace on an otherwise-empty prompt does NOT panic (`remove(0)` hits a
boundary). Real panics: (1) text `"éa"`, cursor 2, Backspace →
`remove(1)` → "start byte index 1 is not a char boundary; it is inside 'é'";
(2) text `"é"`, cursor 1 (char count), type any ASCII → `insert(1, 'x')` panics.
Tests below use both.

---

## B1 — prompt multibyte panic

### Anchors
- main.rs:57-62 — `pub struct Prompt { pub kind: PromptKind, pub text: String, pub cursor: usize }` — cursor is a CHAR count (see Right/End arms).
- main.rs:1408-1412 — `KeyCode::Backspace if p.cursor > 0 => { p.text.remove(p.cursor - 1); p.cursor -= 1; self.prompt = Some(p); }` — `String::remove` takes BYTE index.
- main.rs:1429-1433 — `KeyCode::Char(c) if !c.is_control() => { p.text.insert(p.cursor, c); p.cursor += 1; self.prompt = Some(p); }` — `String::insert` takes BYTE index.
- main.rs:1417 / 1426 — Right/End arms use `p.text.chars().count()` → cursor is definitively char-indexed.
- main.rs:738, 752, 811 — start_search / start_search_backward / start_replace seed `cursor: self.search.query.len()` — that is a BYTE length; reopening a prompt whose query contains a multibyte char starts the cursor past EOL (mis-edit, can panic via insert). Same family, fix here.
- main.rs:981, 1067 — start_write/start_backup already use `chars().count()` (correct, leave).
- ui.rs:98, 179 — prompt draw + cursor position use `chars().count()` → stays correct as long as cursor remains char-indexed.

### Design
Keep `text: String` and `cursor: usize` (char index); convert via `Vec<char>` inside two helpers (PLAN.md B1 sketch). Byte-offset cursor rejected: it would force char→byte math in Right/End/ui.rs and every new arm.

```rust
fn prompt_insert(p: &mut Prompt, c: char) {
    let idx = p.cursor.min(p.text.chars().count());
    let mut v: Vec<char> = p.text.chars().collect();
    v.insert(idx, c);
    p.cursor = idx + 1;
    p.text = v.into_iter().collect();
}
fn prompt_backspace(p: &mut Prompt) {
    if p.cursor == 0 { return; }
    let mut v: Vec<char> = p.text.chars().collect();
    v.remove(p.cursor - 1);
    p.cursor -= 1;
    p.text = v.into_iter().collect();
}
```
Callers: Backspace arm (1408) → `prompt_backspace(&mut p)` (keep the `if p.cursor > 0` guard); Char arm (1429) → `prompt_insert(&mut p, c)`. Both arms still `self.prompt = Some(p)` (re-store unchanged).
Seed-cursor fix: 738/752/811 become `cursor: self.search.query.chars().count()`. Cleanest: add `impl Prompt { fn new(kind: PromptKind, text: String) -> Self }` with `cursor: text.chars().count()` and convert ALL 13 Prompt literals (735, 749, 808, 827, 904, 922, 978, 992, 1024, 1064, 1090, 1109, 1389) to it — kills the byte-len bug class and pre-works E4's extra fields.

### Tests (new `#[cfg(test)] mod ed_tests` in main.rs, harness per PLAN.md:383-390)
- `prompt_multibyte_backspace_no_panic` — RED. test_ed(""), press ^F, Char('e'), Char('é'), Char('a'), Backspace → no panic, text == "ea", cursor == 2. Today: remove(1) on "eéa"… panics (verified repro 1).
- `prompt_multibyte_insert_no_panic` — RED. ^F, Char('é'), Char('x') → text == "éx", cursor == 2. Today: insert(1,'x') panics (verified repro 2).
- `prompt_mid_string_multibyte_edit` — RED. ^F, "éa", Left, Char('x') → "éxa" wait, "xaé"? cursor 1 insert → "xéa" wait: "éa" insert at 1 → "é" + x + "a" = "éxa"? No: chars [é,a], insert idx1 → [é,x,a] → "éxa". Assert text == "éxa", cursor == 2.
- `prompt_reopen_multibyte_query_cursor_ok` — RED (compiles after SearchState.query seeded via do_search; assert `p.cursor == 1` for query "é", i.e. `cursor == ed.prompt.as_ref().unwrap().text.chars().count()`).
- `prompt_ascii_edit_still_works` — GREEN regression: "abc", Backspace, Left, Char('x') → "abxc"? (guard against regressions in the arms).

### Touches
main.rs (helpers near normalize/plural; Backspace+Char arms; 13 Prompt literals → Prompt::new), new `mod ed_tests`. Nothing else; ui.rs untouched.

### Risks
- handle_prompt_key takes the prompt by value after `self.prompt.take()` (1229-1231); any new code path that forgets `self.prompt = Some(p)` silently drops the prompt — tests must assert `ed.prompt.is_some()` after edit keys.
- F8: `prompt` stays an Editor-global (PLAN.md:336 lists it under global fields) — no Pane changes needed; tests keep using `ed.prompt` directly.
- G1: Prompt/PromptKind/prompt_insert/prompt_backspace/handle_prompt_key move verbatim to src/prompt.rs (PLAN.md:358-359). Keep helpers as free fns (not methods) to make the move trivial.

---

## C3 — show_loc sticky → 2 s expiry

### Anchors
- main.rs:93 — `pub show_loc: bool,` ; main.rs:132 — `show_loc: false,` (Editor::new).
- main.rs:1275 — `KeyCode::Char('c') => self.show_loc = true,` (^C, never cleared).
- main.rs:916 — `self.show_loc = true;` (do_goto — also sticky; must be converted too).
- main.rs:170-176 — status_text: `if self.show_loc { return Some(format!("Line {}, Col {}", ...)); }`.
- main.rs:180-186 — tick_status clears only `self.status`, never show_loc.
- main.rs:22-26 — Flash pattern (`until: Instant`) is the model to copy.
- ui.rs:112 — only consumer of status_text; no ui change.

### Design
- Editor field: replace `pub show_loc: bool` with `pub loc_until: Option<Instant>` (init `None` at 132).
- ^C arm (1275): `self.loc_until = Some(Instant::now() + Duration::from_secs(2));`
- do_goto (916): same expression.
- status_text (170): `if let Some(t) = self.loc_until && t > Instant::now() { return Some(format!("Line {}, Col {}", ...)); }` (let-chains, edition 2024 style used at 162-163).
- tick_status (180-186): add `if self.loc_until.is_some_and(|t| t <= Instant::now()) { self.loc_until = None; }` after the status clear. Keep return type `()` — D6 will make it return bool (dirty-draw); note there so D6 doesn't miss the new clear.

### Tests
- `show_loc_expires` — RED (field rename = compile-red first, then assert). ^C → `status_text() == Some("Line 1, Col 1".into())`; then `ed.loc_until = Some(Instant::now() - Duration::from_secs(1))`; `ed.tick_status()`; → `status_text().is_none()`.
- `goto_still_shows_loc` — GREEN protective. start_goto, Char('1'), Enter → status_text contains "Line 1" (do_goto path must not regress).

### Touches
main.rs only (field, 2 setters, status_text, tick_status, Editor::new init).

### Risks
- F8: `loc_until` is already listed in PLAN.md's Pane struct (PLAN.md:332) — per-buffer cursor position is correct semantics. Land on Editor now; F8 moves it mechanically. Test helpers reference `ed.loc_until` today, `ed.pane().loc_until` after F8 (PLAN.md:387 convention).
- `Instant::now()` in tests: inject by overwriting `loc_until` directly (no clock abstraction needed).

---

## E4 — prompt upgrades

Shared prerequisite: `Prompt::new` constructor from B1; Prompt gains two fields (needed only for history):
```rust
pub hist_idx: Option<usize>,   // None = editing own text
pub stashed: Option<String>,   // text being edited when history browsing started
```
`Prompt::new` sets both `None`; the 3 Confirm*/ReplaceAsk literals keep cursor 0 via `Prompt::new(kind, String::new())`.

### E4a — expand_tilde

#### Anchors
- main.rs:985-999 do_write — `let path = PathBuf::from(&name);` (989) then `path.exists()` (990).
- main.rs:1031-1058 do_read — `fs::read_to_string(&name)` (1035).
- main.rs:1076-1084 do_backup — `fs::copy(src, &name)` (1080).

#### Design
```rust
fn expand_tilde(p: &str) -> String {
    if p == "~" { return std::env::var("HOME").unwrap_or_else(|_| p.to_string()); }
    if let Some(rest) = p.strip_prefix("~/") {
        if let Ok(home) = std::env::var("HOME") { return format!("{}/{}", home, rest); }
    }
    p.to_string()
}
```
`~user` intentionally NOT expanded. Call sites: 989 → `PathBuf::from(expand_tilde(&name))` (exists()-check and pending_write both get the expanded path); 1035 → `fs::read_to_string(expand_tilde(&name))`; 1080 → `fs::copy(src, expand_tilde(&name))`.

#### Tests
- `expand_tilde_home` — RED (new fn). "~" == HOME; "~/sub/x" == HOME + "/sub/x".
- `expand_tilde_leaves_paths` — RED. "/abs/path", "rel/name", "", "x~y", "~user/x" all unchanged.
- `do_write_tilde_path` — GREEN integration. In temp HOME fixture: ^O, type "~/rano_tilde_test.txt", Enter → file exists at HOME. (Needs HOME override per-test via `std::env::set_var` in a serial block, or skip integration and keep pure-fn tests + one do_read test.) Prefer pure-fn tests only + manual note, to avoid env races in parallel tests.

#### Touches
main.rs (fn + 3 call sites).

#### Risks
- `std::env::set_var` in tests is process-global and racy with parallel tests — keep env-dependent assertions to one test fn, or read HOME once and compare against `std::env::var("HOME")` without setting it.
- G1: expand_tilde → src/prompt.rs.

### E4b — prompt history (per kind)

#### Anchors
- PromptKind: main.rs:42-55 (11 variants; Copy).
- Enter arm: main.rs:1379-1407 (dispatch per kind).
- Esc/^G cancel: main.rs:1326-1327 — prompt dropped, nothing recorded (already correct).
- Fallback `_` arm: main.rs:1434-1438 re-stores when `!cancel` — Up/Down currently land here as no-ops.

#### Design
- Editor field: `histories: Histories` where `pub struct Histories { pub search: Vec<String>, pub exec: Vec<String>, pub file: Vec<String> }` (init empty in Editor::new). Global on Editor, NOT per-pane — history is session state, survives buffer switches (F8-safe).
- Kind mapping: Search → search; Exec → exec; WriteName | ReadName | BackupName → file; others → None (ReplaceFind/ReplaceWith excluded — two-stage flow, revisit later; GoTo digits-only).
- `fn history_of(&self, kind: PromptKind) -> Option<&Vec<String>>` (+ `&mut` twin or index copy).
- In handle_prompt_key, before the main match (so it applies to text prompts only, after the Confirm*/ReplaceAsk early returns at 1330-1375):
```rust
KeyCode::Up => { if let Some(h) = self.history_of(p.kind) && !h.is_empty() {
    if p.stashed.is_none() { p.stashed = Some(p.text.clone()); }
    let i = match p.hist_idx { None => h.len() - 1, Some(i) => i.saturating_sub(1) };
    p.hist_idx = Some(i); p.text = h[i].clone(); p.cursor = p.text.chars().count();
} self.prompt = Some(p); }   // re-store regardless (no-op otherwise)
KeyCode::Down => { if let Some(i) = p.hist_idx {
    let h = self.history_of(p.kind).unwrap();
    if i + 1 < h.len() { p.hist_idx = Some(i + 1); p.text = h[i + 1].clone(); }
    else { p.hist_idx = None; p.text = p.stashed.take().unwrap_or_default(); }
    p.cursor = p.text.chars().count();
} self.prompt = Some(p); }
```
- Enter recording (top of Enter arm, before dispatch): `self.record_history(p.kind, &p.text)` where
```rust
fn record_history(&mut self, kind: PromptKind, text: &str) {
    if text.trim().is_empty() { return; }
    if let Some(h) = self.histories_mut(kind) { h.retain(|s| s != text); h.push(text.to_string()); }
}
```
Full dedupe (remove all prior equals, push). DEVIATION from PLAN.md E4 ("dedupe adjacent") — task wording says "appends+dedupes"; full dedupe is what shells do and keeps Up-cycling useful; flagged in the return summary.
- Both new arms re-store (always). Cancel arm unchanged.

#### Tests
- `prompt_history_up_prefills` — RED. ^T (Exec), "echo hi", Enter, ^T again, Up → text == "echo hi", cursor == 7, prompt still Some. Today: Up falls to `_` arm, text stays "" → red.
- `prompt_history_up_older_then_down_restores` — GREEN after. Two exec entries ("a", "b"): Up → "b", Up → "a", Down → "b", Down → original stashed text.
- `prompt_history_enter_dedupes` — GREEN after. Run "echo hi" twice → `ed.histories.exec == ["echo hi"]`.
- `prompt_history_skips_empty` — GREEN after. Open ^O, Enter (empty) → file hist empty.
- `prompt_history_per_kind` — GREEN after. Search hist unaffected by exec entries (search hist exercised via ^F with a real query on a buffer containing it).
- `prompt_history_cancel_not_recorded` — GREEN after. ^T, "x", Esc → exec hist empty.

#### Touches
main.rs (Histories struct + Editor field + init; 2 key arms; record_history/history_of fns; Prompt fields via B1's Prompt::new).

#### Risks
- Up/Down on file prompts pre-filled with a path: first Up stashes the prefilled name — expected nano-ish behavior; document in help text later (C2/G-phase).
- F8: nothing per-buffer here; histories stay on Editor.
- G1: history fns + Up/Down arms → src/prompt.rs; Histories struct can live there too.

### E4c — Tab path completion

#### Anchors
- Tab today: main.rs:1379 Enter arm has no Tab case; Tab (control char) fails the `!c.is_control()` guard (1429) and lands in the `_` fallback (1434) → re-store, no-op.
- Target prompts: WriteName (978), ReadName (1024), BackupName (1064) — the kinds whose text is a path. FilterCmd (F6) will be added to the same `matches!` when it exists.
- fs entry points to mirror: fs::read_dir, fs::metadata (std, no new deps).

#### Design
```rust
fn longest_common_prefix(names: &[String]) -> String // pure, char-wise
fn complete_path(prefix: &str) -> Option<(String, Vec<String>)> {
    let expanded = expand_tilde(prefix);
    let (dir, file) = match expanded.rfind('/') {
        Some(i) => (&expanded[..=i], &expanded[i + 1..]),
        None => ("", expanded.as_str()),
    };
    let rd = fs::read_dir(if dir.is_empty() { "." } else { dir }).ok()?;
    let mut names: Vec<String> = rd.flatten()
        .filter(|e| { let n = e.file_name().to_string_lossy().into_owned();
            n.starts_with(file) && (file.starts_with('.') || !n.starts_with('.')) })
        .collect();
    names.sort();
    if names.is_empty() { return None; }
    let mut completed = format!("{}{}", dir, longest_common_prefix(&names));
    if names.len() == 1 && fs::metadata(Path::new(dir).join(&names[0]))
        .is_ok_and(|m| m.is_dir()) { completed.push('/'); }
    Some((completed, names))
}
```
Tab arm (placed before the `_` fallback, after Enter):
```rust
KeyCode::Tab if matches!(p.kind, PromptKind::WriteName | PromptKind::ReadName | PromptKind::BackupName) => {
    match complete_path(&p.text) {
        Some((completed, opts)) => {
            p.text = completed; p.cursor = p.text.chars().count();
            if opts.len() > 1 {
                let shown: Vec<&str> = opts.iter().take(5).map(|s| s.as_str()).collect();
                let mut m = shown.join(", ");
                if opts.len() > 5 { m.push_str(&format!(" (+{})", opts.len() - 5)); }
                self.flash(&m);
            }
        }
        None => self.flash("No match"),
    }
    self.prompt = Some(p);   // re-stores in ALL outcomes; prompt never closes on Tab
}
```
Notes: text becomes the EXPANDED path after Tab with `~` (acceptable, nano-like; documented). Unique non-dir file → no trailing '/'; unique dir → trailing '/' (enables chained Tab). Ambiguous → common prefix + flash up to 5 names (+N). No candidates → flash, text unchanged.

#### Test fixture (fs)
```rust
struct Fixture(PathBuf);
impl Drop for Fixture { fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); } }
fn fs_fixture() -> Fixture {
    let d = std::env::temp_dir().join(format!("rano_cmpl_{}_{:?}", std::process::id(), std::time::Instant::now()));
    fs::create_dir_all(d.join("alphabet")).unwrap();
    fs::write(d.join("alpha.txt"), "").unwrap();
    fs::write(d.join("other.txt"), "").unwrap();
    Fixture(d)
}
```
Unique per-run name (pid + Instant) so parallel/aborted runs never collide; Drop cleans up.

#### Tests
- `complete_path_unique_file` — RED (new fn). prefix "{d}/alpha.txt" → Some((that path, 1 opt)), no trailing '/'.
- `complete_path_unique_dir_slash` — RED. prefix "{d}/alphabet" → completed ends with "alphabet/".
- `complete_path_ambiguous_common_prefix` — RED. prefix "{d}/alp" → completed "{d}/alpha" (common prefix of alpha.txt/alphabet), opts.len() == 2, sorted.
- `complete_path_no_match` — RED. prefix "{d}/zz" → None.
- `complete_path_hidden_skipped` — GREEN after. Add ".hidden" to fixture; prefix "{d}/" does not return it; "{d}/.h" does.
- `tab_completes_in_prompt` — RED. ^O, type fixture path prefix "{d}/alp" (via Char presses), Tab → `ed.prompt` text == "{d}/alpha", still Some. Today Tab is a no-op → red.
- `longest_common_prefix_basics` — RED (pure): ["alpha.txt","alphabet"] → "alpha"; ["a"] → "a"; [] → "".

#### Touches
main.rs (2 fns, 1 key arm), tests + fixture in ed_tests.

#### Risks
- complete_path does real fs I/O — never call it with cwd-relative prefixes in tests; always prefix the fixture path. Manual use reads "." which is fine.
- F6 coordination: when PromptKind::FilterCmd lands, its Enter arm is F6's job, but its Tab arm belongs in this `matches!` list — add kind + one arm edit.
- F8: prompt/completion stay Editor-global; no Pane impact. G1: complete_path/longest_common_prefix → src/prompt.rs.

### E4d — word motion inside prompts (M-b / M-f / Ctrl+Left / Ctrl+Right)

#### Anchors
- main.rs:1413-1416 — `KeyCode::Left if p.cursor > 0 => …` — NO modifier guard, so Ctrl+Left currently moves 1 char.
- main.rs:1429-1433 — `KeyCode::Char(c) if !c.is_control()` — NO modifier guard, so M-b/M-f currently INSERT 'b'/'f' into the prompt text. Any new alt/ctrl arm MUST be ordered above this arm.
- Reference semantics: main.rs:575-602 prev_word / 613-643 next_word (buffer version; is_word = alphanumeric | '_').

#### Design
```rust
fn prompt_next_word(p: &mut Prompt) {      // skip ws, skip word, land after last word char
    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    let v: Vec<char> = p.text.chars().collect();
    let mut c = p.cursor.min(v.len());
    while c < v.len() && !is_word(v[c]) { c += 1; }
    while c < v.len() && is_word(v[c]) { c += 1; }
    p.cursor = c;
}
fn prompt_prev_word(p: &mut Prompt) {      // skip ws back, skip word back, land on word start
    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    let v: Vec<char> = p.text.chars().collect();
    let mut c = p.cursor.min(v.len());
    while c > 0 && !is_word(v[c - 1]) { c -= 1; }
    while c > 0 && is_word(v[c - 1]) { c -= 1; }
    p.cursor = c;
}
```
New arms in handle_prompt_key, ordered BEFORE the plain Left/Right/Char arms (match-arm order matters — see anchors):
```rust
KeyCode::Char('b') if alt => { prompt_prev_word(&mut p); self.prompt = Some(p); }
KeyCode::Char('f') if alt => { prompt_next_word(&mut p); self.prompt = Some(p); }
KeyCode::Left if ctrl      => { prompt_prev_word(&mut p); self.prompt = Some(p); }
KeyCode::Right if ctrl     => { prompt_next_word(&mut p); self.prompt = Some(p); }
```
(alt/ctrl bound at 1324-1325 from key.modifiers; cancel check at 1326-1327 only matches ^G — unaffected.) All four arms re-store.
SPEC CORRECTION: task/PLAN.md E4d say `M-f from 0 → 4 … M-f → 12` for "foo bar_baz", but that string is 11 chars — with "land after word (+trailing ws skip)" semantics the values are 4 then 11 (12 would need a trailing space in the fixture). Tests assert 4/11; flagged as a PLAN.md number typo.

#### Tests
- `prompt_word_forward` — RED. ^T, "foo bar_baz", Home, M-f → cursor 4; M-f → 11. Today M-f inserts 'f' → red.
- `prompt_word_back` — GREEN after. cursor End, M-b → 4; M-b → 0.
- `prompt_ctrl_arrow_word` — RED. Home, Ctrl+Right → cursor 4 (today: 1). Ctrl+Left from End → 4 (today: 10).
- `prompt_word_motion_multibyte` — GREEN after B1. "héllo wörld", Home, M-f → 6, M-f → 11; M-b → 6; M-b → 0 (char-indexed, no panics).

#### Touches
main.rs (2 fns, 4 arms above the Char/Left/Right arms).

#### Risks
- Arm ORDER is the whole bug surface: below 1429 these keys silently type letters. Add a comment at the Char arm: "keep modifier-specific Char arms above this".
- M-b/M-f in the MAIN editor are prev/next MATCH (1243-1244); inside prompts they are word motion — intentional divergence (nano does the same), note in help text during C2.
- F8/G1: free fns + arms move to src/prompt.rs verbatim; no state involved.

---

## F5 (prompt side) — M-C case toggle / M-R regex toggle in Search prompt

### Anchors
- SearchState: main.rs:28-34 — `pub struct SearchState { pub query, pub matches, pub current, pub backwards }` — no case/regex fields yet.
- SearchState init: main.rs:123-128 (Editor::new).
- Search prompt: main.rs:729-740 (start_search) — the only kind these toggles apply to.
- do_search: main.rs:756-781 — currently `self.buf.find_all(&query)` (761) — LITERAL, case-sensitive; core upgrade owned by S5 (Matcher in src/search.rs, PLAN.md:296-307). This plan owns ONLY the toggles + fields.
- M-c/M-r today: fall through to Char arm (1429) → typing 'c'/'r' into the search text. New arms must sit above it (same ordering trap as E4d).
- buffer.rs:197 find_all / 216 find_next — stay literal (used by replace); S5 decides their fate, not this plan.

### Design
- SearchState gains `pub regex: bool` (init false) and `pub case_sensitive: bool` (init true — nano default). Added HERE so the prompt arms compile independently of S5's landing; S5's Matcher::find_all consumes them later (coordination note below).
- Arms in handle_prompt_key (above the Char arm; kind-gated so they do nothing elsewhere):
```rust
KeyCode::Char('c') if alt && p.kind == PromptKind::Search => {
    self.search.case_sensitive = !self.search.case_sensitive;
    self.flash(if self.search.case_sensitive { "Case: sensitive" } else { "Case: insensitive" });
    self.prompt = Some(p);                                  // stay in prompt
}
KeyCode::Char('r') if alt && p.kind == PromptKind::Search => {
    self.search.regex = !self.search.regex;
    self.flash(if self.search.regex { "Regex: on" } else { "Regex: off" });
    self.prompt = Some(p);
}
```
- Both arms re-store unconditionally (stay-in-prompt requirement). Esc/^G cancel (1326-1327) still wins if pressed after a toggle. Toggle state lives on SearchState, so it persists to the NEXT search and is visible to do_search via `self.search` — no Prompt field needed.
- UI: no label change (prompt_label stays static, ui.rs:269-283); the 3 s flash communicates state. Optional follow-up (not this plan): show `[case]`/`[re]` flags in the search label.
- Coordination with S5 (search core, planned separately): S5 owns do_search/find_all/Matcher and must (1) read `self.search.case_sensitive`/`.regex` when building the Matcher, (2) make current_match_range (main.rs:1212-1220) use stored match len instead of `query.chars().count()` (1218) — regex matches have different length. Until S5 lands, the toggles flash but matching stays literal/case-sensitive — accepted sequencing, no prompt-side change needed when S5 lands. Replace stays literal-only (PLAN.md F5); if user hits ^\ with regex on, S5 flashes a notice — prompt side does nothing.

### Tests
- `prompt_search_case_toggle` — RED (fields don't exist → compile-red, then assert). ^F, M-c → `ed.search.case_sensitive == false`, `ed.prompt.is_some()`, text unchanged (""), no 'c' inserted. Today: 'c' inserted → red.
- `prompt_search_regex_toggle` — RED. ^F, M-r → `ed.search.regex == true`, prompt still Some, text unchanged.
- `prompt_search_toggle_stays_in_prompt` — GREEN after. ^F, "ab", M-c, Char('x') → text == "abx" (prompt still editing), `case_sensitive == false`.
- `prompt_search_toggle_kind_gated` — GREEN after. ^T (Exec prompt), M-c → text contains 'c' (inserted; toggle did NOT fire), `ed.search.case_sensitive` unchanged.

### Touches
main.rs (2 SearchState fields + init at 123-128, 2 key arms). NO ui.rs, NO buffer.rs, NO do_search changes.

### Risks
- Ownership split is the main risk: exactly one field-add (here) and one consumer (S5). If S5 lands first with its own fields, drop the duplicate add and keep the arms.
- Same arm-order trap as E4d — M-c/M-r currently type letters; arms must precede main.rs:1429.
- F8: SearchState is per-pane in PLAN.md's Pane struct (PLAN.md:334 `pub search: SearchState`) — toggle state becomes per-buffer after F8, which is fine (tests use `ed.search` today, `ed.pane().search` after).
- ReplaceAsk prompt-drop quirk (main.rs:1363-1374: the bare `return;` at 1373 drops the prompt on any non-char key, e.g. arrows, silently ending the loop) — out of scope here, but adjacent to every arm edit in this file; do NOT "fix in passing", file under S5/F-followup if wanted.

---

## Execution order & conflicts

Order: B1 → C3 → E4a → E4b → E4c → E4d → F5 (B1 first: E4d/F5 tests type text into prompts; C3 independent). `cargo test` after each; all RED-first (compile-red for new fns/fields is acceptable red, per PLAN.md Phase A discipline).

handle_prompt_key (main.rs:1324-1440) — touched by: THIS PLAN (B1 arms, E4b Up/Down, E4c Tab, E4d 4 arms, F5 2 arms — all inserted between line 1379 and 1434; arm ORDER vs the modifier-blind Char/Left/Right arms is the critical invariant). Others: F6 adds FilterCmd Enter arm + Tab-kind (later). No one else.

handle_key (main.rs:1224-1322) — touched by: C3 (^C arm 1275 only). Conflicts to watch: D6 (tick_status return bool — C3 leaves a note), F5-core/S5 (consumes SearchState fields, does NOT touch handle_key), F6 (adds M-| binding), F8 (wraps dispatch with pane()).

Editor struct — this plan adds: `histories: Histories`; renames `show_loc` → `loc_until`; Prompt gains hist_idx/stashed; SearchState gains regex/case_sensitive. F8 must carry all of these (histories/prompt global; loc_until/search per-Pane — already reflected in PLAN.md:332-336 except `histories`, which stays global).

PLAN.md deviations: (1) history dedupe is FULL, not "adjacent" (PLAN.md:241); (2) E4d fixture numbers corrected 4/12 → 4/11 (11-char string); (3) SearchState regex/case fields land here, not with F5-core, to decouple prompt arms from S5; (4) B1 repro corrected: bare "é"+Backspace does not panic — real repros are "éa"+Backspace and "é"+ASCII insert (verified by rustc).
