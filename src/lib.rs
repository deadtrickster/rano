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
// The row store (§15, §16.3). It is declared in `main.rs` too now: the
// binary's `Buffer` is built on `Rows`, which is stage A of that migration.
pub mod rows;
pub mod syntax;
// The version check and self-update. lib-only: the binary reads it, but so
// could an embedder that wants to offer the same thing.
pub mod update;
// Likewise lib-only for now: the editor has no todo UI yet, and the first
// consumer is leticl embedding this — which is why it must NOT live behind
// `main.rs`.
pub mod todo;
pub mod width;
