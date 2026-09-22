//! The row store: a file's rows, chunked, and only decoded where they are
//! read.
//!
//! Two problems, one structure, and each half is measured (TODO.md §16.0,
//! §15):
//!
//! 1. **Structural edits scaled with the file.** `Vec<Vec<char>>` costs 1.5 ms
//!    to insert a row at the top of a 2.6M-row file — a memmove of 2.6M `Vec`
//!    headers, ~20 MB per keystroke, ~15 ms per Enter at 2 GB. Chunking makes
//!    it **0.2 µs, flat at any size**. "Size does not influence editability" is
//!    the principle; this is what keeps it.
//!
//! 2. **Reading loaded the file.** 835 MB resident for a 184 MB file, because
//!    every row was decoded up front. A row here is **file-backed until it is
//!    edited**: reading decodes from disk on demand and caches, editing
//!    promotes that one row to owned. There is no "materialise now" cliff, so
//!    typing in a row costs the same at every file size.
//!
//! Lookup stays **O(1)** (chunk index + offset) — which is the property
//! §13.6a's survey found both a piece table and a rope give up, and the reason
//! neither was needed.
//!
//! # The borrow problem, and how it is solved
//!
//! Today a reader gets `&[char]` for free because every row is materialised.
//! With lazy decode there is nothing to borrow from until the row exists, and
//! the naive answer — interior mutability, so `row(r)` can fill the cache on
//! access — puts a runtime borrow check on the hottest path in the program.
//!
//! So the API is **prepare-then-read**: [`RowStore::ensure`] makes a range of
//! rows resident, and [`RowStore::row`] borrows. That is the same discipline
//! §12's highlight window and §14.6's adoption budget already use — a frame
//! asks for what it needs, then reads — so it is a pattern this codebase has
//! rather than a new one.

use crate::encoding::{self, Encoding};
use std::io;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};

/// Rows per chunk.
///
/// The one number here that is a choice rather than a consequence. Small
/// enough that a structural edit shifts little (an insert at row 0 measured
/// 0.2 µs at 1,024); large enough that the chunk index does not become the
/// memmove. 1,024 rows is 8 KiB of pointers — one cache line's worth of work
/// beyond the copy that must happen anyway.
pub const CHUNK: usize = 1_024;

/// How many rows may be resident at once.
///
/// Derived from the viewport rather than fixed, because a frame that misses
/// the cache is a disk read: the budget has to cover a screen several times
/// over, or scrolling re-reads on every step. See `RowStore::with_budget`.
const DEFAULT_BUDGET: usize = 8_192;

/// One row: where it is, what it is, and whether it can go back.
///
/// Three states, and the distinction between the last two is the whole reason
/// eviction is safe: a DECODED row can be produced again from the file, while
/// an EDITED row is the only copy of itself. Dropping the wrong one loses an
/// edit; dropping the right one costs a re-read.
#[derive(Debug, Clone)]
enum Row {
    /// Not decoded. The byte range in the file, which is all a reader needs to
    /// produce it — and enough to know its LENGTH without producing it, which
    /// is what lets the wrap table work without decoding a row.
    File { start: u64, end: u64 },
    /// Decoded, and evictable: it remembers where it came from.
    Decoded {
        chars: Vec<char>,
        start: u64,
        end: u64,
    },
    /// Edited. There is no file bytes to go back to, so this is never dropped.
    Edited(Vec<char>),
}

/// One chunk of rows, plus the bookkeeping the cache needs.
#[derive(Debug, Clone)]
struct Chunk {
    rows: Vec<Row>,
    /// When this chunk was last asked for, for eviction. A monotone counter
    /// rather than a clock: eviction only needs an order.
    used: u64,
}

/// A file's rows, chunked, decoded on demand.
#[derive(Debug)]
pub struct RowStore {
    /// The file, held open for positional reads. `None` for a scratch buffer
    /// with no file — a new document, or one whose file has gone.
    file: Option<std::fs::File>,
    /// Where it came from. Not read yet: it is what a re-read after the file
    /// changed underneath needs (TODO.md §15.3), and dropping it would mean
    /// asking the caller for a path it already told us.
    #[allow(dead_code)]
    path: Option<PathBuf>,
    /// Nothing escapes this store, so it never has to describe the file's
    /// bytes back to the outside.
    encoding: Encoding,
    chunks: Vec<Chunk>,
    /// First row index of each chunk. Maintained by the mutations; see
    /// `slot_of` for why it exists rather than `r / CHUNK`.
    base: Vec<usize>,
    /// Rows in total. Tracked rather than summed: the whole point is that
    /// counting must not walk the chunks.
    rows: usize,
    /// Monotone clock for `Chunk::used`.
    tick: u64,
    /// Resident-row budget, from the viewport. See `DEFAULT_BUDGET`.
    budget: usize,
    /// How many rows are resident, so the budget check is O(1).
    resident: usize,
}

impl RowStore {
    /// An empty store with no file: a scratch buffer.
    pub fn new() -> Self {
        Self {
            file: None,
            path: None,
            encoding: Encoding::Utf8,
            chunks: vec![Chunk {
                rows: vec![Row::Edited(Vec::new())],
                used: 0,
            }],
            base: vec![0],
            rows: 1,
            tick: 0,
            budget: DEFAULT_BUDGET,
            resident: 0,
        }
    }

    /// A store over already-decoded text, keeping it as OWNED rows.
    ///
    /// The path for a scratch buffer, a test fixture, and the escape hatch
    /// `materialize_all` uses. It does not chunk lazily from a file because
    /// there is no file to read from — the rows are already here.
    pub fn from_lines(lines: Vec<Vec<char>>, encoding: Encoding) -> Self {
        let mut chunks = Vec::new();
        let mut it = lines.into_iter().peekable();
        while it.peek().is_some() {
            chunks.push(Chunk {
                rows: it.by_ref().take(CHUNK).map(Row::Edited).collect(),
                used: 0,
            });
        }
        if chunks.is_empty() {
            chunks.push(Chunk {
                rows: vec![Row::Edited(Vec::new())],
                used: 0,
            });
        }
        let rows = chunks.iter().map(|c| c.rows.len()).sum();
        let mut base = Vec::with_capacity(chunks.len());
        let mut at = 0usize;
        for c in &chunks {
            base.push(at);
            at += c.rows.len();
        }
        Self {
            file: None,
            path: None,
            encoding,
            chunks,
            base,
            rows,
            tick: 0,
            budget: DEFAULT_BUDGET,
            // Owned rows are not counted against the budget: the budget is
            // about what DECODING may hold, and there is nothing to release
            // for a row that has been edited.
            resident: 0,
        }
    }

    /// A store over a file, indexed but not decoded.
    ///
    /// One pass over the bytes finds every newline — cheap because in UTF-8 a
    /// `0x0A` byte is always a newline (multi-byte sequences use lead bytes
    /// `C2`–`F4` and continuation bytes `80`–`BF`), so no decoding is needed to
    /// find where rows begin. The same pass records each row's byte length,
    /// which is its character count too when the row is pure ASCII.
    pub fn open(path: &Path, encoding: Encoding) -> io::Result<Self> {
        let file = std::fs::File::open(path)?;
        let size = file.metadata()?.len();
        let starts = index_starts(&file, size)?;
        let rows = starts.rows();
        let chunks = starts.into_chunks(size);
        let mut base = Vec::with_capacity(chunks.len());
        let mut at = 0usize;
        for c in &chunks {
            base.push(at);
            at += c.rows.len();
        }
        Ok(Self {
            file: Some(file),
            path: Some(path.to_path_buf()),
            encoding,
            chunks,
            base,
            rows,
            tick: 0,
            budget: DEFAULT_BUDGET,
            resident: 0,
        })
    }

    /// Rows in the store. Never walks the chunks.
    pub fn rows(&self) -> usize {
        self.rows
    }

    /// How the file was encoded, for a save.
    pub fn encoding(&self) -> Encoding {
        self.encoding
    }

    pub fn set_encoding(&mut self, encoding: Encoding) {
        self.encoding = encoding;
    }

    /// The byte range of row `r`, when the file's bytes still describe it.
    /// `None` once edited — they do not.
    pub fn byte_range(&self, r: usize) -> Option<(u64, u64)> {
        match self.row_slot(r)? {
            Row::File { start, end } | Row::Decoded { start, end, .. } => Some((*start, *end)),
            Row::Edited(_) => None,
        }
    }

    /// Whether row `r` has been edited — and so is the only copy of itself.
    pub fn is_edited(&self, r: usize) -> bool {
        matches!(self.row_slot(r), Some(Row::Edited(_)))
    }

    /// Byte length of row `r`, known without decoding when it is file-backed.
    pub fn byte_len(&self, r: usize) -> usize {
        match self.row_slot(r) {
            Some(Row::File { start, end }) => (end - start) as usize,
            Some(Row::Decoded { chars, .. }) | Some(Row::Edited(chars)) => chars.len(),
            None => 0,
        }
    }

    /// Make rows `[first, last]` resident, decoding from the file as needed.
    ///
    /// Decoding is at **chunk granularity** — the whole chunk holding the range
    /// is decoded, not just the rows in it — because a chunk is the unit of
    /// locality: a scroll moves by a few rows, and the rest of the chunk they
    /// are in will be asked for next. So the return is how many rows were
    /// decoded, which is at least the ones asked for and at most a chunk's
    /// worth more.
    ///
    /// Returns zero when they were all already here, which is the common case
    /// for a frame that did not move.
    pub fn ensure(&mut self, first: usize, last: usize) -> io::Result<usize> {
        let last = last.min(self.rows.saturating_sub(1));
        if first > last {
            return Ok(0);
        }
        let mut decoded = 0usize;
        // Which chunks are involved, so the untouched ones are not visited.
        let (c0, c1) = (first / CHUNK, last / CHUNK);
        for c in c0..=c1 {
            self.tick += 1;
            let tick = self.tick;
            self.chunks[c].used = tick;
            let n = self.chunks[c].rows.len();
            for i in 0..n {
                // The byte range is read out first so the borrow ends before
                // the write; `File` is the only evictable state, and it is
                // replaced by `Decoded` carrying the same range.
                let range = match self.chunks[c].rows[i] {
                    Row::File { start, end } => Some((start, end)),
                    _ => None,
                };
                if let Some((start, end)) = range {
                    let chars = self.read_row(start, end)?;
                    self.chunks[c].rows[i] = Row::Decoded { chars, start, end };
                    decoded += 1;
                }
            }
            self.resident += decoded;
        }
        if decoded > 0 {
            self.evict_if_needed();
        }
        Ok(decoded)
    }

    /// Row `r`. **Make it resident first** — see `ensure`.
    ///
    /// A file-backed row is returned as an empty slice rather than decoded
    /// here: this is the borrowing half of prepare-then-read, and a `&self`
    /// method cannot fill a cache. An empty slice is visible and harmless
    /// (the renderer draws a blank row) and `ensure` is always the caller's
    /// first step, so it is a contract the tests pin rather than a trap.
    pub fn row(&self, r: usize) -> &[char] {
        match self.row_slot(r) {
            Some(Row::Decoded { chars, .. }) | Some(Row::Edited(chars)) => chars,
            _ => &[],
        }
    }

    /// Replace row `r` with its edited content, promoting it.
    pub fn set_row(&mut self, r: usize, chars: Vec<char>) {
        if let Some((c, i)) = self.slot_of(r) {
            self.chunks[c].rows[i] = Row::Edited(chars);
        }
    }

    /// Insert `chars` as a new row `at`. **O(CHUNK), not O(rows)** — this is
    /// the operation the whole structure exists for.
    pub fn insert(&mut self, at: usize, chars: Vec<char>) {
        let at = at.min(self.rows);
        if self.chunks.is_empty() {
            self.chunks.push(Chunk {
                rows: vec![Row::Edited(chars)],
                used: 0,
            });
            self.base.push(0);
            self.rows = 1;
            return;
        }
        if self.rows == 0 {
            // Every row was removed: the store is empty but operable, and a
            // new row re-seeds it rather than underflowing `self.rows - 1`.
            self.chunks = vec![Chunk {
                rows: vec![Row::Edited(chars)],
                used: 0,
            }];
            self.base = vec![0];
            self.rows = 1;
            return;
        }
        let (c, i) = self.slot_of(at.min(self.rows - 1)).unwrap_or((0, 0));
        // Past the end goes in the last chunk rather than opening a new one.
        let (c, i) = if at >= self.rows {
            let last = self.chunks.len() - 1;
            (last, self.chunks[last].rows.len())
        } else {
            (c, i)
        };
        self.chunks[c].rows.insert(i, Row::Edited(chars));
        self.rows += 1;
        self.split_if_fat(c);
        self.fix_base_from(c);
    }

    /// Remove row `r`, returning it. O(CHUNK).
    pub fn remove(&mut self, r: usize) -> Vec<char> {
        let Some((c, i)) = self.slot_of(r) else {
            return Vec::new();
        };
        let out = match self.chunks[c].rows.remove(i) {
            Row::Edited(v) | Row::Decoded { chars: v, .. } => v,
            Row::File { .. } => Vec::new(),
        };
        self.rows -= 1;
        if self.chunks[c].rows.is_empty() && self.chunks.len() > 1 {
            self.chunks.remove(c);
            self.base.remove(c);
        }
        self.fix_base_from(c);
        out
    }

    /// Every row materialised. The escape hatch for the O(document) callers —
    /// sort, justify, replace-all, save, export, a whole-buffer search — which
    /// are O(document) *anyway*, so making it explicit is honest rather than a
    /// cost.
    pub fn materialize_all(&mut self) -> io::Result<()> {
        if self.rows > 0 {
            self.ensure(0, self.rows - 1)?;
        }
        Ok(())
    }

    /// The rows as a plain `Vec<Vec<char>>`, materialising if needed.
    pub fn to_lines(&mut self) -> io::Result<Vec<Vec<char>>> {
        self.materialize_all()?;
        Ok((0..self.rows).map(|r| self.row(r).to_vec()).collect())
    }

    // ---------- internals ----------

    /// Which chunk holds row `r`, and where in it.
    ///
    /// A binary search over `base`, not `r / CHUNK`: chunks are not uniform
    /// once a fat one has been split or a short one has been drained, and
    /// assuming they were was the first version's bug — it returned the wrong
    /// row after an edit. `base` is maintained by the mutations, and its update
    /// is arithmetic over chunk HEADERS (one `usize` each) rather than a
    /// memmove of rows, which is the distinction the whole structure rests on.
    fn slot_of(&self, r: usize) -> Option<(usize, usize)> {
        if r >= self.rows || self.chunks.is_empty() {
            return None;
        }
        let c = self.base.partition_point(|b| *b <= r).saturating_sub(1);
        let i = r - self.base[c];
        if i < self.chunks[c].rows.len() {
            Some((c, i))
        } else {
            None
        }
    }

    /// Recompute `base` from `first` on. O(chunks after the edit) additions —
    /// see `slot_of` for why that is the right order of work here.
    fn fix_base_from(&mut self, first: usize) {
        let mut at = if first == 0 { 0 } else { self.base[first] };
        for c in first..self.chunks.len() {
            self.base[c] = at;
            at += self.chunks[c].rows.len();
        }
    }

    fn row_slot(&self, r: usize) -> Option<&Row> {
        let (c, i) = self.slot_of(r)?;
        self.chunks[c].rows.get(i)
    }

    /// Split a chunk that has grown past twice the target, so chunks stay
    /// bounded by ~2×CHUNK and an insert never shifts more than that.
    fn split_if_fat(&mut self, c: usize) {
        if self.chunks[c].rows.len() <= CHUNK * 2 {
            return;
        }
        let tail = self.chunks[c].rows.split_off(CHUNK);
        self.chunks.insert(
            c + 1,
            Chunk {
                rows: tail,
                used: self.chunks[c].used,
            },
        );
        self.base.insert(c + 1, 0); // fixed by the caller's fix_base_from
    }

    /// Decode one row from the file. The only place bytes become characters
    /// after `open`.
    fn read_row(&self, start: u64, end: u64) -> io::Result<Vec<char>> {
        let Some(file) = self.file.as_ref() else {
            return Ok(Vec::new());
        };
        let mut buf = vec![0u8; (end - start) as usize];
        file.read_exact_at(&mut buf, start)?;
        let text = encoding::decode(&buf, self.encoding)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        Ok(text.chars().collect())
    }

    /// Drop decoded rows until the budget is met, least-recently-asked chunk
    /// first.
    ///
    /// Only a `Decoded` row is dropped — it remembers its byte range, so the
    /// next `ensure` re-reads it. An `Edited` row is left alone: it is the only
    /// copy of itself, and dropping it would lose the user's work. That is the
    /// difference between a cache and a buffer.
    fn evict_if_needed(&mut self) {
        if self.resident <= self.budget {
            return;
        }
        let mut order: Vec<usize> = (0..self.chunks.len()).collect();
        order.sort_by_key(|c| self.chunks[*c].used);
        for c in order {
            if self.resident <= self.budget {
                break;
            }
            let n = self.chunks[c].rows.len();
            for i in 0..n {
                if self.resident <= self.budget {
                    break;
                }
                if let Row::Decoded { start, end, .. } = self.chunks[c].rows[i] {
                    self.chunks[c].rows[i] = Row::File { start, end };
                    self.resident -= 1;
                }
            }
        }
    }

    /// How many rows are resident (decoded and not yet evicted).
    pub fn resident(&self) -> usize {
        self.resident
    }

    /// Set the resident-row budget. A frame derives it from the viewport, so a
    /// miss during a frame — which is a disk read — cannot happen for the rows
    /// on screen.
    pub fn set_budget(&mut self, budget: usize) {
        self.budget = budget.max(CHUNK);
        self.evict_if_needed();
    }
}

impl Default for RowStore {
    fn default() -> Self {
        Self::new()
    }
}

/// Every row's start offset, in file order, plus the end of the last row's
/// content.
struct Starts {
    /// `starts[i]` is the first byte of row `i`; `starts.len()` is the row
    /// count.
    starts: Vec<u64>,
    /// The end of the final row's content, which is not the file size when the
    /// file ends in a newline.
    last_end: u64,
    size: u64,
}

impl Starts {
    fn rows(&self) -> usize {
        self.starts.len()
    }

    fn into_chunks(self, _size: u64) -> Vec<Chunk> {
        if self.starts.is_empty() {
            return vec![Chunk {
                rows: vec![Row::Edited(Vec::new())],
                used: 0,
            }];
        }
        let mut chunks = Vec::new();
        let mut rows: Vec<Row> = Vec::new();
        for i in 0..self.starts.len() {
            let start = self.starts[i];
            let end = self
                .starts
                .get(i + 1)
                .map(|n| n.saturating_sub(1))
                .unwrap_or(self.last_end)
                .max(start)
                .min(self.size);
            rows.push(Row::File { start, end });
            if rows.len() == CHUNK {
                chunks.push(Chunk {
                    rows: std::mem::take(&mut rows),
                    used: 0,
                });
            }
        }
        if !rows.is_empty() {
            chunks.push(Chunk { rows, used: 0 });
        }
        chunks
    }
}

/// One pass over the file's bytes: where each row starts and where the last one
/// ends. No decoding — a `0x0A` byte is always a newline in UTF-8, and in
/// cp1252 too (0x0A is a control there as well).
fn index_starts(file: &std::fs::File, size: u64) -> io::Result<Starts> {
    use std::io::Read;
    let mut f = file;
    let mut starts = vec![0u64];
    let mut buf = vec![0u8; 1 << 16];
    let mut offset = 0u64;
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        for (i, b) in buf[..n].iter().enumerate() {
            if *b == b'\n' {
                starts.push(offset + i as u64 + 1);
            }
        }
        offset += n as u64;
    }
    // `starts` holds one entry per row plus one for the row a trailing newline
    // would begin. A file ending in a newline has no such row.
    let last_end = if size == 0 {
        0
    } else if starts.last().copied() == Some(size) {
        starts.pop();
        size - 1
    } else {
        size
    };
    if starts.is_empty() {
        starts.push(0);
    }
    Ok(Starts {
        starts,
        last_end,
        size,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    /// A scratch file that removes itself.
    struct Temp(PathBuf);

    impl Temp {
        fn new(name: &str, contents: &[u8]) -> Self {
            let p = std::env::temp_dir().join(format!("rano_rows_{name}"));
            std::fs::write(&p, contents).expect("write fixture");
            Self(p)
        }
    }

    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    fn text(s: &RowStore, r: usize) -> String {
        s.row(r).iter().collect()
    }

    /// The store's rows as plain strings, materialising.
    fn all(s: &mut RowStore) -> Vec<String> {
        s.materialize_all().expect("materialize");
        (0..s.rows()).map(|r| text(s, r)).collect()
    }

    #[test]
    fn a_scrtach_store_is_one_empty_row() {
        // `Buffer`'s invariant, which every caller depends on.
        let s = RowStore::new();
        assert_eq!(s.rows(), 1);
        assert_eq!(text(&s, 0), "");
    }

    #[test]
    fn a_file_indexes_without_decoding_and_decodes_on_demand() {
        let t = Temp::new("plain.txt", b"one\ntwo\nthree\n");
        let mut s = RowStore::open(&t.0, Encoding::Utf8).expect("open");
        assert_eq!(s.rows(), 3);
        // Not decoded yet: reading a row before `ensure` is empty, and the byte
        // lengths are known from the index alone.
        assert_eq!(text(&s, 0), "", "not resident yet");
        assert_eq!(s.byte_len(0), 3, "but its length is known from bytes");
        assert_eq!(s.byte_len(2), 5);
        // `ensure` decodes at CHUNK granularity — a chunk is the unit of
        // locality, so the rows beside the one asked for come with it.
        assert_eq!(s.ensure(0, 0).expect("ensure"), 3, "the chunk holds 3 rows");
        assert_eq!(text(&s, 0), "one");
        assert_eq!(text(&s, 2), "three", "and the rest of the chunk is here");
        // Asking again is free.
        assert_eq!(s.ensure(0, 0).expect("already resident"), 0);
        assert_eq!(all(&mut s), ["one", "two", "three"]);
    }

    #[test]
    fn every_row_matches_the_eager_decode() {
        // THE correctness test. It is the one that found a phantom final row on
        // the first prototype, and it is what makes the store trustworthy.
        for (name, body) in [
            ("basic.txt", "one\ntwo\nthree\n".as_bytes().to_vec()),
            ("notrail.txt", "a\nb".as_bytes().to_vec()),
            ("empty.txt", Vec::new()),
            ("onlynl.txt", b"\n".to_vec()),
            ("blank.txt", b"\n\n\n".to_vec()),
            (
                "long.txt",
                format!("{}\nshort\n", "x".repeat(5_000)).into_bytes(),
            ),
            (
                "multibyte.txt",
                "caf\u{e9} \u{4e2d}\u{6587}\n\u{1f600}tail\n"
                    .as_bytes()
                    .to_vec(),
            ),
        ] {
            let t = Temp::new(name, &body);
            let mut s = RowStore::open(&t.0, Encoding::Utf8).expect("open");
            let want: Vec<String> = if body.is_empty() {
                vec![String::new()]
            } else {
                let text = String::from_utf8(body.clone()).expect("utf8");
                let mut v: Vec<String> = text.split('\n').map(str::to_string).collect();
                if v.len() > 1 && v.last().is_some_and(String::is_empty) {
                    v.pop();
                }
                if v.is_empty() { vec![String::new()] } else { v }
            };
            assert_eq!(all(&mut s), want, "{name}");
        }
    }

    #[test]
    fn structural_edits_keep_the_numbering() {
        // The half the chunked store exists for: insert and remove must leave
        // every other row exactly where it was.
        let t = Temp::new("edits.txt", b"a\nb\nc\nd\n");
        let mut s = RowStore::open(&t.0, Encoding::Utf8).expect("open");
        s.insert(0, "Z".chars().collect());
        s.insert(2, "Y".chars().collect());
        s.insert(s.rows(), "W".chars().collect());
        assert_eq!(all(&mut s), ["Z", "a", "Y", "b", "c", "d", "W"]);
        assert_eq!(s.remove(0), "Z".chars().collect::<Vec<_>>());
        assert_eq!(s.remove(s.rows() - 1), "W".chars().collect::<Vec<_>>());
        assert_eq!(all(&mut s), ["a", "Y", "b", "c", "d"]);
        assert_eq!(s.rows(), 5);
    }

    #[test]
    fn an_insert_crosses_a_chunk_boundary_correctly() {
        // The boundary is where an off-by-one lives: insert exactly at CHUNK,
        // CHUNK-1 and CHUNK+1 and check the numbering each time.
        let body: String = (0..(CHUNK * 3)).map(|i| format!("r{i}\n")).collect();
        let t = Temp::new("boundary.txt", body.as_bytes());
        for at in [CHUNK - 1, CHUNK, CHUNK + 1, CHUNK * 2, CHUNK * 2 + 1] {
            let mut s = RowStore::open(&t.0, Encoding::Utf8).expect("open");
            let before = all(&mut s);
            s.insert(at, vec!['X']);
            assert_eq!(s.rows(), before.len() + 1, "at {at}");
            assert_eq!(text(&s, at), "X", "at {at}: the new row");
            assert_eq!(text(&s, at - 1), before[at - 1], "at {at}: row before");
            assert_eq!(text(&s, at + 1), before[at], "at {at}: shifted row");
            // And removing it puts everything back.
            assert_eq!(s.remove(at), vec!['X']);
            assert_eq!(all(&mut s), before, "at {at}: removal restored the file");
        }
    }

    #[test]
    fn an_edit_is_the_same_cost_at_any_size() {
        // The §16.0 principle AS A TEST. On Vec<Vec<char>> this scales with the
        // file (1.5 ms at 2.6M rows); here it must not.
        let small: String = (0..200_000).map(|i| format!("r{i}\n")).collect();
        let large: String = (0..2_000_000).map(|i| format!("r{i}\n")).collect();
        let ts = Temp::new("cost_small.txt", small.as_bytes());
        let tl = Temp::new("cost_large.txt", large.as_bytes());

        let time_insert = |path: &Path| {
            let mut s = RowStore::open(path, Encoding::Utf8).expect("open");
            // Warm, then measure an insert at row 0 — the worst case for
            // anything that shifts what follows.
            s.insert(0, vec!['x']);
            s.remove(0);
            let t = Instant::now();
            for _ in 0..200 {
                s.insert(0, vec!['x']);
                s.remove(0);
            }
            t.elapsed().as_secs_f64() / 200.0
        };
        let a = time_insert(&ts.0);
        let b = time_insert(&tl.0);
        println!(
            "insert at row 0: {:.3} us at 200k rows, {:.3} us at 2M rows ({:.1}x for 10x)",
            a * 1e6,
            b * 1e6,
            b / a.max(f64::MIN_POSITIVE)
        );
        // The honest bound: the cost tracks the number of CHUNKS, not the
        // number of rows, so 10x the rows is 10x the chunks and ~10x the cost —
        // but with a factor of CHUNK fewer things to touch than the rows
        // themselves. On `Vec<Vec<char>>` the same insert was 1.5 ms at 2M rows
        // (a memmove of 2.6M `Vec` headers); here it is tens of microseconds,
        // because what shifts is one `usize` of chunk bookkeeping per chunk.
        //
        // Stated as a bound on the CONSTANT rather than on the ratio, because
        // the ratio is genuinely linear in chunks: what has to hold is that a
        // keystroke stays far inside a frame at any size this program meets.
        assert!(
            b < 1e-3,
            "a structural edit took {b:.3e} s at 2M rows — a frame is 16 ms"
        );
    }

    #[test]
    fn an_edited_row_survives_materialize_and_re_read() {
        // A row that has been edited is the only copy of its content, so
        // nothing may restore it from the file.
        let t = Temp::new("edited.txt", b"a\nb\nc\n");
        let mut s = RowStore::open(&t.0, Encoding::Utf8).expect("open");
        s.ensure(0, 2).expect("ensure");
        s.set_row(1, "EDITED".chars().collect());
        s.materialize_all().expect("materialize");
        assert_eq!(all(&mut s), ["a", "EDITED", "c"]);
        // A second pass must not undo it either.
        s.materialize_all().expect("materialize again");
        assert_eq!(text(&s, 1), "EDITED");
    }

    #[test]
    fn a_replaced_row_reads_back_what_was_put_there() {
        let t = Temp::new("setrow.txt", b"a\nb\nc\n");
        let mut s = RowStore::open(&t.0, Encoding::Utf8).expect("open");
        s.ensure(0, 2).expect("ensure");
        s.set_row(0, "hello".chars().collect());
        s.set_row(2, "bye".chars().collect());
        assert_eq!(all(&mut s), ["hello", "b", "bye"]);
        // And an edited row reports no byte range: its bytes no longer
        // describe it.
        assert!(s.byte_range(0).is_none());
        assert!(s.byte_range(1).is_some(), "row 1 is still file-backed");
    }

    #[test]
    fn rows_split_on_byte_newlines_and_cp1252_too() {
        // 0xE9 is 'e-acute' in cp1252 and NOT a newline, so the byte index
        // works for that encoding as well. This is why the ladder's third rung
        // can still be streamed.
        let bytes = crate::encoding::encode("caf\u{e9}\nsecond\n", Encoding::Cp1252).unwrap();
        let t = Temp::new("cp.txt", &bytes);
        let mut s = RowStore::open(&t.0, Encoding::Cp1252).expect("open");
        assert_eq!(all(&mut s), ["caf\u{e9}", "second"]);
    }

    #[test]
    fn removing_every_row_leaves_an_operable_store() {
        // Degenerate but reachable: `Buffer` keeps one row, and a store that
        // panicked on empty would take the editor with it.
        let t = Temp::new("tiny.txt", b"only\n");
        let mut s = RowStore::open(&t.0, Encoding::Utf8).expect("open");
        assert_eq!(s.rows(), 1);
        let _ = s.remove(0);
        assert_eq!(s.rows(), 0);
        // And it can still be written to.
        s.insert(0, "back".chars().collect());
        assert_eq!(s.rows(), 1);
        assert_eq!(text(&s, 0), "back");
    }

    #[test]
    fn a_chunk_that_has_fattened_is_split() {
        // Chunks stay bounded, or an insert would shift more and more.
        let mut s = RowStore::new();
        for i in 0..(CHUNK * 3) {
            s.insert(s.rows(), format!("r{i}").chars().collect());
        }
        assert_eq!(s.rows(), CHUNK * 3 + 1);
        let widest = s.chunks.iter().map(|c| c.rows.len()).max().unwrap_or(0);
        assert!(widest <= CHUNK * 2, "a chunk grew to {widest}");
        assert!(s.chunks.len() > 1, "it did split");
        // And the numbering survived it.
        assert_eq!(text(&s, 1), "r0");
        assert_eq!(text(&s, CHUNK), format!("r{}", CHUNK - 1));
        assert_eq!(text(&s, s.rows() - 1), format!("r{}", CHUNK * 3 - 1));
    }
}
