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

/// Map a file name to its language by extension (scratch buffers get none).
pub fn detect(name: Option<&Path>) -> Option<Lang> {
    let ext = name?.extension()?.to_str()?;
    match ext.to_ascii_lowercase().as_str() {
        "rs" => Some(Lang::Rust),
        "go" => Some(Lang::Go),
        "sh" | "bash" => Some(Lang::Bash),
        "py" | "pyw" => Some(Lang::Python),
        "c" | "h" => Some(Lang::C),
        "json" => Some(Lang::Json),
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
}

impl Highlighter {
    pub fn new() -> Self {
        Self {
            parser: Parser::new(),
            query: None,
            query_lang: None,
            line_styles: Vec::new(),
        }
    }

    /// Re-parse the buffer and rebuild the style grid. No-op for scratch
    /// buffers (no file name, no language).
    pub fn refresh(&mut self, buf: &Buffer) {
        let Some(lang) = detect(buf.name.as_deref()) else {
            self.line_styles.clear();
            return;
        };

        let source = buf.text();
        if self.parser.set_language(&lang.language()).is_err() {
            self.line_styles.clear();
            return;
        }

        // Always do a full parse. Reusing the previous tree for incremental
        // parsing leaks byte offsets from the old (possibly longer) source
        // into the new tree, which panics inside tree-sitter when a line is
        // shortened. Full re-parses are fast enough for editor-sized buffers.
        let Some(tree) = self.parser.parse(source.as_bytes(), None) else {
            self.line_styles.clear();
            return;
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
                    self.line_styles.clear();
                    return;
                }
            }
        }
        let Some(query) = self.query.as_ref() else {
            self.line_styles.clear();
            return;
        };

        self.line_styles = Self::build_styles(&buf.lines, &source, &tree, query);
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

    fn build_styles(
        lines: &[Vec<char>],
        source: &str,
        tree: &Tree,
        query: &Query,
    ) -> Vec<Vec<Style>> {
        // Style grid mirroring `lines`.
        let mut line_styles: Vec<Vec<Style>> = lines
            .iter()
            .map(|l| vec![Style::default(); l.len()])
            .collect();

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

        // Byte offset of each line start within `source`.
        let mut line_offsets = Vec::with_capacity(lines.len());
        let mut off = 0usize;
        for co in char_offsets.iter() {
            line_offsets.push(off);
            off += co.len() - 1 + 1; // line bytes + '\n'
        }

        let names = query.capture_names();
        let mut cursor = QueryCursor::new();
        let mut caps = cursor.captures(query, tree.root_node(), source.as_bytes());
        while let Some((m, i)) = caps.next() {
            let cap = m.captures()[*i];
            let name = names.get(cap.index as usize).copied().unwrap_or("");
            let style = theme(name);
            if style == Style::default() {
                continue;
            }
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
            for r in r0..=r1.min(line_styles.len() - 1) {
                let lo = line_offsets[r];
                let line_byte_len = char_offsets[r].len() - 1;
                let s_l = s.max(lo);
                let e_l = e.min(lo + line_byte_len);
                if s_l >= e_l {
                    continue;
                }
                let offs = &char_offsets[r];
                // Char index containing byte `b` (node ranges are half-open).
                let ci = |b: usize| offs.partition_point(|&o| o <= b).saturating_sub(1);
                let cs = ci(s_l - lo);
                let ce = (ci(e_l - lo - 1) + 1).min(line_styles[r].len());
                for cell in line_styles[r][cs..ce].iter_mut() {
                    *cell = style;
                }
            }
        }
        line_styles
    }
}

impl Default for Highlighter {
    fn default() -> Self {
        Self::new()
    }
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
    fn detects_languages() {
        assert_eq!(detect(Some(Path::new("a/b.rs"))), Some(Lang::Rust));
        assert_eq!(detect(Some(Path::new("main.go"))), Some(Lang::Go));
        assert_eq!(detect(Some(Path::new("run.SH"))), Some(Lang::Bash));
        assert_eq!(detect(Some(Path::new("x.py"))), Some(Lang::Python));
        assert_eq!(detect(Some(Path::new("x.pyw"))), Some(Lang::Python));
        assert_eq!(detect(Some(Path::new("x.c"))), Some(Lang::C));
        assert_eq!(detect(Some(Path::new("x.h"))), Some(Lang::C));
        assert_eq!(detect(Some(Path::new("x.json"))), Some(Lang::Json));
        assert_eq!(detect(None), None);
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
        hl.refresh(&b);
        // Reaching here without a panic is the assertion.
    }
}
