// **An example rano plugin in TypeScript (AssemblyScript)**: uppercases a file's text as it is
// opened and as it is saved — the same plugin as `examples/plugin-upper`, in another language.
//
// ```sh
// npm i -g assemblyscript        # once
// asc index.ts -o plugin.wasm --optimize
// ```
//
// then `plugin = "…/plugin-upper-as/plugin.wasm"`, or `RANO_PLUGIN=… rano notes.txt`.
//
// The ABI's pointers, allocator and result block are the SDK's; this file is the plugin.

import {
  answer,
  input,
  rano_abi_impl,
  rano_alloc_impl,
} from "../../sdks/assemblyscript/rano";

export function rano_abi(): i32 {
  return rano_abi_impl();
}

export function rano_alloc(n: u32): u32 {
  return rano_alloc_impl(n);
}

export function on_open(ptr: u32, len: u32): u32 {
  return answer(input(ptr as usize, len as usize).toUpperCase());
}

export function on_save(ptr: u32, len: u32): u32 {
  return answer(input(ptr as usize, len as usize).toUpperCase());
}

// **The one thing AssemblyScript would otherwise import.** Its runtime calls `env.abort` on a
// panic, and a rano plugin may import nothing it was not granted — so the example supplies its own
// abort (an export, not an import) and the build points at it:
// `asc index.ts -o plugin.wasm --optimize --use abort=index/rano_abort`.
// (The name matters: an export starting with `on_` is a HOOK by the ABI's convention, and the host
// would try to call it with a hook's signature. `rano_` is the prefix for everything else.)
export function rano_abort(
  _message: string | null,
  _fileName: string | null,
  _line: u32,
  _column: u32,
): void {}
