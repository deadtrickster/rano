//! **rano's manual as a plugin**: asked for the virtual path `rano:manual`, it
//! answers with `docs/*.md` as one document; anything else is "not mine".
//! The docs are `include_str!`d, so `docs/` stays the single copy of the
//! manual and the two cannot drift. Ships **in the binary**: unpacked to
//! `…/rano/scripts/built-in/manual.wasm` on first use, so the reader can read,
//! edit or replace it like any other plugin.
//!
//! ```sh
//! rustup target add wasm32-unknown-unknown
//! cargo build --release --target wasm32-unknown-unknown
//! cp target/wasm32-unknown-unknown/release/rano_plugin_manual.wasm plugin.wasm
//! ```

use rano_plugin as rano;

rano::abi!();

/// The path that means "the manual": `provider_open` claims it before
/// anything asks the filesystem.
pub const PATH: &str = "rano:manual";

/// **The manual's source**, in the order it reads. A doc added to `docs/` has
/// to be added here too — the test says so by failing.
const DOCS: &[&str] = &[
    include_str!("../../../docs/configuration.md"),
    include_str!("../../../docs/embedding.md"),
    include_str!("../../../docs/keys.md"),
    include_str!("../../../docs/logs.md"),
    include_str!("../../../docs/pdf.md"),
    include_str!("../../../docs/plugins.md"),
];

/// The docs as one document: each file whole, a blank line between them.
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

/// **"Should this path open as the manual?"** The viewport is ignored: the
/// host's markdown renderer lays the document out.
#[unsafe(no_mangle)]
pub extern "C" fn provider_open(ptr: u32, len: u32, _cols: u32, _rows: u32) -> u32 {
    rano::provider(ptr, len, _cols, _rows, |path, _, _| {
        (path.trim() == PATH).then(manual)
    })
}
