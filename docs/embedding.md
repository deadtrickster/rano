# Embedding

The editor is a library (`rano::editor`), so another program can show it as a pane rather than
run it as a second process. The `rano` binary is one such host.

```rust
use std::path::Path;
use std::time::Instant;
use rano::buffer::Buffer;
use rano::editor::{Area, Editor, KeyOutcome};
use rano::term::KeyEvent;
use rano::{config, keymap::Keymap, ui};

let mut ed = Editor::new(Buffer::new(), config::load());
let mut host = Keymap::new("host");
host.bind("C-q", "close-pane"); // not an editor command: handed back
ed.push_keymap(host);
ed.open_at(Path::new("src/main.rs"), 120, Some(5)).ok();
ed.set_area(Area::new(0, 1, 80, 30));

let mut frame = rano::render::Buffer::empty(rano::render::Rect::new(0, 0, 80, 31));
// Each iteration of the host's loop:
if ed.tick(Instant::now()) {
    for bytes in ed.take_graphics() {
        // write them to the terminal, when `ed.images` says it speaks kitty
    }
    if let Some(text) = ed.take_clipboard() {
        // terminal.copy(&text)
    }
    let _cursor = ui::draw_in(&mut frame, pane, &ed);
}
let wait = ed.next_wakeup();
match ed.handle_key(KeyEvent::ctrl('q')) {
    KeyOutcome::Host(cmd) if cmd == "close-pane" => { /* close the pane */ }
    KeyOutcome::Unhandled => { /* try the host's own bindings */ }
    _ => {}
}
if ed.wants_quit() { /* ^X: close the pane */ }
```

## The calls

| Call | What it does |
|---|---|
| `Editor::new(Buffer::new(), config::load())` | Build an editor with its configuration |
| `set_area(Area)` | Give the editor its pane; returns whether the area changed |
| `set_chrome(Chrome)` | Show or hide the title bar and the key bar (`Chrome::BARE` for neither) |
| `open_at(path, line, column)` | Open or switch to a file, cursor on that 1-based line and column, centred. A file with unsaved edits gets its own buffer, so a jump never discards work |
| `handle_key(KeyEvent)` | One key. `KeyOutcome` is `Handled`, `Unhandled`, or `Host(name)` for a key a host keymap bound to a command the editor does not have |
| `handle_mouse(MouseEvent)`, `paste_text(&str)` | Input that is not a key |
| `push_keymap(Keymap)`, `pop_keymap()` | Stack a host's keymap over the editor's. Its bindings win key by key, in every mode. A binding to an editor command's name runs that command, which is how a host rebinds |
| `tick(Instant)` | Everything between events: loaders, language servers, jobs, diagnostics, scroll, highlight. Returns whether the screen changed. Never blocks |
| `next_wakeup()` | How long the host may wait for input before ticking again |
| `wants_quit()` | The editor was asked to exit; tearing down is the host's |
| `ui::draw_in(&mut Buffer, Rect, &Editor)` | Draw the editor into a pane; returns where the cursor goes |
| `set_read_only(bool)`, `open_dir(&Path)` | A view that cannot be edited, and a folder listing |

## Sending the reader's place

`Editor::on_send` takes a callback over `rano::send::SendEvent`: the file's absolute path, the
cursor, the selection (start, end and text) when a mark is set, and whether the buffer has
unsaved edits. Lines and columns are 1-based and count characters. `SendEvent::to_json` is one
JSON object:

```json
{"path":"/src/main.rs","cursor":{"line":120,"column":5},"selection":{"start":{"line":118,"column":1},"end":{"line":120,"column":5},"text":"..."},"modified":false}
```

The binary's callback runs `send_command` with `sh -c`, the event on stdin and `RANO_FILE`,
`RANO_LINE` and `RANO_COLUMN` in the environment. It does not wait for the command, and the
output is discarded, because the terminal belongs to the editor.

## Reviewing a change

`Editor::open_review(path, Change { before, after, before_start, after_start })` opens the file
on the first changed line and draws the change over the text as a diff view. `Esc` goes to the
text, `M-p` shows the change again, and `M-s` sends the reader's place from either.

The change is the host's snapshot of the edit when it was made, not a re-diff against the
buffer.

## Pieces without the editor

The pieces a host may want on their own: `rano::buffer` (the text model), `rano::syntax`
(language detection and highlighting, with `Highlighter::classes` for a caller that owns its
palette), `rano::diff` / `rano::sidediff` / `rano::patch` (line diffs and patch files as
`render` lines), `rano::render` (role-styled spans and lines, a cell buffer, widgets),
`rano::width` (display width and wrap segments), `rano::theme` (roles and their overrides),
`rano::term` (raw mode, the row-diffing painter, input decoding, kitty graphics, OSC 8),
`rano::todo` (the `TODO.md` model, without the editor), and `rano::logtail` / `rano::logsearch`
(a file's line count and byte offsets with no rows kept, and a search over a file's bytes).
