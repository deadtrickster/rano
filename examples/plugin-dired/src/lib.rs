//! **A folder viewer as a rano plugin, in Rust**: on open, checks whether the path is a folder and,
//! if it is, answers with its listing — the file "opens" as its contents.
//!
//! It needs one capability, because looking at the filesystem is not something a plugin may do
//! unasked:
//!
//! ```toml
//! plugin = "…/rano_plugin_dired.wasm"
//! capabilities = ["read-dir"]
//! ```
//!
//! `rano_plugin::read_dir` is the import: it answers the host's own rendering of the folder's
//! text (directories first, `name/`), or `None` when the path is not a folder — which becomes the
//! "not mine" sentinel, and rano opens the path the ordinary way.

use rano_plugin as rano;

rano::abi!();

/// **"Should this path open as a listing?"** — asked with the path and the viewport it would be
/// drawn at. A listing needs no columns, so the size is ignored here; a table or a chart would use
/// it to stay adaptive.
#[unsafe(no_mangle)]
pub extern "C" fn provider_open(ptr: u32, len: u32, _cols: u32, _rows: u32) -> u32 {
    rano::provider(ptr, len, _cols, _rows, |path, _cols, _rows| rano::read_dir(path))
}
