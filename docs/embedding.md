# Embedding

The editor is a library (`rano::editor`), so a program can show it as a pane. The `rano` binary
is one such host.

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
| `set_chrome(Chrome)` | Show or hide the title bar, the key bar and the status row. `Chrome::BARE` is neither bar, and the status row only while it has a prompt or a message (`Chrome.status`: `Always` or `WhenBusy`) |
| `open_at(path, line, column)` | Open or switch to a file, cursor on that 1-based line and column, centred. A file with unsaved edits gets its own buffer, so a jump never discards work |
| `handle_key(KeyEvent)` | One key. `KeyOutcome` is `Handled`, `Unhandled`, or `Host(name)` for a key a host keymap bound to a command the editor does not have |
| `handle_mouse(MouseEvent)`, `paste_text(&str)` | Input that is not a key |
| `push_keymap(Keymap)`, `pop_keymap()` | Stack a host's keymap over the editor's. Its bindings win key by key, in every mode. A binding to an editor command's name runs that command, which is how a host rebinds |
| `tick(Instant)` | Everything between events: loaders, language servers, jobs, diagnostics, scroll, highlight. Returns whether the screen changed. Never blocks |
| `next_wakeup()` | How long the host may wait for input before ticking again |
| `wants_quit()` | The editor was asked to exit; tearing down is the host's |
| `ui::draw_in(&mut Buffer, Rect, &Editor)` | Draw the editor into a pane; returns where the cursor goes |
| `set_read_only(bool)`, `open_dir(&Path)` | A view that cannot be edited, and a folder listing |
| `frame_title()`, `position()` | What the pane's status row says: the file and whether it is modified, and the cursor's 1-based column with the file's line when the buffer can state one |
| `chrome_rows()`, `status_shown()` | The rows the chrome takes this frame, and whether the status row is among them — what a host laying out around a `Chrome::BARE` pane measures |

## Sending the reader's place

`Editor::on_send` takes a callback over `rano::send::SendEvent`: the file's absolute path, the
cursor, the selection (start, end and text) when a mark is set, and whether the buffer has
unsaved edits. Lines and columns are 1-based and count characters. `SendEvent::to_json` is one
JSON object:

```json
{"path":"/src/main.rs","cursor":{"line":120,"column":5},"selection":{"start":{"line":118,"column":1},"end":{"line":120,"column":5},"text":"..."},"modified":false}
```

The binary's callback runs `send_command` with `sh -c`, the event on stdin and `RANO_FILE`,
`RANO_LINE` and `RANO_COLUMN` set. It does not wait, and output is discarded.

## Asking about a selection

`Editor::on_ask` takes a callback over `rano::ask::AskEvent`: the `send` a host already knows,
the preset's `id`, its `answer` mode (`Chat`, `Notes` or `Suggest`) and the `prompt` to send.

```rust
ed.on_ask = Some(Box::new(move |ask| {
    // `ask.send` is the selection, or the paragraph or line the cursor is on; the
    // head sends `ask.prompt` with that place as the prompt's context.
    Ok(format!("asked {} about {}", ask.preset, ask.send.cursor.line))
}));
```

It is the whole of `M-k`: the prefix, its entries and the target rule are rano's, and the
answer comes back by whatever path the host already has (a conversation reply, marks on the
buffer). `M-k k` is the same event with an empty `prompt`, where the host's composer takes the
question. A host without `on_ask` still hears an ask through `on_send`, with its prompt as
`SendEvent::ask`.

`Editor::bind_ask()` rebuilds the `M-k` keys from `config.ask`, so a host that replaces the
presets at runtime calls it after the change.

## Reviewing a change

`Editor::open_review(path, Change { before, after, before_start, after_start })` opens the file
on the first changed line and draws the change over the text as a diff view. `Esc` goes to the
text, `M-p` shows the change again, and `M-s` sends the reader's place from either.

The change is the host's snapshot of the edit, not a re-diff against the buffer.

## Marks

`rano::marks` puts line-addressed things beside the text and lets the editor decide them,
because only the editor holds the buffer a decision has to land in.

```rust
use rano::marks::{Mark, Severity};

// A reply cited these lines, and suggests what they should say instead.
ed.set_marks(
    Path::new("/src/article.md"),
    "reply:item-42",
    vec![Mark::suggest("m1", 41, 47, anchor_lines, "the replacement, as many lines as it takes")],
);
ed.set_marks(path, "reply:item-42", vec![Mark::note("n1", 3, 3, anchor, Severity::Warn, "no source")]);
ed.clear_marks(Some("reply:item-42")); // or None for every mark
for (id, outcome) in ed.take_mark_outcomes() {
    // Accepted | Rejected | Reverted, once each, for the next prompt
}
```

A mark is 1-based inclusive lines, an optional char range on one line, a kind (`cite`,
`note`, `suggest`, `edit`), a severity, the one line of text the status row shows at it, and
the lines it expects to find — its anchor. `M-d` walks marks with the diagnostics and draws a
hunk (a `suggest` or an `edit`) as a review; `a` takes it, `r` drops it, `A` and `R` do that
for every hunk in the file, and the palette has the wider scopes — one reply's hunks, or every
file's. A suggestion taken replaces the lines as one undo step, unsaved; an edit reverted puts
its `before` back the same way.

Marks ride the diagnostics, so the gutter signs every line of a range without any drawing
code of its own. They are not stored: closing the buffer drops them, and a host sets them
again from what a snapshot replays. An edit inside a marked range drops a `cite` or a `note`,
and makes a `suggest` or an `edit` stale — drawn as a hint, saying so on the status row, and
refused by `a`: the anchor, not a guess about line shifts, is what decides. `marks()` and
`mark_stale(id)` answer the same two questions from outside, for a host that draws its own.

## One process, two themes

`rano::theme::set_active` is the process's theme. A host that draws more than one editor in one
process lends each pane its own by wrapping that pane's draw — and its emit — in
`rano::theme::scoped`, which puts everything back afterwards, even if the draw panics:

```rust
let rows = rano::theme::scoped(Some(gray), || {
    let cursor = ui::draw_in(&mut frame, pane, &ed);
    frame.emit(rano::style::Palette::Colour)
});
```

`None` is no override, so a pane inherits whatever is active. The scope is the thread's, so both
halves of one pane's frame must be drawn on it.

## Pieces without the editor

 `rano::buffer` (the text model), `rano::syntax`
(language detection and highlighting, with `Highlighter::classes` for a caller that owns its
palette), `rano::diff` / `rano::sidediff` / `rano::patch` (line diffs and patch files as
`render` lines), `rano::render` (role-styled spans and lines, a cell buffer, widgets),
`rano::width` (display width and wrap segments), `rano::theme` (roles and their overrides),
`rano::term` (raw mode, painter, input decoding, kitty graphics, OSC 8), `rano::todo` (the
`TODO.md` model), and `rano::logtail` / `rano::logsearch` (line offsets and search over a file
without loading it).
