//! A folder viewer as a rano plugin: on open it answers a folder's path with its
//! listing, so the file "opens" as its contents. It needs the `read-dir` capability,
//! because looking at the filesystem is not something a plugin may do unasked.

use rano_plugin as rano;

rano::abi!();

/// "Should this path open as a listing?" A listing needs no columns, so the
/// viewport is ignored.
#[unsafe(no_mangle)]
pub extern "C" fn provider_open(ptr: u32, len: u32, _cols: u32, _rows: u32) -> u32 {
    rano::provider(ptr, len, _cols, _rows, |path, _cols, _rows| rano::read_dir(path))
}
