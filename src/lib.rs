//! rano's reusable parts, as a library target.
//!
//! The binary keeps its own module tree under `main.rs`; this root exists so
//! another crate can depend on rano for a piece of it without the editor.
//! Only what is genuinely self-contained is exposed:
//!
//! - [`buffer`] — the text model (lines of `char`, byte offsets).
//! - [`syntax`] — tree-sitter highlighting: language detection, the
//!   capture walk, and [`syntax::Highlighter::classes`] for callers that
//!   own their palette rather than borrowing rano's.
//! - [`width`] — display width: how many columns a character takes, where a
//!   line's wrap segments begin, and which clusters may never be split.

pub mod buffer;
pub mod encoding;
// The row store (§15) is exposed by the LIBRARY target until the editor's
// migration to it lands, which is what §16.3 stage 3 is. It is not declared in
// `main.rs` yet because nothing there reads it, and a module the binary does
// not use is dead code by the compiler's reckoning — correctly.
pub mod rows;
pub mod syntax;
pub mod width;
