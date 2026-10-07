//! The list overlay: one modal list over the text area, used by the buffer
//! list (M-L) and by find-usages (M-?). Both end the same way — Enter puts
//! the cursor somewhere — so they share the keys, the drawing and, for a
//! usage in another file, the jump-to-definition path (`goto_location`), which
//! is what makes M-, come back from a usage exactly as it does from a
//! definition.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::buffer::Pos;
use crate::editor::Editor;
use crate::lsp;

/// What choosing a row does.
#[derive(Debug, Clone, PartialEq)]
pub enum PickTarget {
    /// Make this buffer current.
    Buffer(usize),
    /// Jump here, as a definition jump would (pushes an M-, entry).
    Location(lsp::DefLocation),
}

#[derive(Debug, Clone, PartialEq)]
pub struct PickItem {
    pub label: String,
    pub target: PickTarget,
}

#[derive(Debug)]
pub struct Picker {
    /// The header row: what the list is.
    pub title: String,
    pub items: Vec<PickItem>,
    pub sel: usize,
    /// The cursor when the list was opened: where M-, returns to after a
    /// usage is chosen.
    pub from: Pos,
}

impl Picker {
    fn is_buffers(&self) -> bool {
        self.items
            .first()
            .is_some_and(|i| matches!(i.target, PickTarget::Buffer(_)))
    }
}

/// Rows of the overlay that list items: the text area minus the header.
pub(crate) fn list_rows(text_h: usize) -> usize {
    text_h.saturating_sub(1).max(1)
}

/// `path` relative to the working directory when it is under it — the form a
/// person reads a grep hit in — else as given.
fn display_path(path: &Path) -> String {
    if let Ok(cwd) = std::env::current_dir()
        && let Ok(rel) = path.strip_prefix(&cwd)
    {
        return rel.display().to_string();
    }
    path.display().to_string()
}

impl Editor {
    // ---------- M-L: buffer list ----------

    pub(crate) fn open_buffer_list(&mut self) {
        self.completion_close();
        let items = self.buffer_items();
        let from = self.bs().cursor;
        self.picker = Some(Picker {
            title: format!("Buffers ({})", items.len()),
            items,
            sel: self.cur,
            from,
        });
    }

    fn buffer_items(&self) -> Vec<PickItem> {
        (0..self.buffers.len())
            .map(|i| {
                let bs = &self.buffers[i];
                let mut label = format!("{:>3}  {}", i + 1, self.buffer_name(i));
                if bs.buf.modified {
                    label.push_str("  (modified)");
                }
                if bs.load.is_some() {
                    label.push_str("  (loading)");
                }
                PickItem {
                    label,
                    target: PickTarget::Buffer(i),
                }
            })
            .collect()
    }

    // ---------- M-?: find usages ----------

    /// Ask the language server for every reference to the symbol under the
    /// cursor and list them; Enter jumps to one (M-, comes back).
    pub(crate) fn find_usages(&mut self) {
        self.completion_close();
        let cur = self.bs().cursor;
        let bs = self.bs_mut();
        let Some(l) = bs.lsp.as_mut() else {
            self.flash("No LSP server");
            return;
        };
        // The position must match the server's view of the document.
        if bs.lsp_dirty {
            l.change(&bs.buf.text());
            bs.lsp_dirty = false;
            bs.lsp_last_send = Instant::now();
        }
        let uri = l.doc_uri.clone().unwrap_or_default();
        let line = bs.buf.row_opt(cur.row).cloned().unwrap_or_default();
        let locs = match l.references(
            &uri,
            cur.row as u64,
            lsp::utf16_col(&line, cur.col) as u64,
            Duration::from_secs(5),
        ) {
            Ok(x) => x,
            Err(e) => {
                self.flash(&e);
                return;
            }
        };
        self.show_usages(locs, cur);
    }

    /// Build the usages list from a server answer. Separate from the request
    /// so it can be tested without a language server.
    pub(crate) fn show_usages(&mut self, mut locs: Vec<lsp::DefLocation>, from: Pos) {
        if locs.is_empty() {
            self.flash("No usages found");
            return;
        }
        // Grouped by file, in reading order, with any duplicates gone:
        // servers differ in how they order references, and a list a person
        // scans should not depend on which server it came from.
        locs.sort_by(|a, b| (&a.uri, a.line, a.character).cmp(&(&b.uri, b.line, b.character)));
        locs.dedup();
        let here = self.bs().buf.name.as_deref().map(lsp::path_to_uri);
        let mut files: HashMap<String, Vec<String>> = HashMap::new();
        let mut items = Vec::with_capacity(locs.len());
        let mut sel = 0;
        for loc in locs {
            let path = lsp::uri_to_path(&loc.uri);
            let row = loc.line as usize;
            let text = self.line_of(&path, &loc.uri, row, &mut files);
            let col = lsp::utf16_to_char(&text.chars().collect::<Vec<_>>(), loc.character as usize);
            // Start on the usage the cursor is on, so Down/Up read as "next /
            // previous usage".
            if here.as_deref() == Some(loc.uri.as_str()) && row == from.row {
                sel = items.len();
            }
            items.push(PickItem {
                label: format!(
                    "{}:{}:{}  {}",
                    display_path(&path),
                    row + 1,
                    col + 1,
                    text.trim()
                ),
                target: PickTarget::Location(loc),
            });
        }
        let n = items.len();
        let word = self.word_at(from);
        self.picker = Some(Picker {
            title: match word {
                Some(w) => format!("{n} usage{} of {w}", if n == 1 { "" } else { "s" }),
                None => format!("{n} usage{}", if n == 1 { "" } else { "s" }),
            },
            items,
            sel,
            from,
        });
    }

    /// The text of `row` in `path`: from its open buffer when that has it
    /// (so unsaved edits show), otherwise from disk, read once per file.
    fn line_of(
        &self,
        path: &Path,
        uri: &str,
        row: usize,
        files: &mut HashMap<String, Vec<String>>,
    ) -> String {
        if let Some(i) = self.find_buffer(path) {
            let bs = &self.buffers[i];
            if bs.load.is_none() && bs.pending_load.is_none() {
                return bs
                    .buf
                    .row_opt(row)
                    .map(|l| l.iter().collect())
                    .unwrap_or_default();
            }
        }
        let lines = files.entry(uri.to_string()).or_insert_with(|| {
            std::fs::read(PathBuf::from(path))
                .map(|b| {
                    String::from_utf8_lossy(&b)
                        .lines()
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default()
        });
        lines.get(row).cloned().unwrap_or_default()
    }

    /// The identifier under (or just before) `p`, for the list's title.
    fn word_at(&self, p: Pos) -> Option<String> {
        let line = self.bs().buf.row_opt(p.row)?;
        let is_word = |c: &char| c.is_alphanumeric() || *c == '_';
        let mut s = p.col.min(line.len());
        if (s == line.len() || !is_word(&line[s])) && s > 0 && is_word(&line[s - 1]) {
            s -= 1;
        }
        if s >= line.len() || !is_word(&line[s]) {
            return None;
        }
        let start = line[..s]
            .iter()
            .rposition(|c| !is_word(c))
            .map_or(0, |i| i + 1);
        let end = line[s..]
            .iter()
            .position(|c| !is_word(c))
            .map_or(line.len(), |i| s + i);
        Some(line[start..end].iter().collect())
    }

    // ---------- keys ----------

    /// Keys while the list is open. Returns with the list closed (Enter,
    /// Esc) or still open (motion).
    pub(crate) fn handle_picker_key(&mut self, mut p: Picker, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let n = p.items.len();
        let page = list_rows(self.text_h);
        match key.code {
            KeyCode::Esc => return,
            KeyCode::Char('g' | 'c' | 'x') if ctrl => return,
            // The key that opened the list closes it again.
            KeyCode::Char('l' | '?') if alt => return,
            KeyCode::Enter => {
                self.picker_accept(p);
                return;
            }
            KeyCode::Up => p.sel = p.sel.saturating_sub(1),
            KeyCode::Char('p') if ctrl => p.sel = p.sel.saturating_sub(1),
            KeyCode::Down => p.sel = (p.sel + 1).min(n.saturating_sub(1)),
            KeyCode::Char('n') if ctrl => p.sel = (p.sel + 1).min(n.saturating_sub(1)),
            KeyCode::PageUp => p.sel = p.sel.saturating_sub(page),
            KeyCode::PageDown => p.sel = (p.sel + page).min(n.saturating_sub(1)),
            KeyCode::Home => p.sel = 0,
            KeyCode::End => p.sel = n.saturating_sub(1),
            // Buffer list only: close the selected buffer from the list.
            KeyCode::Delete if p.is_buffers() => {
                let PickTarget::Buffer(i) = p.items[p.sel].target else {
                    return;
                };
                self.cur = i;
                self.highlight_dirty = true;
                self.close_buffer();
                // A modified buffer is now asking whether to save: that
                // prompt replaces the list.
                if self.prompt.is_some() || self.buffers.len() == n {
                    return;
                }
                p.items = self.buffer_items();
                p.title = format!("Buffers ({})", p.items.len());
                p.sel = p.sel.min(p.items.len() - 1);
            }
            _ => {}
        }
        self.picker = Some(p);
    }

    fn picker_accept(&mut self, p: Picker) {
        let Some(item) = p.items.into_iter().nth(p.sel) else {
            return;
        };
        match item.target {
            PickTarget::Buffer(i) => self.set_current(i),
            PickTarget::Location(loc) => self.goto_location(loc, p.from),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::BufferState;
    use crate::buffer::Buffer;
    use crate::config;
    use crossterm::event::KeyEvent;

    fn buf(text: &str, name: Option<&Path>) -> Buffer {
        let mut b = Buffer::new();
        b.set_rows(text.lines().map(|l| l.chars().collect()).collect());
        b.name = name.map(Path::to_path_buf);
        b
    }

    fn press(ed: &mut Editor, code: KeyCode, mods: KeyModifiers) {
        ed.handle_key(KeyEvent::new(code, mods));
    }

    fn lines(ed: &Editor) -> Vec<String> {
        ed.bs()
            .buf
            .rows()
            .map(|l| l.iter().collect::<String>())
            .collect()
    }

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("rano_picker_{tag}_{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn three_buffers() -> Editor {
        let cfg = config::Config {
            multibuffer: true,
            ..config::Config::default()
        };
        let mut ed = Editor::new(buf("a", Some(Path::new("/tmp/rano_pk_a.txt"))), cfg);
        ed.buffers.push(BufferState::new(buf(
            "b",
            Some(Path::new("/tmp/rano_pk_b.txt")),
        )));
        ed.buffers.push(BufferState::new(buf(
            "c",
            Some(Path::new("/tmp/rano_pk_c.txt")),
        )));
        ed
    }

    #[test]
    fn buffer_list_starts_on_the_current_buffer_and_switches() {
        let mut ed = three_buffers();
        ed.cur = 1;
        press(&mut ed, KeyCode::Char('l'), KeyModifiers::ALT);
        let p = ed.picker.as_ref().expect("M-L opens the list");
        assert_eq!(p.items.len(), 3);
        assert_eq!(p.sel, 1);
        assert!(p.items[2].label.contains("rano_pk_c.txt"));
        press(&mut ed, KeyCode::Down, KeyModifiers::NONE);
        press(&mut ed, KeyCode::Enter, KeyModifiers::NONE);
        assert!(ed.picker.is_none());
        assert_eq!(ed.cur, 2);
        assert_eq!(lines(&ed), vec!["c"]);
    }

    #[test]
    fn buffer_list_escape_changes_nothing() {
        let mut ed = three_buffers();
        press(&mut ed, KeyCode::Char('l'), KeyModifiers::ALT);
        press(&mut ed, KeyCode::End, KeyModifiers::NONE);
        press(&mut ed, KeyCode::Esc, KeyModifiers::NONE);
        assert!(ed.picker.is_none());
        assert_eq!(ed.cur, 0);
    }

    #[test]
    fn buffer_list_marks_modified_buffers() {
        let mut ed = three_buffers();
        ed.buffers[2].buf.modified = true;
        ed.open_buffer_list();
        let p = ed.picker.as_ref().unwrap();
        assert!(p.items[2].label.ends_with("(modified)"));
        assert!(!p.items[0].label.contains("(modified)"));
    }

    #[test]
    fn delete_in_the_buffer_list_closes_the_selected_buffer() {
        let mut ed = three_buffers();
        ed.open_buffer_list();
        press(&mut ed, KeyCode::Down, KeyModifiers::NONE);
        press(&mut ed, KeyCode::Delete, KeyModifiers::NONE);
        assert_eq!(ed.buffers.len(), 2);
        let p = ed.picker.as_ref().expect("the list stays open");
        assert_eq!(p.items.len(), 2);
        assert!(p.items[1].label.contains("rano_pk_c.txt"));
        assert!(
            p.items[1].label.starts_with("  2"),
            "renumbered: {}",
            p.items[1].label
        );
    }

    #[test]
    fn usages_list_jumps_across_files_and_back() {
        let d = tmp("usages");
        let a = d.join("a.rs");
        let b = d.join("b.rs");
        std::fs::write(&a, "fn foo() {}\nfn main() { foo(); }\n").unwrap();
        std::fs::write(&b, "fn bar() {\n    crate::foo();\n}\n").unwrap();
        let cfg = config::Config {
            multibuffer: true,
            ..config::Config::default()
        };
        let mut ed = Editor::new(Buffer::from_file(&a).unwrap(), cfg);
        let from = Pos { row: 0, col: 4 };
        ed.bs_mut().cursor = from;
        let loc = |p: &Path, line, character| lsp::DefLocation {
            uri: lsp::path_to_uri(p),
            line,
            character,
        };
        // Out of order and with a duplicate, as a server may send them.
        ed.show_usages(
            vec![loc(&b, 1, 11), loc(&a, 1, 12), loc(&a, 0, 3), loc(&a, 0, 3)],
            from,
        );
        let p = ed.picker.as_ref().expect("usages open the list");
        assert_eq!(p.title, "3 usages of foo");
        assert_eq!(p.items.len(), 3, "the duplicate is gone");
        assert!(
            p.items[0].label.ends_with("a.rs:1:4  fn foo() {}"),
            "{}",
            p.items[0].label
        );
        assert!(
            p.items[2].label.ends_with("b.rs:2:12  crate::foo();"),
            "{}",
            p.items[2].label
        );
        assert_eq!(p.sel, 0, "starts on the usage under the cursor");
        press(&mut ed, KeyCode::End, KeyModifiers::NONE);
        press(&mut ed, KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(ed.cur, 1, "b.rs opened as a buffer");
        assert_eq!(ed.bs().cursor, Pos { row: 1, col: 11 });
        // Asking again and choosing b.rs reuses its buffer.
        ed.show_usages(vec![loc(&b, 1, 11)], Pos { row: 1, col: 11 });
        press(&mut ed, KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(ed.buffers.len(), 2, "no second copy of b.rs");
        // M-, unwinds both jumps, back to where the first list was opened.
        press(&mut ed, KeyCode::Char(','), KeyModifiers::ALT);
        press(&mut ed, KeyCode::Char(','), KeyModifiers::ALT);
        assert_eq!(ed.cur, 0);
        assert_eq!(ed.bs().cursor, from);
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn usages_show_unsaved_text_from_an_open_buffer() {
        let d = tmp("unsaved");
        let a = d.join("a.rs");
        std::fs::write(&a, "fn foo() {}\n").unwrap();
        let mut ed = Editor::new(Buffer::from_file(&a).unwrap(), config::Config::default());
        ed.bs_mut()
            .buf
            .set_rows(vec!["fn foo() { /* edited */ }".chars().collect()]);
        ed.show_usages(
            vec![lsp::DefLocation {
                uri: lsp::path_to_uri(&a),
                line: 0,
                character: 3,
            }],
            Pos { row: 0, col: 3 },
        );
        let p = ed.picker.as_ref().unwrap();
        assert!(
            p.items[0].label.contains("/* edited */"),
            "{}",
            p.items[0].label
        );
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn no_usages_flashes_and_opens_nothing() {
        let mut ed = Editor::new(buf("x", None), config::Config::default());
        ed.show_usages(Vec::new(), Pos { row: 0, col: 0 });
        assert!(ed.picker.is_none());
        assert_eq!(ed.status_text(), Some("No usages found".to_string()));
    }

    #[test]
    fn find_usages_without_a_server_says_so() {
        let mut ed = Editor::new(buf("x", None), config::Config::default());
        press(&mut ed, KeyCode::Char('?'), KeyModifiers::ALT);
        assert!(ed.picker.is_none());
        assert_eq!(ed.status_text(), Some("No LSP server".to_string()));
    }
}
