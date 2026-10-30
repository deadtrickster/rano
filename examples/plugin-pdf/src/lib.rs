//! **A PDF reader as a rano plugin.**
//!
//! The host rasterizes pages; this plugin owns the reading. A `.pdf` opens through
//! `provider_open` as a view with its page, its zoom and its keys, `provider_picture` answers the
//! page as `[width u32 LE][height u32 LE][rgb…]` for the host to draw, and `on_key` answers the
//! next path for a key the reader pressed. Everything a reader does — page forward and back, a
//! page number, a zoom that re-renders at a different dpi, and a text mode for terminals that
//! cannot draw — is in [`reader`], which is a pure function of the path and is tested on the host.
//!
//! ```sh
//! rustup target add wasm32-unknown-unknown          # once
//! cargo build --release --target wasm32-unknown-unknown --features page-picture,page-text
//! cp target/wasm32-unknown-unknown/release/rano_plugin_pdf.wasm plugin.wasm
//! ```
//!
//! `plugin.wasm` beside this file is built **with** those features, because `src/plugin.rs` answers
//! both imports when the config grants `capabilities = ["convert-page"]`. The features stay because
//! a host that does not grant it refuses a module that imports them at load: a build without them
//! still claims a PDF and answers it with a sentence naming what the host lacks, which is a view
//! the reader can act on rather than a binary file full of noise.
//!
//! The ABI is the one `plans/S10-wasm-hooks.md` §2.1 defines: `rano_abi`, `rano_alloc`, hooks found
//! by name, and a hook answers the offset of `{out_ptr, out_len}` (or `0xFFFFFFFF` for "not mine").
//! `rano-plugin` (`sdks/rust`) owns the arena and the pointers; this crate adds the one thing it
//! does not have, an answer that is bytes rather than text, because a picture is not a `&str`.

use rano_plugin as rano;

pub mod cache;
pub mod host;
pub mod reader;

// `rano_abi` and `rano_alloc`, in the one shape the host expects.
rano::abi!();

/// The most this plugin writes into the SDK's 18 MiB arena in one text answer: the block plus the
/// payload. A picture is bounded by [`reader::PAGE_BYTES`] and answers in place, so it does not go
/// through this.
const ARENA: usize = 17 << 20;

/// **A result block plus a payload**, which is `rano::answer` for bytes: the block is two
/// little-endian `u32`s (`{out_ptr, out_len}`) at an offset the host reads, and the payload follows
/// it in the arena. `0xFFFFFFFF` for an answer too big to fit, which the host reads as "not mine"
/// rather than as a truncated view.
fn answer(bytes: &[u8]) -> u32 {
    let len = bytes.len();
    if len == 0 || len + 8 > ARENA {
        return rano::UNCHANGED;
    }
    let at = rano::alloc(len as u32 + 8);
    // SAFETY: `at` is an arena offset this module's allocator just handed out, `at + 8 + len` is
    // inside the arena, and the host reads both after the call returns.
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), (at + 8) as *mut u8, len);
        std::ptr::write_unaligned(at as *mut u32, (at + 8).to_le());
        std::ptr::write_unaligned((at + 4) as *mut u32, (len as u32).to_le());
    }
    at
}

/// **What should this path open as?** The reader's first view of a PDF, or `0xFFFFFFFF` for
/// anything else, which rano then opens the ordinary way. The viewport is passed through because a
/// plugin view is laid out at the width it will be drawn at.
#[unsafe(no_mangle)]
pub extern "C" fn provider_open(ptr: u32, len: u32, cols: u32, rows: u32) -> u32 {
    // SAFETY: the host's range, checked by `rano::input`.
    let Some(path) = (unsafe { rano::input(ptr, len) }) else {
        return rano::UNCHANGED;
    };
    // **The document as markdown**, which the host renders as prose; a build without the text
    // import falls back to the view that says so.
    if let Some(md) = reader::markdown(path, &mut host::Host) {
        return answer(md.as_bytes());
    }
    match reader::view(path, cols as usize, rows as usize, &mut host::Host) {
        Some(text) => answer(text.as_bytes()),
        None => rano::UNCHANGED,
    }
}

/// **The page the path names, as pixels** for the host to draw in the terminal, or `0xFFFFFFFF`
/// when there is no picture to draw: not a PDF, text mode, a build with no rasterizer, or a page
/// the host did not answer.
///
/// **The page is written where this answers it.** The block goes in the arena, the buffer the host
/// fills starts eight bytes past it, and the block is then made to point at those very bytes: one
/// allocation, no copy, and a page as large as the ABI's cap ([`reader::PAGE_BYTES`]) fits an 18
/// MiB arena with room to spare. A `Vec` in between would need a second and a third copy of the
/// same megabyte, which is why there is not one.
/// The room two half-pages are composed in before they become one picture: the arena has 18 MiB,
/// a page may take 16, and a page at the reader's dpi in half a pane is a few hundred KiB.
const SCRATCH_BYTES: u32 = 2 << 20;

#[unsafe(no_mangle)]
pub extern "C" fn provider_picture(ptr: u32, len: u32, cols: u32, rows: u32) -> u32 {
    // SAFETY: the host's range, checked by `rano::input`.
    let Some(path) = (unsafe { rano::input(ptr, len) }) else {
        return rano::UNCHANGED;
    };
    // The page's own bytes, and **the scratch a spread is composed in**: the arena has 18 MiB,
    // the page may take 16, and the rest is what two half-pages need to be built before they
    // become one picture.
    let at = rano::alloc(8 + reader::PAGE_BYTES as u32);
    // SAFETY: `at + 8 .. at + 8 + PAGE_BYTES` is the arena this module's allocator just handed
    // out — the SDK's 18 MiB, of which the page takes 16 MiB and the path above it a few bytes —
    // and the host writes into it only within the capacity it is given.
    let out = unsafe { core::slice::from_raw_parts_mut((at + 8) as *mut u8, reader::PAGE_BYTES) };
    let spare = rano::alloc(SCRATCH_BYTES);
    // SAFETY: a second arena block this module's allocator just handed out, and the reader writes
    // only inside it. An allocator that could not find room answers a sentinel, and a spread then
    // falls back to one page rather than writing anywhere.
    let scratch: &mut [u8] = if spare == u32::MAX {
        &mut []
    } else {
        unsafe { core::slice::from_raw_parts_mut(spare as *mut u8, SCRATCH_BYTES as usize) }
    };
    let Some(len) = reader::picture(
        path,
        cols as usize,
        rows as usize,
        &mut host::Host,
        out,
        scratch,
    ) else {
        return rano::UNCHANGED;
    };
    // SAFETY: the block sits at `at`, inside the same allocation, and the host reads both after
    // the call returns.
    unsafe {
        std::ptr::write_unaligned(at as *mut u32, (at + 8).to_le());
        std::ptr::write_unaligned((at + 4) as *mut u32, (len as u32).to_le());
    }
    at
}

/// **A key, answered as the path to open next.** The input is `"<path>\n<key>"`; the answer is the
/// same path with the state fragment the key moves it to (`report.pdf#p=4`). `0xFFFFFFFF` when the
/// key is not this plugin's, which includes `q`: closing a view is the host's business.
///
/// This is the hook that makes the hint line real without a new ABI: one host call site
/// (`Plugin::call("on_key", …)` then open what it answers) turns the seven keys into a reader.
#[unsafe(no_mangle)]
pub extern "C" fn on_key(ptr: u32, len: u32) -> u32 {
    // SAFETY: the host's range, checked by `rano::input`.
    let Some(input) = (unsafe { rano::input(ptr, len) }) else {
        return rano::UNCHANGED;
    };
    match reader::key(input) {
        Some(path) => answer(path.as_bytes()),
        None => rano::UNCHANGED,
    }
}
