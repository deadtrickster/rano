//! Write a rano plugin in Rust, not in wasm: the ABI is four exports and a few raw pointers, and
//! this crate is the safe layer over them, so a plugin is a function from `&str` to `String`. A
//! plugin may reach nothing unless the config grants it.

#![allow(clippy::missing_safety_doc)]

use std::sync::atomic::{AtomicU32, Ordering};

#[cfg(feature = "json")]
use serde::Serialize;
#[cfg(feature = "json")]
use serde::de::DeserializeOwned;

/// The ABI this crate speaks, and the value [`abi!`]'s `rano_abi` answers.
pub const ABI: i32 = 1;

/// The sentinel a hook answers when it has nothing to say: the host leaves the text (or the path)
/// alone.
pub const UNCHANGED: u32 = u32::MAX;

/// The module's memory: the result block at its start, the arena above it
/// (BSS — the `.wasm` file does not carry it).
static mut MEM: [u8; 18 << 20] = [0; 18 << 20];

/// Where the arena starts, past the result block (the block is two `u32`s at offset 0).
const ARENA: u32 = 8;

/// What the arena holds, in bytes: the static's size less the block.
const ARENA_BYTES: u32 = (18 << 20) - ARENA;

static NEXT: AtomicU32 = AtomicU32::new(ARENA);

/// The address of this module's memory — `addr_of_mut!` because edition 2024
/// has no static-mut references.
fn base() -> u32 {
    std::ptr::addr_of_mut!(MEM) as u32
}

/// **`rano_alloc`: reserve `n` bytes and answer their offset.** A fresh
/// instance per call means the bump never has to be rewound.
///
/// Past the arena it panics instead of walking off it: a store past the end is
/// a wasm trap either way, and this one names itself.
pub fn alloc(n: u32) -> u32 {
    let at = NEXT.fetch_add(n.max(1), Ordering::Relaxed) + base();
    if at.saturating_add(n) > base() + ARENA_BYTES {
        panic!("rano: alloc past the plugin's arena");
    }
    at
}

/// Where the result block lives: the first bytes of memory, deliberately not
/// address 0 — a write through a null pointer traps in Rust.
fn block() -> u32 {
    base()
}

/// The text a hook was handed, or `None` when the host's range is outside this module's memory
/// (checked rather than trusted). Safety: `ptr` and `len` come from the host, as a range inside it.
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

/// The answer to a hook: the block, which tells the host where the text is and how long it is.
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

/// One text hook: `f` is the plugin's whole job — the text in, the text out. `None` means "leave
/// it alone", which is also what an unreadable input gives.
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

/// A JSON hook: the input parsed as `I`, `f`'s answer serialized; `None` is the sentinel.
/// Malformed input is answered, not trapped — it comes back as `{"error": …}` so the host can say
/// why the pane is empty, the one failure a reader cannot act on.
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

/// [`json_hook`] without the pointers: the parse, `f`, and the serialize — separate because this
/// is the half that can be tested natively (the arena's address truncated to a `u32` is the ABI on
/// wasm32 and a wrong address on the host).
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

#[cfg(feature = "json")]
fn error(why: &str) -> u32 {
    answer(&error_text(why))
}

/// `{"error": …}`, escaped by serde rather than by hand: the message quotes the input, and the
/// input may quote anything.
#[cfg(feature = "json")]
fn error_text(why: &str) -> String {
    #[derive(serde::Serialize)]
    struct Error<'a> {
        error: &'a str,
    }
    // Serializing a struct of one `&str` cannot fail; the fallback is here so
    // a plugin never panics into a trap, the one outcome the host cannot report.
    serde_json::to_string(&Error { error: why })
        .unwrap_or_else(|_| "{\"error\":\"unreportable error\"}".to_string())
}

#[link(wasm_import_module = "rano")]
unsafe extern "C" {
    #[link_name = "read_dir"]
    fn host_read_dir(path_ptr: u32, path_len: u32, out_ptr: u32, out_cap: u32) -> i32;
}

/// The host's listing of `path` — a folder's text, rendered by the host with its own dired's
/// rules, or `None` for a non-folder. Calling this is what gives the module the `read-dir` import.
pub fn read_dir(path: &str) -> Option<String> {
    // A listing that does not fit is retried once at the size the host asks for.
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
        // A listing the arena cannot hold is not this plugin's to claim: a retry would
        // allocate past the end and trap, where `None` opens the path the ordinary way.
        if needed > cap && needed <= ARENA_BYTES as usize - (1 << 20) {
            cap = needed;
        } else {
            return None;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_alloc_inside_the_arena_answers_a_usable_offset() {
        let at = alloc(64);
        assert!(at >= base() && at + 64 <= base() + ARENA_BYTES);
    }

    #[test]
    #[should_panic(expected = "past the plugin's arena")]
    fn an_alloc_past_the_arena_panics_rather_than_walking_off_it() {
        // One more than the arena, in one go: the store would be past the end and wasm would
        // trap with no reason at all.
        let _ = alloc(ARENA_BYTES + 1);
    }
}

/// The two exports every plugin has: `rano_abi` and `rano_alloc`.
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

/// A native test of the JSON path — the parse, the serialize and the `{"error"}` escaping. The
/// pointer half is exercised by the host's WAT fixtures, where the arena address is a `u32`.
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

        // Malformed input is an error answered as JSON, not a panic.
        let e = json_answer::<Ask, View>("{\"n\": ", |_| Some(View { doubled: 0 })).unwrap_err();
        assert!(e.contains("malformed input"), "{e}");
    }

    #[test]
    fn an_error_answer_is_json_with_the_message_escaped() {
        // The message is what a reader sees, so it must survive being put in a
        // JSON string.
        let text = error_text("the input said \"no\"");
        let back: serde_json::Value = serde_json::from_str(&text).expect("an error answer is JSON");
        assert_eq!(back["error"], serde_json::json!("the input said \"no\""));
    }
}
