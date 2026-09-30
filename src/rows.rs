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

// ===========================================================================
// Stage B — file-backed rows, decoded on demand
// ===========================================================================
//
// **Nothing constructs a `RowStore` yet.** Stage A (`Rows`, at the bottom of
// this file) took the chunking without the lazy decode, because lazy decode is
// the half that can blank a row: `RowStore::row` returns `&[]` for a row it has
// not decoded, and an empty row reaches `Buffer::text()` — so a missed `ensure`
// writes an empty line to the saved file. It waits for an API that cannot lie
// about a missing row.
//
// The binary declares this module `#[allow(dead_code)]` for exactly that
// reason; the library exports both halves, where nothing here is dead.

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

    /// A store over `path`, **empty and ready to ingest**: the streaming open.
    ///
    /// [`Self::open`] indexes the whole file in one pass, which is O(bytes) at
    /// ~2.2 GB/s — 84 ms for a 193 MB file, but ~470 ms for 1 GB and ~940 ms for
    /// 2 GB. That is a dead screen at the sizes this editor exists for, and it is
    /// what §14.6's scheduled loader avoids. So the two halves are separated:
    /// this opens the file and holds it, and [`Self::ingest`] takes byte ranges as
    /// they are found — which is what the loader is already computing when it
    /// scans for newlines to split rows.
    ///
    /// The file is opened HERE rather than handed in, which is the other half of
    /// why this exists: the loader used to own the `File` and drop it when its
    /// reader thread ended (TODO.md §16.3, option A). Nothing has to hand it back
    /// now — the store opened its own.
    pub fn begin(path: &Path, encoding: Encoding) -> io::Result<Self> {
        let file = std::fs::File::open(path)?;
        Ok(Self {
            file: Some(file),
            path: Some(path.to_path_buf()),
            encoding,
            chunks: Vec::new(),
            base: Vec::new(),
            rows: 0,
            tick: 0,
            budget: DEFAULT_BUDGET,
            resident: 0,
        })
    }

    /// Append rows for `ranges`, in file order, as file-backed rows.
    ///
    /// Nothing is decoded — the ranges are the whole of what this needs, and the
    /// byte lengths they carry are enough for the wrap table to work without
    /// producing a row. That is the point: ingest is O(ranges) and allocates two
    /// `u64` per row, where the loader's decode allocated a `Vec<char>` of four
    /// bytes per character.
    ///
    /// Chunks fill to [`CHUNK`] before a new one starts, so the shape is the same
    /// as [`Self::open`]'s, which is why the two cannot disagree about row
    /// numbering.
    pub fn ingest(&mut self, ranges: &[(u64, u64)]) {
        for &(start, end) in ranges {
            if self.chunks.last().is_none_or(|c| c.rows.len() >= CHUNK) {
                self.base.push(self.rows);
                self.chunks.push(Chunk {
                    rows: Vec::with_capacity(CHUNK),
                    used: 0,
                });
            }
            // `last_mut` cannot be `None`: the branch above guarantees one.
            if let Some(c) = self.chunks.last_mut() {
                c.rows.push(Row::File { start, end });
                self.rows += 1;
            }
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

    /// Row `r`, when it is resident. **Make it resident first** — see `ensure`.
    ///
    /// `None` rather than an empty slice, because an empty slice is not a
    /// smaller answer, it is a wrong one: an un-decoded row that reads as `&[]`
    /// reaches `Buffer::text()` as a blank line and is written to the saved
    /// file, and reaches `begin_action` as a blank row in the undo record. A
    /// missing row is a state a caller can see and handle; a row that silently
    /// reads as empty is data loss waiting for a missed `ensure`.
    ///
    /// Use [`Self::row`] where the row is known to be resident (the frame's
    /// path, after `ensure`) — it panics rather than lying.
    pub fn try_row(&self, r: usize) -> Option<&[char]> {
        match self.row_slot(r) {
            Some(Row::Decoded { chars, .. }) | Some(Row::Edited(chars)) => Some(chars),
            _ => None,
        }
    }

    /// Row `r`. **Panics if it is not resident**, and if it is out of range.
    ///
    /// The previous version returned `&[]` for both, and its comment called that
    /// "visible and harmless (the renderer draws a blank row)". That was wrong,
    /// and worth spelling out because the comment was the whole argument: blank
    /// on screen is the *smallest* part of it. `Buffer::text()` maps every row to
    /// a `String`, so an un-decoded row is a blank line **written to the file**;
    /// `begin_action` captures rows the same way, so it is a blank row **in the
    /// undo record**. Silent, and in the artefact rather than on the screen.
    ///
    /// So the contract is now enforced instead of documented. A missed `ensure`
    /// is a loud panic naming the fix, and the panic is the *good* outcome: the
    /// alternative is a save that quietly loses lines.
    pub fn row(&self, r: usize) -> &[char] {
        if r >= self.rows {
            panic!(
                "row {r} is out of range: the store holds {} rows",
                self.rows
            );
        }
        self.try_row(r).unwrap_or_else(|| {
            panic!(
                "row {r} is not resident: call ensure({r}, {r}) before reading it. \
                 Reading it as empty would write a blank line to the saved file."
            )
        })
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

    /// The rows as a plain `Vec<Vec<char>>`, materialising first.
    ///
    /// `materialize_lines` rather than `to_lines` because it takes `&mut self`:
    /// a `to_*` name promises a cheap conversion, and this one fills the decode
    /// cache on the way. The name is the only thing that says so at a call site.
    pub fn materialize_lines(&mut self) -> io::Result<Vec<Vec<char>>> {
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
        // **Not resident, and it says so rather than reading as empty.** This
        // assertion used to be `assert_eq!(text(&s, 0), "", "not resident")`,
        // which pinned the defect: an un-decoded row that reads as `&[]` is a
        // blank line in the saved file and a blank row in the undo record. The
        // byte lengths are known from the index alone and need no decode.
        assert_eq!(s.try_row(0), None, "not resident yet");
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

    /// **The streaming open must agree with the whole-file one, row for row.**
    ///
    /// `open` indexes the file in one pass; `begin` + `ingest` takes ranges as a
    /// loader finds them. They are two ways to build the same index, so they are
    /// asserted equal on every file shape the suite already cares about —
    /// including the ones with a trailing newline, no trailing newline, and
    /// multibyte characters, which is where a byte range can be off by one.
    #[test]
    fn ingesting_ranges_equals_indexing_the_whole_file() {
        // **Not the empty file**: it is the one shape where the two paths
        // deliberately differ, and `an_empty_file_gets_its_row_from_the_buffer`
        // below pins where that row comes from. Every file WITH content must
        // agree exactly.
        for (name, body) in [
            ("s_basic.txt", b"one\ntwo\nthree\n".to_vec()),
            ("s_notrail.txt", b"a\nb".to_vec()),
            ("s_onlynl.txt", b"\n".to_vec()),
            ("s_blank.txt", b"\n\n\n".to_vec()),
            (
                "s_long.txt",
                format!("{}\nshort\n", "x".repeat(5_000)).into_bytes(),
            ),
            (
                "s_multibyte.txt",
                "caf\u{e9} \u{4e2d}\u{6587}\n\u{1f600}tail\n"
                    .as_bytes()
                    .to_vec(),
            ),
        ] {
            let t = Temp::new(name, &body);
            // The whole-file index.
            let mut whole = RowStore::open(&t.0, Encoding::Utf8).expect("open");
            // The same index, from ranges — computed here the way the loader
            // computes them: scan the bytes, and every newline ends a row.
            let mut ranges: Vec<(u64, u64)> = Vec::new();
            let mut start = 0u64;
            for (i, b) in body.iter().enumerate() {
                if *b == b'\n' {
                    let mut end = i as u64;
                    // `\r\n`: the CR is not content, which is what the loader
                    // does too.
                    if end > start && body[end as usize - 1] == b'\r' {
                        end -= 1;
                    }
                    ranges.push((start, end));
                    start = i as u64 + 1;
                }
            }
            if start < body.len() as u64 {
                ranges.push((start, body.len() as u64));
            }
            let mut streamed = RowStore::begin(&t.0, Encoding::Utf8).expect("begin");
            streamed.ingest(&ranges);

            assert_eq!(
                streamed.rows(),
                whole.rows(),
                "{name}: {} rows streamed vs {} indexed",
                streamed.rows(),
                whole.rows()
            );
            // And the content agrees, read row by row through the same API a
            // caller uses.
            whole.materialize_all().expect("materialize");
            streamed.materialize_all().expect("materialize");
            for r in 0..whole.rows() {
                assert_eq!(
                    text(&streamed, r),
                    text(&whole, r),
                    "{name}: row {r} differs"
                );
            }
        }
    }

    /// **The one-row invariant belongs to the BUFFER, not to the store.**
    ///
    /// Found by this file's own equality test disagreeing on the empty file:
    /// `open` yields 1 row (it is constructing a whole document, so it applies
    /// the invariant) while `begin` + no ranges yields 0 — and the store cannot
    /// know that no more ranges are coming, so it must not invent a row.
    ///
    /// The consumer applies it, and already did before this existed:
    /// `load_ctrl`'s `Adopted::Finished` arm is
    /// `if bs.buf.rows_is_empty() { bs.buf.push_row(Vec::new()) }`.
    ///
    /// So the seam is stated rather than papered over: a begun-but-unfed or
    /// genuinely empty file is 0 rows from the store, and 1 row from the buffer.
    #[test]
    fn an_empty_file_gets_its_row_from_the_buffer_not_the_store() {
        let t = Temp::new("s_empty2.txt", b"");
        let indexed = RowStore::open(&t.0, Encoding::Utf8).expect("open");
        assert_eq!(
            indexed.rows(),
            1,
            "`open` builds a whole document, so it applies the one-row invariant"
        );
        let streamed = RowStore::begin(&t.0, Encoding::Utf8).expect("begin");
        assert_eq!(
            streamed.rows(),
            0,
            "the store cannot know more ranges are not coming, so it must not \
             invent a row — the buffer's `Adopted::Finished` arm adds it"
        );

        // And a store that ingests an empty range list stays at zero rather than
        // growing a phantom row.
        let mut fed = RowStore::begin(&t.0, Encoding::Utf8).expect("begin");
        fed.ingest(&[]);
        assert_eq!(fed.rows(), 0, "no ranges, no rows");
    }

    /// Batching must not matter: the loader hands over ranges in `BATCH`-sized
    /// pieces, and the index cannot depend on where the pieces fell.
    #[test]
    fn the_batching_of_ranges_does_not_matter() {
        let body: Vec<u8> = (0..3_000)
            .map(|i| format!("row {i}\n"))
            .collect::<String>()
            .into_bytes();
        let t = Temp::new("s_batch.txt", &body);
        let ranges: Vec<(u64, u64)> = {
            let mut v = Vec::new();
            let mut start = 0u64;
            for (i, b) in body.iter().enumerate() {
                if *b == b'\n' {
                    v.push((start, i as u64));
                    start = i as u64 + 1;
                }
            }
            v
        };
        // All at once.
        let mut one = RowStore::begin(&t.0, Encoding::Utf8).expect("begin");
        one.ingest(&ranges);
        // In ragged pieces, as a loader would.
        let mut many = RowStore::begin(&t.0, Encoding::Utf8).expect("begin");
        for piece in ranges.chunks(7) {
            many.ingest(piece);
        }
        assert_eq!(one.rows(), ranges.len());
        assert_eq!(many.rows(), one.rows(), "batching changed the row count");

        // And an ingest that straddles a CHUNK boundary still numbers right.
        assert!(ranges.len() > CHUNK, "the fixture must span chunks");
        one.materialize_all().expect("materialize");
        many.materialize_all().expect("materialize");
        for r in [0, CHUNK - 1, CHUNK, CHUNK + 1, ranges.len() - 1] {
            assert_eq!(text(&many, r), text(&one, r), "row {r}");
        }
    }

    /// A store that has been begun but not fed is empty and operable — the state
    /// the first frame of a load is drawn from.
    #[test]
    fn a_begun_but_unfed_store_is_an_empty_frame_not_a_panic() {
        let t = Temp::new("s_unfed.txt", b"a\nb\n");
        let s = RowStore::begin(&t.0, Encoding::Utf8).expect("begin");
        assert_eq!(s.rows(), 0, "no rows until ranges arrive");
        assert!(s.try_row(0).is_none());
        // And reading past it is the "out of range" panic, not the "not
        // resident" one — a caller debugging an empty first frame should be told
        // it has no rows, not that it forgot an ensure.
        assert_eq!(s.rows(), 0);
    }

    /// **A row that is not resident must not read as empty, and must not read as
    /// wrong either.** The old contract returned `&[]`, and that empty row does
    /// not stay on screen — `Buffer::text()` writes it to the file and
    /// `begin_action` records it in the undo step.
    ///
    /// So it panics, naming the fix. A missed `ensure` is a panic for a
    /// developer; the alternative is a save that quietly loses lines for a user.
    #[test]
    #[should_panic(expected = "is not resident")]
    fn reading_an_unresident_row_panics_rather_than_lying() {
        let t = Temp::new("unres.txt", b"one\ntwo\n");
        let s = RowStore::open(&t.0, Encoding::Utf8).expect("open");
        // No `ensure`: this must not hand back an empty slice.
        let _ = s.row(0);
    }

    /// The same rule for a row that does not exist at all, and the message says
    /// which of the two it is — "not resident" and "out of range" want
    /// different fixes, and a caller debugging one should not be told the other.
    #[test]
    #[should_panic(expected = "out of range")]
    fn reading_a_missing_row_panics_with_the_other_message() {
        let s = RowStore::new();
        let _ = s.row(7);
    }

    /// `ensure` then `row` is the working path, so the panic above is only ever
    /// reached by a mistake.
    #[test]
    fn ensure_then_read_is_the_working_path() {
        let t = Temp::new("ens.txt", b"one\ntwo\n");
        let mut s = RowStore::open(&t.0, Encoding::Utf8).expect("open");
        assert!(s.try_row(0).is_none());
        s.ensure(0, 1).expect("ensure");
        assert_eq!(text(&s, 0), "one");
        assert_eq!(text(&s, 1), "two");
        assert!(s.try_row(0).is_some());
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

// ---------------------------------------------------------------------------
// Rows: stage A — chunked, and eager
// ---------------------------------------------------------------------------
//
// The same chunking as `RowStore`, without the lazy half. Every row is
// resident, so nothing above needs an `ensure` step and **no row can be
// silently blank**: `row(r)` is a plain borrow of real content, exactly as
// `Vec<Vec<char>>` was.
//
// That is the whole point of doing this in two stages. `RowStore::row` returns
// `&[]` for a row it has not decoded, and an empty row is not a local
// inconvenience — `Buffer::text()` would write it to the file as a blank line
// and `begin_action` would record it as a blank row in the undo step. Lazy
// decode is worth having (835 MB → ~20 MB on a 184 MB file) and it is the half
// that can lose data, so it goes second, on its own, behind an API that cannot
// lie about a missing row.
//
// What this stage buys, measured (`bench_edit_scaling`): inserting a row costs
// `O(CHUNK)` — the rows of one chunk — instead of `O(rows)`, a memmove of every
// row header below the insert. 1.3 ms per line insert at 2.6M rows is the
// number that goes away.
//
// # The index
//
// `base[i]` is the first row index of chunk `i`, and chunks hold between 1 and
// `2 * CHUNK` rows. Lookup is a binary search over `base` — O(log chunks), which
// at 2.6M rows is 12 steps — rather than `r / CHUNK`, because a chunk that has
// been inserted into is not the same size as its neighbours.

use std::ops::{Index, IndexMut, RangeBounds};

/// Rows in chunks, every row resident.
#[derive(Debug, Clone, Default)]
pub struct Rows {
    chunks: Vec<Vec<Vec<char>>>,
    /// First row index of each chunk. Parallel to `chunks`.
    base: Vec<usize>,
    len: usize,
}

impl Rows {
    pub fn new() -> Self {
        Self::default()
    }

    /// Take ownership of a flat `Vec<Vec<char>>`, chunking it on the way in.
    pub fn from_vec(rows: Vec<Vec<char>>) -> Self {
        let mut out = Self::new();
        // Filled by chunks directly rather than row by row: a file's worth of
        // rows arriving at once is the common construction, and pushing one at
        // a time would do the bookkeeping n times for no reason.
        let mut it = rows.into_iter().peekable();
        while it.peek().is_some() {
            let chunk: Vec<Vec<char>> = it.by_ref().take(CHUNK).collect();
            out.base.push(out.len);
            out.len += chunk.len();
            out.chunks.push(chunk);
        }
        out
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// `(chunk, offset within it)` of row `r`. Must not be called when empty.
    fn locate(&self, r: usize) -> (usize, usize) {
        let c = self.base.partition_point(|&b| b <= r).saturating_sub(1);
        (c, r - self.base[c])
    }

    pub fn get(&self, r: usize) -> Option<&Vec<char>> {
        if r >= self.len {
            return None;
        }
        let (c, i) = self.locate(r);
        self.chunks[c].get(i)
    }

    pub fn get_mut(&mut self, r: usize) -> Option<&mut Vec<char>> {
        if r >= self.len {
            return None;
        }
        let (c, i) = self.locate(r);
        self.chunks[c].get_mut(i)
    }

    pub fn last(&self) -> Option<&Vec<char>> {
        self.get(self.len.checked_sub(1)?)
    }

    pub fn iter(&self) -> impl Iterator<Item = &Vec<char>> {
        self.chunks.iter().flatten()
    }

    /// A copy of rows `[a, b)`.
    ///
    /// This is what a `&[Vec<char>]` slice used to give a caller that wanted a
    /// range of rows, and it is deliberately a copy: the rows are in chunks, so
    /// there is no contiguous range to lend out. Every caller of the old
    /// `lines_slice()` was O(document) already, so the copy is the same order as
    /// what it replaced.
    pub fn range(&self, a: usize, b: usize) -> Vec<Vec<char>> {
        let b = b.min(self.len);
        if a >= b {
            return Vec::new();
        }
        // Walk by chunk rather than by row: a range of a big file is much
        // cheaper to clone in chunk-sized pieces than one `get` per row.
        let mut out = Vec::with_capacity(b - a);
        let (c0, i0) = self.locate(a);
        let (c1, i1) = self.locate(b - 1);
        if c0 == c1 {
            out.extend(self.chunks[c0][i0..=i1].iter().cloned());
            return out;
        }
        out.extend(self.chunks[c0][i0..].iter().cloned());
        for c in (c0 + 1)..c1 {
            out.extend(self.chunks[c].iter().cloned());
        }
        out.extend(self.chunks[c1][..=i1].iter().cloned());
        out
    }

    /// Everything, as a flat Vec. The escape hatch the O(document) callers use.
    pub fn to_vec(&self) -> Vec<Vec<char>> {
        self.iter().cloned().collect()
    }

    /// Add `delta` to the base of every chunk after `c`.
    fn shift(&mut self, c: usize, delta: isize) {
        for b in self.base.iter_mut().skip(c + 1) {
            *b = (*b as isize + delta) as usize;
        }
    }

    /// Drop empty chunks and recompute `base`. Used after a range removal.
    fn compact(&mut self) {
        let mut base = Vec::with_capacity(self.chunks.len());
        let mut at = 0usize;
        let mut kept: Vec<Vec<Vec<char>>> = Vec::with_capacity(self.chunks.len());
        for chunk in self.chunks.drain(..) {
            if chunk.is_empty() {
                continue;
            }
            base.push(at);
            at += chunk.len();
            kept.push(chunk);
        }
        self.chunks = kept;
        self.base = base;
        self.len = at;
    }

    pub fn push(&mut self, row: Vec<char>) {
        if self.chunks.is_empty() {
            self.chunks.push(Vec::new());
            self.base.push(0);
        }
        let c = self.chunks.len() - 1;
        self.chunks[c].push(row);
        self.len += 1;
    }

    pub fn insert(&mut self, at: usize, row: Vec<char>) {
        let at = at.min(self.len);
        if self.chunks.is_empty() || at == self.len {
            self.push(row);
            return;
        }
        let (c, i) = self.locate(at);
        self.chunks[c].insert(i, row);
        self.len += 1;
        self.shift(c, 1);
        if self.chunks[c].len() > 2 * CHUNK {
            self.split(c);
        }
    }

    /// Halve a chunk that has grown past `2 * CHUNK`.
    fn split(&mut self, c: usize) {
        let half = self.chunks[c].len() / 2;
        let tail = self.chunks[c].split_off(half);
        let base = self.base[c] + half;
        self.chunks.insert(c + 1, tail);
        self.base.insert(c + 1, base);
    }

    pub fn remove(&mut self, r: usize) -> Vec<char> {
        if r >= self.len {
            return Vec::new();
        }
        let (c, i) = self.locate(r);
        let out = self.chunks[c].remove(i);
        self.len -= 1;
        self.shift(c, -1);
        if self.chunks[c].is_empty() && self.chunks.len() > 1 {
            self.chunks.remove(c);
            self.base.remove(c);
        }
        out
    }

    pub fn clear(&mut self) {
        self.chunks.clear();
        self.base.clear();
        self.len = 0;
    }

    pub fn extend(&mut self, rows: impl IntoIterator<Item = Vec<char>>) {
        for row in rows {
            self.push(row);
        }
    }

    /// Take everything out of `rows`, leaving it empty — the loader's handoff.
    pub fn append(&mut self, rows: &mut Vec<Vec<char>>) {
        for row in rows.drain(..) {
            self.push(row);
        }
    }

    /// Remove rows `[start, end)` and return them.
    ///
    /// Chunk-aware: the tail of the first chunk, whole chunks in between, and
    /// the head of the last. Doing this one `remove` per row would be
    /// `O(rows × chunks)`, and `sort_lines`/`justify` drain real ranges.
    pub fn drain(&mut self, start: usize, end: usize) -> Vec<Vec<char>> {
        let end = end.min(self.len);
        if start >= end {
            return Vec::new();
        }
        let (c0, i0) = self.locate(start);
        let (c1, i1) = self.locate(end - 1);
        let mut out = Vec::with_capacity(end - start);
        if c0 == c1 {
            out.extend(self.chunks[c0].drain(i0..=i1));
        } else {
            out.extend(self.chunks[c0].drain(i0..));
            for c in (c0 + 1)..c1 {
                out.extend(std::mem::take(&mut self.chunks[c]));
            }
            out.extend(self.chunks[c1].drain(..=i1));
        }
        self.compact();
        out
    }

    /// Replace rows `[start, end)` with `items`.
    ///
    /// Takes explicit bounds rather than a `RangeBounds` because callers use
    /// both `a..b` and `a..=b`, and converting the second to the first at the
    /// call site is one character where guessing inside would be a bug.
    pub fn splice(&mut self, start: usize, end: usize, items: impl IntoIterator<Item = Vec<char>>) {
        let end = end.min(self.len);
        let start = start.min(end);
        let _ = self.drain(start, end);
        // Rows are inserted one at a time, which is what `insert_lines_at`
        // already does for a paste. Each insert costs O(CHUNK) for the chunk it
        // lands in plus O(chunks) for the base shift, not O(rows) — so a splice
        // of a few hundred rows into a 2.6M-row file is well under a millisecond.
        for (k, row) in items.into_iter().enumerate() {
            self.insert(start + k, row);
        }
    }

    /// Sort rows `[a, b)` in place by `f`.
    ///
    /// The replacement for handing out `&mut [Vec<char>]` to `sort_by_key`: the
    /// rows are copied out, sorted, and copied back. `sort_lines` is a
    /// whole-region operation already, so the copy is the same order as the sort
    /// it replaces.
    pub fn sort_range_by<F>(&mut self, a: usize, b: usize, f: F)
    where
        F: FnMut(&Vec<char>, &Vec<char>) -> std::cmp::Ordering,
    {
        let b = b.min(self.len);
        if a >= b {
            return;
        }
        let mut rows = self.range(a, b);
        rows.sort_by(f);
        for (k, row) in rows.into_iter().enumerate() {
            if let Some(slot) = self.get_mut(a + k) {
                *slot = row;
            }
        }
    }
}

impl Index<usize> for Rows {
    type Output = Vec<char>;
    fn index(&self, r: usize) -> &Vec<char> {
        let (c, i) = self.locate(r);
        &self.chunks[c][i]
    }
}

impl IndexMut<usize> for Rows {
    fn index_mut(&mut self, r: usize) -> &mut Vec<char> {
        let (c, i) = self.locate(r);
        &mut self.chunks[c][i]
    }
}

/// A `RangeBounds`-shaped convenience for callers that have one.
pub fn bounds_of<R: RangeBounds<usize>>(range: &R, len: usize) -> (usize, usize) {
    use std::ops::Bound;
    let start = match range.start_bound() {
        Bound::Included(&s) => s,
        Bound::Excluded(&s) => s + 1,
        Bound::Unbounded => 0,
    };
    let end = match range.end_bound() {
        Bound::Included(&e) => e + 1,
        Bound::Excluded(&e) => e,
        Bound::Unbounded => len,
    };
    (start, end)
}

#[cfg(test)]
mod rows_tests {
    use super::*;

    /// The rows as plain strings, for comparing against the model.
    fn shape(r: &Rows) -> Vec<String> {
        r.iter().map(|row| row.iter().collect()).collect()
    }

    fn flat(v: &[Vec<char>]) -> Vec<String> {
        v.iter().map(|row| row.iter().collect()).collect()
    }

    /// A deterministic xorshift, so a failure is reproducible.
    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            let mut x = self.0;
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            self.0 = x;
            x
        }
        fn below(&mut self, n: usize) -> usize {
            if n == 0 {
                0
            } else {
                (self.next() % n as u64) as usize
            }
        }
    }

    #[test]
    fn an_empty_store_is_operable() {
        let mut r = Rows::new();
        assert_eq!(r.len(), 0);
        assert!(r.is_empty());
        assert!(r.get(0).is_none());
        assert!(r.last().is_none());
        assert!(r.range(0, 10).is_empty());
        assert!(r.drain(0, 5).is_empty());
        // And the first push creates the first chunk.
        r.push("a".chars().collect());
        assert_eq!(shape(&r), ["a"]);
        // Removing the only row leaves it usable, not broken.
        assert_eq!(r.remove(0).iter().collect::<String>(), "a");
        assert!(r.is_empty());
        r.push("b".chars().collect());
        assert_eq!(shape(&r), ["b"]);
    }

    /// `from_vec` and `to_vec` are inverses, and the chunking is invisible.
    #[test]
    fn from_vec_and_to_vec_round_trip_across_many_chunks() {
        let rows: Vec<Vec<char>> = (0..5_000)
            .map(|i| format!("row {i}").chars().collect())
            .collect();
        let r = Rows::from_vec(rows.clone());
        assert_eq!(r.len(), rows.len());
        assert_eq!(flat(&r.to_vec()), flat(&rows));
        // And random access lands on the right row, which is what the binary
        // search over `base` is for.
        for i in [0, 1, 1023, 1024, 1025, 2047, 2048, 4999] {
            assert_eq!(
                r.get(i).map(|c| c.iter().collect::<String>()),
                Some(format!("row {i}"))
            );
            assert_eq!(r[i].iter().collect::<String>(), format!("row {i}"));
        }
    }

    /// **The property that matters: `Rows` behaves exactly like the
    /// `Vec<Vec<char>>` it replaces.**
    ///
    /// Driven with pseudo-random operations rather than a fixed script, because
    /// the bugs in a chunked index live at the boundaries — an insert that lands
    /// on a chunk edge, a drain that starts in one chunk and ends in another —
    /// and a hand-written script tests the boundaries its author thought of.
    /// Both are run side by side and compared after every step.
    #[test]
    fn it_behaves_like_the_vec_it_replaces() {
        let mut rng = Rng(0x1234_5678_9abc_def0);
        let mut rows = Rows::new();
        let mut model: Vec<Vec<char>> = Vec::new();
        // Larger than a chunk, so the operations cross boundaries constantly.
        for i in 0..3 * CHUNK {
            let row: Vec<char> = format!("r{i}").chars().collect();
            rows.push(row.clone());
            model.push(row);
        }
        assert_eq!(shape(&rows), flat(&model), "after seeding");

        for step in 0..2_000 {
            let n = model.len();
            match rng.below(6) {
                // push
                0 => {
                    let row: Vec<char> = format!("p{step}").chars().collect();
                    rows.push(row.clone());
                    model.push(row);
                }
                // insert, biased to chunk edges
                1 => {
                    let at = if rng.below(2) == 0 {
                        rng.below(n + 1)
                    } else {
                        // exactly on a multiple of CHUNK, or one either side
                        (rng.below(n / CHUNK.max(1) + 1) * CHUNK)
                            .saturating_sub(rng.below(2))
                            .min(n)
                    };
                    let row: Vec<char> = format!("i{step}").chars().collect();
                    rows.insert(at, row.clone());
                    model.insert(at, row);
                }
                // remove
                2 if n > 0 => {
                    let at = rng.below(n);
                    assert_eq!(rows.remove(at), model.remove(at), "remove at {at}");
                }
                // set through IndexMut
                3 if n > 0 => {
                    let at = rng.below(n);
                    let row: Vec<char> = format!("s{step}").chars().collect();
                    rows[at] = row.clone();
                    model[at] = row;
                }
                // drain a random range, bounded the way real ones are: a cut
                // line, a paragraph, a filter region — not a third of the file.
                // Unbounded drains shrunk the model until it no longer spanned a
                // chunk, which is the state the boundaries live in.
                4 if n > 1 => {
                    let a = rng.below(n - 1);
                    let b = (a + 1 + rng.below(64)).min(n);
                    let got = rows.drain(a, b);
                    let want: Vec<Vec<char>> = model.drain(a..b).collect();
                    assert_eq!(flat(&got), flat(&want), "drain {a}..{b}");
                }
                // splice a random block in
                5 if n > 1 => {
                    let a = rng.below(n - 1);
                    let b = (a + 1 + rng.below(64)).min(n);
                    let items: Vec<Vec<char>> = (0..rng.below(5))
                        .map(|k| format!("x{step}.{k}").chars().collect())
                        .collect();
                    rows.splice(a, b, items.clone());
                    model.splice(a..b, items);
                }
                _ => {}
            }
            assert_eq!(rows.len(), model.len(), "length after step {step}");
            assert_eq!(shape(&rows), flat(&model), "content after step {step}");

            // Top the document back up when the removal-heavy mix has eaten it,
            // so every step runs against a store that spans several chunks.
            // Without this the drains win and the model collapses to a single
            // row, where no boundary is ever exercised — the first version of
            // this test did exactly that and its per-step comparison passed
            // 2,000 times on a one-row document.
            while model.len() < 2 * CHUNK {
                let row: Vec<char> = format!("t{step}.{}", model.len()).chars().collect();
                rows.push(row.clone());
                model.push(row);
            }
        }
        // The model never shrank below a chunk, so every step above ran against
        // a store whose index had to cross chunk boundaries.
        assert!(
            model.len() > CHUNK,
            "the run must still span chunks, ended at {} rows",
            model.len()
        );
    }

    /// `range` and `sort_range_by` are the two replacements for handing out a
    /// mutable slice, and both must be exact.
    #[test]
    fn range_and_sort_match_the_model() {
        let rows: Vec<Vec<char>> = (0..3 * CHUNK)
            .map(|i| format!("row {i}").chars().collect())
            .collect();
        let mut r = Rows::from_vec(rows.clone());
        // `range` across a chunk boundary.
        assert_eq!(
            flat(&r.range(CHUNK - 2, CHUNK + 2)),
            flat(&rows[CHUNK - 2..CHUNK + 2])
        );
        assert_eq!(flat(&r.range(0, 3)), flat(&rows[0..3]));
        assert!(r.range(5, 5).is_empty());
        assert!(r.range(9, 3).is_empty());
        // Out of range clamps rather than panicking.
        assert_eq!(flat(&r.range(rows.len() - 1, rows.len() + 99)).len(), 1);

        // Sort a middle region, exactly as `sort_lines` does.
        let (a, b) = (CHUNK - 3, CHUNK + 3);
        let mut want = rows.clone();
        want[a..b].sort_by(|x, y| {
            x.iter()
                .collect::<String>()
                .cmp(&y.iter().collect::<String>())
        });
        r.sort_range_by(a, b, |x, y| {
            x.iter()
                .collect::<String>()
                .cmp(&y.iter().collect::<String>())
        });
        assert_eq!(flat(&r.to_vec()), flat(&want), "sorted region");
        // And nothing outside the region moved.
        assert_eq!(flat(&r.range(0, a)), flat(&want[0..a]));
        assert_eq!(flat(&r.range(b, rows.len())), flat(&want[b..rows.len()]));
    }

    /// Splitting is the one place a chunk stops being its neighbours' size, so
    /// the index has to survive it — and so does `locate`, which is a binary
    /// search over `base` rather than `r / CHUNK` for exactly this reason.
    #[test]
    fn the_index_survives_a_split() {
        let mut r = Rows::from_vec(
            (0..CHUNK)
                .map(|i| format!("a{i}").chars().collect())
                .collect(),
        );
        assert_eq!(r.chunks.len(), 1);
        // Filling past 2 * CHUNK forces a split.
        for i in 0..(2 * CHUNK + 8) {
            r.insert(0, format!("b{i}").chars().collect());
        }
        assert!(r.chunks.len() > 1, "the chunk should have split");
        // Every row still reads back what was put there, which is the property
        // `locate` has to preserve across a split.
        assert_eq!(
            r[0].iter().collect::<String>(),
            format!("b{}", 2 * CHUNK + 7)
        );
        assert_eq!(r.len(), CHUNK + 2 * CHUNK + 8);
        for i in 0..r.len() {
            assert!(!r[i].is_empty(), "row {i} is empty");
        }
    }

    /// Appending a loader's batch, which is the one operation the load path
    /// does per batch and so the one that has to be cheap and exact.
    #[test]
    fn append_takes_the_batch_and_leaves_it_empty() {
        let mut r = Rows::new();
        for batch in 0..3 {
            let mut rows: Vec<Vec<char>> = (0..CHUNK + 7)
                .map(|i| format!("b{batch}.{i}").chars().collect())
                .collect();
            r.append(&mut rows);
            assert!(rows.is_empty(), "the batch is handed over, not copied");
            assert_eq!(r.len(), (batch + 1) * (CHUNK + 7));
        }
        assert_eq!(r[0].iter().collect::<String>(), "b0.0");
        assert_eq!(
            r[r.len() - 1].iter().collect::<String>(),
            format!("b2.{}", CHUNK + 6)
        );
    }

    /// `clear`, `take` and `set` — the wholesale operations, where a stale index
    /// would show up as a row from the previous document.
    #[test]
    fn wholesale_replacement_leaves_no_trace() {
        let mut r = Rows::from_vec(
            (0..CHUNK + 10)
                .map(|i| format!("old{i}").chars().collect())
                .collect(),
        );
        let taken: Rows = std::mem::take(&mut r);
        assert!(r.is_empty());
        assert_eq!(taken.len(), CHUNK + 10);
        assert_eq!(r.get(0), None, "a taken store has no rows");

        let mut r2 = Rows::from_vec(
            (0..5)
                .map(|i| format!("new{i}").chars().collect())
                .collect(),
        );
        r2.push("last".chars().collect());
        assert_eq!(shape(&r2), ["new0", "new1", "new2", "new3", "new4", "last"]);
        r.clear();
        assert!(r.is_empty());
        r.extend((0..3).map(|i| format!("e{i}").chars().collect()));
        assert_eq!(shape(&r), ["e0", "e1", "e2"]);
    }
}
