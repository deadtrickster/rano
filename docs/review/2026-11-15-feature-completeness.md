# Feature completeness, 2026-11-15

Written by cc@rano at `9047198`. "Driven" means run in `target/release/rano` inside tmux at
100x30, 110x28 and 80x24 with an empty config, and the screen read back. "Read" means the code
was read and nothing was run. Anything not listed was not looked at. LSP, plugins, PDF, pictures,
themes, the updater and the embedded `Editor` API were not driven.

## Verdict

The editor core is complete and behaves as documented: open, edit, search, undo, save prompts,
several buffers, previews, the conflict view, log tail, export to text, markdown and html. The
three newest surfaces are not finished. The bottom terminal is a demo: it corrupts the screen
under ordinary programs and leaks keys into the file. The panel works but shares the frame
badly with the terminal. `--export ansi` is broken. The documentation stops at the panel and
does not know the terminal exists.

## Claims against behaviour

| Feature | Claimed in | State | Evidence |
|---|---|---|---|
| Open, `file:LINE`, centred | README usage | works, driven | `long.txt:150` opens with line 150 centred, `Ln 150, Col 1` |
| Several files, buffer list, `M-<` `M->` | README, keys | works, driven | `[1/3]` in the title, `M-l` lists three |
| Search `C-f`, next `M-f` | README, keys | works, driven | `line 29` goes to 290, `M-f` to 291 |
| Replace `C-\` | keys | suspect, driven | after a search, needle typed again gave `Replaced 0 occurrences` with eight matches below, and the next key was typed into the buffer. Cause not established, see open questions |
| Undo `M-u` | keys | works, driven | |
| Execute `C-t` | keys | works, driven | `Ran: echo out`, output inserted |
| Quit with a modified buffer | keys | works, driven | `Save modified buffer? (y, n, or ^G to cancel)` |
| Help prefix card, all keys | README, keys | works, driven | card under `C-g`, `b` lists by group |
| `M-x` palette | README | works, driven | fuzzy match, says `no command matches` |
| Markdown preview `M-p` | README, keys | works, driven | ticked and unticked tasks both draw as `·` in a plain capture. Colour was not captured |
| Conflict view | keys | works, driven | `Conflict 1 of 1`, ours and theirs |
| Folder listing `M-g` | keys | partial, driven | opens a `Directory:` prompt first. The docs say it opens a listing |
| Log mode, tail, follow | README, logs | works, driven | `Tailing - showing the last 200 lines`, an appended row appears, `VIEW` in the title |
| Search inside a log | logs | not confirmed | `C-f row 1999` left no message on screen in my capture |
| `--export text`, `markdown`, `html` | README usage | works, driven | |
| `--export ansi` | README usage | broken, driven | `fn main() {` comes out as `fnmain(){`, indentation and inner spaces are dropped. Stripping SGR from the output does not give the source back |
| Syntax errors as diagnostics | README | not confirmed | ` zzz` appended to a Rust line showed nothing in a plain capture after 3 s. rust-analyzer is installed. May be colour only |
| Editor scrollbar | none | works, undocumented | a `│` track with the thumb the accent on the same `│` in the text column — the split rule's own column while the panel is open |
| Panel `M-i`, zoom, no-daemon state | keys | works, driven | split at 110 columns, whole frame at 80. Offline text names `/` instead of the folder |
| Panel key bar | keys | defect, driven | `<pgup> Page UESC Back to tM-f Follow th`, the same defect the rano session recorded in room row 1329 |
| Bottom terminal `M-0` | nowhere | partial, driven | see below |
| "No tabs" | README line 96 | stale | the terminal has tabs |
| Key rebinding | not claimed | missing | no `[keys]` in `src/config.rs` |

## The bottom terminal

Not in README, `docs/keys.md` or `docs/configuration.md` apart from the `terminal_scrollback`
commit. Driven at 100x30:

- It opens below the key bar, twelve rows, height fixed in code (`src/keys.rs:211`, `:223`). No
  key or mouse resize. This is the operator's complaint.
- Scrollback moves on `PgUp`. Nothing on screen says the view is off the bottom, and there is no
  scrollbar. This is the operator's complaint.
- After `seq 1 100` the visible rows end at 98. The last rows and the prompt are under the frame's
  edge.
- `clear; seq 1 5` leaves the earlier prompt mixed into the new output (`111:49:02] [0.000s]`,
  `31:49:03`). Erase and cursor addressing do not reach the screen.
- `less long.txt` draws with the first column missing (`ine 1`), and `PgDn` scrolls rano's
  scrollback, not less.
- `Tab` and `C-c` go to the editor: the file was indented and marked modified while the shell
  had focus. Details in the keys audit, findings 1 to 3.
- With the panel open, the panel's composer and hint rows are drawn a second time under the
  terminal's rule.
- The strip reads `tabs >1 (M-n new, M-[ M-] switch, M-w close)`. The bar under the editor keeps
  showing the editor's keys.

## letibot integration

Design: `letibot/docs/rano-integration.md` section 8.1.

| Row | State on master | Evidence |
|---|---|---|
| R1 prompt-cancel | landed | `6282759`, `16a2765` |
| R2 shared-keys | landed | `66fd0eb`, `src/agent/keys.rs` |
| R3 marks | landed | `src/marks.rs:258`, `:310`. `pk-r3-marks` still holds `39e17a9` as a patch not on master under that id |
| R4 ask | landed | `M-k` prefix, `src/ask.rs`. `pk-r4-ask` holds two patches not on master under those ids |
| R5 chrome-theme | landed | `src/theme.rs:75`, `src/editor.rs:1564`. `pk-r5-chrome` holds one |
| R6 letibot client | landed | `src/letibot.rs` |
| R7 panel | landed, one open defect | key bar truncation |
| R8 panel-context | not verified | `Citations` appears in `src/panel.rs`, `src/letibot.rs`. Not driven, no daemon in the test folder |

Whether the `pk-*` patches are superseded copies or lost work was not established. The six
`pk-*` worktrees each hold an untracked `pkg/` and were left in place.

letibot pins rano at `cafb702` (`letibot/crates/code/Cargo.toml:31`). That is before the bottom
terminal, so letibot has none of the terminal's defects yet and gets all of them on the next
bump. cc@letibot was asked for its own list on 2026-11-15 and had not answered when this was
written.

## Open questions

- Replace after a search: is the prompt prefilled with the last needle as editable text?
- Do diagnostics for a syntax error show without colour?
- Does an Alt chord typed in a non-Latin layout reach its binding?
- Are the `pk-r3`, `pk-r4`, `pk-r5` patches on master under other ids?

## Decisions

None taken. Proposed order: terminal correctness and key leak, `--export ansi`, bars, docs for
panel and terminal, then the keymap decisions in `2026-11-15-keys.md`.

### What was done (2026-11-15, the rano session)

The findings above are untouched; this is the record of what landed against the proposed order.

- Bars: `5eda3ca` — the function bar shows whole entries or none, a gap always between
  neighbours, entries past the width dropped rather than cut mid-label.
- The frame: `16a19bf` (the panel a full-height column, the strip under the text, the pty
  resized on every layout change), then one rule style (`c6664a9`), the grabbable thumb and
  the preview's single bar on the rule (`2a3b751`), the focused surface marked (`c761da5`),
  and the terminal's tabs on the strip's rule (`d487e05`).
- Docs for the panel and terminal: `528cc7f` for the chords, and the keys.md rewrite that
  adds the terminal's section and makes the doc agree with the maps in both directions by
  test (see the keys record's own appendix).
- The panel's letibot surface: a settled tool call is the crate's ToolRow with its real size
  (`0604738`), a run folds to one line naming the dominant non-success kind (`4b48106`,
  `34db3e5`), and the turn's state is one line above the composer — Responding with its clock,
  a pending ask named until it is answered anywhere (`372a553`, `11a5be3`). Attaching still
  does not replay the conversation: the panel fills from live events only.
- Terminal correctness and the key leak are track A's, untouched here; `--export ansi` is
  not done.

