//! **The one place the two host imports are named.**
//!
//! `plans/S10-wasm-hooks.md` §2.3 gives a plugin capabilities as host imports; this plugin needs
//! exactly two, and both are *page renderers* rather than a new power over the filesystem — the
//! host rasterizes the document it was asked about, and the plugin owns the reading:
//!
//! ```wat
//! ;; one page of the file this call is about, rasterized at `dpi`, as PNG bytes
//! (func (import "rano" "convert_page") (param i32 i32) (result i32))
//! ;; one page of it as text, for a terminal with no picture protocol
//! (func (import "rano" "convert_page_text") (param i32) (result i32))
//! ```
//!
//! **The shape of the answer, and the assumption in it.** The brief says `-> packed {ptr, len}`. A
//! wasm function can return several values, but Rust cannot call a multi-value import through
//! `extern "C"` (one return value is all a C signature has), so "packed" has to mean what it means
//! everywhere else in this ABI: **an offset, into this module's own memory, of a pair of
//! little-endian `u32`s** — `{ptr, len}` — with the bytes themselves somewhere the host chose. That
//! is the same structure the result block uses, and the host can write it wherever it likes
//! (including through `rano_alloc`), so nothing on the host side has to change if it disagrees:
//! **this file, and the three functions in it, are the whole wiring point.** An import that turns
//! out to be `(page, dpi, out_ptr) -> i32` is one `extern` line and one `packed` call.
//!
//! **Why the imports are behind features and a target.** A module that imports what the host
//! cannot provide fails to instantiate, and a plugin that cannot load cannot say why. So a build
//! without `page-png`/`page-text` has no imports at all, answers `false` to `can_png`/`can_text`,
//! and [`crate::reader`] turns that into a sentence in the view. The `wasm32` half of each `cfg` is
//! what lets the reader's tests run on the host, where there is no linear memory to point into.

use crate::reader::Render;

/// **The host, as the reader sees it**: two functions, and whether they exist in this build.
pub struct Host;

impl Render for Host {
    fn can_png(&self) -> bool {
        cfg!(all(target_arch = "wasm32", feature = "page-png"))
    }

    fn can_text(&self) -> bool {
        cfg!(all(target_arch = "wasm32", feature = "page-text"))
    }

    fn page_png(&mut self, page: u32, dpi: u32) -> Option<Vec<u8>> {
        #[cfg(all(target_arch = "wasm32", feature = "page-png"))]
        {
            return packed(unsafe { convert_page(page as i32, dpi as i32) });
        }
        #[cfg(not(all(target_arch = "wasm32", feature = "page-png")))]
        {
            let _ = (page, dpi);
            None
        }
    }

    fn page_text(&mut self, page: u32) -> Option<String> {
        #[cfg(all(target_arch = "wasm32", feature = "page-text"))]
        {
            let bytes = packed(unsafe { convert_page_text(page as i32) })?;
            return String::from_utf8(bytes).ok();
        }
        #[cfg(not(all(target_arch = "wasm32", feature = "page-text")))]
        {
            let _ = page;
            None
        }
    }
}

#[cfg(all(target_arch = "wasm32", feature = "page-png"))]
#[link(wasm_import_module = "rano")]
unsafe extern "C" {
    #[link_name = "convert_page"]
    fn convert_page(page: i32, dpi: i32) -> i32;
}

#[cfg(all(target_arch = "wasm32", feature = "page-text"))]
#[link(wasm_import_module = "rano")]
unsafe extern "C" {
    #[link_name = "convert_page_text"]
    fn convert_page_text(page: i32) -> i32;
}

/// **Read the `{ptr, len}` pair the host answered** out of this module's memory. `None` for the
/// sentinel (`-1`, which is `0xFFFFFFFF` read as signed), for a pair that points outside the
/// memory, and for an empty answer — a page the converter could not make is all three.
#[cfg(all(
    target_arch = "wasm32",
    any(feature = "page-png", feature = "page-text")
))]
fn packed(block: i32) -> Option<Vec<u8>> {
    if block < 0 {
        return None;
    }
    // The module's own memory, in bytes: a page is written into it by the host, and a pair that
    // points past the end is a broken answer rather than a page.
    let bytes = core::arch::wasm32::memory_size::<0>() << 16;
    let at = block as usize;
    if at + 8 > bytes {
        return None;
    }
    // SAFETY: both reads are inside this module's memory (checked above), and the pair is written
    // little-endian by the host.
    let (ptr, len) = unsafe {
        (
            (at as *const u32).read_unaligned() as usize,
            ((at + 4) as *const u32).read_unaligned() as usize,
        )
    };
    if len == 0 || ptr.checked_add(len)? > bytes {
        return None;
    }
    // SAFETY: the range is inside this module's memory, and the host wrote it before returning.
    let answer = unsafe { std::slice::from_raw_parts(ptr as *const u8, len) };
    Some(answer.to_vec())
}
