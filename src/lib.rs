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

pub mod buffer;
pub mod syntax;
