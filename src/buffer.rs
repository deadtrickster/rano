use std::fs;
use std::path::PathBuf;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct Pos {
    pub row: usize,
    pub col: usize,
}

#[derive(Debug, Clone)]
pub struct Buffer {
    pub lines: Vec<Vec<char>>,
    pub name: Option<PathBuf>,
    pub modified: bool,
    pub crlf: bool,
}

impl Buffer {
    pub fn new() -> Self {
        Self {
            lines: vec![Vec::new()],
            name: None,
            modified: false,
            crlf: false,
        }
    }
}

impl Default for Buffer {
    fn default() -> Self {
        Self::new()
    }
}

impl Buffer {
    pub fn from_file(path: &std::path::Path) -> std::io::Result<Self> {
        let text = fs::read_to_string(path)?;
        // detect CRLF before str::lines() strips the \r
        let crlf = text.contains("\r\n");
        let lines: Vec<Vec<char>> = text.lines().map(|l| l.chars().collect()).collect();
        let lines = if lines.is_empty() {
            vec![Vec::new()]
        } else {
            lines
        };
        Ok(Self {
            lines,
            name: Some(path.to_path_buf()),
            modified: false,
            crlf,
        })
    }

    pub fn line_len(&self, row: usize) -> usize {
        self.lines.get(row).map_or(0, |l| l.len())
    }

    pub fn row_count(&self) -> usize {
        self.lines.len()
    }

    pub fn clamp(&self, p: Pos) -> Pos {
        let row = p.row.min(self.row_count().saturating_sub(1));
        let col = p.col.min(self.line_len(row));
        Pos { row, col }
    }

    pub fn text(&self) -> String {
        let joined: String = self
            .lines
            .iter()
            .map(|l| l.iter().collect::<String>())
            .collect::<Vec<_>>()
            .join("\n");
        if let Some(last) = self.lines.last()
            && !last.is_empty()
        {
            return joined + "\n";
        }
        joined
    }

    /// text() but with CRLF line endings when the file was CRLF (save path).
    pub fn file_text(&self) -> String {
        if !self.crlf {
            return self.text();
        }
        let joined: String = self
            .lines
            .iter()
            .map(|l| l.iter().collect::<String>())
            .collect::<Vec<_>>()
            .join("\r\n");
        if let Some(last) = self.lines.last()
            && !last.is_empty()
        {
            return joined + "\r\n";
        }
        joined
    }

    fn remove_empty_line(&mut self, row: usize) -> bool {
        if row < self.lines.len() && self.lines[row].is_empty() && self.lines.len() > 1 {
            self.lines.remove(row);
            true
        } else {
            false
        }
    }

    pub fn insert_char(&mut self, row: usize, col: usize, ch: char) {
        if let Some(line) = self.lines.get_mut(row) {
            line.insert(col.min(line.len()), ch);
        }
    }

    pub fn delete_at(&mut self, row: usize, col: usize) -> bool {
        let n = self.lines.len();
        if row >= n {
            return false;
        }
        let l = self.line_len(row);
        if col < l {
            self.lines[row].remove(col);
            self.remove_empty_line(row);
            true
        } else if row + 1 < n {
            let mut next = self.lines.remove(row + 1);
            self.lines[row].append(&mut next);
            true
        } else {
            false
        }
    }

    pub fn backspace(&mut self, row: usize, col: usize) {
        if col > 0 {
            self.lines[row].remove(col - 1);
            self.remove_empty_line(row);
        } else if row > 0 {
            let line = self.lines.remove(row);
            self.lines[row - 1].extend(line);
        }
    }

    pub fn newline(&mut self, row: usize, col: usize) {
        let col = col.min(self.line_len(row));
        let right: Vec<char> = self.lines[row].drain(col..).collect();
        self.lines.insert(row + 1, right);
    }

    /// Cut the region [a, b) in row-major order.
    pub fn cut_range(&mut self, a: Pos, b: Pos) -> Vec<Vec<char>> {
        let mut out: Vec<Vec<char>> = Vec::new();
        if a.row == b.row {
            let frag: Vec<char> = self.lines[a.row].drain(a.col..b.col).collect();
            out.push(frag);
            self.remove_empty_line(a.row);
            return out;
        }
        let first: Vec<char> = self.lines[a.row].drain(a.col..).collect();
        if !first.is_empty() {
            out.push(first);
        }
        let first_removed = self.remove_empty_line(a.row) as usize;
        // Whole rows strictly between a's and b's rows. Their indices are
        // shifted down by first_removed if a's row disappeared.
        let r = a.row + 1 - first_removed;
        let end = b.row - 1 - first_removed; // inclusive
        if r <= end {
            out.extend(self.lines.drain(r..=end));
        }
        // b's row now sits right after everything that remains above it.
        let b_row = a.row + 1 - first_removed;
        let len = self.lines[b_row].len();
        let last_end = b.col.min(len);
        let last: Vec<char> = self.lines[b_row].drain(..last_end).collect();
        if !last.is_empty() {
            out.push(last);
        }
        self.remove_empty_line(b_row);
        out
    }

    /// Copy the region [a, b) in row-major order without modifying the
    /// buffer (M-6). Empty edge fragments are dropped.
    pub fn copy_range(&self, a: Pos, b: Pos) -> Vec<Vec<char>> {
        let mut out: Vec<Vec<char>> = Vec::new();
        if a.row == b.row {
            out.push(self.lines[a.row][a.col..b.col].to_vec());
            return out;
        }
        let first = self.lines[a.row][a.col..].to_vec();
        if !first.is_empty() {
            out.push(first);
        }
        for r in a.row + 1..b.row {
            out.push(self.lines[r].clone());
        }
        let last = self.lines[b.row][..b.col].to_vec();
        if !last.is_empty() {
            out.push(last);
        }
        if out.is_empty() {
            out.push(Vec::new());
        }
        out
    }

    /// Insert lines as new rows starting at `row`, pushing existing rows down.
    pub fn insert_lines_at(&mut self, row: usize, lines: Vec<Vec<char>>) -> usize {
        let row = row.min(self.lines.len());
        let n = lines.len();
        for (i, l) in lines.into_iter().enumerate() {
            self.lines.insert(row + i, l);
        }
        if self.lines.is_empty() {
            self.lines.push(Vec::new());
        }
        n
    }

    /// Merge a text fragment into (row, col) on the same line.
    pub fn merge_inline(&mut self, row: usize, col: usize, frag: &[char]) {
        if let Some(line) = self.lines.get_mut(row) {
            let c = col.min(line.len());
            for (i, ch) in frag.iter().enumerate() {
                line.insert(c + i, *ch);
            }
        }
    }

    pub fn find_all(&self, needle: &str) -> Vec<Pos> {
        let mut out = Vec::new();
        let n: Vec<char> = needle.chars().collect();
        if n.is_empty() {
            return out;
        }
        for (row, line) in self.lines.iter().enumerate() {
            let mut i = 0;
            while i + n.len() <= line.len() {
                if line[i..i + n.len()] == n[..] {
                    out.push(Pos { row, col: i });
                }
                i += 1;
            }
        }
        out
    }

    /// First occurrence strictly after `from`, wrapping to the start.
    pub fn find_next(&self, from: Pos, needle: &str) -> Option<Pos> {
        let all = self.find_all(needle);
        all.iter()
            .copied()
            .find(|m| *m > from)
            .or_else(|| all.first().copied())
    }

    pub fn replace_at(&mut self, pos: Pos, find: &str, with: &str) -> bool {
        let n: Vec<char> = find.chars().collect();
        let w: Vec<char> = with.chars().collect();
        let Some(line) = self.lines.get_mut(pos.row) else {
            return false;
        };
        if pos.col + n.len() > line.len() || line[pos.col..pos.col + n.len()] != n[..] {
            return false;
        }
        line.splice(pos.col..pos.col + n.len(), w.iter().cloned());
        self.modified = true;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buf(text: &str) -> Buffer {
        let lines: Vec<Vec<char>> = text.lines().map(|l| l.chars().collect()).collect();
        let lines = if lines.is_empty() {
            vec![Vec::new()]
        } else {
            lines
        };
        Buffer {
            lines,
            name: None,
            modified: false,
            crlf: false,
        }
    }

    fn lines_of(b: &Buffer) -> Vec<String> {
        b.lines.iter().map(|l| l.iter().collect()).collect()
    }

    // ---------- text / clamp ----------

    #[test]
    fn text_trailing_newline() {
        assert_eq!(buf("abc").text(), "abc\n");
        assert_eq!(buf("a\nb").text(), "a\nb\n");
        assert_eq!(buf("").text(), "");
        assert_eq!(buf("a\n").text(), "a\n");
    }

    #[test]
    fn clamp_inbounds() {
        let b = buf("ab\ncdef");
        assert_eq!(b.clamp(Pos { row: 9, col: 9 }), Pos { row: 1, col: 4 });
        assert_eq!(b.clamp(Pos { row: 1, col: 2 }), Pos { row: 1, col: 2 });
    }

    // ---------- edits ----------

    #[test]
    fn newline_split() {
        let mut b = buf("abcd");
        b.newline(0, 2);
        assert_eq!(lines_of(&b), vec!["ab", "cd"]);
    }

    #[test]
    fn backspace_joins_lines() {
        let mut b = buf("ab\ncd");
        b.backspace(1, 0);
        assert_eq!(lines_of(&b), vec!["abcd"]);
    }

    #[test]
    fn delete_at_joins_lines() {
        let mut b = buf("ab\ncd");
        assert!(b.delete_at(0, 2));
        assert_eq!(lines_of(&b), vec!["abcd"]);
        // nothing after the last char of the last line
        assert!(!b.delete_at(0, 4));
    }

    // ---------- cut_range ----------

    #[test]
    fn cut_range_single_row() {
        let mut b = buf("hello world");
        let out = b.cut_range(Pos { row: 0, col: 0 }, Pos { row: 0, col: 5 });
        assert_eq!(out, vec!["hello".chars().collect::<Vec<char>>()]);
        assert_eq!(lines_of(&b), vec![" world"]);
    }

    #[test]
    fn cut_range_multi_row_keeps_endpoints() {
        let mut b = buf("aaa\nbbb\nccc\nddd");
        // cut from (0,1) to (3,2): drops 'aaa'[1..], whole 'bbb','ccc', 'dd'
        let out = b.cut_range(Pos { row: 0, col: 1 }, Pos { row: 3, col: 2 });
        assert_eq!(
            out.iter()
                .map(|l| l.iter().collect::<String>())
                .collect::<Vec<_>>(),
            vec!["aa", "bbb", "ccc", "dd"]
        );
        // remaining: 'a' and last char 'd' of the final line
        assert_eq!(lines_of(&b), vec!["a", "d"]);
    }

    #[test]
    fn cut_range_whole_lines_no_infinite_loop() {
        // Regression: the middle-row loop used to never advance `r`.
        let mut b = buf("111\n222\n333\n444");
        let out = b.cut_range(Pos { row: 0, col: 0 }, Pos { row: 2, col: 0 });
        assert_eq!(
            out.iter()
                .map(|l| l.iter().collect::<String>())
                .collect::<Vec<_>>(),
            vec!["111", "222"]
        );
        assert_eq!(lines_of(&b), vec!["333", "444"]);
    }

    #[test]
    fn cut_range_first_row_removed() {
        let mut b = buf("x\naaa\nbbb\nccc");
        // (0,0) cuts all of 'x' -> first row removed, shifts indices
        let out = b.cut_range(Pos { row: 0, col: 0 }, Pos { row: 3, col: 1 });
        assert_eq!(
            out.iter()
                .map(|l| l.iter().collect::<String>())
                .collect::<Vec<_>>(),
            vec!["x", "aaa", "bbb", "c"]
        );
        assert_eq!(lines_of(&b), vec!["cc"]);
    }

    #[test]
    fn cut_range_empty_buffer_safe() {
        let mut b = buf("");
        let out = b.cut_range(Pos { row: 0, col: 0 }, Pos { row: 0, col: 0 });
        assert!(out.is_empty() || out.iter().all(|l| l.is_empty()));
        assert_eq!(b.row_count(), 1);
    }

    // ---------- copy_range ----------

    #[test]
    fn copy_range_single_row() {
        let b = buf("hello world");
        let out = b.copy_range(Pos { row: 0, col: 0 }, Pos { row: 0, col: 5 });
        assert_eq!(out, vec!["hello".chars().collect::<Vec<char>>()]);
        // copy leaves the buffer untouched
        assert_eq!(lines_of(&b), vec!["hello world"]);
    }

    #[test]
    fn copy_range_multi_row_keeps_endpoints() {
        let b = buf("aaa\nbbb\nccc\nddd");
        // copy from (0,1) to (3,2): 'aaa'[1..], whole 'bbb','ccc', 'dd'
        let out = b.copy_range(Pos { row: 0, col: 1 }, Pos { row: 3, col: 2 });
        assert_eq!(
            out.iter()
                .map(|l| l.iter().collect::<String>())
                .collect::<Vec<_>>(),
            vec!["aa", "bbb", "ccc", "dd"]
        );
        assert_eq!(lines_of(&b), vec!["aaa", "bbb", "ccc", "ddd"]);
    }

    #[test]
    fn copy_range_whole_lines() {
        let b = buf("111\n222\n333\n444");
        let out = b.copy_range(Pos { row: 0, col: 0 }, Pos { row: 2, col: 0 });
        assert_eq!(
            out.iter()
                .map(|l| l.iter().collect::<String>())
                .collect::<Vec<_>>(),
            vec!["111", "222"]
        );
        assert_eq!(lines_of(&b), vec!["111", "222", "333", "444"]);
    }

    #[test]
    fn copy_range_empty_region() {
        // same-row a == b: the single-row branch always pushes, even empty
        let b = buf("hello world");
        let out = b.copy_range(Pos { row: 0, col: 3 }, Pos { row: 0, col: 3 });
        assert_eq!(out, vec![Vec::<char>::new()]);
        // cross-row empty: a at end of row 0, b at start of row 1 hits the
        // out.is_empty() fallback
        let b2 = buf("abc\nd");
        let out2 = b2.copy_range(Pos { row: 0, col: 3 }, Pos { row: 1, col: 0 });
        assert_eq!(out2, vec![Vec::<char>::new()]);
    }

    // ---------- insert / merge ----------

    #[test]
    fn insert_lines_at_middle() {
        let mut b = buf("a\nc");
        let new: Vec<Vec<char>> = vec!["b".chars().collect()];
        b.insert_lines_at(1, new);
        assert_eq!(lines_of(&b), vec!["a", "b", "c"]);
    }

    #[test]
    fn merge_inline_mid() {
        let mut b = buf("helo");
        b.merge_inline(0, 2, &"l".chars().collect::<Vec<char>>());
        assert_eq!(lines_of(&b), vec!["hello"]);
    }

    // ---------- insert_char ----------

    #[test]
    fn insert_char_mid() {
        let mut b = buf("helo");
        b.insert_char(0, 2, 'l');
        assert_eq!(lines_of(&b), vec!["hello"]);
    }

    #[test]
    fn insert_char_eol_clamps() {
        // col past EOL is clamped to the line length
        let mut b = buf("ab");
        b.insert_char(0, 99, '!');
        assert_eq!(lines_of(&b), vec!["ab!"]);
    }

    #[test]
    fn insert_char_empty_buffer() {
        let mut b = buf("");
        b.insert_char(0, 0, 'x');
        assert_eq!(lines_of(&b), vec!["x"]);
    }

    #[test]
    fn insert_char_bad_row_no_op() {
        // silent no-op on a row that does not exist
        let mut b = buf("ab");
        b.insert_char(5, 0, 'x');
        assert_eq!(lines_of(&b), vec!["ab"]);
    }

    // ---------- find / replace ----------

    #[test]
    fn find_all_multiple() {
        let b = buf("a a\naa");
        let m = b.find_all("a");
        assert_eq!(
            m,
            vec![
                Pos { row: 0, col: 0 },
                Pos { row: 0, col: 2 },
                Pos { row: 1, col: 0 },
                Pos { row: 1, col: 1 }
            ]
        );
    }

    #[test]
    fn find_next_wraps() {
        let b = buf("x\nx");
        let from = Pos { row: 1, col: 1 };
        // strictly after (1,1) wraps to the first match
        assert_eq!(b.find_next(from, "x"), Some(Pos { row: 0, col: 0 }));
    }

    #[test]
    fn replace_at_basic() {
        let mut b = buf("foo bar");
        b.replace_at(Pos { row: 0, col: 0 }, "foo", "baz");
        assert_eq!(lines_of(&b), vec!["baz bar"]);
        assert!(b.modified);
    }

    #[test]
    fn find_all_overlap() {
        // overlapping matches are kept: scan steps by 1, not by needle length
        let b = buf("aaa");
        assert_eq!(
            b.find_all("aa"),
            vec![Pos { row: 0, col: 0 }, Pos { row: 0, col: 1 }]
        );
        let b2 = buf("aaaa");
        assert_eq!(
            b2.find_all("aa"),
            vec![
                Pos { row: 0, col: 0 },
                Pos { row: 0, col: 1 },
                Pos { row: 0, col: 2 }
            ]
        );
    }

    #[test]
    fn replace_at_longer() {
        let mut b = buf("foo bar");
        assert!(b.replace_at(Pos { row: 0, col: 0 }, "foo", "foobar"));
        assert_eq!(lines_of(&b), vec!["foobar bar"]);
    }

    #[test]
    fn replace_at_shorter() {
        let mut b = buf("foo bar");
        assert!(b.replace_at(Pos { row: 0, col: 0 }, "foo", "f"));
        assert_eq!(lines_of(&b), vec!["f bar"]);
    }

    #[test]
    fn replace_at_multibyte() {
        // positions and lengths are in chars, not bytes
        let mut b = buf("héllo");
        assert!(b.replace_at(Pos { row: 0, col: 1 }, "é", "e"));
        assert_eq!(lines_of(&b), vec!["hello"]);
    }

    #[test]
    fn replace_at_non_match_returns_false() {
        let mut b = buf("foo bar");
        assert!(!b.replace_at(Pos { row: 0, col: 1 }, "foo", "x"));
        assert_eq!(lines_of(&b), vec!["foo bar"]);
        assert!(!b.modified);
    }

    #[test]
    fn replace_at_past_eol_false() {
        // match would run past end of line
        let mut b = buf("foo bar");
        assert!(!b.replace_at(Pos { row: 0, col: 5 }, "foo", "x"));
        assert_eq!(lines_of(&b), vec!["foo bar"]);
        assert!(!b.modified);
    }

    #[test]
    fn replace_at_bad_row_false() {
        let mut b = buf("foo bar");
        assert!(!b.replace_at(Pos { row: 3, col: 0 }, "foo", "x"));
        assert!(!b.modified);
    }

    // ---------- crlf ----------

    fn tmp_path(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("rano_{}_{}", tag, std::process::id()))
    }

    #[test]
    fn crlf_roundtrip() {
        let path = tmp_path("crlf_rt");
        fs::write(&path, "a\r\nb\r\n").unwrap();
        let b = Buffer::from_file(&path).unwrap();
        let _ = fs::remove_file(&path);
        assert!(b.crlf);
        // in-buffer text is normalized to \n
        assert_eq!(b.text(), "a\nb\n");
        // file_text() restores the original line endings
        assert_eq!(b.file_text(), "a\r\nb\r\n");
    }

    #[test]
    fn crlf_lf_file_untouched() {
        let path = tmp_path("crlf_lf");
        fs::write(&path, "a\nb\n").unwrap();
        let b = Buffer::from_file(&path).unwrap();
        let _ = fs::remove_file(&path);
        assert!(!b.crlf);
        assert_eq!(b.text(), "a\nb\n");
        assert_eq!(b.file_text(), b.text());
    }

    #[test]
    fn crlf_mixed_eol_detected() {
        // characterization: any \r\n in the file flips the flag, so LF-only
        // rows come back with \r\n on save (nano behaves the same)
        let path = tmp_path("crlf_mixed");
        fs::write(&path, "a\nb\r\n").unwrap();
        let b = Buffer::from_file(&path).unwrap();
        let _ = fs::remove_file(&path);
        assert!(b.crlf);
        assert_eq!(b.text(), "a\nb\n");
        assert_eq!(b.file_text(), "a\r\nb\r\n");
    }

    #[test]
    fn crlf_file_text_trailing_rule() {
        // file_text() mirrors text()'s trailing-newline rule exactly:
        // it is text() with every \n swapped for \r\n
        for c in ["a\n", "a\n\n", "", "a\nb\n\n"] {
            let mut b = buf(c);
            b.crlf = true;
            assert_eq!(b.file_text(), b.text().replace('\n', "\r\n"));
        }
        // empty last row: no EXTRA trailing newline (join separator remains)
        let mut b = buf("a\n\n");
        b.crlf = true;
        assert_eq!(b.file_text(), "a\r\n");
    }
}
