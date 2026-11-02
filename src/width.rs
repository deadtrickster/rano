//! Everything that measures text goes through here: the soft-wrap table,
//! horizontal scroll, the cursor, mouse hit-testing, the renderer's windows.
//! CJK and most emoji take two columns; combining marks and joiners take
//! none — so `col == display_col` is false, and wrapping must move whole
//! clusters. The tables are by hand, not UAX #11/#29: a character outside the
//! listed ranges is one column and its own cluster, and a miss is a line one
//! column narrow, once. Tabs advance from the line start, not the wrap
//! segment, so a wrapped line's width is the sum of its segments exactly.

// The same tables, walked over a `&str` that may carry escapes; a child
// module so the tables stay private and stay one copy.
pub mod text;

/// Columns one character claims on a terminal: 0, 1 or 2.
pub fn char_width(c: char) -> usize {
    let u = c as u32;
    // C0/C1 and DEL: not a column.
    if is_control(u) {
        return 0;
    }
    if is_zero_width(u) {
        return 0;
    }
    if is_wide(u) { 2 } else { 1 }
}

/// C0, C1 and DEL — zero columns, and never part of the cluster beside them.
fn is_control(u: u32) -> bool {
    u < 0x20 || (0x7f..0xa0).contains(&u)
}

/// Marks, joiners and selectors: they attach to the character before them
/// instead of taking a cell of their own.
fn is_zero_width(u: u32) -> bool {
    matches!(u,
        0x0300..=0x036f      // combining diacritical marks
        | 0x0483..=0x0489    // Cyrillic combining
        | 0x0591..=0x05bd | 0x05bf | 0x05c1..=0x05c2 | 0x05c4..=0x05c5 | 0x05c7
        | 0x0610..=0x061a | 0x064b..=0x065f | 0x0670
        | 0x06d6..=0x06dc | 0x06df..=0x06e4 | 0x06e7..=0x06e8 | 0x06ea..=0x06ed
        | 0x0900..=0x0903 | 0x093a..=0x093c | 0x0941..=0x0948 | 0x094d
        | 0x0951..=0x0957 | 0x0962..=0x0963
        | 0x0e31 | 0x0e34..=0x0e3a | 0x0e47..=0x0e4e
        | 0x1ab0..=0x1aff | 0x1dc0..=0x1dff
        | 0x200b..=0x200f    // ZWSP, ZWNJ, ZWJ, LRM, RLM
        | 0x2028..=0x202e | 0x2060..=0x2064 | 0x20d0..=0x20f0
        | 0xfe00..=0xfe0f    // variation selectors
        | 0xfe20..=0xfe2f | 0xfeff
        | 0xe0100..=0xe01ef  // variation selectors supplement
    )
}

/// [`char_width`]'s decision for a bare codepoint, for a caller scanning
/// bytes; two copies of the table would drift.
#[allow(dead_code)]
pub fn is_wide_cp(u: u32) -> bool {
    is_wide(u)
}

/// East-Asian Wide and Fullwidth, plus the emoji planes terminals render
/// double-width.
fn is_wide(u: u32) -> bool {
    matches!(u,
        0x1100..=0x115f      // Hangul Jamo initial
        | 0x231a..=0x231b | 0x23e9..=0x23ec | 0x23f0 | 0x23f3
        | 0x25fd..=0x25fe | 0x2614..=0x2615 | 0x2648..=0x2653
        | 0x267f | 0x2693 | 0x26a1 | 0x26aa..=0x26ab | 0x26bd..=0x26be
        | 0x26c4..=0x26c5 | 0x26ce | 0x26d4 | 0x26ea | 0x26f2..=0x26f3
        | 0x26f5 | 0x26fa | 0x26fd | 0x2705 | 0x270a..=0x270b | 0x2728
        | 0x274c | 0x274e | 0x2753..=0x2755 | 0x2757 | 0x2795..=0x2797
        | 0x27b0 | 0x27bf | 0x2b1b..=0x2b1c | 0x2b50 | 0x2b55
        | 0x2e80..=0x2e99 | 0x2e9b..=0x2ef3 | 0x2f00..=0x2fd5 | 0x2ff0..=0x2ffb
        | 0x3000..=0x303e    // CJK symbols and punctuation
        | 0x3041..=0x3096 | 0x3099..=0x30ff | 0x3105..=0x312f | 0x3131..=0x318e
        | 0x3190..=0x31e3 | 0x31f0..=0x321e | 0x3220..=0x3247 | 0x3250..=0x4dbf
        | 0x4e00..=0xa48c    // CJK unified ideographs, Yi
        | 0xa490..=0xa4c6 | 0xa960..=0xa97c | 0xac00..=0xd7a3 | 0xf900..=0xfaff
        | 0xfe10..=0xfe19 | 0xfe30..=0xfe52 | 0xfe54..=0xfe66 | 0xfe68..=0xfe6b
        | 0xff01..=0xff60    // fullwidth forms
        | 0xffe0..=0xffe6
        | 0x16fe0..=0x16fe4 | 0x17000..=0x18d08 | 0x1b000..=0x1b2fb
        | 0x1f004 | 0x1f0cf | 0x1f18e | 0x1f191..=0x1f19a
        | 0x1f1e6..=0x1f1ff  // regional indicators
        | 0x1f200..=0x1f320 | 0x1f32d..=0x1f335 | 0x1f337..=0x1f37c
        | 0x1f37e..=0x1f393 | 0x1f3a0..=0x1f3ca | 0x1f3cf..=0x1f3d3
        | 0x1f3e0..=0x1f3f0 | 0x1f3f4 | 0x1f3f8..=0x1f43e | 0x1f440
        | 0x1f442..=0x1f4fc | 0x1f4ff..=0x1f53d | 0x1f54b..=0x1f54e
        | 0x1f550..=0x1f567 | 0x1f57a | 0x1f595..=0x1f596 | 0x1f5a4
        | 0x1f5fb..=0x1f64f | 0x1f680..=0x1f6c5 | 0x1f6cc
        | 0x1f6d0..=0x1f6d2 | 0x1f6d5..=0x1f6d7 | 0x1f6eb..=0x1f6ec
        | 0x1f6f4..=0x1f6fc | 0x1f7e0..=0x1f7eb
        | 0x1f90c..=0x1f93a | 0x1f93c..=0x1f945 | 0x1f947..=0x1f9ff
        | 0x1fa70..=0x1faff
        | 0x20000..=0x3fffd  // CJK extension B and beyond
    )
}

/// True when every character is exactly one column and no tab is present, so
/// `display col == char index` holds — the case the fast paths assume.
pub fn is_simple(chars: &[char]) -> bool {
    is_simple_prefix(chars, usize::MAX)
}

/// [`is_simple`] over at most the first `limit` characters, for rows measured
/// in megabytes where the scan itself is the cost and the caller has a cheap
/// fallback. The trade: a row longer than `limit` whose prefix is ordinary is
/// assumed ordinary for its whole length.
pub fn is_simple_prefix(chars: &[char], limit: usize) -> bool {
    // ASCII and not a tab: the answer `char_width(c) == 1` gives, without the
    // range checks. A one-column non-ASCII character (é) answers false —
    // conservative, and what makes the scan cheap.
    chars[..limit.min(chars.len())]
        .iter()
        .all(|&c| c.is_ascii() && c != '\t')
}

/// One cluster: where it starts, and the display column its first cell sits
/// at. `w` is its own width, 0/1/2.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cluster {
    /// Char index of the base.
    pub start: usize,
    /// Char index just past the trailing characters that joined it.
    pub end: usize,
    /// Display column the cluster's first cell occupies.
    pub disp: usize,
    /// Columns it occupies.
    pub w: usize,
}

fn is_regional_indicator(c: char) -> bool {
    (0x1f1e6..=0x1f1ff).contains(&(c as u32))
}

/// The clusters of a line, in order, with their absolute display columns.
/// A character joins the cluster before it when it is zero-width, when the
/// character before it was a ZWJ (`👨‍👩‍👧` is five code points and one glyph),
/// or as the second of two regional indicators; a tab and a zero-width
/// character with nothing to attach to are clusters of their own. Width is a
/// maximum over parts, never a sum.
pub fn clusters(chars: &[char], tab_width: usize) -> Vec<Cluster> {
    Clusters::new(chars, tab_width).collect()
}

/// [`clusters`] as a lazy iterator, so a caller that stops early does not walk
/// the tail; one implementation, so the forms cannot drift.
pub struct Clusters<'a> {
    chars: &'a [char],
    tab_width: usize,
    i: usize,
    disp: usize,
}

impl<'a> Clusters<'a> {
    pub fn new(chars: &'a [char], tab_width: usize) -> Self {
        Self {
            chars,
            tab_width: tab_width.max(1),
            i: 0,
            disp: 0,
        }
    }
}

impl Iterator for Clusters<'_> {
    type Item = Cluster;

    fn next(&mut self) -> Option<Cluster> {
        let chars = self.chars;
        let tw = self.tab_width;
        let start = self.i;
        let c = *chars.get(start)?;
        if c == '\t' {
            let w = tw - (self.disp % tw);
            let disp = self.disp;
            self.disp += w;
            self.i = start + 1;
            return Some(Cluster {
                start,
                end: start + 1,
                disp,
                w,
            });
        }
        // Scan the whole cluster before yielding it: the parts that join are
        // only known once seen.
        let mut w = char_width(c);
        let mut end = start + 1;
        while let Some(&n) = chars.get(end) {
            // A tab never joins: it is its own cluster with a real advance.
            let nw = char_width(n);
            let joins = n != '\t'
                && (nw == 0
                    || chars[end - 1] == '\u{200d}'
                    // A flag is exactly TWO indicators; three are two clusters.
                    || (end == start + 1
                        && is_regional_indicator(chars[start])
                        && is_regional_indicator(n)));
            if !joins {
                break;
            }
            w = w.max(nw);
            end += 1;
        }
        let disp = self.disp;
        self.disp += w;
        self.i = end;
        Some(Cluster {
            start,
            end,
            disp,
            w,
        })
    }
}

/// Rendered width of a line: the sum of its cluster widths, tabs measured
/// from the line start. A ZWJ sequence counts as its one glyph, not its parts.
pub fn width(chars: &[char], tab_width: usize) -> usize {
    clusters(chars, tab_width).iter().map(|c| c.w).sum()
}

/// Where each wrap segment of a line begins: `(char index, display column)`,
/// always at least one entry, for a viewport `view_w` columns wide. Greedy
/// over whole clusters — never a split wide character or a base from its
/// marks; a cluster wider than the viewport overflows its segment rather than
/// becoming a row per character. The display column travels with the index
/// because a segment's start is not `seg * view_w` once characters are wider
/// than one column.
pub fn segments(chars: &[char], tab_width: usize, view_w: usize) -> Vec<(usize, usize)> {
    let vw = view_w.max(1);
    let cls = clusters(chars, tab_width);
    if cls.is_empty() {
        return vec![(0, 0)];
    }
    let mut starts = vec![(cls[0].start, cls[0].disp)];
    let mut used = 0usize;
    for c in &cls {
        if used > 0 && used + c.w > vw && used <= vw {
            // Start a new segment here and measure from it; a tab's width
            // depends on the absolute column, so only the fill resets.
            starts.push((c.start, c.disp));
            used = c.w;
        } else {
            used += c.w;
        }
    }
    starts
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cs(s: &str) -> Vec<char> {
        s.chars().collect()
    }

    #[test]
    fn lazy_and_eager_clusters_agree() {
        // The two forms are separate code paths; an early stop must see the
        // same clusters.
        for s in [
            "",
            "abc",
            "a\tb",
            "中文字",
            "e\u{301}x",
            "\u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467}",
            "\u{1f1eb}\u{1f1f7}",
            "\u{1f1eb}\u{1f1f7}\u{1f1ee}\u{1f1f3}",
            "中\te\u{301}\u{1f1eb}",
        ] {
            let line = cs(s);
            let eager = clusters(&line, 8);
            let lazy: Vec<Cluster> = Clusters::new(&line, 8).collect();
            assert_eq!(eager, lazy, "{s:?}");
            for k in 0..=eager.len() {
                let take: Vec<Cluster> = Clusters::new(&line, 8).take(k).collect();
                assert_eq!(take, eager[..k], "{s:?} stopped after {k}");
            }
        }
    }

    #[test]
    fn a_bounded_simplicity_scan_is_conservative_and_cheap() {
        // The bound answers for a prefix; everything it accepts IS simple.
        let plain = cs(&"a".repeat(50_000));
        assert!(is_simple_prefix(&plain, 4_000));
        assert!(is_simple(&plain)); // and unbounded agrees
        let mut late_tab = cs(&"a".repeat(10_000));
        late_tab[5_000] = '\t';
        assert!(is_simple_prefix(&late_tab, 4_000), "the bound stops first");
        assert!(!is_simple(&late_tab), "the full scan sees it");
        assert!(!is_simple_prefix(&cs("café"), 100));
        assert!(!is_simple_prefix(&cs("a\tb"), 100));
        assert!(!is_simple_prefix(&cs("中日"), 100));
        assert!(is_simple_prefix(&[], 100));
        assert!(is_simple_prefix(&cs("\t"), 0));
    }

    #[test]
    fn ascii_is_one_column_and_simple() {
        assert_eq!(char_width('a'), 1);
        assert_eq!(width(&cs("hello"), 8), 5);
        assert!(is_simple(&cs("hello world")));
    }

    #[test]
    fn wide_characters_are_two_columns() {
        for (s, want) in [
            ("中文", 4),
            ("こんにちは", 10),
            ("한국어", 6),
            ("ＡＢ", 4),
            ("😀", 2),
            ("café", 4),
        ] {
            assert_eq!(width(&cs(s), 8), want, "{s:?}");
        }
        assert!(!is_simple(&cs("中文")));
    }

    #[test]
    fn combining_marks_add_no_column_and_join_their_base() {
        let line = cs("e\u{301}x");
        assert_eq!(width(&line, 8), 2);
        let cl = clusters(&line, 8);
        assert_eq!(cl.len(), 2, "the mark must not be its own cluster: {cl:?}");
        assert_eq!((cl[0].start, cl[0].end, cl[0].disp, cl[0].w), (0, 2, 0, 1));
        assert_eq!((cl[1].start, cl[1].w), (2, 1));
    }

    #[test]
    fn a_zwj_emoji_and_a_flag_are_clusters() {
        let family = cs("\u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467}");
        assert_eq!(width(&family, 8), 2, "a ZWJ sequence is one glyph");
        let flag = cs("\u{1f1eb}\u{1f1f7}");
        assert_eq!(width(&flag, 8), 2);
        // Indicators pair; they do not chain.
        let flags = cs("\u{1f1eb}\u{1f1f7}\u{1f1ee}\u{1f1f3}");
        assert_eq!(width(&flags, 8), 4);
        assert_eq!(clusters(&flags, 8).len(), 2);
    }

    #[test]
    fn a_control_character_is_not_a_column() {
        assert_eq!(char_width('\u{7}'), 0);
        assert_eq!(char_width('\u{1b}'), 0);
        assert_eq!(width(&cs("a\u{7}b"), 8), 2);
    }

    #[test]
    fn tabs_advance_to_the_next_stop_from_the_line_start() {
        assert_eq!(width(&cs("ab\t"), 8), 8);
        assert_eq!(width(&cs("\t"), 8), 8);
        assert_eq!(width(&cs("a\tb"), 8), 9);
        // A wide character moves the tab stop too.
        assert_eq!(width(&cs("中\t"), 8), 8);
        assert_eq!(width(&cs("中\u{4e2d}\t"), 8), 8);
    }

    #[test]
    fn segments_never_split_a_wide_character() {
        // 5 CJK chars = 10 columns, view 4 → 3 segments.
        let line = cs("中文字语言");
        let segs = segments(&line, 8, 4);
        let starts: Vec<usize> = segs.iter().map(|s| s.0).collect();
        assert_eq!(starts, vec![0, 2, 4]);
        assert_eq!(segs.iter().map(|s| s.1).collect::<Vec<_>>(), vec![0, 4, 8]);
        for (start, _) in &segs {
            assert!(
                clusters(&line, 8).iter().any(|c| c.start == *start),
                "{start} is not a cluster start"
            );
        }
        assert_eq!(width(&line, 8), 10);
    }

    #[test]
    fn segments_never_split_a_base_from_its_combining_marks() {
        let line = cs("e\u{301}e\u{301}e\u{301}");
        let segs = segments(&line, 8, 2);
        assert_eq!(
            segs.iter().map(|s| s.0).collect::<Vec<_>>(),
            vec![0, 4],
            "the cut landed inside a cluster"
        );
        assert_eq!(segments(&cs("中"), 8, 1), vec![(0, 0)]);
        assert_eq!(segments(&cs("ab"), 8, 1), vec![(0, 0), (1, 1)]);
    }

    #[test]
    fn segments_agree_with_width() {
        // Each segment fits the viewport unless it holds a cluster that
        // cannot fit on its own.
        for (s, vw) in [
            ("hello world this is a line", 7),
            ("中文中文中文中文", 5),
            ("a\tb\tc", 4),
            ("", 10),
        ] {
            let line = cs(s);
            let segs = segments(&line, 8, vw);
            let cls = clusters(&line, 8);
            for (i, (start, disp_start)) in segs.iter().enumerate() {
                assert_eq!(
                    *disp_start,
                    cls.iter()
                        .find(|c| c.start == *start)
                        .map(|c| c.disp)
                        .unwrap_or_else(|| width(&line, 8)),
                    "segment {i} of {s:?} reports the wrong display column"
                );
                let end = segs.get(i + 1).map(|s| s.0).unwrap_or(line.len());
                let held: Vec<&Cluster> = cls
                    .iter()
                    .filter(|c| c.start >= *start && c.start < end)
                    .collect();
                let w: usize = held.iter().map(|c| c.w).sum();
                let over_wide = held.iter().any(|c| c.w > vw.max(1));
                assert!(
                    w <= vw.max(1) || over_wide,
                    "{s:?}: segment {i} is {w} wide, view {vw}, no over-wide cluster"
                );
            }
        }
    }

    #[test]
    fn segments_are_the_minimum_number_of_rows() {
        assert_eq!(segments(&cs(&"x".repeat(25)), 8, 10).len(), 3);
        assert_eq!(segments(&cs("\tabcdef"), 8, 4), vec![(0, 0)]);
    }
}
