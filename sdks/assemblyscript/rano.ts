// **Write a rano plugin in TypeScript (AssemblyScript), not in wasm.**
//
// The ABI is four exports and a few raw pointers: `rano_abi`, `rano_alloc`, and one function per
// hook, each handed a pointer into linear memory and answering the address of a two-`u32` result
// block. This module is the safe layer over that, so a plugin is a function from `string` to
// `string`:
//
// ```ts
// import { abi, answer, unchanged, input } from "../../sdks/assemblyscript/rano";
//
// abi();   // hmm — see below: the exports have to be written out, AS has no macro.
//
// export function on_save(ptr: u32, len: u32): u32 {
//   return answer(input(ptr, len).toUpperCase());
// }
// ```
//
// `rano_abi` and `rano_alloc` cannot be generated from an import in AssemblyScript, so a plugin
// writes those two one-liners too (they are the `abi()` pair at the top of every example).

/** The ABI version the host wants. */
export const ABI: i32 = 1;

/** "Leave it alone" — what a hook answers when it has nothing to say. */
export const UNCHANGED: u32 = 0xFFFF_FFFF;

/** The result block: two `u32`s in a data segment. **Not address 0** — AssemblyScript puts its
 * shadow stack there, and a write through it corrupts the runtime before the first hook returns. */
const BLOCK: usize = memory.data(8);

/** Reserve `n` bytes and answer their offset: the host writes a hook's input here. */
export function alloc(n: u32): u32 {
  return heap.alloc(n) as u32;
}

/** The text a hook was handed. */
export function input(ptr: usize, len: usize): string {
  return String.UTF8.decodeUnsafe(ptr, len);
}

/** The answer to a hook: the text, and the block that names it. */
export function answer(text: string): u32 {
  const bytes = String.UTF8.encode(text);
  const at = heap.alloc(bytes.byteLength);
  memory.copy(at, changetype<usize>(bytes), bytes.byteLength);
  store<u32>(BLOCK, at as u32);
  store<u32>(BLOCK, bytes.byteLength, 4);
  return BLOCK as u32;
}

/** The two exports every plugin has. Called once at module scope, so the plugin's own file reads
 * `rano_plugin();` and nothing else. It cannot export from here (AS has no macro), so it defines
 * the two functions a plugin then re-exports — see the examples. */
export function rano_abi_impl(): i32 {
  return ABI;
}

export function rano_alloc_impl(n: u32): u32 {
  return alloc(n);
}

// A plugin's file, in full:
//
// ```ts
// import { UNCHANGED, answer, input, rano_abi_impl, rano_alloc_impl } from "../../sdks/assemblyscript/rano";
//
// export function rano_abi(): i32 { return rano_abi_impl(); }
// export function rano_alloc(n: u32): u32 { return rano_alloc_impl(n); }
// export function on_save(ptr: u32, len: u32): u32 { return answer(input(ptr, len).toUpperCase()); }
// ```
