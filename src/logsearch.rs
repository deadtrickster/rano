//! Search a log by reading the FILE, never the buffer: a log is opened at its tail, so
//! the wanted line is usually older than the held rows. Windows of `block` bytes, memory
//! O(block), interruptible via `cancel`; a hit is an offset plus a row counted in `\n`.

use std::fs::File;
use std::io;
use std::os::unix::fs::FileExt;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

/// Direction of a scan. `Backward` reports nearest `from` first, i.e.
/// descending offsets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    Forward,
    Backward,
}

/// One match: byte offset from the file's start, 0-based row (counted in
/// `\n` bytes — the row a jump lands on), and length in bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hit {
    pub offset: u64,
    pub row: u64,
    pub len: usize,
}

/// What to look for: a bytewise literal with optional ASCII case folding (overlapping
/// occurrences reported), or a `regex::bytes::Regex`, which matches raw bytes including
/// non-UTF-8.
pub enum Pattern<'a> {
    Literal {
        needle: &'a [u8],
        case_sensitive: bool,
    },
    Regex(regex::bytes::Regex),
}

impl Pattern<'_> {
    /// `(?i)`-prefixed when case-insensitive, mirroring
    /// `search::Matcher::regex`. The returned pattern owns its regex.
    pub fn regex(pattern: &str, case_sensitive: bool) -> Result<Pattern<'static>, regex::Error> {
        let re = if case_sensitive {
            regex::bytes::Regex::new(pattern)?
        } else {
            regex::bytes::Regex::new(&format!("(?i){}", pattern))?
        };
        Ok(Pattern::Regex(re))
    }
}

/// What a scan found. `interrupted` is true only when `cancel` was set
/// mid-scan: `hits` is then the partial list found before that.
#[derive(Debug)]
pub struct Outcome {
    pub hits: Vec<Hit>,
    pub interrupted: bool,
}

/// Search `path`'s bytes in `block`-sized windows from `from` (clamped to the end), forward
/// over `[from, size)` or backward over `[0, from)` — the two partition the file, so a hit
/// is never repeated. `max_hits` caps the scan, `cancel` is checked between blocks.
pub fn scan(
    path: &Path,
    pattern: &Pattern,
    dir: Dir,
    from: u64,
    block: usize,
    max_hits: usize,
    cancel: &AtomicBool,
) -> io::Result<Outcome> {
    let file = File::open(path)?;
    let size = file.metadata()?.len();
    let block = block.max(1);
    // How much of the previous block each window re-reads. A literal cannot
    // begin further back than needle-1 bytes; a regex has no bound
    // (unbounded quantifiers), so its carry is a whole block.
    let overlap = match pattern {
        Pattern::Literal { needle, .. } => needle.len().saturating_sub(1),
        Pattern::Regex(_) => block,
    };
    if max_hits == 0 || matches!(pattern, Pattern::Literal { needle, .. } if needle.is_empty()) {
        return Ok(Outcome {
            hits: Vec::new(),
            interrupted: false,
        });
    }
    let from = from.min(size);
    match dir {
        Dir::Forward => scan_forward(file, size, pattern, from, block, overlap, max_hits, cancel),
        Dir::Backward => scan_backward(file, size, pattern, from, block, overlap, max_hits, cancel),
    }
}

/// Forward scan over `[from, size)`. Block `k` is `[from + k·block, ...)`, so tests can
/// place a needle against a known boundary; the first window carries nothing, because a
/// match starting before `from` is out of range.
#[allow(clippy::too_many_arguments)]
fn scan_forward(
    file: File,
    size: u64,
    pattern: &Pattern,
    from: u64,
    block: usize,
    overlap: usize,
    max_hits: usize,
    cancel: &AtomicBool,
) -> io::Result<Outcome> {
    let mut hits = Vec::new();
    let mut buf = vec![0u8; overlap + block];
    // Rows before `from`: free when scanning from the top; otherwise one
    // counting pass over [0, from).
    let (mut nl, mut interrupted) = if from > 0 {
        count_newlines(&file, &mut buf[..block], from, cancel)?
    } else {
        (0, false)
    };
    let regex = matches!(pattern, Pattern::Regex(_));
    let mut last_reported: Option<u64> = None;
    let mut pos = from;
    while !interrupted && pos < size {
        if cancel.load(Ordering::Relaxed) {
            interrupted = true;
            break;
        }
        // The window is [pos - carry, pos + n). Actual read lengths are used
        // throughout, so a file that changes size mid-scan degrades to a
        // shorter scan, not an error and not a loop.
        let want_carry = ((pos - from).min(overlap as u64)) as usize;
        let carry = read_at_most(&file, &mut buf[..want_carry], pos - want_carry as u64)?;
        let want = ((size - pos).min(block as u64)) as usize;
        let n = read_at_most(&file, &mut buf[carry..carry + want], pos)?;
        if n == 0 {
            break; // EOF before the snapshot size: the file shrank under us
        }
        let win = &buf[..carry + n];
        let win_base = pos - carry as u64;
        let win_end = pos + n as u64;
        for_each_match(pattern, win, |rel, len| {
            if hits.len() >= max_hits {
                return;
            }
            let abs = win_base + rel as u64;
            if abs < from {
                return; // out of range; the first window carries nothing, so belt-and-braces
            }
            // Deduplicate against earlier windows: never re-report at or
            // below the highest offset already reported (a regex need; a
            // literal's overlap already guarantees it).
            if let Some(last) = last_reported
                && abs <= last
            {
                return;
            }
            // A regex match ending flush at the window's end may extend into
            // unread bytes; the next window's carry reports it whole.
            if regex && rel + len == win.len() && win_end < size {
                return;
            }
            // Row = newlines in [0, abs), adjusted for where the hit sits
            // relative to `pos` (the boundary between carry and new block).
            let row = if rel < carry {
                nl - count_newlines_in(&win[rel..carry])
            } else {
                nl + count_newlines_in(&win[carry..rel])
            } as u64;
            hits.push(Hit {
                offset: abs,
                row,
                len,
            });
            last_reported = Some(abs);
        });
        if hits.len() >= max_hits {
            break;
        }
        nl += count_newlines_in(&win[carry..]);
        pos += n as u64;
    }
    Ok(Outcome { hits, interrupted })
}

/// Backward scan over `[0, from)`: blocks tile DOWN from `from`, each window plus
/// `overlap` bytes read ABOVE the block — the peek above is what finds a match straddling
/// the boundary. Matches at `from` or later are not reported.
#[allow(clippy::too_many_arguments)]
fn scan_backward(
    file: File,
    size: u64,
    pattern: &Pattern,
    from: u64,
    block: usize,
    overlap: usize,
    max_hits: usize,
    cancel: &AtomicBool,
) -> io::Result<Outcome> {
    let mut hits = Vec::new();
    let mut buf = vec![0u8; block + overlap];
    // Every backward hit is below `from`, so every row needs the newline
    // count of [0, from) — one forward counting pass.
    let (nl_from, mut interrupted) = count_newlines(&file, &mut buf[..block], from, cancel)?;
    let regex = matches!(pattern, Pattern::Regex(_));
    let mut pending: Vec<(u64, usize)> = Vec::new();
    let mut nl_above = 0usize; // newlines in [top, from)
    let mut lowest: Option<u64> = None; // lowest offset reported so far
    let mut top = from;
    while !interrupted && top > 0 {
        if cancel.load(Ordering::Relaxed) {
            interrupted = true;
            break;
        }
        let want = (top.min(block as u64)) as usize;
        let n = read_at_most(&file, &mut buf[..want], top - want as u64)?;
        if n == 0 {
            break; // the file shrank under us
        }
        let want_carry = ((size - top).min(overlap as u64)) as usize;
        let carry = read_at_most(&file, &mut buf[n..n + want_carry], top)?;
        let win = &buf[..n + carry];
        let win_base = top - n as u64;
        pending.clear();
        for_each_match(pattern, win, |rel, len| {
            let abs = win_base + rel as u64;
            if abs >= from {
                return; // the peek at/past `from` is context, not range
            }
            // Deduplicate against earlier (higher) windows.
            if let Some(low) = lowest
                && abs >= low
            {
                return;
            }
            // A regex match starting flush at the window's start may extend
            // into bytes below; the next (lower) window reports it whole.
            if regex && rel == 0 && win_base > 0 {
                return;
            }
            pending.push((abs, len));
        });
        let mut stop = false;
        // Descending: the hit nearest `from` comes out first.
        for &(abs, len) in pending.iter().rev() {
            let rel = (abs - win_base) as usize;
            // Row = newlines in [0, abs) = nl_from minus those in [abs, from):
            // the window's share plus `nl_above` for the bytes above it.
            let newlines_above_abs = if rel > n {
                nl_above - count_newlines_in(&win[n..rel])
            } else {
                nl_above + count_newlines_in(&win[rel..n])
            };
            let row = (nl_from - newlines_above_abs) as u64;
            hits.push(Hit {
                offset: abs,
                row,
                len,
            });
            lowest = Some(abs);
            if hits.len() >= max_hits {
                stop = true;
                break;
            }
        }
        if stop {
            break;
        }
        nl_above += count_newlines_in(&win[..n]);
        top -= n as u64;
    }
    Ok(Outcome { hits, interrupted })
}

/// Count `\n` in `[0, upto)` in `scratch`-sized positional reads, checking
/// `cancel` between reads.
fn count_newlines(
    file: &File,
    scratch: &mut [u8],
    upto: u64,
    cancel: &AtomicBool,
) -> io::Result<(usize, bool)> {
    let mut total = 0usize;
    let mut pos = 0u64;
    while pos < upto {
        if cancel.load(Ordering::Relaxed) {
            return Ok((total, true));
        }
        let want = ((upto - pos).min(scratch.len() as u64)) as usize;
        let n = read_at_most(file, &mut scratch[..want], pos)?;
        if n == 0 {
            break;
        }
        total += count_newlines_in(&scratch[..n]);
        pos += n as u64;
    }
    Ok((total, false))
}

/// One positional read: bytes at `at` into `buf`, how many came back (0 at EOF or if the
/// file shrank mid-scan). Positional, so a window's block and carry reads keep no seek state.
fn read_at_most(file: &File, buf: &mut [u8], at: u64) -> io::Result<usize> {
    if buf.is_empty() {
        return Ok(0);
    }
    file.read_at(buf, at)
}

fn count_newlines_in(s: &[u8]) -> usize {
    s.iter().filter(|&&b| b == b'\n').count()
}

/// Every match in `win`, as (index into `win`, length), ascending. Literal matches
/// overlap; regex matches follow `find_iter` within the window.
fn for_each_match(pattern: &Pattern, win: &[u8], mut f: impl FnMut(usize, usize)) {
    match pattern {
        Pattern::Literal {
            needle,
            case_sensitive,
        } => {
            let n = needle.len();
            let mut i = 0;
            while i + n <= win.len() {
                let cand = &win[i..i + n];
                let hit = if *case_sensitive {
                    cand == *needle
                } else {
                    cand.eq_ignore_ascii_case(needle)
                };
                if hit {
                    f(i, n);
                }
                i += 1;
            }
        }
        Pattern::Regex(re) => {
            for m in re.find_iter(win) {
                f(m.start(), m.len());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    /// A temp file, removed on drop, so a panicking test never reaches a tidy-up line. The
    /// name carries the pid and the test's tag, so runs and tests cannot collide.
    struct Temp(PathBuf);
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.0);
        }
    }
    /// So a test can keep saying `&p` where a path is wanted.
    impl std::ops::Deref for Temp {
        type Target = std::path::Path;
        fn deref(&self) -> &std::path::Path {
            &self.0
        }
    }
    impl Temp {
        /// The path, owned — for a worker thread; the file is still removed on drop.
        fn to_path_buf(&self) -> PathBuf {
            self.0.clone()
        }
    }

    fn tmp(name: &str, bytes: &[u8]) -> Temp {
        let p =
            std::env::temp_dir().join(format!("rano-logsearch-{}-{}", std::process::id(), name));
        fs::write(&p, bytes).expect("write temp file");
        Temp(p)
    }

    fn go() -> AtomicBool {
        AtomicBool::new(false)
    }
    fn lit(needle: &[u8]) -> Pattern<'_> {
        Pattern::Literal {
            needle,
            case_sensitive: true,
        }
    }
    fn ici(needle: &[u8]) -> Pattern<'_> {
        Pattern::Literal {
            needle,
            case_sensitive: false,
        }
    }
    fn scan_lit(
        path: &std::path::Path,
        needle: &[u8],
        dir: Dir,
        from: u64,
        block: usize,
    ) -> Outcome {
        scan(path, &lit(needle), dir, from, block, usize::MAX, &go()).expect("scan")
    }
    fn hits(
        path: &std::path::Path,
        needle: &[u8],
        dir: Dir,
        from: u64,
        block: usize,
    ) -> Vec<(u64, u64, usize)> {
        scan_lit(path, needle, dir, from, block)
            .hits
            .into_iter()
            .map(|h| (h.offset, h.row, h.len))
            .collect()
    }

    #[test]
    fn literal_forward_offsets_rows_len_exact() {
        // Two matches on row 0, one on row 1, two on row 2, block 8 < file:
        // a at 0, 4; beta's a at 9; gamma's a at 12 and 15.
        let p = tmp("fwd", b"alpha\nbeta\ngamma\n");
        assert_eq!(
            hits(&p, b"a", Dir::Forward, 0, 8),
            vec![(0, 0, 1), (4, 0, 1), (9, 1, 1), (12, 2, 1), (15, 2, 1)]
        );
    }

    #[test]
    fn literal_backward_descending_same_rows() {
        let p = tmp("bwd", b"alpha\nbeta\ngamma\n");
        let fwd = hits(&p, b"a", Dir::Forward, 0, 8);
        let bwd = hits(&p, b"a", Dir::Backward, 17, 8);
        // Nearest `from` first...
        assert_eq!(bwd.first().copied(), Some((15, 2, 1)));
        // ...and the same hits as forward, reversed.
        let mut asc = bwd;
        asc.reverse();
        assert_eq!(asc, fwd);
    }

    #[test]
    fn forward_and_backward_partition_the_file() {
        // 5 rows of "ha\n": hits at 0, 3, 6, 9, 12.
        let p = tmp("partition", b"ha\nha\nha\nha\nha\n");
        let f_all = hits(&p, b"ha", Dir::Forward, 0, 4);
        assert_eq!(
            f_all,
            vec![(0, 0, 2), (3, 1, 2), (6, 2, 2), (9, 3, 2), (12, 4, 2)]
        );
        // Backward over the whole file: the same list, descending.
        let mut expect = f_all.clone();
        expect.reverse();
        assert_eq!(hits(&p, b"ha", Dir::Backward, 15, 4), expect);
        // Split at 6: forward sees [6, size), backward [0, 6) — every hit once.
        let f6 = hits(&p, b"ha", Dir::Forward, 6, 4);
        let b6 = hits(&p, b"ha", Dir::Backward, 6, 4);
        assert_eq!(f6, vec![(6, 2, 2), (9, 3, 2), (12, 4, 2)]);
        assert_eq!(b6, vec![(3, 1, 2), (0, 0, 2)]);
        let mut seen: Vec<u64> = b6.iter().map(|h| h.0).collect();
        seen.extend(f6.iter().map(|h| h.0));
        seen.sort_unstable();
        assert_eq!(seen, vec![0, 3, 6, 9, 12]);
    }

    #[test]
    fn case_insensitive_literal() {
        let p = tmp("case", b"Foo FOO foo");
        let insensitive = scan(&p, &ici(b"fOO"), Dir::Forward, 0, 8, usize::MAX, &go())
            .unwrap()
            .hits
            .into_iter()
            .map(|h| (h.offset, h.row, h.len))
            .collect::<Vec<_>>();
        assert_eq!(insensitive, vec![(0, 0, 3), (4, 0, 3), (8, 0, 3)]);
        // Case-sensitive: only the exact spelling.
        assert_eq!(
            hits(&p, b"fOO", Dir::Forward, 0, 8),
            Vec::<(u64, u64, usize)>::new()
        );
        assert_eq!(hits(&p, b"FOO", Dir::Forward, 0, 8), vec![(4, 0, 3)]);
    }

    #[test]
    fn ascii_folding_does_not_touch_non_ascii() {
        // Documented: folding is ASCII-only, so 'É' (C3 89) does not fold to 'é' (C3 A9).
        let p = tmp("nonascii", "É\né\n".as_bytes());
        let insensitive = scan(
            &p,
            &ici("é".as_bytes()),
            Dir::Forward,
            0,
            8,
            usize::MAX,
            &go(),
        )
        .unwrap()
        .hits
        .into_iter()
        .map(|h| h.offset)
        .collect::<Vec<_>>();
        assert_eq!(insensitive, vec![3]);
        // A multibyte needle still matches its exact bytes.
        assert_eq!(
            hits(&p, "é".as_bytes(), Dir::Forward, 0, 8),
            vec![(3, 1, 2)]
        );
    }

    #[test]
    fn byte_oriented_needle_matches_raw_bytes() {
        // Matching is over bytes: 0xA9 is the second byte of 'é' and matches.
        let p = tmp("halfchar", "éx".as_bytes());
        assert_eq!(hits(&p, b"\xa9", Dir::Forward, 0, 8), vec![(1, 0, 1)]);
    }

    #[test]
    fn regex_over_bytes() {
        // "333" ends flush at the first window's end; the defer path reports
        // it whole from the second.
        let p = tmp("regex", b"a1b22c333");
        let pat = Pattern::regex(r"\d+", true).unwrap();
        let out = scan(&p, &pat, Dir::Forward, 0, 8, usize::MAX, &go()).unwrap();
        let got: Vec<(u64, u64, usize)> = out
            .hits
            .into_iter()
            .map(|h| (h.offset, h.row, h.len))
            .collect();
        assert_eq!(got, vec![(1, 0, 1), (3, 0, 2), (6, 0, 3)]);
    }

    #[test]
    fn regex_matches_non_utf8_bytes() {
        // regex::bytes is legal on non-UTF-8 input; (?-u:.) is any byte.
        let p = tmp("regex-bytes", b"a\xffb");
        let pat = Pattern::regex(r"(?-u:.)", true).unwrap();
        let out = scan(&p, &pat, Dir::Forward, 0, 8, usize::MAX, &go()).unwrap();
        let got: Vec<u64> = out.hits.into_iter().map(|h| h.offset).collect();
        assert_eq!(got, vec![0, 1, 2]);
    }

    #[test]
    fn regex_case_insensitive() {
        let p = tmp("regex-case", b"ERROR error\n");
        let pat = Pattern::regex("error", false).unwrap();
        let out = scan(&p, &pat, Dir::Forward, 0, 8, usize::MAX, &go()).unwrap();
        let got: Vec<(u64, u64, usize)> = out
            .hits
            .into_iter()
            .map(|h| (h.offset, h.row, h.len))
            .collect();
        assert_eq!(got, vec![(0, 0, 5), (6, 0, 5)]);
    }

    /// Block-boundary straddle: block 8, so forward windows are [0,8), [7,16), [15,24) with
    /// a 1-byte carry. `at` is every position that matters: inside a block, beside it, on it.
    fn straddle_file(tag: &str, at: usize) -> Temp {
        let mut data = vec![b'.'; 24];
        data[at..at + 2].copy_from_slice(b"XY");
        // The tag keeps the forward and backward straddle tests off each
        // other's files.
        tmp(&format!("{}{}", tag, at), &data)
    }

    #[test]
    fn literal_straddles_forward_block_boundary() {
        // The boundary at 8 is between windows [0,8) and [7,16); a needle
        // at 7 is half in each and only the carry finds it.
        for at in [6, 7, 8, 9] {
            let p = straddle_file("strad-f", at);
            assert_eq!(
                hits(&p, b"XY", Dir::Forward, 0, 8),
                vec![(at as u64, 0, 2)],
                "needle at {} (boundary 8)",
                at
            );
        }
    }

    #[test]
    fn literal_straddles_backward_block_boundary() {
        // Backward from 16, block 8: regions [8,16) then [0,8), boundary 8;
        // window 1 is [0,8) + carry [8,9) — a needle at 7 straddles.
        for at in [6, 7, 8, 9] {
            let p = straddle_file("strad-b", at);
            assert_eq!(
                hits(&p, b"XY", Dir::Backward, 16, 8),
                vec![(at as u64, 0, 2)],
                "needle at {} (boundary 8)",
                at
            );
        }
    }

    #[test]
    fn needle_at_file_edges_and_last_window() {
        // Last bytes of the file (the final block is short but its window
        // still carries), and first bytes.
        let mut data = vec![b'.'; 19];
        data[17..19].copy_from_slice(b"XY");
        let p = tmp("edges", &data);
        assert_eq!(hits(&p, b"XY", Dir::Forward, 0, 8), vec![(17, 0, 2)]);
        assert_eq!(hits(&p, b"XY", Dir::Backward, 19, 8), vec![(17, 0, 2)]);

        let mut data = vec![b'.'; 10];
        data[0..2].copy_from_slice(b"XY");
        let p = tmp("edges2", &data);
        assert_eq!(hits(&p, b"XY", Dir::Forward, 0, 8), vec![(0, 0, 2)]);
        assert_eq!(hits(&p, b"XY", Dir::Backward, 10, 8), vec![(0, 0, 2)]);
    }

    #[test]
    fn multibyte_needle_straddling_boundary() {
        // 'é' is C3 A9; at 7 the two bytes straddle the boundary at 8. The
        // carry is 1 BYTE — the overlap is counted in bytes, not chars.
        let mut data = vec![b'.'; 24];
        data[7..9].copy_from_slice("é".as_bytes());
        let p = tmp("mb-straddle", &data);
        assert_eq!(
            hits(&p, "é".as_bytes(), Dir::Forward, 0, 8),
            vec![(7, 0, 2)]
        );
        assert_eq!(
            hits(&p, "é".as_bytes(), Dir::Backward, 16, 8),
            vec![(7, 0, 2)]
        );
    }

    #[test]
    fn regex_straddling_block_boundary() {
        // An 8-byte regex spanning 5..13 across the boundary at 8; the regex
        // carry is a whole block, so window 1 sees it whole.
        let mut data = vec![b'.'; 24];
        data[5..13].copy_from_slice(b"ERROR-42");
        let p = tmp("regex-straddle", &data);
        let pat = Pattern::regex(r"ERROR-4\d", true).unwrap();
        for dir in [Dir::Forward, Dir::Backward] {
            let from = if dir == Dir::Forward { 0 } else { 24 };
            let out = scan(&p, &pat, dir, from, 8, usize::MAX, &go()).unwrap();
            let got: Vec<(u64, u64, usize)> = out
                .hits
                .into_iter()
                .map(|h| (h.offset, h.row, h.len))
                .collect();
            assert_eq!(got, vec![(5, 0, 8)], "{:?}", dir);
        }
    }

    #[test]
    fn regex_long_match_is_where_it_enters_the_last_window() {
        // DOCUMENTED BEHAVIOUR: `a+` over a 20-byte run is longer than any window. One hit,
        // at the offset where it enters the last window, not its true start (1).
        let mut data = vec![b'b'];
        data.extend(std::iter::repeat_n(b'a', 20));
        data.push(b'b');
        let p = tmp("regex-long", &data);
        let pat = Pattern::regex("a+", true).unwrap();
        let out = scan(&p, &pat, Dir::Forward, 0, 4, usize::MAX, &go()).unwrap();
        assert_eq!(out.hits.len(), 1, "one hit, not one per window");
        // The last window is [16,22) = "aaaaab": a+ entered at 16, 5 bytes visible.
        assert_eq!(out.hits[0].offset, 16);
        assert_eq!(out.hits[0].len, 5);
    }

    #[test]
    fn interrupt_before_any_block() {
        // Flag pre-set: checked before the first block, so not even the first hit comes back.
        let p = tmp("cancel-pre", b"hit\nhit\nhit\n");
        let stop = AtomicBool::new(true);
        let out = scan(&p, &lit(b"hit"), Dir::Forward, 0, 2, usize::MAX, &stop).unwrap();
        assert!(out.interrupted, "flag was set");
        assert!(out.hits.is_empty(), "nothing should be found");

        // Backward: the row pre-pass is what the flag stops.
        let out = scan(&p, &lit(b"hit"), Dir::Backward, 12, 2, usize::MAX, &stop).unwrap();
        assert!(out.interrupted);
        assert!(out.hits.is_empty());
    }

    /// One scan of `path`, cancelled `wait` after it starts. The block is a single byte, so the
    /// scan's cost is syscalls rather than CPU speed: it cannot finish inside any wait used here,
    /// on any machine, which is what makes "the flag lands mid-scan" a property of the fixture
    /// rather than a hope about the box (the 30 ms this test used to rely on was the box's answer,
    /// and under a gate it was wrong).
    fn cancelled_after(file: &Temp, wait: Duration) -> Outcome {
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = Arc::clone(&cancel);
        let path = file.to_path_buf();
        let worker = std::thread::spawn(move || {
            scan(
                &path,
                &lit(b"hit"),
                Dir::Forward,
                0,
                1,
                usize::MAX,
                &worker_cancel,
            )
            .unwrap()
        });
        std::thread::sleep(wait);
        cancel.store(true, Ordering::Relaxed);
        worker.join().unwrap()
    }

    #[test]
    fn interrupt_mid_scan_returns_partial_prefix() {
        let data: Vec<u8> = b"hit\n"
            .iter()
            .copied()
            .cycle()
            .take(4 * 1024 * 1024)
            .collect();
        let p = tmp("cancel-mid", &data);
        // The wait escalates until one attempt lands MID-scan. A starved worker can be scheduled so
        // late that the flag arrives before it has read a block, and that is the machine answering,
        // not the scan; every attempt that lands must still hold the invariant — interrupted, a true
        // prefix in order, and not the whole file — so the escalation buys a fair sample and weakens
        // nothing. The flag winning the race outright is what `interrupt_before_any_block` covers.
        let mut landed = false;
        for wait_ms in [30u64, 100, 300] {
            let out = cancelled_after(&p, Duration::from_millis(wait_ms));
            assert!(
                out.interrupted,
                "a 4 MB scan read four bytes at a time cannot finish in {wait_ms} ms"
            );
            // A true prefix: hit k is at offset 4k on row k.
            for (k, h) in out.hits.iter().enumerate() {
                assert_eq!(h.offset, (4 * k) as u64, "hit {k} after {wait_ms} ms");
                assert_eq!(h.row, k as u64, "hit {k} after {wait_ms} ms");
            }
            if out.hits.is_empty() {
                continue;
            }
            assert!(
                out.hits.len() < 1024 * 1024,
                "{} hits after {wait_ms} ms is the whole file, not a partial scan",
                out.hits.len()
            );
            landed = true;
            break;
        }
        assert!(
            landed,
            "no attempt interrupted the scan after it had found anything, so the worker never ran: \
             the machine, not the scan"
        );
    }

    #[test]
    fn needle_never_occurs() {
        let p = tmp("absent", b"nothing to see here\nmoving along\n");
        for dir in [Dir::Forward, Dir::Backward] {
            let out = scan_lit(&p, b"XYZ", dir, 22, 8);
            assert!(!out.interrupted);
            assert!(out.hits.is_empty());
        }
    }

    #[test]
    fn max_hits_caps_and_stops() {
        // Ten hits; forward takes the first three, backward the three
        // NEAREST `from`. Reaching the cap is not an interruption.
        let mut data = Vec::new();
        for i in 0..10 {
            data.extend_from_slice(format!("hit{}\n", i).as_bytes());
        }
        let p = tmp("cap", &data);
        let f = scan(&p, &lit(b"hit"), Dir::Forward, 0, 8, 3, &go()).unwrap();
        assert!(!f.interrupted);
        let got: Vec<u64> = f.hits.iter().map(|h| h.offset).collect();
        assert_eq!(got, vec![0, 5, 10]);

        let b = scan(
            &p,
            &lit(b"hit"),
            Dir::Backward,
            data.len() as u64,
            8,
            3,
            &go(),
        )
        .unwrap();
        assert!(!b.interrupted);
        let got: Vec<u64> = b.hits.iter().map(|h| h.offset).collect();
        assert_eq!(got, vec![45, 40, 35]);

        // Zero: reads nothing, finds nothing.
        let z = scan(&p, &lit(b"hit"), Dir::Forward, 0, 8, 0, &go()).unwrap();
        assert!(!z.interrupted && z.hits.is_empty());
    }

    #[test]
    fn empty_file() {
        let p = tmp("empty", b"");
        for dir in [Dir::Forward, Dir::Backward] {
            let out = scan(&p, &lit(b"x"), dir, 0, 8, usize::MAX, &go()).unwrap();
            assert!(!out.interrupted && out.hits.is_empty());
            // and with `from` past the end
            let out = scan(&p, &lit(b"x"), dir, 999, 8, usize::MAX, &go()).unwrap();
            assert!(!out.interrupted && out.hits.is_empty());
        }
    }

    #[test]
    fn no_trailing_newline() {
        // The last row has no '\n'; a hit on it still gets the right row.
        let p = tmp("no-nl", b"abc\nxyz");
        assert_eq!(hits(&p, b"xyz", Dir::Forward, 0, 8), vec![(4, 1, 3)]);
        assert_eq!(hits(&p, b"xyz", Dir::Backward, 7, 8), vec![(4, 1, 3)]);
    }

    #[test]
    fn crlf_rows() {
        // Rows are counted in '\n' bytes; CRLF files get one row per \r\n.
        // "one\r\ntwo\r\nthree\r\n": two at 5, three at 10.
        let p = tmp("crlf", b"one\r\ntwo\r\nthree\r\n");
        assert_eq!(hits(&p, b"two", Dir::Forward, 0, 8), vec![(5, 1, 3)]);
        assert_eq!(hits(&p, b"three", Dir::Backward, 18, 8), vec![(10, 2, 5)]);
        // A needle spanning the \r\n itself — before "two" and before "three".
        assert_eq!(
            hits(&p, b"\r\nt", Dir::Forward, 0, 8),
            vec![(3, 0, 3), (8, 1, 3)]
        );
    }

    #[test]
    fn rows_after_multibyte_and_many_matches() {
        // 'é' and 'ö' are 2 bytes: rows must count '\n', not divide by
        // anything. "alpha" starts at 14 and 25, both on row 2.
        let p = tmp("rows-mb", "héllo\nwörld\nalpha beta alpha\n".as_bytes());
        assert_eq!(
            hits(&p, b"alpha", Dir::Forward, 0, 8),
            vec![(14, 2, 5), (25, 2, 5)]
        );
        // Backward: same two hits, nearest `from` first.
        assert_eq!(
            hits(&p, b"alpha", Dir::Backward, 31, 8),
            vec![(25, 2, 5), (14, 2, 5)]
        );
        // Forward from a nonzero offset: pre-pass for [0,7), then counting
        // continues in the matching pass.
        assert_eq!(
            hits(&p, b"alpha", Dir::Forward, 7, 8),
            vec![(14, 2, 5), (25, 2, 5)]
        );
    }

    #[test]
    fn empty_needle_matches_nothing() {
        let p = tmp("empty-needle", b"abc");
        for cs in [true, false] {
            let pat = Pattern::Literal {
                needle: b"",
                case_sensitive: cs,
            };
            let out = scan(&p, &pat, Dir::Forward, 0, 8, usize::MAX, &go()).unwrap();
            assert!(out.hits.is_empty() && !out.interrupted);
        }
    }

    #[test]
    fn from_is_clamped() {
        let p = tmp("clamp", b"abc\n");
        assert_eq!(
            hits(&p, b"abc", Dir::Forward, 999, 8),
            Vec::<(u64, u64, usize)>::new()
        );
        assert_eq!(hits(&p, b"abc", Dir::Backward, 999, 8), vec![(0, 0, 3)]);
    }

    #[test]
    fn backward_from_zero_is_empty() {
        let p = tmp("bwd-zero", b"abc\n");
        let out = scan_lit(&p, b"abc", Dir::Backward, 0, 8);
        assert!(!out.interrupted && out.hits.is_empty());
    }

    #[test]
    fn multi_mb_scan_measured() {
        // ~4 MB of log-shaped lines, an ERROR every 1000th. Timed and printed for the
        // record, asserting correctness only: the buffer and the hits Vec are all that is
        // allocated.
        let stamp = "2026-10-09T12:00:00Z ";
        let error_at = format!("{}ERROR", stamp).find("ERROR").unwrap() as u64;
        let mut data = Vec::with_capacity(4 * 1024 * 1024);
        let mut expect = Vec::new();
        let mut off = 0u64;
        let mut row = 0u64;
        while data.len() < 4 * 1024 * 1024 {
            let line = if row % 1000 == 999 {
                format!("{}ERROR disk on fire again {}\n", stamp, row)
            } else {
                format!("{}info all quiet on the western front {}\n", stamp, row)
            };
            if row % 1000 == 999 {
                expect.push((off + error_at, row, 5));
            }
            off += line.len() as u64;
            row += 1;
            data.extend_from_slice(line.as_bytes());
        }
        let p = tmp("big", &data);

        let t = Instant::now();
        let out = scan(
            &p,
            &lit(b"ERROR"),
            Dir::Forward,
            0,
            64 * 1024,
            usize::MAX,
            &go(),
        )
        .unwrap();
        let elapsed = t.elapsed();
        let got: Vec<(u64, u64, usize)> = out
            .hits
            .into_iter()
            .map(|h| (h.offset, h.row, h.len))
            .collect();
        assert_eq!(got, expect);
        let mb = data.len() as f64 / (1024.0 * 1024.0);
        println!(
            "logsearch multi_mb_scan_measured: {:.1} MB forward literal scan in {:?} ({:.0} MB/s), block 64 KiB, buffer 64 KiB + 4 B, {} hits",
            mb,
            elapsed,
            mb / elapsed.as_secs_f64(),
            got.len()
        );

        // Backward over the same file: same hits reversed, plus the row pre-pass.
        let t = Instant::now();
        let out = scan(
            &p,
            &lit(b"ERROR"),
            Dir::Backward,
            off,
            64 * 1024,
            usize::MAX,
            &go(),
        )
        .unwrap();
        let elapsed = t.elapsed();
        let mut got: Vec<(u64, u64, usize)> = out
            .hits
            .into_iter()
            .map(|h| (h.offset, h.row, h.len))
            .collect();
        got.reverse();
        assert_eq!(got, expect);
        println!(
            "logsearch multi_mb_scan_measured: {:.1} MB backward scan (row pre-pass + match pass) in {:?} ({:.0} MB/s)",
            mb,
            elapsed,
            mb / elapsed.as_secs_f64()
        );
    }

    /// Against a whole-file in-memory scan — no windows, no carry, no direction — over a
    /// corpus of real log shapes, at block sizes from 1 byte to longer than the file, both
    /// directions, eight offsets. Literals only: the regex caveats are real limits.
    #[test]
    fn agrees_with_a_whole_file_scan_over_a_corpus() {
        let mut corpus: Vec<u8> = Vec::new();
        for i in 0..300 {
            match i % 7 {
                0 => corpus
                    .extend_from_slice(format!("2026-10-09T12:00:0{i} INFO row {i}\n").as_bytes()),
                1 => corpus.extend_from_slice(b"ERROR something went wrong\n"),
                2 => corpus.extend_from_slice(b"  at frame::of::a::stack::trace (file.rs:12)\n"),
                3 => corpus.extend_from_slice(b"\n"),
                4 => corpus.extend_from_slice(format!("weird \u{e9} multibyte {i}\n").as_bytes()),
                5 => corpus.extend_from_slice(b"no newline in the middle of this one"),
                _ => corpus.extend_from_slice(format!("ERROR {i} error ERROR\n").as_bytes()),
            }
        }
        let p = tmp("corpus", &corpus);
        let size = corpus.len() as u64;

        // `before[i]` is the number of newlines in `[0, i)` — the row of a
        // match starting at `i`, computed the slow honest way.
        let before: Vec<u64> = {
            let mut v = Vec::with_capacity(corpus.len() + 1);
            let mut nl = 0u64;
            for b in &corpus {
                v.push(nl);
                if *b == b'\n' {
                    nl += 1;
                }
            }
            v.push(nl);
            v
        };
        let reference = |needle: &[u8], cs: bool, from: u64, dir: Dir| -> Vec<(u64, u64, usize)> {
            let n = needle.len();
            let mut out = Vec::new();
            let mut i = 0usize;
            while i + n <= corpus.len() {
                let cand = &corpus[i..i + n];
                let hit = if cs {
                    cand == needle
                } else {
                    cand.eq_ignore_ascii_case(needle)
                };
                let start = i as u64;
                let in_range = match dir {
                    Dir::Forward => start >= from,
                    Dir::Backward => start < from,
                };
                if hit && in_range {
                    out.push((start, before[i], n));
                }
                i += 1;
            }
            if dir == Dir::Backward {
                out.reverse();
            }
            out
        };

        let needles: [(&[u8], bool); 5] = [
            (b"ERROR", true),
            (b"error", false),
            (b"\n", true),
            ("\u{e9}".as_bytes(), true),
            (b"  at ", true),
        ];
        let froms = [0u64, 1, 5, 40, 1000, size - 1, size, size + 10];
        let blocks = [
            1usize,
            2,
            3,
            8,
            64,
            1000,
            corpus.len() - 1,
            corpus.len(),
            corpus.len() + 7,
        ];
        let mut checked = 0usize;
        for block in blocks {
            for (needle, cs) in needles {
                for from in froms {
                    for dir in [Dir::Forward, Dir::Backward] {
                        let want = reference(needle, cs, from, dir);
                        let pattern = Pattern::Literal {
                            needle,
                            case_sensitive: cs,
                        };
                        let out =
                            scan(&p, &pattern, dir, from, block, usize::MAX, &go()).expect("scan");
                        let got: Vec<(u64, u64, usize)> = out
                            .hits
                            .into_iter()
                            .map(|h| (h.offset, h.row, h.len))
                            .collect();
                        assert_eq!(
                            got, want,
                            "needle {needle:?} case_sensitive={cs} from={from} dir={dir:?} block={block}"
                        );
                        checked += 1;
                    }
                }
            }
        }
        assert_eq!(checked, blocks.len() * needles.len() * froms.len() * 2);
    }
}
