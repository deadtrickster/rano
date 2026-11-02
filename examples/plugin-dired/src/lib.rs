//! **A folder viewer as a rano plugin**: on open, answers a folder's path with
//! its listing — the file "opens" as its contents. Needs one capability,
//! because looking at the filesystem is not something a plugin may do
//! unasked:
//!
//! ```toml
//! plugin = "…/rano_plugin_dired.wasm"
//! capabilities = ["read-dir"]
//! ```
//!
//! `rano_plugin::read_dir` answers the host's own rendering of the listing
//! (directories first, `name/`), or `None` for a non-folder — which becomes
//! the "not mine" sentinel.

use rano_plugin as rano;

rano::abi!();

/// **"Should this path open as a listing?"** A listing needs no columns, so
/// the viewport is ignored; a table or chart would use it to stay adaptive.
#[unsafe(no_mangle)]
pub extern "C" fn provider_open(ptr: u32, len: u32, _cols: u32, _rows: u32) -> u32 {
    rano::provider(ptr, len, _cols, _rows, |path, _cols, _rows| rano::read_dir(path))
}
