//! What the editor hands a host when the reader sends their place: the file,
//! the cursor, and the selection if there is one.
//!
//! The editor's side is a callback (`Editor::on_send`); this is the value it is
//! called with, in the library so a host and the editor share one definition.
//! The standalone binary's callback runs the configured `send_command` with
//! [`SendEvent::to_json`] on its stdin.
//!
//! Positions are **1-based** lines and columns, the way a compiler error, a grep
//! hit or `$EDITOR +line` counts them, and a column counts characters (Unicode
//! scalar values), not bytes or display cells.

use std::path::PathBuf;

/// A place in a file: 1-based line and column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct Point {
    pub line: usize,
    pub column: usize,
}

/// A selected range: `start` before `end`, `end` exclusive, and the text
/// between them with lines joined by `\n`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Selection {
    pub start: Point,
    pub end: Point,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SendEvent {
    /// The file, absolute when it has a name; `None` for an unnamed buffer.
    pub path: Option<PathBuf>,
    /// The cursor.
    pub cursor: Point,
    /// The marked region, when a mark is set and the region is not empty.
    pub selection: Option<Selection>,
    /// The buffer has edits not yet saved, so the file on disk may differ from
    /// what `cursor` and `selection` point at.
    pub modified: bool,
}

impl SendEvent {
    /// One JSON object, e.g.
    /// `{"path":"/src/a.rs","cursor":{"line":3,"column":5},"selection":null,"modified":false}`.
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("a SendEvent always serialises")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_event_serialises_with_its_selection() {
        let e = SendEvent {
            path: Some(PathBuf::from("/src/a.rs")),
            cursor: Point { line: 3, column: 5 },
            selection: Some(Selection {
                start: Point { line: 2, column: 1 },
                end: Point { line: 3, column: 5 },
                text: "fn a() {\n    ".into(),
            }),
            modified: true,
        };
        let v: serde_json::Value = serde_json::from_str(&e.to_json()).unwrap();
        assert_eq!(v["path"], "/src/a.rs");
        assert_eq!(v["cursor"]["line"], 3);
        assert_eq!(v["selection"]["start"]["column"], 1);
        assert_eq!(v["selection"]["text"], "fn a() {\n    ");
        assert_eq!(v["modified"], true);
        let none = SendEvent {
            path: None,
            selection: None,
            ..e
        };
        let v: serde_json::Value = serde_json::from_str(&none.to_json()).unwrap();
        assert!(v["path"].is_null() && v["selection"].is_null());
    }
}
