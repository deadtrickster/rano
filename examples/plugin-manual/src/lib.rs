//! rano's manual as a plugin: for the virtual path `rano:manual` it answers
//! `docs/*.md` as one document, anything else is "not mine". The docs are
//! `include_str!`d, so `docs/` stays the single copy and the two cannot drift.

use rano_plugin as rano;

rano::abi!();

/// The path that means "the manual", claimed before anything asks the filesystem.
pub const PATH: &str = "rano:manual";

/// The manual's source, in the order it reads: a doc added to `docs/` has to be
/// added here too, and the test fails if it is not.
const DOCS: &[&str] = &[
    include_str!("../../../docs/configuration.md"),
    include_str!("../../../docs/embedding.md"),
    include_str!("../../../docs/keys.md"),
    include_str!("../../../docs/logs.md"),
    include_str!("../../../docs/pdf.md"),
    include_str!("../../../docs/plugins.md"),
];

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

/// "Should this path open as the manual?" The viewport is ignored: the host's
/// markdown renderer lays the document out.
#[unsafe(no_mangle)]
pub extern "C" fn provider_open(ptr: u32, len: u32, _cols: u32, _rows: u32) -> u32 {
    rano::provider(ptr, len, _cols, _rows, |path, _, _| {
        (path.trim() == PATH).then(manual)
    })
}
