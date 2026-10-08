//! **The config pane**: every setting the head and the session hold, grouped by where it
//! lives, with the one under the cursor saying where its value came from.
//!
//! Ported from letibot's `ui/panes/config.rs`. A row is marked `✎` when it changes now and
//! is kept, and blank when it shows its source and takes a restart — the mark is the whole
//! of the difference, so a reader scanning the list sees which rows are worth pressing
//! Enter on before reading any of them.

use crate::render::Line;
use crate::style::Role;

use super::pane;
use super::text::{clean_line, one, trim_to};

/// One setting.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConfigRow {
    /// The group the row is listed under: `head`, `session`.
    pub section: String,
    pub key: String,
    pub value: String,
    /// It changes now and is kept (`✎`), rather than taking a restart.
    pub editable: bool,
    /// Where the value came from, shown under the row the cursor is on. Empty draws
    /// nothing.
    pub source: String,
}

/// The config pane's facts.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConfigPane {
    pub rows: Vec<ConfigRow>,
    pub selected: usize,
    /// A sentence for a session that has nothing to list yet (not attached, or asked and
    /// not answered), drawn under the rows. `None` when the session's rows are there.
    pub session_note: Option<String>,
}

impl ConfigPane {
    /// Every row of the pane, at `w` columns.
    pub fn lines(&self, w: usize) -> Vec<Line> {
        let mut out = vec![pane::title("config"), Line::default()];
        // The key column is as wide as the longest key, up to a cap: a key past it is cut
        // by the row's own trim rather than pushing every value right.
        let keyw = self
            .rows
            .iter()
            .map(|r| r.key.chars().count())
            .max()
            .unwrap_or(8)
            .min(28);
        let mut section = "";
        let sel = self.selected.min(self.rows.len().saturating_sub(1));
        for (i, r) in self.rows.iter().enumerate() {
            if r.section != section {
                if !section.is_empty() {
                    out.push(Line::default());
                }
                out.push(one(format!("  {}", r.section), Role::Faint));
                section = &r.section;
            }
            let mark = if r.editable { "✎" } else { " " };
            let line = format!(
                "{} {mark} {:<keyw$}  {}",
                pane::mark(i == sel),
                clean_line(&r.key),
                clean_line(&r.value)
            );
            let line = Line::raw(trim_to(&line, w.saturating_sub(2)));
            out.push(pane::picked(line, i == sel));
            if i == sel && !r.source.is_empty() {
                out.push(one(
                    format!("       from {}", clean_line(&r.source)),
                    Role::Faint,
                ));
            }
        }
        if let Some(note) = &self.session_note {
            out.push(Line::default());
            out.push(one(format!("  {note}"), Role::Faint));
        }
        out.push(Line::default());
        out.push(one(
            "    ✎ changes now and is kept (head → head.toml, mode → project store); \
             the rest shows its source and takes a restart",
            Role::Faint,
        ));
        out
    }
}

super::lines_widget!(ConfigPane);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::testing::plain;
    use crate::style::Palette;

    fn row(section: &str, key: &str, value: &str, editable: bool) -> ConfigRow {
        ConfigRow {
            section: section.into(),
            key: key.into(),
            value: value.into(),
            editable,
            source: format!("{key}.toml"),
        }
    }

    /// Grouped by section, the editable rows marked, and only the row under the cursor
    /// says where its value came from.
    #[test]
    fn the_rows_are_grouped_and_the_cursors_row_names_its_source() {
        let p = ConfigPane {
            rows: vec![
                row("head", "diff", "split", true),
                row("head", "theme", "dark", false),
                row("session", "mode", "always-ask", true),
            ],
            selected: 1,
            session_note: None,
        };
        let rows = plain(&p.lines(80));
        assert_eq!(rows[0], "config");
        assert_eq!(rows[2], "  head");
        assert_eq!(rows[3], "  ✎ diff   split");
        assert_eq!(rows[4], "▸   theme  dark");
        assert_eq!(rows[5], "       from theme.toml");
        assert_eq!(rows[6], "");
        assert_eq!(rows[7], "  session");
        assert!(!rows.iter().any(|l| l.contains("diff.toml")), "{rows:#?}");
    }

    /// The highlighted row is one inverse run, as letibot drew it.
    #[test]
    fn the_highlighted_row_is_inverse() {
        let p = ConfigPane {
            rows: vec![row("head", "diff", "split", true)],
            selected: 0,
            session_note: Some("session — asked the daemon; nothing back yet".into()),
        };
        let lines = p.lines(80);
        assert_eq!(
            lines[3].to_ansi(Palette::Colour),
            "\x1b[7m▸ ✎ diff  split\x1b[0m"
        );
        assert!(
            plain(&lines)
                .iter()
                .any(|l| l == "  session — asked the daemon; nothing back yet")
        );
    }
}
