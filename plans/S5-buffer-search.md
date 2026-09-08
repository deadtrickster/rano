# S5 — Buffer tests, CRLF preservation, search matcher core

Scope: Phase A buffer.rs tests (green) + C1 (CRLF) + F5 (Matcher/search.rs).
NOT in scope: Phase A lsp.rs/ui.rs tests (other slice), D1 replace_all (S1),
D3 undo, F8 panes, G1 file split — see Conflicts at the bottom.
Ground truth: `replace_at` (buffer.rs:224-236) ALREADY verifies the match and
returns bool; the fix has landed. Tests are written green (characterization),
not red-first. This deviates from PLAN.md STATE line 13-15 (which says revert →
red → re-apply); that window is gone, do not revert.

## Item A — Phase A buffer tests (green, characterization)

### Anchors
- `copy_range` buffer.rs:151-172 (mirror of `cut_range` tests buffer.rs:309-359)
- `insert_char` buffer.rs:75-79 (col clamped to line len; silent no-op on bad row)
- `replace_at` buffer.rs:224-236 (returns bool, match-checked, `modified` set only on success)
- `find_all` buffer.rs:197-213 (overlap by design: `i += 1`, not `i += n.len()`)
- test helper `buf(text)` buffer.rs:243-258

### Design
No production code changes. Append to `mod tests` in buffer.rs:
- `copy_range_single_row`: "hello world", (0,0)-(0,5) → ["hello"], buffer unchanged.
- `copy_range_multi_row`: "aaa\nbbb\nccc\nddd", (0,1)-(3,2) → ["aa","bbb","ccc","dd"],
  buffer unchanged (mirrors cut_range_multi_row_keeps_endpoints).
- `copy_range_whole_lines`: "111\n222\n333\n444", (0,0)-(2,0) → ["111","222"], unchanged.
- `copy_range_empty_region`: same-row a==b → out == [vec![]]; cross-row empty
  ((0,3)-(1,0) on "abc\nd") → also [vec![]] (the `out.is_empty()` fallback, buffer.rs:168-170).
- `insert_char_mid` ("helo" + (0,2) 'l' → "hello"), `insert_char_eol_clamps`
  ((0,99) '!' on "ab" → "ab!"), `insert_char_empty_buffer`, `insert_char_bad_row_no_op`.
- `replace_at_longer` ("foo"→"foobar" → "foobar bar"), `replace_at_shorter`
  ("foo"→"f" → "f bar"), `replace_at_multibyte` ("héllo", "é"→"e" at (0,1) → "hello"),
  `replace_at_non_match_returns_false` (wrong text at pos → false + buffer unchanged +
  `modified` stays false), `replace_at_past_eol_false`, `replace_at_bad_row_false`.
- `find_all_overlap`: "aa" in "aaa" → [(0,0),(0,1)]; "aa" in "aaaa" → cols 0,1,2.
  Documents overlapping-match semantics literal search relies on.

### Tests
All green by construction; run `cargo test` — baseline 27 + ~14 new.

### Touches
src/buffer.rs (tests mod only).

### Risks
If any characterization fails, the surprise is a real bug — stop and report, do
not "fix" silently (S1's D1 depends on exact replace_at/find_all behavior).

## Item C1 — CRLF preservation

### Anchors
- `Buffer` struct buffer.rs:10-15; `Buffer::new` buffer.rs:18-24; `from_file`
  buffer.rs:26-35 — `text.lines()` (line 28) strips `\r` BEFORE we could see it.
- `text()` buffer.rs:51-64 (joins "\n", trailing "\n" iff last row non-empty).
- `save_to` main.rs:1002-1021 (`let text = self.buf.text();` line 1003, `fs::write` 1005).
- Internal `\n` consumers that must NOT change: `edit_invalidate` lsp change
  (main.rs:208), `Highlighter::refresh` (syntax.rs:102 `buf.text()`), justify/sort
  (operate on `lines` directly).

### Design
- `Buffer` gains `pub crlf: bool` (default false).
- `from_file`: `let crlf = text.contains("\r\n");` BEFORE `text.lines()`; store it.
- `Buffer::new`: `crlf: false`.
- New method:
```rust
pub fn file_text(&self) -> String // text() but joins with "\r\n" (and trailing "\r\n") when self.crlf
```
  Mirror text()'s trailing-newline rule exactly (append "\r\n" iff last row non-empty).
- `save_to` main.rs:1003 → `let text = self.buf.file_text();` (bonus: "Wrote N
  bytes" becomes accurate for CRLF files). Atomic-save (TODO §2, another item)
  changes only the `fs::write` call on line 1005 — one-line merge overlap.
- Internal text()/LSP/syntax stay `\n` untouched.
- **Struct-literal enumeration (corrects PLAN.md C1)**: the ONLY `Buffer { .. }`
  literal outside the two constructors is the test helper `buf()` buffer.rs:253 →
  add `crlf: false`. `syntax.rs buf_named` (236) and `scratch_buffer_has_no_highlighting`
  (295) use `Buffer::new()` → no change. main.rs has ZERO Buffer literals
  (`Editor::new(buf: Buffer)` takes one). Future editor test harness (Phase B+,
  PLAN.md:385) must build buffers via `Buffer::new()` + assign `lines`, NOT a
  struct literal, to stay field-churn-proof.

### Tests
RED first (both compile-fail red, then implement):
- `crlf_roundtrip` (buffer.rs): write temp file "a\r\nb\r\n" (`std::env::temp_dir()`
  + unique name), `from_file` → `b.crlf` true, `b.text()` == "a\nb\n",
  `b.file_text()` == "a\r\nb\r\n". LF file → crlf false, file_text() == text().
- `crlf_save_roundtrip` (editor-level, main.rs tests once harness exists — or
  buffer-level via `save_to` being private-in-main: keep it in main.rs `mod tests`):
  crlf buffer + `save_to(tmp)` → raw file bytes contain `\r\n`; LF buffer → no `\r`.

### Touches
src/buffer.rs (field, new, from_file, file_text, test helper, tests),
src/main.rs:1003 (one line), main.rs `mod tests` (save round-trip).

### Risks
- Mixed EOL file ("a\nb\r\n") → crlf=true, LF rows get \r\n on save (nano behaves
  the same; document in test as characterization).
- do_read inserts foreign file text via `lines()` → normalizes to \n in-buffer;
  acceptable, out of scope (note only).

## Item F5 — Matcher core (regex + case toggle)

### Anchors
- `SearchState` main.rs:29-34; literal at main.rs:123-128.
- `do_search` main.rs:756-781, `next_match`/`prev_match` 783-797, `jump_to_match`
  799-803, `start_search`/`start_search_backward` 729-754.
- `start_replace` main.rs:807-813, `next_replace_ask` 815-842, `answer_replace_ask`
  844-899 (all stay literal — they use `Buffer::find_next`/`replace_at`).
- `current_match_range` main.rs:1212-1220 (len from `query.chars().count()` — wrong
  for variable-length matches); `char_style` 1188-1210 (consumes the range, no change).
- `matches.clear()` also at main.rs:204 (edit_invalidate), 326, 339 (undo/redo) —
  `.clear()` is type-agnostic, no edits needed.
- `handle_prompt_key` 1229-1440: Search-Enter arm at 1382; catch-all
  `Char(c) if !c.is_control()` insert arm at 1429 would swallow ALT chords.
- Cargo.toml (deps end line 14).

### Design
Cargo.toml += `regex = "1"`. New src/search.rs:
```rust
use crate::buffer::Pos;
pub enum Matcher {
    Literal { needle: Vec<char>, case_sensitive: bool },
    Re(regex::Regex),
}
impl Matcher {
    pub fn literal(query: &str, case_sensitive: bool) -> Matcher
    pub fn regex(pattern: &str, case_sensitive: bool) -> Result<Matcher, regex::Error> // "(?i)" prefix when !case_sensitive
    pub fn find_all(&self, lines: &[Vec<char>]) -> Vec<(Pos, usize)> // pos + match len in CHARS
}
```
- **Case-insensitivity over Vec<char>**: per-char fold —
  `fn fold(c: char) -> char { c.to_lowercase().next().unwrap_or(c) }`. Literal
  matching slides over `lines` comparing `fold(line[i+k]) == fold(needle[k])`.
  Rationale: keeps char-index positions natively (no byte/char remap for the
  literal path) and O(n·m) like today's find_all. Documented limitation: chars
  whose lowercase expands to >1 char (e.g. 'İ' → "i̇") fold to the first char —
  ASCII + Latin-1 (é/É) is correct; full-Unicode casing is out of scope.
- **Regex path**: `Regex` matches `&str` BYTES but positions are char indices.
  Per line: `let s: String = line.iter().collect();` then for each `m` in
  `re.find_iter(&s)`: `col = s[..m.start()].chars().count()`,
  `len = s[m.start()..m.end()].chars().count()` (same byte→char technique as
  syntax.rs `build_styles` char_offsets, syntax.rs:164-173). Per-line only:
  `.` and `\n` never cross rows; `^`/`$` anchor per line (nano-like). Allocation
  per line per search is fine (search is on-demand, not per keystroke).
- `SearchState` becomes:
```rust
pub struct SearchState {
    pub query: String,
    pub matches: Vec<(Pos, usize)>, // WAS Vec<Pos>
    pub current: usize,
    pub backwards: bool,
    pub regex: bool,          // default false
    pub case_sensitive: bool, // default true (nano default)
}
```
- `do_search`: keep empty-query early return; build `Matcher` from query + flags;
  `Matcher::regex` Err → flash "Bad regex: {e}", keep old query/matches, return.
  `self.search.matches = matcher.find_all(&self.buf.lines);` current-pick logic
  (766-778) destructures: `|&(m, _)|` in rposition/position.
- `jump_to_match` (800): `let (m, _) = ...; self.cursor = m;`
- `current_match_range` (1212-1220): `let (m, len) = self.search.matches[idx];`
  end col = `m.col + len` (stored len — regex matches get correct width).
- `start_replace` (807): if `self.search.regex` → flash "Replace uses literal text"
  and proceed literally (replace is literal-only, case-sensitive, ignores the
  case toggle — decision: nano parity deferred, keep simple).
- Prompt toggles in `handle_prompt_key`, BEFORE the Char insert arm: for
  `PromptKind::Search` + `key.modifiers.contains(KeyModifiers::ALT)`:
  Char('c') → flip `case_sensitive`, Char('r') → flip `regex`; flash
  "[Case sensitive]" / "[Case insensitive]" / "[Regex search]" / "[Literal search]";
  re-run `do_search(existing query)` if query non-empty; restore prompt (it was
  `take()`n at 1229) and return.

### Consumer inventory (every `search.matches` / `search.query` site)
main.rs:204 clear (no change) · 326, 339 clear (no change) · 731/745 is_empty
(no change) · 737-738, 751-752, 810-811 query prefill (no change) · 760-761
(query + find_all → Matcher) · 762 is_empty (no change) · 766-778 position/
rposition (destructure tuple) · 784-796 len() (no change) · 800 (destructure) ·
1213-1219 (stored len) · Editor::new literal 123-128 (+2 fields). ui.rs/syntax.rs
do not touch SearchState.

### Tests
RED first:
- search.rs `literal_case_insensitive`: lines ["foo Bar","FOO"], needle "fOO",
  case_sensitive=false → [(0,0,3),(1,0,3)]; case_sensitive=true → [].
- search.rs `regex_matches`: "a.c" over ["abc axc a-c"] → 3 hits len 3.
- search.rs `regex_variable_len`: "a+" over ["caaad"] → [(0,1),3].
- search.rs `regex_multibyte_char_cols`: line "éa", regex "a" → col 1 (not byte 2).
- search.rs `regex_bad_pattern`: `Matcher::regex("(", true)` → Err.
- search.rs `literal_case_sensitive_default`: "Foo" vs lines ["foo"] → empty.
Editor-level (main.rs mod tests, harness per PLAN.md:381-390):
- ^W "foo" Enter → matches non-empty, cursor at first hit.
- M-C in Search prompt → flash contains "Case", flag flipped, matches re-computed.
- do_search "[" in regex mode → query UNCHANGED (old kept), flash "Bad regex".
- regex "a+" on "caaad" → current_match_range() == Some(((0,1),(0,4))) — variable
  width highlight (old code would yield len 2 from query.chars().count()).

### Touches
Cargo.toml (+regex), Cargo.lock (regen), NEW src/search.rs (+tests),
src/main.rs: 29-34, 123-128, 756-781, 799-803, 807-813, 1212-1220,
handle_prompt_key (~1363-1430 region: new ALT arms), mod tests additions.

### Risks
- F8 multi-buffer: `Buffer` stays per-pane; `SearchState` moves into `Pane` —
  it already derives Clone, the two new bool fields ride along; Matcher is built
  stateless per do_search, nothing to move. No F8 prep needed beyond keeping
  SearchState self-contained.
- G1 split: PLAN.md:357 has search.rs holding "Matcher, SearchState, search/
  replace methods" — F5 pre-creates the file with Matcher only; G1 then moves
  SearchState + do_search/next_match/prev_match/jump_to_match into it verbatim.
- Regex crate pulls in ~a few deps; check `cargo build` time acceptable.
- `find_iter` byte→char mapping is THE bug magnet — multibyte test is mandatory.

## D1 note (owned by S1, not S5)

S1 (replace_all one-pass, PLAN.md D1) consumes these Buffer primitives as-is:
- `replace_at` bool-return + match-check: already exactly what S1's drift/skip
  logic relies on (PLAN.md:120 `if !self.buf.replace_at(...) { continue; }`). ✔
- `find_all` returns ALL matches; S1 filters `>= cursor` itself for 'a' semantics.
- Primitive-level gaps S1 may want (S1's call, coordinate before editing buffer.rs):
  1. `find_all_from(&self, from: Pos, needle: &str) -> Vec<Pos>` — avoids allocating
     the full list for from-cursor scans and makes non-wrapping semantics explicit
     (find_next wraps; 'a' must not).
  2. find_next is a full find_all rescan per call — fine for interactive y/n, S1's
     one-pass removes the O(N²); no fix needed here.
  3. If replace-all ever honors the case toggle (post-F5), primitives need a
     Matcher-flavored find_all_from — deliberately NOT added now; replace stays
     literal per F5 decision.

## Conflicts / coordination

- **src/buffer.rs**: S5 owns Phase A tests + C1 (field/from_file/file_text).
  S1 (D1) may add `find_all_from` — sequence S5-A first (its tests characterize
  find_all/replace_at that S1 builds on); S1 rebases on top.
- **SearchState / main.rs search block**: S5 (F5) owns 29-34, 123-128, 729-813,
  1212-1220. D3 (undo) owns 326/339 clear sites — type-agnostic, no merge risk.
  D4 (lsp debounce) rewrites edit_invalidate (204 clear stays verbatim inside).
- **save_to main.rs:1002-1021**: S5-C1 touches line 1003 only; atomic-save item
  touches 1005 only. One-line overlap, trivial.
- **F8/G1**: consume F5's shapes as described in F5 Risks; no pre-coordination
  needed beyond keeping SearchState/Matcher where they are.

## Contradictions with PLAN.md

1. PLAN.md STATE:13-15 + Phase A:26-28 demand revert→red→re-apply for replace_at;
   the fix already landed. S5 writes the tests green and does NOT revert.
2. PLAN.md C1:85-86 says fix "all struct literals incl. buffer.rs:249, syntax.rs
   test helper" — verified false: only buffer.rs:253 is a literal; syntax.rs:236
   uses Buffer::new(); main.rs has none.
3. PLAN.md F5:299 `find_all(&self, lines)` — deepened: regex needs per-line
   String + byte→char offset mapping (PLAN.md omits this; without it positions
   are byte-based and highlights land wrong on multibyte lines).
