# S3 — LSP: debounce, async handshake, diagnostics surfacing (+ lsp coverage tests)

Companion to PLAN.md phases A (lsp part), D4, D5, E5. Grounded to code as of
2026-09-06 (main.rs 1528 lines, lsp.rs 485 lines). Execution order: A-lsp tests
can land anytime (pure extraction); D4 → D5 → E5 strictly (D5 adopts catch-up via
D4's flush state; E5 reads diags D5's flow produces).

## A-lsp — Phase A coverage tests for lsp.rs

### Anchors
- `change()` lsp.rs:321-348 — builds didChange inline: `let lines = text.split('\n')…`;
  `last = lines.len().saturating_sub(1)`; `last_len` summed via `c.len_utf16() as u64`
  (lsp.rs:331-334); range end `{line: last, character: last_len}` (lsp.rs:342).
- `poll()` lsp.rs:353-364 — `while let Ok(e) = self.rx.try_recv()`; ServerRequest arm
  calls `self.respond(id, Value::Null)` (lsp.rs:357-359); respond writes via
  `write_message(self.stdin.as_mut(), …)` (lsp.rs:367-370).
- `LspClient` fields all private but the `#[cfg(test)] mod tests` (lsp.rs:389+) is in
  the same module → struct-literal construction is legal there.
- Existing test style: `rust_diagnostics_flow` lsp.rs:443-484 (skips when server absent).

### Design
- Extract pure fn (change() becomes a thin wrapper):
  ```rust
  fn did_change_params(uri: &str, version: i32, text: &str) -> Value
  // contentChanges[0].range.end = { line: last, character: utf16_len(last_line) }
  ```
  change() keeps doc_version bump + notify, calls did_change_params(&uri, self.doc_version, text).
- Test-only poll fixture (inside `mod tests`):
  ```rust
  struct SharedBuf(Arc<Mutex<Vec<u8>>>);
  impl Write for SharedBuf { … appends to the mutex vec … }
  fn test_client(rx: mpsc::Receiver<LspEvent>, buf: Arc<Mutex<Vec<u8>>>) -> LspClient {
      let child = Command::new("cat").stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
      LspClient { child, stdin: Box::new(SharedBuf(buf)), next_id: 1,
                  pending: Arc::new(Mutex::new(HashMap::new())), rx,
                  root_uri: "file:///tmp".into(), doc_uri: None, doc_path: None,
                  doc_version: 1, initialized: true, lang: Lang::Rust }
  }
  ```
  `cat`'s stdin pipe is never taken, so it blocks forever; Drop::shutdown kills it
  (lsp.rs:378-379). No server binary needed.

### Tests
- red (compile-fail: did_change_params doesn't exist) → green after extraction:
  - `did_change_params("file:///t", 3, "a𝕏\nb\n")` → end `{line:2, character:0}`
    (split gives ["a𝕏","b",""], last line empty); version 3 in payload; uri echoed.
  - `did_change_params(uri, 1, "abc")` → end `{0,3}`.
  - `did_change_params(uri, 1, "a𝕏")` → end `{0,3}` (astral char = 2 UTF-16 units).
- green (characterization): preload rx with `LspEvent::ServerRequest{id:5, method:"workspace/x"}`;
  `poll()` → SharedBuf contains `"id":5` and `"result":null`; poll returns empty Vec
  (request consumed, not surfaced).
- green: `poll()` with empty rx → returns empty, writes nothing.

### Touches
- src/lsp.rs only (extraction + mod tests additions).

### Risks
- None functional: extraction is byte-identical math. G1: lsp.rs already standalone.
- F8: irrelevant here (client stays global per active doc).

## D4 — didChange debounce

### Anchors
- `edit_invalidate` main.rs:202-210: `if let Some(l) = self.lsp.as_mut() { l.change(&self.buf.text()); }`
  — full `buf.text()` alloc + full-doc send per keystroke (TODO §3 item, TODO.md:23-24).
- Run loop main.rs:1503-1524: `ed.tick_status(); ed.lsp_poll();` at 1522-1523 — flush
  call goes here; poll timeout 200ms (main.rs:1512) so worst-case send latency ≈ 500ms.
- `save_to` main.rs:1002-1011 calls `self.lsp_sync()` at 1010 — lsp_sync may kill the
  client, so the pending change must flush BEFORE lsp_sync.
- `Editor` fields main.rs:79-105; init site Editor::new main.rs:108-141.

### Design
- Editor fields: `pub lsp_dirty: bool` (init false), `pub lsp_last_send: Instant`
  (init Instant::now()).
- edit_invalidate (main.rs:207-209) becomes: `self.lsp_dirty = true;` — no lsp access,
  no text() alloc. (hl.refresh/search clearing stay.)
- ```rust
  /// Send pending didChange if the 300 ms window elapsed. Clears the flag even
  /// when no client is attached (lsp None), so a later handshake doesn't send
  /// a stale extra change (D5's adopt already catch-ups with current text).
  fn lsp_flush(&mut self, now: Instant) {
      if !self.lsp_dirty { return; }
      if now < self.lsp_last_send + Duration::from_millis(300) { return; }
      if let Some(l) = self.lsp.as_mut() { l.change(&self.buf.text()); }
      self.lsp_dirty = false;
      self.lsp_last_send = now;
  }
  ```
- Call sites: run loop after `ed.lsp_poll()` → `ed.lsp_flush(Instant::now());`
  (main.rs:1523 area); `save_to` flushes before `lsp_sync` (main.rs:1010 → insert
  `self.lsp_flush(Instant::now());` above); quit path: run loop breaks on
  `ed.quit` at main.rs:1509-1511 BEFORE flush — move `ed.lsp_flush(Instant::now())`
  above the `if ed.quit` check so a final edit is sent before shutdown/exit.
- Threading: all on UI thread; no locks. `Instant` monotonic; tests pass synthetic
  `now` values, no sleeping.

### Tests
- red: `test_ed()` with lang-less scratch buffer (lsp None): simulate edit via
  `ed.edit_invalidate()` → assert `ed.lsp_dirty == true`; `ed.lsp_flush(now)` →
  still dirty; `ed.lsp_flush(now + 400ms)` → dirty false, lsp_last_send advanced.
- red (the None path): after the 400ms flush with `lsp: None`, flag MUST be false
  (current code would keep it true forever without a client — that's the trap).
- green (behavioral, optional, needs rust-analyzer → mark `#[ignore]` like
  rust_diagnostics_flow): typed edits within 300ms produce ≤1 didChange on the wire
  (assert via RANO_LSP_RAW? no — skip; debounce correctness is covered by flag test).

### Touches
- src/main.rs: Editor struct + new(), edit_invalidate, new lsp_flush, run loop,
  save_to. No lsp.rs change.

### Risks
- F8: `lsp_dirty`/`lsp_last_send` are per-buffer state → move into `Pane`
  (PLAN.md F8 field list already includes them). Keep method signatures taking
  `&mut self` reading through `self.pane_mut()` so the move is mechanical.
- G1: lsp_flush moves to editor.rs with the other Editor methods (PLAN.md G1 list).
- Debounce + quit: final keystroke before ^X must flush (covered by call-site move).

## D5 — async LSP handshake

### Anchors
- `LspClient::spawn` lsp.rs:216-248 is sync; the 15s hang point is
  `self.wait(id, rx, Duration::from_secs(15))` at lsp.rs:296 inside `handshake`
  (lsp.rs:278-316). TODO.md:25.
- Blocking call site `lsp_sync` main.rs:247-250:
  `match lsp::LspClient::spawn(lang, &root, &name, &text) { Ok(client) => self.lsp = Some(client), Err(e) => self.flash(&e) }`.
- lsp_sync restart logic main.rs:229-239: `need` check → `self.lsp.take()` +
  `old.shutdown()` → `self.lsp_diags.clear()` (239).
- `lsp_poll` main.rs:255-264 returns early when `self.lsp` is None (main.rs:256-258)
  — must learn to check `lsp_starting` first.
- `LspClient::drop` → `shutdown()` lsp.rs:383-387 (request/wait 2s + kill).

### Design
- lsp.rs (spawn stays, used by tests):
  ```rust
  /// Spawn on a background thread; the initialize handshake (up to 15 s) never
  /// blocks the UI. Receiver yields exactly one result.
  pub fn spawn_async(lang: Lang, root: &Path, doc: &Path, text: &str)
      -> mpsc::Receiver<Result<LspClient, String>>
  {
      let (tx, rx) = mpsc::channel();
      let (root, doc) = (root.to_path_buf(), doc.to_path_buf());
      let text = text.to_string();
      std::thread::spawn(move || { let _ = tx.send(LspClient::spawn(lang, &root, &doc, &text)); });
      rx
  }
  ```
- Editor state: `pub lsp_starting: Option<(String, mpsc::Receiver<Result<LspClient, String>>)>`
  — the `String` tag is the `buf.name` (doc path) the handshake was started FOR.
- `lsp_sync` main.rs:247-250 becomes:
  ```rust
  // Dropping a stale receiver drops its in-flight LspClient (→ Drop::shutdown).
  self.lsp_starting = Some((name.to_string_lossy().into_owned(),
                            lsp::LspClient::spawn_async(lang, &root, &name, &text)));
  ```
  The stop-old + clear-diags lines (236-239) run unchanged and immediately — a
  restart while a handshake is in flight replaces `lsp_starting`, dropping the
  stale receiver.
- `lsp_poll` rewritten:
  ```rust
  fn lsp_poll(&mut self) {
      if let Some((tag, rx)) = self.lsp_starting.as_mut() {
          match rx.try_recv() {
              Ok(Ok(mut client)) => {
                  let started_for = tag.clone();
                  self.lsp_starting = None;
                  if self.buf.name.as_deref() != Some(Path::new(&started_for)) {
                      return; // stale: user saved to a different file meanwhile;
                              // dropping `client` here runs shutdown() (see Risks)
                  }
                  self.lsp = Some(client); // …or move out first, see below
                  // catch-up: edits made during the handshake were only flagged
                  if self.lsp_dirty {
                      if let Some(l) = self.lsp.as_mut() { l.change(&self.buf.text()); }
                      self.lsp_dirty = false;
                      self.lsp_last_send = Instant::now();
                  }
              }
              Ok(Err(e)) => {
                  let started_for = tag.clone();
                  self.lsp_starting = None;
                  if self.buf.name.as_deref() == Some(Path::new(&started_for)) {
                      self.flash(&e);   // stale failures are silent
                  }
              }
              Err(mpsc::TryRecvError::Empty) => {}
              Err(mpsc::TryRecvError::Disconnected) => { self.lsp_starting = None; }
          }
          // still fall through to draining an already-attached client below
      }
      if let Some(l) = self.lsp.as_mut() {
          for e in l.poll() { if let lsp::LspEvent::Diagnostics { diags, .. } = e { self.lsp_diags = diags; } }
      }
  }
  ```
  Note: move `client` out of the Option before the catch-up branch (avoid borrow
  clash with `self.buf`): `let mut client = match rx.try_recv() { … }` pattern —
  take `lsp_starting` first (`let pending = self.lsp_starting.take()`), then match.
  Diags between start and adopt: none — lsp_sync cleared them (main.rs:239) and no
  client existed; the server re-pushes after didOpen (handshake lsp.rs:303-313).
  uri check on Diagnostics events: optional hardening — `client.doc_uri` is known;
  filter `e.uri == doc_uri` to be safe for F8 (cheap, do it now).

### Tests
- red (Err-adoption, injected): build `test_ed()` named scratch→ set
  `ed.buf.name = Some(PathBuf::from("/tmp/x.rs"))`; `let (tx, rx) = mpsc::channel();
  tx.send(Err("boom".into())).unwrap();` `ed.lsp_starting = Some(("/tmp/x.rs".into(), rx));`
  → `ed.lsp_poll()` → status/flash contains "boom" (`ed.status` Some with message
  containing "boom"), `ed.lsp.is_none()`, `ed.lsp_starting.is_none()`.
- red (stale tag): same but tag `"/tmp/other.rs"` while buf name is `/tmp/x.rs` →
  no flash, lsp still None.
- green (Ok-adoption, injected): `tx.send(Ok(<test_client from A-lsp, initialized:true>))`
  → lsp_poll → `ed.lsp.is_some()`; with `lsp_dirty=true` pre-set, poll() writes a
  didChange into SharedBuf (assert buffer non-empty) and clears lsp_dirty.
- red (flag-when-no-client interplay, from D4): adopt with dirty=false sends nothing.
- green (existing): rust_diagnostics_flow untouched (still uses sync spawn).

### Touches
- src/lsp.rs: spawn_async (+ mod tests fixture reuse). src/main.rs: Editor field +
  init, lsp_sync, lsp_poll. No ui.rs change.

### Risks
- Dropping a stale receiver whose Ok(LspClient) already sits in the channel runs
  `LspClient::drop → shutdown()` ON THE UI THREAD: up to 2s request-wait + kill.
  Worst case is rare (save-to-different-language mid-handshake). Acceptable now;
  alternative if it bites: send the stale client to a janitor thread
  (mpsc::Sender<Result<LspClient,String>> kept on Editor, thread drops it).
- Handshake thread leaks nothing: channel send of Err on spawn failure; text/root
  cloned into the thread (no borrows → no lifetime issues).
- F8: `lsp` + `lsp_starting` stay on Editor (client is per-active-doc, restarted by
  lsp_sync on pane switch — PLAN.md F8 keeps them global); the tag mechanism is
  exactly the "global-but-validated" design the tag requirement asks for. `lsp_diags`
  moves into Pane in F8 (per-pane), validated by the uri filter added above.
- G1: lsp_sync/lsp_poll move verbatim to editor.rs.
- Test-only constructor is `#[cfg(test)]`-scoped in lsp.rs — never compiled into
  the binary.

## E5 — diagnostics surfacing (underline + M-D jump)

### Anchors
- `parse_diagnostics` lsp.rs:161-174: reads only `range.start.{line,character}`
  (166-167); `Diagnostic` struct lsp.rs:22-29 has `{line, col, message, severity}`.
- `char_style` main.rs:1188-1210: order today = search match (1191-1196, early
  return) → selection (1197-1204, early return) → syntax `hl.style_at` (1206-1208)
  → default. ui::draw consumes it per char (no change needed; D6 will wrap it).
- Alt bindings main.rs:1235-1252: **CONFLICT — `KeyCode::Char('d')` at main.rs:1247
  is already `self.prev_word()`** ("nano alternates for Prev/Next Word"). M-D must
  be freed for next-diagnostic (TODO.md:37-38; PLAN.md C2 lists "M-D diag").
  Resolution (default, confirm with user): move `prev_word`→Alt+Left,
  `next_word`→Alt+Right (`KeyCode::Left/Right` + ALT), free M-D for
  `jump_next_diag()`. Bindings table/help text updated in C2's single source.
- `lsp_status` main.rs:267-299 unaffected.

### Design
- lsp.rs: `Diagnostic` gains `pub end_col: usize`; parse_diagnostics:
  `let end_col = range["end"]["character"].as_u64().unwrap_or(0) as usize;` (lsp.rs:167 area).
  Update `parses_diagnostics` test (lsp.rs:418-431) + any struct literals.
- char_style placement — INSERT between the selection block (ends main.rs:1204)
  and the syntax block (main.rs:1205-1208), i.e. LAST before syntax:
  ```rust
  // Diagnostics: underline in severity color. Search match and selection
  // return above, so they still win.
  for d in &self.lsp_diags {
      if d.line == p.row && d.col <= p.col && p.col < d.end_col {
          let c = match d.severity { 1 => Color::Red, 2 => Color::Yellow, _ => Color::Blue };
          return Style::default().fg(c).add_modifier(Modifier::UNDERLINED);
      }
  }
  ```
  This REPLACES the syntax style on the range (early return before style_at) —
  intended: the underline must be visible regardless of token color.
  ui::draw needs zero changes (it calls `ed.char_style(pos)` per cell).
- Jump:
  ```rust
  /// M-D: move to the next diagnostic line strictly below the cursor, wrapping.
  fn jump_next_diag(&mut self) {
      if self.lsp_diags.is_empty() { self.flash("No diagnostics"); return; }
      let mut ds: Vec<&lsp::Diagnostic> = self.lsp_diags.iter().collect();
      ds.sort_by_key(|d| (d.line, d.col));
      let next = ds.iter().find(|d| d.line > self.cursor.row)
          .unwrap_or(&ds[0]);                       // wrap to first
      self.cursor = Pos { row: next.line, col: next.col.min(self.buf.line_len(next.line)) };
      self.edit_invalidate-free: no; adjust_scroll happens in run loop (main.rs:1507).
  }
  ```
  (col clamped to line_len — LSP cols are UTF-16 units and can exceed the char
  count of the line; see Risks.)
- Binding: in alt arm (main.rs:1238-1250): `KeyCode::Char('d') => self.jump_next_diag()`,
  and move prev_word/next_word to `KeyCode::Left | KeyCode::Right` under ALT.

### Tests
- red: `parses_diagnostics` (lsp.rs:418) extended — end `{2,9}` → `d.end_col == 9`;
  missing `range.end` → end_col 0.
- red: char_style — `test_ed("fn main() {}\n")` with `ed.lsp_diags = [Diagnostic{line:0, col:0, end_col:2, severity:1, …}]`:
  `ed.char_style(Pos{row:0,col:1})` fg == Red && UNDERLINED; col 3 (outside) → not;
  severity 2 → Yellow; severity 3 → Blue.
- red: priority — with an active selection covering the diag range, selection style
  (White on DarkGray) wins; with a current search match overlapping, search style
  (Black on Yellow) wins.
- red: jump — diags on lines 1 and 3: cursor (0,0) → jump → (1, col-of-first-on-1);
  jump → (3, …); jump → wraps to (1, …). col clamp: diag col 99 on a 5-char line →
  col == 5.
- green: empty diags → flash "No diagnostics", cursor unmoved.

### Touches
- src/lsp.rs (struct + parser + tests), src/main.rs (char_style, jump_next_diag,
  alt arm). No ui.rs change. (Help text/README follow C2's bindings.rs.)

### Risks
- UTF-16 vs char-index: `col`/`end_col` are UTF-16 units, buffer cols are char
  counts; astral chars (emoji) before the range shift underlines left. Pre-existing
  for `col` (lsp_status); accepted limitation, note in code comment. Proper fix =
  char↔utf16 mapping helper (out of scope here).
- rust-analyzer pushes diags once at open (lsp.rs:438-441 comment) — underlines
  will show stale ranges after edits until the next push; debounce (D4) doesn't
  change that. Fine for v1.
- M-D reassignment touches the word-motion bindings — update the alt-arm comment
  (main.rs:1236-1237) and ensure C2's BAR/help table matches (do E5 before or with
  C2 to avoid doc drift; if C2 landed first, edit bindings.rs only).
- F8: `lsp_diags` → `Pane` field (PLAN.md F8); char_style/jump read
  `self.pane().lsp_diags`. jump_next_diag's cursor write goes to pane_mut().
- G1: char_style + jump_next_diag → editor.rs; tests move with them.
