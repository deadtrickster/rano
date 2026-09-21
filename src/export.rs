//! Render a buffer to a file: colourised HTML, ANSI for a terminal, a
//! markdown code fence, or plain text.
//!
//! Why it exists, beyond being a feature: it is the only path that runs the
//! highlighter and then does NOT draw to a terminal. `rano --export html
//! big.rs` measures parse + colorisation with ratatui, the terminal and the
//! event loop all out of the picture, which is what makes it possible to say
//! where a slow file's time actually goes (`--bench` reports both sides of
//! that line).
//!
//! The style source is a closure rather than a [`crate::Editor`], so the same
//! renderer serves the CLI, an embedder with its own palette, and the tests.
//! Nothing here knows what a terminal is: it writes bytes.

use crate::buffer::Pos;
use crate::width;
use ratatui::style::{Color, Modifier, Style};

/// Output format for [`render`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    /// A self-contained HTML document, colours inline.
    Html,
    /// SGR-escaped text: `rano --export ansi f.rs | less -R`.
    Ansi,
    /// A markdown fenced code block holding the text, tagged with the
    /// detected language — how you paste a file into a document.
    Markdown,
    /// The plain text, tabs expanded, nothing else.
    Text,
}

impl Format {
    /// Parse a format name. `None` for an unknown name, so a caller can
    /// report the valid set rather than guess.
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "html" | "htm" => Some(Self::Html),
            "ansi" | "term" | "terminal" => Some(Self::Ansi),
            "markdown" | "md" => Some(Self::Markdown),
            "text" | "txt" | "plain" => Some(Self::Text),
            _ => None,
        }
    }

    /// The names [`Self::parse`] accepts, in help-text order.
    pub const NAMES: &'static str = "html, ansi, markdown, text";
}

/// Render `lines` in `format`, resolving each character's style through
/// `style_of(p)`. `title` names the document in the HTML `<title>` and the
/// markdown heading.
pub fn render(
    lines: &[Vec<char>],
    tab_width: usize,
    title: &str,
    format: Format,
    style_of: &dyn Fn(Pos) -> Option<Style>,
) -> String {
    match format {
        Format::Html => html(lines, tab_width, title, style_of),
        Format::Ansi => ansi(lines, tab_width, style_of),
        Format::Markdown => markdown(lines, tab_width, title),
        Format::Text => text(lines, tab_width),
    }
}

/// One run of characters sharing a style, already tab-expanded. The run
/// boundaries are what both colourised formats need, so they are computed once
/// per line, here.
struct Run {
    style: Option<Style>,
    text: String,
}

/// Walk a line, expanding tabs to the next multiple of `tab_width` and
/// coalescing neighbours that resolved to the same style. `style_of` is asked
/// with real positions, so callers can resolve styles however they like.
fn runs_at_row(
    line: &[char],
    row: usize,
    tab_width: usize,
    style_of: &dyn Fn(Pos) -> Option<Style>,
) -> Vec<Run> {
    let tw = tab_width.max(1);
    let mut out: Vec<Run> = Vec::new();
    let mut disp = 0usize;
    for (i, &c) in line.iter().enumerate() {
        let style = style_of(Pos { row, col: i });
        match out.last_mut() {
            Some(r) if r.style == style => {
                if c == '\t' {
                    let spaces = tw - (disp % tw);
                    for _ in 0..spaces {
                        r.text.push(' ');
                    }
                    disp += spaces;
                } else {
                    disp += width::char_width(c);
                    r.text.push(c);
                }
            }
            _ => {
                let mut text = String::new();
                if c == '\t' {
                    let spaces = tw - (disp % tw);
                    for _ in 0..spaces {
                        text.push(' ');
                    }
                    disp += spaces;
                } else {
                    disp += width::char_width(c);
                    text.push(c);
                }
                out.push(Run { style, text });
            }
        }
    }
    out
}

// ---------------------------------------------------------------- HTML ----

fn html(
    lines: &[Vec<char>],
    tab_width: usize,
    title: &str,
    style_of: &dyn Fn(Pos) -> Option<Style>,
) -> String {
    let mut s = String::with_capacity(lines.iter().map(|l| l.len() + 16).sum::<usize>() + 512);
    s.push_str("<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n");
    s.push_str("<meta name=\"generator\" content=\"rano\">\n");
    s.push_str(&format!("<title>{}</title>\n", esc(title)));
    s.push_str(
        "<style>\n\
         :root { color-scheme: dark }\n\
         body { margin: 0; background: #1e1e1e; color: #d4d4d4 }\n\
         pre { margin: 0; padding: 1em; overflow-x: auto;\n\
         \x20     font: 13px/1.45 ui-monospace, SFMono-Regular, Menlo, Consolas, monospace }\n\
         </style>\n</head>\n<body>\n<pre>",
    );
    for (r, line) in lines.iter().enumerate() {
        if r > 0 {
            s.push('\n');
        }
        for run in runs_at_row(line, r, tab_width, style_of) {
            let body = esc(&run.text);
            match run.style.as_ref().and_then(css) {
                Some(decl) => {
                    s.push_str("<span style=\"");
                    s.push_str(&decl);
                    s.push_str("\">");
                    s.push_str(&body);
                    s.push_str("</span>");
                }
                None => s.push_str(&body),
            }
        }
    }
    s.push_str("</pre>\n</body>\n</html>\n");
    s
}

/// The CSS declarations a style needs, or `None` when it renders as the
/// page default (so plain text carries no markup at all).
fn css(style: &Style) -> Option<String> {
    let mut d = String::new();
    if let Some(c) = style.fg.and_then(hex) {
        d.push_str(&format!("color:{c}"));
    }
    if let Some(c) = style.bg.and_then(hex) {
        if !d.is_empty() {
            d.push(';');
        }
        d.push_str(&format!("background-color:{c}"));
    }
    let m = style.add_modifier;
    for (bit, css) in [
        (Modifier::BOLD, "font-weight:bold"),
        (Modifier::ITALIC, "font-style:italic"),
        (Modifier::DIM, "opacity:.65"),
        (Modifier::REVERSED, "filter:invert(1)"),
    ] {
        if m.contains(bit) {
            if !d.is_empty() {
                d.push(';');
            }
            d.push_str(css);
        }
    }
    // Underline and strikethrough share one property, so they are collected.
    let mut deco: Vec<&str> = Vec::new();
    if m.contains(Modifier::UNDERLINED) {
        deco.push("underline");
    }
    if m.contains(Modifier::CROSSED_OUT) {
        deco.push("line-through");
    }
    if !deco.is_empty() {
        if !d.is_empty() {
            d.push(';');
        }
        d.push_str("text-decoration:");
        d.push_str(&deco.join(" "));
    }
    if d.is_empty() { None } else { Some(d) }
}

/// `#rrggbb` for the colours a theme or a selection actually uses. `None` for
/// the 256-colour palette, which this export does not carry (rano's own
/// themes are all RGB, so nothing is lost in practice).
fn hex(c: Color) -> Option<String> {
    let (r, g, b) = match c {
        Color::Rgb(r, g, b) => (r, g, b),
        Color::Black => (0, 0, 0),
        Color::Red => (0xcd, 0x31, 0x31),
        Color::Green => (0x0d, 0xbc, 0x79),
        Color::Yellow => (0xe5, 0xe5, 0x10),
        Color::Blue => (0x24, 0x72, 0xc8),
        Color::Magenta => (0xbc, 0x3f, 0xbc),
        Color::Cyan => (0x11, 0xa8, 0xcd),
        Color::Gray => (0xe5, 0xe5, 0xe5),
        Color::DarkGray => (0x66, 0x66, 0x66),
        Color::LightRed => (0xf1, 0x4c, 0x4c),
        Color::LightGreen => (0x23, 0xd1, 0x8b),
        Color::LightYellow => (0xf5, 0xf5, 0x43),
        Color::LightBlue => (0x3b, 0x8e, 0xea),
        Color::LightMagenta => (0xd6, 0x70, 0xd6),
        Color::LightCyan => (0x29, 0xb8, 0xdb),
        Color::White => (0xff, 0xff, 0xff),
        // Reset, Indexed, and any future variant: no colour, which the
        // caller reads as "the page default".
        _ => return None,
    };
    Some(format!("#{r:02x}{g:02x}{b:02x}"))
}

fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(c),
        }
    }
    out
}

// ---------------------------------------------------------------- ANSI ----

fn ansi(lines: &[Vec<char>], tab_width: usize, style_of: &dyn Fn(Pos) -> Option<Style>) -> String {
    let mut s = String::new();
    for (r, line) in lines.iter().enumerate() {
        if r > 0 {
            s.push('\n');
        }
        let mut open = false;
        for run in runs_at_row(line, r, tab_width, style_of) {
            // Trailing blanks are the screen's, not the file's: a colourless
            // run of spaces adds nothing and costs a reset per line.
            let text = if run.style.is_none() {
                run.text.trim_end()
            } else {
                run.text.as_str()
            };
            if text.is_empty() {
                continue;
            }
            if open {
                s.push_str("\x1b[0m");
                open = false;
            }
            if let Some(seq) = sgr(&run.style) {
                s.push_str(&seq);
                open = true;
            }
            s.push_str(text);
        }
        if open {
            s.push_str("\x1b[0m");
        }
    }
    if !s.ends_with('\n') {
        s.push('\n');
    }
    s
}

fn sgr(style: &Option<Style>) -> Option<String> {
    let st = style.as_ref()?;
    let mut parts: Vec<String> = Vec::new();
    let m = st.add_modifier;
    if m.contains(Modifier::BOLD) {
        parts.push("1".into());
    }
    if m.contains(Modifier::DIM) {
        parts.push("2".into());
    }
    if m.contains(Modifier::ITALIC) {
        parts.push("3".into());
    }
    if m.contains(Modifier::UNDERLINED) {
        parts.push("4".into());
    }
    if m.contains(Modifier::REVERSED) {
        parts.push("7".into());
    }
    if m.contains(Modifier::CROSSED_OUT) {
        parts.push("9".into());
    }
    if let Some(c) = st.fg.and_then(fg_code) {
        parts.push(c);
    }
    if let Some(c) = st.bg.and_then(bg_code) {
        parts.push(c);
    }
    if parts.is_empty() {
        return None;
    }
    Some(format!("\x1b[{}m", parts.join(";")))
}

fn fg_code(c: Color) -> Option<String> {
    let n = named_code(c)?;
    Some(match n {
        // Bright variants already carry their own parameter.
        s if s.starts_with("38;5;") || s.starts_with("38;2;") => s,
        s => format!("38;5;{s}"),
    })
}

fn bg_code(c: Color) -> Option<String> {
    let n = named_code(c)?;
    Some(match n {
        s if s.starts_with("38;5;") || s.starts_with("38;2;") => s.replace("38;", "48;"),
        s => format!("48;5;{s}"),
    })
}

/// The SGR colour parameters a [`Color`] maps to, as a partial sequence.
fn named_code(c: Color) -> Option<String> {
    let s = match c {
        Color::Black => "0".into(),
        Color::Red => "1".into(),
        Color::Green => "2".into(),
        Color::Yellow => "3".into(),
        Color::Blue => "4".into(),
        Color::Magenta => "5".into(),
        Color::Cyan => "6".into(),
        Color::Gray => "7".into(),
        Color::DarkGray => "8".into(),
        Color::LightRed => "9".into(),
        Color::LightGreen => "10".into(),
        Color::LightYellow => "11".into(),
        Color::LightBlue => "12".into(),
        Color::LightMagenta => "13".into(),
        Color::LightCyan => "14".into(),
        Color::White => "15".into(),
        Color::Rgb(r, g, b) => format!("38;2;{r};{g};{b}"),
        Color::Indexed(i) => format!("38;5;{i}"),
        // Reset, and any future variant: no colour, which the caller reads as
        // "the page default".
        _ => return None,
    };
    Some(s)
}

// ------------------------------------------------------------ markdown ----

fn markdown(lines: &[Vec<char>], tab_width: usize, title: &str) -> String {
    let fence = fence_for(lines);
    let mut s = String::new();
    if !title.is_empty() {
        s.push_str(&format!("# {title}\n\n"));
    }
    s.push_str(&format!("{fence}{}\n", lang_hint(title)));
    s.push_str(&text(lines, tab_width));
    s.push_str(&format!("{fence}\n"));
    s
}

/// A fence long enough to hold the document: one backtick longer than the
/// longest run in the content, minimum three. Without this a file containing
/// a ``` fence of its own — any markdown file — would close the block early.
fn fence_for(lines: &[Vec<char>]) -> String {
    let mut longest = 0usize;
    for line in lines {
        let mut run = 0usize;
        for &c in line {
            if c == '`' {
                run += 1;
                longest = longest.max(run);
            } else {
                run = 0;
            }
        }
    }
    "`".repeat((longest + 1).max(3))
}

/// The language tag for the fence, from the title's extension. Best effort:
/// an unknown extension gets no tag, which is exactly what markdown wants.
fn lang_hint(title: &str) -> &str {
    let ext = title.rsplit('.').next().unwrap_or("");
    match ext {
        "rs" => "rust",
        "py" => "python",
        "js" => "javascript",
        "ts" => "typescript",
        "tsx" => "tsx",
        "sh" | "bash" => "bash",
        "lisp" | "cl" | "asd" => "common-lisp",
        "md" => "markdown",
        "toml" => "toml",
        "yml" | "yaml" => "yaml",
        "c" | "h" => "c",
        "go" => "go",
        "json" => "json",
        "html" | "htm" => "html",
        "css" => "css",
        "lua" => "lua",
        "rb" => "ruby",
        "php" => "php",
        "java" => "java",
        "sql" => "sql",
        "clj" => "clojure",
        "scm" => "scheme",
        "el" => "emacs-lisp",
        "diff" | "patch" => "diff",
        "ini" => "ini",
        _ => "",
    }
}

// ---------------------------------------------------------------- text ----

fn text(lines: &[Vec<char>], tab_width: usize) -> String {
    let tw = tab_width.max(1);
    let mut s = String::new();
    for (r, line) in lines.iter().enumerate() {
        if r > 0 {
            s.push('\n');
        }
        let mut disp = 0usize;
        for &c in line {
            if c == '\t' {
                let spaces = tw - (disp % tw);
                for _ in 0..spaces {
                    s.push(' ');
                }
                disp += spaces;
            } else {
                disp += width::char_width(c);
                s.push(c);
            }
        }
    }
    s.push('\n');
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Color;

    fn cs(s: &str) -> Vec<char> {
        s.chars().collect()
    }

    /// Two lines, the second styled: enough to check every format's shape.
    fn sample() -> (Vec<Vec<char>>, impl Fn(Pos) -> Option<Style>) {
        let lines = vec![cs("let x = 1;\n<<not a tag>>"), cs("\ttabbed & <escaped>")];
        let f = |p: Pos| {
            if p.row == 0 && p.col < 3 {
                Some(Style::default().fg(Color::Rgb(0xc6, 0x78, 0xdd)))
            } else if p.row == 1 {
                Some(
                    Style::default()
                        .fg(Color::Rgb(0x98, 0xc3, 0x79))
                        .add_modifier(Modifier::BOLD | Modifier::CROSSED_OUT),
                )
            } else {
                None
            }
        };
        (lines, f)
    }

    #[test]
    fn html_is_a_document_with_the_colours_inline() {
        let (lines, f) = sample();
        let out = render(&lines, 8, "big.rs", Format::Html, &f);
        assert!(out.starts_with("<!DOCTYPE html>"));
        assert!(out.contains("<title>big.rs</title>"));
        assert!(out.contains("<pre>"));
        assert!(out.ends_with("</html>\n"));
        // The keyword run carries its colour, and only that much of the line.
        assert!(
            out.contains("<span style=\"color:#c678dd\">let</span>"),
            "{out}"
        );
        // Everything after it is default, so it is bare text.
        assert!(out.contains("> x = 1;"));
    }

    #[test]
    fn html_escapes_markup_and_expands_tabs() {
        let (lines, f) = sample();
        let out = render(&lines, 8, "t.rs", Format::Html, &f);
        // `<` `>` `&` from the source must not become tags or entities by
        // accident: the file's own text may be anything.
        assert!(out.contains("&lt;&lt;not a tag&gt;&gt;"), "{out}");
        assert!(out.contains("&amp; &lt;escaped&gt;"), "{out}");
        // The tab became eight spaces, at the start of the styled run.
        assert!(out.contains("        tabbed"), "{out}");
    }

    #[test]
    fn html_maps_modifiers_to_css() {
        let (lines, f) = sample();
        let out = render(&lines, 8, "t.rs", Format::Html, &f);
        assert!(out.contains("font-weight:bold"), "{out}");
        assert!(out.contains("text-decoration:line-through"), "{out}");
    }

    #[test]
    fn plain_text_has_no_markup_at_all() {
        let (lines, f) = sample();
        let out = render(&lines, 8, "t.rs", Format::Text, &f);
        // No escapes at all, and the document's own text passes through
        // byte for byte — including the `<` characters a naive check would
        // mistake for markup.
        assert!(!out.contains('\u{1b}'));
        assert!(!out.contains("<span"));
        assert!(!out.contains("&lt;"));
        assert_eq!(
            out,
            "let x = 1;\n<<not a tag>>\n        tabbed & <escaped>\n"
        );
    }

    #[test]
    fn ansi_resets_between_runs_and_ends_clean() {
        let (lines, f) = sample();
        let out = render(&lines, 8, "t.rs", Format::Ansi, &f);
        assert!(out.contains("\x1b[38;2;198;120;221m"), "{out:?}");
        assert!(out.contains("\x1b[1;9;38;2;152;195;121m"), "{out:?}");
        // Every line ends reset, so a redirected file cannot leak attributes
        // into whatever is printed after it.
        for line in out.lines() {
            let opens = line.matches('\u{1b}').count();
            let closes = line.matches("\x1b[0m").count();
            assert!(opens == 0 || closes >= 1, "unclosed SGR: {line:?}");
        }
    }

    #[test]
    fn markdown_uses_a_fence_longer_than_the_content() {
        // A markdown file containing its own fence must not close the block:
        // this is the case the feature exists for.
        let lines = vec![cs("```rust"), cs("fn main() {}"), cs("```")];
        let out = render(&lines, 8, "a.md", Format::Markdown, &|_| None);
        assert!(out.starts_with("# a.md\n\n"), "{out}");
        assert!(out.contains("````markdown"), "{out}");
        assert!(out.trim_end().ends_with("````"), "{out}");
        // …and a longer run in the content pushes it further.
        let lines = vec![cs("`````"), cs("x")];
        let out = render(&lines, 8, "a.md", Format::Markdown, &|_| None);
        assert!(out.contains("``````"), "{out}");
    }

    #[test]
    fn markdown_tags_the_fence_from_the_title() {
        let lines = vec![cs("fn main() {}")];
        for (title, want) in [
            ("a.rs", "```rust\n"),
            ("a.lisp", "```common-lisp\n"),
            ("a.unknown", "```\n"),
            ("noext", "```\n"),
        ] {
            let out = render(&lines, 8, title, Format::Markdown, &|_| None);
            assert!(out.contains(want), "{title}: {out}");
        }
    }

    #[test]
    fn an_empty_buffer_is_still_valid_output() {
        let empty: Vec<Vec<char>> = vec![];
        for fmt in [Format::Html, Format::Ansi, Format::Text, Format::Markdown] {
            let out = render(&empty, 8, "e.rs", fmt, &|_| None);
            assert!(!out.is_empty(), "{fmt:?} produced nothing");
        }
    }

    #[test]
    fn format_names_parse() {
        assert_eq!(Format::parse("HTML"), Some(Format::Html));
        assert_eq!(Format::parse("htm"), Some(Format::Html));
        assert_eq!(Format::parse("ansi"), Some(Format::Ansi));
        assert_eq!(Format::parse("md"), Some(Format::Markdown));
        assert_eq!(Format::parse("txt"), Some(Format::Text));
        assert_eq!(Format::parse("nope"), None);
    }
}
