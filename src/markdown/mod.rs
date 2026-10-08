//! Streaming markdown: a model's reply rendered as prose while it is still
//! being written.
//!
//! # Provenance
//!
//! Ported from letibot's `crates/tui/src/ui/markdown.rs` (the streaming window
//! and the block model) and `crates/tui/src/ui/render.rs` (the block painter and
//! its cache), so that every piece of terminal rendering has one home and a host
//! does not carry its own copy. The reasoning in the comments was worked out
//! against letibot's screen and is kept as it was written; a `§n` in them is a
//! section of letibot's design brief (`docs/` in that repository), and "the
//! operator" is the person whose screen found the defect being described.
//!
//! The painter's output is [`crate::render::Line`]s of role-tagged spans: a span
//! says what its text means — a [`crate::style::Role`] plus markdown's inline
//! attributes — and the host's palette decides what that looks like, at the edge.
//! A host drawing cells renders the lines into a [`crate::render::Buffer`];
//! [`line::to_plain`] and [`line::to_ansi`] are the edge for a host that prints
//! strings, and for tests.
//!
//! # The parts
//!
//! - [`parse`] — [`IncrementalMarkdown`], the window that keeps the cost of a
//!   push bounded, and the [`Block`] model it produces; [`lex`] for text rendered
//!   once.
//! - [`line`](mod@line) — the span helpers, and the plain-text and ANSI writers.
//! - [`render`] — one block to rows: [`render_block`], [`render_blocks`].
//! - [`view`] — [`MarkdownView`], a growing document's rows with each settled
//!   block rendered once per width.
//! - [`wrap`] — wrapping and truncating spans by display columns.

pub mod line;
pub mod parse;
pub mod render;
pub mod view;
pub mod wrap;

#[cfg(test)]
pub(crate) mod testing;

pub use line::{STRUCK, base, line_to_ansi, span, to_ansi, to_plain};
pub use parse::{
    Align, Block, DEFAULT_MAX_UNFROZEN, IncrementalMarkdown, InlineStyle, Run, images, lex,
    runs_text, stable_boundary, stable_boundary_with, tail_cut,
};
pub use render::{RenderOptions, render_block, render_blocks, render_bounded};
pub use view::{Decor, MarkdownView};
