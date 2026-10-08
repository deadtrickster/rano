//! **The help screen**: a title, every key and command with what it does, and how to close
//! it — drawn in place of the conversation.
//!
//! Ported from letibot's `ui/panes/help.rs`. The rows are the host's (a host knows its own
//! keys and verbs; this module knows how a list of them is laid out), so the view model is
//! the list and nothing else.
//!
//! The shape: each name in a sixteen-column field, in the key register, and its sentence
//! wrapped in the columns that are left, its continuation rows under the sentence and not
//! under the name — so the names read as one column down the left however long a
//! sentence runs.

use crate::render::{Line, Span};
use crate::style::Role;

use super::text::{one, wrap};

/// The width of the name field, the two-column step in front of it included.
const NAME: usize = 18;
/// Where a sentence starts: the name field and one column of air.
const BODY: usize = NAME + 1;

/// The help screen's facts.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HelpPane {
    /// The screen's title: `keys and commands`.
    pub title: String,
    /// `(name, what it does)`, in the order a reader is meant to meet them.
    pub rows: Vec<(String, String)>,
    /// The last row: how to close it.
    pub footer: String,
}

impl HelpPane {
    /// Every row of the screen, at `w` columns.
    pub fn lines(&self, w: usize) -> Vec<Line> {
        let mut out = vec![one(self.title.clone(), Role::Strong), Line::default()];
        for (k, v) in &self.rows {
            let head = format!("  {k:<16}");
            for (i, l) in wrap(v, w.saturating_sub(BODY)).into_iter().enumerate() {
                out.push(if i == 0 {
                    Line::new(vec![Span::role(head.clone(), Role::Key), Span::raw(l)])
                } else {
                    Line::raw(format!("{:BODY$}{l}", ""))
                });
            }
        }
        out.push(Line::default());
        out.push(one(format!("  {}", self.footer), Role::Faint));
        out
    }
}

super::lines_widget!(HelpPane);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::testing::{plain, role_of};
    use crate::style::Palette;

    fn pane() -> HelpPane {
        HelpPane {
            title: "keys and commands".into(),
            rows: vec![
                ("enter".into(), "send what you typed".into()),
                (
                    "ctrl-v".into(),
                    "open the rest of the newest long tool result — ↓ pages it, esc closes".into(),
                ),
            ],
            footer: "/help or esc closes this".into(),
        }
    }

    /// The names are one column down the left, and a wrapped sentence continues under the
    /// sentence, not under the name.
    #[test]
    fn a_wrapped_sentence_continues_under_itself() {
        let rows = plain(&pane().lines(44));
        assert_eq!(rows[0], "keys and commands");
        assert_eq!(rows[2], "  enter           send what you typed");
        let at = rows.iter().position(|l| l.starts_with("  ctrl-v")).unwrap();
        assert!(rows[at + 1].starts_with(&" ".repeat(BODY)), "{rows:#?}");
        assert_eq!(rows.last().unwrap(), "  /help or esc closes this");
    }

    /// letibot's bytes: the name field painted in the key register and the sentence plain.
    #[test]
    fn the_name_is_the_key_register_and_the_sentence_is_plain() {
        let l = &pane().lines(80)[2];
        assert_eq!(role_of(l, "enter"), Some(Role::Key));
        assert_eq!(
            l.to_ansi(Palette::Colour),
            "\x1b[36m  enter           \x1b[0msend what you typed"
        );
    }
}
