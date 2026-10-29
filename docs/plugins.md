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
```

A hook returns the offset of two little-endian `u32`s, `{out_ptr, out_len}`, or `0xFFFFFFFF`
for "leave it alone". The hooks are found by name, so a later rano can add one without this
changing. An `on_*` export is a text hook. A `provider_*` export takes the viewport too.

`on_open` runs when a file's text is read. `on_save` runs when a buffer is written.

## What the host guarantees

- 50 million units of fuel and 64 MiB of guest memory per call.
- 16 MiB of text in and out, each way.
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
