// **A folder viewer as a rano plugin, in TypeScript (AssemblyScript)** — the same plugin as
// `examples/plugin-dired`, in another language.
//
// On open, rano asks `provider_open` with the path it is about to open. This module asks the host
// for that path's listing (the only way a plugin can look at anything) and answers it — the file
// "opens" as its contents. A path that is not a folder it can read gets the "not mine" sentinel,
// and rano opens it the ordinary way.
//
// ```sh
// npm i -g assemblyscript        # once
// asc index.ts -o plugin.wasm --optimize --use abort=index/rano_abort
// ```
//
// and in `~/.config/rano/config.toml`:
//
// ```toml
// plugin = "…/plugin-dired-as/plugin.wasm"
// capabilities = ["read-dir"]
// ```

import {
  UNCHANGED,
  answer,
  rano_abi_impl,
  rano_alloc_impl,
  readDir,
} from "../../sdks/assemblyscript/rano";

export function rano_abi(): i32 {
  return rano_abi_impl();
}

export function rano_alloc(n: u32): u32 {
  return rano_alloc_impl(n);
}

// "Should this path open as a listing?" — asked with the path and the viewport it would be drawn
// at. A listing needs no columns; a table or a chart would use them.
export function provider_open(
  ptr: u32,
  len: u32,
  _cols: u32,
  _rows: u32,
): u32 {
  const path = String.UTF8.decodeUnsafe(ptr as usize, len as usize);
  const listing = readDir(path);
  return listing === null ? UNCHANGED : answer(listing);
}

// AssemblyScript's runtime would import `env.abort`, and a plugin may import nothing it was not
// granted: this export is what the build points at (`--use abort=index/rano_abort`), and its name
// does not start with `on_`, so it is not mistaken for a hook.
export function rano_abort(
  _message: string | null,
  _fileName: string | null,
  _line: u32,
  _column: u32,
): void {}
