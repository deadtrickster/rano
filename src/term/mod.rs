//! The terminal, and nothing about what is drawn on it: raw mode and the row-diffing
//! painter ([`terminal`]), decoded input ([`mod@decode`], [`event`]), the extras the
//! terminal speaks ([`features`]), kitty graphics ([`graphics`]) and hyperlinks
//! ([`links`]), and the picture protocols ([`proto`], [`picture`]). It knows rows
//! of text and bytes, nothing about the editor: `crate::render` builds the rows,
//! this writes them.

pub mod decode;
pub mod event;
pub mod features;
pub mod graphics;
pub mod links;
pub mod picture;
pub mod proto;
pub mod terminal;

pub use decode::{decode, decode_prefix, legacy_bytes};
pub use event::{Event, KeyCode, KeyEvent, Mods, MouseButton, MouseEvent, MouseKind};
pub use features::Features;
pub use picture::{Picture, Pictures};
pub use proto::Proto;
pub use terminal::{Mouse, Options, Progress, Terminal, WriteStats};
