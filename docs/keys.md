# Keys

`C-x` is Ctrl+x, `M-x` is Alt+x, `F8` is a function key. `key_notation = nano` in the config
prints them nano's way (`^X`, `M-X`). The keys do not change.

## Finding a key

- `M-x` runs a command by name. A few letters in any order match (`wri out` finds Write Out).
  Recent commands come first, each with its key.
- `C-g` (or `F1`) is the help prefix: `?` for the help page, `k` then a key to see what it
  runs, `b` for every key in effect, `m` for rano's own manual, `x` for `M-x`.
- A prefix key (`M-t`, `C-g`) shows a card of what can follow. `Esc` abandons it.
- The bottom bar shows the keys of the current mode.

## Text

| Key | What it does |
|---|---|
| `C-o` / `F2` | Save the buffer, asking for the file name |
| `C-r` / `F5` | Insert a file's contents at the cursor |
| `F8` | Open a file. `name:line[:col]` jumps there. Replaces the current buffer unless it has unsaved edits or `multibuffer` is on |
| `F7` | Copy the file on disk to `file~` |
| `M-g` | Open a folder as a listing |
| `C-x` | Quit, offering each modified buffer for saving in turn |
| `RET` | Break the line, keeping its indent |
| `TAB` | Insert the indent unit (see [configuration](configuration.md)) |
| `DEL` / `C-h` | Delete the character before the cursor |
| `Delete` | Delete the character under the cursor |
| `C-d` | Delete the character under the cursor into the cutbuffer |
| `M-u` | Undo. Runs of the same action coalesce into one step |
| `M-e` | Redo |
| `C-j` / `F10` | Re-wrap the paragraph to the window width |
| `F9` | Sort the marked lines, or the whole buffer |
| `C-t` / `F6` | Run a shell command. Its stdout is inserted below the cursor when it finishes |
| `M-\|` | Filter the marked lines through a shell command |

## Mark and clipboard

| Key | What it does |
|---|---|
| `M-a` / `C-a` | Set or clear the mark. The region runs from it to the cursor |
| `Esc` | Clear the mark, or cancel a prompt |
| `C-k` | Cut the line, or the marked region. Repeat to cut more lines |
| `M-6` | Copy the line, or the marked region |
| `C-u` | Paste the cutbuffer at the cursor |

`C-k` and `M-6` reach the system clipboard as well as rano's own cutbuffer.

## Search

| Key | What it does |
|---|---|
| `C-f` / `F3` | Where is |
| `C-b` | Where was |
| `M-f` | Jump to the next match of the last search |
| `M-b` | Jump to the previous match |
| `C-\` / `F4` | Replace. `y` / `n` / `a` / `q` per match, `a` replaces from the cursor on |

In the search prompt `M-C` toggles case sensitivity and `M-R` toggles regex.

## Moving

| Key | What it does |
|---|---|
| arrows | Left, right, up, down |
| `C-p` / `C-n` | Up / down a line |
| `Home` / `End` | Start / end of the line |
| `C-e` | End of the line |
| `PgUp` / `PgDn` | Up / down a page |
| `C-Left` / `C-Right`, `M-Left` / `M-Right` | Previous / next word |
| `C-/` / `F11` | Go to a line, and a column |
| `M-]` | Jump to the bracket matching the one at the cursor |
| `C-c` | Say where the cursor is |

## Code

| Key | What it does |
|---|---|
| `M-.` | Jump to the definition of the symbol at the cursor (LSP) |
| `M-?` | List every usage of the symbol at the cursor (LSP). Enter jumps to one |
| `M-,` | Jump back, one press per jump, from a definition or a usage |
| `M-d` | Jump to the next diagnostic, wrapping |

`M-x set-mode` sets the current buffer's language. `plain` stops the guessing, `auto` puts it
back.

## Buffers

| Key | What it does |
|---|---|
| `M-<` / `M->` | Previous / next buffer |
| `M-l` | Buffer list |
| `M-w` | Close the current buffer, asking to save a modified one |

`multibuffer = true`, or several files on the command line, gives each file its own buffer,
cursor, undo history and language server. The last buffer cannot be closed. `C-x` exits.

## Todo lists and headings

| Key | What it does |
|---|---|
| `M-t t` | Tick the task on this line (`[ ]` and `[x]`) |
| `M-t c` | Tick every task in this section, or in this task's subtree. Press again to clear |
| `M-t x` | Decline the task on this line (`[-]`), or un-decline it |
| `M-t ]` / `M-}` | Next heading |
| `M-t [` / `M-{` | Previous heading |

The todo keys work on any task line in any markdown file. Heading motion wraps at the ends.
A file named `TODO.md` follows the [todo-md](https://github.com/todo-md/todo-md) standard:
`- [ ]`, `- [-]` (declined, not in progress), `- [x]`. There a parent follows its children,
and an unknown marker such as `- [/]` is an error in the gutter. A tick rewrites only the
marker.

## View

| Key | What it does |
|---|---|
| `M-p` | Preview a diff or patch, a markdown file as prose with its pictures, a picture as a picture, or a file's merge conflicts |
| `M-n` | Toggle the line-number gutter |
| `M-\` | Toggle soft line wrap |

## Host and updates

| Key | What it does |
|---|---|
| `M-s` | Send the file, cursor and selection to `send_command` (see [embedding](embedding.md)) |
| `M-v` | Install the newer release the startup check found |

## In a view

Previews, diffs, pictures, reviews and the conflict view replace the text's keys. `F8`,
`M-w`, `M-<` / `M->` and `M-x` still work.

| Key | What it does |
|---|---|
| `s` / `TAB` | Switch between two panels and one. The choice sticks for the session |
| arrows, `C-p` / `C-n`, `PgUp` / `PgDn`, `Space`, `Home` / `End` | Scroll |
| `Esc`, `q`, `RET`, `C-g`, `C-c`, `M-p` | Back to the text |

A markdown view and a picture are one column, so `s` says there is nothing to split.

## Merge conflicts

`M-p` on a file holding `<<<<<<<` / `=======` / `>>>>>>>` shows one conflict per section, with
the surrounding lines for context.

| Key | What it does |
|---|---|
| `n` / `p`, `]` / `[` | Next / previous conflict |
| `o` | Take ours |
| `t` | Take theirs |
| `b` / `B` | Take both, ours first / theirs first |
| `c` | Cycle ours against theirs, base against ours, base against theirs |

Each resolution is one undo step. `c` needs the base: `git config merge.conflictStyle diff3`.

## A file changed on disk

Saving over a file changed on disk since rano read it asks first.

| Key | What it does |
|---|---|
| `y` | Overwrite the file |
| `n` / `C-g` | Do not save |
| `d` | Show what the save would change: disk against buffer, unified or in two panels |

In that diff `y` / `n` answer, `s` switches panels and `Esc` returns to the question. `n`
also calls off the `C-x` or `M-w` the save was part of.

## Folder listing

`M-g`, `F8` on a folder, or `rano some/folder/` opens a listing, directories first with `/`.

| Key | What it does |
|---|---|
| `RET` | Enter the entry under the cursor: a folder replaces the listing, a file opens |
| `-` | The folder above |
| `r` | Read the folder again, keeping the cursor where it was |
| `q` / `Esc` / `C-g` | Close the listing |

The listing is read-only. Motion keys work as over text.

## Lists

The buffer list and the usage list.

| Key | What it does |
|---|---|
| `RET` | Go to the selected item |
| arrows, `C-p` / `C-n`, `PgUp` / `PgDn`, `Home` / `End` | Move |
| `Delete` | In the buffer list: close the selected buffer |
| `Esc`, `C-g`, `C-c`, `C-x`, `M-l`, `M-?` | Close the list |

## A help page

| Key | What it does |
|---|---|
| `q`, `RET`, `Esc`, `C-g` | Close |
| arrows, `C-p` / `C-n` | Scroll a line |
| `PgUp` / `PgDn`, `Space` | Previous / next page |
| `Home` / `End` | Top / bottom |

## M-x

| Key | What it does |
|---|---|
| `RET`, `TAB` | Run the selected command |
| arrows, `C-p` / `C-n` | Pick |
| `DEL`, `C-h` | Backspace |
| `Esc`, `C-g`, `M-x` | Cancel |

## Prompts and mouse

Prompts keep separate histories for search, exec and file names: `Up` / `Down` cycle them,
`Tab` completes file names, `~` expands to `$HOME`, `M-b` / `M-f` move by word. `Esc` or `C-g`
cancels a prompt.

Bracketed paste arrives as a single undoable edit.

Mouse: a click moves the cursor, a click on the cursor toggles the mark, a drag selects and
copies to the system clipboard (OSC 52), the wheel scrolls.
