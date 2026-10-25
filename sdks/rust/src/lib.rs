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
//!     rano::hook(ptr, len, |text| Some(text.to_uppercase()))
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
//!
//! # JSON hooks
//!
//! A pane hook (`pane_<name>`) is handed a JSON object and answers one, so the `json` feature adds
//! [`json_hook`]: serde in, serde out, and the malformed-input case answered as the `{"error"}` the
//! host already knows how to report.
//!
//! ```no_run
//! # #[cfg(feature = "json")] {
//! use serde::{Deserialize, Serialize};
//!
//! #[derive(Deserialize)] struct In_ { v: u32 }
//! #[derive(Serialize)] struct Out { title: String }
//!
//! #[unsafe(no_mangle)]
//! pub extern "C" fn pane_x(ptr: u32, len: u32) -> u32 {
//!     rano_plugin::json_hook(ptr, len, |_in: In_| Some(Out { title: "x".into() }))
//! }
//! # }
//! ```

#![allow(clippy::missing_safety_doc)]

use std::sync::atomic::{AtomicU32, Ordering};

#[cfg(feature = "json")]
use serde::Serialize;
#[cfg(feature = "json")]
use serde::de::DeserializeOwned;

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

/// **A JSON hook**: the input parsed as `I`, `f`'s answer serialized. `None` means "leave it
/// alone", the same sentinel every other hook answers.
///
/// **Malformed input is answered, not trapped**: `I` that the host's text does not fit comes back
/// as `{"error": "…"}` — JSON, so the host reads it through the same path as a real view and says
/// why the pane is empty instead of showing a trap. A pane that answers nothing at all is the one
/// failure a reader cannot act on, which is why the absence of `I` is still an answer.
#[cfg(feature = "json")]
pub fn json_hook<I: DeserializeOwned, O: Serialize>(
    ptr: u32,
    len: u32,
    f: impl FnOnce(I) -> Option<O>,
) -> u32 {
    // SAFETY: the host's range, checked by `input`.
    let Some(text) = (unsafe { input(ptr, len) }) else {
        return error("the host's input is not text in this module's memory");
    };
    match json_answer(text, f) {
        Ok(Some(text)) => answer(&text),
        Ok(None) => UNCHANGED,
        Err(why) => error(&why),
    }
}

/// [`json_hook`] without the pointers: the parse, `f`, and the serialize — `Ok(None)` is "leave it
/// alone", `Err` is the reason an `{"error"}` answer will carry.
///
/// It is a separate function because **this is the half that can be tested natively**: the SDK's
/// arena is an 18 MiB static addressed as a `u32` (only true on wasm32), so a test that went
/// through the real pointers would be a test of the host's pointer width, not of the JSON path.
#[cfg(feature = "json")]
fn json_answer<I: DeserializeOwned, O: Serialize>(
    text: &str,
    f: impl FnOnce(I) -> Option<O>,
) -> Result<Option<String>, String> {
    let input: I = serde_json::from_str(text).map_err(|e| format!("malformed input: {e}"))?;
    match f(input) {
        Some(out) => {
            let text = serde_json::to_string(&out)
                .map_err(|e| format!("cannot serialize the answer: {e}"))?;
            Ok(Some(text))
        }
        None => Ok(None),
    }
}

/// The one thing a plugin says when it could not do its job.
#[cfg(feature = "json")]
fn error(why: &str) -> u32 {
    answer(&error_text(why))
}

/// **`{"error": "…"}`, escaped by serde** rather than by hand: the message quotes the input, and
/// the input may quote anything.
#[cfg(feature = "json")]
fn error_text(why: &str) -> String {
    #[derive(serde::Serialize)]
    struct Error<'a> {
        error: &'a str,
    }
    // Serializing a struct of one `&str` cannot fail; the fallback is here so a plugin never panics
    // into a trap, which is the one outcome the host cannot report.
    serde_json::to_string(&Error { error: why })
        .unwrap_or_else(|_| "{\"error\":\"unreportable error\"}".to_string())
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
        let n = unsafe { host_read_dir(path.as_ptr() as u32, path.len() as u32, out, cap as u32) };
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

/// **A native test of the JSON path**: the parse, the serialize, and the `{"error"}` escaping — the
/// three things a pane plugin's correctness actually rests on. What a native test cannot do is go
/// through `json_hook`'s pointers: the arena is an 18 MiB static whose address is truncated to a
/// `u32`, which is the ABI on wasm32 and a wrong address on the host, so that half is exercised by
/// the host's own WAT fixtures instead.
#[cfg(all(test, feature = "json"))]
mod json {
    use super::*;
    use serde::Deserialize;

    #[derive(Deserialize)]
    struct Ask {
        n: u32,
    }

    #[derive(serde::Serialize)]
    struct View {
        doubled: u32,
    }

    #[test]
    fn json_answer_parses_serializes_and_refuses_malformed_input() {
        // The happy path: a typed input in, a typed answer out, through JSON both ways.
        let out = json_answer::<Ask, View>(r#"{"n": 21}"#, |ask| Some(View { doubled: ask.n * 2 }))
            .expect("parses");
        assert_eq!(out.as_deref(), Some(r#"{"doubled":42}"#));

        // A hook with nothing to say answers `None`, which `json_hook` turns into the sentinel.
        assert_eq!(
            json_answer::<Ask, View>(r#"{"n": 1}"#, |_| None).unwrap(),
            None
        );

        // Malformed input is an error the hook answers as JSON, not a panic.
        let e = json_answer::<Ask, View>("{\"n\": ", |_| Some(View { doubled: 0 })).unwrap_err();
        assert!(e.contains("malformed input"), "{e}");
    }

    #[test]
    fn an_error_answer_is_json_with_the_message_escaped() {
        // The message is what a reader sees, so it must survive being put in a JSON string: a
        // quote inside it would otherwise end the string and produce a view the host cannot parse.
        let text = error_text("the input said \"no\"");
        let back: serde_json::Value = serde_json::from_str(&text).expect("an error answer is JSON");
        assert_eq!(back["error"], serde_json::json!("the input said \"no\""));
    }
}
