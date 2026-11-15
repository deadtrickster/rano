# Keys

`C-x` is Ctrl+x, `M-x` is Alt+x, `<f8>` is a function key. `key_notation = nano` in the config
prints them nano's way (^X, M-X). The keys do not change.

## Finding a key

- `M-x` runs a command by name. A few letters in any order match (`wri out` finds Write Out).
  Recent commands come first, each with its key.
- `C-g` is the help prefix, with the sequences below; `<f1>` opens the help page directly.
- A prefix key (`M-t`, `M-k`, `C-g`) shows a card of what can follow. `ESC` abandons it.
- The bottom bar shows the keys of the current mode.

| Key | What it does |
|---|---|
| `<f1>` | The help page |
| `C-g ?` | The help page |
| `C-g C-g` | The help page, again |
| `C-g k` | What a key runs: the chord is typed next |
| `C-g b` | Every key in effect |
| `C-g m` | rano's own manual |
| `C-g x` | `M-x` |

## Text

| Key | What it does |
|---|---|
| `C-o` / `<f2>` | Save the buffer, asking for the file name |
| `C-r` / `<f5>` | Insert a file's contents at the cursor |
| `M-o` / `<f8>` | Open a file. `name:line[:col]` jumps there. Replaces the current buffer unless it has unsaved edits or `multibuffer` is on |
| `M-c` / `<f7>` | Copy the file on disk to `file~` |
| `M-g` | Ask for a folder, then open it as a listing |
| `C-x` | Quit, offering each modified buffer for saving in turn |
| `RET` | Break the line, keeping its indent |
| `TAB` | Insert the indent unit (see [configuration](configuration.md)) |
| `DEL` / `C-h` | Delete the character before the cursor |
| `<delete>` | Delete the character under the cursor |
| `C-d` | Delete the character under the cursor into the cutbuffer |
| `M-u` / `C-z` | Undo. Runs of the same action coalesce into one step |
| `M-e` / `M-z` | Redo |
| `C-j` / `<f10>` | Re-wrap the paragraph to the window width |
| `M-r` / `<f9>` | Sort the marked lines, or the whole buffer |
| `C-t` / `<f6>` | Run a shell command. Its stdout is inserted below the cursor when it finishes |
| `M-|` | Filter the marked lines through a shell command |

## Mark and clipboard

| Key | What it does |
|---|---|
| `M-a` / `C-a` | Set or clear the mark. The region runs from it to the cursor |
| `ESC` | Clear the mark, or cancel a prompt |
| `C-k` | Cut the line, or the marked region. Repeat to cut more lines |
| `M-6` | Copy the line, or the marked region |
| `C-u` / `C-y` | Paste the cutbuffer at the cursor |

`C-k` and `M-6` reach the system clipboard as well as rano's own cutbuffer.

## Search

| Key | What it does |
|---|---|
| `C-f` / `<f3>` | Where is |
| `C-b` | Where was |
| `M-f` | Jump to the next match of the last search |
| `M-b` | Jump to the previous match |
| `C-\` / `<f4>` | Replace. `y` / `n` / `a` / `q` per match, `a` replaces from the cursor on |

## Moving

| Key | What it does |
|---|---|
| `<left>` / `<right>` / `<up>` / `<down>` | Left, right, up, down |
| `C-p` / `C-n` | Up / down a line |
| `<home>` / `<end>` | Start / end of the line |
| `C-e` | End of the line |
| `<pgup>` / `<pgdn>` | Up / down a page |
| `C-<left>` / `C-<right>`, `M-<left>` / `M-<right>` | Previous / next word |
| `C-/` / `<f11>` | Go to a line, and a column |
| `M-]` | Jump to the bracket matching the one at the cursor |
| `C-c` | Say where the cursor is |

## Code

| Key | What it does |
|---|---|
| `M-.` | Jump to the definition of the symbol at the cursor (LSP) |
| `M-?` | List every usage of the symbol at the cursor (LSP). Enter jumps to one |
| `M-,` | Jump back, one press per jump, from a definition or a usage |
| `M-d` | Jump to the next mark or diagnostic, wrapping |

`M-h` or `M-x` `set-mode` sets the current buffer's language. `plain` stops the guessing,
`auto` puts it back.

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

## Quick asks

`M-k` is a prefix whose second key picks an ask. The selection is what it is about; with no
mark set it is the paragraph around the cursor in prose and that line in code.

| Key | What it asks | Where the answer goes |
|---|---|---|
| `M-k e` | Explain the selection | The conversation |
| `M-k f` | Fact check it, one note per claim | Notes on the selection |
| `M-k g` | Fill the gap it leaves | A suggested hunk |
| `M-k r` | Rewrite it | A suggested hunk |
| `M-k k` | Ask what you like: the question is typed | The conversation |
| `M-k c` | Clear every mark on this file | - |

A host that answers asks (letibot's editor pane) sends them; standalone rano hands them to
`send_command`, and `M-k k` opens the chat beside the text to type in. The presets are
`ask.<id>.<field>` lines in the config (see [configuration](configuration.md)): the keys,
labels, prompts and answer modes are the reader's.

## The panel

`M-i` shows a conversation beside the text and `M-i` again hides it: the buffers stay open and
nothing is asked. The panel talks to the letibot daemon for the folder the focused file is in —
one session, its rows, and a composer at the foot. Its top edge names the session and what it is
doing (`idle`, `thinking`, `waiting on you`), or says why there is no daemon.

| Key | What it does |
|---|---|
| `M-i` | Show or hide the panel |
| `C-]` | The keyboard to the other surface, opening the panel when it is shut |
| `M-m` | The focused surface takes the whole width, and gives it back |
| `RET` | Send what the composer says; in the session pick, attach the session under the cursor |
| `M-n` | A new session in this folder, and into it |
| `ESC` | Back to the text. The conversation stays open, and keeps streaming |
| `C-c` | Stop the running turn |
| `<up>` / `<down>`, `C-p` / `C-n`, `<pgup>` / `<pgdn>` | Move in the session pick, or scroll the conversation |
| `M-f` | Follow the focused file into another folder: that folder's daemon, that conversation |
| `M-t` | Fold a run of tool calls to one line, or open every call |

The composer is one line: any character is typed into it, `<home>` / `<end>` / `<left>` / `<right>` move
in it, and `RET` sends. In the session pick `M-n` makes a new session and a digit picks the row
with that number.

The panel takes the second column while the terminal has room for both: two fifths of it, capped
at eighty columns, and never less than sixty for the text. Below that the focused surface takes
the whole frame and `C-]` swaps which one shows. The border between them resizes on a drag.

Opening the panel attaches the session this folder used last, without asking; a folder with
several sessions and none remembered shows them newest first, and one session is taken. The last
session per folder is remembered in `panel-sessions`, beside `config.toml`.

With no `letibot` on `PATH` the panel says so and stays shut. A folder with no daemon leaves it
naming the folder, and `RET` asks the launcher for one — rano never starts a daemon itself, since
the model, the mode and the placement are the launcher's to choose. A card the session is waiting
on is named in the rows and answered in letibot: the panel says it can decide nothing, so nothing
is routed to it that would time out.

## The bottom terminal

`M-0` shows a shell strip under the text and hides it again; while the strip has the keyboard
the same key hands it back rather than hiding. The tabs live on the strip's rule, the active
one marked, and a drag on the rule resizes the strip.

| Key | What it does |
|---|---|
| `M-0` | Show or hide the terminal; focused, the keyboard back to the text |
| `M-n` | A new tab |
| `M-w` | Close the current tab |
| `M-[` / `M-]` | Previous / next tab |
| `M-+` / `M--` | A taller / shorter strip |
| `<pgup>` / `<pgdn>` | Scroll the strip's history; the wheel scrolls too |

A key the strip does not bind goes to the shell, apart from the host's own (`C-]`, `M-i`,
`M-m`), which work over every surface. `ESC` is the strip's own too: it hands the keyboard
back to the text rather than to the program in the shell.

## In a view

Previews, diffs, pictures, reviews and the conflict view replace the text's keys. `<f8>`,
`M-w`, `M-<` / `M->` and `M-x` still work.

| Key | What it does |
|---|---|
| `s` / `TAB` | Switch between two panels and one. The choice sticks for the session |
| `<up>` / `<down>`, `C-p` / `C-n`, `<pgup>` / `<pgdn>`, `SPC`, `<home>` / `<end>` | Scroll |
| `ESC`, `q`, `RET`, `C-g`, `C-c`, `M-p` | Back to the text |

A markdown view and a picture are one column, so `s` says there is nothing to split.

## Hunks

A mark a host set — a suggested replacement, or an edit already on disk — is drawn as a
review: `before` against `after`, over the file. `M-d` walks them with the diagnostics.

| Key | What it does |
|---|---|
| `a` | Take the suggestion, or keep the edit: the hunk's mark goes |
| `r` | Drop the suggestion, or put the edited lines back |
| `A` / `R` | The same for every hunk in the file |
| `y` / `n` | The same for every hunk of one reply set, in every open file the set touched |
| `e` / `E` | The same for every reply set at once, in every open file |
| `c` | Clear every mark in every open file, taking nothing |
| `M-d` | The next hunk or diagnostic |

Taking a suggestion and reverting an edit are each one undo step (`M-u`), and neither saves:
the buffer is left modified for you to read before `C-o`.

The palette (`M-x`) has the wider scopes: accept or reject every hunk one reply set, accept or
reject every hunk in every open file, and clear every mark.

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

In that diff `Y` / `N` answer, `s` switches panels and `ESC` returns to the question. `n`
also calls off the `C-x` or `M-w` the save was part of.

## Folder listing

`M-g`, `<f8>` on a folder, or `rano some/folder/` opens a listing, directories first with `/`.

| Key | What it does |
|---|---|
| `RET` | Enter the entry under the cursor: a folder replaces the listing, a file opens |
| `-` | The folder above |
| `r` | Read the folder again, keeping the cursor where it was |
| `q` / `ESC` / `C-g` | Close the listing |

The listing is read-only. Motion keys work as over text.

## Lists

The buffer list and the usage list.

| Key | What it does |
|---|---|
| `RET` | Go to the selected item |
| `<up>` / `<down>`, `C-p` / `C-n`, `<pgup>` / `<pgdn>`, `<home>` / `<end>` | Move |
| `<delete>` | In the buffer list: close the selected buffer |
| `ESC`, `C-g`, `C-c`, `C-x`, `M-l`, `M-?` | Close the list |

## A help page

| Key | What it does |
|---|---|
| `q`, `RET`, `ESC`, `C-g` | Close |
| `<up>` / `<down>`, `C-p` / `C-n` | Scroll a line |
| `<pgup>` / `<pgdn>`, `SPC` | Previous / next page |
| `<home>` / `<end>` | Top / bottom |

## M-x

| Key | What it does |
|---|---|
| `RET`, `TAB` | Run the selected command |
| `<up>` / `<down>`, `C-p` / `C-n` | Pick |
| `DEL`, `C-h` | Backspace |
| `ESC`, `C-g`, `M-x` | Cancel |

## Prompts and mouse

Prompts keep separate histories for search, exec and file names: `<up>` / `<down>` cycle them,
`TAB` completes file names, `~` expands to `$HOME`, `M-b` / `M-f` move by word. `ESC` or `C-g`
cancels a prompt.

Bracketed paste arrives as a single undoable edit.

Mouse: a click moves the cursor, a click on the cursor toggles the mark, a drag selects and
copies to the system clipboard (OSC 52), the wheel scrolls.
