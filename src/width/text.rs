//! Strings as a terminal measures them: grapheme cells, columns, wrapping
//! and truncation over text that may carry escapes. The Unicode tables live
//! once in [`super`]; this is the other walk over them: [`super::Clusters`]
//! walks the editor's `&[char]` rows (a tab advances, nothing is an escape),
//! while [`for_each_cell`] walks a `&str` where an escape is zero columns and
//! never split, and a control character breaks a cluster instead of joining
//! it. They agree on every cluster of plain text; the test
//! `the_two_walks_agree_on_plain_text` holds them to it.
//!
//! [`wrap`] carries SGR state across breaks, closing and reopening it, so
//! every line it returns is independently paintable — a painter that erases
//! to end of line does so in the current attributes.

use super::{char_width, is_regional_indicator};

/// One display cell: the escape run that precedes it, the grapheme cluster
/// itself, and how many columns it occupies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cell<'a> {
    /// Escapes immediately before this cluster: zero columns, never split.
    pub esc: &'a str,
    /// The cluster. Empty only for the trailing escape-only cell.
    pub text: &'a str,
    /// Columns occupied: 0, 1 or 2.
    pub cols: usize,
}

/// Split a string into escape-prefixed grapheme clusters. The last cell may
/// have an empty `text` when the string ends in escapes — dropping a trailing
/// reset would leave attributes open.
pub fn cells(s: &str) -> Vec<Cell<'_>> {
    // A capacity guess: this runs under every frame the head draws, so the
    // early doublings are worth skipping; a wrong guess costs a little memory.
    let mut out = Vec::with_capacity(s.len() / 2 + 1);
    for_each_cell(s, |c| out.push(c));
    out
}

/// The cluster walk, once: every cell of `s`, in order, handed to `f`. [`width`]
/// sums one field, [`cells`] keeps them all; neither allocates for the other.
/// Shared because the rules are the subtle ones (ZWJ, regional pairs, controls
/// that do not combine) — one implementation, not two to keep in step.
pub fn for_each_cell<'a>(s: &'a str, mut f: impl FnMut(Cell<'a>)) {
    let b = s.as_bytes();
    let mut i = 0usize;
    while i < b.len() {
        let esc_start = i;
        while i < b.len() && b[i] == 0x1b {
            i = skip_escape(b, i);
        }
        let esc = &s[esc_start..i];
        if i >= b.len() {
            if !esc.is_empty() {
                f(Cell {
                    esc,
                    text: "",
                    cols: 0,
                });
            }
            break;
        }
        let cluster_start = i;
        let mut cols = 0usize;
        let mut prev_ri = false;
        let mut first = true;
        while i < b.len() {
            let c = match s[i..].chars().next() {
                Some(c) => c,
                None => break,
            };
            if c == '\x1b' {
                break;
            }
            let w = char_width(c);
            if first {
                cols = w;
                first = false;
                prev_ri = is_regional_indicator(c);
                i += c.len_utf8();
                continue;
            }
            // Extend the cluster with anything that does not stand alone.
            if w == 0 {
                // A control character is zero columns but NOT a combining
                // mark: absorbing a `\n` hides a row break inside a cell.
                if is_control(c) {
                    break;
                }
                // A mark, a selector or a ZWJ; the loop continuing is what
                // pulls in whatever follows a ZWJ.
                i += c.len_utf8();
                continue;
            }
            if prev_ri && is_regional_indicator(c) {
                // A flag: exactly two indicators, two columns.
                cols = 2;
                prev_ri = false;
                i += c.len_utf8();
                continue;
            }
            // A ZWJ immediately before this one joins it into the cluster.
            if s[cluster_start..i].ends_with('\u{200d}') {
                cols = cols.max(w);
                i += c.len_utf8();
                continue;
            }
            break;
        }
        f(Cell {
            esc,
            text: &s[cluster_start..i],
            cols,
        });
    }
}

/// Bytes past the escape at `i`: CSI to its final byte, OSC to BEL or ST, the
/// two-byte forms. An unterminated sequence consumes the rest — a partially
/// arrived frame is not content.
fn skip_escape(b: &[u8], i: usize) -> usize {
    let mut j = i + 1;
    if j >= b.len() {
        return j;
    }
    match b[j] {
        b'[' => {
            j += 1;
            while j < b.len() && !(0x40..=0x7e).contains(&b[j]) {
                j += 1;
            }
            (j + 1).min(b.len())
        }
        b']' => {
            j += 1;
            while j < b.len() {
                if b[j] == 0x07 {
                    return j + 1;
                }
                if b[j] == 0x1b && j + 1 < b.len() && b[j + 1] == b'\\' {
                    return j + 2;
                }
                j += 1;
            }
            j
        }
        _ => j + 1,
    }
}

/// Columns a string occupies on a terminal, escapes excluded — a sum over the
/// walk, so it allocates nothing.
pub fn width(s: &str) -> usize {
    let mut cols = 0usize;
    for_each_cell(s, |c| cols += c.cols);
    cols
}

/// C0, C1 and DEL. Zero columns, and never part of the cluster beside them.
pub fn is_control(c: char) -> bool {
    super::is_control(c as u32)
}

/// The reset sequence, spelled once.
pub const RESET: &str = "\x1b[0m";

/// Active SGR state, so a wrapped line can reopen what the previous one
/// closed. Not a full model: it keeps the sequences since the last reset, in
/// order, and replays them — order is why a cancelling sequence replays right.
#[derive(Debug, Default, Clone)]
struct Sgr {
    open: String,
}

impl Sgr {
    fn feed(&mut self, esc: &str) {
        let b = esc.as_bytes();
        let mut i = 0;
        while i < b.len() {
            let end = skip_escape(b, i);
            let seq = &esc[i..end];
            // Only SGR affects rendition; a cursor move is not ours to replay.
            if seq.ends_with('m') && seq.starts_with("\x1b[") {
                let params = &seq[2..seq.len() - 1];
                if params.is_empty() || params == "0" {
                    self.open.clear();
                } else {
                    self.open.push_str(seq);
                }
            }
            i = end;
        }
    }

    fn is_open(&self) -> bool {
        !self.open.is_empty()
    }
}

/// Truncate to `cols` columns, never splitting a cluster or an escape. When
/// something is dropped the last column becomes `…`; any open attribute is
/// closed at the end.
pub fn truncate(s: &str, cols: usize) -> String {
    if width(s) <= cols {
        return s.to_string();
    }
    if cols == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut sgr = Sgr::default();
    let mut used = 0usize;
    // A hyperlink open where the cut lands is closed with it, like an open colour.
    let mut link_open = false;
    for c in cells(s) {
        sgr.feed(c.esc);
        out.push_str(c.esc);
        // `ESC ] 8 ; params ; URI ST` — an empty URI is the closer.
        for part in c.esc.split("\x1b]8;").skip(1) {
            let uri = part.split_once(';').map(|(_, u)| u).unwrap_or("");
            link_open = !uri.is_empty() && !uri.starts_with('\x1b') && !uri.starts_with('\x07');
        }
        if used + c.cols > cols.saturating_sub(1) {
            break;
        }
        out.push_str(c.text);
        used += c.cols;
    }
    out.push('…');
    if link_open {
        out.push_str("\x1b]8;;\x1b\\");
    }
    if sgr.is_open() {
        out.push_str(RESET);
    }
    out
}

/// Shorten to `cols` columns by eating the left, keeping the end — the one for
/// a path: `…/src/protocol.rs` names the file, `crates/…/protoco…` does not.
/// The cut lands at a separator when one is within six columns.
pub fn ellipsise_left(s: &str, cols: usize) -> String {
    if width(s) <= cols {
        return s.to_string();
    }
    if cols < 2 {
        return String::new();
    }
    let keep = cols - 1;
    let cs = cells(s);
    let mut used = 0usize;
    let mut start = cs.len();
    for (i, c) in cs.iter().enumerate().rev() {
        if used + c.cols > keep {
            start = i + 1;
            break;
        }
        used += c.cols;
        start = i;
    }
    // Nudge forward to the next separator when it is close — bounded, so a
    // long argument between separators is not eaten whole.
    const NUDGE: usize = 6;
    if start < cs.len() && !cs[start].text.starts_with('/') {
        match cs[start..].iter().position(|c| c.text.starts_with('/')) {
            Some(next) if next <= NUDGE => start += next,
            _ => {}
        }
    }
    let mut out = String::from("…");
    for c in &cs[start..] {
        out.push_str(c.esc);
        out.push_str(c.text);
    }
    out
}

/// Pad to exactly `cols` columns with spaces, truncating if too long.
pub fn fit(s: &str, cols: usize) -> String {
    let w = width(s);
    if w > cols {
        return truncate(s, cols);
    }
    let mut out = s.to_string();
    out.push_str(&" ".repeat(cols - w));
    out
}

/// Wrap to `cols` columns. Three break rules, in priority: at a space;
/// between two wide clusters (without which CJK does not wrap at all);
/// anywhere, when one unbreakable run is longer than the width — overflowing
/// instead is the same corruption deferred to the painter. Every returned
/// line is independently paintable: escapes carried, attributes closed and
/// reopened.
pub fn wrap(s: &str, cols: usize) -> Vec<String> {
    let cs = cells(s);
    let rows = break_cells(&cs, cols.max(4));
    let mut sgr = Sgr::default();
    let mut out = Vec::with_capacity(rows.len());
    for (a, b) in rows {
        let mut line = String::new();
        if sgr.is_open() {
            line.push_str(&sgr.open);
        }
        for c in &cs[a..b] {
            sgr.feed(c.esc);
            line.push_str(c.esc);
            line.push_str(c.text);
        }
        // Trailing space at a break is invisible; a trailing newline is a byte
        // the terminal acts on.
        while line.ends_with(' ') || line.ends_with('\n') || line.ends_with('\r') {
            line.pop();
        }
        if sgr.is_open() {
            line.push_str(RESET);
        }
        out.push(line);
    }
    out
}

/// The single breakpoint finder: [`wrap`] and [`wrap_ranges`] both go through
/// it, so the two cannot drift. Returns half-open cell-index ranges that tile
/// the input; a trailing space belongs to the row it ended, so a row may be
/// one column over `cols` in trailing whitespace only.
pub(crate) fn break_cells(cs: &[Cell], cols: usize) -> Vec<(usize, usize)> {
    let mut rows: Vec<(usize, usize)> = Vec::new();
    let mut row_start = 0usize;
    let mut used = 0usize;
    let mut word_start: Option<usize> = None;
    let mut word_cols = 0usize;

    for (i, c) in cs.iter().enumerate() {
        if c.text.is_empty() {
            continue; // escape-only cell: it styles, it does not occupy
        }
        // A newline is a hard break, here rather than in a caller —
        // `char_width` measures it as zero columns, so without this a `\n`
        // survives into a row the terminal then obeys. The break ends the row
        // it belongs to, so the ranges still tile.
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

        if word_start.is_none() {
            word_start = Some(i);
            word_cols = 0;
        }
        if word_cols + c.cols > cols {
            // The word alone is longer than a whole row: place what fits and
            // hard-break here.
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

/// Wrap, returning byte ranges rather than strings — the editor needs to know
/// which bytes are on which row. Same breakpoints as [`wrap`], both through
/// [`break_cells`]; the ranges tile the input, and a trailing space belongs to
/// the row it ended.
pub fn wrap_ranges(s: &str, cols: usize) -> Vec<std::ops::Range<usize>> {
    let cs = cells(s);
    // Byte offset of each cell's start, plus the end sentinel.
    let mut at = Vec::with_capacity(cs.len() + 1);
    let mut off = 0usize;
    for c in &cs {
        at.push(off);
        off += c.esc.len() + c.text.len();
    }
    at.push(s.len());
    break_cells(&cs, cols.max(4))
        .into_iter()
        .map(|(a, b)| at[a]..at[b])
        .collect()
}

/// The row a byte offset lands on, and the display column within it — display
/// columns, not characters and not bytes.
pub fn locate(s: &str, byte: usize, cols: usize) -> (usize, usize) {
    let rows = wrap_ranges(s, cols);
    let byte = byte.min(s.len());
    let row = rows.iter().rposition(|r| r.start <= byte).unwrap_or(0);
    (row, width(&s[rows[row].start..byte]))
}

/// The byte offset at display column `col` of row `row`, clamping into the
/// row: a cursor moved onto a shorter row lands at its end.
pub fn offset_at(s: &str, row: usize, col: usize, cols: usize) -> usize {
    let rows = wrap_ranges(s, cols);
    let r = match rows.get(row) {
        Some(r) => r.clone(),
        None => return s.len(),
    };
    let mut used = 0usize;
    let mut at = r.start;
    for c in cells(&s[r.clone()]) {
        if used >= col {
            return at;
        }
        at += c.esc.len() + c.text.len();
        used += c.cols;
    }
    // Not past a trailing break space or the row's newline: end of row n and
    // start of row n+1 are different places.
    let mut end = r.end;
    while end > r.start && matches!(s.as_bytes()[end - 1], b' ' | b'\n' | b'\r') {
        end -= 1;
    }
    end.max(r.start)
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_newline_is_a_row_break_and_never_reaches_the_terminal() {
        // Without the hard break, a `\n` survives into a row the terminal obeys.
        let s = "first line\nsecond line";
        assert_eq!(wrap(s, 40), vec!["first line", "second line"]);
        assert_eq!(wrap_ranges(s, 40), vec![0..11, 11..22]);
        assert_eq!(locate(s, 11, 40), (1, 0));
        assert_eq!(locate(s, 10, 40), (0, 10));
        assert_eq!(wrap("a\n", 40), vec!["a", ""]);
        assert_eq!(locate("a\n", 2, 40), (1, 0));
        assert_eq!(offset_at(s, 0, 99, 40), 10);
        for l in wrap("a very long first line that wraps\nand a second", 12) {
            assert!(!l.contains('\n'), "{l:?}");
            assert!(width(&l) <= 12, "{l:?}");
        }
    }

    #[test]
    fn wrap_ranges_tile_the_input_and_agree_with_wrap() {
        let inputs = [
            "the quick brown fox jumps over the lazy dog",
            "你好世界，这是一个测试。abc def",
            "short",
            "",
            &"x".repeat(120),
            "a bb ccc dddd eeeee ffffff ggggggg",
            "two\nlines",
            "trailing\n",
            "\n\n",
        ];
        for s in inputs {
            for w in [5usize, 8, 13, 40] {
                let r = wrap_ranges(s, w);
                assert_eq!(r[0].start, 0, "{s:?} @{w}");
                assert_eq!(r.last().unwrap().end, s.len(), "{s:?} @{w}");
                for pair in r.windows(2) {
                    assert!(pair[0].end <= pair[1].start, "{s:?} @{w}: {r:?}");
                }
                for range in &r {
                    let slice = &s[range.clone()];
                    assert!(
                        width(slice.trim_end()) <= w,
                        "{s:?} @{w}: {range:?} = {slice:?}"
                    );
                }
                // Same breakpoints by construction; asserted because that is a
                // claim about code that changes.
                let strings = wrap(s, w);
                assert_eq!(r.len(), strings.len(), "{s:?} @{w}: {r:?}");
                for (range, line) in r.iter().zip(&strings) {
                    assert_eq!(s[range.clone()].trim_end(), line, "{s:?} @{w}");
                }
            }
        }
    }

    #[test]
    fn a_cjk_char_is_two_columns() {
        assert_eq!(width("你好"), 4);
        assert_eq!(width("abc"), 3);
        // Fullwidth ASCII, which a model emits when quoting Japanese source.
        assert_eq!(width("ＡＢ"), 4);
    }

    #[test]
    fn a_combining_mark_is_not_a_column() {
        assert_eq!(width("e\u{301}"), 1);
        assert_eq!(cells("e\u{301}").len(), 1);
    }

    #[test]
    fn a_zwj_emoji_sequence_is_one_cluster() {
        let s = "\u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467}";
        let cs = cells(s);
        assert_eq!(cs.len(), 1, "{cs:?}");
        assert_eq!(cs[0].cols, 2);
    }

    #[test]
    fn a_flag_is_one_cluster_of_two_columns() {
        let s = "\u{1f1ef}\u{1f1f5}"; // JP
        let cs = cells(s);
        assert_eq!(cs.len(), 1);
        assert_eq!(cs[0].cols, 2);
    }

    #[test]
    fn escapes_are_zero_columns_and_are_never_split() {
        let s = "\x1b[1mbold\x1b[0m";
        assert_eq!(width(s), 4);
        let cs = cells(s);
        assert_eq!(cs[0].esc, "\x1b[1m");
        assert_eq!(cs.last().unwrap().esc, "\x1b[0m");
        assert_eq!(cs.last().unwrap().text, "");
    }

    #[test]
    fn truncation_does_not_cut_a_cluster_in_half() {
        let s = "a\u{301}b\u{301}c\u{301}";
        let t = truncate(s, 2);
        assert_eq!(width(&t), 2);
        assert!(t.starts_with("a\u{301}"), "{t:?}");
    }

    #[test]
    fn truncation_closes_an_open_attribute() {
        let t = truncate("\x1b[1maaaaaaaa", 4);
        assert!(t.ends_with(RESET), "{t:?}");
        assert_eq!(width(&t), 4);
    }

    #[test]
    fn cjk_prose_wraps_even_though_it_has_no_spaces() {
        let doc = "你好".repeat(20);
        let lines = wrap(&doc, 20);
        assert!(lines.len() >= 4, "{} lines", lines.len());
        for l in &lines {
            assert!(width(l) <= 20, "{:?} is {} columns", l, width(l));
        }
    }

    #[test]
    fn an_unbreakable_run_is_hard_broken_not_overflowed() {
        let url = "https://example.com/".to_string() + &"a".repeat(200);
        let lines = wrap(&url, 40);
        assert!(lines.len() >= 5);
        for l in &lines {
            assert!(width(l) <= 40, "{} columns: {l:?}", width(l));
        }
        let rejoined: String = lines.concat();
        assert_eq!(rejoined, url, "hard breaking must not lose characters");
    }

    #[test]
    fn every_wrapped_line_is_independently_paintable() {
        // A painter that erases to end of line does so in the current attributes.
        let s = format!("\x1b[1m{}\x1b[0m", "word ".repeat(20));
        let lines = wrap(&s, 20);
        assert!(lines.len() > 1);
        for l in &lines[..lines.len() - 1] {
            assert!(l.ends_with(RESET), "line left bold open: {l:?}");
            assert!(l.starts_with("\x1b[1m"), "line did not reopen bold: {l:?}");
        }
    }

    #[test]
    fn wrapping_never_exceeds_the_width_for_any_input() {
        let inputs = [
            "the quick brown fox jumps over the lazy dog",
            "你好世界，这是一个测试。",
            "mixed 混合 text with émojis 🎉 and \x1b[31mcolour\x1b[0m",
            "a",
            "",
            &"x".repeat(300),
        ];
        for s in inputs {
            for w in [4usize, 7, 20, 41, 80] {
                for l in wrap(s, w) {
                    assert!(width(&l) <= w, "{s:?} at {w}: {l:?} is {}", width(&l));
                }
            }
        }
    }

    #[test]
    fn fit_pads_to_exactly_the_width() {
        assert_eq!(width(&fit("ab", 5)), 5);
        assert_eq!(width(&fit("你好世界", 5)), 5);
        assert_eq!(width(&fit("", 3)), 3);
    }

    /// The two walks over the one table agree wherever both can see the text
    /// (they differ by design on tabs, escapes and controls).
    #[test]
    fn the_two_walks_agree_on_plain_text() {
        for s in [
            "abc",
            "中文字",
            "e\u{301}x",
            "\u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467}",
            "\u{1f1eb}\u{1f1f7}\u{1f1ee}\u{1f1f3}",
            "mixed 混合 émojis 🎉 ok",
            "\u{10EEEE}\u{305}\u{305}\u{10EEEE}",
        ] {
            let chars: Vec<char> = s.chars().collect();
            let editor: Vec<(String, usize)> = super::super::clusters(&chars, 8)
                .iter()
                .map(|c| (chars[c.start..c.end].iter().collect(), c.w))
                .collect();
            let text: Vec<(String, usize)> = cells(s)
                .iter()
                .map(|c| (c.text.to_string(), c.cols))
                .collect();
            assert_eq!(editor, text, "{s:?}");
        }
    }

    /// A link cut by the truncation is closed, or the rest of the row would be one link.
    #[test]
    fn truncate_closes_a_link_it_cuts() {
        let s = "\x1b]8;;https://x.org\x1b\\a long link text\x1b]8;;\x1b\\";
        let t = truncate(s, 6);
        assert!(t.ends_with("…\x1b]8;;\x1b\\"), "{t:?}");
        assert_eq!(width(&t), 6);
        assert_eq!(truncate(s, 40), s);
    }
}
