# Configuration

`$XDG_CONFIG_HOME/rano/config.toml`, falling back to `~/.config/rano/config.toml`. A flat
`key = value` file. Unknown keys are ignored and an out-of-range value falls back to the
default, so a bad line is never fatal. A value containing `#` needs double quotes.

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
| `key_notation` | `emacs`, `nano` | Write keys `C-x` / `M-p` or `^X` / `M-P` in the bar and the help pages |
| `send_command` | a shell command | What `M-s` runs (see [embedding](embedding.md)) |
| `theme` | a name | `themes/NAME.toml` beside this file |
| `plugin` | a path | A `.wasm` plugin (see [plugins](plugins.md)) |
| `capabilities` | `["read-dir"]` | What the plugin may import. Empty grants nothing |
| `color.ROLE` | a look | One role overridden on top of the theme |

The todo keys need no configuration.

## Themes

A theme is a set of role overrides, one `role = "look"` line each, read from
`themes/NAME.toml` beside the config. Without one, every role draws in the terminal's own
sixteen slots, so the terminal's theme is the theme.

```toml
# ~/.config/rano/themes/gray.toml
user_block = "bg:#2f363b"
heading    = "fg:#6fbcbf bold"
added      = "bg:cube:22"
faint      = "fg:8"
```

A role's name is its variant in snake case. A look is space-separated words:

- `fg:N` / `bg:N` - a theme slot, 0 to 15.
- `fg:#rrggbb` / `bg:#rrggbb` - an exact colour, for a theme that looks the same whatever the
  terminal's palette.
- `fg:cube:N` / `bg:cube:N` - an xterm 256-colour index.
- `bold`, `dim`, `italic`, `underline`, `reverse`.
- `plain` - no look at all, on its own.

An override replaces the role's look whole: `heading = "fg:4"` is blue and not bold. A line
the reader cannot use is reported rather than skipped.

`color.ROLE = "look"` in `config.toml` does the same thing without a file, in file order.

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

`LETIBOT_TERM_FEATURES` is read when `RANO_TERM_FEATURES` is unset. Inside tmux nothing is on
unless the override says so.

## Pictures

A terminal that takes inline images draws them. Ghostty does. `RANO_TERM_FEATURES=images`
turns them on anywhere else. Without one a picture file is read as text, and `M-p` says why.

The protocol carries PNG, so anything else is converted first by a converter on the machine:
ImageMagick (`magick` or `convert`), macOS's `sips`, `ffmpeg`, or Python with Pillow, in that
order, whichever is installed. A conversion is asked to fit the box the picture will be drawn
in and is watched while it runs: two seconds, 256 MiB of memory, 32 MiB written. A converter
that passes any of those is killed and said to have been.

`RANO_IMAGE_CONVERT` names a converter yourself: a command line with `{in}`, `{out}`, `{w}`,
`{h}`, `{max}`. An empty value turns conversion off.

## Updates

With `autoupdate` on, rano makes one request to the GitHub releases API at startup, on a
background thread, and compares the tag with its own version. A newer release is said so, and
`M-v` installs it, replacing the binary in place. Nothing is downloaded until you press it. A
failed check is silent.

Downloads are HTTPS-only and certificate verification is not negotiable: curl runs with `-q`,
so `~/.curlrc` is not read at all. Both the request and any redirect must be HTTPS.
`install.sh` uses the same flags.
