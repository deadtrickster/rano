# S4 — render pipeline: spans, dirty-flag redraw, horizontal scroll, tabs, gutter

Grounded to code as of 2026-09-06 (main.rs 1528 lines, ui.rs 284 lines). Covers
TODO §3 "ui::draw" item and TODO §5 horizontal-scroll + gutter. Order: D6 → E3 →
F2 → F4. D6 is self-contained; E3/F2/F4 each build on the previous (composition
contract in the final section — every later item must keep that formula true).

---

## D6 — draw-path optimization

### Anchors
- `ui::draw` text loop (ui.rs:77-90): clones `Vec<char>` per visible line per
  frame (`ed.buf.lines.get(r).cloned()`), then builds one `Span` per char via
  `ed.char_style(Pos{row: r, col: i})` (ui.rs:82) — O(visible chars) allocations
  per frame.
- `Editor::char_style` (main.rs:1188-1210): priority = current search match →
  selection (normalize(mark, cursor)) → `hl.style_at(p)` (syntax.rs:141-149,
  per-char grid, `None` for default) → `Style::default()`. Already works on
  absolute `Pos`; nothing here changes its contract.
- `Editor::new` is private (`fn new`, main.rs:108) — ui.rs tests cannot build an
  Editor today. `Buffer::new` is pub; all Editor fields used by draw are pub.
- Run loop (main.rs:1503-1524): `terminal.draw` runs at the TOP of every
  iteration; the loop cycles at least every 200 ms (`event::poll` timeout), so
  idle redraws happen 5×/s. `Event::Paste` and `Event::Resize` fall into
  `_ => {}` (main.rs:1519). `ed.text_w/text_h` refreshed from `terminal.size()`
  every iteration (main.rs:1504-1506).
- `tick_status` (main.rs:180-186) returns `()`; `lsp_poll` (main.rs:255-264)
  returns `()` (drains `LspEvent`s into `lsp_diags`).
- ratatui 0.30.2 → `ratatui::backend::TestBackend` available for draw-level
  tests. `Style: PartialEq` (coalescing works).
- Later phases that must hook the loop: D4 `lsp_flush` (PLAN.md), D7 `exec_poll`
  (PLAN.md), E1 `Event::Paste`, C3 loc expiry inside `tick_status`.

### Design

**1. Span coalescing + no line clones** — new fn in ui.rs:

```rust
/// One rendered text line. `scroll_x` is the left edge of the visible window
/// (in char cols until F2, display cols after); `max_w` the viewport width.
/// Styles are resolved at ABSOLUTE positions (abs_row, scroll_x + i) so
/// char_style/search/selection lookups never see window-relative coords.
fn line_to_spans(chars: &[char], abs_row: usize, scroll_x: usize, max_w: usize, ed: &Editor) -> Line
```

Body (D6 version; E3 replaces the windowing, F2 the col mapping):
- window = `chars[scroll_x .. min(scroll_x + max_w, chars.len())]` (empty →
  `Line::default()`).
- Walk the window, resolve `ed.char_style(Pos { row: abs_row, col: scroll_x + i })`,
  and coalesce: keep `(Style, String)` accumulator; when the next char's style
  `==` the running style, push the char into the running string; else flush a
  `Span::styled` and start a new run. Returns `Line::from(spans)`.

`draw` text loop becomes (no clone):
```rust
for r in ed.scroll..ed.scroll + text_h {
    match ed.buf.lines.get(r) {
        Some(chars) => lines.push(line_to_spans(chars, r, 0, width as usize, ed)),
        None => lines.push(Line::default()),
    }
}
```
(scroll_x arg is `0` until E3; the `Paragraph` render at ui.rs:87-90 unchanged.)

**2. Dirty-flag redraw** — restructure of run loop (main.rs:1492-1528):

```rust
let mut dirty = true;
let mut last_size = (0u16, 0u16);
let result = loop {
    let size = terminal.size()?;
    if (size.width, size.height) != last_size {
        last_size = (size.width, size.height);
        ed.text_w = /* see E3: full viewport width */;
        ed.text_h = (size.height as usize).saturating_sub(4);
        dirty = true;
    }
    ed.adjust_scroll(ed.text_h);
    if dirty {
        terminal.draw(|f| ui::draw(f, &ed))?;
        dirty = false;
    }
    if ed.quit { break Ok(()); }                       // moved OUT of `if dirty`
    if event::poll(Duration::from_millis(200))? {
        match event::read()? {
            Event::Key(k) if matches!(k.kind, KeyEventKind::Press | KeyEventKind::Repeat) => {
                ed.handle_key(k);
                dirty = true;                          // coarse: any key dirties
            }
            _ => {}                                    // E1 adds Paste arm here
        }
    }
    dirty |= ed.tick_status();                         // now returns bool
    dirty |= ed.lsp_poll();                            // now returns bool
    // D4 lands:  dirty |= ed.lsp_flush(Instant::now());
    // D7 lands:  dirty |= ed.exec_poll();
};
```

Complete enumeration of state changes that must set `dirty`:
1. **Any handled `Event::Key`** (Press|Repeat) — coarse on purpose: unbound keys
   cost one cheap no-diff redraw; per-key return-value plumbing (handle_key →
   bool) is not worth it. (key)
2. **`Event::Paste`** (E1) — `paste_text` mutates buffer. (paste)
3. **Resize** — both the size-cache mismatch above and a `Event::Resize` match
   arm (`_ => {}` becomes `Event::Resize(..) => dirty = true`). (resize)
4. **Status-flash expiry** — `tick_status(&mut self) -> bool`: returns true iff
   it cleared an expired `Flash` (main.rs:180-186). When C3 lands, loc_until
   expiry returns true the same way. (status-flash expiry)
5. **LSP event arrival** — `lsp_poll(&mut self) -> bool`: true iff ≥1
   `LspEvent` was drained into `lsp_diags` (diags change char_style + status).
   (lsp event arrival)
6. **`lsp_flush` actually sent** (D4) — true iff the debounced `didChange` went
   out (lsp state changed). (lsp flush sent)
7. **Exec job completion** (D7) — `exec_poll` true iff a job finished this tick
   (output inserted or error flashed). (exec job completion)

Nothing else mutates visible state outside an event handler, so the enumeration
is closed. Poll timeout stays 200 ms; initial `dirty = true` covers first paint.

Visibility changes for tests: `Editor::new` → `pub(crate)`; `tick_status`/
`lsp_poll` keep private visibility but gain `#[cfg(test)]`-reachable behavior
via editor tests (same module). No other signature churn.

### Tests (red|green)
- **red** `ui::line_to_spans_coalesces_unstyled`: Editor with scratch buffer
  line `"abc"` (no name → no syntax → all `Style::default()`), no mark/search →
  `line_to_spans(&chars, 0, 0, 80, &ed).spans.len() == 1` and spans[0].content
  == "abc". (Today's per-char path would produce 3 — the fn doesn't exist: red
  by absence; assert after extraction.)
- **red** `ui::line_to_spans_selection_three_spans`: same line, `ed.mark =
  Some(Pos{row:0,col:1})`, `ed.cursor = Pos{row:0,col:2}` → 3 spans: unstyled,
  `fg White bg DarkGray`, unstyled.
- **red** `ed_tests::tick_status_returns_bool`: expired flash set by hand →
  `ed.tick_status() == true` and `ed.status.is_none()`; live flash → false.
  (Red: currently returns `()`.)
- **red** `ed_tests::lsp_poll_returns_bool` on adoption: preload client channel
  with a `LspEvent::Diagnostics` (test ctor as in PLAN.md Phase A poll test) →
  `lsp_poll() == true` and diags non-empty; second call → false.
- **green** `draw_skips_when_clean` (manual/eyeball, no unit test): verified by
  `TestBackend` buffer version counter? Not exposed — instead green test
  `draw_renders_text_after_keypress`: TestBackend(80,24) + draw → buffer cell
  (0,1) == first char of buffer line 0; cursor_position == (0,1). Guards the
  restructure didn't break first paint.
- green: existing 27 tests stay green (no behavior change intended).

### Touches
- src/ui.rs: new `line_to_spans`; draw text loop (ui.rs:77-90) rewritten.
- src/main.rs: `Editor::new` → `pub(crate)` (main.rs:108); `tick_status` →
  `-> bool` (main.rs:180-186); `lsp_poll` → `-> bool` (main.rs:255-264); run
  loop restructure (main.rs:1503-1524).
- Tests: new `#[cfg(test)] mod tests` in ui.rs (Editor construction via
  `crate::Editor::new(Buffer::new())`-style helper); editor tests in main.rs
  `mod tests` per PLAN.md harness conventions.

### Risks
- **quit check ordering**: `ed.quit` must be tested every iteration even when
  clean (it is — moved out of `if dirty`); `try_quit` sets it inside
  handle_key, which also dirtied the flag anyway.
- **cursor visibility**: ratatui sets the cursor only inside `draw`; skipping
  clean draws keeps the last cursor — correct since nothing moved. A flash
  expiring redraws (item 4), restoring the cursor after a status change.
- **autoresize**: `Terminal::draw` calls autoresize; since every real resize
  dirties via the size cache, the next draw resizes buffers before rendering.
- **coarse key dirtying** means every keypress redraws even for no-ops —
  acceptable; the win is killing the 5 Hz idle redraw + all clones/allocs.
- **F8**: `line_to_spans` takes `&Editor`; post-F8 it must read per-pane state
  (scroll, buf) — keep the signature and read `ed.pane()` internally so ui.rs
  doesn't churn (see cross-cutting risks).

---

## E3 — horizontal scroll

### Anchors
- No horizontal scroll today: `draw` renders `chars` from col 0 (ui.rs:81-84);
  cursor x is `ed.cursor.col` verbatim (ui.rs:185-186, clamped to width-1 —
  wrong past the right edge); long lines are silently clipped.
- `Editor` has `scroll` (vertical) but no `scroll_x` (main.rs:79-105).
  `adjust_scroll` (main.rs:188-200) is the vertical twin to mirror.
- **Pre-existing inconsistency**: `ed.text_w = size.width - 2` (main.rs:1505)
  but the text `Paragraph` renders at full `area.width` (ui.rs:89). text_w
  feeds only `justify` (main.rs:1126, `self.text_w.max(20)`).
- `char_style` (main.rs:1188-1210) is absolute-col based; `line_to_spans`
  (D6) already receives `scroll_x` but callers pass 0.

### Design
- `Editor` field: `pub scroll_x: usize` (init 0 in the struct literal
  main.rs:113-144).
- New methods (main.rs, next to `adjust_scroll`):

```rust
/// Display col of the cursor on its current line. E3: identity over
/// cursor.col; F2 swaps the body for ui::display_col (tab_width-aware).
fn cursor_col_display(&self, line: &[char]) -> usize { self.cursor.col }

/// Visible text width in display cols. E3: text_w; F4 subtracts the gutter.
fn text_view_w(&self) -> usize { self.text_w }

fn adjust_scroll_x(&mut self) {
    let w = self.text_view_w();
    if w == 0 { return; }
    let row = self.cursor.row.min(self.buf.lines.len().saturating_sub(1));
    let col = self.cursor_col_display(&self.buf.lines[row]);
    if col < self.scroll_x { self.scroll_x = col; }
    if col >= self.scroll_x + w { self.scroll_x = col + 1 - w; }
}
```
  (Exactly the user's spec: `col < scroll_x → scroll_x = col`; `col >=
  scroll_x + text_w → scroll_x = col + 1 - text_w`. No upper clamp needed — a
  short line can only pull scroll_x down via the first rule.)

- Run loop: `ed.adjust_scroll(ed.text_h); ed.adjust_scroll_x();` (main.rs:1507,
  every iteration — cheap, and self-heals after pane switches at F8).
- ui.rs `draw`: pass `ed.scroll_x` as `line_to_spans`'s scroll_x arg; text
  window becomes `chars[scroll_x .. scroll_x + width]` (slice, no copy).
- Cursor position (ui.rs:185-186): `cx = (ed.cursor.col -
  ed.scroll_x.min(ed.cursor.col)) as u16`, then `.min(width - 1)` as today.
  (Post-F2 the term becomes `display_col(line, cursor.col, tw) - scroll_x`;
  post-F4 add `g` — composed formula at the end of this file.)
- `char_style` calls inside `line_to_spans` keep absolute cols: with a char
  window the absolute col is `scroll_x + i` (D6 already does this).
- **text_w unification**: `ed.text_w = size.width as usize` (full text
  viewport, matching ui.rs:89); `justify` keeps today's wrap width by using
  `self.text_w.saturating_sub(2).max(20)` at main.rs:1126 (behavior preserved;
  the `-2` moves from the size plumbing into the one consumer that had it).
  F2/F4 later replace that expression with `self.text_view_w()` — see F2.

### Tests (red|green)
- **red** `ed_tests::adjust_scroll_x_follows_cursor_right`: buffer 40 `a`s,
  `text_w = 10`, `move_end` (col 40) + `adjust_scroll_x()` → `scroll_x == 31`
  (= 40 + 1 − 10). Red: field/fn absent.
- **red** `ed_tests::adjust_scroll_x_follows_cursor_left`: from the above,
  `move_home` + `adjust_scroll_x()` → `scroll_x == 0`; and a third state
  scroll_x=30 with cursor.col=5 → `scroll_x == 5`.
- **red** `ui::line_to_spans_slices_window`: line `"abcdefgh"`, `scroll_x = 5`,
  `max_w = 3` → single span content `"fgh"`; with a selection spanning absolute
  col 6, the middle span carries the selection style (proves absolute-col
  styling survives windowing).
- **green** `draw_cursor_clamped_with_scroll` (TestBackend 20×24, long line,
  cursor at EOL): cursor_position.x < 20 and points at the last visible col.
- green: E3 red tests from F2 (below) must keep passing — F2 updates their
  inputs to tabs, not their expectations.

### Touches
- src/main.rs: `scroll_x` field (main.rs:79-105, 113-144); `adjust_scroll_x`,
  `cursor_col_display`, `text_view_w` (new, near main.rs:188-200); run loop
  call (main.rs:1507); `text_w` size plumbing (main.rs:1505); justify width
  (main.rs:1126).
- src/ui.rs: scroll_x passed to `line_to_spans`; cursor x math (ui.rs:183-187).
- Tests: main.rs `mod tests`.

### Risks
- **EOL cursor col** = `line_len` (one past last char): `scroll_x = col+1-w`
  may expose a fully-empty right edge column — matches nano; fine.
- **F8**: `scroll_x` is per-pane state (moves into `Pane`, PLAN.md:331);
  `adjust_scroll_x` becomes a Pane/Editor method over `pane_mut()`. On pane
  switch the loop-top call re-clamps immediately.
- **G1**: the three new fns move to editor.rs verbatim with the Editor.
- Interaction with F2/F4 is by-construction: they only swap `cursor_col_display`
  / `text_view_w` bodies — no call-site changes.

---

## F2 — tab_width rendering

### Anchors
- Tabs are stored as `'\t'` chars (`KeyCode::Tab → insert_char('\t')`,
  main.rs:1294) and today render as a literal 1-col `^I`-ish glyph (terminal
  behavior), so cursor columns and rendered columns disagree.
- `justify` (main.rs:1125-1165): wrap check `cur.len() + 1 + w.chars().count()
  <= width` (main.rs:1146), width `self.text_w.max(20)` (main.rs:1126).
  **Nuance vs the brief**: `split_whitespace` (main.rs:1139) treats `\t` as
  whitespace, so reflowed lines contain no tabs and `cur.len()` already equals
  display width. The real gaps are (a) wrap `width` must become the F4-aware
  viewport width and (b) the loop should be tab-proof for the future. Both
  fixed here; a red test on "justify wraps differently because of tabs" is not
  achievable (documented contradiction with the brief — see Risks).
- `char_style` / `hl.style_at` are char-indexed (syntax grid per char,
  syntax.rs:157-161) — tabs are one char; highlighting is unaffected by
  display expansion.
- `current_match_range` (main.rs:1212-1220) is char-based (query char count) —
  unaffected.
- F1 (config) precedes F2 in the execution order, so `Editor.config.tab_width`
  exists; F2 adds a direct `pub tab_width: usize` field on Editor (default 8,
  seeded from config in F1's wiring) to avoid `ed.config.tab_width` churn
  everywhere.

### Design
- New pure helpers in ui.rs (pub(crate); display concerns live with draw):
```rust
/// Rendered width of `chars` with tabs advancing to the next multiple of
/// `tab_width` (tab_width 0 → treat as 1 to avoid div-by-zero).
pub(crate) fn display_width(chars: &[char], tab_width: usize) -> usize

/// Display col of char index `col` within `line` (clamped to line len).
pub(crate) fn display_col(line: &[char], col: usize, tab_width: usize) -> usize
```
  `display_width` = fold with running col: `'\t'` → `tw - (col % tw)`, else 1.
  `display_col(line, col, tw) = display_width(&line[..col.min(line.len())], tw)`.

- `line_to_spans` rewrite (E3 body replaced): walk ALL chars with a running
  display col `d`; skip until `d >= scroll_x`; for `'\t'` emit one
  `Span::styled(" ".repeat(step), tab_char_style)` (step = advance to next
  stop, clipped so `d` never exceeds `scroll_x + max_w`); for other chars emit
  1 col; stop when `d >= scroll_x + max_w`. Styles still resolved at absolute
  char index. `scroll_x` is now in DISPLAY cols.
- `adjust_scroll_x` (E3) swaps bodies only:
  `cursor_col_display` → `ui::display_col(line, self.cursor.col, self.tab_width)`;
  `text_view_w` unchanged (F4 changes it). No call-site edits.
- Cursor x in `draw` (ui.rs:183-187):
  `cx = ui::display_col(line_of_cursor_row, ed.cursor.col, ed.tab_width) -
  ed.scroll_x` (+ gutter after F4), clamped as today.
- `justify` (main.rs:1143-1152): wrap width → `self.text_view_w().max(20)`
  (E3 already moved the `-2` out; F4 makes `text_view_w` gutter-aware); running
  width → `ui::display_width(&cur, self.tab_width)`; word width stays
  `w.chars().count()` (words are tab-free — defensive note in a comment).
- E3's red tests gain tab variants (below); `scroll_x` semantics flip from
  char cols to display cols at this point — the only consumer transition.

### Tests (red|green)
- **red** `ui::display_width_tab8`: `display_width(&['a','b','\t'], 8) == 8`
  (brief's example); `display_width(&['\t'], 4) == 4`;
  `display_width(&['a','\t','b'], 8) == 10` (tab at col 1 → 8, then b → 9? no:
  col after tab = 8, +b = 9 — assert 9).
- **red** `ui::display_col_mapping`: line `"ab\tcd"`, tw 8 → col 0→0, 2→2,
  3→8 (cursor after the tab), 4→9, 6→11 (EOL clamp).
- **red** `ui::line_to_spans_tab_renders_spaces`: line `"a\tb"`, tw 8,
  scroll_x 0, max_w 80 → spans whose total content width == 9 and the tab run
  is 7 spaces carrying the tab char's style.
- **red** `ed_tests::adjust_scroll_x_tabs`: line `"a\tb…"` (tab early), tw 8,
  cursor at col 3 (display col 8), text_w 4 → `adjust_scroll_x()` →
  `scroll_x == 5` (= 8 + 1 − 4). Update E3's 40-`a` test stays as-is (no tabs →
  identical math).
- **green** `ed_tests::justify_wraps_at_viewport_width`: characterize that a
  paragraph of long words wraps to `text_view_w()` cols and emits no tabs
  (input line contains `\t\tfoo bar …` → tabs stripped, lines ≤ viewport).
- green: all D6/E3 tests stay green.

### Touches
- src/ui.rs: `display_width`, `display_col` (new pub(crate)); `line_to_spans`
  tab-aware rewrite; cursor x (ui.rs:183-187).
- src/main.rs: `tab_width` field (default 8; F1 seeds from config);
  `cursor_col_display` body swap; justify loop (main.rs:1143-1152, 1126).
- Tests: ui.rs tests + main.rs `mod tests`.

### Risks
- **scroll_x semantic flip**: any scroll_x persisted across the F2 boundary
  (none — runtime field, no serialization) or set by tests written pre-F2 (all
  updated here). F1 config only sets tab_width at startup; no mid-session
  toggle, so display-col scroll_x is stable.
- **syntax/search vs tabs**: both char-indexed; the tab's SPACES inherit the
  tab char's style — a search match covering a tab colors the whole expansion
  (desired).
- **justify "red with tabs" impossible**: `split_whitespace` strips tabs before
  the width loop, so `cur.len()` == display width today; the brief's premise
  ("must use display width") is satisfied defensively, not observably. The red
  tests target the helpers + viewport width instead. Flagged as a
  contradiction with the task brief / PLAN.md F2 wording.
- **F8**: `tab_width` is GLOBAL config (not per-pane); `display_width` is a
  free fn — no ownership issue.

---

## F4 — line-number gutter

### Anchors
- No gutter today: text `Paragraph` at `Rect::new(0, 1, width, text_h)`
  (ui.rs:87-90); cursor x has no offset (ui.rs:183-187); overlays: status row
  = `area.height - 3` (ui.rs:95), function bar last 2 rows (ui.rs:141-172),
  help overlay paints the whole `area` LAST (ui.rs:192-230) so it already
  covers any gutter.
- `M-n` is currently bound to `next_word` (main.rs:1248) — conflicts with
  nano's M-N line-numbers toggle that TODO §5 requests. `next_word` keeps
  M-d (main.rs:1247) and Ctrl+Right (main.rs:1285), so M-n is free to take.
- `Editor.text_view_w()` (E3) and `justify`'s width are the two width
  consumers that must shrink.
- F1 precedes F4: `config.line_numbers` exists; F4 adds the runtime toggle
  field seeded from it.

### Design
- `Editor` field: `pub show_line_numbers: bool` (init from `config.line_numbers`,
  default false).
- Pure fn in ui.rs:
```rust
/// Gutter columns for a buffer of `rows` lines: right-aligned number +
/// one trailing space, min width 2 digits.
pub(crate) fn gutter_width(rows: usize) -> usize {
    usize::max(2, digits(rows)) + 1   // digits(n) = n.to_string().len(), digits(0)=1
}
```
  **Contradiction resolved**: PLAN.md:292-293 states the formula
  `max(2, digits(rows)) + 1` but its own test says `gutter_width(10) == 4` —
  by the formula 10 (2 digits) gives 3. The formula is kept (monotone, matches
  "digits + separator"), the test is corrected: 9→3, 10→3, 100→4. PLAN.md must
  be amended when F4 lands.
- `text_view_w` body (E3 stub) becomes:
  `self.text_w.saturating_sub(if self.show_line_numbers { ui::gutter_width(self.buf.lines.len()) } else { 0 })`
  — `adjust_scroll_x` and `justify` inherit the shrink with zero call-site
  changes. `justify`'s `.max(20)` floor stays.
- `draw` changes (ui.rs:53-90, 183-187):
  - `let g = if ed.show_line_numbers { gutter_width(ed.buf.lines.len()) } else { 0 };`
  - gutter widget when `g > 0`: for each visible row `r` in
    `ed.scroll..ed.scroll + text_h`, text =
    `r < lines.len() ? format!("{:>w$} ", r + 1, w = g - 1) : " ".repeat(g)`,
    style `Style::default().fg(Color::DarkGray)` (dim), rendered as one
    `Paragraph` at `Rect::new(0, 1, g as u16, text_h as u16)`.
  - text `Paragraph` moves to `Rect::new(g as u16, 1, (width - g) as u16,
    text_h as u16)`; `line_to_spans` max_w = `width - g`.
  - cursor: `x = g + display_col(...) - scroll_x` (full formula below),
    `y` unchanged; clamp as today.
- Overlays: gutter exists only in text rows `1..1+text_h`; status row
  (`height-3`), function bar, prompt, and help overlay are untouched (help
  paints last and covers everything — verified anchor).
- Binding: alt arm (main.rs:1238-1250) `Char('n') => self.toggle_line_numbers()`
  (replacing `next_word` there); `toggle_line_numbers` flips the flag and
  flashes `"[ line numbers on ]"` / `"[ line numbers off ]"`. Help text +
  C2 bindings table get the M-N row (C2 owns the single source of truth).

### Tests (red|green)
- **red** `ui::gutter_width`: `gutter_width(1) == 3`, `gutter_width(9) == 3`,
  `gutter_width(10) == 3`, `gutter_width(99) == 3`, `gutter_width(100) == 4`.
  (Red vs PLAN.md's stated `gutter_width(10) == 4`; see contradiction above.)
- **red** `ed_tests::toggle_line_numbers_binding`: `press(M-n)` flips
  `ed.show_line_numbers` and sets a flash; `press(M-d)` still moves a word
  (guard against stealing the wrong binding). Red: field/binding absent.
- **green** `draw_gutter_layout` (TestBackend 40×24, 12-line buffer, toggle
  on): cells (1,1) == '1' right-aligned with one trailing space at x =
  gutter_width(12)−1 = 3; first text char of row 0 at x = 4;
  `cursor_position` == (4 + cursor.col, cursor.row + 1) for cursor (2,3).
  Toggle off → text back at x=0.
- **green** `gutter_scrolls_with_buffer`: 100-line buffer, scroll to bottom →
  visible numbers are 77..=100, width stays `gutter_width(100)` = 4.
- green: all D6/E3/F2 tests stay green (`text_view_w` shrink is exercised by
  the E3/F2 math tests with show_line_numbers=true variants).

### Touches
- src/ui.rs: `gutter_width`; draw gutter + text offset + cursor offset
  (ui.rs:53-90, 174-189).
- src/main.rs: `show_line_numbers` field; `toggle_line_numbers`; M-n rebinding
  (main.rs:1247-1248); `text_view_w` body (E3 stub).
- src/main.rs help text (ui.rs:196-213 pre-C2, bindings.rs post-C2) + README
  row deferred to Phase G per PLAN.md.
- Tests: ui.rs + main.rs `mod tests`.

### Risks
- **Width feedback loop**: gutter width depends on `lines.len()`, which changes
  on every newline/`^K` → viewport width `T` changes → `scroll_x` may need
  re-clamping. Handled: `adjust_scroll_x` runs every loop iteration (E3) and
  reads `text_view_w()` fresh.
- **M-n rebinding** changes an existing binding — the only user-visible
  regression risk in S4; flagged for C2's bindings table and README (G).
- **F8**: `show_line_numbers` is a GLOBAL view setting (config-driven, M-N
  toggles it for all panes — nano semantics), NOT per-pane; `gutter_width`
  takes the ACTIVE pane's `lines.len()`. `buf`/`cursor`/`scroll`/`scroll_x`
  move into `Pane`; `toggle_line_numbers` stays on Editor.
- **G1**: `gutter_width`/display helpers remain in ui.rs (display concerns);
  `toggle_line_numbers` moves to editor.rs.

---

## Composed draw() layout formula (E3 × F2 × F4 — the interaction contract)

Every item above is phrased to keep this single formula true; F2 and F4 only
swap terms inside it:

```text
W, H   = frame size;                    text_h = H - 4
g      = show_line_numbers ? gutter_width(active pane lines.len()) : 0   // F4
T      = W - g                          // text viewport width, DISPLAY cols
sx     = scroll_x                       // display cols (char cols until F2)  // E3
tw     = tab_width                      // F2

gutter widget : Rect { x: 0, y: 1, width: g, height: text_h }   (skip if g==0)
text  widget  : Rect { x: g, y: 1, width: T, height: text_h }
row r render  : line_to_spans(&lines[r], r, sx, T, ed)
                — window = display cols [sx, sx+T) of row r
                — tabs expand to spaces (F2), styles coalesced (D6),
                  style lookups at ABSOLUTE char cols
cursor        : x = g + display_col(lines[cur.row], cur.col, tw) - sx
                y = cur.row - scroll + 1            (clamped to viewport)
status/help/bar: rows H-3 / full-area overlay / H-2..H — independent of g, sx, tw
```

Composition rule (order matters): **char col → display col (F2) → subtract
scroll_x (E3, same units after F2) → add gutter (F4)**. The three terms act on
independent axes of one sum, so they commute numerically but must stay in the
same units — which is why `scroll_x` flips to display cols exactly when F2
lands and `g` enters `text_view_w()` exactly when F4 lands.

## Cross-cutting risks (F8 multi-buffer, G1 split)

- **Per-pane (into `Pane`, PLAN.md:329-336)**: `buf`, `cursor`, `scroll`,
  `scroll_x`, `mark`, `loc_until`, `search`, `lsp_diags`, `lsp_dirty`,
  `lsp_last_send`, undo/redo/last_kind — i.e. everything `adjust_scroll`,
  `adjust_scroll_x`, `char_style`, and `line_to_spans` read.
- **Global (stay on Editor)**: `show_line_numbers` (view setting, M-N global),
  `tab_width` + config, `text_w`/`text_h`, prompt/help/status/flash,
  `exec_job`, cutbuffers.
- **Gap in PLAN.md F8**: the `Pane` struct omits `hl: syntax::Highlighter`, but
  `char_style` → `hl.style_at` (main.rs:1206) needs it per buffer. `hl` must be
  added to `Pane` (and `hl.refresh` moves to pane-local edit paths). Flagged
  for amendment when F8 lands.
- **`line_to_spans(&Editor)` post-F8**: keep the signature; body reads
  `ed.pane().buf.lines` / `ed.pane().scroll_x` etc. `char_style` stays a
  delegating `Editor` method over `pane()`. No ui.rs churn at F8.
- **G1 split**: `Editor` moves to editor.rs; `use crate::Editor` in ui.rs keeps
  working via re-export; display helpers + `gutter_width` stay in ui.rs (no new
  module); `adjust_scroll_x`/`text_view_w`/`cursor_col_display` move with
  Editor/Pane verbatim.
- **Per-pane gutter width**: `g` derives from the active pane's line count, so
  `text_view_w()` (used by scroll_x and justify) is pane-dependent after F8 —
  `adjust_scroll_x` at loop top re-clamps on every switch; justify uses the
  active pane's width (correct).
- **Dirty-flag at F8**: pane switch is a key event → dirty (coarse rule covers
  it); lsp_flush/exec hooks remain global with pane-internal state.
