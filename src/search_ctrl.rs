//! Search & replace control (F5): SearchState/ReplaceState, match
//! navigation and the replace state machine, backed by search::Matcher.

use crate::buffer::Pos;
use crate::editor::{ActionKind, Editor, plural};
use crate::prompt::{Prompt, PromptKind};
use crate::search;

#[derive(Debug, Clone)]
pub struct SearchState {
    pub query: String,
    pub current: usize,
    pub backwards: bool,
}

#[derive(Debug, Clone)]
pub struct ReplaceState {
    pub find: String,
    pub with: String,
}

impl Editor {
    // ---------- search ----------

    pub(crate) fn start_search(&mut self) {
        self.bs_mut().search.backwards = false;
        if self
            .bs()
            .search_matches
            .as_ref()
            .is_some_and(|ms| !ms.is_empty())
        {
            self.next_match();
            return;
        }
        let query = self.bs().search.query.clone();
        self.prompt = Some(Prompt {
            kind: PromptKind::Search,
            text: query.clone(),
            cursor: query.chars().count(),
        });
    }

    // Where Was (nano's ^B): search backwards from the cursor.
    pub(crate) fn start_search_backward(&mut self) {
        self.bs_mut().search.backwards = true;
        if self
            .bs()
            .search_matches
            .as_ref()
            .is_some_and(|ms| !ms.is_empty())
        {
            self.prev_match();
            return;
        }
        let query = self.bs().search.query.clone();
        self.prompt = Some(Prompt {
            kind: PromptKind::Search,
            text: query.clone(),
            cursor: query.chars().count(),
        });
    }

    pub(crate) fn do_search(&mut self, query: String) {
        if query.is_empty() {
            return;
        }
        let matcher = if self.search_regex {
            match search::Matcher::regex(&query, self.search_case_sensitive) {
                Ok(m) => m,
                Err(e) => {
                    self.flash(&format!("Invalid regex: {e}"));
                    return;
                }
            }
        } else {
            search::Matcher::literal(&query, self.search_case_sensitive)
        };
        let all = matcher.find_all(&self.bs().buf.lines);
        let bs = self.bs_mut();
        bs.search.query = query;
        bs.search_matches = Some(all);
        let ms = bs.search_matches.as_ref().expect("just stored");
        if ms.is_empty() {
            bs.search.current = 0;
            self.flash("No matches");
            return;
        }
        // First match at-or-after the cursor (previous < cursor when
        // backwards), wrapping.
        let cur = if bs.search.backwards {
            ms.iter()
                .rposition(|(m, _)| *m < bs.cursor)
                .unwrap_or(ms.len() - 1)
        } else {
            ms.iter().position(|(m, _)| *m >= bs.cursor).unwrap_or(0)
        };
        bs.search.current = cur;
        self.jump_to_match();
    }

    pub(crate) fn next_match(&mut self) {
        let Some(ms) = &self.bs().search_matches else {
            return;
        };
        if ms.is_empty() {
            return;
        }
        // Next match strictly after the cursor, wrapping to the first.
        let cur = self.bs().cursor;
        let next = ms.iter().position(|(m, _)| *m > cur).unwrap_or(0);
        self.bs_mut().search.current = next;
        self.jump_to_match();
    }

    pub(crate) fn prev_match(&mut self) {
        let Some(ms) = &self.bs().search_matches else {
            return;
        };
        if ms.is_empty() {
            return;
        }
        // Previous match strictly before the cursor, wrapping to the last.
        let cur = self.bs().cursor;
        let prev = ms
            .iter()
            .rposition(|(m, _)| *m < cur)
            .unwrap_or(ms.len() - 1);
        self.bs_mut().search.current = prev;
        self.jump_to_match();
    }

    pub(crate) fn jump_to_match(&mut self) {
        let Some(ms) = &self.bs().search_matches else {
            return;
        };
        if ms.is_empty() {
            return;
        }
        let idx = self.bs().search.current.min(ms.len() - 1);
        let (m, _) = ms[idx];
        let bs = self.bs_mut();
        bs.cursor = m;
        bs.mark = None;
    }

    // ---------- replace ----------

    pub(crate) fn start_replace(&mut self) {
        let query = self.bs().search.query.clone();
        self.prompt = Some(Prompt {
            kind: PromptKind::ReplaceFind,
            text: query.clone(),
            cursor: query.chars().count(),
        });
    }

    pub(crate) fn next_replace_ask(&mut self) {
        let st = match self.replace.clone() {
            Some(s) => s,
            None => return,
        };
        if st.find.is_empty() {
            self.replace = None;
            return;
        }
        let cursor = self.bs().cursor;
        let found = self.bs().buf.find_next(cursor, &st.find);
        self.replace_pos = found;
        match self.replace_pos {
            Some(_) => {
                self.prompt = Some(Prompt {
                    kind: PromptKind::ReplaceAsk,
                    text: format!("Replace #{} (y,n,a,q): ", self.replace_count + 1),
                    cursor: 0,
                });
            }
            None => {
                self.replace = None;
                self.flash(&format!(
                    "Replaced {} occurrence{}",
                    self.replace_count,
                    plural(self.replace_count)
                ));
            }
        }
    }

    /// One pass, left-to-right, non-overlapping, from the first match
    /// at-or-after the cursor (no wrap). Single-line find/with only; rows
    /// never shift, so row-major order is preserved and drift only applies
    /// within the anchor row. Returns the cumulative replace_count.
    fn replace_all_from(&mut self, find: &str, with: &str) -> usize {
        let n_len = find.chars().count();
        let w_len = with.chars().count();
        let cur = self.bs().cursor;
        // collect once; keep only matches at-or-after cursor, row-major
        let matches: Vec<Pos> = self
            .bs()
            .buf
            .find_all(find)
            .into_iter()
            .filter(|m| m.row > cur.row || (m.row == cur.row && m.col >= cur.col))
            .collect();
        let (mut drift, mut next_free, mut anchor) = (0isize, 0usize, None);
        for m in matches {
            let col = if anchor == Some(m.row) {
                (m.col as isize + drift).max(0) as usize
            } else {
                m.col
            };
            if anchor == Some(m.row) && col < next_free {
                continue; // overlap skip
            }
            if !self
                .bs_mut()
                .buf
                .replace_at(Pos { row: m.row, col }, find, with)
            {
                continue;
            }
            anchor = Some(m.row);
            drift += w_len as isize - n_len as isize;
            next_free = col + w_len;
            self.replace_count += 1;
            self.bs_mut().cursor = Pos {
                row: m.row,
                col: next_free,
            };
        }
        self.replace_count
    }

    pub(crate) fn answer_replace_ask(&mut self, c: char) {
        let st = match self.replace.clone() {
            Some(s) => s,
            None => return,
        };
        match c {
            'y' | 'Y' => {
                if let Some(pos) = self.replace_pos {
                    self.begin_action(ActionKind::Replace, pos.row, pos.row + 1);
                    self.bs_mut().buf.replace_at(pos, &st.find, &st.with);
                    self.replace_count += 1;
                    self.bs_mut().cursor = Pos {
                        row: pos.row,
                        col: pos.col + st.with.chars().count(),
                    };
                    self.finish_step();
                    self.edit_invalidate();
                    self.next_replace_ask();
                }
            }
            'n' | 'N' => {
                if let Some(pos) = self.replace_pos {
                    self.bs_mut().cursor = Pos {
                        row: pos.row,
                        col: pos.col + st.find.chars().count(),
                    };
                    self.next_replace_ask();
                }
            }
            'a' | 'A' => {
                let cur = self.bs().cursor;
                let all: Vec<Pos> = self
                    .bs()
                    .buf
                    .find_all(&st.find)
                    .into_iter()
                    .filter(|m| m.row > cur.row || (m.row == cur.row && m.col >= cur.col))
                    .collect();
                if let (Some(first), Some(last)) = (all.first(), all.last()) {
                    self.begin_action(ActionKind::Replace, first.row, last.row + 1);
                    self.replace_all_from(&st.find, &st.with);
                    self.finish_step();
                    self.edit_invalidate();
                }
                self.replace = None;
                self.flash(&format!(
                    "Replaced {} occurrence{}",
                    self.replace_count,
                    plural(self.replace_count)
                ));
            }
            _ => {
                self.replace = None;
                self.flash(&format!(
                    "Replaced {} occurrence{}",
                    self.replace_count,
                    plural(self.replace_count)
                ));
            }
        }
    }

    pub(crate) fn current_match_range(&self) -> Option<(Pos, Pos)> {
        let ms = self.bs().search_matches.as_ref()?;
        if ms.is_empty() || self.bs().search.query.is_empty() {
            return None;
        }
        let idx = self.bs().search.current.min(ms.len() - 1);
        let (m, len) = ms[idx];
        Some((
            m,
            Pos {
                row: m.row,
                col: m.col + len,
            },
        ))
    }
}
