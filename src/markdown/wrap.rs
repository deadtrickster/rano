//! Wrapping and truncating role-tagged text by display columns.
//!
//! Ported from letibot's `crates/ui/src/width.rs` (`wrap`, `truncate` and the
//! breakpoint finder they share), which walked ANSI strings and had to carry the
//! open SGR state across each break so every row stayed independently paintable.
//! Spans carry their meaning with them, so that half has no counterpart here: a
//! piece of a span cut onto the next row is still the same role. What remains is
//! the measuring — clusters by [`crate::width`], so a wide character is two
//! columns and a combining mark never lands alone on a row — and the three break
//! rules below.

use super::line::{MdSpan, push_span};

/// One display cell: a grapheme cluster, the span it came from, its columns.
struct Cell<'a> {
    span: usize,
    text: &'a str,
    cols: usize,
}

fn cells(spans: &[MdSpan]) -> Vec<Cell<'_>> {
    let mut out = Vec::new();
    for (i, s) in spans.iter().enumerate() {
        let chars: Vec<char> = s.text.chars().collect();
        // Byte offset of every char, plus the end, so a cluster's char range
        // becomes a slice of the span's own text.
        let mut at: Vec<usize> = s.text.char_indices().map(|(b, _)| b).collect();
        at.push(s.text.len());
        // A **control character is not a combining mark**: it measures zero
        // columns for the same reason one does, and that is the whole of the
        // resemblance. The editor's cluster walk lets it join the cluster before
        // it (a `\n` can never reach an editor row), so it is cut out here and
        // stands as its own cell — otherwise `a\nb` is one cluster and the hard
        // break below never sees its newline.
        let mut from = 0;
        while from < chars.len() {
            let to = (from..chars.len())
                .find(|&k| chars[k].is_control())
                .unwrap_or(chars.len());
            for c in crate::width::Clusters::new(&chars[from..to], 1) {
                out.push(Cell {
                    span: i,
                    text: &s.text[at[from + c.start]..at[from + c.end]],
                    cols: c.w,
                });
            }
            if to < chars.len() {
                out.push(Cell {
                    span: i,
                    text: &s.text[at[to]..at[to + 1]],
                    cols: usize::from(chars[to] == '\t'),
                });
            }
            from = to + 1;
        }
    }
    out
}

/// Wrap to `cols` columns.
///
/// Three break rules, in priority order:
///
/// 1. At a space, the ordinary case.
/// 2. Between two wide clusters, which is what makes CJK wrap at all. A
///    space-only wrapper returns one 400-column line for a paragraph of Chinese,
///    and that line then scrolls the whole screen sideways.
/// 3. Anywhere, when a single unbreakable run is longer than the width — a URL,
///    a base64 blob, a 200-character type signature. Overflowing instead is not
///    a gentler failure: it is the same corruption, deferred to the painter.
///
/// Trailing blanks at a break are dropped: invisible, and a painter that fills a
/// row's background would paint one column further than the text goes.
pub fn wrap(spans: &[MdSpan], cols: usize) -> Vec<Vec<MdSpan>> {
    let cs = cells(spans);
    let rows = break_cells(&cs, cols.max(4));
    let mut out = Vec::with_capacity(rows.len());
    for (a, mut b) in rows {
        while b > a && matches!(cs[b - 1].text, " " | "\n" | "\r") {
            b -= 1;
        }
        out.push(collect(spans, &cs[a..b]));
    }
    out
}

fn collect(spans: &[MdSpan], cs: &[Cell]) -> Vec<MdSpan> {
    let mut row: Vec<MdSpan> = Vec::new();
    for c in cs {
        let s = &spans[c.span];
        push_span(
            &mut row,
            MdSpan {
                text: c.text.to_string(),
                role: s.role,
                attrs: s.attrs,
            },
        );
    }
    row
}

/// Truncate to `cols` columns, never splitting a cluster.
///
/// When something is dropped the last column becomes `…`, so the elision is
/// visible rather than silent. It takes the look of the first cell it replaces.
pub fn truncate(spans: &[MdSpan], cols: usize) -> Vec<MdSpan> {
    let cs = cells(spans);
    if cs.iter().map(|c| c.cols).sum::<usize>() <= cols {
        return spans.to_vec();
    }
    if cols == 0 {
        return Vec::new();
    }
    let mut used = 0usize;
    let mut cut = cs.len();
    for (i, c) in cs.iter().enumerate() {
        if used + c.cols > cols - 1 {
            cut = i;
            break;
        }
        used += c.cols;
    }
    let mut row = collect(spans, &cs[..cut]);
    let look = &spans[cs[cut.min(cs.len() - 1)].span];
    push_span(
        &mut row,
        MdSpan {
            text: "…".into(),
            role: look.role,
            attrs: look.attrs,
        },
    );
    row
}

/// The single breakpoint finder, letibot's `break_cells` unchanged but for the
/// cell type.
///
/// Returns half-open **cell index** ranges that tile the input. A trailing space
/// belongs to the row it ended, so a row's slice may be one column over `cols`
/// *in trailing whitespace only* — the usual line-breaking contract.
fn break_cells(cs: &[Cell], cols: usize) -> Vec<(usize, usize)> {
    let mut rows: Vec<(usize, usize)> = Vec::new();
    let mut row_start = 0usize;
    let mut used = 0usize;
    let mut word_start: Option<usize> = None;
    let mut word_cols = 0usize;

    for (i, c) in cs.iter().enumerate() {
        if c.text.is_empty() {
            continue;
        }
        // A newline is a **hard break**, and it has to be one here rather than in
        // a caller: it measures zero columns, so without this a two-line run
        // wraps to a single row with a literal `\n` inside it — which the
        // terminal then obeys, putting a row on the screen the host did not
        // count. The break belongs to the row it *ends*, so the ranges still
        // tile the input; [`wrap`] strips it.
        if c.text == "\n" {
            if let Some(ws) = word_start.take() {
                if used > 0 && used + word_cols > cols {
                    rows.push((row_start, ws));
                    row_start = ws;
                }
                word_cols = 0;
            }
            rows.push((row_start, i + 1));
            row_start = i + 1;
            used = 0;
            continue;
        }
        let space = c.text == " " || c.text == "\t";
        let wide = c.cols == 2;

        if space || wide {
            if let Some(ws) = word_start.take() {
                if used > 0 && used + word_cols > cols {
                    rows.push((row_start, ws));
                    row_start = ws;
                    used = 0;
                }
                used += word_cols;
                word_cols = 0;
            }
            if space {
                used += c.cols;
                if used > cols {
                    rows.push((row_start, i + 1));
                    row_start = i + 1;
                    used = 0;
                }
            } else {
                if used > 0 && used + c.cols > cols {
                    rows.push((row_start, i));
                    row_start = i;
                    used = 0;
                }
                used += c.cols;
            }
            continue;
        }

        // An ordinary cluster joins the pending word.
        if word_start.is_none() {
            word_start = Some(i);
            word_cols = 0;
        }
        if word_cols + c.cols > cols {
            // The word alone is longer than a whole row. Place what there is,
            // then hard-break here — overflowing instead is the same corruption
            // deferred to the painter.
            let ws = word_start.unwrap();
            if used > 0 && used + word_cols > cols {
                rows.push((row_start, ws));
                row_start = ws;
            }
            rows.push((row_start, i));
            row_start = i;
            used = 0;
            word_start = Some(i);
            word_cols = 0;
        }
        word_cols += c.cols;
    }
    if let Some(ws) = word_start
        && used > 0
        && used + word_cols > cols
    {
        rows.push((row_start, ws));
        row_start = ws;
    }
    rows.push((row_start, cs.len()));
    rows
}

#[cfg(test)]
mod tests {
    use super::super::line::{Attrs, MdLine, spans_width, text_width};
    use super::*;
    use crate::style::Role;

    fn texts(rows: &[Vec<MdSpan>]) -> Vec<String> {
        rows.iter().map(|r| MdLine::new(r.clone()).text()).collect()
    }

    #[test]
    fn words_wrap_at_spaces_and_trailing_blanks_go() {
        let rows = wrap(&[MdSpan::plain("one two three four")], 9);
        assert_eq!(texts(&rows), ["one two", "three", "four"]);
    }

    /// A piece of a styled span cut onto the next row is still that span's role.
    #[test]
    fn a_role_survives_the_break() {
        let spans = [
            MdSpan::plain("a "),
            MdSpan::new("long code span", Role::Code),
        ];
        let rows = wrap(&spans, 8);
        assert_eq!(texts(&rows), ["a long", "code", "span"]);
        assert_eq!(rows[1][0].role, Role::Code);
        assert_eq!(rows[0][1].role, Role::Code);
    }

    #[test]
    fn a_wide_character_measures_two_columns_and_wraps() {
        // The bug the whole width swap was for: a line of CJK counted at half its
        // real width, wrapped by the terminal into a row nobody counted.
        assert_eq!(text_width("你好"), 4);
        assert_eq!(text_width("héllo"), 5);
        let rows = wrap(&[MdSpan::plain("你好世界这是一个测试用的句子没有空格")], 10);
        assert!(rows.len() > 1);
        for r in &rows {
            assert!(spans_width(r) <= 10, "{r:?}");
        }
        // …and a truncation never splits a cluster.
        assert_eq!(spans_width(&truncate(&[MdSpan::plain("你好世界")], 5)), 5);
    }

    #[test]
    fn an_unbreakable_run_is_hard_broken() {
        let rows = wrap(&[MdSpan::plain("abcdefghij")], 4);
        assert_eq!(texts(&rows), ["abcd", "efgh", "ij"]);
    }

    #[test]
    fn a_newline_is_a_hard_break() {
        let rows = wrap(&[MdSpan::plain("a\nb")], 20);
        assert_eq!(texts(&rows), ["a", "b"]);
    }

    #[test]
    fn truncation_marks_what_it_dropped_in_the_look_it_dropped() {
        let bold = Attrs {
            bold: true,
            ..Attrs::NONE
        };
        let t = truncate(
            &[MdSpan::plain("ab"), MdSpan::plain("cdef").with_attrs(bold)],
            4,
        );
        assert_eq!(MdLine::new(t.clone()).text(), "abc…");
        assert!(t.last().unwrap().attrs.bold);
        assert_eq!(truncate(&[MdSpan::plain("abc")], 3).len(), 1);
    }
}
