//! An example rano plugin: uppercases a file's text as it is opened and saved. This
//! is the whole plugin — the ABI's memory, allocator and result block are
//! `rano-plugin`'s job, not this file's. Build it for `wasm32-unknown-unknown`.

use rano_plugin as rano;

// `rano_abi` and `rano_alloc`, in the one shape the host expects.
rano::abi!();

/// As a file opens: the text to use.
#[unsafe(no_mangle)]
pub extern "C" fn on_open(ptr: u32, len: u32) -> u32 {
    rano::hook(ptr, len, |text| Some(text.to_uppercase()))
}

/// As a buffer is saved: the same transform, and because it changes the text the
/// buffer becomes it, in one undo step (`M-U` takes it back).
#[unsafe(no_mangle)]
pub extern "C" fn on_save(ptr: u32, len: u32) -> u32 {
    rano::hook(ptr, len, |text| Some(text.to_uppercase()))
}
