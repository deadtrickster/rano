//! The log mode's not-rendered half: a file's line bookkeeping and nothing else. A tailed
//! file that is not on screen keeps a sparse line index, its size and its block heuristics —
//! no decoded rows, no wrap geometry, no highlight grid; every row it is asked for is read.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use crate::logblocks::Blocks;

/// Rows between entries of the sparse index: one `u64` per 4096 rows, turning "which byte does
/// row N start at?" into a lookup plus a short scan. Deliberately NOT every row — per-row
/// state is the cost this module exists to avoid.
pub const INDEX_EVERY: u64 = 4096;

/// Read granularity for the bookkeeping pass. One page-cache-friendly read, as
/// [`crate::loader`]'s is, and for the same reason.
const CHUNK: usize = 64 * 1024;

/// How much of a row's head is kept, to decode its first character: four bytes
/// is every UTF-8 scalar value.
const HEAD: usize = 4;

/// A file's line bookkeeping, and no rows. See the module header for what it keeps and what it
/// deliberately does not.
#[derive(Debug)]
pub struct LogTail {
    path: PathBuf,
    /// The file's length when this was opened — the line between "this row was
    /// in the file before us" and "this row is arriving".
    pub size_at_open: u64,
    /// The file's length as of the last [`LogTail::poll`].
    pub size: u64,
    /// Rows in the file, counted as their newlines arrive; a partial row at the end is not one.
    pub lines: u64,
    /// Block boundaries under the layout rule.
    pub blocks: Blocks,
    /// Sparse row → byte: `starts[k]` is the offset at which row `k * INDEX_EVERY` begins.
    starts: Vec<u64>,
    /// The byte the next read begins at, and the offset the current (possibly
    /// unfinished) row began at.
    read: u64,
    row_start: u64,
    /// The first [`HEAD`] bytes of the row being read, and how many of them are
    /// filled. Carried across reads because a row can outlast a chunk.
    head: [u8; HEAD],
    head_len: u8,
    /// How the file's bytes decode, decided from the first chunk as the loader decides it —
    /// one enum, not a copy of the file, so a latin-1 log's letters come back as letters.
    encoding: crate::encoding::Encoding,
}

impl LogTail {
    /// Open `path` and make one bookkeeping pass over it: count its rows, index
    /// every [`INDEX_EVERY`]th, and work its blocks out. **No rows are kept.**
    pub fn open(path: &Path) -> io::Result<Self> {
        let file = File::open(path)?;
        let size = file.metadata()?.len();
        let mut tail = Self {
            path: path.to_path_buf(),
            size_at_open: size,
            size,
            lines: 0,
            blocks: Blocks::new(),
            starts: Vec::new(),
            read: 0,
            row_start: 0,
            head: [0; HEAD],
            head_len: 0,
            encoding: crate::encoding::Encoding::Utf8,
        };
        tail.scan(size, true)?;
        Ok(tail)
    }

    /// Read whatever has been appended since the last call, holding back a row whose newline
    /// has not arrived: half a stack trace is not a row, and counting it would count what
    /// nobody has written.
    pub fn poll(&mut self) -> io::Result<bool> {
        let file = File::open(&self.path)?;
        let size = file.metadata()?.len();
        self.size = size;
        let before = self.lines;
        self.scan(size, false)?;
        Ok(self.lines != before)
    }

    /// Read `[self.read, to)`. `detect_encoding` is true for the first pass only: the encoding
    /// is decided from the file's beginning, so a later read of an appended tail must not
    /// re-decide it.
    fn scan(&mut self, to: u64, detect_encoding: bool) -> io::Result<()> {
        if to <= self.read {
            return Ok(());
        }
        let mut file = File::open(&self.path)?;
        file.seek(SeekFrom::Start(self.read))?;
        let mut buf = vec![0u8; CHUNK];
        let mut pos = self.read;
        let mut first = detect_encoding;
        while pos < to {
            let want = ((to - pos) as usize).min(CHUNK);
            let n = file.read(&mut buf[..want])?;
            if n == 0 {
                break; // the file shrank under us; the next poll picks it up
            }
            if first {
                first = false;
                let whole = (n as u64) == to;
                let scope = if whole {
                    crate::encoding::Scope::Whole
                } else {
                    crate::encoding::Scope::Prefix
                };
                self.encoding = crate::encoding::detect(&buf[..n], scope);
            }
            // Two passes over the bytes: the row's HEAD only matters right after a newline,
            // while the search for the next newline is a whole-chunk `position` — a branch per
            // byte for both measured far slower.
            let mut i = 0usize;
            while i < n {
                if (self.head_len as usize) < HEAD && buf[i] != b'\n' {
                    self.head[self.head_len as usize] = buf[i];
                    self.head_len += 1;
                    i += 1;
                    continue;
                }
                match buf[i..n].iter().position(|b| *b == b'\n') {
                    Some(k) => {
                        self.end_row();
                        self.row_start = pos + (i + k) as u64 + 1;
                        i += k + 1;
                    }
                    // No newline left in this chunk: whatever remains is the
                    // head of a row that continues into the next read.
                    None => i = n,
                }
            }
            pos += n as u64;
        }
        self.read = pos;
        Ok(())
    }

    /// The row just ended: count it, index it, and let the one rule see its first character.
    fn end_row(&mut self) {
        if self.lines.is_multiple_of(INDEX_EVERY) {
            self.starts.push(self.row_start);
        }
        let mut len = self.head_len as usize;
        // A row of nothing but `\r` is an EMPTY row: the reader strips CRLF
        // before the rule sees a row, so a byte-level pass must too, or a blank
        // CRLF line would "start with whitespace".
        if len == 1 && self.head[0] == b'\r' {
            len = 0;
        }

        // Lossy rather than `ok()`: a row that is not UTF-8 still needs a
        // verdict, and "starts a block" is the right one for a byte that decodes
        // to nothing.
        let first = String::from_utf8_lossy(&self.head[..len]).chars().next();
        self.blocks.push_first(first);
        self.lines += 1;
        self.head_len = 0;
    }

    /// The nearest indexed row at or below `row`, as `(row, byte offset)`. The exact offset of
    /// an arbitrary row is this plus a forward scan, which is the caller's job: only it knows
    /// whether it wants the row's start or the rows after it.
    pub fn nearest_index(&self, row: u64) -> Option<(u64, u64)> {
        let k = row / INDEX_EVERY;
        let byte = *self.starts.get(usize::try_from(k).ok()?)?;
        Some((k * INDEX_EVERY, byte))
    }

    /// The last `n` rows, read from the file and returned to be dropped — they exist while
    /// something is drawing them and not before. Reuses [`crate::loader::tail_offset`], so a
    /// 2 GiB log's last screenful costs a few hundred KiB of reading.
    pub fn last_rows(&self, n: usize) -> io::Result<Vec<Vec<char>>> {
        Ok(self.last_rows_counted(n)?.0)
    }

    /// [`last_rows`], and how many bytes of the file that read covered.
    ///
    /// The count is returned rather than derived, because the test that asserts it is asserting what
    /// this read — a count the test worked out for itself would agree with any implementation of
    /// this one, including one that read the whole file.
    pub(crate) fn last_rows_counted(&self, n: usize) -> io::Result<(Vec<Vec<char>>, u64)> {
        if n == 0 {
            return Ok((Vec::new(), 0));
        }
        let mut file = File::open(&self.path)?;
        let size = file.metadata()?.len();
        if size == 0 {
            return Ok((Vec::new(), 0));
        }
        let from = crate::loader::tail_offset(&mut file, size, n, crate::loader::TAIL_WINDOW)?;
        let bytes = crate::loader::read_from(&mut file, from)?;
        Ok((split_rows(&bytes, self.encoding), size - from))
    }

    /// The bytes this holds, from the two vectors' capacity and nothing else. Not the file:
    /// this is state per row, so it does not depend on how long the rows are.
    pub fn state_bytes(&self) -> usize {
        self.starts.capacity() * std::mem::size_of::<u64>() + self.blocks.state_bytes()
    }

    /// Whether the FILE's row `row` (1-based) continues the block above it —
    /// the 0-based/1-based conversion and the [`Blocks`] lookup in one place.
    pub fn row_continues(&self, row: u64) -> bool {
        let r = row.saturating_sub(1);
        let r = usize::try_from(r).unwrap_or(usize::MAX);
        self.blocks.block_of(r).is_some_and(|(start, _)| r > start)
    }

    /// The rows the index knows about — one per [`INDEX_EVERY`] rows.
    pub fn index_len(&self) -> usize {
        self.starts.len()
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Split `bytes` into rows as [`crate::loader`] splits a chunk: a row ends at its `\n`, a `\r`
/// before that is a line ending, and a last piece with no newline is not a row yet.
fn split_rows(bytes: &[u8], encoding: crate::encoding::Encoding) -> Vec<Vec<char>> {
    let mut out = Vec::new();
    let mut start = 0usize;
    for (i, b) in bytes.iter().enumerate() {
        if *b != b'\n' {
            continue;
        }
        let mut row = &bytes[start..i];
        if let Some(p) = row.strip_suffix(b"\r") {
            row = p;
        }
        out.push(decode(row, encoding));
        start = i + 1;
    }
    out
}

/// One row's bytes as characters, in the file's encoding — the same decision the loader's
/// `decode_row` makes.
fn decode(bytes: &[u8], encoding: crate::encoding::Encoding) -> Vec<char> {
    match encoding {
        crate::encoding::Encoding::Cp1252 => {
            crate::encoding::decode(bytes, crate::encoding::Encoding::Cp1252)
                .map(|s| s.chars().collect())
                .unwrap_or_else(|_| String::from_utf8_lossy(bytes).chars().collect())
        }
        _ => String::from_utf8_lossy(bytes).chars().collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::Instant;

    /// A scratch file, removed when the test that made it ends.
    struct Temp(PathBuf);
    impl Temp {
        fn new(name: &str, contents: &[u8]) -> Self {
            let p =
                std::env::temp_dir().join(format!("rano-logtail-{}-{name}", std::process::id()));
            fs::write(&p, contents).expect("write fixture");
            Temp(p)
        }
        fn path(&self) -> &Path {
            &self.0
        }
        fn append(&self, bytes: &[u8]) {
            use std::io::Write as _;
            let mut f = fs::OpenOptions::new()
                .append(true)
                .open(&self.0)
                .expect("append");
            f.write_all(bytes).expect("write");
            f.flush().expect("flush");
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.0);
        }
    }

    fn text(rows: &[Vec<char>]) -> Vec<String> {
        rows.iter().map(|r| r.iter().collect()).collect()
    }

    #[test]
    fn open_counts_rows_and_blocks_without_keeping_rows() {
        let body = b"one\ntwo\nthree\n";
        let t = Temp::new("basic.log", body);
        let tail = LogTail::open(t.path()).expect("open");
        assert_eq!(tail.lines, 3);
        assert_eq!(tail.blocks.count(), 3, "three unindented records");
        assert_eq!(tail.size, body.len() as u64);
        assert_eq!(tail.size_at_open, body.len() as u64);
    }

    /// The two shapes the rule is for, decided by the byte pass exactly as the row pass
    /// decides them.
    #[test]
    fn the_byte_pass_agrees_with_the_row_pass_about_blocks() {
        let body = b"ERROR boom\n  at frame::one\n  at frame::two\n\nERROR again\n";
        let t = Temp::new("blocks.log", body);
        let tail = LogTail::open(t.path()).expect("open");
        // The same rows, through the row door: a row ends at its `\n`, and the
        // empty piece after the final newline is not a row.
        let mut rows: Vec<Vec<char>> = body
            .split(|b| *b == b'\n')
            .map(|p| p.iter().map(|b| *b as char).collect())
            .collect();
        if rows.last().is_some_and(Vec::is_empty) {
            rows.pop();
        }
        assert_eq!(rows.len(), 5);
        assert_eq!(tail.blocks, Blocks::of(&rows), "the two doors disagree");
        // The blocks are the first trace [0,3), the blank [3,4) and the record
        // [4,5) — three, because a blank row is a block of its own and the two
        // traces are the two that span more than one row.
        assert_eq!(tail.blocks.count(), 3);
        assert_eq!(tail.lines, 5);
    }

    #[test]
    fn a_partial_final_row_is_not_a_row() {
        let t = Temp::new("partial.log", b"one\ntwo\nhalf a li");
        let tail = LogTail::open(t.path()).expect("open");
        assert_eq!(tail.lines, 2, "the unfinished row is not counted");
        assert_eq!(tail.blocks.count(), 2);
    }

    /// **The count moves when the file does, and the partial row moves with it.**
    #[test]
    fn poll_counts_growth_and_holds_the_partial_row_back() {
        let t = Temp::new("follow.log", b"one\n");
        let mut tail = LogTail::open(t.path()).expect("open");
        assert_eq!(tail.lines, 1);

        // A row with no newline yet: nothing changes.
        t.append(b"tw");
        assert!(!tail.poll().expect("poll"));
        assert_eq!(tail.lines, 1, "an unfinished row was counted");

        // Its newline: the row appears, once.
        t.append(b"o\n");
        assert!(tail.poll().expect("poll"));
        assert_eq!(tail.lines, 2);
        assert!(!tail.poll().expect("poll"), "nothing new the second time");

        // An indented continuation joins the block above.
        t.append(b"  more\nthird\n");
        assert!(tail.poll().expect("poll"));
        assert_eq!(tail.lines, 4);
        assert_eq!(tail.blocks.count(), 3, "the indented row continued");
    }

    /// A blank CRLF line is an EMPTY row and so starts a block — where a byte pass that took
    /// `\r` for whitespace would swallow the blank.
    #[test]
    fn crlf_and_blank_lines_get_the_same_verdict_as_the_row_door() {
        let body = b"ERROR boom\r\n  at frame\r\n\r\nnext\r\n";
        let t = Temp::new("crlf.log", body);
        let tail = LogTail::open(t.path()).expect("open");
        let rows: Vec<Vec<char>> = vec![
            "ERROR boom".chars().collect(),
            "  at frame".chars().collect(),
            Vec::new(),
            "next".chars().collect(),
        ];
        assert_eq!(tail.blocks, Blocks::of(&rows));
        assert_eq!(tail.lines, 4);
        assert_eq!(tail.blocks.count(), 3, "trace, blank, next");
    }

    #[test]
    fn a_row_beginning_with_a_multibyte_character_is_not_whitespace() {
        // U+00A0 IS `char::is_whitespace`, and it is two bytes: the byte pass
        // has to decode it rather than look at 0xC2 and give up.
        let body = "starts\n\u{a0}continues\n".as_bytes();
        let t = Temp::new("nbsp.log", body);
        let tail = LogTail::open(t.path()).expect("open");
        assert_eq!(tail.lines, 2);
        assert_eq!(
            tail.blocks.count(),
            1,
            "a NBSP-indented row continues the block above"
        );
        // And the same two rows through the row door, which is the definition.
        let rows: Vec<Vec<char>> = vec![
            "starts".chars().collect(),
            "\u{a0}continues".chars().collect(),
        ];
        assert_eq!(tail.blocks, Blocks::of(&rows));
    }

    /// The index is sparse and the count is free: 10 000 rows give three entries.
    #[test]
    fn the_index_is_sparse() {
        // Fixed-width rows, so the offsets are arithmetic: every row is 7 bytes.
        let body: Vec<u8> = (0..10_000)
            .map(|i| format!("{i:06}\n"))
            .collect::<String>()
            .into_bytes();
        let t = Temp::new("index.log", &body);
        let tail = LogTail::open(t.path()).expect("open");
        assert_eq!(tail.lines, 10_000);
        assert_eq!(tail.index_len(), 3, "0, 4096, 8192");
        assert_eq!(tail.nearest_index(0), Some((0, 0)));
        assert_eq!(tail.nearest_index(4095), Some((0, 0)));
        assert_eq!(tail.nearest_index(4096), Some((4096, 4096 * 7)));
        assert_eq!(tail.nearest_index(9999), Some((8192, 8192 * 7)));
    }

    /// The budget, as a number: the same rows in two files whose rows are ten times longer
    /// cost the same state.
    #[test]
    fn state_does_not_depend_on_how_long_the_rows_are() {
        let short: Vec<u8> = (0..50_000)
            .map(|i| format!("{i}\n"))
            .collect::<String>()
            .into_bytes();
        let long: Vec<u8> = (0..50_000)
            .map(|i| format!("{i}{}\n", "x".repeat(100)))
            .collect::<String>()
            .into_bytes();
        assert!(
            long.len() > 4 * short.len(),
            "the fixtures must differ in size"
        );
        let a = Temp::new("short.log", &short);
        let b = Temp::new("long.log", &long);
        let ta = LogTail::open(a.path()).expect("open");
        let tb = LogTail::open(b.path()).expect("open");
        assert_eq!(ta.lines, tb.lines);
        assert_eq!(
            ta.state_bytes(),
            tb.state_bytes(),
            "state grew with the row length: {} against {}",
            ta.state_bytes(),
            tb.state_bytes()
        );
        // And it is small per row in absolute terms: a usize per block (every
        // row here starts one) plus a u64 per 4096 rows.
        let per_row = ta.state_bytes() as f64 / ta.lines as f64;
        assert!(per_row < 16.0, "{per_row} bytes per row");
    }

    /// The rows come back from the file, in order, exactly `n` of them.
    #[test]
    fn last_rows_reads_the_end_of_the_file() {
        let body: Vec<u8> = (0..500)
            .map(|i| format!("line {i}\n"))
            .collect::<String>()
            .into_bytes();
        let t = Temp::new("rows.log", &body);
        let tail = LogTail::open(t.path()).expect("open");
        let got = text(&tail.last_rows(3).expect("rows"));
        assert_eq!(got, vec!["line 497", "line 498", "line 499"]);
        // More than the file has: the whole file.
        let all = text(&tail.last_rows(1000).expect("rows"));
        assert_eq!(all.len(), 500);
        assert_eq!(all[0], "line 0");
        // Zero rows asks for nothing.
        assert!(tail.last_rows(0).expect("rows").is_empty());
    }

    #[test]
    fn last_rows_does_not_invent_a_row_from_a_partial_line() {
        let t = Temp::new("partialrows.log", b"one\ntwo\nhalf");
        let tail = LogTail::open(t.path()).expect("open");
        assert_eq!(text(&tail.last_rows(9).expect("rows")), vec!["one", "two"]);
    }

    /// Reading the window costs the window, not the file.
    #[test]
    fn a_large_file_gives_its_last_rows_quickly() {
        let body: Vec<u8> = (0..100_000)
            .map(|i| format!("2026-10-09T12:00:00 INFO a row of a log {i}\n"))
            .collect::<String>()
            .into_bytes();
        assert!(body.len() > 4 * 1024 * 1024, "the fixture must be big");
        let t = Temp::new("bigrows.log", &body);
        let tail = LogTail::open(t.path()).expect("open");
        assert_eq!(tail.lines, 100_000);

        // What was READ, not how long it took: the rows come from one tail window at the end of the
        // file, and "not the file" is a count. The count comes back with the rows, so it is the read
        // that happened and not one the test worked out for itself. A clock is printed because it is
        // worth seeing, and not asserted, because a busy machine answers a clock and cannot answer a
        // count.
        let t0 = Instant::now();
        let (rows, read) = tail.last_rows_counted(40).expect("rows");
        let dt = t0.elapsed();
        assert_eq!(rows.len(), 40);
        assert_eq!(
            rows[39].iter().collect::<String>(),
            "2026-10-09T12:00:00 INFO a row of a log 99999"
        );
        let size = body.len() as u64;
        println!("a screenful: {read} of {size} bytes read in {dt:?}");
        assert!(
            read <= crate::loader::TAIL_WINDOW,
            "a screenful read {read} of {size} bytes — more than one tail window"
        );
    }

    /// The bookkeeping pass over a large file is one forward read; the timing
    /// asserted as an order of magnitude, not a bound.
    #[test]
    fn a_large_file_is_indexed_in_one_pass() {
        let body: Vec<u8> = (0..200_000)
            .map(|i| format!("2026-10-09T12:00:00 INFO a row of a log {i}\n"))
            .collect::<String>()
            .into_bytes();
        let t = Temp::new("bigpass.log", &body);
        let t0 = Instant::now();
        let tail = LogTail::open(t.path()).expect("open");
        let dt = t0.elapsed();
        assert_eq!(tail.lines, 200_000);
        assert_eq!(tail.index_len(), 49, "0, 4096, … 196608");
        assert!(dt.as_secs() < 5, "indexing 9 MB took {dt:?}");
        println!(
            "logtail: {} rows of {:.1} MB indexed in {dt:?}, state {} KiB ({:.1} bytes/row)",
            tail.lines,
            body.len() as f64 / (1024.0 * 1024.0),
            tail.state_bytes() / 1024,
            tail.state_bytes() as f64 / tail.lines as f64
        );
    }

    /// `row_continues` is `Blocks`' verdict with the row arithmetic done once.
    #[test]
    fn row_continues_agrees_with_the_blocks() {
        let t = Temp::new("continues.log", b"ERROR boom\n  at one\n  at two\nnext\n");
        let tail = LogTail::open(t.path()).expect("open");
        assert!(!tail.row_continues(1), "a record");
        assert!(tail.row_continues(2), "a frame");
        assert!(tail.row_continues(3), "a frame");
        assert!(!tail.row_continues(4), "the next record");
        // Row 0 and a row past the end are not rows, and neither continues one.
        assert!(!tail.row_continues(0));
        assert!(!tail.row_continues(99));
    }

    /// The two halves join: `LogTail` holds the count, the index and the boundaries and no
    /// rows; `LogPane` holds rows and no file. This is the four lines a host needs, with the
    /// row numbers and block markers from the same pass.
    #[test]
    fn the_bookkeeping_fills_a_pane() {
        use crate::agent::logpane::{LogPane, LogRow};
        let body: Vec<u8> = (0..500)
            .map(|i| format!("row {i}\n"))
            .collect::<String>()
            .into_bytes();
        let t = Temp::new("pane.log", &body);
        let tail = LogTail::open(t.path()).expect("open");

        let raw = tail.last_rows(4).expect("rows");
        let first_row = tail.lines - raw.len() as u64 + 1;
        let pane = LogPane {
            name: "pane.log".into(),
            rows: raw
                .into_iter()
                .enumerate()
                .map(|(i, r)| {
                    let row = first_row + i as u64;
                    LogRow {
                        text: r.iter().collect(),
                        continues: tail.row_continues(row),
                    }
                })
                .collect(),
            first_row,
            total: tail.lines,
            scroll: 0,
            following: true,
        };

        let drawn = crate::agent::testing::drawn(&pane, 40, 8);
        assert_eq!(first_row, 497);
        assert!(drawn[1].contains("500 lines"), "{:?}", drawn[1]);
        // The gutter pads to the file's width, and the numbers are the file's.
        // File row 497 holds the text the fixture labels `row 496` (the fixture's
        // rows are 0-based, the file's lines are not).
        assert!(drawn[3].starts_with(" 497 "), "{:?}", drawn[3]);
        assert!(drawn[3].contains("row 496"), "{:?}", drawn[3]);
        assert!(drawn[6].starts_with(" 500 "), "{:?}", drawn[6]);
        assert!(drawn[6].contains("row 499"), "{:?}", drawn[6]);
    }

    #[test]
    fn a_missing_file_fails_at_open() {
        let missing = std::env::temp_dir().join("rano-logtail-there-is-no-such-file");
        let _ = fs::remove_file(&missing);
        assert!(LogTail::open(&missing).is_err());
    }

    #[test]
    fn an_empty_file_is_zero_rows() {
        let t = Temp::new("empty.log", b"");
        let tail = LogTail::open(t.path()).expect("open");
        assert_eq!(tail.lines, 0);
        assert_eq!(tail.blocks.count(), 0);
        assert!(tail.last_rows(10).expect("rows").is_empty());
    }
}
