//! **A folder viewer as a plugin**: rano's `provider_open` hook, plus one capability.
//!
//! On open, rano asks `provider_open` with the path it is about to open. This module checks
//! whether the path is a folder — by asking the host for its listing, which is the only way a
//! plugin can look at anything — and, if it gets one, answers with that text: the file "opens" as
//! its listing. If it gets an error instead (a file, a path that is not there, a permission it
//! does not have), it answers `0xFFFFFFFF` — "not mine" — and rano opens the path the ordinary
//! way.
//!
//! ```sh
//! rustup target add wasm32-unknown-unknown      # once
//! cargo build --release --target wasm32-unknown-unknown
//! ```
//!
//! and in `~/.config/rano/config.toml`:
//!
//! ```toml
//! plugin = "…/rano_plugin_dired.wasm"
//! capabilities = ["read-dir"]
//! ```
//!
//! # The two things this demonstrates
//!
//! - **A provider hook is asked before the editor reads anything**, and its answer *is* the
//!   buffer: a folder keeps the listing keys, so `RET` enters what the cursor is on and `-` goes
//!   up — the plugin decided the content, the editor kept the navigation.
//! - **A capability is an import.** `rano.read_dir(path_ptr, path_len, out_ptr, out_cap) -> i32`
//!   answers the number of bytes written, `-needed` when the output buffer is too small, or
//!   `i32::MIN` when the path is not a folder it can read. The listing's *format* is the host's
//!   (directories first, `name/`), so this module and rano's own listing cannot disagree about
//!   what a row means.

/// The address of this module's memory, taken without making a reference to a static.
static mut MEM: [u8; 2 << 20] = [0; 2 << 20];

fn mem_addr() -> u32 {
    std::ptr::addr_of_mut!(MEM) as u32
}

/// The result block sits at the start of `MEM` — a real address (a null base is the one Rust turns
/// into `unreachable`).
fn block_addr() -> u32 {
    mem_addr()
}

/// Where `rano_alloc` hands memory out: past the block, and past the path rano writes for us.
const ARENA: u32 = 1 << 16;

/// The listing buffer, above the arena: 1 MiB, which is a folder with tens of thousands of
/// entries and well inside the host's 64 MiB cap.
const LISTING: u32 = 1 << 19;
const LISTING_CAP: u32 = 1 << 20;

static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(ARENA);

#[link(wasm_import_module = "rano")]
unsafe extern "C" {
    /// **The host's listing of `path`**: `(path_ptr, path_len, out_ptr, out_cap) -> i32`.
    fn read_dir(path_ptr: u32, path_len: u32, out_ptr: u32, out_cap: u32) -> i32;
}

#[unsafe(no_mangle)]
pub extern "C" fn rano_abi() -> i32 {
    1
}

#[unsafe(no_mangle)]
pub extern "C" fn rano_alloc(n: u32) -> u32 {
    NEXT.fetch_add(n.max(1), std::sync::atomic::Ordering::Relaxed) + mem_addr()
}

fn answer(out: &[u8]) -> u32 {
    // SAFETY: `out` lives in `MEM`, whose address is inside this module's memory; two stores at a
    // known offset, and the host reads them after the call returns.
    unsafe {
        let block = block_addr() as *mut u32;
        block.write_unaligned(mem_addr() + LISTING);
        block.add(1).write_unaligned(out.len() as u32);
    }
    block_addr()
}

/// **"Should this path open as a listing?"** — asked with the path before rano reads anything, and
/// with the viewport (`cols`×`rows`) it would be drawn at. A listing needs no columns, so the size
/// is ignored here; a plugin that lays out a table or a chart uses it.
#[unsafe(no_mangle)]
pub extern "C" fn provider_open(path_ptr: u32, path_len: u32, _cols: u32, _rows: u32) -> u32 {
    // SAFETY: rano wrote `path_len` bytes at `path_ptr`, both inside this module's memory.
    let n = unsafe { read_dir(path_ptr, path_len, mem_addr() + LISTING, LISTING_CAP) };
    if n < 0 {
        // Not a folder it can read (or a listing bigger than the buffer): not ours, open it the
        // ordinary way. `-1` is the ABI's "leave it alone".
        return u32::MAX;
    }
    let out = n as usize;
    // SAFETY: the host just wrote `out` bytes at that address, inside our memory.
    let bytes = unsafe { std::slice::from_raw_parts((mem_addr() + LISTING) as *const u8, out) };
    answer(bytes)
}
