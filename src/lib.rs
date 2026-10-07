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
//! - [`diff`] / [`sidediff`] — line diffs drawn as ratatui lines, unified or
//!   in two panels, with [`style`]'s roles and [`highlight`]'s syntax mapping.
//! - [`width`] — display width: how many columns a character takes, where a
//!   line's wrap segments begin, and which clusters may never be split.

pub mod buffer;
// Merge conflicts taken apart: the file resolved ours-way and theirs-way,
// diffed side by side with the renderers below (the editor's M-P).
pub mod conflict;
// Diffs, as ratatui lines: the edit script and the unified view (`diff`), the
// two-panel view (`sidediff`), the roles and palette they paint with (`style`),
// and syntax captures onto those roles (`highlight`). Ported from letibot's
// `crates/ui` so that text rendering has one home; lib-only, and the editor's
// external-change diff reaches them as `rano::diff` / `rano::sidediff`.
pub mod diff;
pub mod encoding;
pub mod highlight;
// Unified diffs read back in: a patch file parsed into files and hunks, drawn
// with the renderers above (the editor's M-P view of a .diff/.patch buffer).
pub mod patch;
pub mod sidediff;
pub mod style;
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
