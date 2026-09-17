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
}

impl Default for Config {
    fn default() -> Self {
        Self {
            tab_width: 8,
            auto_indent: true,
            line_numbers: true,
            multibuffer: false,
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
        // Strip an inline comment, then require `key = value`.
        let line = match line.find('#') {
            Some(i) => line[..i].trim(),
            None => line,
        };
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        let value = value.trim();
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
    use super::*;

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
    fn invalid_values_keep_defaults() {
        let cfg = parse_config("tab_width = abc\nauto_indent = yes\nline_numbers = 1");
        assert_eq!(cfg, Config::default());
    }

    #[test]
    fn unknown_keys_and_comments_ignored() {
        let cfg = parse_config("# hi\ntheme = dark\ntab_width = 2 # inline");
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
