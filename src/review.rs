//! **A change a host hands the editor to review**: the file, and what one edit did to it.
//!
//! A host that watched a file change — an agent's edit, a formatter, a patch it applied —
//! knows two things the editor cannot work out for itself: what the lines were before, and
//! where in the file the change landed. [`Editor::open_review`] takes both, opens the file
//! with the cursor on the first line that changed, and draws the change over the text as a
//! diff view (split or unified, the reader's `s`). Leaving the view lands the reader in the
//! text, on the change, where `M-S` sends their place back to the host and `M-P` shows the
//! change again.
//!
//! The change is the host's **snapshot**: it says what the edit did when it was made, and is
//! not re-diffed against the buffer. A file edited since still opens, and the view still
//! shows the edit that was asked about — which is the thing being reviewed.

use std::path::Path;

use crate::diffview::Source;
use crate::editor::Editor;

/// One change to one file: an excerpt of the file before and after it, each side's lines
/// joined by `\n`, with the 1-based line of its own file that the excerpt starts at.
///
/// An excerpt may carry unchanged lines around the change (context); they are drawn as
/// context and are what [`Change::first_changed_line`] steps over.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Change {
    pub before: String,
    pub after: String,
    /// The line of the old file `before` starts at (1-based).
    pub before_start: usize,
    /// The line of the new file `after` starts at (1-based).
    pub after_start: usize,
}

impl Change {
    /// **The line of the new file the change begins at**, 1-based: past the context the two
    /// sides share, and never past the excerpt's own last line.
    ///
    /// The cap is for a change that only removed lines at the excerpt's end: the first line
    /// that differs is then *after* the last line the new side has, and a position past the
    /// end of the file is one the editor would wait for and never reach. Its last line is the
    /// nearest place that exists.
    pub fn first_changed_line(&self) -> usize {
        let start = self.after_start.max(1);
        let after: Vec<&str> = self.after.lines().collect();
        let same = self
            .before
            .lines()
            .zip(&after)
            .take_while(|(b, a)| b == *a)
            .count();
        start + same.min(after.len().saturating_sub(1))
    }
}

impl Editor {
    /// **Open `path` to review `change`**: the file with the cursor on the first changed line
    /// ([`Change::first_changed_line`], through [`Editor::open_at`]), and the change drawn over
    /// the text as a diff view. Esc goes to the text, `M-P` shows the change again, and `M-S`
    /// — from the view or the text — sends the reader's place to the host (`on_send`).
    ///
    /// An error is [`Editor::open_at`]'s: the file could not be opened, and nothing changed.
    pub fn open_review(&mut self, path: &Path, change: Change) -> Result<(), String> {
        self.open_at(path, change.first_changed_line(), None)?;
        self.bs_mut().review = Some(change);
        self.show_review();
        Ok(())
    }

    /// The current buffer's review, drawn as a diff view. False when it has none.
    pub(crate) fn show_review(&mut self) -> bool {
        let Some(change) = self.bs().review.clone() else {
            return false;
        };
        let name = self.buffer_name(self.cur);
        self.open_view(Source::Review { name, change });
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::buffer::{Buffer, Pos};
    use crate::config::Config;
    use crate::term::{KeyCode, KeyEvent, Mods};
    use std::time::{Duration, Instant};

    fn change(before: &str, after: &str, start: usize) -> Change {
        Change {
            before: before.into(),
            after: after.into(),
            before_start: start,
            after_start: start,
        }
    }

    #[test]
    fn the_first_changed_line_steps_over_the_shared_context() {
        let c = change("a\nb\nc\nd", "a\nb\nX\nd", 10);
        assert_eq!(c.first_changed_line(), 12);
        // A pure insertion: the new line is the first that differs.
        assert_eq!(change("a\nb", "a\nNEW\nb", 1).first_changed_line(), 2);
        // A created file: everything is new, from its first line.
        assert_eq!(change("", "one\ntwo", 1).first_changed_line(), 1);
    }

    #[test]
    fn a_removal_at_the_excerpts_end_lands_on_its_last_line_not_past_it() {
        assert_eq!(change("a\nb\ngone", "a\nb", 5).first_changed_line(), 6);
        // Nothing left at all: the excerpt's own start.
        assert_eq!(change("gone", "", 7).first_changed_line(), 7);
        // A start of 0 is not a line; it reads as the first.
        assert_eq!(change("", "x", 0).first_changed_line(), 1);
    }

    fn opened(text: &str, c: Change) -> (Editor, std::path::PathBuf) {
        let path = std::env::temp_dir().join(format!(
            "rano_review_{}_{}.txt",
            std::process::id(),
            c.after_start
        ));
        std::fs::write(&path, text).unwrap();
        let mut ed = Editor::new(Buffer::new(), Config::default());
        ed.set_area(crate::editor::Area::new(0, 0, 80, 24));
        ed.open_review(&path, c).unwrap();
        for _ in 0..500 {
            ed.tick(Instant::now());
            if !ed.loading() {
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        ed.tick(Instant::now());
        (ed, path)
    }

    /// **The review opens on the change**: the diff view is up with the change's lines in it,
    /// the cursor underneath is already on the first changed line, and Esc is the way down to
    /// it — where `M-P` brings the change back.
    #[test]
    fn a_review_opens_the_change_over_the_text_and_esc_lands_on_it() {
        let text: String = (1..=40).map(|i| format!("line {i}\n")).collect();
        let c = change(
            "line 18\nline 19\nline 20\nold 21\nline 22",
            "line 18\nline 19\nline 20\nline 21\nline 22",
            18,
        );
        let (mut ed, path) = opened(&text, c);
        let _ = std::fs::remove_file(&path);
        let view = ed.diff_view.as_ref().expect("the change is drawn");
        assert!(view.header().contains("Review"), "{}", view.header());
        let shown: String = view
            .lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_str())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(shown.contains("old 21"), "{shown}");
        assert_eq!(ed.bs().cursor, Pos { row: 20, col: 0 });

        ed.handle_key(KeyEvent::new(KeyCode::Esc, Mods::NONE));
        assert!(ed.diff_view.is_none(), "Esc is back to the text");
        assert_eq!(ed.bs().cursor, Pos { row: 20, col: 0 }, "on the change");

        ed.handle_key(KeyEvent::new(KeyCode::Char('p'), Mods::ALT));
        assert!(ed.diff_view.is_some(), "M-P shows the change again");
        ed.handle_key(KeyEvent::new(KeyCode::Char('p'), Mods::ALT));
        assert!(ed.diff_view.is_none(), "and closes it");
    }

    /// **`M-S` from the review sends the place the change is at**, without leaving the view:
    /// the reader asks about what they are looking at.
    #[test]
    fn send_from_the_review_hands_the_host_the_changed_line() {
        let text: String = (1..=10).map(|i| format!("line {i}\n")).collect();
        let (mut ed, path) = opened(&text, change("line 4\nold", "line 4\nline 5", 4));
        let _ = std::fs::remove_file(&path);
        let got = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let sink = got.clone();
        ed.on_send = Some(Box::new(move |e| {
            sink.borrow_mut().push(e.clone());
            Ok("sent".into())
        }));
        ed.handle_key(KeyEvent::new(KeyCode::Char('s'), Mods::ALT));
        let got = got.borrow();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].cursor.line, 5);
        assert!(ed.diff_view.is_some(), "the view stays up");
    }
}
