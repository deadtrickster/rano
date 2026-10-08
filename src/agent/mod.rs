//! **The agent UI's widgets**: tool calls and their results, the permission card, panes,
//! the header and hint bar, the composer's edges, the asks — drawn on [`crate::render`].
//!
//! # Where these come from, and the one rule they keep
//!
//! Ported from letibot's `crates/tui/src/ui/`, where each widget was an `impl App` method
//! or a free function that read the head's state and returned rows of ANSI strings. The
//! *drawing* moved here; the state did not. So every widget in this module takes a
//! **view model**: a plain struct of the facts it draws, owned by rano and named in rano's
//! words. A host (letibot) maps its own state onto it each frame. rano never depends on a
//! host's crates, and nothing here knows what a session, a transcript item or a daemon is.
//!
//! What letibot's code said about *why* each rule exists — mostly the operator's reports
//! that caused it — is kept on the code that implements the rule, because the rule is
//! only safe to change by someone who has read the report.
//!
//! # Shapes
//!
//! A widget whose height follows its content (a tool row, a card) exposes
//! `fn lines(&self, width: usize) -> Vec<Line>` — the host needs the count to lay out a
//! screen before anything is drawn — and implements [`Widget`] by drawing those lines
//! into the area, clipped. A widget with a fixed height (the hint bar, a box edge) is a
//! `Widget` and may also offer `line(width)`.
//!
//! Roles, never colours: every span names a [`Role`] and the [`crate::style::Palette`] the
//! buffer is emitted under decides what that looks like, so the same widget is colour,
//! light-background or byte-identical plain text without knowing which.
//!
//! # Text a host did not author
//!
//! Every string a view model carries that came from a tool, a model or a daemon is
//! *foreign*. The buffer never writes an escape or a control character from span content
//! into a cell, so nothing here can reconfigure a terminal; [`text::clean`] additionally
//! turns a control character into a space before text is measured, so a `\r` in a target
//! cannot shift a row the wrapper already measured.

use crate::render::{Buffer, Line, Rect, Widget};
#[allow(unused_imports)]
use crate::style::Role;

pub mod card;
pub mod decision;
pub mod outcome;
pub mod text;

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

/// Lines drawn top-down into an area and clipped to it: how every content-driven widget
/// here renders. Rows past the area's height are not drawn; a host that wants a window
/// slices the lines first.
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
    //! What the tests in this module assert with: the reader's text, and the roles a
    //! column was painted in.
    use crate::render::{Line, Role, TestBuffer, Widget};

    /// The plain text of each line, as [`TestBuffer::plain`] would emit it (trailing
    /// blanks trimmed).
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
