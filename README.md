# rano

A nano clone for the terminal, written in Rust. Built on
[crossterm](https://crates.io/crates/crossterm) (raw mode, key events) and
[ratatui](https://crates.io/crates/ratatui) (rendering), with tree-sitter
syntax highlighting and a minimal LSP client.

## Build

```sh
cargo build --release
./target/release/rano [file]
```

## Usage

```
rano [file]
rano --version
```

Opens `file` if it exists, otherwise starts a new buffer that will be saved
under that name. No arguments starts an empty buffer. `--version` / `-V`
prints the version; `--help` / `-h` prints usage.

## Keys

| Key | Action |
|---|---|
| `^G` / `F1` | Help page |
| `^O` / `F2` | Write out (save) |
| `^R` / `F5` | Read a file into the buffer |
| `^F` / `F3` | Where is (search); `^B` searches backwards |
| `M-B` / `M-F` | Jump to previous / next match |
| `M-C` / `M-R` | In the search prompt: toggle case sensitivity / regex |
| `^\` / `F4` | Replace (y/n/a/q per match; `a` replaces from the cursor on) |
| `^T` / `F6` | Execute shell command (stdout inserted async on success) |
| `M-\|` | Filter the marked region through a shell command |
| `M-U` | Undo (runs of the same action coalesce into one step) |
| `M-E` | Redo |
| `^K` | Cut line / marked region (repeat to cut more lines) |
| `^U` | Paste (nano's "Un-Cut") |
| `M-6` | Copy current line / marked region to the cutbuffer (no delete) |
| `^D` | Delete char (remembered for `^U`) |
| `M-A` / `^A` | Set/unset mark (selection) |
| `M-]` | Jump to matching bracket |
| `^/` / `F11` | Go to line |
| `M-D` | Jump to next diagnostic (wraps) |
| `M-.` | Jump to definition (LSP) |
| `M-,` | Jump back (stacked — one press per jump) |
| `M-N` | Toggle line-number gutter |
| `F8` | Open file (new buffer when `multibuffer`, else replaces current) |
| `M-<` / `M->` | Previous / next buffer |
| `F9` | Sort lines (whole buffer, or marked region) |
| `^J` / `F10` | Justify current paragraph |
| `F7` | Make backup (`file~`) |
| `^C` | Show line/column |
| `^X` | Exit (asks to save each modified buffer in turn) |

Movement: arrows, `Home`, `End`, `PgUp`, `PgDn`, `^P`/`^N` (line), `^E` (end
of line), `Alt+Left`/`Alt+Right` or `Ctrl+Left`/`Ctrl+Right` (word), `◂`/`▸`
for back/forward in prompts. Mouse: left click moves the cursor (and starts
a selection), left drag extends it, the wheel scrolls the view (the cursor
stays put unless the scroll would push it out of sight). Editing: `Enter`,
`Backspace`, `Delete`, `Tab`.
`Tab` follows the buffer's own indent style: space-indented files get spaces
up to the next unit boundary (the unit is detected from the file — e.g.
rano's own 4-space source), tab-indented files get a tab char; `Backspace`
on leading whitespace eats a whole unit. `Esc` clears an active mark or
cancels a prompt (`^G` also cancels prompts).

Prompts: `Up`/`Down` cycle history (search, exec, and filename prompts keep
separate histories), `Tab` completes file names, `~` expands to `$HOME` in
filename prompts, `M-b`/`M-f` move by word.

Bracketed paste is supported: multi-line pastes arrive as a single undoable
edit.

## Configuration

`$XDG_CONFIG_HOME/rano/config.toml` (falling back to `~/.config/rano/config.toml`):

```toml
tab_width = 8      # 1..=16, tab rendering + horizontal scrolling
auto_indent = true
line_numbers = true
multibuffer = false # F8 pushes a new buffer instead of replacing the current one
```

Unknown keys are ignored; out-of-range values fall back to the defaults.

## Notes

- The look follows nano's default theme: an inverted title bar (name,
  centered file name + ` *` when modified, `[i/n]` buffer position), an
  inverted status line (centered messages, right-aligned prompts, and the
  cursor's `Ln X, Col Y` at the right edge when idle), and a two-line
  inverted function bar. The bar, the help overlay, and their key labels
  are all generated from one binding table, so they cannot drift.
- Syntax highlighting via tree-sitter for Rust, Go, Bash, Python, C, JSON,
  Common Lisp, JavaScript, TypeScript (+TSX), Markdown, TOML, YAML, HTML,
  CSS, Lua, Ruby, PHP, Java, Make, Dockerfile, INI-style configs, diffs,
  Elisp, Scheme, SQL and Clojure — detected by file extension, by
  conventional file names (`Makefile`, `Dockerfile`, `.gitconfig`), and by
  shebangs for extension-less scripts.
  Scratch buffers are not highlighted. Search
  matches, the selection, and diagnostics take priority over highlight
  colors (diagnostics underline the offending range in red/yellow/blue; the
  line-number gutter colors diagnostic rows the same way).
- Syntax errors are visible without a language server: tree-sitter `ERROR`
  and missing nodes become red diagnostics on every re-parse. Zero-width
  LSP diagnostics (rust-analyzer's insertion-point errors) are widened to
  one visible column. Language servers only fully engage inside a supported
  project (e.g. a `Cargo.toml` for rust-analyzer); standalone files get
  tree-sitter feedback only.
- Live completion (LSP): typing an identifier, or `.` / `::` after one,
  requests `textDocument/completion` and shows a popup below the word
  (above it near the bottom). The server's own filtered, relevance-ordered
  list is shown as-is in an 8-row scrolling window — fuzzy matches keep
  the server's ranking. `Up`/`Down` or `^P`/`^N` pick, `Enter` or `Tab`
  insert, `Esc` dismisses; typing and Backspace keep it open. The document
  is synced to the server before each request, each response is matched to
  the keystroke that asked for it (late answers for older typing are
  dropped), and snippet placeholders are flattened to plain text.
- LSP: when a language server is on `$PATH` (rust-analyzer, gopls,
  bash-language-server, pylsp, clangd, vscode-json-language-server, cl-lsp,
  typescript-language-server, marksman, taplo, yaml-language-server,
  vscode-html-language-server, vscode-css-language-server,
  lua-language-server, ruby-lsp, intelephense, jdtls, sqls, clojure-lsp),
  rano
  starts it in the background, syncs changes with 300 ms debounce, and shows
  publishDiagnostics. The handshake never blocks the UI. Languages rano has
  no server for (Make, Dockerfile, INI, diff, Elisp, Scheme) run with
  tree-sitter feedback only, without a spawn attempt.
- Undo keeps up to 500 steps as region-based edits; runs of the same action
  (typed words, backspace runs, repeated `^K`, replace-all) coalesce into a
  single step. Redo is exact (each undo step is its own inverse).
- Atomic saves: writes go to `file.tmp` + rename. CRLF line endings are
  detected on load and preserved on save.
- Executed commands run asynchronously; their stdout is inserted below the
  cursor when they finish, with one undo step. Failing commands insert
  nothing and show the exit code.
- Long lines scroll horizontally (display-column aware, so tabs behave).

## Development

```sh
cargo test            # unit tests (the slow rust-analyzer e2e is #[ignore]d)
cargo test -- --ignored   # includes the LSP end-to-end flow
cargo clippy --all-targets -- -D warnings
cargo fmt --all -- --check
```

CI (`.github/workflows/ci.yml`) runs fmt, clippy, and tests on every push and
pull request.
