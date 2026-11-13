//! Streaming markdown: a model's reply rendered as prose while it is still
//! being written.
//!
//! The painter's output is [`crate::render::Line`]s of role-tagged spans: a
//! span says what its text means and the host's palette decides what that
//! looks like, at the edge. A host drawing cells renders the lines into a
//! [`crate::render::Buffer`]; [`line::to_plain`] and [`line::to_ansi`] are
//! the edge for a host that prints strings, and for tests.
//!
//! The parts: [`parse`] (the [`IncrementalMarkdown`] window and the
//! [`Block`] model; [`lex`] for one-shot text), [`line`](mod@line) (span
//! helpers and the writers), [`render`] ([`render_block`],
//! [`render_blocks`]), [`view`] ([`MarkdownView`]) and [`wrap`] (wrapping
//! and truncating by display columns).

pub mod line;
pub mod parse;
pub mod render;
pub mod view;
pub mod wrap;

#[cfg(test)]
pub(crate) mod testing;

pub use line::{STRUCK, base, line_to_ansi, span, to_ansi, to_plain};
pub use parse::{
    Align, Block, DEFAULT_MAX_UNFROZEN, IncrementalMarkdown, InlineStyle, Run, code_runs, images,
    lex, runs_text, stable_boundary, stable_boundary_with, tail_cut,
};
pub use render::{RenderOptions, citation, render_block, render_blocks, render_bounded};
pub use view::{Decor, MarkdownView};
