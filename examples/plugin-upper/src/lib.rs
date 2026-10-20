//! **An example rano plugin, in Rust**: uppercases a file's text as it is opened and as it is
//! saved. This is the whole plugin — the ABI's memory, allocator and result block are
//! `rano-plugin`'s job, not this file's.
//!
//! ```sh
//! rustup target add wasm32-unknown-unknown      # once
//! cargo build --release --target wasm32-unknown-unknown
//! # target/wasm32-unknown-unknown/release/rano_plugin_upper.wasm
//! ```
//!
//! then `plugin = "…/rano_plugin_upper.wasm"` in `~/.config/rano/config.toml`, or
//! `RANO_PLUGIN=… rano notes.txt` for one run.

use rano_plugin as rano;

// `rano_abi` and `rano_alloc`, in the one shape the host expects.
rano::abi!();

/// **As a file opens.** `Some` is the text to use; `None` would mean "leave it alone" — which is
/// what this plugin answers for a text it has no opinion about.
#[unsafe(no_mangle)]
pub extern "C" fn on_open(ptr: u32, len: u32) -> u32 {
    rano::hook(ptr, len, |text| Some(text.to_uppercase()))
}

/// **As a buffer is saved**: the same transform, and because it changes the text the buffer becomes
/// it — one undo step (`M-U` takes it back).
#[unsafe(no_mangle)]
pub extern "C" fn on_save(ptr: u32, len: u32) -> u32 {
    rano::hook(ptr, len, |text| Some(text.to_uppercase()))
}
