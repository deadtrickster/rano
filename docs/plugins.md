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
for "leave it alone". Hooks are found by name. An `on_*` export is a text hook, a `provider_*`
export also gets the viewport. Any other export is named `rano_…`. The host checks each hook's
signature at load.

- `on_open` runs when a file's text is read.
- `on_save` runs when a buffer is written. Its answer replaces the buffer as one undo step.
- `provider_open` is asked first when a path is opened, and answers the text it opens as.
- `provider_picture` answers an RGB picture instead (see below).

Per call: 50 million units of fuel, 64 MiB of guest memory, 16 MiB of text each way, and a fresh
instance, so nothing carries over between calls.

A failure costs nothing. No ABI, an ungranted import, a trap, fuel or memory exhausted, an answer
that is not text or is over the cap: the hook is skipped, the open or save goes ahead as without
it, and one line names the plugin and the reason. A binary file never reaches a hook, and a file
above 16 MiB skips it.

## Capabilities

```toml
plugin = "…/rano_plugin_dired.wasm"
capabilities = ["read-dir"]
```

```sh
RANO_PLUGIN=$PWD/examples/plugin-dired/plugin.wasm RANO_CAPABILITIES=read-dir rano .
```

- `read-dir` grants `rano.read_dir(path_ptr, path_len, out_ptr, out_cap) -> i32`: a folder's
  listing, rendered as rano's own listing renders it.
- `read-file` grants `rano.read_file` with the same shape: the bytes of the file the call is
  about, and no other. The path is resolved first, so `..`, relative spellings and symlinks to
  another file are refused. Text hooks have no path, so it refuses there. Files above 16 MiB are
  refused.
- `convert-page` grants the PDF page renderers ([pdf](pdf.md)).

Each answers the bytes written, `-needed` if the buffer is too small, or `i32::MIN` for an
error. A module importing anything ungranted is refused at load, by name. A misspelt capability
grants nothing.

## Providers

`examples/plugin-dired/` answers `provider_open` with a folder's listing:

```ts
export function provider_open(ptr: u32, len: u32, _cols: u32, _rows: u32): u32 {
  const listing = readDir(String.UTF8.decodeUnsafe(ptr as usize, len as usize));
  return listing === null ? UNCHANGED : answer(listing);
}
```

The plugin decides the content. `RET`, `-`, `r` and `q` still work.

`provider_picture` takes the same arguments and answers a picture:

```
[width: u32 LE][height: u32 LE][rgb: width * height * 3]
```

Row-major, top row first, at most 16 MiB. A picture over the cap or disagreeing with its header
is refused, not clipped. A host that draws pictures asks `provider_picture` before
`provider_open`. One that cannot asks only `provider_open`, so a viewer should answer both. A
cell is about twice as tall as wide: return `cols` × `2 * rows` pixels, or squash the
projection. The host scales the picture into the cell rectangle.

`examples/plugin-stl/` is a Zig provider for `*.stl`: a shaded picture, or characters for a
terminal without graphics. It needs `read-file`, and refuses models above 12,000 facets or 1 MiB.

```sh
RANO_PLUGIN=$PWD/examples/plugin-stl/plugin.wasm RANO_CAPABILITIES=read-file rano model.stl
cd examples/plugin-stl && zig build && cp zig-out/bin/plugin.wasm plugin.wasm  # zig 0.16
```

## SDKs

Rust, `rano-plugin` in `sdks/rust/`:

```rust
use rano_plugin as rano;
rano::abi!();

#[unsafe(no_mangle)]
pub extern "C" fn on_save(ptr: u32, len: u32) -> u32 {
    rano::hook(ptr, len, |text| Some(text.to_uppercase()))
}
```

`examples/plugin-upper/` is that plugin, with a built `plugin.wasm`:

```sh
RANO_PLUGIN=$PWD/examples/plugin-upper/plugin.wasm rano notes.txt
```

Rebuild: `rustup target add wasm32-unknown-unknown`, then
`cargo build --release --target wasm32-unknown-unknown` there, and copy the `.wasm` beside it.

AssemblyScript, `sdks/assemblyscript/rano.ts`, example `examples/plugin-upper-as/`:

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

The `--use abort` keeps AssemblyScript from importing `env.abort`, which no capability grants.
Any language that emits wasm32 works.

## The manual

`C-g m` (or `M-x manual`) opens rano's manual, a builtin plugin (`examples/plugin-manual/`) that
answers `provider_open("rano:manual")` with `docs/*.md`. The docs are `include_str!`d, so editing
one without rebuilding the plugin fails a test.

It ships in the binary and is written on first use to
`$XDG_CONFIG_HOME/rano/scripts/built-in/manual.wasm` (else `~/.config/rano/scripts/built-in/`).
It is never overwritten, so a file you edit or replace is what runs.
`scripts/built-in/.rano-version` records what was unpacked, so an upgrade says the shipped one
is newer instead of reverting your file.
