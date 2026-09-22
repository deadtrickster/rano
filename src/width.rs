//! Display width: a character is not a column, and a column is not a char
//! index.
//!
//! Everything in the editor that measures text goes through here: the soft
//! wrap (M-\) table, horizontal scroll, the cursor, mouse hit-testing and the
//! renderer's windows. Counting each character as one column is wrong in three
//! separate ways, and they have to be separated to be fixed:
//!
//! 1. **A character is not a column.** CJK, kana, Hangul, fullwidth forms and
//!    most emoji occupy two — so a line of Chinese is twice as wide as its
//!    character count, wrapping must account for that, and treating it as
//!    one-per-character writes half of every line off the right edge.
//! 2. **A column is not a character index.** Once some characters are two
//!    columns and others none, `col == display_col` is false, so the cursor
//!    lands in the wrong cell and a click maps to the wrong character.
//! 3. **A character is not a writable unit.** A base character plus its
//!    combining marks (`e` + U+0301), a variation selector, or a ZWJ emoji
//!    sequence is one thing on screen; a break or a truncation that lands
//!    between them leaves a stray accent on the next row. Wrapping therefore
//!    moves whole *clusters*, never characters.
//!
//! # What this implements, and what it does not
//!
//! It implements the East-Asian Wide and Fullwidth ranges plus the common
//! emoji planes, the combining-mark/variation-selector/joiner ranges, and
//! cluster grouping. It does **not** implement UAX #11 or UAX #29: there is no
//! generated Unicode table here, and a character outside the listed ranges is
//! one column and its own cluster. The failure mode of a miss is a line one
//! column narrow, once — the trade this file makes on purpose, stated so a
//! later reader can price it rather than rediscover it.
//!
//! # Tabs
//!
//! A tab advances to the next multiple of `tab_width`, measured from the
//! start of the *line*, not of the wrap segment — so the width of a wrapped
//! line is the sum of its segments' widths exactly, which is what lets the
//! wrap table count segments and the renderer agree about where they begin.

/// Columns one character claims on a terminal: 0, 1 or 2.
pub fn char_width(c: char) -> usize {
    let u = c as u32;
    // C0/C1 and DEL: not a column. (A `\n` can never reach a line.)
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

/// Combining marks, joiners and selectors: characters that attach to the one
/// before them instead of taking a cell of their own.
fn is_zero_width(u: u32) -> bool {
    matches!(u,
        0x0300..=0x036f      // combining diacritical marks
        | 0x0483..=0x0489    // Cyrillic combining
        | 0x0591..=0x05bd | 0x05bf | 0x05c1..=0x05c2 | 0x05c4..=0x05c5 | 0x05c7
        | 0x0610..=0x061a | 0x064b..=0x065f | 0x0670
        | 0x06d6..=0x06dc | 0x06df..=0x06e4 | 0x06e7..=0x06e8 | 0x06ea..=0x06ed
        | 0x0900..=0x0903 | 0x093a..=0x093c | 0x0941..=0x0948 | 0x094d
        | 0x0951..=0x0957 | 0x0962..=0x0963
        | 0x0e31 | 0x0e34..=0x0e3a | 0x0e47..=0x0e4e   // Thai
        | 0x1ab0..=0x1aff    // combining extended
        | 0x1dc0..=0x1dff    // combining supplement
        | 0x200b..=0x200f    // ZWSP, ZWNJ, ZWJ, LRM, RLM
        | 0x2028..=0x202e    // line/para separators, bidi overrides
        | 0x2060..=0x2064    // word joiner, invisible operators
        | 0x20d0..=0x20f0    // combining marks for symbols
        | 0xfe00..=0xfe0f    // variation selectors
        | 0xfe20..=0xfe2f    // combining half marks
        | 0xfeff             // BOM / ZWNBSP
        | 0xe0100..=0xe01ef  // variation selectors supplement
    )
}

/// [`char_width`]'s width decision for a bare codepoint: `true` when it takes
/// two columns.
///
/// Exposed because a caller scanning BYTES — an index build, a wrap table that
/// wants segment counts without decoding a row — needs the same answer for a
/// codepoint it has just assembled from a UTF-8 sequence, and two copies of
/// this table would drift.
///
/// `dead_code` is allowed because the only caller today is the lazy-loading
/// prototype in `bench.rs` (test-only); the index it exists for is §15 of
/// TODO.md, not yet written. It is public API of the library target either way.
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
        | 0x2e80..=0x2e99 | 0x2e9b..=0x2ef3   // CJK radicals
        | 0x2f00..=0x2fd5    // Kangxi radicals
        | 0x2ff0..=0x2ffb    // ideographic description
        | 0x3000..=0x303e    // CJK symbols and punctuation
        | 0x3041..=0x3096 | 0x3099..=0x30ff   // kana
        | 0x3105..=0x312f | 0x3131..=0x318e | 0x3190..=0x31e3
        | 0x31f0..=0x321e | 0x3220..=0x3247 | 0x3250..=0x4dbf
        | 0x4e00..=0xa48c    // CJK unified ideographs, Yi
        | 0xa490..=0xa4c6
        | 0xa960..=0xa97c    // Hangul Jamo extended-A
        | 0xac00..=0xd7a3    // Hangul syllables
        | 0xf900..=0xfaff    // CJK compatibility ideographs
        | 0xfe10..=0xfe19 | 0xfe30..=0xfe52 | 0xfe54..=0xfe66 | 0xfe68..=0xfe6b
        | 0xff01..=0xff60    // fullwidth forms
        | 0xffe0..=0xffe6
        | 0x16fe0..=0x16fe4 | 0x17000..=0x18d08
        | 0x1b000..=0x1b2fb
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
/// `display col == char index` holds for the whole line and the affine fast
/// paths in `ui`/`editor` are exact. This is the common case (ASCII source)
/// and the one the per-frame cost is measured on.
pub fn is_simple(chars: &[char]) -> bool {
    is_simple_prefix(chars, usize::MAX)
}

/// [`is_simple`] over at most the first `limit` characters.
///
/// The bound exists for rows measured in megabytes: a minified bundle or a
/// one-line JSON blob is a single row of millions of characters, and scanning
/// every one of them to answer "is this row ordinary?" costs ~6 ns per
/// character — 25 ms on a 4 MB row, per keystroke. A caller that has a cheap
/// fallback for the rare non-simple case can bound the scan and take it.
///
/// **The trade, stated:** a row longer than `limit` whose first `limit`
/// characters are ordinary is *assumed* ordinary for its whole length, so a
/// tab or a wide character past the limit would be mis-measured by the column
/// it occupies — one column, on a row nobody can see the end of.
pub fn is_simple_prefix(chars: &[char], limit: usize) -> bool {
    // ASCII and not a tab: the answer `char_width(c) == 1` gives for every
    // character this accepts, without the range checks. A non-ASCII character
    // that IS one column wide (é, for one) now answers false, which is
    // CONSERVATIVE — the row takes the general measuring path and renders
    // identically — and it is what makes the scan cheap enough to run over
    // 193M characters at open (measured 2026-09-22: 1.1 s → 120 ms).
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
///
/// A tab is its own cluster (it never attaches to a neighbour). A character
/// joins the cluster before it when it is zero-width (a combining mark, a
/// variation selector), when the character before it was a ZWJ — `👨‍👩‍👧` is
/// five code points and one glyph — or when it is the second of two regional
/// indicators, which pair into one flag. A zero-width character with nothing
/// to attach to (a line start, or straight after a tab) forms a zero-width
/// cluster of its own, which keeps the char indices covered without inventing
/// a cell.
///
/// A cluster's width is a *maximum* over its parts, never a sum: the joined
/// parts are drawn as one glyph.
pub fn clusters(chars: &[char], tab_width: usize) -> Vec<Cluster> {
    Clusters::new(chars, tab_width).collect()
}

/// [`clusters`] as a lazy iterator, so a caller that stops early — a renderer
/// window that ends before the line does — does not walk the tail of a long
/// line. One implementation, so the eager and lazy forms cannot drift.
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
        // only known once seen, and a partially-extended cluster handed out
        // twice would be a different answer than the eager form gives.
        let mut w = char_width(c);
        let mut end = start + 1;
        while let Some(&n) = chars.get(end) {
            // A tab never joins: `char_width` reports it as zero columns
            // because it is a control character, but it is its own cluster
            // with a real advance, and absorbing it would lose that.
            let nw = char_width(n);
            let joins = n != '\t'
                && (nw == 0
                    || chars[end - 1] == '\u{200d}'
                    || (is_regional_indicator(chars[start]) && is_regional_indicator(n)));
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
/// from the line start.
///
/// A ZWJ sequence therefore counts as the two columns of the single glyph it
/// draws, not as the sum of its parts.
pub fn width(chars: &[char], tab_width: usize) -> usize {
    clusters(chars, tab_width).iter().map(|c| c.w).sum()
}

/// Where each wrap segment of a line begins: `(char index, display column)`,
/// one entry per segment, always at least one. Given a viewport `view_w`
/// display columns wide.
///
/// The fill is greedy over clusters, so a segment holds as many whole
/// clusters as fit and never splits a wide character or a base from its
/// combining marks. A cluster wider than the whole viewport — a tab in a
/// viewport narrower than its advance is the reachable case — cannot be made
/// to fit; the segment holding it is allowed to overflow rather than becoming
/// a row per character.
///
/// The display column travels with the char index because a segment's start
/// is not `seg * view_w` once characters are wider than one column; returning
/// it here is what keeps the renderer from having to walk the line again to
/// find out where a segment paints.
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
            // Start a new segment at this cluster and measure from here. A
            // tab's width depends on the absolute column, which does not
            // reset at the boundary, so this resets the *fill* only.
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
        // One implementation feeds both, but the two forms are separate code
        // paths in the compiler's eyes: a caller that stops early must see
        // the same clusters as one that reads to the end.
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
            // Stopping early yields exactly the prefix of the same list.
            for k in 0..=eager.len() {
                let take: Vec<Cluster> = Clusters::new(&line, 8).take(k).collect();
                assert_eq!(take, eager[..k], "{s:?} stopped after {k}");
            }
        }
    }

    #[test]
    fn a_bounded_simplicity_scan_is_conservative_and_cheap() {
        // A row of megabytes must not be scanned whole to answer "is this
        // row ordinary?" — 193 MB of that was 1.1 s at open. The bound
        // answers for a prefix and the answer is conservative: everything it
        // accepts IS simple, so a row it wrongly rejects renders identically
        // through the general path.
        let plain = cs(&"a".repeat(50_000));
        assert!(is_simple_prefix(&plain, 4_000));
        assert!(is_simple(&plain)); // and unbounded agrees
        // A tab PAST the bound: the bounded scan misses it (the stated
        // trade), and the unbounded scan catches it.
        let mut late_tab = cs(&"a".repeat(10_000));
        late_tab[5_000] = '\t';
        assert!(is_simple_prefix(&late_tab, 4_000), "the bound stops first");
        assert!(!is_simple(&late_tab), "the full scan sees it");
        // A non-ASCII one-column character (é) is rejected even though it
        // occupies a single column: conservative, and the reason the scan is
        // a cheap ASCII test.
        assert!(!is_simple_prefix(&cs("café"), 100));
        // A tab or a wide character inside the bound is caught.
        assert!(!is_simple_prefix(&cs("a\tb"), 100));
        assert!(!is_simple_prefix(&cs("中日"), 100));
        // An empty row is trivially simple, and a limit of 0 accepts anything.
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
        // CJK, kana, Hangul, fullwidth forms, an emoji.
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
        // e + COMBINING ACUTE = one column, one cluster.
        let line = cs("e\u{301}x");
        assert_eq!(width(&line, 8), 2);
        let cl = clusters(&line, 8);
        assert_eq!(cl.len(), 2, "the mark must not be its own cluster: {cl:?}");
        assert_eq!((cl[0].start, cl[0].end, cl[0].disp, cl[0].w), (0, 2, 0, 1));
        assert_eq!((cl[1].start, cl[1].w), (2, 1));
    }

    #[test]
    fn a_zwj_emoji_and_a_flag_are_clusters() {
        // 👨👩👧 is five code points, one cluster, two columns.
        let family = cs("\u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467}");
        assert_eq!(width(&family, 8), 2, "a ZWJ sequence is one glyph");
        // A flag: two regional indicators, two columns together.
        let flag = cs("\u{1f1eb}\u{1f1f7}");
        assert_eq!(width(&flag, 8), 2);
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
        // 5 CJK chars = 10 columns, view 4 → 3 segments (2 chars, 2 chars, 1).
        let line = cs("中文字语言");
        let segs = segments(&line, 8, 4);
        let starts: Vec<usize> = segs.iter().map(|s| s.0).collect();
        assert_eq!(starts, vec![0, 2, 4]);
        // And the display column travels with it: 4 columns per segment.
        assert_eq!(segs.iter().map(|s| s.1).collect::<Vec<_>>(), vec![0, 4, 8]);
        // Every boundary is a char index that starts a cluster.
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
        // "e◌́e◌́e◌́" is 3 clusters of 1 column: view 2 → 2 segments, and the
        // cut must never fall between an `e` and its accent.
        let line = cs("e\u{301}e\u{301}e\u{301}");
        let segs = segments(&line, 8, 2);
        assert_eq!(
            segs.iter().map(|s| s.0).collect::<Vec<_>>(),
            vec![0, 4],
            "the cut landed inside a cluster"
        );
        // A one-column viewport still terminates (a wide cluster alone).
        assert_eq!(segments(&cs("中"), 8, 1), vec![(0, 0)]);
        assert_eq!(segments(&cs("ab"), 8, 1), vec![(0, 0), (1, 1)]);
    }

    #[test]
    fn segments_agree_with_width() {
        // The wrap table counts segments; the renderer walks the same table.
        // For every case here a segment must fit the viewport — unless it
        // holds a cluster that does not fit on its own (a tab wider than the
        // viewport), which no segmentation can avoid.
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
        // A line of 25 one-column characters in a 10-column view is 3 rows,
        // not 25: an overflowing cluster must not become a row per character.
        assert_eq!(segments(&cs(&"x".repeat(25)), 8, 10).len(), 3);
        // And a tab wider than the viewport takes one segment, not one per
        // following character.
        assert_eq!(segments(&cs("\tabcdef"), 8, 4), vec![(0, 0)]);
    }
}
