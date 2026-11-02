//! **The one place the two host imports are named.** This plugin needs
//! exactly two, both *page renderers* rather than a power over the filesystem
//! — the host rasterizes the document it was asked about, the plugin owns the
//! reading:
//!
//! ```wat
//! ;; one page of the file this call is about, rasterized at `dpi` and written at `out_ptr` as
//! ;; [width u32 LE][height u32 LE][rgb]
//! (func (import "rano" "convert_page") (param i32 i32 i32 i32) (result i32))
//! ;; one page of it as text, for a terminal with no picture protocol
//! (func (import "rano" "convert_page_text") (param i32 i32 i32) (result i32))
//! ```
//!
//! **The answer is a count, not a pointer**: the guest names a buffer it got
//! from `rano_alloc`, the host writes into it and answers the number of bytes
//! — `-needed` when the buffer was too small, `i32::MIN` for a refusal — so
//! the picture is written **where `provider_picture` answers it**: one buffer,
//! no copy.
//!
//! **The imports are behind features and a target** because a module that
//! imports what the host cannot provide fails to instantiate, and a plugin
//! that cannot load cannot say why. A build without `page-picture`/`page-text`
//! has no imports and [`crate::reader`] turns that into a sentence in the
//! view. The `wasm32` half of each `cfg` is what lets the reader's tests run on
//! the host.

use crate::reader::{Answer, Render};

/// **The host, as the reader sees it**: two functions, and whether they exist in this build.
pub struct Host;

impl Render for Host {
    fn can_picture(&self) -> bool {
        cfg!(all(target_arch = "wasm32", feature = "page-picture"))
    }

    fn can_text(&self) -> bool {
        cfg!(all(target_arch = "wasm32", feature = "page-text"))
    }

    /// One page as `[width][height][rgb]`, written into `out` — the caller's
    /// buffer, not a `Vec` from here, because the caller answers with those
    /// very bytes.
    fn page_picture(&mut self, page: u32, dpi: u32, out: &mut [u8]) -> Answer {
        #[cfg(all(target_arch = "wasm32", feature = "page-picture"))]
        {
            // SAFETY: `out` is a buffer in this module's own memory, and its
            // length is the capacity the host is given and stays inside.
            let n = unsafe {
                convert_page(
                    // The host counts pages from zero and the reader's state
                    // is what a person reads, so the conversion happens here.
                    page.saturating_sub(1) as i32,
                    dpi as i32,
                    out.as_mut_ptr() as u32,
                    out.len() as u32,
                )
            };
            answered(n)
        }
        #[cfg(not(all(target_arch = "wasm32", feature = "page-picture")))]
        {
            let _ = (page, dpi, out);
            Answer::Nothing
        }
    }

    /// One page's text, **copied out** of the arena: a page of prose is
    /// kilobytes, and the view that carries it is a `String` anyway.
    fn page_text(&mut self, page: u32) -> Option<String> {
        #[cfg(all(target_arch = "wasm32", feature = "page-text"))]
        {
            // 64 KiB fits a page of prose many times over; a page that does
            // not fit answers `-needed`, and one retry covers the rest.
            let mut cap = 64 << 10;
            for _ in 0..2 {
                let at = rano_plugin::alloc(cap as u32);
                // SAFETY: `at` is an arena offset this module's allocator just
                // handed out and `cap` is how much of it there is, which is all
                // the host is told. **Zero-based here too**: the host counts
                // pages from zero and the reader's state is what a person reads.
                let n = unsafe { convert_page_text(page.saturating_sub(1) as i32, at, cap as u32) };
                if n >= 0 {
                    // SAFETY: the host wrote `n` bytes at `at`, inside this module's memory.
                    let bytes = unsafe { core::slice::from_raw_parts(at as *const u8, n as usize) };
                    return String::from_utf8(bytes.to_vec()).ok();
                }
                if n == i32::MIN {
                    return None;
                }
                let needed = n.unsigned_abs() as usize;
                if needed <= cap {
                    return None;
                }
                cap = needed;
            }
            None
        }
        #[cfg(not(all(target_arch = "wasm32", feature = "page-text")))]
        {
            let _ = page;
            None
        }
    }
}

#[cfg(all(target_arch = "wasm32", feature = "page-picture"))]
#[link(wasm_import_module = "rano")]
unsafe extern "C" {
    #[link_name = "convert_page"]
    fn convert_page(page: i32, dpi: i32, out_ptr: u32, out_cap: u32) -> i32;
}

#[cfg(all(target_arch = "wasm32", feature = "page-text"))]
#[link(wasm_import_module = "rano")]
unsafe extern "C" {
    #[link_name = "convert_page_text"]
    fn convert_page_text(page: i32, out_ptr: u32, out_cap: u32) -> i32;
}

/// **The host's count as one of the three things it can mean.** `i32::MIN` is
/// a refusal; zero is an answer of no bytes, which for a picture is a page
/// with no header and the reader refuses it there.
#[cfg(all(target_arch = "wasm32", feature = "page-picture"))]
fn answered(n: i32) -> Answer {
    if n == i32::MIN {
        Answer::Nothing
    } else if n < 0 {
        Answer::TooBig(n.unsigned_abs() as usize)
    } else {
        Answer::Page(n as usize)
    }
}
