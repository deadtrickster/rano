//! Styled text, a cell buffer, widgets, and the emitter that turns a buffer into terminal
//! rows. Styles name what text means (a `Role`); the `Palette` decides the look only when a
//! buffer is emitted. Build [`Line`]s of [`Span`]s → render into a [`Buffer`] →
//! [`Buffer::emit`] → `Terminal::draw` writes the rows that changed.

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
