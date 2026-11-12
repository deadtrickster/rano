//! Editor configuration: a tiny `key = value` file at
//! $XDG_CONFIG_HOME/rano/config.toml. Unknown keys and bad values are ignored.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    pub tab_width: usize,
    pub auto_indent: bool,
    /// What a Tab inserts — see [`Indent`]. Defaults to `indent_size` spaces.
    pub indent: Indent,
    /// Columns one indent step is, when the step is spaces, clamped to 1..=16
    /// (unlike [`Config::tab_width`], how wide a tab is drawn).
    pub indent_size: usize,
    pub line_numbers: bool,
    pub multibuffer: bool,
    pub wrap: bool,
    /// Check GitHub for a newer release at startup: `None` leaves it to
    /// `RANO_AUTOUPDATE`. See `crate::update`.
    pub autoupdate: Option<bool>,
    /// M-S: a shell command (`sh -c`) that gets the cursor, the file and the
    /// selection as JSON, plus `RANO_FILE`/`RANO_LINE`/`RANO_COLUMN`. `None`: nowhere to send.
    pub send_command: Option<String>,
    /// How keys are written: `key_notation = emacs` (the default), `nano` (`^X`, `M-U`), or
    /// `words` (`ctrl-x`, `alt-u`) — the spelling the two programs share.
    pub notation: crate::keymap::Notation,
    /// `theme = NAME`: the theme in `themes/NAME.toml` beside this file.
    pub theme: Option<String>,
    /// The WASM plugin: a `.wasm` whose `on_open`/`on_save` hooks run at the two
    /// seams. `RANO_PLUGIN` overrides it.
    pub plugin: Option<String>,
    /// What the configured plugin may import — `capabilities = ["read-dir"]`.
    /// Empty grants nothing; a misspelt name is ignored rather than granted.
    pub capabilities: Vec<String>,
    /// `color.ROLE = "look"` lines, in file order; [`crate::theme::resolve`] reads them.
    pub colors: Vec<(String, String)>,
    /// The `M-k` presets, compiled in and then changed field by field by
    /// `ask.<id>.<field>` lines. In the order the key bar shows them.
    pub ask: Vec<crate::ask::AskPreset>,
}

/// What a Tab inserts: Go and the Makefile world want a tab, most everything else
/// spaces, so it is one setting with three values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Indent {
    #[default]
    Spaces,
    /// One tab character, drawn at `tab_width`. gofmt rewrites a Go file's
    /// indentation to these whatever rano inserts.
    Tabs,
    /// The buffer's own style: a tab-indented file gets tabs, a two-space file two.
    /// Fallback: `indent_size` spaces.
    Auto,
}

impl Indent {
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
    /// The whitespace a Tab (and an auto-indent) inserts; under [`Indent::Auto`],
    /// the buffer's own.
    pub fn indent_unit(&self, buf: &crate::buffer::Buffer) -> String {
        match self.indent {
            Indent::Tabs => "\t".to_string(),
            Indent::Spaces => " ".repeat(self.indent_size),
            Indent::Auto => detect_indent(buf).unwrap_or_else(|| " ".repeat(self.indent_size)),
        }
    }
}

/// The buffer's own indent style, or `None` when it has none to speak of: a tab if
/// any row indents with one, else the GCD of the leading-space runs. One pass: this
/// runs on every Enter with auto-indent, every Tab and every Backspace.
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
    // Eight or more is not a unit anyone means, and a one-space run usually means
    // "this row has one space", not "my style is one column".
    (unit > 1 && unit <= 8).then(|| " ".repeat(unit))
}

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
            notation: crate::keymap::Notation::default(),
            theme: None,
            plugin: None,
            capabilities: Vec::new(),
            colors: Vec::new(),
            ask: crate::ask::default_presets(),
        }
    }
}

/// $XDG_CONFIG_HOME/rano/config.toml, falling back to $HOME/.config/...
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
    // A preset edited field by field: a file that changes only the prompt keeps the
    // compiled-in key, and a preset it invents needs a key of its own to reach.
    let mut ask: Vec<(String, Draft)> = Vec::new();
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
        // A double-quoted value is taken whole, `#` and all; anything else ends at
        // an inline comment.
        let value = match value.strip_prefix('"') {
            Some(q) => q.split_once('"').map_or(q, |(v, _)| v),
            None => value.split('#').next().unwrap_or("").trim(),
        };
        match key {
            "tab_width" => {
                if let Ok(n) = value.parse::<usize>() {
                    cfg.tab_width = n.clamp(1, 16);
                }
            }
            // `indent = spaces | tabs | auto`; `tab_width` above is a different
            // question — how wide a tab is *drawn*.
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
                "nano" => cfg.notation = crate::keymap::Notation::Nano,
                "emacs" => cfg.notation = crate::keymap::Notation::Emacs,
                "words" => cfg.notation = crate::keymap::Notation::Words,
                _ => {}
            },
            "send_command" => {
                cfg.send_command = (!value.is_empty()).then(|| value.to_string());
            }
            "theme" => {
                cfg.theme = (!value.is_empty()).then(|| value.to_string());
            }
            "plugin" => {
                let v = value.trim();
                cfg.plugin = (!v.is_empty()).then(|| v.to_string());
            }
            // `capabilities = ["read-dir"]`: brackets and quotes optional, so
            // `capabilities = read-dir` is the same statement.
            "capabilities" => {
                cfg.capabilities = value
                    .trim_matches(|c| c == '[' || c == ']')
                    .split(',')
                    .map(|c| c.trim().trim_matches('"').trim_matches('\'').to_string())
                    .filter(|c| !c.is_empty())
                    .collect();
            }
            _ if key.starts_with("color.") => {
                cfg.colors
                    .push((key["color.".len()..].to_string(), value.to_string()));
            }
            _ if key.starts_with("ask.") => {
                let Some((id, field)) = key["ask.".len()..].rsplit_once('.') else {
                    continue;
                };
                if !ask.iter().any(|(i, _)| i == id) {
                    ask.push((id.to_string(), Draft::default()));
                }
                let Some((_, d)) = ask.iter_mut().find(|(i, _)| i == id) else {
                    continue;
                };
                match field {
                    "key" => d.key = crate::keymap::Key::parse(value),
                    "label" => d.label = (!value.is_empty()).then(|| value.to_string()),
                    "answer" => d.answer = crate::ask::Answer::parse(value),
                    "prompt" => d.prompt = (!value.is_empty()).then(|| value.to_string()),
                    _ => {}
                }
            }
            _ => {}
        }
    }
    cfg.ask = asked(ask);
    cfg
}

/// The presets as the file left them: the compiled-in ones edited in place, then the
/// ones it added, in file order — so the key bar's order is the reader's.
fn asked(drafts: Vec<(String, Draft)>) -> Vec<crate::ask::AskPreset> {
    let mut presets = crate::ask::default_presets();
    for (id, d) in drafts {
        let p = match presets.iter_mut().find(|p| p.id == id) {
            Some(p) => p,
            None => {
                let Some(key) = d.key else { continue };
                presets.push(crate::ask::AskPreset {
                    id: id.clone(),
                    key,
                    label: id.clone(),
                    answer: crate::ask::Answer::Chat,
                    prompt: String::new(),
                });
                presets.last_mut().expect("just pushed")
            }
        };
        if let Some(k) = d.key {
            p.key = k;
        }
        if let Some(l) = d.label {
            p.label = l;
        }
        if let Some(a) = d.answer {
            p.answer = a;
        }
        if let Some(t) = d.prompt {
            p.prompt = t;
        }
    }
    presets
}

/// One preset's fields as the file gave them; `None` is a field it did not mention.
#[derive(Default)]
struct Draft {
    key: Option<crate::keymap::Key>,
    label: Option<String>,
    answer: Option<crate::ask::Answer>,
    prompt: Option<String>,
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
    fn an_ask_line_changes_one_field_and_can_add_a_preset() {
        use crate::ask::Answer;
        use crate::keymap::Key;
        use crate::term::KeyCode;
        // One field of a compiled-in preset: the rest stays, so a file that changes
        // only the prompt does not lose the key or where the answer goes.
        let c = parse_config("ask.fact-check.prompt = \"Just the wrong ones. # no comment\"\n");
        let p = c.ask.iter().find(|p| p.id == "fact-check").expect("kept");
        assert_eq!(p.prompt, "Just the wrong ones. # no comment");
        assert_eq!(p.key, Key::plain(KeyCode::Char('f')));
        assert_eq!(p.answer, Answer::Notes);

        // A preset of the file's own, after the compiled-in ones in file order.
        let c = parse_config(
            "ask.translate.key = \"t\"\nask.translate.label = \"translate\"\n\
             ask.translate.answer = suggest\nask.translate.prompt = \"Translate it.\"\n",
        );
        let p = c.ask.iter().find(|p| p.id == "translate").expect("added");
        assert_eq!(p.label, "translate");
        assert_eq!(p.answer, Answer::Suggest);
        assert_eq!(c.ask.last().map(|p| p.id.as_str()), Some("translate"));
        assert_eq!(c.ask.len(), crate::ask::default_presets().len() + 1);

        // No key means no way to reach it; a bad mode or key keeps what was there.
        let c = parse_config("ask.nokey.label = \"no key\"\n");
        assert!(c.ask.iter().all(|p| p.id != "nokey"));
        let c = parse_config("ask.rewrite.answer = poetry\nask.rewrite.key = not-a-key\n");
        let p = c.ask.iter().find(|p| p.id == "rewrite").expect("kept");
        assert_eq!(p.answer, Answer::Suggest);
        assert_eq!(p.key, Key::plain(KeyCode::Char('r')));
    }

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
        // The defaults: a drawn tab is four columns, a Tab inserts four spaces.
        assert_eq!(cfg.tab_width, 4, "four columns for a drawn tab");
        assert_eq!(cfg.indent, Indent::Spaces);
        assert_eq!(cfg.indent_size, 4);
        assert!(cfg.auto_indent, "auto-indent defaults to on");
        assert!(!cfg.multibuffer);
    }

    #[test]
    fn a_capability_is_a_named_list_and_the_empty_list_grants_nothing() {
        assert!(
            parse_config("capabilities = [\"read-dir\"]").capabilities
                == vec!["read-dir".to_string()]
        );
        assert_eq!(
            parse_config("capabilities = read-dir").capabilities,
            vec!["read-dir".to_string()],
            "the brackets and quotes are optional"
        );
        assert_eq!(
            parse_config("capabilities = [\"read-dir\", \"read-file\"]").capabilities,
            vec!["read-dir".to_string(), "read-file".to_string()]
        );
        assert!(
            parse_config("").capabilities.is_empty(),
            "nothing granted by default"
        );
        assert!(parse_config("capabilities = []").capabilities.is_empty());
    }

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

        // `auto` follows the buffer: a tab-indented file gets a tab, a two-space file
        // two, and a file with nothing to detect gets the configured size.
        let cfg = parse_config("indent = auto\nindent_size = 3");
        assert_eq!(cfg.indent, Indent::Auto);
        let mut tabs = crate::buffer::Buffer::new();
        tabs.set_rows(vec![vec!['\t', 'x']]);
        assert_eq!(cfg.indent_unit(&tabs), "\t");
        let mut two = crate::buffer::Buffer::new();
        two.set_rows(vec![vec![' ', ' ', 'x'], vec![' ', ' ', ' ', ' ', 'y']]);
        assert_eq!(cfg.indent_unit(&two), "  ", "the GCD of the runs");
        assert_eq!(cfg.indent_unit(&buf), "   ", "nothing to detect: the size");

        // A bad value is ignored; the size is clamped like `tab_width`.
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
        // None, not Some(true): unset is what lets RANO_AUTOUPDATE decide.
        assert_eq!(Config::default().autoupdate, None);
        assert_eq!(parse_config("autoupdate = false").autoupdate, Some(false));
        assert_eq!(parse_config("autoupdate = true").autoupdate, Some(true));
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
