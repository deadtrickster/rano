//! Single source of truth for the key bindings (C2): the bottom bar and the
//! help overlay are both built from [`BAR`], so the two surfaces can never
//! disagree about which key does what.

pub struct Binding {
    pub key: &'static str,
    pub label: &'static str,
}

/// Bottom-bar entries, in nano's column-major pairing order (item `2c`
/// renders in the top bar row, item `2c+1` in the bottom row). The trailing
/// five rows are reserved for features whose wiring lands with later items
/// (diagnostics, line numbers, filter, buffer switching).
#[rustfmt::skip]
    pub static BAR: [Binding; 31] = [
    Binding { key: "^G", label: "Help" },
    Binding { key: "^X", label: "Exit" },
    Binding { key: "^O", label: "Write Out" },
    Binding { key: "^R", label: "Read File" },
    Binding { key: "^F", label: "Where Is" },
    Binding { key: "^\\", label: "Replace" },
    Binding { key: "^K", label: "Cut" },
    Binding { key: "^U", label: "Paste" },
    Binding { key: "^T", label: "Execute" },
    Binding { key: "^J", label: "Justify" },
    Binding { key: "^C", label: "Location" },
    Binding { key: "^/", label: "Go To Line" },
    Binding { key: "M-U", label: "Undo" },
    Binding { key: "M-E", label: "Redo" },
    Binding { key: "M-A", label: "Set Mark" },
    Binding { key: "M-6", label: "Copy" },
    Binding { key: "M-]", label: "To Bracket" },
    Binding { key: "^B", label: "Where Was" },
    Binding { key: "M-B", label: "Previous" },
    Binding { key: "M-F", label: "Next" },
    Binding { key: "\u{25c2}", label: "Back" },
    Binding { key: "\u{25b8}", label: "Forward" },
    Binding { key: "^\u{25c2}", label: "Prev Word" },
    Binding { key: "^\u{25b8}", label: "Next Word" },
    Binding { key: "M-D", label: "Next Diagnostic" },
    Binding { key: "M-.", label: "Definition" },
    Binding { key: "M-,", label: "Jump Back" },
    Binding { key: "M-N", label: "Line Numbers" },
    Binding { key: "M-|", label: "Filter" },
    Binding { key: "M-<", label: "Prev Buffer" },
    Binding { key: "M->", label: "Next Buffer" },
];

/// One help-grid cell: readable "key label" pair padded to a fixed column
/// width. The single space between key and label keeps the text searchable
/// ("^F Where Is").
fn cell(key: &str, label: &str) -> String {
    format!("{:<20}", format!("{} {}", key, label))
}

/// The help overlay content: a key grid built from [`BAR`] at runtime (so it
/// always matches the bar), the F-key table, and the static prose. Rows are
/// hand-wrapped; a terminal narrower than a line just clips it.
pub fn help_lines() -> Vec<String> {
    let mut lines = vec![
        "  R A N O  -  a nano clone written in Rust".to_string(),
        String::new(),
    ];
    for chunk in BAR.chunks(4) {
        let mut row = String::from("  ");
        for b in chunk {
            row.push_str(&cell(b.key, b.label));
        }
        lines.push(row.trim_end().to_string());
    }
    lines.push(String::new());
    const FKEYS: [(&str, &str); 11] = [
        ("F1", "Help"),
        ("F2", "Write Out"),
        ("F3", "Where Is"),
        ("F4", "Replace"),
        ("F5", "Read File"),
        ("F6", "Execute"),
        ("F7", "Make Backup"),
        ("F8", "Open File"),
        ("F9", "Sort"),
        ("F10", "Justify"),
        ("F11", "Go To Line"),
    ];
    for chunk in FKEYS.chunks(4) {
        let mut row = String::from("  ");
        for (k, l) in chunk {
            row.push_str(&cell(k, l));
        }
        lines.push(row.trim_end().to_string());
    }
    lines.push(String::new());
    lines.extend([
        "  Move with the arrow keys, Home, End, PgUp, PgDn.".to_string(),
        "  Edit with Enter, Backspace, Delete, Tab.".to_string(),
        "  ^A / M-A sets a mark; move to select, then type or ^K to act on it.".to_string(),
        "  M-6 copies the current line or marked region to the cutbuffer.".to_string(),
        "  ^F searches; ^B searches backwards; M-B/M-F jump to the previous/next match."
            .to_string(),
        "  ^X exits; if the buffer is modified you will be asked to save.".to_string(),
        String::new(),
        "  Press any key to continue".to_string(),
    ]);
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn help_is_accurate() {
        let text = help_lines().join("\n");
        assert!(text.contains("^F Where Is"));
        assert!(text.contains("^B Where Was"));
        assert!(text.contains("M-U Undo"));
        assert!(text.contains("M-E Redo"));
        // Stale nano keys that rano does not bind.
        assert!(!text.contains("^W"));
        assert!(!text.contains("^Y"));
    }

    #[test]
    fn bar_matches_help() {
        let text = help_lines().join("\n");
        for b in &BAR {
            assert!(
                text.contains(&format!("{} {}", b.key, b.label)),
                "help overlay is missing {} {}",
                b.key,
                b.label
            );
        }
    }

    #[test]
    fn bar_count_and_reserved_rows() {
        assert_eq!(BAR.len(), 31);
        let keys: Vec<&str> = BAR.iter().map(|b| b.key).collect();
        for k in ["M-D", "M-.", "M-,", "M-N", "M-|", "M-<", "M->"] {
            assert!(keys.contains(&k), "reserved row {} missing", k);
        }
    }
}
