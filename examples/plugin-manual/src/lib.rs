//! **rano's manual as a plugin**: asked for the virtual path `rano:manual`, it answers with
//! `docs/*.md` as one document; asked for anything else it says "not mine" and the path opens
//! the ordinary way.
//!
//! The docs are `include_str!`d, so `docs/` stays the single copy of the manual and the two
//! cannot drift: a doc edited without a rebuild fails `the_manual_is_the_docs`.
//!
//! It ships **in the binary** (`src/builtin.rs`, plans/S10 §2.4): unpacked to
//! `…/rano/scripts/built-in/manual.wasm` on first use and loaded from there, so the reader can
//! read, edit or replace it like any other plugin.
//!
//! ```sh
//! rustup target add wasm32-unknown-unknown
//! cargo build --release --target wasm32-unknown-unknown
//! cp target/wasm32-unknown-unknown/release/rano_plugin_manual.wasm plugin.wasm
//! ```

use rano_plugin as rano;

rano::abi!();

/// The path that means "the manual". Not a file: `provider_open` claims it before anything asks
/// the filesystem, and nothing else may.
pub const PATH: &str = "rano:manual";

/// **The manual's source**, in the order it reads: one entry per `docs/*.md`, named by its
/// heading. A doc added to `docs/` has to be added here too — the test says so by failing.
const DOCS: &[&str] = &[
    include_str!("../../../docs/configuration.md"),
    include_str!("../../../docs/embedding.md"),
    include_str!("../../../docs/keys.md"),
    include_str!("../../../docs/logs.md"),
    include_str!("../../../docs/plugins.md"),
];

/// The docs as one document: each file whole, ending in exactly one newline, a blank line
/// between them. `the_manual_is_the_docs` builds the same text from the files on disk.
fn manual() -> String {
    let mut out = String::new();
    for (i, doc) in DOCS.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        out.push_str(doc.trim_end_matches('\n'));
        out.push('\n');
    }
    out
}

/// **"Should this path open as the manual?"** The viewport is ignored: a document is laid out
/// by the host's markdown renderer at the width it is drawn at, not by the plugin.
#[unsafe(no_mangle)]
pub extern "C" fn provider_open(ptr: u32, len: u32, _cols: u32, _rows: u32) -> u32 {
    rano::provider(ptr, len, _cols, _rows, |path, _, _| {
        (path.trim() == PATH).then(manual)
    })
}
