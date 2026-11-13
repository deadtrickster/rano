//! The agent UI's widgets: tool calls and their results, the permission card, panes, the header
//! and hint bar, the composer's edges, the asks — drawn on [`crate::render`].
//!
//! Drawing lives here, state does not: each widget takes a view model — a plain struct of the
//! facts it draws, in rano's words — which a host fills from its own state each frame, so nothing
//! here knows what a session, a transcript item or a daemon is.
//!
//! A widget whose height follows its content exposes `fn lines(&self, width: usize) -> Vec<Line>`
//! and implements [`Widget`] by drawing those lines clipped; a fixed-height one (the hint bar, a
//! box edge) is a `Widget` and may also offer `line(width)`.
//!
//! Roles, never colours: the palette the buffer is emitted under decides how a role looks, so the
//! same widget is colour, light-background or plain without knowing which.
//!
//! Foreign text — anything a tool, a model or a daemon wrote — never carries an escape into a
//! cell, and [`text::clean`] turns a control character into a space before measuring, so a `\r`
//! cannot shift a row the wrapper already measured.

use crate::render::{Buffer, Line, Rect, Widget};
#[allow(unused_imports)]
use crate::style::Role;

pub mod asks;
pub mod blocks;
pub mod card;
pub mod composer;
pub mod config;
pub mod decision;
mod diffbody;
pub mod fit;
pub mod header;
pub mod help;
pub mod hint_bar;
pub mod jobs;
pub mod keys;
pub mod loading;
pub mod logpane;
pub mod monitors;
pub mod notes;
pub mod outcome;
pub mod pane;
pub mod panel;
pub mod picker;
pub mod queue;
pub mod quit;
pub mod screens;
pub mod status;
pub mod subagents;
pub mod text;
pub mod todos;
pub mod tool_row;
pub mod tree_list;
pub mod turn_status;

/// Whether a block shows its body. Letibot's `Fold`: the conversation-wide tool fold
/// (`/t`) and the reasoning fold are each one of these.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Fold {
    #[default]
    Folded,
    Open,
}

impl Fold {
    pub fn is_open(self) -> bool {
        self == Fold::Open
    }
}

/// Lines drawn top-down into an area and clipped to it: how every content-driven widget here
/// renders. A host that wants a window slices the lines first.
pub struct Rows<'a>(pub &'a [Line]);

impl Widget for Rows<'_> {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        draw_lines(self.0, area, buf);
    }
}

/// See [`Rows`].
pub fn draw_lines(lines: &[Line], area: Rect, buf: &mut Buffer) {
    for (i, l) in lines.iter().take(area.height as usize).enumerate() {
        buf.set_line(area.x, area.y + i as u16, l, area.width as usize);
    }
}

/// Implement [`Widget`] for a type with `fn lines(&self, width: usize) -> Vec<Line>`.
#[allow(unused_macros)]
macro_rules! lines_widget {
    ($t:ty) => {
        impl $crate::render::Widget for $t {
            fn render(&self, area: $crate::render::Rect, buf: &mut $crate::render::Buffer) {
                $crate::agent::draw_lines(&self.lines(area.width as usize), area, buf);
            }
        }
    };
}
#[allow(unused_imports)]
pub(crate) use lines_widget;

#[cfg(test)]
#[allow(dead_code)]
pub(crate) mod testing {
    //! What the tests in this module assert with: the reader's text, and the roles a column was
    //! painted in.
    use crate::render::{Line, Role, TestBuffer, Widget};

    /// The plain text of each line, as [`TestBuffer::plain`] would emit it (trailing blanks
    /// trimmed).
    pub fn plain(lines: &[Line]) -> Vec<String> {
        lines
            .iter()
            .map(|l| l.plain().trim_end().to_string())
            .collect()
    }

    /// Draw `w` in a `width`×`height` buffer and read it back as plain rows.
    pub fn drawn(w: &impl Widget, width: u16, height: u16) -> Vec<String> {
        TestBuffer::new(width, height).draw(w).plain()
    }

    /// The topmost role of the span that covers display column `col` of `line`.
    pub fn role_at(line: &Line, col: usize) -> Role {
        let mut at = 0usize;
        for sp in &line.spans {
            let w = sp.width();
            if col < at + w {
                return line.style.patch(&sp.style).top();
            }
            at += w;
        }
        Role::Plain
    }

    /// The role of the first span whose text contains `needle`.
    pub fn role_of(line: &Line, needle: &str) -> Option<Role> {
        line.spans
            .iter()
            .find(|s| s.content.contains(needle))
            .map(|s| line.style.patch(&s.style).top())
    }
}
