//! **A PDF reader as a rano plugin.** The host rasterizes pages; this plugin
//! owns the reading: `provider_open` answers a view with its page, zoom and
//! keys, `provider_picture` answers the page as `[width u32 LE][height u32
//! LE][rgb…]`, and `on_key` answers the next path. Everything a reader does is
//! in [`reader`], a pure function of the path, tested on the host.
//!
//! ```sh
//! rustup target add wasm32-unknown-unknown          # once
//! cargo build --release --target wasm32-unknown-unknown --features page-picture,page-text
//! cp target/wasm32-unknown-unknown/release/rano_plugin_pdf.wasm plugin.wasm
//! ```
//!
//! The features stay because a host that does not grant `convert-page`
//! refuses a module that imports them: a build without them still claims a PDF
//! and answers a sentence naming what the host lacks. `rano-plugin` owns the
//! arena and pointers; this crate adds the one thing it does not have, an
//! answer that is bytes rather than text.

use rano_plugin as rano;

pub mod cache;
pub mod host;
pub mod reader;

// `rano_abi` and `rano_alloc`, in the one shape the host expects.
rano::abi!();

/// The most this plugin writes into the SDK's 18 MiB arena in one text
/// answer. A picture is bounded by [`reader::PAGE_BYTES`] and answers in place.
const ARENA: usize = 17 << 20;

/// **A result block plus a payload** — `rano::answer` for bytes. `0xFFFFFFFF`
/// for an answer too big to fit, which the host reads as "not mine" rather
/// than a truncated view.
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

/// **What should this path open as?** The reader's first view of a PDF, or
/// `0xFFFFFFFF` for anything else. The viewport is passed through because a
/// view is laid out at the width it will be drawn at.
#[unsafe(no_mangle)]
pub extern "C" fn provider_open(ptr: u32, len: u32, cols: u32, rows: u32) -> u32 {
    // SAFETY: the host's range, checked by `rano::input`.
    let Some(path) = (unsafe { rano::input(ptr, len) }) else {
        return rano::UNCHANGED;
    };
    // **The document as markdown**, which the host renders as prose; a build
    // without the text import falls back to the view that says so.
    if let Some(md) = reader::markdown(path, &mut host::Host) {
        return answer(md.as_bytes());
    }
    match reader::view(path, cols as usize, rows as usize, &mut host::Host) {
        Some(text) => answer(text.as_bytes()),
        None => rano::UNCHANGED,
    }
}

/// **The page the path names, as pixels**, or `0xFFFFFFFF` when there is no
/// picture to draw.
///
/// **The page is written where this answers it**: the block goes in the arena,
/// the buffer the host fills starts eight bytes past it, and the block points
/// at those very bytes — one allocation, no copy, and a page as large as
/// [`reader::PAGE_BYTES`] fits an 18 MiB arena with room to spare.
/// The room two half-pages are composed in before they become one picture.
const SCRATCH_BYTES: u32 = 2 << 20;

#[unsafe(no_mangle)]
pub extern "C" fn provider_picture(ptr: u32, len: u32, cols: u32, rows: u32) -> u32 {
    // SAFETY: the host's range, checked by `rano::input`.
    let Some(path) = (unsafe { rano::input(ptr, len) }) else {
        return rano::UNCHANGED;
    };
    // The page's own bytes, and the scratch a spread is composed in.
    let at = rano::alloc(8 + reader::PAGE_BYTES as u32);
    // SAFETY: `at + 8 .. at + 8 + PAGE_BYTES` is arena this module's allocator
    // just handed out, and the host writes into it only within the capacity it
    // is given.
    let out = unsafe { core::slice::from_raw_parts_mut((at + 8) as *mut u8, reader::PAGE_BYTES) };
    let spare = rano::alloc(SCRATCH_BYTES);
    // SAFETY: a second arena block this module's allocator just handed out. An
    // allocator that could not find room answers a sentinel, and a spread then
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

/// **A key, answered as the path to open next** — the input is `"<path>\n<key>"`,
/// the answer the same path with the state fragment the key moves it to.
/// `0xFFFFFFFF` when the key is not this plugin's (which includes `q`:
/// closing a view is the host's). One host call site turns the seven keys
/// into a reader, with no new ABI.
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
