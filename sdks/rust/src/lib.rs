//! **Write a rano plugin in Rust, not in wasm.**
//!
//! The ABI (`plans/S10-wasm-hooks.md`) is four exports and a few raw pointers; this crate is the
//! safe layer over them, so a plugin is a function from `&str` to `String` and nothing else:
//!
//! ```no_run
//! use rano_plugin as rano;
//!
//! rano::abi!();
//!
//! #[unsafe(no_mangle)]
//! pub extern "C" fn on_save(ptr: u32, len: u32) -> u32 {
//!     rano::hook(ptr, len, |text| text.to_uppercase())
//! }
//! ```
//!
//! What the helpers do, so a plugin does not have to: read the input out of linear memory, write
//! the answer back, and write the two-`u32` result block the host reads — including the sentinel
//! that means "leave it alone" when a hook decides it has nothing to say.
//!
//! # What a plugin may reach
//!
//! Nothing, unless the config grants it: [`read_dir`] is the one capability in stage 1, and a
//! module that calls it has the import and is refused at load unless the config says
//! `capabilities = ["read-dir"]`.

#![allow(clippy::missing_safety_doc)]

use std::sync::atomic::{AtomicU32, Ordering};

/// The ABI this crate speaks, and the value [`abi!`]'s `rano_abi` answers.
pub const ABI: i32 = 1;

/// **The sentinel a hook answers when it has nothing to say**: the host leaves the text (or the
/// path) alone.
pub const UNCHANGED: u32 = u32::MAX;

/// The module's memory: the result block at its start, the arena above it. 18 MiB is the host's
/// 16 MiB text cap plus room, and it is BSS — the `.wasm` file does not carry it.
static mut MEM: [u8; 18 << 20] = [0; 18 << 20];

/// Where the arena starts, past the result block (the block is two `u32`s at offset 0).
const ARENA: u32 = 8;

static NEXT: AtomicU32 = AtomicU32::new(ARENA);

/// The address of this module's memory. `addr_of_mut!` rather than `&mut MEM`: edition 2024 has no
/// static-mut references, and a pointer is all this needs.
fn base() -> u32 {
    std::ptr::addr_of_mut!(MEM) as u32
}

/// **`rano_alloc`: reserve `n` bytes and answer their offset.** The host writes a hook's input at
/// the answer; a fresh instance per call means the bump never has to be rewound.
pub fn alloc(n: u32) -> u32 {
    // The arena never grows back, so an overflow is a plugin that asked for more than
    // `ARENA_BYTES` at once — the host's own cap (16 MiB of text) is far below this.
    NEXT.fetch_add(n.max(1), Ordering::Relaxed) + base()
}

/// Where the result block lives: the first bytes of the module's memory. It is deliberately not
/// address 0 — Rust turns a write through a null pointer into `unreachable`, which the host sees
/// as a bare trap.
fn block() -> u32 {
    base()
}

/// **The text a hook was handed**, or `None` when the host's range is outside this module's
/// memory (which cannot happen from a correct host, and is checked rather than trusted).
///
/// # Safety
///
/// `ptr` and `len` come from the host as a range inside this module's memory; the slice lives as
/// long as the call, and the hook's answer is copied out before it returns.
pub unsafe fn input<'a>(ptr: u32, len: u32) -> Option<&'a str> {
    let start = usize::try_from(ptr).ok()?;
    let len = usize::try_from(len).ok()?;
    let end = start.checked_add(len)?;
    if end > (base() as usize) + std::mem::size_of::<[u8; 18 << 20]>() || start < base() as usize {
        return None;
    }
    // SAFETY: the range was checked to be inside this module's own memory, and the host wrote it.
    let bytes = unsafe { std::slice::from_raw_parts(ptr as *const u8, len) };
    std::str::from_utf8(bytes).ok()
}

/// **The answer to a hook**: the block, which tells the host where the text is and how long it is.
pub fn answer(text: &str) -> u32 {
    let at = alloc(text.len() as u32);
    // SAFETY: `at` is an arena offset this module's allocator just handed out, and the block is at
    // a fixed, non-null address inside the same memory; the host reads both after the call.
    unsafe {
        let dst = at as *mut u8;
        std::ptr::copy_nonoverlapping(text.as_ptr(), dst, text.len());
        let b = block() as *mut u32;
        b.write_unaligned(at);
        b.add(1).write_unaligned(text.len() as u32);
    }
    block()
}

/// **One text hook**: `f` is the plugin's whole job — the text in, the text out. `None` means
/// "leave it alone", which is also what an unreadable input gives.
pub fn hook(ptr: u32, len: u32, f: impl FnOnce(&str) -> Option<String>) -> u32 {
    // SAFETY: the host's range, checked by `input`.
    let Some(text) = (unsafe { input(ptr, len) }) else {
        return UNCHANGED;
    };
    match f(text) {
        Some(out) => answer(&out),
        None => UNCHANGED,
    }
}

/// **A provider hook**: the path in, and the viewport it would be drawn at. `None` is "not mine".
pub fn provider(
    ptr: u32,
    len: u32,
    cols: u32,
    rows: u32,
    f: impl FnOnce(&str, u32, u32) -> Option<String>,
) -> u32 {
    // SAFETY: the host's range, checked by `input`.
    let Some(path) = (unsafe { input(ptr, len) }) else {
        return UNCHANGED;
    };
    match f(path, cols, rows) {
        Some(out) => answer(&out),
        None => UNCHANGED,
    }
}

#[link(wasm_import_module = "rano")]
unsafe extern "C" {
    #[link_name = "read_dir"]
    fn host_read_dir(path_ptr: u32, path_len: u32, out_ptr: u32, out_cap: u32) -> i32;
}

/// **The host's listing of `path`** — a folder's text, rendered by the host with the same rules as
/// its own dired (directories first, `name/`), or `None` when the path is not a folder the host can
/// read. Calling this is what gives the module the `read-dir` import, so a plugin that does not
/// want the capability does not have it.
pub fn read_dir(path: &str) -> Option<String> {
    // A listing that does not fit is retried once at the size the host asks for; 1 MiB fits a
    // folder with tens of thousands of entries, and the retry covers the rest.
    let mut cap = 1usize << 20;
    for _ in 0..2 {
        let out = alloc(cap as u32);
        let n = unsafe {
            host_read_dir(path.as_ptr() as u32, path.len() as u32, out, cap as u32)
        };
        if n >= 0 {
            // SAFETY: the host wrote `n` bytes at `out`, inside this module's memory.
            let bytes = unsafe { std::slice::from_raw_parts(out as *const u8, n as usize) };
            return String::from_utf8(bytes.to_vec()).ok();
        }
        if n == i32::MIN {
            return None;
        }
        let needed = n.unsigned_abs() as usize;
        if needed > cap {
            cap = needed;
        } else {
            return None;
        }
    }
    None
}

/// **The two exports every plugin has**: `rano_abi` and `rano_alloc`.
///
/// ```no_run
/// rano_plugin::abi!();
/// ```
#[macro_export]
macro_rules! abi {
    () => {
        #[unsafe(no_mangle)]
        pub extern "C" fn rano_abi() -> i32 {
            $crate::ABI
        }

        #[unsafe(no_mangle)]
        pub extern "C" fn rano_alloc(n: u32) -> u32 {
            $crate::alloc(n)
        }
    };
}
