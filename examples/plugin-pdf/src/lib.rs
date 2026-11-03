//! A PDF reader as a rano plugin: the host rasterizes pages, this plugin owns the
//! reading. Everything a reader does is in [`reader`], a pure function of the path,
//! tested on the host; a build with neither import still claims a PDF and says why.

use rano_plugin as rano;

pub mod cache;
pub mod host;
pub mod reader;

// `rano_abi` and `rano_alloc`, in the one shape the host expects.
rano::abi!();

/// The most this plugin writes into the SDK's 18 MiB arena in one text
/// answer. A picture is bounded by [`reader::PAGE_BYTES`] and answers in place.
const ARENA: usize = 17 << 20;

/// A result block plus a payload, which is `rano::answer` for bytes. `0xFFFFFFFF`
/// for an answer too big to fit, which the host reads as "not mine" rather than a
/// truncated view.
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

/// What this path should open as: the reader's first view of a PDF, or
/// `0xFFFFFFFF`. The viewport is passed through because a view is laid out at the
/// width it will be drawn at.
#[unsafe(no_mangle)]
pub extern "C" fn provider_open(ptr: u32, len: u32, cols: u32, rows: u32) -> u32 {
    // SAFETY: the host's range, checked by `rano::input`.
    let Some(path) = (unsafe { rano::input(ptr, len) }) else {
        return rano::UNCHANGED;
    };
    // The document as markdown, which the host renders as prose; a build without the
    // text import falls back to the view that says so.
    if let Some(md) = reader::markdown(path, &mut host::Host) {
        return answer(md.as_bytes());
    }
    match reader::view(path, cols as usize, rows as usize, &mut host::Host) {
        Some(text) => answer(text.as_bytes()),
        None => rano::UNCHANGED,
    }
}

/// The room two half-pages are composed in before they become one picture. The page
/// itself is written eight bytes past the block that points at it, so a page at
/// [`reader::PAGE_BYTES`] is one allocation with room to spare.
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

/// A key, answered as the path to open next: the same path with the state fragment
/// the key moves it to, or `0xFFFFFFFF` when the key is not this plugin's (`q`
/// included, because closing a view is the host's).
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
