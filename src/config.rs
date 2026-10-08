//! Editor configuration (F1): a tiny `key = value` file, no serde. The file
//! lives at $XDG_CONFIG_HOME/rano/config.toml (else $HOME/.config/...).
//! Unknown keys and bad values are ignored, never fatal.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    pub tab_width: usize,
    pub auto_indent: bool,
    pub line_numbers: bool,
    pub multibuffer: bool,
    pub wrap: bool,
    /// Check GitHub for a newer release at startup: `Some(false)` off,
    /// `Some(true)` on, `None` not set — and `None` is what lets the
    /// `RANO_AUTOUPDATE` environment variable decide. See `crate::update`.
    pub autoupdate: Option<bool>,
    /// M-S: a shell command (`sh -c`) that receives the cursor, the file and
    /// the selection — as JSON on stdin and as `RANO_FILE` / `RANO_LINE` /
    /// `RANO_COLUMN` in its environment. `None`: M-S has nowhere to send.
    pub send_command: Option<String>,
    /// Write keys nano's way (`^X`, `M-U`) instead of emacs's (`C-x`, `M-u`)
    /// in the bar, the help pages and `M-x`: `key_notation = nano`.
    pub nano_keys: bool,
    /// `theme = NAME`: the theme in `themes/NAME.toml` beside this file. See
    /// [`crate::theme`].
    pub theme: Option<String>,
    /// `color.ROLE = "look"` lines, in file order: one role overridden on top of the
    /// theme. Kept as written; [`crate::theme::resolve`] reads and reports them.
    pub colors: Vec<(String, String)>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            tab_width: 8,
            auto_indent: true,
            line_numbers: true,
            multibuffer: false,
            wrap: true,
            autoupdate: None,
            send_command: None,
            nano_keys: false,
            theme: None,
            colors: Vec::new(),
        }
    }
}

/// $XDG_CONFIG_HOME/rano/config.toml, falling back to $HOME/.config/...
/// when XDG_CONFIG_HOME is unset or empty.
pub fn config_path() -> PathBuf {
    let base = match std::env::var_os("XDG_CONFIG_HOME") {
        Some(v) if !v.is_empty() => PathBuf::from(v),
        _ => match std::env::var_os("HOME") {
            Some(h) if !h.is_empty() => PathBuf::from(h).join(".config"),
            // No HOME either: load() below just hands back defaults.
            _ => PathBuf::from(".config"),
        },
    };
    base.join("rano").join("config.toml")
}

/// Load the user's config; a missing or unreadable file yields the defaults.
pub fn load() -> Config {
    load_from(&config_path())
}

pub fn load_from(path: &Path) -> Config {
    match std::fs::read_to_string(path) {
        Ok(text) => parse_config(&text),
        Err(_) => Config::default(),
    }
}

fn parse_config(text: &str) -> Config {
    let mut cfg = Config::default();
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        let value = value.trim();
        // A double-quoted value is taken whole, `#` and all (a shell command
        // can carry one); anything else ends at an inline comment.
        let value = match value.strip_prefix('"') {
            Some(q) => q.split_once('"').map_or(q, |(v, _)| v),
            None => value.split('#').next().unwrap_or("").trim(),
        };
        match key {
            // Clamped so the display math can never break.
            "tab_width" => {
                if let Ok(n) = value.parse::<usize>() {
                    cfg.tab_width = n.clamp(1, 16);
                }
            }
            "auto_indent" => {
                if let Some(b) = parse_bool(value) {
                    cfg.auto_indent = b;
                }
            }
            "line_numbers" => {
                if let Some(b) = parse_bool(value) {
                    cfg.line_numbers = b;
                }
            }
            "multibuffer" => {
                if let Some(b) = parse_bool(value) {
                    cfg.multibuffer = b;
                }
            }
            "wrap" => {
                if let Some(b) = parse_bool(value) {
                    cfg.wrap = b;
                }
            }
            "autoupdate" => {
                if let Some(b) = parse_bool(value) {
                    cfg.autoupdate = Some(b);
                }
            }
            "key_notation" => match value {
                "nano" => cfg.nano_keys = true,
                "emacs" => cfg.nano_keys = false,
                _ => {}
            },
            "send_command" => {
                cfg.send_command = (!value.is_empty()).then(|| value.to_string());
            }
            "theme" => {
                cfg.theme = (!value.is_empty()).then(|| value.to_string());
            }
            _ if key.starts_with("color.") => {
                cfg.colors
                    .push((key["color.".len()..].to_string(), value.to_string()));
            }
            _ => {}
        }
    }
    cfg
}

fn parse_bool(value: &str) -> Option<bool> {
    match value {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {

    /// `theme` names a file; `color.ROLE` lines are kept in order for the theme to read.
    #[test]
    fn a_theme_and_colour_lines_are_read() {
        let c = parse_config(
            "theme = gray\ncolor.user_block = \"bg:#2f363b\"\ncolor.heading = \"fg:4 bold\"\n",
        );
        assert_eq!(c.theme.as_deref(), Some("gray"));
        assert_eq!(
            c.colors,
            vec![
                ("user_block".to_string(), "bg:#2f363b".to_string()),
                ("heading".to_string(), "fg:4 bold".to_string())
            ]
        );
    }

    use super::*;

    #[test]
    fn send_command_keeps_a_quoted_hash_and_drops_a_trailing_comment() {
        let c = parse_config("send_command = \"letibot send --tag '#rano'\"  # the host\n");
        assert_eq!(
            c.send_command.as_deref(),
            Some("letibot send --tag '#rano'")
        );
        let c = parse_config("send_command = pbcopy # clipboard\n");
        assert_eq!(c.send_command.as_deref(), Some("pbcopy"));
        assert_eq!(parse_config("send_command =\n").send_command, None);
        // The old keys are unchanged by the quoting rule.
        let c = parse_config("wrap = false # a = b\ntab_width = 4\n");
        assert!(!c.wrap);
        assert_eq!(c.tab_width, 4);
    }

    #[test]
    fn empty_text_is_default() {
        assert_eq!(parse_config(""), Config::default());
        assert_eq!(parse_config("\n   \n# only a comment\n"), Config::default());
    }

    #[test]
    fn parses_valid_pairs() {
        let cfg = parse_config("tab_width = 4\nauto_indent = true");
        assert_eq!(cfg.tab_width, 4);
        assert!(cfg.auto_indent);
        assert!(cfg.line_numbers, "line numbers default to on");
        assert!(!cfg.multibuffer);
    }

    #[test]
    fn partial_keeps_defaults() {
        let cfg = parse_config("line_numbers = true");
        assert!(cfg.line_numbers);
        assert_eq!(cfg.tab_width, 8);
        assert!(cfg.auto_indent, "auto-indent defaults to on");
        assert!(!cfg.multibuffer);
    }

    #[test]
    fn autoupdate_is_unset_by_default_and_parseable() {
        // None, not Some(true): unset is what lets RANO_AUTOUPDATE decide, and
        // what makes the config "not saying" rather than "saying yes".
        assert_eq!(Config::default().autoupdate, None);
        assert_eq!(parse_config("autoupdate = false").autoupdate, Some(false));
        assert_eq!(parse_config("autoupdate = true").autoupdate, Some(true));
        // A bad value leaves it unset rather than guessing.
        assert_eq!(parse_config("autoupdate = yes").autoupdate, None);
    }

    #[test]
    fn invalid_values_keep_defaults() {
        let cfg = parse_config("tab_width = abc\nauto_indent = yes\nline_numbers = 1");
        assert_eq!(cfg, Config::default());
    }

    #[test]
    fn wrap_defaults_on_and_parses() {
        assert!(Config::default().wrap, "soft wrap defaults to on (nano)");
        assert!(!parse_config("wrap = false").wrap);
        assert!(parse_config("wrap = true").wrap);
        assert!(
            parse_config("wrap = bogus").wrap,
            "bad value keeps the default"
        );
    }

    #[test]
    fn unknown_keys_and_comments_ignored() {
        let cfg = parse_config("# hi\ncolour_scheme = dark\ntab_width = 2 # inline");
        assert_eq!(
            cfg,
            Config {
                tab_width: 2,
                ..Config::default()
            }
        );
    }

    #[test]
    fn tab_width_clamped() {
        assert_eq!(parse_config("tab_width = 0").tab_width, 1);
        assert_eq!(parse_config("tab_width = 99").tab_width, 16);
        assert_eq!(parse_config("tab_width = 16").tab_width, 16);
    }

    #[test]
    fn missing_file_is_default() {
        let p = std::env::temp_dir().join("rano-no-such-config-test.toml");
        let _ = std::fs::remove_file(&p);
        assert_eq!(load_from(&p), Config::default());
    }

    #[test]
    fn load_from_reads_real_file() {
        let p = std::env::temp_dir().join(format!("rano-config-test-{}.toml", std::process::id()));
        std::fs::write(&p, "tab_width = 3\nmultibuffer = true\n").expect("write config");
        let cfg = load_from(&p);
        let _ = std::fs::remove_file(&p);
        assert_eq!(cfg.tab_width, 3);
        assert!(cfg.multibuffer);
    }
}
