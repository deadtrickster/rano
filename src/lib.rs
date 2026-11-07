//! rano, as a library: the editor itself and the self-contained pieces it is built from. The
//! binary (`main.rs`) is one host of [`editor::Editor`] among others — it parses arguments, owns
//! the terminal and runs the loop; a host such as letibot can embed the editor in-process.

// The agent UI's widgets as rano-owned view models drawn on `render`; nothing
// here depends on a host's crates.
pub mod agent;
pub mod buffer;
// Scripts that ship in the binary and unpack into the reader's folder, so a
// builtin is a file they can read and replace.
pub mod builtin;
// Merge conflicts taken apart: the file resolved ours-way and theirs-way, diffed side by side.
pub mod conflict;
// Diffs as `render` lines: the edit script and unified view, the two-panel view, the roles they
// paint with, and syntax captures onto those roles.
pub mod diff;
pub mod dirview;
/// Docker's `json-file` log driver, unwrapped into the container's own text.
pub mod dockerlog;
pub mod encoding;
pub mod highlight;
// A picture read off the disk: the PNG bytes the kitty protocol carries.
pub mod image;
// Keys and keymaps, emacs-shaped: pure data — command names, not functions —
// so a host can stack its own map over the editor's.
pub mod keymap;
// A model's markdown reply, rendered while it streams.
pub mod markdown;
// Unified diffs read back in: a patch parsed into files and hunks (the editor's M-P view of a
// .diff/.patch buffer).
pub mod patch;
// A PDF page, read off the disk. The rasterizing is a tool's — `pdftoppm`, `gs`, `pdftotext`.
pub mod pdf;
pub mod plugin;
// The render core: what rano draws with, and what letibot draws through.
pub mod render;
pub mod sidediff;
pub mod style;
pub mod theme;
// A change a host hands the editor to review: the file opened on the change, the change drawn
// over it as a diff view.
pub mod review;
// The row store: the editor's `Buffer` is built on `Rows`.
pub mod rows;
// What the editor sends a host (file, cursor, selection).
pub mod send;
pub mod syntax;
// The terminal: raw mode, the row-diffing painter, input decoding, feature detection, kitty
// graphics and OSC 8 links.
pub mod term;
// The version check and self-update; an embedder could offer the same thing.
pub mod update;
// The TODO.md model; the editor's todo commands drive it, and leticl embeds it without the editor.
pub mod todo;
pub mod width;

// The editor. `editor` and `ui` are the surface a host uses; the rest are the editor's
// controllers — `impl Editor` blocks split by concern — and private.

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
mod linecount;
mod load_ctrl;
mod loader;
pub mod logblocks;
// Log search: pattern-matching a file's BYTES in blocks, forward or backward, interruptible —
// over what the FILE holds, not the buffer. `pub` because it has no caller in the editor yet.
pub mod logsearch;
// The not-rendered half of log mode: a line count, a sparse row → byte index and block
// boundaries, with NO rows kept. `pub` because its caller is a host's log pane.
pub mod logtail;
mod lsp;
mod lsp_ctrl;
mod picker;
mod prompt;
mod search;
mod search_ctrl;
pub mod send_ctrl;
mod todo_ctrl;
pub mod ui;

/// A path with a leading `~` made absolute against `$HOME` — the way every prompt reads one, and
/// what a host needs when it hands rano a path a person typed.
pub use prompt::expand_tilde;
mod update_ctrl;

#[cfg(test)]
mod bench;
#[cfg(test)]
mod ed_tests;
#[cfg(test)]
mod timing;
