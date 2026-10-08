//! **rano's render core**: styled text, a cell buffer, widgets, and the emitter that turns
//! a buffer into terminal rows.
//!
//! # Why this exists instead of ratatui
//!
//! rano drew with ratatui, and letibot — which draws through rano — drew with strings of
//! escapes it composed by hand. Neither said what the two of them need, and each need
//! here is a defect one of them paid for:
//!
//! - **Roles, not colours.** A [`Style`] names what text *means* ([`Role`]); a
//!   [`Palette`] decides what that looks like only when a buffer is emitted, so one frame
//!   is colour, light-background or byte-identical plain text (`Palette::None`, for
//!   replays and CI) without anything that built it knowing which.
//! - **Hyperlinks are an attribute.** OSC 8 opens and closes around runs of linked
//!   cells, so a clip or a truncation can never leave a link open across the row.
//! - **Graphics placeholders are cells.** A kitty Unicode placeholder with its row and
//!   column diacritics is one cluster in one cell; the image id (a 24-bit foreground) and
//!   placement id (the underline colour) ride in [`Raw`]. An escape is never written
//!   inside a cluster, so nothing can split one.
//! - **Width is clusters, not chars**: wide characters take a lead and a continuation
//!   cell, a write onto half of a pair blanks the other half, and every row a buffer emits
//!   measures exactly its width — which is what the row-diffing painter in
//!   `crate::term` relies on.
//!
//! # The shape
//!
//! Build [`Line`]s of [`Span`]s → render them (or a [`Widget`]) into an area of a
//! [`Buffer`] → [`Buffer::emit`] the rows under a palette → hand the rows to
//! `crate::term::Terminal::draw`, which writes only the rows that changed.

pub mod buffer;
pub mod emit;
pub mod style;
pub mod text;
pub mod widget;

pub use crate::style::{Palette, Role};
pub use buffer::{Buffer, Cell, Rect};
pub use emit::{TestBuffer, emit_row};
pub use style::{Raw, Style};
pub use text::{Line, Span, Text, ellipsise_left, fit, truncate, wrap};
pub use widget::{Bordered, Paragraph, Widget};
