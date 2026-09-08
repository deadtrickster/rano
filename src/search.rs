//! Search matcher core (F5): literal and regex matching over the buffer's
//! char rows, returning (Pos, match length in chars). Lengths are stored so
//! variable-width regex matches can be highlighted correctly.

use crate::buffer::Pos;

pub enum Matcher {
    Literal {
        needle: Vec<char>,
        case_sensitive: bool,
    },
    Re(regex::Regex),
}

impl Matcher {
    pub fn literal(query: &str, case_sensitive: bool) -> Matcher {
        Matcher::Literal {
            needle: query.chars().collect(),
            case_sensitive,
        }
    }

    /// `(?i)`-prefixed pattern when the search is case-insensitive.
    pub fn regex(pattern: &str, case_sensitive: bool) -> Result<Matcher, regex::Error> {
        if case_sensitive {
            Ok(Matcher::Re(regex::Regex::new(pattern)?))
        } else {
            Ok(Matcher::Re(regex::Regex::new(&format!("(?i){}", pattern))?))
        }
    }

    /// All matches in row-major order. Literal matches overlap (mirroring
    /// `Buffer::find_all`); regex matches follow `find_iter` (non-overlapping).
    pub fn find_all(&self, lines: &[Vec<char>]) -> Vec<(Pos, usize)> {
        match self {
            Matcher::Literal {
                needle,
                case_sensitive,
            } => {
                let mut out = Vec::new();
                if needle.is_empty() {
                    return out;
                }
                // Case-insensitivity folds each char with
                // `to_lowercase().next()`: chars whose lowercase expands to
                // more than one char (e.g. 'İ' → "i̇") fold to the first char
                // only. ASCII and Latin-1 (é/É) are correct; full-Unicode
                // casing is out of scope.
                let fold = |c: char| {
                    if *case_sensitive {
                        c
                    } else {
                        c.to_lowercase().next().unwrap_or(c)
                    }
                };
                let n: Vec<char> = needle.iter().copied().map(fold).collect();
                for (row, line) in lines.iter().enumerate() {
                    let mut i = 0;
                    while i + n.len() <= line.len() {
                        if (0..n.len()).all(|k| fold(line[i + k]) == n[k]) {
                            out.push((Pos { row, col: i }, n.len()));
                        }
                        i += 1;
                    }
                }
                out
            }
            Matcher::Re(re) => {
                let mut out = Vec::new();
                for (row, line) in lines.iter().enumerate() {
                    let s: String = line.iter().collect();
                    for m in re.find_iter(&s) {
                        // Regex reports BYTE offsets; positions are CHAR cols.
                        let col = s[..m.start()].chars().count();
                        let len = s[m.start()..m.end()].chars().count();
                        out.push((Pos { row, col }, len));
                    }
                }
                out
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(text: &str) -> Vec<Vec<char>> {
        text.lines().map(|l| l.chars().collect()).collect()
    }

    #[test]
    fn literal_case_insensitive() {
        let ls = lines("foo Bar\nFOO");
        let m = Matcher::literal("fOO", false);
        assert_eq!(
            m.find_all(&ls),
            vec![(Pos { row: 0, col: 0 }, 3), (Pos { row: 1, col: 0 }, 3)]
        );
    }

    #[test]
    fn literal_case_sensitive() {
        let ls = lines("foo Bar\nFOO");
        assert!(Matcher::literal("fOO", true).find_all(&ls).is_empty());
        assert_eq!(
            Matcher::literal("Bar", true).find_all(&ls),
            vec![(Pos { row: 0, col: 4 }, 3)]
        );
    }

    #[test]
    fn literal_empty_needle_matches_nothing() {
        let ls = lines("abc");
        assert!(Matcher::literal("", true).find_all(&ls).is_empty());
        assert!(Matcher::literal("", false).find_all(&ls).is_empty());
    }

    #[test]
    fn regex_char_class() {
        let ls = lines("abc axc a-c");
        let m = Matcher::regex("a.c", true).expect("valid pattern");
        assert_eq!(
            m.find_all(&ls),
            vec![
                (Pos { row: 0, col: 0 }, 3),
                (Pos { row: 0, col: 4 }, 3),
                (Pos { row: 0, col: 8 }, 3)
            ]
        );
    }

    #[test]
    fn regex_variable_len() {
        let ls = lines("caaad");
        let m = Matcher::regex("a+", true).expect("valid pattern");
        assert_eq!(m.find_all(&ls), vec![(Pos { row: 0, col: 1 }, 3)]);
    }

    #[test]
    fn regex_multibyte_char_cols() {
        // 'é' sits BEFORE the match: its 2-byte encoding must remap to char
        // col 1, not leave the highlight at byte offset 2.
        let ls = lines("éa");
        let m = Matcher::regex("a", true).expect("valid pattern");
        assert_eq!(m.find_all(&ls), vec![(Pos { row: 0, col: 1 }, 1)]);
    }

    #[test]
    fn regex_case_insensitive_flag() {
        let ls = lines("FOO foo");
        let m = Matcher::regex("foo", false).expect("valid pattern");
        assert_eq!(
            m.find_all(&ls),
            vec![(Pos { row: 0, col: 0 }, 3), (Pos { row: 0, col: 4 }, 3)]
        );
    }

    #[test]
    fn regex_bad_pattern_is_err() {
        assert!(Matcher::regex("(", true).is_err());
    }
}
