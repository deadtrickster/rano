# A PDF reader, as a plugin

`examples/plugin-pdf/` opens a `.pdf` as markdown: `# <file name>`, then `## Page N` and that
page's laid-out text in a fenced block. Search, copy and `M-P` work on that text. On a terminal
with a picture protocol the host draws the page over it. A page with no text layer (a scan) says
so and points at `t`.

It is an example, not a builtin. Point a config at it:

```toml
plugin = "…/examples/plugin-pdf/plugin.wasm"
```

```sh
RANO_PLUGIN=$PWD/examples/plugin-pdf/plugin.wasm RANO_CAPABILITIES=convert-page rano paper.pdf
```

## Keys

The view's hint line names them:

```
paper.pdf · page 3 · 96 dpi · auto
n next · p prev · g<N> goto · z/Z zoom · 0 reset · t text · q close
```

| Key | What it does |
|---|---|
| `n` / `p` | Next / previous page |
| `g<N>` | Page `N` (`g12`) |
| `z` / `Z` | Zoom ×1.5 / ÷1.5, re-rendered at the new dpi |
| `0` | Back to the opening zoom |
| `s` | Two pages side by side, if the pane's scratch holds both |
| `t` | Toggle the page's text and the picture |
| `q` | Close the view |

## State is the path

The ABI instantiates a fresh module per call, so the plugin keeps its state in a fragment on the
path:

```text
paper.pdf                        page 1, 96 dpi, auto
paper.pdf#p=4                    page 4
paper.pdf#p=4&z=144              page 4, zoomed in
paper.pdf#m=text                 text only
paper.pdf#s=2                    two pages side by side
```

A suffix counts as state only if every `k=v` in it is one the plugin writes, so `a#b.pdf` is a
file name. A page past the end shows a message, not an error. The ABI has no page count, so the
end is found by being refused.

## Host interface

```wat
;; one page of the file this call is about, as [width u32 LE][height u32 LE][rgb]
(func (import "rano" "convert_page") (param i32 i32 i32 i32) (result i32))
;; the same page as text
(func (import "rano" "convert_page_text") (param i32 i32 i32) (result i32))
```

Both write into the guest's memory and return the byte count, or `i32::MIN` for a refusal. The
capability is `convert-page`. The host answers only for the file the call is about and counts
pages from zero. The pixel block is the one `provider_picture` answers with. Zoom is a dpi, not a
size. `on_key(ptr, len)` takes `"<path>\n<key>"` and returns the path to open next.

Bounds: a page is at most 16 MiB (`MAX_HOOK_BYTES`), reached by A4 at 240 dpi, and a larger page
is re-rendered at a dpi that fits. The dpi floor is the host's 24. Nothing is decoded in wasm, so
the default `HOOK_FUEL` is enough.

## Build and test

```sh
rustup target add wasm32-unknown-unknown      # once
cd examples/plugin-pdf
cargo build --release --target wasm32-unknown-unknown --features page-picture,page-text
cp target/wasm32-unknown-unknown/release/rano_plugin_pdf.wasm plugin.wasm
cargo test                                    # native, needs no PDF
```

Without the features the plugin still claims a `.pdf` and names what the host lacks.

Not supported: a page count, links, selection inside the picture, OCR.
