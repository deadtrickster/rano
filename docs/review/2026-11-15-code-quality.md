# Code quality, 2026-11-15

Asked by the operator. The findings below are the verbatim report of an independent reviewer
(a Fable 5.2 subagent of cc@rano) that read `9047198` without building or running it. cc@rano
then checked findings 1, 2, 3, 4 and 8 against the source and found the code as quoted. The
others were not re-checked. Master has since moved to `7d47254` (terminal resize, scroll
indicator, terminal keys in a map), so line numbers in `src/bottomterm.rs`, `src/keys.rs` and
`src/editor.rs` have shifted and parts of findings 8, 9 and 23 may already be addressed.
Decisions are at the end.

Read at `9047198` (master). Nothing was built, run or edited. Every finding was reached by reading
the code path named. "Confirmed" means I followed the path to its callers and found no guard.
"Plausible" means the code is as quoted but the trigger depends on something I could not see
(a peer's behaviour, timing).

## Verdict

The editor core I would ship. The four blockers of the 0.10 review are really fixed: the save is
temp + fsync + rename (`src/editor.rs:3966`), a partial buffer is refused at the one write door
(`src/editor.rs:2914`), `^T` no longer blocks on a held pipe (`src/exec.rs:101-113`), and the
text/cursor arithmetic I sampled is clamped (`visual_pos`, `begin_action`, `undo`, `write_hunk`).
The three newest surfaces I would not ship, and I would not let letibot move its rano pin past
them: the bottom terminal (`src/bottomterm.rs`) runs a raw `fork`, an unbounded pty drain and a
blocking `waitpid` on the UI thread, inside `Editor::tick` and `Editor::handle_key`, so every
embedder inherits it. The single biggest risk is the combination of findings 1 and 2: the terminal
can freeze the UI for as long as a program prints, the only way out is to kill rano from outside,
and rano has no signal handler and no recovery file, so that kill costs every unsaved buffer and
leaves the outer terminal raw. Second is the panel's session walk-up (finding 4), which loops or
dead-ends for a daemon that holds no session, a case no test fixture has.

Not re-reported here: what `TODO.md`, `docs/review/2026-11-03-release-0.10.md` and the two records
of today (`2026-11-15-feature-completeness.md`, `2026-11-15-keys.md`) already hold - the key leak
out of a focused terminal, Esc/PgUp taken from programs, the fixed height, the missing docs, the
overlay Options, `M-|` blocking, the undo byte budget, LSP items. Where I extend a recorded item it
says so.

## Findings

### 1. The pty drain loop has no budget: fast output freezes the editor

Severity: critical. Confidence: confirmed by reading, not run.

Location: `src/bottomterm.rs:71-83`, called from `src/host.rs:36` inside `Editor::tick`.

Evidence:

```rust
loop {
    let n = unsafe { libc::read(self.master, buf.as_mut_ptr() as *mut _, buf.len()) };
    if n <= 0 {
        break;
    }
    dirty = true;
    self.parser.process(&buf[..n as usize]);
}
```

The loop ends only on EAGAIN. Each 4 KiB chunk costs the emulator one `linefeed` per newline, and
a linefeed is three `Vec::drain(..cols)` plus `history.remove(0)` (`src/bottomterm.rs:330-344`).
A producer refills 4 KiB faster than that.

Scenario: `M-0`, type `yes` (or `cat` a large file, `find /`, a verbose build). `tick` never
returns, so no key is read and no frame is drawn. `C-c` would not reach the shell anyway (recorded
in the keys audit). The operator kills rano from another terminal, see finding 2. Embedded, the
same loop runs on letibot's UI thread under a doc comment that says `tick` "Never blocks"
(`src/host.rs:19`).

Fix: bound the drain per tick (bytes or a few milliseconds), report `dirty`, and let the next tick
continue. Longer term the pty belongs on a reader thread with a channel, like `exec.rs`.

### 2. No SIGHUP/SIGTERM handling and no emergency save

Severity: high. Confidence: confirmed (a search for `sigaction`, `signal(`, `SIGTERM`, `SIGHUP`
across `src/` finds only the `kill(pid, SIGHUP)` in `bottomterm.rs`).

Location: `src/term/terminal.rs:219-224` (the panic hook is the only abnormal-exit path),
`src/term/terminal.rs:504-508` (restore is in `Drop` only), `src/main.rs:377-457`.

Evidence: `std::panic::set_hook(Box::new(move |info| { restore(fd, &saved, features, options); prev(info); }));`
and nothing else. No autosave, swap or `.save` file exists anywhere in the tree.

Scenario A: an ssh session drops, or the terminal window is closed. SIGHUP's default action ends
the process. Every modified buffer is gone. nano writes `name.save` here.
Scenario B: `kill <pid>` (the only way out of finding 1, of a hung `M-|`, of a hung launcher).
`Drop` does not run, so the outer terminal stays in raw mode, on the alternate screen, with mouse
reporting and bracketed paste on.
Scenario C: a panic on the UI thread restores the terminal and unwinds; buffers are dropped
unsaved.

Fix: a self-pipe or `signal_hook`-style flag for HUP/TERM/INT that the loop reads; on it, write
each modified named buffer to `<name>.save` (atomic), restore the terminal, exit. Do the same from
the panic hook for the buffers, best effort.

### 3. A paste goes into the document while a prompt or the terminal has the keyboard, and `M-|` then panics

Severity: high. Confidence: confirmed by reading the full path.

Location: `src/editor.rs:2186-2211`, `src/main.rs:442-445`, `src/exec_ctrl.rs:98`.

Evidence:

```rust
pub fn paste_text(&mut self, s: &str) {
    if self.panel.focus && self.prompt.is_none() { ...composer...; return; }
    if self.refuse_read_only("paste") { return; }
    ...
    self.begin_action(ActionKind::Paste, first, last);
    self.delete_selection_if_any();
```

There is no branch for `self.prompt.is_some()` and none for `self.bottom.focus`. `main.rs` sends
every `Event::Paste` here.

Scenario A: `C-f` (Search:), paste the needle. The text is inserted into the file at the cursor,
the prompt stays empty, the buffer is modified. Same for Save-as, Open, Go-to, Replace.
Scenario B: focus the bottom terminal, paste a command. It lands in the file, not in the shell.
Scenario C (panic): select rows, `M-|`, type `sort ` and paste `-u`. The paste runs
`delete_selection_if_any`, which deletes the selection and sets `mark = None`
(`src/editor.rs:1188`). Enter then reaches
`let mark = self.bs().mark.expect("start_filter guarantees mark");` and panics. The guarantee held
only while nothing else could run between the prompt opening and Enter.

The comment at `src/editor.rs:2188` also says the composer's "newlines become spaces";
`Panel::insert` (`src/panel.rs:1028-1038`) inserts the `\n` as it is.

No test pastes with a prompt open or with the terminal focused (`src/ed_tests.rs:2879-2905` paste
into a bare buffer only).

Fix: route in one place, in priority order: prompt (insert at its cursor, first line only), bottom
terminal (`tab.send`, bracketed), panel composer (newline to space), else the buffer. Replace the
`expect` with a `let Some(mark) = ... else { flash; return }`.

### 4. The panel's walk-up to a parent folder never advances

Severity: high. Confidence: confirmed by reading for the loop; the daemon's answer for a
zero-session attach is taken from the comment at `src/main.rs:691-693`.

Location: `src/panel.rs:510-524`, `src/main.rs:685-721`.

Evidence:

```rust
0 => {
    if let Some(parent) = self.folder.parent().map(Path::to_path_buf) {
        self.state = PanelState::Connecting;
        self.said = format!("no session here; asking {}", parent.display());
        self.acts.push(Act::Connect(parent));
```

`self.folder` is not changed here, and `connect()` in `main.rs` does not change it either. So the
next empty `Sessions` frame computes the same parent again.

Scenario A: `$LETIBOT_SOCKET` is set (`letibot::named` answers every folder with it) and the daemon
holds no session. Connect, attach, list, empty list, `Act::Connect(parent)`, connect to the same
socket, empty list, the same act again. It never ends; each round drops the link (joins the reader
thread) and reconnects, about five times a second, and the panel never reaches "enter starts one".
Scenario B: no variable. The folder's daemon is up with no session. The walk asks the parent; no
daemon there, so `main.rs:708-712` recurses to `/` and ends in `no_daemon`. The working connection
was dropped at `main.rs:686`, the panel is `Offline { startable: true }`, Enter runs
`Act::Start(folder)`, the launcher answers "up", the list is empty again, and the same walk runs.
A first session can only be made from `/` (the one folder with no parent), which is what the unit
test uses (`src/panel.rs:1517-1526`).

Why the tests pass: `a_folder_with_no_session_asks_the_one_above_it` (`src/panel.rs:1497`) asserts
one step and stops. The scripted daemon in `tests/panel.rs:119-123` always holds one session and
answers a bare attach with it, and the test sets `LETIBOT_SOCKET` (`tests/panel.rs:202`), so
neither the launcher nor the empty list is ever on the path.

Fix: keep the folder being asked in the panel (`asking: PathBuf`), advance it per empty answer,
stop when it stops resolving to a different socket, and do not drop a live link to try a parent.
Add a fixture daemon with zero sessions and one with two.

### 5. `fork()` in a threaded process, with allocation and a lock in the child

Severity: high. Confidence: confirmed by reading; the failure is a race, so rare.

Location: `src/bottomterm.rs:28-52`.

Evidence:

```rust
let pid = unsafe { libc::fork() };
match pid {
    0 => {
        unsafe {
            ...
            let shell = shell();                       // std::env::var + CString (malloc)
            let cwd = std::ffi::CString::new(cwd.as_os_str().as_encoded_bytes())
                .expect("no NUL in a path");           // malloc, and a panic path
            libc::chdir(cwd.as_ptr());
            libc::execlp(...);
```

rano always has other threads by then (the update check from `main.rs:295`, loaders, the LSP
reader, the letibot reader; in letibot, the host's own). After `fork` only the calling thread
exists in the child, and only async-signal-safe calls are allowed. `std::env::var` takes std's
environment lock and `CString::new` allocates. If another thread held either at the instant of the
fork, the child deadlocks holding the pty's slave end, and the tab is a blank pane for ever. The
unsafe blocks carry no SAFETY comment at all. Also:

- No return value is checked: `setsid`, `dup2`, `TIOCSCTTY`, `chdir`, `fcntl`.
- When `fork` fails (`_ => Err(...)`, line 66) both `master` and `slave` leak.
- Signal dispositions are inherited. Rust ignores SIGPIPE in the parent, and `std::process::Command`
  resets it in its children, but this raw fork does not. Every program started from the bottom
  terminal runs with SIGPIPE ignored: `yes | head -1` prints "Broken pipe" instead of ending
  quietly.
- `TERM` is inherited from the outer terminal, so programs emit sequences for xterm or kitty at an
  emulator that knows a dozen (finding 9).

Fix: build both `CString`s and the argv before the fork; in the child call only `setsid`, `dup2`,
`ioctl`, `chdir`, `signal(SIGPIPE, SIG_DFL)`, `execvp`, `_exit`. Or use `Command` with
`pre_exec` for `setsid`/`TIOCSCTTY`, which already does the rest correctly. Set `TERM` to what the
emulator implements.

### 6. The pty master is not close-on-exec: every later child holds it

Severity: medium. Confidence: confirmed by reading.

Location: `src/bottomterm.rs:232-245` (`libc::openpty`, no `FD_CLOEXEC`), `:57`
(`fcntl(master, F_SETFL, O_NONBLOCK)` sets status flags only).

Scenario: open a terminal tab, then open a `.rs` file. rust-analyzer is spawned with the tab's
master fd inherited. So is every `^T` command, every `pdftoppm`, every `letibot --where`, and the
shell of every later tab (tab 2's shell holds tab 1's master). Closing tab 1 closes rano's copy
only; the kernel sees the master still open, so the slave side gets no hangup and background jobs
of that shell keep a live tty until the LSP server exits.

Fix: `fcntl(master, F_SETFD, FD_CLOEXEC)` right after `openpty` (the slave is closed in the parent
already).

### 7. Blocking waits on the UI thread in the panel and terminal paths

Severity: medium (high when embedded). Confidence: confirmed that there is no deadline; the hang
itself is plausible.

Evidence and scenarios:

- `src/letibot.rs:1224-1228`: `Command::new(launcher).args(&args).current_dir(folder).output()`.
  No deadline. `output()` returns when stdout and stderr reach EOF, so `letibot --where --ensure`
  blocks rano for as long as a daemon it started keeps either pipe (the same grandchild trap the
  0.10 review fixed in `exec.rs`). Called from `pump` in the draw loop (`src/main.rs:568-572`).
- `src/main.rs:708-712`: on `NotRunning`, `connect` recurses to the parent folder. One launcher
  process per ancestor, synchronously: a file six levels deep is six spawns before the next frame.
- `src/panel.rs:1131-1135`: `git rev-parse --show-toplevel` with `.output()`, from `tick`
  (`panel_watch_folder`) on every buffer switch while the panel is open. A cold or network
  filesystem stalls the frame.
- `src/bottomterm.rs:106-114`: `Drop for Tab` does `kill(pid, SIGHUP)` then `waitpid(pid, .., 0)`.
  A shell or an `exec`ed program that ignores SIGHUP makes `M-w` and the editor's own exit hang
  for ever. There is no WNOHANG, no SIGKILL after a grace.
- `src/letibot.rs:779-785`: `write_all` on a blocking `UnixStream` from the UI thread. A daemon
  that stops reading blocks the prompt that fills the socket buffer.
- Recorded already and unchanged: `M-|` (`src/exec_ctrl.rs:129`), `M-.`, `LspClient::shutdown`.
  One addition: `run_filter` (`src/exec_ctrl.rs:230-231`) does `out_rx.recv()` after the deadline
  kill, which waits for pipe EOF, so its own timeout does not bound it when a grandchild holds the
  pipe.

Fix: one helper, `run_with_deadline(cmd, timeout) -> Result<Output>`, that polls `try_wait`, drains
on threads and gives up on the pipes after a grace; use it for the launcher and git, or move both
off the UI thread and deliver the answer as a frame. `Tab::drop`: SIGHUP, poll WNOHANG for 200 ms,
SIGKILL, then reap on a detached thread.

### 8. With the bottom terminal open the mouse is dead in the editor

Severity: medium. Confidence: confirmed.

Location: `src/editor.rs:1641-1658`.

Evidence:

```rust
if self.bottom.open {
    return match m.kind {
        MouseKind::WheelUp => { ...t.scroll_up(WHEEL_LINES as usize)...; true }
        MouseKind::WheelDown => { ... true }
        _ => false,
    };
}
```

No position test. The comment above says "a touchpad over the panel"; the code is "anywhere".

Scenario: open the terminal, press Esc to return to the text. A click in the text no longer places
the cursor, a drag no longer selects, and the wheel over the document scrolls the shell's
scrollback. This holds until the strip is closed. Preview selection (`view_select`) and the
scrollbar jump are behind the same early return.

Fix: the host knows the strip's rectangle (`Layout::bottom`); route by position in `main.rs::mouse`
the way the panel is routed, and delete this block.

### 9. The emulator's parser: root causes of the recorded screen corruption

Severity: medium. Confidence: confirmed by tracing each sequence through the code. This extends
`2026-11-15-feature-completeness.md`, which records the symptoms.

Location: `src/bottomterm.rs:291-432`, `src/keys.rs:211`, `src/main.rs:743,806`.

- Every ESC is parsed as CSI: `escape` (`:350-362`) collects characters until the first ASCII
  letter and calls `csi`. `ESC ] 0 ; user@host: ~ BEL` (the title sequence in Debian's and
  Ubuntu's default `PS1`) ends at the `u` of `user`, and `ser@host: ~` is printed on every prompt.
  `ESC ( B` (part of `sgr0` in most terminfo entries, emitted constantly by fish and by `tput`)
  becomes `csi("(", 'B')`, which is cursor-down by one. `ESC =`, `ESC >`, `ESC 7`, `ESC 8` are not
  letters, so they swallow the text after them up to its first letter.
- Parser state is not kept across reads: `process` (`:291-315`) starts fresh on each 4096-byte
  chunk and `String::from_utf8_lossy` is applied per chunk. An escape or a multi-byte character
  split at a chunk edge is printed as text and U+FFFD.
- `sgr` (`:417-432`) reads each `;` part alone. `38;5;1` (256-colour red) is "ignore, ignore, bold".
  `38;5;0` resets all attributes.
- `'J'` (`:395-401`) handles only `2J`, and clears `cells` but not `fg`/`bold`. `ESC [ J`, which zsh
  and readline send after every prompt, does nothing, hence the stale text the record saw.
  `'K'` ignores its parameter.
- The screen is 12 rows but at most 10 are drawn: `Tab::open(.., self.text_w as u16, 12)`
  (`src/keys.rs:211`), the strip is `12.min(h / 3)` rows (`src/main.rs:743`), one is the rule and
  one the tab strip, and the draw takes `(area.height).saturating_sub(1)` lines
  (`src/main.rs:806`). Once output reaches the bottom the prompt is on rows 11-12, which are never
  drawn. At 80x24 six rows are visible. This is the cause of "the last rows and the prompt are
  under the frame's edge".
- The width is the editor column's (`text_w`) at open time, the strip is the frame's full width,
  and nothing calls `TIOCSWINSZ` afterwards.
- `Vt` assumes `cols >= 1` and `rows >= 1`: `self.cols - 1` (`:308`, `:378`, `:393`),
  `self.rows - 1` (`:377`, `:387`), `self.cells[self.y * self.cols + self.x]` (`:303`). A host pane
  of width 0 at `M-0` panics on the first backspace or tab.
- Scrollback: `history_lines` (`:485-492`) shows history only, so one step back hides the whole
  live screen and skips it.
- `Tab::send` (`:86-88`) drops the result of a non-blocking `write`: a long line is cut at the
  pty's buffer with no notice.

Fix: do not grow this by hand. Either gate the feature off until it is a real emulator (the
`vt100` crate the module doc already names), or set `TERM=dumb` so programs stop sending what it
cannot read, keep the unconsumed tail between reads, and size the pty from the strip's rectangle.

### 10. `write_atomic` changes more than the bytes

Severity: medium. Confidence: confirmed by reading.

Location: `src/editor.rs:3966-4003`.

Evidence: `fs::File::create(&tmp)` then `write_all`, `sync_all`, `set_permissions(&tmp, mode)`,
`fs::rename(&tmp, &target)`.

- Ownership: the new inode belongs to the saver. `sudo rano /home/u/.bashrc`, save: the file is now
  `root:root`. In a group-shared directory the group changes. Nothing copies uid/gid, ACLs, xattrs
  or the SELinux label, and no test looks (`a_save_keeps_the_files_mode` checks the mode only).
- Hard links: the other name keeps the old content after a save.
- The temp file is created with the umask's mode and gets the real mode only after the content is
  in it. Saving a 0600 file leaves its full text readable at 0644 for the duration of the write
  and fsync.
- The temp name is predictable (`.{name}.{pid}.rano-tmp`) and `File::create` follows symlinks and
  truncates. In a directory others can write, a planted symlink of that name receives the file's
  content, and the rename then installs the symlink as the file.
- The parent directory is not fsynced, so the rename itself is not durable.

Fix: create with `OpenOptions::new().write(true).create_new(true).mode(mode)`; after writing,
`fchown` to the original's uid/gid and fall back to the in-place write when that fails or when
`nlink > 1`; fsync the directory.

### 11. A shell that exits is never noticed

Severity: medium. Confidence: confirmed.

Location: `src/bottomterm.rs:20-21`, `:71-83`.

Evidence: the field doc says `/// The tab's label: the shell's name, and whether it has exited.`
The only write to `title` is `title: "sh".to_string()` at `:63`. `feed` treats `n <= 0` as "nothing
yet", for EAGAIN and for EIO alike, and nothing calls `waitpid` outside `Drop`.

Scenario: type `exit`. The tab stays, the pane shows the last screen, keys are written to a dead
fd and dropped, and the shell is a zombie until `M-w`. There is no "[exited]" and no auto-close.

Fix: in `feed`, on EIO or 0 do `waitpid(pid, WNOHANG)`, record the status, title the tab
`sh [exit N]`, and stop routing keys to it.

### 12. pty output and panel frames are polled at 5 Hz when idle

Severity: medium. Confidence: confirmed by reading the loop.

Location: `src/host.rs:11`, `src/host.rs:91-105`, `src/main.rs:429`.

Evidence: `const IDLE_WAIT: Duration = Duration::from_millis(200);` and
`if !terminal.poll(ed.next_wakeup()) { continue; }`. `poll` watches stdin only. `next_wakeup`
shortens the wait for a loading file and a prefix card, never for an open tab or a running turn.

Scenario: type a character in the bottom terminal. The key is written, the loop ticks at once
(the echo has not arrived yet), draws, and sleeps up to 200 ms. The character appears on the next
key or 200 ms later. A streaming reply in the panel and a running build in the terminal both
repaint five times a second.

Fix: add the tab masters and the letibot socket (or an eventfd the reader threads signal) to the
`poll` set; or return a short wakeup from `next_wakeup` while a tab is open or a turn is running.

### 13. A daemon that dies is silent in the panel

Severity: medium. Confidence: confirmed for the code; the visible effect is plausible.

Location: `src/letibot.rs:754-756`, `src/main.rs:556-563`.

Evidence: `self.frames.try_recv().ok()` folds `Empty` and `Disconnected` into `None`, and `pump` in
`main.rs` does `while let Some(frame) = open.client.try_frame()`.

Scenario: the daemon crashes mid-turn without a `bye`. The reader thread ends, `try_frame` answers
`None` for ever, the panel stays "thinking". The operator learns of it on the next prompt, as
"cannot reach the daemon: Broken pipe".

Fix: return `Result<Option<Frame>, Gone>`; on `Gone` drop the link and call `panel.no_daemon`.

### 14. Two surfaces can hold the keyboard at once, and two layouts drop the strip

Severity: medium. Confidence: confirmed.

Location: `src/keys.rs:115-126`, `src/bottomterm.rs:137-138`, `src/main.rs:913`,
`src/main.rs:757-769`.

Evidence: `BottomTerm::toggle` sets `self.focus = self.open` and leaves `panel.focus` alone; a click
in the panel sets `ed.panel.focus = true` and leaves `bottom.focus` alone. `handle_key` asks
`bottom_key` first. In `layout`, the zoomed and the narrow branch end in `..Layout::default()`, so
`bottom` is `None` although `whole` was already shortened by the strip.

Scenario A: terminal focused, click in the panel. The panel draws its caret and key row, and every
key still goes to the shell.
Scenario B: terminal open, then `M-i` on a terminal narrower than 97 columns, or `M-m`. The strip's
rows are reserved and never drawn, and the invisible terminal still takes the keys.
Scenario C (embedding): `M-0` is handled inside `Editor::handle_key`, but the strip is drawn only
by `main.rs::draw_frame`. A host that draws with `ui::draw_in` gets `KeyOutcome::Handled`, a forked
shell, and a keyboard that goes nowhere until Esc. `docs/embedding.md` does not mention it.
letibot pins an older rano today (recorded), so this arrives with the next bump.

This is a second instance of the recorded "eight independent Options" problem, in new code. Fix:
one `enum Focus { Text, Panel, Terminal }` owned by the host layer, and move `bottom_key` out of
`Editor::handle_key` next to `host_key`, where the panel's three keys already are.

### 15. Tests that pass with the feature broken

Severity: medium. Confidence: confirmed by reading the tests.

- `src/bottomterm.rs:524-540` `a_tab_answers_a_command` sends `echo hi\n` and looks for `hi` on the
  screen. The pty's line discipline echoes the typed line back before any shell reads it, so the
  assertion is met by the kernel. A shell that never starts a command passes. It also runs the
  developer's `$SHELL` with its rc files under a 400 ms budget. Use `echo $((6*7))` and look for
  `42`, and exec `/bin/sh` in tests.
- `src/host.rs:201-208` `m_zero_opens_the_bottom_terminal` asserts a `Tab` exists. It forks the
  multi-threaded test binary to do so.
- `src/panel.rs:1497-1512` asserts one step of the walk-up (finding 4) and hard-codes
  `/home/dead/Projects/rano/rano`.
- `tests/panel.rs:83-140`: the scripted daemon has exactly one session, answers an empty attach
  with it, and the test bypasses the launcher through `LETIBOT_SOCKET`. Zero sessions, several
  sessions, a launcher answer and a mid-turn disconnect have no end-to-end test.
- No test for: paste with a prompt open (finding 3), the mouse with the strip open (finding 8),
  a save that must keep the owner (finding 10), an escape split across two reads (finding 9).

### 16. The library has no feature flags, and most of `agent/` has no caller in rano

Severity: medium (structure). Confidence: confirmed by grep.

Evidence: `Cargo.toml` has no `[features]`. `src/agent/` is 13,725 lines in 30 modules; a search
for `agent::<module>` outside `src/agent/` finds callers for `panel`, `keys`, `blocks`, `text` and
`logpane` only. The other 25 (card, decision, tool_row, jobs, picker, header, subagents, ...) are
letibot's widgets living in the editor's crate. In the other direction, a host that wants the
editor compiles wasmtime with cranelift, 25 grammar crates plus the vendored one, the self-updater (`pub mod update`) and
the pty code.

Scenario: a change to a letibot card needs a rano release; a rano refactor of `render` has to keep
25 modules building that no rano test drives through the binary; an embedder cannot turn off
plugins or the terminal.

Fix: move `agent/` (minus the panel view) to its own crate over `rano::render`, or to letibot.
Put `plugin` (wasmtime), `update` and `bottomterm` behind cargo features that the binary enables.

### 17. Three windowing and four left-ellipsis implementations

Severity: low-medium. Confidence: confirmed.

Evidence: `agent::pane::window` (`src/agent/pane.rs:152`) is the documented windowing with one
seam. `agent::panel::View::draw` (`src/agent/panel.rs:86-128`) re-implements it inline and gets the
budget wrong: `shown = room - 1` pays for one seam, but both an "above" and a "below" seam can be
due, so when scrolled into the middle the body fills the room and `if below > 0 && row <
composer_row` is false: the "below" count is never shown. Left-cut exists as `left_cut`
(`src/agent/panel.rs:263`, counts chars, not columns), `agent::text::ellipsise_left`
(`src/agent/text.rs:181`), `width::text::ellipsise_left` (`src/width/text.rs:223`) and
`render::text::ellipsise_left` (`src/render/text.rs:330`); `truncate` exists three times. The
converter runner (`run`, `stop`, `rss`, the `pre_exec` rlimits) is copied between
`src/image.rs:337-475` and `src/pdf.rs:707-856`, about 140 lines differing in the result type.

Fix: the panel calls `pane::window`; `left_cut` goes; one `watched_child` module for image and pdf.

### 18. `terminal_scrollback` is a setting that does nothing

Severity: low. Confidence: confirmed by grep.

Evidence: `src/config.rs:27-29` documents `terminal_scrollback = N` and `:118` defaults it to 200.
No other line in `src/` names it: the config parser does not read the key and
`src/bottomterm.rs:336` has `if self.history.len() > 200`. Commit `07e188f` is titled
"terminal_scrollback in the config".

Fix: parse it and pass it to `Vt::new`, or delete the field.

### 19. Panel rows and the indexes into them can disagree

Severity: low. Confidence: confirmed for `follow`; plausible for a session switch.

Location: `src/panel.rs:271-272`, `:384`, `:608-630`.

Evidence: `running: Vec<(String, usize, String)>` holds a row index. `follow()` does
`self.rows.clear()` and leaves `running`. `ToolFinished` then does `self.rows.get_mut(row)`.

Scenario: a tool call is running, the operator presses `alt-f` to follow a moved folder, the old
daemon's `ToolFinished` or a call id reused by the new one rewrites whichever note now sits at that
index. A `Hello` for a different session does not clear `rows` either, so a pick of another session
appends to the previous conversation. Also `Panel::build` (`:941-981`) calls
`reply.view.lines()` for every reply on every delta, which clones each reply's stable prefix
(`src/markdown/view.rs:114-115`), while the `Reply` doc (`:181-182`) says a frame "costs the tail".

Fix: clear `running` with `rows`; key running calls by id and find the row by id; reset rows on a
`Hello` whose session differs.

### 20. `^T` output lands at a stale row with the cursor out of range

Severity: low. Confidence: confirmed for the assignment; the consequence is confirmed in
`Buffer::insert_char`.

Location: `src/exec_ctrl.rs:26`, `:53-59`.

Evidence: `exec::spawn_job(cmd, self.bs().cursor.row + 1)` fixes the row at spawn;
`insert_lines_at` clamps it (`src/buffer.rs:401`) but `self.bs_mut().cursor = Pos { row, col: 0 }`
does not.

Scenario: `^T sleep 3; date` on row 90, then cut most of the buffer. The output is inserted at the
end, the cursor is set to row 91 of a 2-row buffer, and typed characters are silently dropped
(`Buffer::insert_char` ignores a missing row) until a motion clamps the cursor. Edits above the
spawn row also shift where the output lands.

Fix: clamp through `buf.clamp`, and anchor the job to a mark that edits move.

### 21. The panic hook is process-wide

Severity: low. Confidence: confirmed by reading.

Location: `src/term/terminal.rs:219-224`.

Scenario: a panic on a loader, LSP-reader or letibot-reader thread runs the hook: the terminal
leaves raw mode and the alternate screen while the UI thread is alive and keeps drawing. The
editor continues on a cooked terminal with its frames in the scrollback.

Fix: restore only when `std::thread::current().id()` is the UI thread's, or set a flag the loop
turns into an orderly exit.

### 22. `write_hunk` slices with a host-supplied column

Severity: low. Confidence: confirmed for the expression; reachable only through the host API
(`Mark::col` is a public field no in-crate code sets).

Location: `src/marks.rs:479-480`.

Evidence: `let tail: String = row[c1.min(row.len()).max(c0)..].iter().collect();`. With
`c0 > row.len()` the start is `c0` and the slice panics. `anchored` (`:239-254`) compares rows,
not columns, and trusts a mark with an empty anchor.

Fix: `let c0 = c0.min(row.len()); let c1 = c1.clamp(c0, row.len());` first.

### 23. Errors the user never sees

Severity: low. Confidence: confirmed.

- `src/keys.rs:211`, `:224`: `let _ = self.bottom.toggle(..)` and `let _ = self.bottom.new_tab(..)`.
  `toggle` sets `open = true` before `Tab::open` can fail, so EMFILE or a failed fork shows the
  empty-state line and no reason.
- `src/panel.rs:679`: `let _ = remember_in(..)`; the session memory is a read-modify-write of a
  shared file with a plain `fs::write` (`:1169`), so two rano processes lose each other's line and a
  full disk truncates it.
- `src/main.rs:421`: `let _ = open.client.ack(..)`.
- `src/bottomterm.rs:87`: finding 9, dropped writes.
- 176 `let _ =` in `src/` outside the tests, the same count the 0.10 review gave; the LSP ones are
  recorded.

### 24. `TODO.md` is stale in places, and silent on the new surfaces

Severity: low. Confidence: confirmed.

- "nothing in `tests/` spawns `CARGO_BIN_EXE_rano`": `tests/panel.rs:199` does.
- `src/lib.rs:177` and `src/lib.rs:182`: `lib.rs` has 119 lines.
- Line references have drifted: `UNDO_LIMIT` is at `src/editor.rs:995` (TODO: 936), `do_goto` at
  `:2596` (TODO: 2861), the LSP stderr null at `src/lsp.rs:540` (TODO: 568), the tail window at
  `src/loader.rs:234` still right. A reader following a reference lands in unrelated code.
- `M-|` blocking is listed twice (the 0.10 section and "Crashes and data loss").
- Nothing in it names the bottom terminal, the panel, signals or paste routing. The two records of
  today have no TODO entries yet.

### 25. Comments that state what the code does not do

Severity: low. Confidence: confirmed. Each of these would mislead a reviewer who trusted it.

- `src/bottomterm.rs:1-5` and `src/editor.rs:215`: "each tab is a pty and a `vt100` parser",
  "a `vt100::Parser`". There is no such dependency; the parser is the in-file `Vt`.
- `src/bottomterm.rs:20`: the title says "whether it has exited" (finding 11).
- `src/bottomterm.rs:253`: "in-tree because this box's registry is read-only" - a fact about one
  machine on one day, in the source.
- `src/keys.rs:113`: the bottom terminal is toggled by "ctrl-`"; twelve lines of the same file
  later (`:206`) say that chord cannot exist and the toggle is `M-0`.
- `src/editor.rs:1639`: "a touchpad over the panel" (finding 8).
- `src/editor.rs:2188`: "the newlines become spaces" (finding 3).
- `src/host.rs:19`: "Never blocks" (findings 1 and 7).
- `src/panel.rs:181-182`: "a frame costs the tail" (finding 19).
- `src/config.rs:27-28`: finding 18.
- `Cargo.toml:12-14`: libc is "the only C the terminal layer needs" and for `rano::term`; it now
  also carries `fork`, `openpty`, `waitpid`, `setrlimit`.
- History in comments, against the repo's own rule: `src/keys.rs:207` ("binding M-y broke the
  describe-key test"), `src/host.rs:35` ("is what made it look hung"), `src/main.rs:694`,
  `tests/panel.rs:101-103`.

### 26. Build and vendor notes

Severity: low. Confidence: confirmed.

- `vendor/tree-sitter-dockerfile/README.md` says MIT, © 2020 Camden Cheek, but the licence text
  itself is not in the folder; MIT asks for the notice to travel with copies.
- `build.rs:21-25` has an MSVC branch, while `src/bottomterm.rs` uses `libc::fork`/`openpty` with no
  `cfg(unix)` and `src/letibot.rs` uses `UnixStream`. The crate is unix-only; the branch is dead and
  suggests otherwise.
- `.github/workflows/release.yml` still has no `cargo test` step (recorded, still open).
- `Cargo.toml` still has no `rust-version` (recorded) with `edition = "2024"` and let-chains in use.

## What is good and should be kept

- The save door: `refuse_partial_save` is checked in `save_to` and again in `write_file`, the
  failure path removes the temp file, and there are tests for mode, symlink, read-only directory
  and a failed write.
- `exec.rs` after the fix: reader threads, `try_recv`, a grace for held pipes, and a comment that is
  true.
- Defensive arithmetic in the core: `begin_action`, `undo`/`redo`, `visual_pos`,
  `buf_row_of_visual`, `insert_lines_at`, `Buffer::clamp`, `write_hunk`'s row range all clamp
  instead of trusting the caller. The panel widget's row budget (`src/agent/panel.rs:76-81`) holds
  for every height I worked through.
- The letibot wire reader: tolerant `parse`, unknown frames counted not fatal, lossy UTF-8 per
  line, a reader thread and a `Drop` that shuts the socket down before joining.
- The image/pdf converter runner: no shell, null stdio, rlimits in `pre_exec`, a sampler, a
  deadline, and a `stop` that never waits on a child it could not signal. This is the shape the
  launcher, git and the pty need.
- The panel as acts and frames (`Panel` holds no socket, the host performs `Act`s): it makes the
  panel testable without a daemon. The gap is in the fixtures, not the design.
- `tests/panel.rs` drives the real binary on a pty and enforces the daemon's first-frame rule in
  the fixture after it was found to be too lenient.
- The review records with verbatim positions and decisions. They made it possible to check what
  landed.

## Not covered

- Read in full: `src/bottomterm.rs`, `src/host.rs`, `src/main.rs` (to line 960), `src/exec.rs`,
  `src/exec_ctrl.rs`, `src/agent/panel.rs` (to line 300), `src/lib.rs`, `build.rs`, `Cargo.toml`,
  `TODO.md`, the three review records (the keys audit to finding 5).
- Read in part: `src/panel.rs` (317-1392 and two tests), `src/letibot.rs` (client, pump, launcher),
  `src/term/terminal.rs` (120-650), `src/editor.rs` (save path, undo, mouse, paste, a few helpers:
  perhaps 600 of 4,336 lines), `src/keys.rs` (100-310), `src/marks.rs` (three functions),
  `src/image.rs` (the runner), `tests/panel.rs` (to line 215).
- Not read: `src/syntax.rs`, `src/markdown/`, `src/ui.rs`, `src/commands.rs`, `src/prompt.rs`
  beyond one arm, `src/lsp.rs` and `src/lsp_ctrl.rs` beyond `shutdown` and the lines TODO names,
  `src/plugin.rs`, `src/loader.rs`, `src/load_ctrl.rs`, `src/rows.rs`, `src/buffer.rs` beyond six
  functions, `src/logsearch.rs`, `src/logtail.rs`, `src/pdf.rs` beyond the diff against image.rs,
  `src/term/decode.rs`, `proto.rs` (two functions only), `features.rs`, `links.rs`, `picture.rs`,
  `graphics.rs`, `src/render/`, `src/width/`, `src/update.rs`, `src/config.rs`, `src/theme.rs`,
  `src/todo*.rs`, `src/diff*.rs`, `src/conflict.rs`, 28 of the 30 `src/agent/` modules,
  `src/ed_tests.rs` and `src/bench.rs` beyond greps, the other `tests/` files, `sdks/`,
  `examples/`, `scripts/`, `install.sh`, the CI workflows beyond a grep.
- So this review says nothing new about panics in the syntax, markdown, render or input-decoding
  code, about the plugin sandbox, or about the LSP client, and its claims about the test suite are
  limited to the tests named.
- Nothing was executed. Findings 1, 5, 7 and 13 describe behaviour under timing or a peer I did not
  drive; the code is as quoted, the frequency is not measured. Finding 4 assumes the daemon answers
  a bare attach with an empty session list when it holds none, as `src/main.rs:691-693` describes.

## Decisions

Proposed by cc@rano, not yet taken by the operator except where stated.

1. letibot does not move its rano pin past `cafb702` until findings 1, 3, 5, 6, 7 (the
   `waitpid` part), 11 and 14 are fixed.
2. Fix order for the rano session, after the key leak it is already on: 1 (bounded drain),
   3 (paste routing and the `expect`), 8 (mouse by position), 14 (one focus enum), 6 (cloexec),
   5 (fork hygiene, `TERM`), 11 (shell exit), 9 (parser: use the `vt100` crate or `TERM=dumb`),
   4 (panel walk-up with a zero-session fixture), 2 (signals and `.save`), 7, 13, 10, 18.
3. Open for the operator: finding 9, a real emulator crate against a hand-written one. Finding 16,
   moving `agent/` out of the crate and putting `plugin`, `update` and `bottomterm` behind cargo
   features. Finding 2, whether rano writes `name.save` on SIGHUP as nano does.
4. Findings 15, 17, 19 to 26 go to `TODO.md` with a link to this record.

### What was done (2026-11-15, the rano session)

The findings above are untouched. Tonight's work was the keys and frame records, not this
one's findings; the honest state of the four decisions:

1. The pin question is the operator's; nothing here moved it.
2. Of the fix order, finding 14 (one focus enum) is the code's existing shape and the C-]
   cycle is now pinned by a test in each layout. The rest of the order — 1, 3, 8, 6, 5, 11,
   9, 4, 2, 7, 13, 10, 18 — is not tonight's work and is not claimed here. The pty-test
   flake class the gate kept hitting (prompts the operator's bashrc rewrites, echo-
   satisfiable assertions, iteration counts instead of deadlines) was fixed where it bit:
   `ae3acb0` and the marker discipline the tests now use.
3. Still open and parked with the operator: finding 2's `.save` on SIGHUP/SIGTERM, and
   finding 16's `agent/` out of the crate behind features, and the atomic-write ownership.
4. Not done.

