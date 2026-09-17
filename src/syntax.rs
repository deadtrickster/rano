//! Tree-sitter syntax highlighting for rust, go, bash, python, c and json.
//!
//! The highlighter keeps a per-character style grid (`line_styles`) with the
//! same shape as `Buffer::lines`, so lookups from the UI are plain index
//! accesses. `refresh` re-parses the whole buffer; files are small, so the
//! cost is a fraction of a millisecond.

use ratatui::style::{Color, Style};
use std::path::Path;
use tree_sitter::{Language, Parser, Query, QueryCursor, StreamingIterator, Tree};

use crate::buffer::{Buffer, Pos};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lang {
    Rust,
    Go,
    Bash,
    Python,
    C,
    Json,
}

impl Lang {
    fn language(self) -> Language {
        match self {
            Lang::Rust => tree_sitter_rust::LANGUAGE.into(),
            Lang::Go => tree_sitter_go::LANGUAGE.into(),
            Lang::Bash => tree_sitter_bash::LANGUAGE.into(),
            Lang::Python => tree_sitter_python::LANGUAGE.into(),
            Lang::C => tree_sitter_c::LANGUAGE.into(),
            Lang::Json => tree_sitter_json::LANGUAGE.into(),
        }
    }

    fn query(self) -> &'static str {
        match self {
            Lang::Rust => tree_sitter_rust::HIGHLIGHTS_QUERY,
            Lang::Go => tree_sitter_go::HIGHLIGHTS_QUERY,
            Lang::Bash => tree_sitter_bash::HIGHLIGHT_QUERY,
            Lang::Python => tree_sitter_python::HIGHLIGHTS_QUERY,
            Lang::C => tree_sitter_c::HIGHLIGHT_QUERY,
            Lang::Json => tree_sitter_json::HIGHLIGHTS_QUERY,
        }
    }
}

/// Map a file to its language: by extension first, then by the shebang on
/// line one (`#!/bin/sh`, `#!/usr/bin/env python3`) for extension-less
/// scripts (scratch buffers get none).
pub fn detect(name: Option<&Path>, first_line: Option<&str>) -> Option<Lang> {
    if let Some(ext) = name.and_then(|n| n.extension()).and_then(|e| e.to_str()) {
        match ext.to_ascii_lowercase().as_str() {
            "rs" => return Some(Lang::Rust),
            "go" => return Some(Lang::Go),
            "sh" | "bash" => return Some(Lang::Bash),
            "py" | "pyw" => return Some(Lang::Python),
            "c" | "h" => return Some(Lang::C),
            "json" => return Some(Lang::Json),
            _ => {}
        }
    }
    detect_shebang(first_line?)
}

/// Language for a `#!` first line. Handles `#!/bin/sh`, `#! /bin/sh` and
/// `#!/usr/bin/env [-S …] python3` forms; interpreters we have no grammar
/// for (perl, ruby, …) map to None.
fn detect_shebang(line: &str) -> Option<Lang> {
    let rest = line.strip_prefix("#!")?.trim_start();
    let mut words = rest.split_whitespace();
    let mut interp = words.next()?.rsplit('/').next()?;
    if interp == "env" {
        interp = words.find(|w| !w.starts_with('-'))?.rsplit('/').next()?;
    }
    match interp {
        "sh" | "bash" | "dash" | "ash" | "zsh" | "ksh" => Some(Lang::Bash),
        "python" | "python2" | "python3" | "pypy" | "pypy3" => Some(Lang::Python),
        _ => None,
    }
}

/// One-Dark-ish palette for a dark background.
fn rgb(r: u8, g: u8, b: u8) -> Color {
    Color::Rgb(r, g, b)
}

fn theme(name: &str) -> Style {
    match name {
        "comment" => Style::default().fg(rgb(0x7f, 0x84, 0x8e)),
        "string" => Style::default().fg(rgb(0x98, 0xc3, 0x79)),
        "escape" => Style::default().fg(rgb(0x56, 0xb6, 0xc2)),
        "number" | "constant" => Style::default().fg(rgb(0xd1, 0x9a, 0x66)),
        "type" | "constructor" | "label" => Style::default().fg(rgb(0xe5, 0xc0, 0x7b)),
        "attribute" => Style::default().fg(rgb(0x56, 0xb6, 0xc2)),
        "keyword" | "include" | "preproc" => Style::default().fg(rgb(0xc6, 0x78, 0xdd)),
        "operator" | "punctuation" => Style::default().fg(rgb(0xab, 0xbb, 0xbf)),
        "property" => Style::default().fg(rgb(0xd1, 0x9a, 0x66)),
        "function" => Style::default().fg(rgb(0x61, 0xaf, 0xef)),
        "variable.builtin" => Style::default().fg(rgb(0xe0, 0x6c, 0x75)),
        // Dotted names we didn't match exactly fall back to their prefix
        // (e.g. "type.builtin" -> "type", "punctuation.bracket" -> "punctuation").
        _ => match name.split_once('.') {
            Some((prefix, _)) => theme(prefix),
            None => Style::default(),
        },
    }
}

pub struct Highlighter {
    parser: Parser,
    query: Option<Query>,
    query_lang: Option<Lang>,
    line_styles: Vec<Vec<Style>>,
    /// The last successful parse, kept so syntax errors can be surfaced
    /// without a language server (see [`Highlighter::syntax_errors`]).
    tree: Option<Tree>,
}

impl Highlighter {
    pub fn new() -> Self {
        Self {
            parser: Parser::new(),
            query: None,
            query_lang: None,
            line_styles: Vec::new(),
            tree: None,
        }
    }

    /// Re-parse the buffer and rebuild the style grid. No-op for scratch
    /// buffers (no file name, no language).
    pub fn refresh(&mut self, buf: &Buffer) {
        let first_line = buf.lines.first().map(|l| l.iter().collect::<String>());
        let Some(lang) = detect(buf.name.as_deref(), first_line.as_deref()) else {
            self.line_styles.clear();
            self.tree = None;
            return;
        };

        let source = buf.text();
        if self.parser.set_language(&lang.language()).is_err() {
            self.line_styles.clear();
            self.tree = None;
            return;
        }

        // Always do a full parse. Reusing the previous tree for incremental
        // parsing leaks byte offsets from the old (possibly longer) source
        // into the new tree, which panics inside tree-sitter when a line is
        // shortened. Full re-parses are fast enough for editor-sized buffers.
        let Some(tree) = self.parser.parse(source.as_bytes(), None) else {
            self.line_styles.clear();
            self.tree = None;
            return;
        };
        self.tree = Some(tree);

        let need_query = self.query.is_none() || self.query_lang != Some(lang);
        if need_query {
            match Query::new(&lang.language(), lang.query()) {
                Ok(q) => {
                    self.query = Some(q);
                    self.query_lang = Some(lang);
                }
                Err(_) => {
                    self.query = None;
                    self.query_lang = None;
                    self.line_styles.clear();
                    self.tree = None;
                    return;
                }
            }
        }
        let Some(query) = self.query.as_ref() else {
            self.line_styles.clear();
            self.tree = None;
            return;
        };

        let tree = self.tree.as_ref().unwrap();
        self.line_styles = Self::build_styles(&buf.lines, &source, tree, query);
    }

    /// Style for the character at `p`, if any capture colors it.
    pub fn style_at(&self, p: Pos) -> Option<Style> {
        let row = self.line_styles.get(p.row)?;
        let st = row.get(p.col)?;
        if *st == Style::default() {
            None
        } else {
            Some(*st)
        }
    }

    /// Syntax classes for raw text that never touches an editor buffer: one
    /// row per `\n`-split line, one entry per character — the tree-sitter
    /// capture name that colours it (`"keyword"`, `"function"`, …), or
    /// `None` where no capture applies.
    ///
    /// This is the engine without a palette. The caller owns the mapping
    /// from capture names to colours, so the language is passed explicitly
    /// instead of being detected from a buffer name — [`detect`] is still
    /// the way to get one from a path. The query is cached per language
    /// exactly as [`Self::refresh`] caches it, and the parse tree is kept
    /// as the last parse; the editor's own style grid is **not** touched,
    /// so a `Highlighter` shared between this and a buffer would show stale
    /// [`Self::style_at`] answers — give the embedder its own instance.
    ///
    /// Empty on failure (unknown language, query failed to compile, parse
    /// failed): the caller reads a missing row as "uncoloured", which is
    /// the same thing it does with a `None` cell.
    pub fn classes(&mut self, src: &str, lang: Lang) -> Vec<Vec<Option<String>>> {
        let Some(tree) = (|| {
            self.parser.set_language(&lang.language()).ok()?;
            self.parser.parse(src.as_bytes(), None)
        })() else {
            return Vec::new();
        };
        let need_query = self.query.is_none() || self.query_lang != Some(lang);
        if need_query {
            match Query::new(&lang.language(), lang.query()) {
                Ok(q) => {
                    self.query = Some(q);
                    self.query_lang = Some(lang);
                }
                Err(_) => {
                    self.query = None;
                    self.query_lang = None;
                    return Vec::new();
                }
            }
        }
        let Some(query) = self.query.as_ref() else {
            return Vec::new();
        };
        self.tree = Some(tree);
        let lines: Vec<Vec<char>> = src.split('\n').map(|l| l.chars().collect()).collect();
        Self::build_classes(&lines, src, self.tree.as_ref().unwrap(), query)
    }

    /// Syntax errors from the last parse as `(line, col, end_col, message)`
    /// in char columns, from `ERROR` nodes and missing nodes. Empty for
    /// scratch buffers and clean parses.
    pub fn syntax_errors(&self, lines: &[Vec<char>]) -> Vec<(usize, usize, usize, String)> {
        let Some(tree) = self.tree.as_ref() else {
            return Vec::new();
        };
        let root = tree.root_node();
        if !root.has_error() {
            return Vec::new();
        }
        let mut out = Vec::new();
        let mut stack = vec![(root, false)];
        while let Some((node, in_error)) = stack.pop() {
            let is_error = node.is_error() && !in_error;
            if node.is_missing() {
                // Missing nodes are zero-width insertion points; the caller
                // widens them to one visible column.
                let (l, c) = char_pos(lines, node.start_position());
                out.push((l, c, c, format!("missing {}", node.kind())));
                continue;
            }
            if is_error {
                let (l, c) = char_pos(lines, node.start_position());
                let (el, ec) = char_pos(lines, node.end_position());
                let end = if el == l { ec } else { c + 1 };
                out.push((l, c, end.max(c + 1), "syntax error".to_string()));
            }
            let mut cursor = node.walk();
            if cursor.goto_first_child() {
                loop {
                    stack.push((cursor.node(), in_error || is_error));
                    if !cursor.goto_next_sibling() {
                        break;
                    }
                }
            }
        }
        out.sort();
        out.dedup();
        out
    }

    fn build_styles(
        lines: &[Vec<char>],
        source: &str,
        tree: &Tree,
        query: &Query,
    ) -> Vec<Vec<Style>> {
        let mut line_styles: Vec<Vec<Style>> = lines
            .iter()
            .map(|l| vec![Style::default(); l.len()])
            .collect();
        Self::for_each_capture(lines, source, tree, query, |r, cs, name| {
            let style = theme(name);
            if style == Style::default() {
                return;
            }
            for cell in &mut line_styles[r][cs] {
                *cell = style;
            }
        });
        line_styles
    }

    /// The capture grid without a palette: one row per line, one entry per
    /// character — the tree-sitter capture name that covers it, or `None`.
    ///
    /// [`Self::build_styles`] is this plus rano's [`theme`]; a caller that
    /// owns its palette (another crate embedding the engine) wants the
    /// names instead, because capture → colour is a decision about the
    /// terminal, not about the grammar.
    fn build_classes(
        lines: &[Vec<char>],
        source: &str,
        tree: &Tree,
        query: &Query,
    ) -> Vec<Vec<Option<String>>> {
        let mut grid: Vec<Vec<Option<String>>> = lines
            .iter()
            .map(|l| vec![None; l.len()])
            .collect();
        Self::for_each_capture(lines, source, tree, query, |r, cs, name| {
            for cell in &mut grid[r][cs] {
                *cell = Some(name.to_string());
            }
        });
        grid
    }

    /// Walk every query capture and hand the caller the cells it covers as
    /// `(row, char_range, capture_name)`. Ranges are half-open and within
    /// the row. Later captures overwrite earlier ones cell by cell, which
    /// is the order [`Self::build_styles`] has always had; what a capture
    /// *means* is the callback's decision, which is why the default-styled
    /// skip lives in [`Self::build_styles`] and not here.
    fn for_each_capture(
        lines: &[Vec<char>],
        source: &str,
        tree: &Tree,
        query: &Query,
        mut f: impl FnMut(usize, std::ops::Range<usize>, &str),
    ) {
        // Per-line char-index -> byte-offset maps (char 0 always at 0).
        let char_offsets: Vec<Vec<usize>> = lines
            .iter()
            .map(|l| {
                let mut v = vec![0usize];
                for &c in l.iter() {
                    v.push(v.last().unwrap() + c.len_utf8());
                }
                v
            })
            .collect();

        // Byte offset of each line start within `source`. The line's
        // contribution is its BYTE length — the last entry of its char-offset
        // map — plus the '\n'; counting chars here made every line after a
        // multi-byte one start bytes early in tree-sitter's coordinates.
        let mut line_offsets = Vec::with_capacity(lines.len());
        let mut off = 0usize;
        for co in char_offsets.iter() {
            line_offsets.push(off);
            off += co[co.len() - 1] + 1; // line bytes + '\n'
        }

        let names = query.capture_names();
        let mut cursor = QueryCursor::new();
        let mut caps = cursor.captures(query, tree.root_node(), source.as_bytes());
        while let Some((m, i)) = caps.next() {
            let cap = m.captures()[*i];
            let name = names.get(cap.index as usize).copied().unwrap_or("");
            let (s, e) = (cap.node.start_byte(), cap.node.end_byte());
            let r0 = match line_offsets.partition_point(|&o| o <= s) {
                0 => 0,
                i => i - 1,
            };
            let last = e.saturating_sub(1);
            let r1 = match line_offsets.partition_point(|&o| o <= last) {
                0 => 0,
                i => i - 1,
            };
            for r in r0..=r1.min(lines.len() - 1) {
                let lo = line_offsets[r];
                // The line's BYTE length — the last entry of its char-offset
                // map — not its char count: the clamp is against bytes, and
                // counting chars cut captures short on multi-byte lines.
                let line_byte_len = char_offsets[r][char_offsets[r].len() - 1];
                let s_l = s.max(lo);
                let e_l = e.min(lo + line_byte_len);
                if s_l >= e_l {
                    continue;
                }
                let offs = &char_offsets[r];
                // Char index containing byte `b` (node ranges are half-open).
                let ci = |b: usize| offs.partition_point(|&o| o <= b).saturating_sub(1);
                let cs = ci(s_l - lo);
                let ce = (ci(e_l - lo - 1) + 1).min(lines[r].len());
                f(r, cs..ce, name);
            }
        }
    }
}

impl Default for Highlighter {
    fn default() -> Self {
        Self::new()
    }
}

/// tree-sitter reports byte offsets within a row; diagnostics use char
/// columns, so count the chars that make up the byte prefix.
fn char_pos(lines: &[Vec<char>], p: tree_sitter::Point) -> (usize, usize) {
    let Some(line) = lines.get(p.row) else {
        return (p.row, 0);
    };
    let mut bytes = 0usize;
    let mut col = 0usize;
    for ch in line {
        if bytes >= p.column {
            break;
        }
        bytes += ch.len_utf8();
        col += 1;
    }
    (p.row, col)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn buf_named(name: &str, text: &str) -> Buffer {
        let mut b = Buffer::new();
        b.lines = text.lines().map(|l| l.chars().collect()).collect();
        b.name = Some(PathBuf::from(name));
        b
    }

    fn style_at(hl: &Highlighter, row: usize, col: usize) -> Option<Style> {
        hl.style_at(Pos { row, col })
    }

    #[test]
    fn classes_name_the_tokens_of_raw_text_without_a_buffer() {
        let src = "let done = build(); // tail\n";
        let mut hl = Highlighter::new();
        let grid = hl.classes(src, Lang::Rust);
        assert_eq!(grid.len(), 2, "one row per \\n-split line, trailing piece included");
        let row = &grid[0];
        assert_eq!(row.len(), src.lines().next().unwrap().len());
        // `let` is a keyword, `build` a function call, the comment a comment,
        // the brackets punctuation. A plain identifier is captured by nothing
        // in the Rust query and stays `None` — the embedder reads that as
        // "plain text", which is what it is.
        let class = |needle: &str| {
            let at = src.find(needle).unwrap();
            row[at].clone()
        };
        assert_eq!(class("let").as_deref(), Some("keyword"));
        assert_eq!(class("build").as_deref(), Some("function"));
        assert_eq!(class("// tail").as_deref(), Some("comment"));
        assert_eq!(class("(").as_deref(), Some("punctuation.bracket"));
        assert_eq!(class("done"), None);
        assert!(grid[1].iter().all(|c| c.is_none()), "empty tail row");
    }

    #[test]
    fn classes_of_empty_text_is_one_empty_row_not_a_panic() {
        let mut hl = Highlighter::new();
        let grid = hl.classes("", Lang::Rust);
        assert_eq!(grid.len(), 1, "'' splits to one empty line");
        assert!(grid[0].is_empty());
    }

    /// **A multi-byte character shifts nothing but its own cell.**
    ///
    /// The em-dash is three bytes and one char, and two places in
    /// `for_each_capture` counted the one where the other was meant:
    /// `line_offsets` advanced by the char count, so every line after a
    /// multi-byte line started bytes early in tree-sitter's coordinates and
    /// its classes landed cells to the right; and the per-line clamp used
    /// the char count as the byte length, so a capture ending near the end
    /// of the multi-byte line itself lost its last cells. Seen from the
    /// head that consumes this grid as `tail o[0mff` — a reset sequence
    /// landing mid-word, because a class run ended two columns before the
    /// token did.
    #[test]
    fn a_multibyte_character_shifts_nothing_but_its_own_cell() {
        let src = "let s = \"a—b\"; // dash\nlet done = build(); // tail\n";
        // Char column of the first char of `needle` — `find` answers bytes,
        // and the grid is one cell per char.
        let col_of = |line: &str, needle: &str| {
            let b = line.find(needle).unwrap();
            line[..b].chars().count()
        };
        let mut hl = Highlighter::new();
        let grid = hl.classes(src, Lang::Rust);
        assert_eq!(grid.len(), 3, "one row per \\n-split line, tail included");

        // The line WITH the dash. The clamp bug lived here: the string
        // capture's byte end was clamped to the char count, so the closing
        // quote fell out of the capture and rendered plain.
        let l0 = src.split('\n').next().unwrap();
        assert_eq!(grid[0][col_of(l0, "let")].as_deref(), Some("keyword"));
        assert_eq!(
            grid[0][col_of(l0, "—")].as_deref(),
            Some("string"),
            "the dash itself is inside the string literal"
        );
        let close = col_of(l0, "b\"") + 1;
        assert_eq!(
            grid[0][close].as_deref(),
            Some("string"),
            "the closing quote is still inside the capture"
        );
        assert_eq!(grid[0][col_of(l0, "// dash")].as_deref(), Some("comment"));

        // The line AFTER it. The line-offset bug lived here: this line's
        // byte start was short by the dash's two extra bytes, so every
        // class landed two cells to the right.
        let l1 = src.split('\n').nth(1).unwrap();
        assert_eq!(grid[1][col_of(l1, "let")].as_deref(), Some("keyword"));
        assert_eq!(grid[1][col_of(l1, "build")].as_deref(), Some("function"));
        assert_eq!(grid[1][col_of(l1, "// tail")].as_deref(), Some("comment"));
        assert!(grid[2].iter().all(|c| c.is_none()), "empty tail row");
    }

    #[test]
    fn detects_languages() {
        assert_eq!(detect(Some(Path::new("a/b.rs")), None), Some(Lang::Rust));
        assert_eq!(detect(Some(Path::new("main.go")), None), Some(Lang::Go));
        assert_eq!(detect(Some(Path::new("run.SH")), None), Some(Lang::Bash));
        assert_eq!(detect(Some(Path::new("x.py")), None), Some(Lang::Python));
        assert_eq!(detect(Some(Path::new("x.pyw")), None), Some(Lang::Python));
        assert_eq!(detect(Some(Path::new("x.c")), None), Some(Lang::C));
        assert_eq!(detect(Some(Path::new("x.h")), None), Some(Lang::C));
        assert_eq!(detect(Some(Path::new("x.json")), None), Some(Lang::Json));
        assert_eq!(detect(None, None), None);
    }

    #[test]
    fn detects_shebang_languages() {
        assert_eq!(
            detect(Some(Path::new("letibot")), Some("#!/bin/sh")),
            Some(Lang::Bash)
        );
        assert_eq!(
            detect(Some(Path::new("letibot")), Some("#!/usr/bin/env bash")),
            Some(Lang::Bash)
        );
        assert_eq!(
            detect(Some(Path::new("letibot")), Some("#! /bin/sh")),
            Some(Lang::Bash)
        );
        assert_eq!(
            detect(Some(Path::new("letibot")), Some("#!/usr/bin/env python3")),
            Some(Lang::Python)
        );
        assert_eq!(
            detect(Some(Path::new("letibot")), Some("#!/usr/bin/env -S python3 -u")),
            Some(Lang::Python)
        );
        // No grammar for the interpreter, or no shebang at all.
        assert_eq!(
            detect(Some(Path::new("letibot")), Some("#!/usr/bin/perl")),
            None
        );
        assert_eq!(detect(Some(Path::new("letibot")), Some("echo hi")), None);
        // A known extension still wins over the shebang.
        assert_eq!(
            detect(Some(Path::new("x.py")), Some("#!/bin/sh")),
            Some(Lang::Python)
        );
    }

    #[test]
    fn highlights_rust() {
        let b = buf_named(
            "t.rs",
            "fn main() -> i32 {\n    let x = 42; // fourty two\n    x\n}",
        );
        let mut hl = Highlighter::new();
        hl.refresh(&b);
        assert!(style_at(&hl, 0, 0).is_some()); // "fn" keyword
        assert!(style_at(&hl, 0, 3).is_some()); // "main" function
        let line1: String = b.lines[1].iter().collect();
        assert!(style_at(&hl, 1, line1.find('4').unwrap()).is_some()); // 42
        assert!(style_at(&hl, 1, line1.find("fourty").unwrap()).is_some()); // comment
        assert!(style_at(&hl, 1, line1.find('x').unwrap()).is_none()); // plain local
    }

    #[test]
    fn highlights_go() {
        let b = buf_named(
            "t.go",
            "package main\n\nfunc Add(a int) int {\n\treturn a + 1\n}",
        );
        let mut hl = Highlighter::new();
        hl.refresh(&b);
        assert!(style_at(&hl, 0, 0).is_some()); // "package" keyword
        assert!(style_at(&hl, 2, 5).is_some()); // "Add" function
    }

    #[test]
    fn highlights_python() {
        let b = buf_named("t.py", "def fn(x):\n    return x\n");
        let mut hl = Highlighter::new();
        hl.refresh(&b);
        assert!(style_at(&hl, 0, 0).is_some()); // "def" keyword
        assert!(style_at(&hl, 0, 4).is_some()); // "fn" function
    }

    #[test]
    fn highlights_c() {
        let b = buf_named("t.c", "int main(void) {\n    return 0;\n}\n");
        let mut hl = Highlighter::new();
        hl.refresh(&b);
        assert!(style_at(&hl, 0, 0).is_some()); // "int" type
        assert!(style_at(&hl, 0, 4).is_some()); // "main" function
    }

    #[test]
    fn highlights_json() {
        let b = buf_named("t.json", "{\"k\": 1}\n");
        let mut hl = Highlighter::new();
        hl.refresh(&b);
        assert!(style_at(&hl, 0, 1).is_some()); // "k" property
        assert!(style_at(&hl, 0, 6).is_some()); // 1 number
    }

    #[test]
    fn highlights_bash() {
        let b = buf_named("t.sh", "#!/bin/sh\necho \"hello\"\n");
        let mut hl = Highlighter::new();
        hl.refresh(&b);
        assert!(style_at(&hl, 0, 0).is_some()); // comment
        assert!(style_at(&hl, 1, 0).is_some()); // echo builtin
    }

    // Extension-less scripts (e.g. ~/bin/letibot) are detected by shebang.
    #[test]
    fn highlights_extensionless_shebang_script() {
        let b = buf_named("letibot", "#!/usr/bin/env bash\necho \"hello\"\n");
        let mut hl = Highlighter::new();
        hl.refresh(&b);
        assert!(style_at(&hl, 0, 0).is_some()); // shebang comment
        assert!(style_at(&hl, 1, 0).is_some()); // echo builtin
    }

    #[test]
    fn scratch_buffer_has_no_highlighting() {
        let mut b = Buffer::new();
        b.lines = vec!["fn main() {}".chars().collect()];
        let mut hl = Highlighter::new();
        hl.refresh(&b);
        assert!(hl.style_at(Pos { row: 0, col: 0 }).is_none());
    }

    #[test]
    fn multiline_comment_spans_lines() {
        let b = buf_named("t.rs", "/// doc\n/// line two\nfn main() {}");
        let mut hl = Highlighter::new();
        hl.refresh(&b);
        assert!(style_at(&hl, 0, 4).is_some()); // "doc"
        assert!(style_at(&hl, 1, 4).is_some()); // "line"
    }

    // Regression: re-parsing a buffer that just shrank (backspace) used to
    // panic inside tree-sitter — the reused incremental tree still carried
    // byte offsets from the longer source.
    #[test]
    fn refresh_after_shrink_does_not_panic() {
        let mut b = buf_named("t.sh", "#!/usr/bin/env bash\nbogus_var=9\necho \"ok\"\n");
        let mut hl = Highlighter::new();
        hl.refresh(&b);
        // Delete line 1 char-by-char, re-parsing after every keystroke.
        let line = 1;
        while !b.lines[line].is_empty() {
            b.lines[line].pop();
            hl.refresh(&b);
        }
        // Grow it again to exercise the other direction too.
        b.lines[line].extend("x=1".chars());
    }
}
