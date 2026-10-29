# A PDF reader, as a plugin

`examples/plugin-pdf/` opens a `.pdf` as a **view** instead of as binary noise: a page, a zoom, a
text mode, and keys. The host rasterizes a page when asked; everything that makes it *reading* —
which page, at what dpi, what the screen says, what a key does — is the plugin's.

It is an **example, not a builtin**: shipping it inside the binary is `include_bytes!` plus the
unpack-to-`scripts/built-in/` path of `plans/S10-wasm-hooks.md` §2.4, and that is host work in
`src/plugin.rs`, not a line an example can add to itself. Until then it is a plugin you point at:

```toml
plugin = "…/examples/plugin-pdf/plugin.wasm"
```

```sh
RANO_PLUGIN=$PWD/examples/plugin-pdf/plugin.wasm rano paper.pdf
```

## The keys

Every key is named in a hint line the view itself emits, on the second line, so a reader who never
reads this page still finds them:

```
paper.pdf · page 3 · 96 dpi · picture
n next · p prev · g<N> goto · z/Z zoom · 0 reset · t text · q close
```

| key | what it does |
|---|---|
| `n` | the next page |
| `p` | the page before |
| `g<N>` | page `N` (`g12`). The *prompt* that asks for the number is the host's; this is what it hands the plugin when it has one |
| `z` | sharper: the zoom ×1.5, re-rendered at the new dpi (96 → 144 → 216 → 300) |
| `Z` | smaller: ÷1.5 (96 → 64 → 48) |
| `0` | back to the zoom the document opened at |
| `t` | the page's text, or the picture: the one key that changes what the view is |
| `q` | close the view (the host's key, listed here because the reader does not care which side of the ABI a key lives on) |

## The path is the state

The ABI instantiates a **fresh module for every call** (`plans/S10-wasm-hooks.md` §2.1), so a
plugin cannot remember which page you are on. This one does not try: the state is a fragment on the
path, and every function in it is a pure function of that string.

```text
paper.pdf                        page 1, 96 dpi, auto
paper.pdf#p=4                    page 4
paper.pdf#p=4&z=144              page 4, zoomed in
paper.pdf#m=text                 the page's text, no picture
```

A suffix only counts as state if every `k=v` in it is one this plugin writes, so a document actually
called `a#b.pdf` opens as page 1, and `a.pdf#p=x` is a file name rather than a bad request. Two
panes on one document cannot disagree about the page they are on, and the same keys work from a
shell:

```sh
RANO_PLUGIN=$PWD/examples/plugin-pdf/plugin.wasm rano 'paper.pdf#p=12&z=144'
```

## What a view is

- **The page's text is always the buffer** when the host can give it, in *both* modes. On a terminal
  with a picture protocol the host draws the page over it, and search, copy, `M-P` and `M-G` still
  work on the text. On a terminal without one, the same view is the page — which is why `t` is a
  toggle and not a rescue.
- **Auto** (the default) shows the picture when this build has a rasterizer and the text either way.
  A page with no text layer (a scan) says so and points at `t`.
- **A page that is not there** is a sentence, not an error: *"no page 13 from the renderer … past
  the end of this document, or the file is not one the converter can read. Press p to go back."*
- **The page cache holds three pages** (`src/cache.rs`), least-recently-used first out, keyed by
  document, page, dpi and text-or-picture. Under a fresh instance per call it is empty at the start
  of every call and only saves a repeat render inside one; the moment a host keeps an instance for a
  view's calls, it is already the reader's page cache and no other line moves.

## What the host has to provide

Two imports, and one call site. Everything else in this document is the plugin's.

```wat
;; one page of the file this call is about, rasterized at `dpi`, as PNG bytes
(func (import "rano" "convert_page") (param i32 i32) (result i32))
;; the same page as text, for a terminal with no picture protocol
(func (import "rano" "convert_page_text") (param i32) (result i32))
```

**The shape of the answer.** The brief says `-> packed {ptr, len}`. Rust cannot call a wasm
multi-value import through `extern "C"`, so this plugin reads that as the same structure the result
block uses: **an offset, into the plugin's memory, of a pair of little-endian `u32`s** — `{ptr,
len}` — with the bytes wherever the host put them. `src/host.rs` is the whole wiring point: if the
host's shape is `(page, dpi, out_ptr) -> i32` or a multi-value return, one `extern` line and one
call change, and nothing else in the plugin does.

`provider_picture` answers `[width u32 LE][height u32 LE][rgb…]`, which is the other half: the host
draws those pixels in the terminal (its graphics layer) and scales them into the cell box it wants.
The zoom is therefore **a dpi, not a size**: `z` buys detail, and the box stays the host's.

**`on_key` is one call site.** The plugin exports `on_key(ptr, len)` with the input `"<path>\n<key>"`
and answers the path to open next (`paper.pdf#p=4`). A host that calls it and re-opens the answer
has a reader; a host that does not has a plugin whose view is still correct, and whose keys are
reachable by typing the fragment.

**Two things the ABI does not say, and this plugin cannot invent:**

- **How many pages a document has.** `convert_page` takes a page and answers a page, so "past the
  end" is discovered by asking and being refused — which is why the view says *no page 13* rather
  than *13 of 12*. One more field (a page count, or a structured error) would put the total in the
  state line.
- **How much fuel a picture call has.** See the next section.

## The cost, measured

Measured on the built module with wasmtime 49 and a stub rasterizer (`rano` 0.9.1's own engine),
against `plans/S10-wasm-hooks.md` §5's **50 M units of fuel a call**:

| page | RGB | fuel | verdict |
|---|---|---|---|
| 200×300 | 180 KB | 7.6 M – 11.1 M | fits |
| 340×510 | 520 KB | 21.5 M – 31.4 M | fits, and this is what the budget buys |
| 794×1123 (A4 at 96 dpi) | 2.7 MB | ~163 M | **four times the budget** |

Decoding a page costs **~61 units of fuel per RGB byte** in wasm: inflate (~41) and unfilter
(~20) together, measured with the row filters a rasterizer actually writes. So:

- `PICTURE_BYTES` is **512 KiB** — about 175 000 pixels, half of what 50 M covers, because a page's
  deflate stream is not always as kind as the one measured.
- A page above that is **re-rendered at the dpi that fits** rather than refused: an A4 page comes
  back at ~42 dpi, which is the shape of a page at the size a cell box shows anyway. The ladder
  costs a second rasterization; a host that grants a picture hook ~300 M of fuel would let the first
  render stand, and raising `PICTURE_BYTES` is one constant.
- **The other way out is the host's**: answer raw RGB instead of PNG and there is nothing to decode
  at all, which is what 50 M of fuel comfortably covers.

A view costs ~17 000 units of fuel (the text path), so the picture is the only expensive hook here.

## Building it

```sh
rustup target add wasm32-unknown-unknown      # once
cd examples/plugin-pdf
cargo build --release --target wasm32-unknown-unknown                       # 45 KB, no imports
cargo build --release --target wasm32-unknown-unknown --features page-png,page-text   # 73 KB
cp target/wasm32-unknown-unknown/release/rano_plugin_pdf.wasm plugin.wasm
```

The committed `plugin.wasm` is the **default build**: no imports at all, because `src/plugin.rs`
does not have `convert_page`/`convert_page_text` yet, and a module that imports what the host cannot
provide fails to instantiate and cannot even say why. Without them the plugin still claims a `.pdf`
and answers it with a sentence naming what the host lacks — a view you can act on, instead of a PDF
opened as binary noise. The two features are separate so a host that lands one renderer first still
has a build.

## Tests

`cargo test` in `examples/plugin-pdf` runs 24 tests natively, no wasm toolchain and no PDF:

- a `.pdf` is claimed and anything else is refused (`provider_open`'s sentinel);
- a page past the end is a message in the view, not a panic;
- the text path produces rows, wrapped to the viewport, and says when a page is taller than the pane;
- a build with no renderer answers the message, and a build with a rasterizer but no text says where
  the text went;
- the picture is `[width][height][rgb]`, and a page too big for the budget is re-rendered smaller;
- every key moves the state to the path the hint line implies, and the hint line names every key;
- the fragment round-trips, and a suffix that is not state is part of the file name;
- the cache evicts the oldest page and never confuses two documents;
- the PNG decoder: colour types, all four filters, and a refusal by name for 16-bit, interlaced,
  palette and truncated data;
- `tests/wasm.rs` reads the committed `plugin.wasm` and checks its exports and that every import it
  declares is one of the two renderers.

## What it is not

- **Not a text extractor with a page count**: the total is the host's to add (above).
- **No links, no selection inside the picture**: the buffer's text is what search and copy see; the
  picture is pixels.
- **No interlaced, palette or 16-bit PNG**: a rasterizer does not write them, and refusing by name is
  better than a wrong page.
- **No OCR**: a scanned page with no text layer is a picture and says so.
