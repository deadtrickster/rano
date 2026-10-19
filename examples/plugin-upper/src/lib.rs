//! **An example rano plugin**: uppercases a file's text as it is opened and as it is saved.
//!
//! It is a `cdylib` for `wasm32-unknown-unknown` and nothing else — no imports, no state between
//! calls, no allocator the host has to know about. Build it with
//!
//! ```sh
//! rustup target add wasm32-unknown-unknown      # once
//! cargo build --release --target wasm32-unknown-unknown
//! # target/wasm32-unknown-unknown/release/rano_plugin_upper.wasm
//! ```
//!
//! then point rano at the file: `plugin = "…/rano_plugin_upper.wasm"` in
//! `~/.config/rano/config.toml`, or `RANO_PLUGIN=… rano notes.txt` for one run.
//!
//! # The ABI, as this file spells it
//!
//! - `rano_abi() -> 1` — the version rano refuses to guess about.
//! - `rano_alloc(n) -> ptr` — a bump allocator over one static arena. rano writes the text there
//!   before every call; there is no `free`, because a fresh instance is made per call.
//! - `on_open(ptr, len) -> block` and `on_save(ptr, len) -> block` — the text is at `ptr..ptr+len`,
//!   and the answer is the address of two little-endian `u32`s, `{out_ptr, out_len}`. Here the
//!   bytes are uppercased *in place* and the block points back at the input; `0xFFFF_FFFF` would
//!   mean "leave it alone".

use std::sync::atomic::{AtomicU32, Ordering};

/// The module's memory: the result block lives at its start, the allocator hands out the rest.
/// 2 MiB is room for the block plus the 16 MiB text cap's worth of chunks, and the host's own cap
/// (64 MiB) is the real bound.
static mut MEM: [u8; 2 << 20] = [0; 2 << 20];

/// Where the result block lives: the first bytes of `MEM`, which is an ordinary address (a null
/// pointer is the one base Rust turns into `unreachable`, so *this* is the one to use). The hook
/// answers this offset and the host reads two `u32`s there.
fn block_addr() -> u32 {
    mem_addr()
}
/// Where the arena begins inside `MEM`: past the block's page, so a 16 MiB text and the block
/// cannot collide.
const ARENA: u32 = 1 << 20;

static NEXT: AtomicU32 = AtomicU32::new(ARENA);

/// The address of this module's memory, taken without ever making a reference to the static (the
/// one way edition 2024 allows reaching a `static mut`).
fn mem_addr() -> u32 {
    // `addr_of_mut!` yields a pointer, not a reference — the one way edition 2024 allows reaching
    // a `static mut`; wasm is single-threaded and the host reads the memory only between calls.
    std::ptr::addr_of_mut!(MEM) as u32
}

/// **The ABI version.** Anything but 1 is a module this host will not run.
#[unsafe(no_mangle)]
pub extern "C" fn rano_abi() -> i32 {
    1
}

/// **Reserve `n` bytes and answer the offset.** A bump allocator over the static arena: the host
/// writes the input there, and a fresh instance per call means the bump never has to be rewound.
#[unsafe(no_mangle)]
pub extern "C" fn rano_alloc(n: u32) -> u32 {
    // Offsets are **linear-memory addresses**, so the arena's own address is added: the host
    // writes the text at the answer and the hook reads it from the same number.
    NEXT.fetch_add(n.max(1), Ordering::Relaxed)
        .checked_add(mem_addr())
        .expect("the arena is inside this module's memory")
}

/// Uppercase the ASCII letters at `ptr..ptr + len` and answer the block that names them.
fn upper(ptr: u32, len: u32) -> u32 {
    // SAFETY: rano passes a pointer it got from `rano_alloc` and a length it wrote there, so the
    // range is inside this module's memory and is not aliased by anything else here.
    let bytes = unsafe { std::slice::from_raw_parts_mut(ptr as *mut u8, len as usize) };
    for b in bytes.iter_mut() {
        *b = b.to_ascii_uppercase();
    }
    // SAFETY: two unaligned-free `u32` stores at a fixed offset inside our own memory, before the
    // arena; the host reads them after the call returns.
    unsafe {
        let block = block_addr() as *mut u32;
        block.write_unaligned(ptr);
        block.add(1).write_unaligned(len);
    }
    block_addr()
}

/// **As a file opens**: the text uppercase.
#[unsafe(no_mangle)]
pub extern "C" fn on_open(ptr: u32, len: u32) -> u32 {
    upper(ptr, len)
}

/// **As a buffer is saved**: the text uppercase, and the buffer becomes it (one undo step).
#[unsafe(no_mangle)]
pub extern "C" fn on_save(ptr: u32, len: u32) -> u32 {
    upper(ptr, len)
}
