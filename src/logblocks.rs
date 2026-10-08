//! Block boundaries for a tailed log, from one rule and no formats.
//!
//! A log that is being tailed but not rendered must cost almost nothing
//! (TODO.md §20.4): no decoded rows, no wrap geometry, no styles — line
//! bookkeeping and **block heuristics**. And the heuristics are ONE RULE:
//!
//! > A row that starts with whitespace continues the block above it;
//! > anything else starts a new block.
//!
//! That single rule already covers the two shapes that matter (§20.5): a stack
//! trace's frames are indented, so they group under the line that raised them,
//! and a log's records are not, so each stands alone. It is deliberately not a
//! format registry — no `at `, no `File "`, no per-logger knowledge — because a
//! rule per format would be a rule per file, and the first one written from a
//! sample of one is wrong for the second.
//!
//! The cost is the budget's: [`Blocks::push`] looks at the first `char` of the
//! arriving row and retains nothing per row but the boundary. Boundaries are
//! computed as rows arrive, so a half-written trace already has its structure
//! — the block is open, and unsettled, until a row that is not indented
//! closes it.

/// Row → block boundaries under the one rule.
///
/// Append-only: rows are seen once, in arrival order, through
/// [`push`](Blocks::push), which is what a tail does. Nothing of a row is
/// kept — not its text, not its depth, not its kind — only the row index of
/// each block start and how many rows have been seen.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Blocks {
    /// Row index of the first row of each block, ascending. `starts[0]` is 0
    /// whenever any row has been seen, because the first row has nothing above
    /// it to continue.
    starts: Vec<usize>,
    /// Rows seen so far.
    len: usize,
}

impl Blocks {
    /// No rows, no blocks.
    pub fn new() -> Self {
        Self {
            starts: Vec::new(),
            len: 0,
        }
    }

    /// The blocks of `rows` — the same answer as pushing them one by one.
    pub fn of(rows: &[Vec<char>]) -> Self {
        let mut blocks = Self::new();
        for row in rows {
            blocks.push(row);
        }
        blocks
    }

    /// See one more row, in arrival order, and record a boundary if it starts
    /// a block.
    ///
    /// The first row always starts a block (there is nothing above it to
    /// continue). So does any row whose first `char` is not whitespace; a row
    /// that *does* start with whitespace continues the block above it. An
    /// empty row starts a block — it does not start with whitespace.
    pub fn push(&mut self, row: &[char]) {
        let continues = self.len > 0 && row.first().is_some_and(|c| c.is_whitespace());
        if !continues {
            self.starts.push(self.len);
        }
        self.len += 1;
    }

    /// Rows seen so far.
    pub fn len(&self) -> usize {
        self.len
    }

    /// Whether no rows have been seen.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Blocks so far.
    pub fn count(&self) -> usize {
        self.starts.len()
    }

    /// The row index at which each block starts, ascending, starting at 0.
    pub fn starts(&self) -> &[usize] {
        &self.starts
    }

    /// The block holding `row`: `(start, end)`, `end` exclusive. `None` for a
    /// row not yet seen — including one past the last, for which the block is
    /// still open.
    pub fn block_of(&self, row: usize) -> Option<(usize, usize)> {
        if row >= self.len {
            return None;
        }
        // The block of `row` starts at the greatest block start at or below it.
        let i = self.starts.partition_point(|&start| start <= row);
        let end = self.starts.get(i).copied().unwrap_or(self.len);
        Some((self.starts[i - 1], end))
    }

    /// Every block as `(start, end)`, `end` exclusive, in row order.
    pub fn blocks(&self) -> impl Iterator<Item = (usize, usize)> + '_ {
        let len = self.len;
        let starts = &self.starts;
        starts
            .iter()
            .enumerate()
            .map(move |(i, &start)| (start, starts.get(i + 1).copied().unwrap_or(len)))
    }
}

#[cfg(test)]
mod tests {
    use super::Blocks;

    /// `&str` as a row, so a test reads like the rows it describes.
    fn row(s: &str) -> Vec<char> {
        s.chars().collect()
    }

    fn rows(spec: &[&str]) -> Vec<Vec<char>> {
        spec.iter().map(|s| row(s)).collect()
    }

    /// One shape the rule is for (§20.4): unindented log records — llama.cpp's
    /// `<dotted-time> <I|W|E> <component>: <message>` — each stand alone.
    #[test]
    fn log_records_are_one_block_each() {
        let rows = rows(&[
            "12:34:56.789 I llama_model_load: loading model from models/7b.gguf",
            "12:34:56.790 I llama_model_load: vocab size 32000",
            "12:34:56.801 W llama_model_load: embedding is unmapped",
            "12:34:56.915 E main: failed to load model",
            "12:34:57.001 I main: farewell",
        ]);
        let blocks = Blocks::of(&rows);
        assert_eq!(blocks.len(), 5);
        assert_eq!(
            blocks.count(),
            5,
            "no record is indented, so none continues"
        );
        assert_eq!(blocks.starts(), &[0, 1, 2, 3, 4]);
        for r in 0..rows.len() {
            assert_eq!(blocks.block_of(r), Some((r, r + 1)), "row {r}");
        }
    }

    /// The other shape the rule is for (§20.5): an unindented `Error:` line
    /// with indented frames under it is ONE block.
    #[test]
    fn a_stack_trace_is_one_block() {
        let rows = rows(&[
            "Error: failed to open 'models/7b.gguf'",
            "   0: rano::loader::open at src/loader.rs:212",
            "   1: rano::editor::open at src/editor.rs:88",
            "   2: rano::main at src/main.rs:14",
        ]);
        let blocks = Blocks::of(&rows);
        assert_eq!(blocks.count(), 1);
        assert_eq!(blocks.starts(), &[0]);
        assert_eq!(blocks.block_of(0), Some((0, 4)));
        assert_eq!(
            blocks.block_of(3),
            Some((0, 4)),
            "the last frame is in the same block"
        );
        assert_eq!(blocks.blocks().collect::<Vec<_>>(), vec![(0, 4)]);
    }

    /// A blank row does not start with whitespace, so it STARTS a block: it
    /// closes the trace above it and is itself a single-row block between the
    /// two traces. The two traces are two blocks — not one block reaching
    /// across the blank.
    #[test]
    fn a_blank_row_ends_the_block() {
        let rows = rows(&[
            "Error: boom",
            "   0: frame one",
            "   1: frame two",
            "",
            "Error2: boom again",
            "   0: frame one",
            "   1: frame two",
        ]);
        let blocks = Blocks::of(&rows);
        assert_eq!(
            blocks.blocks().collect::<Vec<_>>(),
            vec![(0, 3), (3, 4), (4, 7)]
        );
        assert_eq!(blocks.count(), 3);
        assert_eq!(
            blocks.block_of(2),
            Some((0, 3)),
            "the blank closes the trace"
        );
        assert_eq!(
            blocks.block_of(4),
            Some((4, 7)),
            "the second trace starts anew"
        );
        assert_eq!(blocks.block_of(5), Some((4, 7)));
        // the two multi-row blocks are the two traces; the blank is the third
        assert_eq!(blocks.blocks().filter(|&(s, e)| e - s > 1).count(), 2);
    }

    /// "Whitespace" is `char::is_whitespace` on the first `char`: a tab, a
    /// space and U+00A0 all continue; a printable non-space and an empty row
    /// do not.
    #[test]
    fn what_counts_as_whitespace() {
        assert!('\t'.is_whitespace());
        assert!(' '.is_whitespace());
        assert!('\u{a0}'.is_whitespace(), "U+00A0 NBSP is White_Space");
        assert!(!'x'.is_whitespace());

        let rows = rows(&[
            "head",
            "\ta tab continues",
            "  a space continues",
            "\u{a0}an nbsp continues",
            "x does not",
            "",
            " a space after a blank continues the blank's block",
        ]);
        let blocks = Blocks::of(&rows);
        assert_eq!(blocks.starts(), &[0, 4, 5]);
        assert_eq!(
            blocks.blocks().collect::<Vec<_>>(),
            vec![(0, 4), (4, 5), (5, 7)]
        );
    }

    /// A row that is only whitespace still starts with whitespace, so it
    /// continues the block above it — the rule looks at the first `char`,
    /// not the row's content.
    #[test]
    fn a_row_of_only_whitespace_continues() {
        let rows = rows(&["head", "   ", "\t"]);
        let blocks = Blocks::of(&rows);
        assert_eq!(blocks.count(), 1);
        assert_eq!(blocks.block_of(2), Some((0, 3)));
    }

    /// The first row of a file always starts a block, indented or not — there
    /// is nothing above it to continue.
    #[test]
    fn an_indented_first_row_still_starts_a_block() {
        let mut blocks = Blocks::new();
        assert!(blocks.is_empty());
        blocks.push(&row("    indented, but there is nothing above it"));
        assert!(!blocks.is_empty());
        assert_eq!(blocks.count(), 1);
        assert_eq!(blocks.starts(), &[0]);
        blocks.push(&row("  still the same block"));
        assert_eq!(
            blocks.count(),
            1,
            "the second indented row continues the first"
        );
        assert_eq!(blocks.len(), 2);
    }

    /// The last block's end is the row count: it is open, but it already ends
    /// at `len()`, so nothing that follows can be wrong about where it ends.
    #[test]
    fn the_last_block_ends_at_the_row_count() {
        let rows = rows(&["a", "  b", "c", "\td", "  e"]);
        let blocks = Blocks::of(&rows);
        assert_eq!(blocks.len(), 5);
        assert_eq!(blocks.blocks().collect::<Vec<_>>(), vec![(0, 2), (2, 5)]);
        assert_eq!(blocks.block_of(4), Some((2, 5)));
        assert_eq!(blocks.blocks().last(), Some((2, 5)));
        assert_eq!(blocks.blocks().map(|(_, end)| end).max(), Some(5));
    }

    /// `block_of` agrees with `starts` for every seen row, and `None` for any
    /// row not seen.
    #[test]
    fn block_of_agrees_with_starts() {
        let rows = rows(&[
            "one",
            "  two",
            "",
            "\tthree",
            "four",
            "  ",
            "five",
            "\u{a0}six",
            "seven",
        ]);
        let blocks = Blocks::of(&rows);
        for r in 0..rows.len() {
            let (start, end) = blocks
                .block_of(r)
                .unwrap_or_else(|| panic!("a seen row is in a block: row {r}"));
            let expected = blocks
                .starts()
                .iter()
                .copied()
                .filter(|&s| s <= r)
                .max()
                .expect("starts[0] is 0, so one start is at or below any row");
            assert_eq!(start, expected, "row {r}");
            assert!(start <= r && r < end, "row {r} outside its own block");
            if end < rows.len() {
                // the row past the end starts the next block
                assert_eq!(blocks.block_of(end).map(|(s, _)| s), Some(end), "row {r}");
            } else {
                assert_eq!(end, rows.len());
                assert_eq!(blocks.block_of(end), None, "row {r}");
            }
        }
        assert_eq!(blocks.block_of(rows.len()), None, "row len is not a row");
        assert_eq!(blocks.block_of(usize::MAX), None);
    }

    /// No rows seen: no blocks, no boundaries, and row 0 is not a row yet.
    #[test]
    fn zero_rows() {
        let blocks = Blocks::new();
        assert!(blocks.is_empty());
        assert_eq!(blocks.len(), 0);
        assert_eq!(blocks.count(), 0);
        assert!(blocks.starts().is_empty());
        assert_eq!(blocks.block_of(0), None);
        assert!(blocks.blocks().next().is_none());
        assert_eq!(Blocks::of(&[]), blocks, "the batch constructor agrees");
    }

    /// The corpus for the equivalence check: the two real shapes the rule is
    /// for, a python traceback, and a raggedy mix of the edge cases — a tail
    /// does not see tidy categories.
    fn corpus() -> Vec<Vec<char>> {
        rows(&[
            // llama.cpp-style records
            "12:34:56.789 I llama_model_load: loading model",
            "12:34:56.790 W llama_model_load: vocab mismatch",
            // a killer stack trace, as one block
            "Error: failed to open 'models/7b.gguf'",
            "   0: rano::loader::open at src/loader.rs:212",
            "   1: rano::main at src/main.rs:14",
            // a blank row closes it and is its own block
            "",
            // a second trace
            "Error2: and again",
            "   0: somewhere else",
            // python traceback: indented frames under unindented lines
            "Traceback (most recent call last):",
            "  File \"app.py\", line 10, in <module>",
            "    main()",
            "  File \"app.py\", line 4, in main",
            "    raise ValueError('boom')",
            "ValueError: boom",
            // raggedy mix: tab-only, space-only and U+00A0 rows all continue
            "\tcontinues the ValueError block",
            "   ",
            "\u{a0}continues too",
            "plain",
            " trailing",
        ])
    }

    /// The batch constructor and N incremental pushes see the same rows in the
    /// same order, so they must give identical boundaries — §20.4 computes
    /// boundaries as rows arrive, and arrival is the only input.
    #[test]
    fn batch_and_incremental_agree() {
        let rows = corpus();
        let batch = Blocks::of(&rows);
        let mut incremental = Blocks::new();
        for row in &rows {
            incremental.push(row);
        }
        assert_eq!(batch, incremental);
        assert_eq!(batch.starts(), incremental.starts());
        assert_eq!(batch.len(), incremental.len());
        assert_eq!(batch.count(), incremental.count());
        // spot-check where the shapes change
        assert_eq!(batch.starts(), &[0, 1, 2, 5, 6, 8, 13, 17]);
        assert_eq!(batch.len(), 19);
        // the second trace is one block, and the blank before it is not in it
        assert_eq!(batch.block_of(6), Some((6, 8)));
        // the python header and its frames are one block; `ValueError:` starts
        // the next, which the tab-only and U+00A0 rows continue
        assert_eq!(batch.block_of(8), Some((8, 13)));
        assert_eq!(batch.block_of(13), Some((13, 17)));
    }

    /// A python traceback under the one rule: the `Traceback` header and its
    /// indented `File`/`raise` frames are one block, the unindented exception
    /// line another. No `File "` anywhere in the rule.
    #[test]
    fn a_python_traceback_is_two_blocks() {
        let rows = rows(&[
            "Traceback (most recent call last):",
            "  File \"app.py\", line 10, in <module>",
            "    main()",
            "  File \"app.py\", line 4, in main",
            "    raise ValueError('boom')",
            "ValueError: boom",
        ]);
        let blocks = Blocks::of(&rows);
        assert_eq!(blocks.blocks().collect::<Vec<_>>(), vec![(0, 5), (5, 6)]);
    }
}
