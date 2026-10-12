//! Editor configuration (F1): a tiny `key = value` file, no serde. The file
//! lives at $XDG_CONFIG_HOME/rano/config.toml (else $HOME/.config/...).
//! Unknown keys and bad values are ignored, never fatal.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    pub tab_width: usize,
    pub auto_indent: bool,
    /// **What a Tab inserts** — see [`Indent`]. Defaults to `indent_size` spaces, which is
    /// what a buffer with no style of its own gets and what `Indent::Auto` falls back to.
    pub indent: Indent,
    /// Columns one indent step is, when the step is spaces. Clamped to 1..=16, like
    /// [`Config::tab_width`] (which is how *wide* a tab is drawn, a different question).
    pub indent_size: usize,
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

/// **What a Tab inserts.** Go and the Makefile world want a tab; most everything else
/// written this decade wants spaces, and how many is a matter of taste — so it is one
/// setting with three values rather than a guess.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Indent {
    /// `indent_size` spaces. **The default**, and what a buffer with no detectable style
    /// gets. (It was a tab before 2026-10-11, which made `Tab` in a fresh buffer a guess
    /// rather than a setting.)
    #[default]
    Spaces,
    /// One tab character, drawn at `tab_width`. `gofmt` rewrites a Go file's indentation to
    /// these whatever rano inserts — it has no `-tabs` flag any more, so it is not a choice
    /// the formatter offers — which is why `auto` exists.
    Tabs,
    /// The buffer's own style: a file that indents with tabs gets tabs, one that indents
    /// with two, three or four spaces gets exactly those. The fallback when a file has
    /// nothing to detect is `indent_size` spaces.
    Auto,
}

impl Indent {
    /// From a config value: `spaces`, `tabs`, `auto`. Anything else is ignored, like every
    /// other bad value in this file.
    pub fn parse(v: &str) -> Option<Indent> {
        match v.trim().to_ascii_lowercase().as_str() {
            "spaces" | "space" => Some(Indent::Spaces),
            "tabs" | "tab" => Some(Indent::Tabs),
            "auto" | "detect" => Some(Indent::Auto),
            _ => None,
        }
    }
}

impl Config {
    /// **The whitespace a Tab (and an auto-indent) inserts for this buffer**: the
    /// configured unit, or — under [`Indent::Auto`] — the buffer's own, detected.
    pub fn indent_unit(&self, buf: &crate::buffer::Buffer) -> String {
        match self.indent {
            Indent::Tabs => "\t".to_string(),
            Indent::Spaces => " ".repeat(self.indent_size),
            Indent::Auto => detect_indent(buf).unwrap_or_else(|| " ".repeat(self.indent_size)),
        }
    }
}

/// **The buffer's own indent style, or `None`** when it has none to speak of: a tab if any
/// row indents with one, else the greatest common divisor of the leading-space runs (an
/// eight-space Python file gets eight, a four-space Rust file four, a two-space JSON file
/// two, and a file someone indented by one and then by three gets one).
///
/// **ONE pass, and no collected Vec.** The version this replaces built a
/// `Vec<&Vec<char>>` of every row and then walked it twice, which on a
/// 2.6M-row file is a 21 MB allocation and two traversals — measured at
/// **36 ms per call**, against 13.2 ms for this. It is not a hot path for
/// typing (`type a char` is 0.1 µs) but it runs on every **Enter** with
/// auto-indent, every `Tab` and every **Backspace**, so 36 ms was the cost
/// of pressing Enter in a large file.
///
/// The GCD is tracked as the scan goes rather than sorted afterwards,
/// because a GCD is all this needs and it is monotonically
/// non-increasing — so reaching 1 ends the search, since no later row can
/// raise it. That bound is exact, not a heuristic.
///
/// It is still O(rows), which §16.0 says an edit must not be, and it is
/// the blocker named in `TODO.md` §16.3 for the lazy store: under a
/// file-backed row this walk would be disk reads. The exact fix is to
/// maintain this incrementally (a row's leading whitespace is known when
/// it is written), and it wants the swap to land first so it can be done
/// once.
pub fn detect_indent(buf: &crate::buffer::Buffer) -> Option<String> {
    let mut unit = 0usize;
    for l in buf.rows() {
        if l.first() == Some(&'\t') {
            return Some("\t".to_string());
        }
        let n = l.iter().take_while(|c| **c == ' ').count();
        if n > 0 {
            unit = if unit == 0 { n } else { gcd(unit, n) };
            if unit == 1 {
                // The floor: already 1, and it cannot fall further.
                break;
            }
        }
    }
    // Eight or more is not a unit anyone means, and a one-space run usually means "this
    // row was indented by one", not "my style is one column".
    (unit > 1 && unit <= 8).then(|| " ".repeat(unit))
}

/// The largest common divisor of `a` and `b`.
fn gcd(mut a: usize, mut b: usize) -> usize {
    while b != 0 {
        let t = b;
        b = a % b;
        a = t;
    }
    a
}

impl Default for Config {
    fn default() -> Self {
        Self {
            tab_width: 4,
            auto_indent: true,
            indent: Indent::Spaces,
            indent_size: 4,
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
            // `indent = spaces | tabs | auto` and how wide a step is. `tab_width` above is a
            // different question — how wide a tab is *drawn*.
            "indent" => {
                if let Some(i) = Indent::parse(value) {
                    cfg.indent = i;
                }
            }
            "indent_size" => {
                if let Ok(n) = value.parse::<usize>() {
                    cfg.indent_size = n.clamp(1, 16);
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
        // **The defaults**: a tab is drawn four columns wide and a Tab inserts four spaces —
        // not a tab, which is what it used to insert for a buffer with no style of its own.
        assert_eq!(cfg.tab_width, 4, "four columns for a drawn tab");
        assert_eq!(cfg.indent, Indent::Spaces);
        assert_eq!(cfg.indent_size, 4);
        assert!(cfg.auto_indent, "auto-indent defaults to on");
        assert!(!cfg.multibuffer);
    }

    /// **What a Tab inserts**, in each of the three modes, and the two keys that set it.
    #[test]
    fn the_indent_setting_says_what_a_tab_is() {
        let cfg = parse_config("indent = spaces\nindent_size = 2");
        assert_eq!(cfg.indent, Indent::Spaces);
        assert_eq!(cfg.indent_size, 2);
        let mut buf = crate::buffer::Buffer::new();
        buf.set_rows(vec![vec!['x']]);
        assert_eq!(cfg.indent_unit(&buf), "  ");

        let cfg = parse_config("indent = tabs");
        assert_eq!(cfg.indent, Indent::Tabs);
        assert_eq!(cfg.indent_unit(&buf), "\t");

        // `auto` follows the buffer: a tab-indented file gets a tab, a two-space file two,
        // and a file with nothing to detect gets the configured size.
        let cfg = parse_config("indent = auto\nindent_size = 3");
        assert_eq!(cfg.indent, Indent::Auto);
        let mut tabs = crate::buffer::Buffer::new();
        tabs.set_rows(vec![vec!['\t', 'x']]);
        assert_eq!(cfg.indent_unit(&tabs), "\t");
        let mut two = crate::buffer::Buffer::new();
        two.set_rows(vec![vec![' ', ' ', 'x'], vec![' ', ' ', ' ', ' ', 'y']]);
        assert_eq!(cfg.indent_unit(&two), "  ", "the GCD of the runs");
        assert_eq!(cfg.indent_unit(&buf), "   ", "nothing to detect: the size");

        // A bad value is ignored, like every other bad value in this file; the size is
        // clamped like `tab_width`.
        let cfg = parse_config("indent = whichever\nindent_size = 99");
        assert_eq!(cfg.indent, Indent::Spaces);
        assert_eq!(cfg.indent_size, 16);
        assert_eq!(parse_config("indent_size = 0").indent_size, 1);
        assert_eq!(Indent::parse("TABS"), Some(Indent::Tabs));
        assert_eq!(Indent::parse("auto"), Some(Indent::Auto));
        assert_eq!(Indent::parse("nonsense"), None);
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
