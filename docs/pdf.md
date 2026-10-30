# A PDF reader, as a plugin

`examples/plugin-pdf/` opens a `.pdf` as **markdown**: the document's text, page by page, in the
renderer rano already has. That is what a terminal reads best - at the reader's own font size,
searchable, scrollable - where a page drawn as a picture is a thumbnail, because kitty's placeholder
scheme allows thirty rows and a whole A4 page needs a hundred.

The host extracts the text a page at a time; everything that makes it *reading* - the headings, the
fences that keep a page's layout, the picture mode for a scan - is the plugin's.

It is an **example, not a builtin**. Shipping it inside the binary is `include_bytes!` plus the
unpack-to-`scripts/built-in/` path of `plans/S10-wasm-hooks.md` §2.4, which is host work. Point a
config at it:

```toml
plugin = "…/examples/plugin-pdf/plugin.wasm"
```

```sh
RANO_PLUGIN=$PWD/examples/plugin-pdf/plugin.wasm RANO_CAPABILITIES=convert-page rano paper.pdf
```

## The keys

Every key is named in a hint line the view emits, so a reader who never reads this page finds them:

```
paper.pdf · page 3 · 96 dpi · auto
n next · p prev · g<N> goto · z/Z zoom · 0 reset · t text · q close
```

| key | what it does |
|---|---|
| `n` | the next page |
| `p` | the page before |
| `g<N>` | page `N` (`g12`). The *prompt* that asks for the number is the host's |
| `z` | sharper: the zoom ×1.5, re-rendered at the new dpi |
| `Z` | smaller: ÷1.5 |
| `0` | back to the zoom the document opened at |
| `s` | two pages side by side, for a pane wide enough to hold them |
| `t` | the page's text, or the picture: the one key that changes what the view is |
| `q` | close the view (the host's key, listed because the reader does not care which side of the ABI a key lives on) |

**`s` composes two pages into one picture**, because the host draws one picture: each page is
rendered into half the scratch the plugin carves from its arena, then their rows are interleaved.
The state line says `· 2-up`, the state lives in the fragment (`#s=2`), and a pane whose scratch
cannot hold both pages shows one - a page, not a failure.

The mode word is **`auto`**, not `picture`: the build offering a picture is not the terminal taking
one, and a line that claims what is not on the glass is worse than a vague one.

## The path is the state

The ABI instantiates a **fresh module for every call** (`plans/S10-wasm-hooks.md` §2.1), so a plugin
cannot remember which page you are on. This one does not try: the state is a fragment on the path,
and every function in it is a pure function of that string.

```text
paper.pdf                        page 1, 96 dpi, auto
paper.pdf#p=4                    page 4
paper.pdf#p=4&z=144              page 4, zoomed in
paper.pdf#m=text                 the page's text, no picture
```

A suffix only counts as state if every `k=v` in it is one this plugin writes, so a document actually
called `a#b.pdf` opens as page 1, and `a.pdf#p=x` is a file name rather than a bad request.

## What a view is

- **The document is markdown**: `# <file name>`, then `## Page N` and that page's laid-out text in
  a fenced block, which the host renders as prose and a page becomes a heading to search for.
- **The page's text is the buffer** when the host can give it, in *both* modes. On a terminal with a
  picture protocol the host draws the page over it, and search, copy and `M-P` still work on the
  text. On a terminal without one, the same view is the page.
- **A page with no text layer** (a scan) says so and points at `t`.
- **A page that is not there** is a sentence, not an error: *"no page 13 from the renderer … past
  the end of this document, or the file is not one the converter can read. Press p to go back."*
- **The page cache holds three pages**, keyed by document, page, dpi and text-or-picture. Under a
  fresh instance per call it only saves a repeat render inside one call.

## What the host has to provide

Two imports and one call site:

```wat
;; one page of the file this call is about, as [width][height][rgb]
(func (import "rano" "convert_page") (param i32 i32 i32 i32) (result i32))
;; the same page as text, for a terminal with no picture protocol
(func (import "rano" "convert_page_text") (param i32 i32 i32) (result i32))
```

Both are the `read_dir`/`read_file` shape: **write into the guest's own memory and return the byte
count**, or `i32::MIN` for a refusal. The grant is one capability, `capabilities = ["convert-page"]`,
and the host answers only for the file the call is about, counting pages **from zero** - a reader
that shows "page 1" subtracts once, at its own edge.

The pixels come back as `[width u32 LE][height u32 LE][rgb…]` - the same block `provider_picture`
answers with, so there is one picture format across the ABI and nothing to decode in wasm. The
host renders PPM, which is what `pdftoppm` writes for free.

`provider_picture` is the other half: the host draws those pixels in the terminal and scales them
into the cell box it wants. The zoom is a **dpi, not a size** - `z` buys detail, the box stays the
host's.

`on_key` is one more call site: the plugin exports `on_key(ptr, len)` with `"<path>\n<key>"` and
answers the path to open next. A host that calls it has a reader; one that does not has a view whose
keys are reachable by typing the fragment.

**One thing the ABI does not say**: how many pages a document has. `convert_page` takes a page and
answers a page, so "past the end" is discovered by asking and being refused - which is why the view
says *no page 13* rather than *13 of 12*.

## The bounds

- **`PAGE_BYTES` is 16 MiB**, the ABI's own `MAX_HOOK_BYTES` on the whole payload. A4 needs 6.2 MiB
  at 150 dpi and 16.7 MiB at 241, so the cap is reached at **240 dpi**; a page above it is
  re-rendered at the dpi that fits rather than refused.
- **The dpi floor is the host's 24**, so the fallback cannot ask for something the host rejects. That
  matters: a floor below it (12 was the first draft) is refused as a bad dpi, which reads as "no
  such page" and sends the reader down the wrong branch.
- **Nothing is decoded**, so `HOOK_FUEL` stays at §5's 50 M. The picture path costs a copy and the
  raster arithmetic, not an inflate.

## Building it

```sh
rustup target add wasm32-unknown-unknown      # once
cd examples/plugin-pdf
cargo build --release --target wasm32-unknown-unknown --features page-picture,page-text
cp target/wasm32-unknown-unknown/release/rano_plugin_pdf.wasm plugin.wasm
```

The two features are separate so a host that lands one renderer first still has a build. Without
them the plugin still claims a `.pdf` and answers it with a sentence naming what the host lacks.

## Tests

`cargo test` in `examples/plugin-pdf` runs 19 tests natively, no wasm toolchain and no PDF. The fake
host models the real one **including its dpi floor** - a fake that answers where the host refuses
certifies a sequence the host cannot serve, which is how a broken fallback once passed.

- a `.pdf` is claimed and anything else refused;
- a page past the end is a message in the view, not a panic;
- the text path produces rows, wrapped to the viewport;
- a page too big for the buffer is re-rendered smaller, and the floor dpi is respected;
- the picture is `[width][height][rgb]`;
- every key moves the state to the path the hint line implies;
- the fragment round-trips, and a suffix that is not state is part of the file name;
- the cache evicts the oldest page and never confuses two documents;
- `tests/wasm.rs` reads the committed `plugin.wasm` and checks its exports and that every import it
  declares is one of the two renderers.

## What it is not

- **Not a text extractor with a page count**: the total is the host's to add.
- **No links, no selection inside the picture**: the buffer's text is what search and copy see.
- **No OCR**: a scanned page with no text layer is a picture and says so.
