//! **The terminal, and nothing about what is drawn on it.**
//!
//! Ported from letibot's `crates/tui/src/backend/`, which is where each rule below was paid
//! for: raw mode and the modes a full-screen program turns on, with a restore that survives a
//! panic ([`terminal`]); the row-diffing painter and its [`terminal::WriteStats`]; what the
//! bytes the terminal sends mean ([`mod@decode`]), as rano's own [`event`] type; which of its
//! extras the terminal speaks ([`features`]); and the two protocols drawn through beyond
//! cells — kitty graphics ([`graphics`]) and OSC 8 hyperlinks ([`links`]).
//!
//! Which of the picture protocols the terminal on the other end actually speaks is
//! [`proto`], and the picture a frame draws through it — whichever one it is, with the bytes
//! sent once — is [`picture`].
//!
//! It knows rows of text and bytes, and nothing about the editor or a session. The dependency
//! runs one way: `crate::render` builds the rows, this writes them.

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
