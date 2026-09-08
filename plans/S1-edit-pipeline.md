# S1 — edit pipeline & undo (B2, D1, D2, D3)

Anchors verified against main.rs as of 2026-09-06 (pre-F8; re-grep at execution).
NOTE for all steps: D7 (exec), E1 (paste_text), F6 (filter) will record
EMPTY-REGION steps (`before == []`) — finish_step must tolerate that.
F8 will move undo fields into `Pane`; G1 moves this file's content to src/undo.rs.

## B2 — move_left BOL bug

### Anchors
main.rs:521-529:
```rust
fn move_left(&mut self) {
    let c = self.cursor;
    if c.col > 0 { self.cursor.col -= 1; }
    else if c.row > 0 {
        self.cursor.row -= 1;
        self.cursor.col = self.buf.line_len(c.row);   // BUG: c.row is the OLD row
    }
}
```
Consequence: cursor lands past EOL of the new row; next Backspace (main.rs:392-394
→ buffer.rs:102 `lines[row].remove(col-1)`) panics.

### Design
`self.cursor.col = self.buf.line_len(self.cursor.row);` + `self.clamp_cursor();`
after the branch (defensive).

### Tests
RED `move_left_bol_clamps_to_prev_row_end` (mod ed_tests in main.rs):
buffer "ab\ncdef", cursor (1,0), `ed.move_left()` → cursor == (0,2).
Follow with Backspace → no panic, buffer "a\ncdef".

### Touches
main.rs move_left only.

## D1 — replace-all one-pass, nano semantics

### Anchors
- answer_replace_ask 'a' arm main.rs:872-889: loops `find_next` (buffer.rs:216-222,
  full `find_all` per iteration → O(N²)).
- buffer.rs:224-238: `replace_at(pos, find, with) -> bool` (verifies match — landed).
- Semantics change: current code starts strictly AFTER cursor (`find_next` uses
  `> from`); nano replaces AT/after cursor. RED test encodes the new semantics.

### Design
```rust
/// One pass, left-to-right, non-overlapping, from first match at-or-after
/// cursor (no wrap). Single-line find/with only. Returns count.
fn replace_all_from(&mut self, find: &str, with: &str) -> usize {
    let n_len = find.chars().count();
    let w_len = with.chars().count();
    let cur = self.cursor;
    // collect once; keep only matches at-or-after cursor, row-major
    let matches: Vec<Pos> = self.buf.find_all(find).into_iter()
        .filter(|m| m.row > cur.row || (m.row == cur.row && m.col >= cur.col))
        .collect();
    let (mut drift, mut next_free, mut anchor) = (0isize, 0usize, None);
    for m in matches {
        let col = if anchor == Some(m.row) { (m.col as isize + drift).max(0) as usize } else { m.col };
        if anchor == Some(m.row) && col < next_free { continue; }        // overlap skip
        if !self.buf.replace_at(Pos { row: m.row, col }, find, with) { continue; }
        anchor = Some(m.row);
        drift += w_len as isize - n_len as isize;
        next_free = col + w_len;
        self.replace_count += 1;
        self.cursor = Pos { row: m.row, col: next_free };
    }
    self.replace_count
}
```
Caller: 'a' arm wraps in `begin_action(ActionKind::Replace)` + `edit_invalidate()`.
Rows never shift (single-line replacements) so row-major order is preserved and
drift only applies within `anchor` row.
Needs from S5/buffer.rs (optional optimization): `find_all_from(pos)` non-wrapping
variant; v1 uses find_all + filter (correct, one allocation).

### Tests
RED `replace_all_from_cursor_and_no_overlap`: buffer "aaa", cursor (0,0),
`do_replace_all` via answer 'a' → lines == ["ba"], replace_count == 1
(current impl yields "ab").
GREEN `replace_all_multiline`: "aa\naa\naa" find "aa" with "x" → 3 replacements,
cursor (2,1).
GREEN `replace_all_drift`: "aaaa" find "aa" with "bbb" → "bbbbbb" (2 repl, drift+1).

### Touches
main.rs: new fn + 'a' arm; buffer.rs untouched (replace_at already fits).

## D2 — sort_by_cached_key

### Anchors
main.rs:1177-1181 `region.sort_by(|x, y| { String allocs per comparison })`.

### Design
`region.sort_by_cached_key(|l| l.iter().collect::<String>().to_lowercase());`
Behavior-identical; existing coverage via manual use.

### Tests
GREEN `sort_lines_region_and_case` (write if missing): marked region sorts
case-insensitively, unmarked sorts whole buffer.

### Touches
main.rs sort_lines only.

## D3 — region-based undo

### Anchors
- Fields main.rs:102-104: `undo/redo: Vec<(Vec<Vec<char>>, Pos)>`, `last_kind`.
- UNDO_LIMIT main.rs:306, begin_action 308-317 (clones whole buffer per step),
  undo 319-330, redo 332-343.
- delete_selection_if_any 345-357 (edits inside other actions).
- Actions: insert_char 361, newline 369, backspace 379, delete_at 403, cut 424,
  paste 464, delete_char_cut 506; justify 1125 (splice top..=bot), sort 1167,
  replace 'a' 872; do_read 1031 (replaces/inserts), do_exec insert 937-947.

### Design
```rust
pub(crate) struct UndoStep {
    pub kind: ActionKind,
    pub start: usize,            // row index, PRE-edit coords
    pub before: Vec<Vec<char>>,  // pre-edit rows [start, start+len)  (may be empty → pure insertion)
    pub after_start: usize,      // row index, POST-edit coords
    pub after: Vec<Vec<char>>,   // post-edit rows (may be empty → pure deletion)
    pub cur_before: Pos,
    pub cur_after: Pos,
    len_at_begin: usize,         // buf.lines.len() when the run began
}
```
- undo: `lines.splice(after_start..after_start+after.len(), before); cursor=cur_before;`
- redo: `lines.splice(start..start+before.len(), after); cursor=cur_after;`
- Stacks: `VecDeque<UndoStep>` both; UNDO_LIMIT trims `pop_front` (O(1)).
- Recording API on Editor:
```rust
fn begin_action(&mut self, kind: ActionKind, first_row: usize, last_row_inclusive: usize)
// empty region allowed: first_row == last_row+1 (pure insertion point)
// snapshots before-rows, after_start = first_row, len_at_begin = lines.len()
fn finish_step(&mut self)
// after = lines[after_start .. after_start + before.len() + lines.len() - len_at_begin]
//   (clamped to lines.len()); push step unless coalesced
```
- Coalescing (checked in finish_step): coalesce into `undo.back()` iff
  `last_kind == Some(kind)` AND kind ∈ {Insert, Backspace, Delete} AND existing
  step is single-row (before.len()==1 && after.len()<=1 && start==row involved)
  → update `after[0]` (or clear) from current lines + `cur_after = cursor`.
  Cut: repeated ^K coalesces iff `last_kind == Some(Cut)` AND `cursor.row ==
  step.cur_after.row`; extend `before` with pre-edit row `start + before.len()`
  when the cut removes a NEW row (cursor.row == start + before.len() pre-cut);
  partial→full cut of an already-included row extends nothing. Recompute after
  via the unified formula. Newline/Paste/Justify/Sort/Replace/ReadFile/Exec/Filter
  never coalesce.
- Per-action region table (first_row, last_row, expected delta):
  - insert_char no-sel: (cur.row, cur.row), Δ0; with selection: (min(mark.row,cur.row), max(...)) — covers delete_selection_if_any's row removals, Δ negative allowed.
  - newline: (row, row), Δ+1 (indent from F3 changes after content only).
  - backspace col>0: (row,row) Δ0 [coalesces]; col==0 join: (row-1, row) Δ-1 [no coalesce].
  - delete_at col<len: (row,row) Δ0 [coalesces; note buffer.remove_empty_line may drop the row → Δ-1, formula handles]; col==len join: (row, row+1) Δ-1 [no coalesce].
  - cut selection: (a.row, b.row) Δcomputed; cut line-at-EOL: (row,row) Δ-1; cut partial: (row,row) Δ0.
  - paste inline: (row,row) Δ0; paste lines: (row,row) Δ+n.
  - delete_char_cut: (row,row) Δ0.
  - justify: (top,bot) Δ = new.len()-(bot-top+1). sort: (top,bot) Δ0.
  - replace_all_from: (first_match.row, last_match.row) Δ0.
  - do_read replace-empty: (0,0) Δ+n-1; do_read insert: (cur.row, cur.row-1) EMPTY before, Δ+n.
  - do_exec insert (D7): (row, row-1) empty, Δ+n. filter (F6): (a.row, b.row) Δcomputed.
  - E1 paste_text: inline (row,row) Δ0; multi (row,row) Δ+k.
- `last_kind` semantics unchanged (set per action, cleared on undo/redo).
- Mark: cleared by undo/redo as today.

### Tests (write ALL first — green on snapshot impl, must stay green after)
mod ed_tests in main.rs (helpers `test_ed(text) -> Editor`, `press(ed, code, mods)`):
- undo_types_coalesce: "abc" typed (3 presses) → M-U once → ""
- undo_backspace_run: "ab" then 2×Backspace → M-U once → "ab"
- undo_redo_roundtrip: edit → M-U → M-E → identical lines+cursor
- undo_repeated_cut_coalesce: "1\n2\n3\n4", 3×^K → M-U once → original
- undo_partial_then_full_cut: "hello", ^K (partial at col2) then ^K (full) → M-U once → "hello"
- undo_paste_roundtrip, undo_replace_all_one_step ('a' path → M-U restores all)
- undo_limit_trims: 600×Enter → 500 steps max (assert ed.undo.len())
- undo_selection_overwrite: mark "bc" in "abcd", type 'X' → M-U → "abcd"
- undo_delete_selection: mark, Backspace → M-U restores
- undo_newline_join: Enter then Backspace → M-U twice returns original
- undo_read_empty_replace: ^R into empty buffer → M-U → empty
- undo_redo_after_new_edit: M-U then type → redo stack cleared (M-E no-op)
After refactor: rerun all + full suite. F8 later re-homes fields into Pane —
tests go through press() so they survive.

### Touches
main.rs: fields, begin_action/undo/redo rewritten, every action's begin/finish
calls inserted, UNDO_LIMIT const, new UndoStep (G1 → src/undo.rs with
ActionKind). buffer.rs: none.

## Risks
- delete_selection_if_any inside backspace/delete_at/insert_char: region must
  span mark rows (table above); forgetting it is the top regression risk.
- buffer.remove_empty_line inside cut_range/delete_at shifts rows — the unified
  after-formula (computed from lines.len() delta) absorbs it; do NOT compute
  after-rows by tracking indices manually.
- Coalesced Insert after selection-delete: last_kind==Insert but region grew —
  rule requires single-row step; selection case always starts a fresh step
  (guard: skip coalesce if before.len()!=1).
- Empty-region steps (before==[]): splice ranges degenerate to insert — splice
  handles len-0 ranges; test paste_text/exec after D7 lands.
