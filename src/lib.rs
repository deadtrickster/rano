//! rano, as a library: the editor itself, and the self-contained pieces it is
//! built from.
//!
//! The binary (`main.rs`) is now one host of [`editor::Editor`] among others:
//! it parses arguments, owns the terminal and runs the loop. Everything the
//! editor is — its state, controllers and renderer — lives here, so a host
//! such as letibot can embed it in-process as a pane rather than shelling out
//! to a second program. The pieces another crate may want without the editor
//! stay usable on their own:
//!
//! - [`buffer`] — the text model (lines of `char`, byte offsets).
//! - [`syntax`] — tree-sitter highlighting: language detection, the
//!   capture walk, and [`syntax::Highlighter::classes`] for callers that
//!   own their palette rather than borrowing rano's.
//! - [`diff`] / [`sidediff`] — line diffs drawn as ratatui lines, unified or
//!   in two panels, with [`style`]'s roles and [`highlight`]'s syntax mapping.
//! - [`width`] — display width: how many columns a character takes, where a
//!   line's wrap segments begin, and which clusters may never be split.
//! - [`editor`] / [`ui`] — the editor and its renderer, for a host to embed.
//!
//! Embedding the editor as a pane, in outline:
//!
//! ```no_run
//! use std::path::Path;
//! use std::time::Instant;
//! use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
//! use rano::buffer::Buffer;
//! use rano::editor::{Area, Editor, KeyOutcome};
//! use rano::{config, keymap::Keymap, ui};
//!
//! let mut ed = Editor::new(Buffer::new(), config::load());
//! let mut host = Keymap::new("host");
//! host.bind("C-q", "close-pane"); // not an editor command: handed back
//! ed.push_keymap(host);
//! ed.open_at(Path::new("src/main.rs"), 120, Some(5)).ok();
//! let pane = ratatui::layout::Rect::new(0, 1, 80, 30);
//! ed.set_area(Area::new(pane.x, pane.y, pane.width, pane.height));
//! # let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 31)).unwrap();
//! // Each iteration of the host's loop:
//! if ed.tick(Instant::now()) {
//!     terminal.draw(|f| ui::draw_in(f, pane, &ed)).unwrap();
//! }
//! let wait = ed.next_wakeup(); // poll input for at most this long
//! # let _ = wait;
//! match ed.handle_key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::CONTROL)) {
//!     KeyOutcome::Host(cmd) if cmd == "close-pane" => { /* close the pane */ }
//!     KeyOutcome::Unhandled => { /* try the host's own bindings */ }
//!     _ => {}
//! }
//! if ed.wants_quit() { /* ^X: close the pane */ }
//! ```

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
// Keys and keymaps, emacs-shaped: chords, prefix maps, a stack of active maps
// (mode over global), where-is. Pure data — command names, not functions — so
// a host can stack its own map over the editor's.
pub mod keymap;
// Unified diffs read back in: a patch file parsed into files and hunks, drawn
// with the renderers above (the editor's M-P view of a .diff/.patch buffer).
pub mod patch;
pub mod sidediff;
pub mod style;
// The row store (§15, §16.3): the editor's `Buffer` is built on `Rows`, which
// is stage A of that migration.
pub mod rows;
// What the editor sends a host (file, cursor, selection): the shared type for
// `Editor::on_send` and the binary's `send_command`.
pub mod send;
pub mod syntax;
// The version check and self-update. lib-only: the binary reads it, but so
// could an embedder that wants to offer the same thing.
pub mod update;
// The TODO.md model; the editor's todo commands (`todo_ctrl`) drive it, and
// leticl embeds it without the editor.
pub mod todo;
pub mod width;

// ---------------------------------------------------------------------------
// The editor. These lived under `main.rs` until a host needed to embed the
// editor itself rather than run it as a program. `editor` and `ui` are the
// surface a host uses; `config` builds an `Editor`, `export` and `send_ctrl`
// are what the binary (a host too) reaches for. The rest are the editor's
// controllers — `impl Editor` blocks split by concern — and stay private.
// ---------------------------------------------------------------------------

// One open document's state, named `crate::BufferState` by the controllers.
mod bufstate;
pub use bufstate::BufferState;
pub(crate) use bufstate::RowWrap;

mod commands;
pub mod config;
mod diffview;
pub mod editor;
mod exec;
mod exec_ctrl;
pub mod export;
mod help;
// What a host's loop calls between events: `Editor::tick`, `next_wakeup`.
mod host;
mod keys;
mod load_ctrl;
mod loader;
mod lsp;
mod lsp_ctrl;
mod picker;
mod prompt;
mod search;
mod search_ctrl;
pub mod send_ctrl;
mod todo_ctrl;
pub mod ui;
mod update_ctrl;

#[cfg(test)]
mod bench;
#[cfg(test)]
mod ed_tests;
