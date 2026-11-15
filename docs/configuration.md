# Configuration

`$XDG_CONFIG_HOME/rano/config.toml`, falling back to `~/.config/rano/config.toml`. A flat
`key = value` file. Unknown keys and bad values are ignored and an out-of-range number is
clamped, so a bad line is never fatal. A value containing `#` needs double quotes.

```toml
tab_width = 4
indent = spaces
indent_size = 4
auto_indent = true
line_numbers = true
multibuffer = false
wrap = true
autoupdate = true
key_notation = emacs
send_command = "my-tool --from rano"
theme = ""
plugin = ""
capabilities = []
```

| Key | Values | What it does |
|---|---|---|
| `tab_width` | 1..=16, default 4 | How wide a tab is drawn, and the step for horizontal scrolling |
| `indent` | `spaces`, `tabs`, `auto` | What `Tab` inserts: `indent_size` spaces, one tab character, or the buffer's own style detected from the file |
| `indent_size` | 1..=16, default 4 | Columns per indent step when the step is spaces |
| `auto_indent` | `true`, `false` | Keep the line's indent on `Enter` |
| `line_numbers` | `true`, `false` | The line-number gutter. `M-n` toggles it at runtime |
| `multibuffer` | `true`, `false` | `F8` pushes a new buffer instead of replacing the current one |
| `wrap` | `true`, `false` | Soft line wrap. `M-\` toggles it at runtime |
| `autoupdate` | `true`, `false` | Check GitHub for a newer release at startup |
| `key_notation` | `emacs`, `nano`, `words` | Write keys `C-x` / `M-p`, `^X` / `M-P`, or `ctrl-x` / `alt-p` in the bar and the help pages. `words` is the spelling the two programs share, and letibot sets it on the embedded editor |
| `send_command` | a shell command | What `M-s` runs (see [embedding](embedding.md)) |
| `theme` | a name | `themes/NAME.toml` beside this file |
| `plugin` | a path | A `.wasm` plugin (see [plugins](plugins.md)) |
| `capabilities` | `["read-dir", "read-file", "convert-page"]` | What the plugin may import. Empty grants nothing |
| `color.ROLE` | a look | One role overridden on top of the theme |
| `ask.ID.FIELD` | see below | One field of a quick ask |

## Quick asks

The `M-k` presets are four compiled in — `explain`, `fact-check`, `fill-gap`, `rewrite` — and the
file changes any field of one, or adds one of its own. A field the file does not mention keeps
what it had, so changing only a prompt does not cost the key.

```toml
ask.fact-check.prompt = "Check each claim. One note per claim: ok, warn or wrong, and why."
ask.fact-check.key = "f"
ask.translate.key = "t"
ask.translate.label = "translate"
ask.translate.answer = suggest
ask.translate.prompt = "Translate the selection into French. Answer with the translation only."
```

| Field | Values | What it does |
|---|---|---|
| `key` | one chord, `f` or `C-f` | The second key of `M-k <key>`. A preset without one is not bound |
| `label` | text | What the key bar and the help page call it |
| `answer` | `chat`, `notes`, `suggest` | Where the answer lands: the conversation, `note` marks on the selection, or a `suggest` hunk |
| `prompt` | text | What the host sends. Quote it: a prompt holds `#` and commas |

## Themes

A theme is a set of role overrides, one `role = "look"` line each, read from
`themes/NAME.toml` beside the config. Without one, roles use the terminal's sixteen
colours - and, on a terminal that says it draws truecolor (`COLORTERM`), the exact orange
`warning` needs, no theme slot being one.

```toml
# ~/.config/rano/themes/gray.toml
user_block = "bg:#2f363b"
heading    = "fg:#6fbcbf bold"
added      = "bg:cube:22"
faint      = "fg:8"
```

A role's name is its variant in snake case. A look is space-separated words:

- `fg:N` / `bg:N` - a theme slot, 0 to 15.
- `fg:#rrggbb` / `bg:#rrggbb` - an exact colour.
- `fg:cube:N` / `bg:cube:N` - an xterm 256-colour index.
- `bold`, `dim`, `italic`, `underline`, `reverse`.
- `plain` - no look at all, on its own.

An override replaces the role's look whole: `heading = "fg:4"` is blue and not bold. A bad
line is reported.

`color.ROLE = "look"` in `config.toml` does the same thing without a file, in file order.

## Key bindings

`[keys]` in `config.toml` rebinds a chord: `"chord" = "command"`, one per line. An empty
command unbinds it. `[keys.terminal]` (or any map's name) binds inside that map instead of
the global one.

```toml
[keys]
"M-K j" = "cut"
"M-q" = ""            # unbinds

[keys.terminal]
"M-n" = "terminal-new"
```

A chord is `C-x`, `M-x`, `S-TAB`, arrows as `<up>` and `<left>`, other special keys as
`<pgup>` and `<f5>`, and a space makes a sequence of them (`"M-K j"` is `M-K` then `j`). The
command is the name `M-x` completes to. A sequence may
go only through keys that are not themselves commands: `C-x` is Quit, so `"C-x k"` is
refused as going through a command.

Every refusal names its line: a chord that is not a chord, an empty chord, a chord that
goes through a bound command, a command there is none of, a map there is none of, a line
that is not a binding at all, and a key the host has promised the terminal — `M-i`, `C-]`,
`M-m`, however they are spelled, and their kind are the host's, and a map may not take one
over. The first refusal is flashed with the count of the rest; every one, with its line, is
said on stderr at startup.

## Environment

| Variable | What it does |
|---|---|
| `RANO_PLUGIN` | The plugin's path, over the config's `plugin` |
| `RANO_CAPABILITIES` | A comma list, over the config's `capabilities` |
| `RANO_AUTOUPDATE` | `0` turns the startup check off when `autoupdate` is unset |
| `RANO_UPDATE_ON_EXIT` | `0` turns off the note about a newer release on exit |
| `RANO_TERM_FEATURES` | Terminal features: `all`, `none`, or a comma list of `notify,progress,keys,links,clipboard,images,background`. `-name` removes one from what detection found |
| `RANO_IMAGE_CONVERT` | The command that converts a picture to PNG (see below) |
| `RANO_FILE`, `RANO_LINE`, `RANO_COLUMN` | Set for `send_command`: the file and the cursor |
| `RANO_LSP_RAW` | A path: append every language server message there, for debugging |
| `RANO_WRITE_STATS` | A path, or `-` for stderr: write the terminal's write stats on exit |
| `RANO_INSTALL_DIR`, `RANO_FROM_SOURCE`, `RANO_VERSION` | `install.sh`: where the binary goes, build instead of download, the tag |

`LETIBOT_TERM_FEATURES` is read when `RANO_TERM_FEATURES` is unset. Inside tmux a picture is
drawn when the client tmux reports (`client_termname`) is in the kitty family and
`allow-passthrough` is on — both asked of tmux once, with a deadline, since `TERM_PROGRAM` does
not cross ssh. With passthrough on and the client unknown, the capability query is sent
DCS-wrapped and its answer decides. With `allow-passthrough` off, `M-p` says exactly what to
change: `tmux: set -g allow-passthrough on`.

## Pictures

A terminal that takes inline images (Ghostty, or any with `RANO_TERM_FEATURES=images`) draws
them. Otherwise a picture opens as text and `M-p` says why.

Only PNG is drawn directly. Other formats go through the first converter found: ImageMagick
(`magick` or `convert`), `sips`, `ffmpeg`, or Python with Pillow. A conversion is killed past
two seconds, 256 MiB of memory or 32 MiB written.

`RANO_IMAGE_CONVERT` names a converter yourself: a command line with `{in}`, `{out}`, `{w}`,
`{h}`, `{max}`. An empty value turns conversion off.

## Updates

With `autoupdate` on, rano asks the GitHub releases API once at startup, in the background.
A newer release is announced, and `M-v` installs it in place. Nothing is downloaded until then.
A failed check is silent. Downloads are HTTPS-only, redirects included, and curl runs with `-q`
so `~/.curlrc` is ignored. `install.sh` uses the same flags.
