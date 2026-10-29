# Plugins

A `.wasm` file named by `plugin` in the config, or by `RANO_PLUGIN`, gets hooks. A hook takes
text and answers text. Nothing else is imported unless a capability grants it.

## The ABI

```wat
(memory (export "memory") 1)                          ;; the guest's memory
(func (export "rano_abi") (result i32) i32.const 1)   ;; the version; anything else is refused
(func (export "rano_alloc") (param i32) (result i32)) ;; reserve n bytes -> offset
(func (export "on_open") (param i32 i32) (result i32)) ;; (in_ptr, in_len) -> result block
(func (export "on_save") (param i32 i32) (result i32))
(func (export "provider_open") (param i32 i32 i32 i32) (result i32))    ;; (path, cols, rows)
(func (export "provider_picture") (param i32 i32 i32 i32) (result i32)) ;; (path, cols, rows)
```

A hook returns the offset of two little-endian `u32`s, `{out_ptr, out_len}`, or `0xFFFFFFFF`
for "leave it alone". The hooks are found by name, so a later rano can add one without this
changing. An `on_*` export is a text hook. A `provider_*` export takes the viewport too.

`on_open` runs when a file's text is read. `on_save` runs when a buffer is written.
`provider_open` is asked first when a path is opened, and answers the text the path should open
as. `provider_picture` answers an RGB picture instead — see [A picture](#a-picture).

## What the host guarantees

- 50 million units of fuel and 64 MiB of guest memory per call.
- 16 MiB of text in and out, each way — and 16 MiB of RGB in a picture.
- A fresh instance per call, so nothing accumulates between saves.
- The answer to `on_save` replaces the buffer as one undo step, so the file and the screen
  hold the same text. A second save of unchanged text writes the same bytes.

## What a failure costs

Nothing. No ABI, an import, a trap, running out of fuel, asking for more memory than allowed,
answering something that is not text, or a text above the cap: the hook is skipped, the open
or the save happens exactly as it would have without it, and one line names the plugin and
the reason. A binary file never reaches a hook, and a file above 16 MiB is read by the
streaming loader with the hook skipped.

## Capabilities

A plugin imports nothing by default. `capabilities = ["read-dir"]` grants one import,
`rano.read_dir(path_ptr, path_len, out_ptr, out_cap) -> i32`: the text of a folder's listing,
rendered by the host with the same rules as rano's own listing, so a plugin view and the
built-in dired cannot disagree about what a row means. It answers the bytes written, `-needed`
for a buffer too small, or `i32::MIN` for an error.

`capabilities = ["read-file"]` grants a second, `rano.read_file` with the same shape and the
same answers: the bytes of a file. **It is scoped to one file.** A provider is handed a path,
so a viewer has to read the bytes itself — but the host answers only for *the path that call
was about*, resolved first, so `..`, a relative spelling and a symlink all land on that file and
any other path is refused with `i32::MIN`. A plugin granted `read-file` cannot read
`/etc/shadow`, a sibling of the file it was asked about, or anything else because it felt like
it. A text hook (`on_open`, `on_save`) is asked about a text rather than a path, so it has no
file to scope to and `read_file` refuses there too. A file above 16 MiB is refused rather than
handed over in pieces.

A module that imports anything ungranted is refused at load, by name. A misspelt capability
grants nothing.

```toml
plugin = "…/rano_plugin_dired.wasm"
capabilities = ["read-dir"]
```

```sh
RANO_PLUGIN=$PWD/examples/plugin-dired/plugin.wasm RANO_CAPABILITIES=read-dir rano .
```

## A folder viewer

`examples/plugin-dired/` answers `provider_open`, which is asked instead of opening a path. It
checks whether the path is a folder and, if it is, answers with its listing.

```ts
export function provider_open(ptr: u32, len: u32, _cols: u32, _rows: u32): u32 {
  const listing = readDir(String.UTF8.decodeUnsafe(ptr as usize, len as usize));
  return listing === null ? UNCHANGED : answer(listing);
}
```

A provider is told the viewport (`cols`, `rows`), so a plugin view is adaptive at any terminal
width. `RET`, `-`, `r` and `q` still work: the plugin decides the content, the editor keeps
the navigation.

## A picture

Some things are not text. `provider_picture(path_ptr, path_len, cols, rows)` is the same call
as `provider_open` — same arguments, same result block, `0xFFFFFFFF` for "not mine" — and its
answer is an RGB picture rather than a text:

```
[width: u32 LE][height: u32 LE][rgb: width * height * 3]
```

Pixels are row-major, three bytes each, top row first. `width * height * 3` may not pass 16 MiB,
and the header must describe exactly the bytes that came with it: a picture that is oversized or
that disagrees with its own header is refused, never clipped to fit, because a picture silently
cropped is a picture that lies about what the file looks like. A host that draws pictures asks
`provider_picture` before `provider_open` for the same path; a host that cannot draw one asks
only `provider_open`, so a viewer should answer both and let the host choose.

A terminal cell is about twice as tall as it is wide. A plugin that renders a scene should take
that into account — either by returning an image with the aspect of the cell area (`cols` wide
and `2 * rows` tall, which is what the STL example does) or by squashing its projection. The
host scales whatever it is given into the `cols` × `rows` cell rectangle.

## A 3D viewer, in Zig

`examples/plugin-stl/` is the worked example of both of the above: a provider written in Zig
against the raw ABI, with a fixed-buffer allocator and no WASI, that turns a `*.stl` file into a
picture and, for a terminal with no graphics protocol, into characters through a luminance ramp.
It asks for `read-file`, because a provider is handed a path and nothing else.

```sh
RANO_PLUGIN=$PWD/examples/plugin-stl/plugin.wasm RANO_CAPABILITIES=read-file rano model.stl
```

It answers `provider_picture` with `cols` × `2 * rows` RGB pixels, and `provider_open` with
`rows` lines of `cols` characters for the same path. Which one is drawn is the host's decision.
It refuses rather than half-draws: a model above 12,000 facets or 1 MiB, or a viewport above
131,072 pixels, is "not mine" and the file opens as text. Its STL reader, 3D maths and
depth-buffered flat-shaded rasteriser are ported from letim (`src/stl.zig`, `src/math3d.zig`,
`src/raster.zig`), Apache-2.0 as of commit `15d2338`.

```sh
cd examples/plugin-stl
zig build && cp zig-out/bin/plugin.wasm plugin.wasm   # zig 0.16, target wasm32-freestanding
```

## Writing one in Rust

`rano-plugin` (`sdks/rust/`) is the SDK. The ABI's raw pointers, arena and result block live
there, and a plugin is a function from `&str` to `String`.

```rust
use rano_plugin as rano;
rano::abi!();

#[unsafe(no_mangle)]
pub extern "C" fn on_save(ptr: u32, len: u32) -> u32 {
    rano::hook(ptr, len, |text| Some(text.to_uppercase()))
}
```

`examples/plugin-upper/` is that plugin, and `plugin.wasm` beside it is built, so it runs
without a `wasm32` toolchain:

```sh
RANO_PLUGIN=$PWD/examples/plugin-upper/plugin.wasm rano notes.txt
```

To rebuild it: `rustup target add wasm32-unknown-unknown`, then
`cargo build --release --target wasm32-unknown-unknown` in that directory, and copy the
`.wasm` beside the source.

## Writing one in TypeScript

`sdks/assemblyscript/rano.ts` is the same SDK for AssemblyScript, TypeScript that compiles to
wasm. `examples/plugin-upper-as/` is the same plugin in it.

```ts
import { answer, input, rano_abi_impl, rano_alloc_impl } from "../../sdks/assemblyscript/rano";

export function rano_abi(): i32 { return rano_abi_impl(); }
export function rano_alloc(n: u32): u32 { return rano_alloc_impl(n); }
export function on_save(ptr: u32, len: u32): u32 {
  return answer(input(ptr as usize, len as usize).toUpperCase());
}
```

```sh
npm i -g assemblyscript && asc index.ts -o plugin.wasm --optimize --use abort=index/rano_abort
```

AssemblyScript's runtime would import `env.abort`, and a plugin may import nothing it was not
granted, so the example supplies its own abort export and the build points at it. An export
whose name starts with `on_` is a hook, so anything else is named `rano_…`. The host checks
each discovered hook's signature at load.

The host does not care what wrote the module: Rust, C, Zig, Go through TinyGo, AssemblyScript,
or `wat2wasm`.

## The manual is one of these

rano's own manual is a plugin: `examples/plugin-manual/` answers `provider_open("rano:manual")`
with `docs/*.md` as one document, and `C-g m` (or `M-x manual`) opens it rendered. The docs are
`include_str!`d, so `docs/` is the only copy of the manual and editing one without rebuilding the
plugin fails a test.

It ships **in the binary** and is written out on first use to
`$XDG_CONFIG_HOME/rano/scripts/built-in/manual.wasm` (else
`~/.config/rano/scripts/built-in/`), then loaded from there like a plugin you installed by hand.

- It is **never overwritten**: edit it, or replace it with your own build, and that is what runs.
- `scripts/built-in/.rano-version` records what was unpacked and from which release, so an
  upgrade says the shipped one is newer instead of reverting your file.
- `scripts/` is yours: `built-in/` holds the unpacked ones and whatever you put beside it.
- Unpacking writes a file; it does not load one. `C-g m` is what loads the manual.
