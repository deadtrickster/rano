//! The editor's behaviour tests: keys in, buffer and screen out.
//!
//! They lived in the binary's `main.rs` while the editor did; they test the
//! library now, so they moved with it. The command-line tests stayed behind,
//! because the command line is still the binary's.

use crate::BufferState;
use crate::buffer::Buffer;
use crate::buffer::Pos;
use crate::config;
use crate::editor::Editor;
use crate::editor::{DefBack, Flash};
use crate::prompt::{PromptKind, complete_path, expand_tilde};
use crate::{editor, lsp, ui};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::style::{Color, Modifier, Style};
use std::fs;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};

fn test_ed(text: &str) -> Editor {
    let mut buf = Buffer::new();
    buf.set_rows(text.lines().map(|l| l.chars().collect()).collect());
    if buf.rows_is_empty() {
        buf.push_row(Vec::new());
    }
    let mut ed = Editor::new(buf, config::Config::default());
    ed.text_w = 80;
    ed.text_h = 24;
    ed
}

fn press(ed: &mut Editor, code: KeyCode, mods: KeyModifiers) {
    ed.handle_key(KeyEvent::new(code, mods));
}

fn me(kind: MouseEventKind, row: u16, col: u16) -> MouseEvent {
    MouseEvent {
        kind,
        column: col,
        row,
        modifiers: KeyModifiers::NONE,
    }
}

fn lines(ed: &Editor) -> Vec<String> {
    ed.bs().buf.rows().map(|l| l.iter().collect()).collect()
}

struct TempDir(PathBuf);
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn temp_dir(tag: &str) -> TempDir {
    let d = std::env::temp_dir().join(format!(
        "rano_ed_{}_{}_{:?}",
        tag,
        std::process::id(),
        std::time::Instant::now()
    ));
    fs::create_dir_all(&d).unwrap();
    TempDir(d)
}

// B2 — move_left at BOL must land on the END of the previous row.

#[test]
fn move_left_bol_clamps_to_prev_row_end() {
    let mut ed = test_ed("ab\ncdef");
    ed.bs_mut().cursor = Pos { row: 1, col: 0 };
    press(&mut ed, KeyCode::Left, KeyModifiers::NONE);
    assert_eq!(ed.bs().cursor, Pos { row: 0, col: 2 });
    press(&mut ed, KeyCode::Backspace, KeyModifiers::NONE);
    assert_eq!(lines(&ed), vec!["a", "cdef"]);
}

// B1 — prompt editing is char-indexed; multibyte text must not panic.

#[test]
fn prompt_multibyte_backspace_no_panic() {
    let mut ed = test_ed("");
    press(&mut ed, KeyCode::Char('f'), KeyModifiers::CONTROL);
    assert!(ed.prompt.is_some());
    press(&mut ed, KeyCode::Char('é'), KeyModifiers::NONE);
    press(&mut ed, KeyCode::Char('a'), KeyModifiers::NONE);
    press(&mut ed, KeyCode::Backspace, KeyModifiers::NONE);
    let p = ed.prompt.as_ref().unwrap();
    assert_eq!(p.text, "é");
    assert_eq!(p.cursor, 1);
}

#[test]
fn prompt_multibyte_insert_no_panic() {
    let mut ed = test_ed("");
    press(&mut ed, KeyCode::Char('f'), KeyModifiers::CONTROL);
    press(&mut ed, KeyCode::Char('é'), KeyModifiers::NONE);
    press(&mut ed, KeyCode::Char('x'), KeyModifiers::NONE);
    let p = ed.prompt.as_ref().unwrap();
    assert_eq!(p.text, "éx");
    assert_eq!(p.cursor, 2);
}

#[test]
fn prompt_mid_string_multibyte_edit() {
    let mut ed = test_ed("");
    press(&mut ed, KeyCode::Char('f'), KeyModifiers::CONTROL);
    press(&mut ed, KeyCode::Char('é'), KeyModifiers::NONE);
    press(&mut ed, KeyCode::Char('a'), KeyModifiers::NONE);
    press(&mut ed, KeyCode::Left, KeyModifiers::NONE);
    press(&mut ed, KeyCode::Char('x'), KeyModifiers::NONE);
    let p = ed.prompt.as_ref().unwrap();
    assert_eq!(p.text, "éxa");
    assert_eq!(p.cursor, 2);
}

#[test]
fn prompt_ascii_edit_still_works() {
    let mut ed = test_ed("");
    press(&mut ed, KeyCode::Char('f'), KeyModifiers::CONTROL);
    for c in ['a', 'b', 'c'] {
        press(&mut ed, KeyCode::Char(c), KeyModifiers::NONE);
    }
    press(&mut ed, KeyCode::Backspace, KeyModifiers::NONE);
    press(&mut ed, KeyCode::Left, KeyModifiers::NONE);
    press(&mut ed, KeyCode::Char('x'), KeyModifiers::NONE);
    let p = ed.prompt.as_ref().unwrap();
    assert_eq!(p.text, "axb");
    assert_eq!(p.cursor, 2);
    assert!(ed.prompt.is_some());
}

// C3 — ^C cursor position flashes for 2 s, then expires.

#[test]
fn show_loc_expires() {
    let mut ed = test_ed("hi");
    press(&mut ed, KeyCode::Char('c'), KeyModifiers::CONTROL);
    assert_eq!(ed.status_text(), Some("Line 1, Col 1".to_string()));
    ed.loc_until = Some(Instant::now() - Duration::from_secs(1));
    ed.tick_status();
    assert_eq!(ed.status_text(), None);
}

#[test]
fn goto_still_shows_loc() {
    let mut ed = test_ed("a\nb\nc");
    press(&mut ed, KeyCode::Char('7'), KeyModifiers::CONTROL);
    press(&mut ed, KeyCode::Char('3'), KeyModifiers::NONE);
    press(&mut ed, KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(ed.bs().cursor, Pos { row: 2, col: 0 });
    let st = ed.status_text().unwrap();
    assert!(st.contains("Line 3"), "status: {st}");
}

#[test]
fn prompt_reopen_multibyte_query_cursor_ok() {
    // ^F re-opens the prompt only when the last search had no matches,
    // so search for something absent; the seeded cursor must be a CHAR
    // index (byte length would start past EOL for "éé").
    let mut ed = test_ed("abc");
    press(&mut ed, KeyCode::Char('f'), KeyModifiers::CONTROL);
    for c in "éé".chars() {
        press(&mut ed, KeyCode::Char(c), KeyModifiers::NONE);
    }
    press(&mut ed, KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(ed.bs().search.query, "éé");
    press(&mut ed, KeyCode::Char('f'), KeyModifiers::CONTROL);
    let p = ed.prompt.as_ref().unwrap();
    assert_eq!(p.text, "éé");
    assert_eq!(p.cursor, 2);
}

// D2 — sort: case-insensitive, region-scoped when marked.

#[test]
fn sort_lines_case_insensitive_and_region() {
    let mut ed = test_ed("b\nA\nc\na\nB");
    press(&mut ed, KeyCode::Char('a'), KeyModifiers::ALT);
    press(&mut ed, KeyCode::Down, KeyModifiers::NONE);
    press(&mut ed, KeyCode::Down, KeyModifiers::NONE);
    press(&mut ed, KeyCode::F(9), KeyModifiers::NONE);
    assert_eq!(lines(&ed), vec!["A", "b", "c", "a", "B"]);
    assert!(ed.bs().mark.is_none());
    let mut ed = test_ed("B\na\nA\nb");
    press(&mut ed, KeyCode::F(9), KeyModifiers::NONE);
    assert_eq!(lines(&ed), vec!["a", "A", "B", "b"]);
}

// C1 — save_to preserves the buffer's original CRLF line endings.

#[test]
fn save_preserves_crlf() {
    let d = temp_dir("crlf");
    let src = d.0.join("in.txt");
    fs::write(&src, "a\r\nb\r\n").unwrap();
    let buf = Buffer::from_file(&src).unwrap();
    assert!(buf.crlf);
    let mut ed = Editor::new(buf, config::Config::default());
    let out = d.0.join("out.txt");
    ed.save_to(out.clone());
    let bytes = fs::read(&out).unwrap();
    assert_eq!(bytes, b"a\r\nb\r\n");
    assert!(!ed.bs().buf.modified);
    assert_eq!(ed.bs().buf.name, Some(out));
}

// Saving over a file something else changed since it was read.

/// A file read into an editor, then rewritten on disk behind its back.
fn externally_changed(tag: &str) -> (TempDir, PathBuf, Editor) {
    let d = temp_dir(tag);
    let f = d.0.join("f.txt");
    fs::write(&f, "one\ntwo\n").unwrap();
    let mut ed = Editor::new(Buffer::from_file(&f).unwrap(), config::Config::default());
    press_text(&mut ed, "X");
    fs::write(&f, "one\ntwo\nthree from elsewhere\n").unwrap();
    (d, f, ed)
}

fn prompt_kind(ed: &Editor) -> Option<PromptKind> {
    ed.prompt.as_ref().map(|p| p.kind)
}

#[test]
fn save_of_an_unchanged_file_does_not_ask() {
    let d = temp_dir("ext_same");
    let f = d.0.join("f.txt");
    fs::write(&f, "one\n").unwrap();
    let mut ed = Editor::new(Buffer::from_file(&f).unwrap(), config::Config::default());
    press_text(&mut ed, "X");
    // ^O on the buffer's own file is a save, not "File exists".
    ed.do_write(f.display().to_string());
    assert_eq!(prompt_kind(&ed), None);
    assert_eq!(fs::read_to_string(&f).unwrap(), "Xone\n");
}

#[test]
fn save_asks_when_the_file_changed_on_disk() {
    let (_d, f, mut ed) = externally_changed("ext_ask");
    ed.save_to(f.clone());
    assert_eq!(prompt_kind(&ed), Some(PromptKind::ConfirmExternal));
    assert_eq!(
        fs::read_to_string(&f).unwrap(),
        "one\ntwo\nthree from elsewhere\n"
    );
    // y writes, and the next save is quiet: the stamp is the new file's.
    press(&mut ed, KeyCode::Char('y'), KeyModifiers::NONE);
    assert_eq!(fs::read_to_string(&f).unwrap(), "Xone\ntwo\n");
    press_text(&mut ed, "Y");
    ed.save_to(f.clone());
    assert_eq!(prompt_kind(&ed), None);
    assert_eq!(fs::read_to_string(&f).unwrap(), "XYone\ntwo\n");
}

#[test]
fn no_to_the_external_change_keeps_the_file_and_disarms_quit() {
    let (_d, f, mut ed) = externally_changed("ext_no");
    press(&mut ed, KeyCode::Char('x'), KeyModifiers::CONTROL);
    press(&mut ed, KeyCode::Char('y'), KeyModifiers::NONE);
    assert_eq!(prompt_kind(&ed), Some(PromptKind::ConfirmExternal));
    press(&mut ed, KeyCode::Char('n'), KeyModifiers::NONE);
    assert!(!ed.quit);
    assert!(!ed.quit_after_save);
    assert!(ed.bs().buf.modified);
    assert_eq!(
        fs::read_to_string(&f).unwrap(),
        "one\ntwo\nthree from elsewhere\n"
    );
}

#[test]
fn d_shows_the_diff_and_esc_comes_back_to_the_question() {
    let (_d, f, mut ed) = externally_changed("ext_diff");
    ed.text_w = 100;
    ed.text_h = 20;
    ed.save_to(f.clone());
    press(&mut ed, KeyCode::Char('d'), KeyModifiers::NONE);
    let text = |ed: &Editor| -> Vec<String> {
        ed.diff_view
            .as_ref()
            .expect("diff view")
            .lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_str()).collect())
            .collect()
    };
    // Unified first (the session's default): the file's name, the hunk, and
    // each line numbered in its own file.
    let rows = text(&ed);
    assert!(rows[0].ends_with("f.txt"), "{rows:?}");
    assert!(rows[1].starts_with("@@"), "{rows:?}");
    assert!(rows.contains(&"1 -one".to_string()), "{rows:?}");
    assert!(rows.contains(&"1 +Xone".to_string()), "{rows:?}");
    assert!(
        rows.contains(&"3 -three from elsewhere".to_string()),
        "{rows:?}"
    );
    // s: two panels, the disk's line on the left and the buffer's on the right.
    press(&mut ed, KeyCode::Char('s'), KeyModifiers::NONE);
    assert!(ed.diff_split, "the choice is remembered");
    let rows = text(&ed);
    let pair = rows.iter().find(|r| r.contains("Xone")).expect("the pair");
    let (left, right) = pair.split_once('│').expect("two panels");
    assert!(left.contains("- one"), "{pair:?}");
    assert!(right.contains("+ Xone"), "{pair:?}");
    // A resize re-renders at the new width.
    ed.text_w = 60;
    assert!(ed.refresh_diff_view());
    assert!(!ed.refresh_diff_view());
    // Drawn over the text, with the header naming the view.
    let backend = ratatui::backend::TestBackend::new(60, 12);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    ed.text_h = 8;
    term.draw(|fr| ui::draw(fr, &ed)).unwrap();
    let buf = term.backend().buffer().clone();
    let row = |y: u16| -> String { (0..60).map(|x| buf[(x, y)].symbol().to_string()).collect() };
    assert!(row(1).contains("Saving would change"), "{}", row(1));
    assert!((2..9).any(|y| row(y).contains("Xone")));
    press(&mut ed, KeyCode::Esc, KeyModifiers::NONE);
    assert!(ed.diff_view.is_none());
    assert_eq!(prompt_kind(&ed), Some(PromptKind::ConfirmExternal));
    // y from the diff itself answers the question.
    press(&mut ed, KeyCode::Char('d'), KeyModifiers::NONE);
    assert!(ed.diff_view.is_some());
    press(&mut ed, KeyCode::Char('y'), KeyModifiers::NONE);
    assert!(ed.diff_view.is_none());
    assert_eq!(fs::read_to_string(&f).unwrap(), "Xone\ntwo\n");
}

fn diff_view_text(ed: &Editor) -> Vec<String> {
    ed.diff_view
        .as_ref()
        .expect("diff view")
        .lines
        .iter()
        .map(|l| l.spans.iter().map(|s| s.content.as_str()).collect())
        .collect()
}

#[test]
fn m_p_previews_a_patch_buffer_and_closes_again() {
    let mut ed = test_ed(
        "diff --git a/src/a.rs b/src/a.rs\n--- a/src/a.rs\n+++ b/src/a.rs\n@@ -7,3 +7,3 @@\n fn a() {\n-    old();\n+    new();\n }",
    );
    ed.bs_mut().buf.name = Some(PathBuf::from("/tmp/rano_preview.patch"));
    ed.text_w = 100;
    ed.text_h = 20;
    press(&mut ed, KeyCode::Char('p'), KeyModifiers::ALT);
    let rows = diff_view_text(&ed);
    assert!(rows.contains(&"src/a.rs".to_string()), "{rows:?}");
    assert!(rows.contains(&"8 -    old();".to_string()), "{rows:?}");
    assert!(rows.contains(&"8 +    new();".to_string()), "{rows:?}");
    assert!(ed.diff_view.as_ref().unwrap().header().contains("Patch"));
    // y and n are not answers here; M-P closes and leaves no prompt.
    press(&mut ed, KeyCode::Char('y'), KeyModifiers::NONE);
    assert!(ed.diff_view.is_some());
    press(&mut ed, KeyCode::Char('p'), KeyModifiers::ALT);
    assert!(ed.diff_view.is_none());
    assert!(ed.prompt.is_none());
    // The text is untouched: the preview is a view, not an edit.
    assert_eq!(lines(&ed)[0], "diff --git a/src/a.rs b/src/a.rs");
}

#[test]
fn m_p_shows_merge_conflicts_ours_against_theirs() {
    let mut ed = test_ed(
        "fn main() {\n<<<<<<< HEAD\n    let total = 1;\n=======\n    let sum = 1;\n>>>>>>> feature\n}",
    );
    ed.bs_mut().buf.name = Some(PathBuf::from("/tmp/rano_conflict.rs"));
    ed.text_w = 100;
    ed.text_h = 20;
    ed.diff_split = true;
    press(&mut ed, KeyCode::Char('p'), KeyModifiers::ALT);
    let rows = diff_view_text(&ed);
    assert!(rows[0].starts_with("1 conflict"), "{rows:?}");
    let pair = rows
        .iter()
        .find(|r| r.contains("let total"))
        .expect("{rows:?}");
    let (left, right) = pair.split_once('│').expect("two panels");
    assert!(
        left.contains("let total") && right.contains("let sum"),
        "{pair:?}"
    );
    assert!(
        ed.diff_view
            .as_ref()
            .unwrap()
            .header()
            .contains("Conflict 1/1")
    );
    press(&mut ed, KeyCode::Esc, KeyModifiers::NONE);
    assert!(ed.diff_view.is_none());
}

const CONFLICTED: &str = "fn main() {\n<<<<<<< HEAD\n    let total = 1;\n=======\n    let sum = 1;\n>>>>>>> feature\n    mid();\n<<<<<<< HEAD\n    one();\n||||||| base\n    zero();\n=======\n    two();\n>>>>>>> feature\n}";

fn conflict_ed() -> Editor {
    let mut ed = test_ed(CONFLICTED);
    ed.bs_mut().buf.name = Some(PathBuf::from("/tmp/rano_resolve.rs"));
    ed.text_w = 120;
    ed.text_h = 30;
    press(&mut ed, KeyCode::Char('p'), KeyModifiers::ALT);
    assert!(ed.diff_view.is_some());
    ed
}

#[test]
fn the_conflict_view_moves_between_conflicts_and_compares_with_the_base() {
    let mut ed = conflict_ed();
    // A short screen, so moving to a conflict has to scroll.
    ed.text_h = 6;
    assert!(
        ed.diff_view
            .as_ref()
            .unwrap()
            .header()
            .contains("Conflict 1/2")
    );
    press(&mut ed, KeyCode::Char('n'), KeyModifiers::NONE);
    let v = ed.diff_view.as_ref().unwrap();
    assert!(v.header().contains("Conflict 2/2"), "{}", v.header());
    // Scrolled to it: the first row shown is its header, marked current.
    let first: String = v.lines[v.top]
        .spans
        .iter()
        .map(|s| s.content.as_str())
        .collect();
    assert!(first.starts_with("▶ Conflict 2 of 2"), "{first:?}");
    // Past the last one it stays.
    press(&mut ed, KeyCode::Char('n'), KeyModifiers::NONE);
    assert!(
        ed.diff_view
            .as_ref()
            .unwrap()
            .header()
            .contains("Conflict 2/2")
    );
    // c: base against ours shows the base's line.
    press(&mut ed, KeyCode::Char('c'), KeyModifiers::NONE);
    let rows = diff_view_text(&ed);
    assert!(
        ed.diff_view
            .as_ref()
            .unwrap()
            .header()
            .contains("base/ours")
    );
    assert!(rows.iter().any(|r| r.contains("zero()")), "{rows:?}");
    press(&mut ed, KeyCode::Char('p'), KeyModifiers::NONE);
    assert!(
        ed.diff_view
            .as_ref()
            .unwrap()
            .header()
            .contains("Conflict 1/2")
    );
}

#[test]
fn taking_a_side_resolves_one_conflict_as_one_undo_step() {
    let mut ed = conflict_ed();
    press(&mut ed, KeyCode::Char('t'), KeyModifiers::NONE);
    assert_eq!(
        lines(&ed)[..3],
        ["fn main() {", "    let sum = 1;", "    mid();"],
        "{:?}",
        lines(&ed)
    );
    // One left, and it is now current.
    let v = ed.diff_view.as_ref().expect("still open");
    assert!(v.header().contains("Conflict 1/1"), "{}", v.header());
    assert!(ed.status_text().unwrap().contains("1 conflict left"));
    // b: both, ours first — the last one, so the view closes.
    press(&mut ed, KeyCode::Char('b'), KeyModifiers::NONE);
    assert!(ed.diff_view.is_none());
    assert!(ed.status_text().unwrap().contains("All conflicts resolved"));
    assert_eq!(
        lines(&ed),
        vec![
            "fn main() {",
            "    let sum = 1;",
            "    mid();",
            "    one();",
            "    two();",
            "}"
        ]
    );
    // Each take is one undo step.
    press(&mut ed, KeyCode::Char('u'), KeyModifiers::ALT);
    assert!(lines(&ed).contains(&"<<<<<<< HEAD".to_string()));
    assert!(lines(&ed).contains(&"    let sum = 1;".to_string()));
    press(&mut ed, KeyCode::Char('u'), KeyModifiers::ALT);
    assert_eq!(lines(&ed).join("\n"), CONFLICTED);
}

// ---- keymaps, M-x, help pages, which-key ----

fn screen(ed: &Editor, w: u16, h: u16) -> Vec<String> {
    let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(w, h)).unwrap();
    term.draw(|f| ui::draw(f, ed)).unwrap();
    let buf = term.backend().buffer().clone();
    (0..h)
        .map(|y| (0..w).map(|x| buf[(x, y)].symbol().to_string()).collect())
        .collect()
}

#[test]
fn a_prefix_waits_for_its_next_key_and_esc_abandons_it() {
    let mut ed = test_ed("- [ ] task");
    press(&mut ed, KeyCode::Char('t'), KeyModifiers::ALT);
    assert_eq!(ed.pending.keys.len(), 1, "M-t is a prefix");
    assert!(ed.pending_card().is_none(), "its card waits a moment");
    assert!(ed.card_due().is_some());
    // Held still: the card shows.
    ed.pending.since = Some(Instant::now() - Duration::from_secs(1));
    let (m, _) = ed.pending_card().expect("card");
    assert_eq!(m.name, "todo");
    let rows = screen(&ed, 80, 16);
    assert!(rows.iter().any(|r| r.contains("todo M-t-")), "{rows:#?}");
    assert!(
        rows.iter()
            .any(|r| r.contains("Tick") && r.contains("Decline")),
        "{rows:#?}"
    );
    // The bar shows what can follow, too.
    assert!(ed.bar_items().iter().any(|(k, t)| k == "t" && t == "Tick"));
    press(&mut ed, KeyCode::Char('t'), KeyModifiers::NONE);
    assert!(ed.pending.keys.is_empty());
    assert_eq!(lines(&ed)[0], "- [x] task", "M-t t ticked it");
    // ESC abandons a prefix; an unbound key says so.
    press(&mut ed, KeyCode::Char('t'), KeyModifiers::ALT);
    press(&mut ed, KeyCode::Esc, KeyModifiers::NONE);
    assert_eq!(ed.status_text().as_deref(), Some("Quit"));
    press(&mut ed, KeyCode::Char('t'), KeyModifiers::ALT);
    press(&mut ed, KeyCode::Char('q'), KeyModifiers::NONE);
    assert_eq!(ed.status_text().as_deref(), Some("M-t q is undefined"));
    assert_eq!(lines(&ed)[0], "- [x] task", "nothing was typed");
}

#[test]
fn the_help_key_shows_its_card_at_once_and_opens_pages() {
    let mut ed = test_ed("text");
    ed.text_w = 100;
    ed.text_h = 30;
    press(&mut ed, KeyCode::Char('g'), KeyModifiers::CONTROL);
    assert!(ed.pending_card().is_some(), "help shows at once");
    press(&mut ed, KeyCode::Char('b'), KeyModifiers::NONE);
    let v = ed.info.as_ref().expect("the bindings page");
    let text: Vec<String> = v
        .lines
        .iter()
        .map(|l| l.spans.iter().map(|s| s.content.as_str()).collect())
        .collect();
    assert!(
        text.iter()
            .any(|r| r.contains("C-o") && r.contains("Write Out")),
        "{text:#?}"
    );
    assert!(
        text.iter()
            .any(|r| r.contains("M-t t") && r.contains("Tick")),
        "{text:#?}"
    );
    assert!(
        text.iter()
            .any(|r| r.contains("Conflict view") || r.contains("Patch and conflict"))
    );
    // A page's own keys, and q closes it; typing does not reach the text.
    press(&mut ed, KeyCode::Char('z'), KeyModifiers::NONE);
    assert_eq!(lines(&ed), vec!["text"]);
    press(&mut ed, KeyCode::Char('q'), KeyModifiers::NONE);
    assert!(ed.info.is_none());
    // C-g C-g: the overview.
    press(&mut ed, KeyCode::Char('g'), KeyModifiers::CONTROL);
    press(&mut ed, KeyCode::Char('g'), KeyModifiers::CONTROL);
    assert_eq!(ed.info.as_ref().unwrap().title, "Help");
}

#[test]
fn describe_key_says_what_a_key_runs_even_through_a_prefix() {
    let mut ed = test_ed("text");
    let page = |ed: &Editor| -> String {
        ed.info
            .as_ref()
            .unwrap()
            .lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_str())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    press(&mut ed, KeyCode::Char('g'), KeyModifiers::CONTROL);
    press(&mut ed, KeyCode::Char('k'), KeyModifiers::NONE);
    press(&mut ed, KeyCode::Char('k'), KeyModifiers::CONTROL);
    let p = page(&ed);
    assert!(p.contains("C-k runs Cut") && p.contains("(cut)"), "{p}");
    assert_eq!(lines(&ed), vec!["text"], "described, not run");
    press(&mut ed, KeyCode::Esc, KeyModifiers::NONE);
    press(&mut ed, KeyCode::Char('g'), KeyModifiers::CONTROL);
    press(&mut ed, KeyCode::Char('k'), KeyModifiers::NONE);
    press(&mut ed, KeyCode::Char('t'), KeyModifiers::ALT);
    press(&mut ed, KeyCode::Char('x'), KeyModifiers::NONE);
    assert!(page(&ed).contains("M-t x runs Decline"), "{}", page(&ed));
    press(&mut ed, KeyCode::Esc, KeyModifiers::NONE);
    press(&mut ed, KeyCode::Char('g'), KeyModifiers::CONTROL);
    press(&mut ed, KeyCode::Char('k'), KeyModifiers::NONE);
    press(&mut ed, KeyCode::Char('z'), KeyModifiers::CONTROL);
    assert!(page(&ed).contains("C-z is not bound"), "{}", page(&ed));
}

#[test]
fn m_x_runs_a_command_by_name_and_offers_recent_ones_first() {
    let mut ed = test_ed("text");
    press(&mut ed, KeyCode::Char('x'), KeyModifiers::ALT);
    assert!(ed.palette.is_some());
    press_text(&mut ed, "wri out");
    assert_eq!(lines(&ed), vec!["text"], "the query is not the text");
    assert_eq!(ed.palette.as_ref().unwrap().items[0], "write-out");
    let rows = screen(&ed, 100, 20);
    assert!(
        rows.iter().any(|r| r.starts_with("M-x wri out")),
        "{rows:#?}"
    );
    assert!(
        rows.iter()
            .any(|r| r.contains("Write Out") && r.contains("C-o")),
        "{rows:#?}"
    );
    press(&mut ed, KeyCode::Enter, KeyModifiers::NONE);
    assert!(ed.palette.is_none());
    assert_eq!(
        ed.prompt.as_ref().map(|p| p.kind),
        Some(PromptKind::WriteName)
    );
    ed.prompt = None;
    // Recently run comes first among equals; an empty query lists all.
    press(&mut ed, KeyCode::Char('x'), KeyModifiers::ALT);
    assert_eq!(ed.palette.as_ref().unwrap().items[0], "write-out");
    assert!(ed.palette.as_ref().unwrap().items.len() > 40);
    press(&mut ed, KeyCode::Esc, KeyModifiers::NONE);
    assert!(ed.palette.is_none());
    // Nothing matches: Enter says so and runs nothing.
    press(&mut ed, KeyCode::Char('x'), KeyModifiers::ALT);
    press_text(&mut ed, "qqqq");
    press(&mut ed, KeyCode::Enter, KeyModifiers::NONE);
    assert!(ed.status_text().unwrap().contains("No command matches"));
}

#[test]
fn a_view_puts_its_own_keys_on_the_bar() {
    let mut ed = conflict_ed();
    let bar = ed.bar_items();
    assert!(
        bar.iter().any(|(k, t)| k == "o" && t == "Take Ours"),
        "{bar:?}"
    );
    assert!(
        bar.iter().any(|(k, t)| k == "s" && t == "Split/Unified"),
        "{bar:?}"
    );
    // A key the view does not bind is ignored, not typed.
    press(&mut ed, KeyCode::Char('z'), KeyModifiers::NONE);
    assert_eq!(lines(&ed).join("\n"), CONFLICTED);
    press(&mut ed, KeyCode::Esc, KeyModifiers::NONE);
    assert!(
        ed.bar_items()
            .iter()
            .any(|(k, t)| k == "C-x" && t == "Exit")
    );
}

#[test]
fn m_p_on_plain_text_says_there_is_nothing_to_render() {
    let mut ed = test_ed("just text");
    press(&mut ed, KeyCode::Char('p'), KeyModifiers::ALT);
    assert!(ed.diff_view.is_none());
    assert!(ed.status_text().unwrap().starts_with("Nothing to render"));
}

#[test]
fn a_timestamp_only_change_says_so_in_the_diff() {
    let d = temp_dir("ext_touch");
    let f = d.0.join("f.txt");
    fs::write(&f, "one\n").unwrap();
    let mut ed = Editor::new(Buffer::from_file(&f).unwrap(), config::Config::default());
    let later = std::time::SystemTime::now() + Duration::from_secs(60);
    fs::File::options()
        .write(true)
        .open(&f)
        .unwrap()
        .set_modified(later)
        .unwrap();
    ed.save_to(f.clone());
    assert_eq!(prompt_kind(&ed), Some(PromptKind::ConfirmExternal));
    press(&mut ed, KeyCode::Char('d'), KeyModifiers::NONE);
    assert!(ed.diff_view.is_none());
    assert_eq!(prompt_kind(&ed), Some(PromptKind::ConfirmExternal));
    assert!(
        ed.status_text()
            .unwrap()
            .contains("only the file's timestamp")
    );
}

// D3 — undo/redo protective tests (green on the snapshot impl; they must
// stay green through the region-based refactor).

fn press_text(ed: &mut Editor, s: &str) {
    for c in s.chars() {
        press(ed, KeyCode::Char(c), KeyModifiers::NONE);
    }
}

fn replace_via_prompt(ed: &mut Editor, find: &str, with: &str, answer: char) {
    press(ed, KeyCode::Char('4'), KeyModifiers::CONTROL);
    press_text(ed, find);
    press(ed, KeyCode::Enter, KeyModifiers::NONE);
    press_text(ed, with);
    press(ed, KeyCode::Enter, KeyModifiers::NONE);
    press(ed, KeyCode::Char(answer), KeyModifiers::NONE);
}

#[test]
fn undo_types_coalesce() {
    let mut ed = test_ed("");
    press_text(&mut ed, "abc");
    assert_eq!(lines(&ed), vec!["abc"]);
    press(&mut ed, KeyCode::Char('u'), KeyModifiers::ALT);
    assert_eq!(lines(&ed), vec![""]);
}

#[test]
fn undo_backspace_run() {
    let mut ed = test_ed("ab");
    press(&mut ed, KeyCode::End, KeyModifiers::NONE);
    press(&mut ed, KeyCode::Backspace, KeyModifiers::NONE);
    press(&mut ed, KeyCode::Backspace, KeyModifiers::NONE);
    assert_eq!(lines(&ed), vec![""]);
    press(&mut ed, KeyCode::Char('u'), KeyModifiers::ALT);
    assert_eq!(lines(&ed), vec!["ab"]);
}

#[test]
fn undo_redo_roundtrip() {
    let mut ed = test_ed("ab");
    press(&mut ed, KeyCode::End, KeyModifiers::NONE);
    press(&mut ed, KeyCode::Char('c'), KeyModifiers::NONE);
    press(&mut ed, KeyCode::Char('u'), KeyModifiers::ALT);
    assert_eq!(lines(&ed), vec!["ab"]);
    assert_eq!(ed.bs().cursor, Pos { row: 0, col: 2 });
    press(&mut ed, KeyCode::Char('e'), KeyModifiers::ALT);
    assert_eq!(lines(&ed), vec!["abc"]);
    assert_eq!(ed.bs().cursor, Pos { row: 0, col: 3 });
}

#[test]
fn undo_repeated_cut_coalesce() {
    let mut ed = test_ed("1\n2\n3\n4");
    for _ in 0..3 {
        press(&mut ed, KeyCode::Char('k'), KeyModifiers::CONTROL);
    }
    assert_eq!(lines(&ed), vec!["4"]);
    press(&mut ed, KeyCode::Char('u'), KeyModifiers::ALT);
    assert_eq!(lines(&ed), vec!["1", "2", "3", "4"]);
}

#[test]
fn undo_partial_then_full_cut() {
    let mut ed = test_ed("hello");
    press(&mut ed, KeyCode::Right, KeyModifiers::NONE);
    press(&mut ed, KeyCode::Right, KeyModifiers::NONE);
    press(&mut ed, KeyCode::Char('k'), KeyModifiers::CONTROL);
    assert_eq!(lines(&ed), vec!["he"]);
    press(&mut ed, KeyCode::Char('k'), KeyModifiers::CONTROL);
    assert_eq!(lines(&ed), vec![""]);
    press(&mut ed, KeyCode::Char('u'), KeyModifiers::ALT);
    assert_eq!(lines(&ed), vec!["hello"]);
}

#[test]
fn undo_paste_roundtrip() {
    let mut ed = test_ed("ab\ncd");
    press(&mut ed, KeyCode::Char('k'), KeyModifiers::CONTROL);
    press(&mut ed, KeyCode::Char('u'), KeyModifiers::CONTROL);
    assert_eq!(lines(&ed), vec!["abcd"]);
    press(&mut ed, KeyCode::Char('u'), KeyModifiers::ALT);
    assert_eq!(lines(&ed), vec!["cd"]);
    assert_eq!(ed.bs().cursor, Pos { row: 0, col: 0 });
    press(&mut ed, KeyCode::Char('e'), KeyModifiers::ALT);
    assert_eq!(lines(&ed), vec!["abcd"]);
    assert_eq!(ed.bs().cursor, Pos { row: 0, col: 2 });
}

#[test]
fn undo_replace_all_one_step() {
    let mut ed = test_ed("aa\naa");
    replace_via_prompt(&mut ed, "aa", "x", 'a');
    assert_eq!(lines(&ed), vec!["x", "x"]);
    press(&mut ed, KeyCode::Char('u'), KeyModifiers::ALT);
    assert_eq!(lines(&ed), vec!["aa", "aa"]);
}

#[test]
fn undo_limit_trims() {
    let mut ed = test_ed("a");
    for _ in 0..600 {
        press(&mut ed, KeyCode::Enter, KeyModifiers::NONE);
    }
    // CURRENT snapshot impl coalesces a run of Enters into one step (1
    // step here); D3 makes Newline never coalesce (500 steps). The plan's
    // contract "500 steps max" holds for both.
    assert!(ed.bs().undo.len() <= 500);
}

#[test]
fn undo_selection_overwrite() {
    let mut ed = test_ed("abcd");
    press(&mut ed, KeyCode::Right, KeyModifiers::NONE);
    press(&mut ed, KeyCode::Char('a'), KeyModifiers::ALT);
    press(&mut ed, KeyCode::Right, KeyModifiers::NONE);
    press(&mut ed, KeyCode::Right, KeyModifiers::NONE);
    press(&mut ed, KeyCode::Char('X'), KeyModifiers::NONE);
    assert_eq!(lines(&ed), vec!["aXd"]);
    press(&mut ed, KeyCode::Char('u'), KeyModifiers::ALT);
    assert_eq!(lines(&ed), vec!["abcd"]);
    assert!(ed.bs().mark.is_none());
}

#[test]
fn undo_delete_selection() {
    let mut ed = test_ed("abcd");
    press(&mut ed, KeyCode::Right, KeyModifiers::NONE);
    press(&mut ed, KeyCode::Char('a'), KeyModifiers::ALT);
    press(&mut ed, KeyCode::Right, KeyModifiers::NONE);
    press(&mut ed, KeyCode::Right, KeyModifiers::NONE);
    press(&mut ed, KeyCode::Backspace, KeyModifiers::NONE);
    assert_eq!(lines(&ed), vec!["ad"]);
    press(&mut ed, KeyCode::Char('u'), KeyModifiers::ALT);
    assert_eq!(lines(&ed), vec!["abcd"]);
}

#[test]
fn undo_newline_join() {
    let mut ed = test_ed("ab");
    press(&mut ed, KeyCode::End, KeyModifiers::NONE);
    press(&mut ed, KeyCode::Enter, KeyModifiers::NONE);
    press(&mut ed, KeyCode::Backspace, KeyModifiers::NONE);
    assert_eq!(lines(&ed), vec!["ab"]);
    press(&mut ed, KeyCode::Char('u'), KeyModifiers::ALT);
    assert_eq!(lines(&ed), vec!["ab", ""]);
    press(&mut ed, KeyCode::Char('u'), KeyModifiers::ALT);
    assert_eq!(lines(&ed), vec!["ab"]);
}

#[test]
fn undo_read_empty_replace() {
    let d = temp_dir("read_undo");
    let src = d.0.join("in.txt");
    fs::write(&src, "x\ny\n").unwrap();
    let mut ed = test_ed("");
    press(&mut ed, KeyCode::Char('r'), KeyModifiers::CONTROL);
    press_text(&mut ed, &src.display().to_string());
    press(&mut ed, KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(lines(&ed), vec!["x", "y"]);
    press(&mut ed, KeyCode::Char('u'), KeyModifiers::ALT);
    assert_eq!(lines(&ed), vec![""]);
}

#[test]
fn undo_redo_after_new_edit() {
    let mut ed = test_ed("ab");
    press(&mut ed, KeyCode::End, KeyModifiers::NONE);
    press(&mut ed, KeyCode::Char('c'), KeyModifiers::NONE);
    press(&mut ed, KeyCode::Char('u'), KeyModifiers::ALT);
    press(&mut ed, KeyCode::Char('x'), KeyModifiers::NONE);
    assert_eq!(lines(&ed), vec!["abx"]);
    press(&mut ed, KeyCode::Char('e'), KeyModifiers::ALT);
    assert_eq!(lines(&ed), vec!["abx"]);
}

// D1 — replace-all is one pass from the cursor, non-overlapping.

#[test]
fn replace_all_from_cursor_and_no_overlap() {
    let mut ed = test_ed("aaa");
    replace_via_prompt(&mut ed, "aa", "b", 'a');
    assert_eq!(lines(&ed), vec!["ba"]);
    assert_eq!(ed.replace_count, 1);
}

#[test]
fn replace_all_multiline() {
    let mut ed = test_ed("aa\naa\naa");
    replace_via_prompt(&mut ed, "aa", "x", 'a');
    assert_eq!(ed.replace_count, 3);
    assert_eq!(ed.bs().cursor, Pos { row: 2, col: 1 });
    assert_eq!(lines(&ed), vec!["x", "x", "x"]);
}

#[test]
fn replace_all_drift() {
    let mut ed = test_ed("aaaa");
    replace_via_prompt(&mut ed, "aa", "bbb", 'a');
    assert_eq!(lines(&ed), vec!["bbbbbb"]);
    assert_eq!(ed.replace_count, 2);
}

// D4 — didChange is debounced: the flag clears only after the 300 ms
// window, even when no client is attached (scratch buffer → lsp None).

#[test]
fn lsp_flush_debounce() {
    let mut ed = test_ed("");
    let now = Instant::now();
    ed.insert_char('x');
    assert!(ed.bs().lsp_dirty);
    ed.lsp_flush(now + Duration::from_millis(100));
    assert!(ed.bs().lsp_dirty, "inside the window the flag must survive");
    ed.lsp_flush(now + Duration::from_millis(400));
    assert!(!ed.bs().lsp_dirty, "no client attached: flag still cleared");
    assert_eq!(ed.bs().lsp_last_send, now + Duration::from_millis(400));
}

// D5 — a failed async handshake is adopted on the next poll and flashes;
// a handshake started for another file stays silent.

#[test]
fn lsp_adopt_err_flashes() {
    let mut ed = test_ed("");
    let (tx, rx) = mpsc::channel::<Result<lsp::LspClient, String>>();
    tx.send(Err("boom".to_string())).unwrap();
    ed.bs_mut().lsp_starting = Some(("".to_string(), rx)); // scratch buffer → tag ""
    ed.lsp_poll();
    assert!(ed.bs().lsp.is_none());
    assert!(ed.bs().lsp_starting.is_none());
    let st = ed.status_text().unwrap();
    assert!(st.contains("boom"), "status: {st}");
}

#[test]
fn lsp_adopt_stale_silent() {
    let mut ed = test_ed("");
    let (tx, rx) = mpsc::channel::<Result<lsp::LspClient, String>>();
    tx.send(Err("stale boom".to_string())).unwrap();
    ed.bs_mut().lsp_starting = Some(("/tmp/other.rs".to_string(), rx));
    ed.lsp_poll();
    assert!(ed.bs().lsp.is_none());
    assert!(ed.bs().lsp_starting.is_none());
    assert!(ed.status.is_none(), "stale failure must be silent");
}

// E5 — diagnostics are underlined in severity color under the cursor;
// search matches and the selection still win.

// ---------- completion (LSP) ----------

use crate::editor::{CompletionPopup, completion_prefix};

fn citem(label: &str, kind: u64) -> lsp::CompletionItem {
    lsp::CompletionItem {
        label: label.to_string(),
        kind,
        insert: label.to_string(),
        sort: label.to_string(),
        filter: label.to_string(),
    }
}

fn popup(items: Vec<lsp::CompletionItem>, row: usize, col: usize) -> CompletionPopup {
    CompletionPopup {
        items,
        sel: 0,
        row,
        col,
    }
}

#[test]
fn completion_prefix_word_dot_and_colons() {
    let l: Vec<Vec<char>> = vec![
        "foo.bar".chars().collect(),
        "x::".chars().collect(),
        " a ".chars().collect(),
    ];
    assert_eq!(completion_prefix(&l, 0, 7), Some(("bar".to_string(), 4)));
    assert_eq!(completion_prefix(&l, 0, 4), Some((String::new(), 4)));
    assert_eq!(completion_prefix(&l, 1, 3), Some((String::new(), 3)));
    assert_eq!(completion_prefix(&l, 2, 2), Some(("a".to_string(), 1)));
    assert_eq!(completion_prefix(&l, 2, 3), None);
    assert_eq!(completion_prefix(&l, 2, 0), None);
}

#[test]
fn completion_response_prefix_first_then_fuzzy() {
    // Exact prefix matches keep the server's order and come first;
    // subsequence matches follow; non-matches are dropped.
    let mut ed = test_ed("x\nis\n");
    ed.bs_mut().cursor = Pos { row: 1, col: 2 };
    ed.completion_q.push_back((1, "is".to_string()));
    ed.completion = Some(popup(Vec::new(), 1, 0));
    // sortText carries the server's ranking: exact matches first here.
    let it = |label: &str, sort: &str| {
        let mut c = citem(label, 6);
        c.sort = sort.to_string();
        c
    };
    ed.complete_response(vec![
        it("into_raw_parts", "3"),
        it("is_empty", "1"),
        it("__is_long", "4"),
        it("as_bytes", "5"),
        it("is_ascii", "2"),
    ]);
    let p = ed.completion.as_ref().unwrap();
    let labels: Vec<_> = p.items.iter().map(|i| i.label.as_str()).collect();
    // "into_raw_parts" survives as a subsequence match (i…s), but only
    // after the exact-prefix items; "as_bytes" has no `i` and is dropped.
    assert_eq!(
        labels,
        vec!["is_empty", "is_ascii", "into_raw_parts", "__is_long"]
    );
    assert_eq!(p.sel, 0);
    assert!(ed.completion_q.is_empty(), "queue entry consumed");
    // Empty prefix (right after `.`): the server's list is kept as-is.
    let mut ed2 = test_ed("x.\n");
    ed2.bs_mut().cursor = Pos { row: 0, col: 2 };
    ed2.completion_q.push_back((0, String::new()));
    ed2.completion = Some(popup(Vec::new(), 0, 0));
    ed2.complete_response(vec![citem("into_raw_parts", 6), citem("zz", 6)]);
    assert_eq!(ed2.completion.as_ref().unwrap().items.len(), 2);
}

#[test]
fn completion_fallback_junk_parks_popup_and_retries() {
    // A `.`-context answered with path-fallback items (rust-analyzer
    // still scanning a freshly opened crate) is not applied: the popup
    // stays empty and a re-request is scheduled. The same items in a
    // word context ("cra") are legitimate and applied as-is.
    let mut ed = test_ed("text.\n");
    ed.bs_mut().cursor = Pos { row: 0, col: 5 };
    ed.completion_q.push_back((0, String::new()));
    ed.completion = Some(popup(Vec::new(), 0, 5));
    ed.complete_response(vec![citem("crate::", 0), citem("text", 6)]);
    assert!(
        ed.completion.as_ref().unwrap().items.is_empty(),
        "fallback junk parked"
    );
    assert!(ed.completion_retry.is_some(), "re-request scheduled");
    assert_eq!(ed.completion_retries, 1);
    // Timer elapsed: the poll consumes it and keeps the popup open
    // (no LSP attached here, so the re-request itself is a no-op).
    ed.completion_retry = Some(Instant::now() - Duration::from_millis(1));
    ed.completion_retry_poll();
    assert!(ed.completion.is_some());
    assert!(ed.completion_retry.is_none(), "timer consumed");
    // Word context: path items are legitimate.
    let mut ed2 = test_ed("cra\n");
    ed2.bs_mut().cursor = Pos { row: 0, col: 3 };
    ed2.completion_q.push_back((0, "cra".to_string()));
    ed2.completion = Some(popup(Vec::new(), 0, 0));
    ed2.complete_response(vec![citem("crate::", 0)]);
    assert_eq!(ed2.completion.as_ref().unwrap().items.len(), 1);
    // A good dot-context response resets the retry state.
    let mut ed3 = test_ed("text.\n");
    ed3.bs_mut().cursor = Pos { row: 0, col: 5 };
    ed3.completion_q.push_back((0, String::new()));
    ed3.completion = Some(popup(Vec::new(), 0, 5));
    ed3.completion_retries = 3;
    ed3.complete_response(vec![citem("is_empty", 1)]);
    assert_eq!(ed3.completion.as_ref().unwrap().items.len(), 1);
    assert!(ed3.completion_retry.is_none());
    assert_eq!(ed3.completion_retries, 0);
}

#[test]
fn completion_response_stale_or_missing_popup() {
    // Response answering an older keystroke (different prefix): dropped,
    // popup untouched.
    let mut ed = test_ed("ab\n");
    ed.bs_mut().cursor = Pos { row: 0, col: 2 };
    ed.completion_q.push_back((0, "a".to_string()));
    ed.completion = Some(popup(vec![citem("ab", 6)], 0, 0));
    ed.complete_response(vec![citem("abc", 6)]);
    let p = ed.completion.as_ref().unwrap();
    assert_eq!(p.items.len(), 1, "stale response must not replace items");
    // Response for a different row than the cursor: popup closed.
    let mut ed2 = test_ed("ab\n");
    ed2.completion_q.push_back((0, "ab".to_string()));
    ed2.completion = Some(popup(Vec::new(), 5, 0));
    ed2.complete_response(vec![citem("ab", 6)]);
    assert!(ed2.completion.is_none());
    // Matched context but no popup open at all: ignored, queue cleared.
    let mut ed3 = test_ed("ab\n");
    ed3.bs_mut().cursor = Pos { row: 0, col: 2 };
    ed3.completion_q.push_back((0, "ab".to_string()));
    ed3.complete_response(vec![citem("ab", 6)]);
    assert!(ed3.completion.is_none());
    assert!(ed3.completion_q.is_empty());
    // No request in flight: response ignored.
    let mut ed4 = test_ed("ab\n");
    ed4.completion = Some(popup(Vec::new(), 0, 0));
    ed4.complete_response(vec![citem("ab", 6)]);
    assert!(ed4.completion_q.is_empty());
}

#[test]
fn completion_response_no_matches_closes() {
    let mut ed = test_ed("zz\n");
    ed.bs_mut().cursor = Pos { row: 0, col: 2 };
    ed.completion_q.push_back((0, "zz".to_string()));
    ed.completion = Some(popup(Vec::new(), 0, 0));
    ed.complete_response(vec![]);
    assert!(ed.completion.is_none());
}

#[test]
fn completion_accept_inserts_remainder() {
    let mut ed = test_ed("pri\n");
    ed.bs_mut().cursor = Pos { row: 0, col: 3 };
    ed.completion = Some(popup(vec![citem("println!", 3)], 0, 0));
    ed.completion_accept();
    assert_eq!(lines(&ed), vec!["println!"]);
    assert_eq!(ed.bs().cursor, Pos { row: 0, col: 8 });
    assert!(ed.completion.is_none());
}

#[test]
fn completion_accept_replaces_divergent_prefix() {
    let mut ed = test_ed("pri\n");
    ed.bs_mut().cursor = Pos { row: 0, col: 3 };
    ed.completion = Some(popup(vec![citem("puts", 6)], 0, 0));
    ed.completion_accept();
    assert_eq!(lines(&ed), vec!["puts"]);
    assert_eq!(ed.bs().cursor, Pos { row: 0, col: 4 });
}

#[test]
fn completion_nav_wraps_and_esc_closes() {
    let mut ed = test_ed("pri\n");
    ed.bs_mut().cursor = Pos { row: 0, col: 3 };
    ed.completion = Some(popup(vec![citem("print!", 3), citem("println!", 3)], 0, 0));
    press(&mut ed, KeyCode::Down, KeyModifiers::NONE);
    assert_eq!(ed.completion.as_ref().unwrap().sel, 1);
    assert_eq!(ed.bs().cursor.col, 3, "cursor must not move");
    press(&mut ed, KeyCode::Up, KeyModifiers::NONE);
    assert_eq!(ed.completion.as_ref().unwrap().sel, 0);
    press(&mut ed, KeyCode::Char('n'), KeyModifiers::CONTROL);
    assert_eq!(ed.completion.as_ref().unwrap().sel, 1);
    press(&mut ed, KeyCode::Esc, KeyModifiers::NONE);
    assert!(ed.completion.is_none());
}

#[test]
fn completion_enter_accepts_not_newline() {
    let mut ed = test_ed("pri\n");
    ed.bs_mut().cursor = Pos { row: 0, col: 3 };
    ed.completion = Some(popup(vec![citem("print!", 3)], 0, 0));
    press(&mut ed, KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(lines(&ed), vec!["print!"]);
    assert_eq!(ed.bs().cursor.col, 6);
}

#[test]
fn completion_typing_backspace_and_space_lifecycle() {
    // No LSP attached: the popup can't (re)open, but typing inside the
    // word and backspacing keep it alive; leaving the word closes it.
    let mut ed = test_ed("pr x\n");
    ed.bs_mut().cursor = Pos { row: 0, col: 2 };
    ed.completion = Some(popup(vec![citem("pr", 6)], 0, 0));
    press_text(&mut ed, "i");
    assert!(ed.completion.is_some(), "identifier char keeps popup");
    press(&mut ed, KeyCode::Backspace, KeyModifiers::NONE);
    assert!(ed.completion.is_some(), "backspace inside word keeps popup");
    press(&mut ed, KeyCode::Char(' '), KeyModifiers::NONE);
    assert!(ed.completion.is_none(), "space closes popup");
}

// ---------- jump to definition (M-. / M-,) ----------

fn named_ed(text: &str, path: &str) -> Editor {
    let mut ed = test_ed(text);
    ed.bs_mut().buf.name = Some(std::path::PathBuf::from(path));
    ed
}

#[test]
fn jump_definition_without_lsp_flashes() {
    let mut ed = test_ed("fn main() {}\n");
    press(&mut ed, KeyCode::Char('.'), KeyModifiers::ALT);
    assert_eq!(ed.status_text(), Some("No LSP server".to_string()));
    press(&mut ed, KeyCode::Char(','), KeyModifiers::ALT);
    assert_eq!(ed.status_text(), Some("No jump to return to".to_string()));
}

#[test]
fn goto_location_same_file_pushes_stack_and_moves() {
    let p = "/tmp/rano_jump_same.rs";
    let mut ed = named_ed("fn a() {}\nfn b() {}\n", p);
    ed.bs_mut().cursor = Pos { row: 1, col: 3 };
    let loc = lsp::DefLocation {
        uri: lsp::path_to_uri(std::path::Path::new(p)),
        line: 0,
        character: 3,
    };
    ed.goto_location(loc, Pos { row: 1, col: 3 });
    assert_eq!(ed.bs().cursor, Pos { row: 0, col: 3 });
    assert_eq!(ed.def_back.len(), 1);
    assert!(ed.def_back[0].buf.is_none());
    assert_eq!(ed.def_back[0].idx, Some(0), "the origin buffer is recorded");
    // M-, returns to the origin row.
    press(&mut ed, KeyCode::Char(','), KeyModifiers::ALT);
    assert_eq!(ed.bs().cursor, Pos { row: 1, col: 3 });
    assert!(ed.def_back.is_empty());
}

#[test]
fn goto_location_cross_file_swaps_and_back() {
    let target = std::env::temp_dir().join("rano_jump_target.rs");
    std::fs::write(&target, "fn target_fn() {}\n").unwrap();
    let mut ed = named_ed("fn a() {}\n", "/tmp/rano_jump_src.rs");
    ed.bs_mut().cursor = Pos { row: 0, col: 1 };
    let loc = lsp::DefLocation {
        uri: lsp::path_to_uri(&target),
        line: 0,
        character: 3,
    };
    ed.goto_location(loc, Pos { row: 0, col: 1 });
    assert_eq!(lines(&ed), vec!["fn target_fn() {}"]);
    assert_eq!(ed.bs().cursor, Pos { row: 0, col: 3 });
    // The origin buffer (with its edits) waits on the stack.
    let back = ed.def_back.pop().unwrap();
    let bs = back.buf.expect("single-buffer swap stores the state");
    assert_eq!(
        bs.buf
            .rows()
            .map(|l| l.iter().collect::<String>())
            .collect::<Vec<_>>(),
        vec!["fn a() {}"]
    );
    assert_eq!(back.pos, Pos { row: 0, col: 1 });
    std::fs::remove_file(&target).ok();
}

#[test]
fn goto_location_multibuffer_keeps_origin_and_back_switches() {
    let target = std::env::temp_dir().join("rano_jump_mb.rs");
    std::fs::write(&target, "fn t() {}\n").unwrap();
    let mut ed = named_ed("fn a() {}\n", "/tmp/rano_jump_mb_src.rs");
    ed.config.multibuffer = true;
    let loc = lsp::DefLocation {
        uri: lsp::path_to_uri(&target),
        line: 0,
        character: 3,
    };
    ed.goto_location(loc, Pos { row: 0, col: 6 });
    assert_eq!(ed.cur, 1, "target opened as a new buffer");
    assert_eq!(lines(&ed), vec!["fn t() {}"]);
    press(&mut ed, KeyCode::Char(','), KeyModifiers::ALT);
    assert_eq!(ed.cur, 0);
    assert_eq!(lines(&ed), vec!["fn a() {}"]);
    assert_eq!(ed.bs().cursor, Pos { row: 0, col: 6 });
    std::fs::remove_file(&target).ok();
}

fn diag(line: usize, col: usize, end_col: usize, severity: u64) -> lsp::Diagnostic {
    lsp::Diagnostic {
        line,
        col,
        end_col,
        message: "m".to_string(),
        severity,
    }
}

#[test]
fn diag_underline_style() {
    let mut ed = test_ed("fn main() {}\n");
    ed.bs_mut().lsp_diags = vec![diag(0, 0, 2, 1)];
    let s = ed.char_style_with(Pos { row: 0, col: 0 }, &ed.all_diags());
    assert_eq!(s.fg, Some(Color::Red));
    assert!(s.add_modifier.contains(Modifier::UNDERLINED));
    let s = ed.char_style_with(Pos { row: 0, col: 3 }, &ed.all_diags());
    assert_eq!(s.fg, None);
    assert!(!s.add_modifier.contains(Modifier::UNDERLINED));
    ed.bs_mut().lsp_diags = vec![diag(0, 0, 2, 2)];
    assert_eq!(
        ed.char_style_with(Pos { row: 0, col: 1 }, &ed.all_diags())
            .fg,
        Some(Color::Yellow)
    );
    ed.bs_mut().lsp_diags = vec![diag(0, 0, 2, 3)];
    assert_eq!(
        ed.char_style_with(Pos { row: 0, col: 1 }, &ed.all_diags())
            .fg,
        Some(Color::Blue)
    );
}

#[test]
fn diag_style_priority() {
    let mut ed = test_ed("fn main() {}\n");
    ed.bs_mut().lsp_diags = vec![diag(0, 0, 5, 1)];
    ed.bs_mut().mark = Some(Pos { row: 0, col: 0 });
    ed.bs_mut().cursor = Pos { row: 0, col: 3 };
    assert_eq!(
        ed.char_style_with(Pos { row: 0, col: 1 }, &ed.all_diags()),
        Style::default().fg(Color::White).bg(Color::DarkGray)
    );
    ed.bs_mut().mark = None;
    ed.bs_mut().search.query = "fn".to_string();
    ed.bs_mut().search_matches = Some(vec![(Pos { row: 0, col: 0 }, 2)]);
    ed.bs_mut().search.current = 0;
    assert_eq!(
        ed.char_style_with(Pos { row: 0, col: 1 }, &ed.all_diags()),
        Style::default().fg(Color::Black).bg(Color::Yellow)
    );
}

// E5 — M-D walks the diagnostics top-down, wrapping past the last one.

// Indentation: Tab follows the buffer's own indent style instead of
// always inserting a literal tab char.

#[test]
fn tab_matches_space_indent() {
    let mut ed = test_ed("    a\n\n");
    ed.bs_mut().cursor = Pos { row: 1, col: 0 };
    press(&mut ed, KeyCode::Tab, KeyModifiers::NONE);
    assert_eq!(lines(&ed), vec!["    a", "    "]);
    assert_eq!(ed.bs().cursor.col, 4);
}

#[test]
fn tab_aligns_to_next_unit_mid_line() {
    let mut ed = test_ed("    a\n  x");
    ed.bs_mut().cursor = Pos { row: 1, col: 2 };
    press(&mut ed, KeyCode::Tab, KeyModifiers::NONE);
    assert_eq!(lines(&ed), vec!["    a", "    x"]);
}

#[test]
fn tab_uses_tab_char_when_file_does() {
    let mut ed = test_ed("\ta\n\tb\n\n");
    ed.bs_mut().cursor = Pos { row: 2, col: 0 };
    press(&mut ed, KeyCode::Tab, KeyModifiers::NONE);
    assert_eq!(lines(&ed), vec!["\ta", "\tb", "\t"]);
}

#[test]
fn backspace_deletes_indent_run() {
    let mut ed = test_ed("    a\n    x");
    ed.bs_mut().cursor = Pos { row: 1, col: 4 };
    press(&mut ed, KeyCode::Backspace, KeyModifiers::NONE);
    assert_eq!(ed.bs().cursor.col, 0);
    assert_eq!(lines(&ed), vec!["    a", "x"]);
}

#[test]
fn backspace_mid_text_still_one_char() {
    let mut ed = test_ed("    ab");
    ed.bs_mut().cursor = Pos { row: 0, col: 6 };
    press(&mut ed, KeyCode::Backspace, KeyModifiers::NONE);
    assert_eq!(lines(&ed), vec!["    a"]);
}

// Syntax errors surface without a language server (tree-sitter ERROR
// nodes become diagnostics), and zero-width LSP ranges become visible.

#[test]
fn syntax_error_diag_without_lsp() {
    let mut buf = Buffer::new();
    buf.name = Some(std::path::PathBuf::from("t.rs"));
    buf.set_rows(vec!["fn main() {".chars().collect()]);
    let mut ed = Editor::new(buf, config::Config::default());
    ed.edit_invalidate();
    // Diagnostics are a WHOLE-document parse now, debounced the way the
    // LSP's `didChange` is — so the test does what the run loop does: the
    // frame's highlight (colour), then a flush past the debounce.
    ed.ensure_highlight();
    let after_the_pause = std::time::Instant::now() + std::time::Duration::from_millis(400);
    ed.diag_flush(after_the_pause);
    assert!(
        !ed.bs().syntax_diags.is_empty(),
        "unclosed fn block must yield a tree-sitter diagnostic"
    );
    ed.bs_mut()
        .buf
        .set_rows(vec!["fn main() {}".chars().collect()]);
    ed.edit_invalidate();
    ed.ensure_highlight();
    ed.diag_flush(after_the_pause);
    assert!(ed.bs().syntax_diags.is_empty(), "clean parse has no diags");
}

#[test]
fn zero_width_diag_widened_to_one_column() {
    let lines = vec!["abcdefghij".chars().collect::<Vec<char>>()];
    let mut d = diag(0, 5, 5, 1);
    editor::widen_zero_width(std::slice::from_mut(&mut d), &lines);
    assert_eq!((d.col, d.end_col), (5, 6));
    let mut d = diag(0, 10, 10, 1); // insertion point at EOL
    editor::widen_zero_width(std::slice::from_mut(&mut d), &lines);
    assert_eq!((d.col, d.end_col), (9, 10));
}

#[test]
fn jump_next_diag_includes_syntax_diags() {
    let mut buf = Buffer::new();
    buf.name = Some(std::path::PathBuf::from("t.rs"));
    buf.set_rows(vec!["fn broken(".chars().collect()]);
    let mut ed = Editor::new(buf, config::Config::default());
    ed.edit_invalidate();
    ed.jump_next_diag();
    assert_eq!(ed.bs().cursor.row, 0, "jumps to the tree-sitter error");
}

#[test]
fn jump_next_diag_wraps() {
    let mut ed = test_ed("a\nb\nc\nd\ne");
    ed.bs_mut().lsp_diags = vec![diag(3, 0, 1, 1), diag(1, 0, 1, 2)];
    ed.bs_mut().cursor = Pos { row: 0, col: 0 };
    ed.jump_next_diag();
    assert_eq!(ed.bs().cursor, Pos { row: 1, col: 0 });
    ed.jump_next_diag();
    assert_eq!(ed.bs().cursor, Pos { row: 3, col: 0 });
    ed.jump_next_diag();
    assert_eq!(ed.bs().cursor, Pos { row: 1, col: 0 }, "wrap past the last");
    assert!(ed.bs().mark.is_none());
}

#[test]
fn jump_next_diag_empty_flashes() {
    let mut ed = test_ed("a\nb");
    ed.bs_mut().cursor = Pos { row: 1, col: 0 };
    press(&mut ed, KeyCode::Char('d'), KeyModifiers::ALT);
    let st = ed.status_text().unwrap();
    assert!(st.contains("No diagnostics"), "status: {st}");
    assert_eq!(ed.bs().cursor, Pos { row: 1, col: 0 });
}

// D7 — exec is async: output lands below the spawn row as ONE undo
// step; a failed command records no step and never edit_invalidates.

fn poll_until(ed: &mut Editor, done: impl Fn(&Editor) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !done(ed) {
        assert!(Instant::now() < deadline, "job did not finish in time");
        std::thread::sleep(Duration::from_millis(10));
        ed.exec_poll();
    }
}

#[test]
fn exec_async_inserts_output_one_undo() {
    let mut ed = test_ed("");
    ed.do_exec("printf hi");
    assert!(ed.bs().exec_job.is_some());
    poll_until(&mut ed, |ed| ed.bs().exec_job.is_none());
    assert_eq!(lines(&ed), vec!["", "hi"]);
    assert_eq!(ed.bs().cursor, Pos { row: 1, col: 0 });
    let st = ed.status_text().unwrap();
    assert!(st.contains("Ran: printf hi"), "status: {st}");
    press(&mut ed, KeyCode::Char('u'), KeyModifiers::ALT);
    assert_eq!(lines(&ed), vec![""]);
}

#[test]
fn exec_failure_no_undo_step() {
    let mut ed = test_ed("");
    ed.do_exec("false");
    poll_until(&mut ed, |ed| ed.bs().exec_job.is_none());
    let st = ed.status_text().unwrap();
    assert!(st.contains("Exit code"), "status: {st}");
    assert_eq!(lines(&ed), vec![""]);
    assert!(ed.bs().undo.is_empty(), "failure must record no undo step");
    assert!(!ed.bs().buf.modified, "failure must not edit_invalidate");
    assert!(!ed.bs().lsp_dirty, "failure must not edit_invalidate");
}

#[test]
fn status_shows_running_command() {
    let mut ed = test_ed("");
    ed.do_exec("sleep 0.3");
    ed.status = None; // skip the spawn flash; exercise the exec_job arm
    let st = ed.status_text().unwrap();
    assert!(st.contains("Running: sleep 0.3"), "status: {st}");
    poll_until(&mut ed, |ed| ed.bs().exec_job.is_none());
}

// E1 — bracketed paste: one undo step, \r stripped, cursor at the end.

#[test]
fn paste_text_single_and_multiline() {
    let mut ed = test_ed("");
    ed.paste_text("ab\ncd");
    assert_eq!(lines(&ed), vec!["ab", "cd"]);
    assert_eq!(ed.bs().cursor, Pos { row: 1, col: 2 });
    let mut ed = test_ed("x");
    ed.paste_text("q");
    assert_eq!(lines(&ed), vec!["qx"]);
    assert_eq!(ed.bs().cursor, Pos { row: 0, col: 1 });
}

#[test]
fn paste_text_one_undo() {
    let mut ed = test_ed("");
    ed.paste_text("ab\ncd");
    press(&mut ed, KeyCode::Char('u'), KeyModifiers::ALT);
    assert_eq!(lines(&ed), vec![""]);
}

#[test]
fn paste_text_strips_cr() {
    let mut ed = test_ed("");
    ed.paste_text("x\r\ny");
    assert_eq!(lines(&ed), vec!["x", "y"]);
}

// F6 — M-| filters the selected rows through an external command.

#[test]
fn filter_region_uppercases() {
    let mut ed = test_ed("hello\nworld");
    ed.bs_mut().cursor = Pos { row: 0, col: 5 };
    ed.bs_mut().mark = Some(Pos { row: 0, col: 0 });
    press(&mut ed, KeyCode::Char('|'), KeyModifiers::ALT);
    assert!(ed.prompt.is_some());
    press_text(&mut ed, "tr a-z A-Z");
    press(&mut ed, KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(lines(&ed), vec!["HELLO", "world"]);
    assert_eq!(ed.bs().cursor, Pos { row: 0, col: 0 });
    assert!(ed.bs().mark.is_none());
    press(&mut ed, KeyCode::Char('u'), KeyModifiers::ALT);
    assert_eq!(lines(&ed), vec!["hello", "world"]);
}

#[test]
fn filter_no_selection_flashes() {
    let mut ed = test_ed("hello");
    press(&mut ed, KeyCode::Char('|'), KeyModifiers::ALT);
    let st = ed.status_text().unwrap();
    assert!(st.contains("No selection"), "status: {st}");
    assert!(ed.prompt.is_none());
}

#[test]
fn filter_failure_no_undo() {
    let mut ed = test_ed("hello\nworld");
    ed.bs_mut().cursor = Pos { row: 0, col: 5 };
    ed.bs_mut().mark = Some(Pos { row: 0, col: 0 });
    press(&mut ed, KeyCode::Char('|'), KeyModifiers::ALT);
    press_text(&mut ed, "false");
    press(&mut ed, KeyCode::Enter, KeyModifiers::NONE);
    let st = ed.status_text().unwrap();
    assert!(st.contains("Exit code"), "status: {st}");
    assert_eq!(lines(&ed), vec!["hello", "world"]);
    assert_eq!(ed.bs().mark, Some(Pos { row: 0, col: 0 }), "mark unchanged");
    assert!(ed.bs().undo.is_empty(), "failure must record no undo step");
}

// E5 — M-D is next-diagnostic; word motions moved to Alt+Left/Right.

#[test]
fn alt_d_jumps_diag_alt_arrows_words() {
    let mut ed = test_ed("ab cd ef");
    ed.bs_mut().lsp_diags = vec![diag(0, 3, 5, 1)];
    press(&mut ed, KeyCode::Char('d'), KeyModifiers::ALT);
    assert_eq!(ed.bs().cursor, Pos { row: 0, col: 3 });
    let mut ed = test_ed("ab cd");
    press(&mut ed, KeyCode::Right, KeyModifiers::ALT);
    assert_eq!(ed.bs().cursor, Pos { row: 0, col: 2 });
    press(&mut ed, KeyCode::Left, KeyModifiers::ALT);
    assert_eq!(ed.bs().cursor, Pos { row: 0, col: 0 });
}

// E3 — horizontal scroll follows the cursor in display cols.

#[test]
fn scroll_x_follows_cursor() {
    let mut ed = test_ed(&"a".repeat(40));
    ed.wrap = false; // horizontal scrolling needs wrap off
    ed.show_line_numbers = false;
    ed.text_w = 10;
    press(&mut ed, KeyCode::End, KeyModifiers::NONE);
    ed.adjust_scroll_x();
    assert_eq!(ed.bs().scroll_x, 31);
    press(&mut ed, KeyCode::Home, KeyModifiers::NONE);
    ed.adjust_scroll_x();
    assert_eq!(ed.bs().scroll_x, 0);
}

#[test]
fn scroll_x_with_tabs() {
    // cursor col 3 → display col 9; 9 >= 0 + 4 → scroll_x = 9 + 1 - 4
    let mut ed = test_ed("a\tb");
    ed.wrap = false; // horizontal scrolling needs wrap off
    ed.show_line_numbers = false;
    ed.text_w = 4;
    press(&mut ed, KeyCode::End, KeyModifiers::NONE);
    ed.adjust_scroll_x();
    assert_eq!(ed.bs().scroll_x, 6);
}

// F4 — M-N toggles the line-number gutter.

#[test]
fn m_n_toggles_line_numbers() {
    let mut ed = test_ed("hi");
    assert!(ed.show_line_numbers, "line numbers default to on");
    press(&mut ed, KeyCode::Char('n'), KeyModifiers::ALT);
    assert!(!ed.show_line_numbers);
    press(&mut ed, KeyCode::Char('n'), KeyModifiers::ALT);
    assert!(ed.show_line_numbers);
}

// M-\ — soft line wrap: scroll, motion and mouse work in VISUAL rows.

#[test]
fn m_backslash_toggles_wrap() {
    let mut ed = test_ed("hi");
    assert!(ed.wrap, "soft wrap defaults to on (nano)");
    press(&mut ed, KeyCode::Char('\\'), KeyModifiers::ALT);
    assert!(!ed.wrap);
    press(&mut ed, KeyCode::Char('\\'), KeyModifiers::ALT);
    assert!(ed.wrap);
}

#[test]
fn wrap_table_and_motion_handle_wide_characters() {
    // 5 CJK characters = 10 display columns in a 4-column view: three
    // visual rows of 2, 2 and 1 characters. Nothing may split a glyph,
    // and the cursor's column must be measured in cells, not characters.
    let mut ed = test_ed("中文字语言\nx");
    ed.show_line_numbers = false;
    ed.text_w = 4;
    ed.ensure_wrap_prefix();
    assert_eq!(
        ed.bs().wrap_prefix,
        vec![0, 3, 4],
        "10 columns / 4 per row = 3 visual rows"
    );
    assert_eq!(ed.seg_count(0), 3);
    // Segments begin at characters 0, 2 and 4 — and at display columns
    // 0, 4 and 8, which is what the renderer paints from.
    assert_eq!(ed.seg_chars(0, 0), (0, 2));
    assert_eq!(ed.seg_chars(0, 1), (2, 4));
    assert_eq!(ed.seg_chars(0, 2), (4, 5));
    assert_eq!(ed.seg_disp(0, 0), (0, 4));
    assert_eq!(ed.seg_disp(0, 1), (4, 8));
    assert_eq!(ed.seg_disp(0, 2), (8, 10));
    // A char col maps to its visual row and to its offset inside it.
    assert_eq!(ed.visual_pos(Pos { row: 0, col: 0 }), 0);
    assert_eq!(ed.visual_pos(Pos { row: 0, col: 1 }), 0);
    assert_eq!(ed.visual_pos(Pos { row: 0, col: 2 }), 1);
    assert_eq!(ed.visual_pos(Pos { row: 0, col: 4 }), 2);
    assert_eq!(ed.disp_in_seg(0, 0), 0);
    assert_eq!(ed.disp_in_seg(0, 1), 2, "one glyph in, on row 0");
    assert_eq!(ed.disp_in_seg(0, 4), 0, "start of the third row");
    // End goes to the end of the VISUAL row — a cluster boundary, never
    // the middle of a glyph — and Down carries that offset across.
    ed.bs_mut().cursor = Pos { row: 0, col: 0 };
    press(&mut ed, KeyCode::End, KeyModifiers::NONE);
    assert_eq!(ed.bs().cursor, Pos { row: 0, col: 2 });
    press(&mut ed, KeyCode::Home, KeyModifiers::NONE);
    ed.bs_mut().cursor = Pos { row: 0, col: 1 };
    press(&mut ed, KeyCode::Down, KeyModifiers::NONE);
    assert_eq!(
        ed.bs().cursor,
        Pos { row: 0, col: 3 },
        "one glyph in on the row below"
    );
}

#[test]
fn mouse_pos_maps_wide_columns_to_the_clicked_character() {
    let mut ed = test_ed("中文字语言\nx");
    ed.show_line_numbers = false;
    ed.text_w = 4;
    ed.ensure_wrap_prefix();
    // Pane column 1 is the left cell of 中; column 3 is inside 文.
    assert!(ed.handle_mouse(me(MouseEventKind::Down(MouseButton::Left), 1, 1)));
    assert_eq!(ed.bs().cursor, Pos { row: 0, col: 0 });
    assert!(ed.handle_mouse(me(MouseEventKind::Down(MouseButton::Left), 1, 3)));
    assert_eq!(ed.bs().cursor, Pos { row: 0, col: 1 });
    // Pane row 2 is the second visual row: its column 3 is inside 语.
    assert!(ed.handle_mouse(me(MouseEventKind::Down(MouseButton::Left), 2, 3)));
    assert_eq!(ed.bs().cursor, Pos { row: 0, col: 3 });
    // Past the last cell of the final row → end of line.
    assert!(ed.handle_mouse(me(MouseEventKind::Down(MouseButton::Left), 3, 4)));
    assert_eq!(ed.bs().cursor, Pos { row: 0, col: 5 });
}

#[test]
fn wheel_scrolls_wide_rows_by_visual_row() {
    // 5 CJK characters in a 4-column view is 3 visual rows; a wheel
    // notch must move three of them, not three characters.
    let mut ed = test_ed("中文字语言\nx");
    ed.show_line_numbers = false;
    ed.text_w = 4;
    ed.text_h = 1; // otherwise the sheet fits and there is nowhere to scroll
    ed.ensure_wrap_prefix();
    assert!(ed.handle_mouse(me(MouseEventKind::ScrollDown, 0, 0)));
    assert_eq!(ed.bs().scroll, 3, "one buffer row is three visual rows");
    // The cursor is pinned onto the edge it would have crossed.
    assert_eq!(ed.bs().cursor, Pos { row: 1, col: 0 });
}

#[test]
fn wrap_prefix_counts_visual_rows() {
    // view_w 10: row 0 (25 cols) → 3 visual rows, row 1 (5) → 1,
    // row 2 (15) → 2.
    let mut ed = test_ed(&format!(
        "{}\n{}\n{}",
        "a".repeat(25),
        "b".repeat(5),
        "c".repeat(15)
    ));
    ed.show_line_numbers = false;
    ed.text_w = 10;
    ed.ensure_wrap_prefix();
    assert_eq!(ed.bs().wrap_prefix, vec![0, 3, 4, 6]);
    assert_eq!(ed.visual_pos(Pos { row: 0, col: 0 }), 0);
    assert_eq!(ed.visual_pos(Pos { row: 0, col: 9 }), 0);
    assert_eq!(ed.visual_pos(Pos { row: 0, col: 10 }), 1);
    assert_eq!(ed.visual_pos(Pos { row: 2, col: 14 }), 5);
    assert_eq!(ed.buf_row_of_visual(2), (0, 2));
    assert_eq!(ed.buf_row_of_visual(3), (1, 0));
    assert_eq!(ed.buf_row_of_visual(5), (2, 1));
    assert_eq!(
        ed.buf_row_of_visual(99),
        (2, 1),
        "clamped to the last visual row"
    );
}

#[test]
fn wrap_prefix_rebuilds_on_edit() {
    let mut ed = test_ed("aaaa");
    ed.show_line_numbers = false;
    ed.text_w = 5;
    ed.ensure_wrap_prefix();
    assert_eq!(ed.bs().wrap_prefix, vec![0, 1]);
    ed.bs_mut().cursor = Pos { row: 0, col: 4 };
    ed.insert_char('b'); // "aaaab" — 5 cols, still one visual row
    ed.insert_char('c'); // "aaaabc" — 6 cols, two visual rows
    ed.ensure_wrap_prefix();
    assert_eq!(ed.bs().wrap_prefix, vec![0, 2]);
}

// ---------- the command line ----------

// ---------- opening at a position ----------

fn ed_with_lines(n: usize) -> Editor {
    let text: String = (1..=n).map(|i| format!("line {i}\n")).collect();
    test_ed(&text)
}

// ---- the host API: open_at, file:line:col, +line, M-S ----

#[test]
fn a_deferred_file_goes_to_its_own_position_when_first_shown() {
    let d = temp_dir("deferred_pos");
    let f = d.0.join("b.txt");
    fs::write(&f, (1..=50).map(|i| format!("l{i}\n")).collect::<String>()).unwrap();
    let mut ed = test_ed("first");
    ed.text_h = 10;
    ed.add_deferred_buffers_at(&[(f.clone(), Some(Pos { row: 29, col: 1 }))]);
    assert_eq!(ed.buffers[1].goto, Some(Pos { row: 29, col: 1 }));
    ed.set_current(1);
    ed.load_now();
    ed.apply_startup_pos();
    assert_eq!(ed.bs().cursor, Pos { row: 29, col: 1 });
    assert!(ed.bs().scroll > 0, "centred, not at the top");
}

#[test]
fn open_at_opens_switches_and_never_discards_unsaved_edits() {
    let d = temp_dir("open_at");
    let a = d.0.join("a.txt");
    let b = d.0.join("b.txt");
    fs::write(&a, "a1\na2\na3\n").unwrap();
    fs::write(&b, "b1\nb2\nb3\nb4\n").unwrap();
    let mut ed = Editor::new(Buffer::from_file(&a).unwrap(), config::Config::default());
    ed.text_h = 10;
    // Single-buffer, but the current buffer is modified: b gets its own.
    press_text(&mut ed, "X");
    ed.open_at(&b, 3, Some(2)).expect("open");
    assert_eq!(ed.buffers.len(), 2, "the edited buffer was kept");
    assert_eq!(ed.bs().buf.name.as_deref(), Some(b.as_path()));
    assert_eq!(ed.bs().cursor, Pos { row: 2, col: 1 });
    assert!(!ed.config.multibuffer, "the config is as it was");
    // Already open: switch, and position.
    ed.open_at(&a, 2, None).expect("switch");
    assert_eq!(ed.bs().buf.name.as_deref(), Some(a.as_path()));
    assert_eq!(lines(&ed)[0], "Xa1", "the edit is still there");
    assert_eq!(ed.bs().cursor, Pos { row: 1, col: 0 });
    // A missing file is an error, and nothing changes.
    assert!(ed.open_at(&d.0.join("missing.txt"), 1, None).is_err());
    assert_eq!(ed.buffers.len(), 2);
}

#[test]
fn f8_takes_a_line_after_the_name() {
    let d = temp_dir("f8_line");
    let f = d.0.join("f.txt");
    fs::write(&f, "1\n2\n3\n4\n").unwrap();
    let mut ed = test_ed("");
    ed.text_h = 10;
    ed.prompt = Some(crate::prompt::Prompt {
        kind: PromptKind::OpenName,
        text: format!("{}:3", f.display()),
        cursor: 0,
    });
    press(&mut ed, KeyCode::Enter, KeyModifiers::NONE);
    assert!(ed.prompt.is_none(), "{:?}", ed.status_text());
    assert_eq!(ed.bs().buf.name.as_deref(), Some(f.as_path()));
    assert_eq!(ed.bs().cursor.row, 2);
}

#[test]
fn m_s_sends_the_file_the_cursor_and_the_selection() {
    use std::cell::RefCell;
    use std::rc::Rc;
    let mut ed = test_ed("fn a() {\n    body();\n}");
    ed.bs_mut().buf.name = Some(PathBuf::from("/tmp/rano_send.rs"));
    // No host yet: it says so.
    press(&mut ed, KeyCode::Char('s'), KeyModifiers::ALT);
    assert!(ed.status_text().unwrap().starts_with("Nowhere to send"));
    let got: Rc<RefCell<Vec<crate::send::SendEvent>>> = Rc::default();
    let sink = got.clone();
    ed.on_send = Some(Box::new(move |e| {
        sink.borrow_mut().push(e.clone());
        Ok("sent".into())
    }));
    ed.bs_mut().cursor = Pos { row: 1, col: 4 };
    press(&mut ed, KeyCode::Char('s'), KeyModifiers::ALT);
    // A selection from (0, 3) to (1, 8): `a() {\n    body`.
    ed.bs_mut().cursor = Pos { row: 0, col: 3 };
    ed.bs_mut().mark = Some(Pos { row: 0, col: 3 });
    ed.bs_mut().cursor = Pos { row: 1, col: 8 };
    press(&mut ed, KeyCode::Char('s'), KeyModifiers::ALT);
    assert_eq!(ed.status_text().as_deref(), Some("sent"));
    let got = got.borrow();
    assert_eq!(got.len(), 2);
    assert_eq!(got[0].cursor, crate::send::Point { line: 2, column: 5 });
    assert_eq!(got[0].selection, None);
    assert!(got[0].path.as_deref().unwrap().ends_with("rano_send.rs"));
    let s = got[1].selection.as_ref().expect("a selection");
    assert_eq!((s.start.line, s.start.column), (1, 4));
    assert_eq!((s.end.line, s.end.column), (2, 9));
    assert_eq!(s.text, "a() {\n    body");
}

#[test]
fn send_command_gets_the_event_as_json_and_in_its_environment() {
    let d = temp_dir("send_cmd");
    let out = d.0.join("out.txt");
    let cmd = format!(
        "{{ echo \"$RANO_FILE|$RANO_LINE|$RANO_COLUMN\"; cat; }} > '{}.tmp' && mv '{0}.tmp' '{0}'",
        out.display()
    );
    let mut send = crate::send_ctrl::command_sender(cmd);
    let e = crate::send::SendEvent {
        path: Some(PathBuf::from("/x/y.rs")),
        cursor: crate::send::Point { line: 4, column: 2 },
        selection: None,
        modified: false,
    };
    assert_eq!(send(&e).unwrap(), "Sent line 4, column 2");
    // The command runs on its own; wait for what it wrote.
    let deadline = Instant::now() + Duration::from_secs(5);
    while !out.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    let text = fs::read_to_string(&out).expect("the command ran");
    let (env, json) = text.split_once('\n').unwrap();
    assert_eq!(env, "/x/y.rs|4|2");
    let v: serde_json::Value = serde_json::from_str(json.trim()).unwrap();
    assert_eq!(v["cursor"]["line"], 4);
    assert_eq!(v["path"], "/x/y.rs");
}

#[test]
fn a_startup_position_is_centred() {
    // 500 lines in a 20-row viewport. The target is a line with room above
    // AND below it, which is what makes centring mean anything — with the
    // file's end in view the clamp wins instead, which
    // `centring_clamps_at_both_ends` pins.
    let mut ed = ed_with_lines(500);
    ed.text_h = 20;
    ed.show_line_numbers = false;
    ed.text_w = 40;
    ed.bs_mut().goto = Some(Pos { row: 249, col: 0 });
    ed.ensure_wrap_prefix();
    ed.apply_startup_pos();
    assert_eq!(ed.bs().cursor, Pos { row: 249, col: 0 });
    assert_eq!(ed.bs().goto, None, "applied once");
    // Centred: text_h/2 = 10 rows above, so the target is on pane row 11.
    assert_eq!(ed.bs().scroll, 239, "249 - text_h/2");
    let vis = ed.visual_pos(Pos { row: 249, col: 0 });
    assert_eq!(
        vis - ed.bs().scroll,
        10,
        "the target's offset into the viewport"
    );
}

#[test]
fn centring_clamps_at_both_ends() {
    // Near the start there is nothing above to show, so the target is not
    // centred — it is as centred as the file allows, which is the top.
    let mut ed = ed_with_lines(100);
    ed.text_h = 20;
    ed.bs_mut().goto = Some(Pos { row: 1, col: 0 });
    ed.ensure_wrap_prefix();
    ed.apply_startup_pos();
    assert_eq!(ed.bs().scroll, 0, "no blank space above the first line");
    // And near the end, the last screenful.
    let mut ed = ed_with_lines(100);
    ed.text_h = 20;
    ed.bs_mut().goto = Some(Pos { row: 99, col: 0 });
    ed.ensure_wrap_prefix();
    ed.apply_startup_pos();
    assert_eq!(ed.bs().scroll, 80, "100 rows - 20 visible");
}

#[test]
fn centring_counts_visual_rows_when_wrapped() {
    // One long line above the target occupies several visual rows, and the
    // scroll is counted in those — so a buffer row and a visual row differ
    // and the centring has to use the visual one.
    // A long line first, then enough short ones that the target can
    // actually be centred (there has to be something below it).
    let tail: String = (0..40).map(|i| format!("tail {i}\n")).collect();
    let mut ed = test_ed(&format!("{}\ntarget\n{tail}", "x".repeat(120)));
    ed.text_h = 10;
    ed.text_w = 20;
    ed.show_line_numbers = false;
    ed.ensure_wrap_prefix();
    let seg = ed.seg_count(0);
    assert!(seg >= 6, "the long line wraps into {seg} rows");
    ed.bs_mut().goto = Some(Pos { row: 20, col: 0 });
    ed.apply_startup_pos();
    let vis = ed.visual_pos(Pos { row: 20, col: 0 });
    // The target's BUFFER row is 20, but its VISUAL row is much further down
    // because the long line above it occupies `seg` rows. Centring uses the
    // visual one — using the buffer row would put the view `seg` rows off.
    assert_eq!(
        vis,
        seg + 19,
        "the long line's segments plus the rows between"
    );
    assert_eq!(ed.bs().scroll, vis - 5, "centred in VISUAL rows");
}

#[test]
fn a_position_waits_for_its_row_to_arrive() {
    // With the loader a row a million lines in arrives long after the first
    // frame. Clamping to what had arrived would open the file at the wrong
    // place and look like the feature was broken, so it waits.
    let mut ed = ed_with_lines(10);
    ed.text_h = 10;
    ed.bs_mut().goto = Some(Pos {
        row: 999_999,
        col: 0,
    });
    ed.apply_startup_pos();
    assert_eq!(ed.bs().cursor, Pos { row: 0, col: 0 }, "not moved");
    assert_eq!(
        ed.bs().goto,
        Some(Pos {
            row: 999_999,
            col: 0
        }),
        "still pending"
    );
    // Now the row arrives.
    ed.bs_mut()
        .buf
        .extend_rows((0..1_000_000).map(|i| format!("r{i}").chars().collect()));
    ed.ensure_wrap_prefix();
    ed.apply_startup_pos();
    assert_eq!(ed.bs().cursor.row, 999_999);
    assert_eq!(ed.bs().goto, None);
    assert_eq!(
        ed.bs().scroll,
        999_999 - 5,
        "centred once the row was there"
    );
}

#[test]
fn a_column_is_clamped_to_the_line() {
    let mut ed = ed_with_lines(10);
    ed.text_h = 10;
    ed.bs_mut().goto = Some(Pos { row: 2, col: 999 });
    ed.ensure_wrap_prefix();
    ed.apply_startup_pos();
    assert_eq!(
        ed.bs().cursor,
        Pos { row: 2, col: 6 },
        "\"line 3\" is 6 chars"
    );
}

#[test]
fn no_position_leaves_the_editor_alone() {
    let mut ed = ed_with_lines(50);
    ed.text_h = 10;
    ed.apply_startup_pos();
    assert_eq!(ed.bs().cursor, Pos { row: 0, col: 0 });
    assert_eq!(ed.bs().scroll, 0);
}

#[test]
fn the_extended_wrap_table_agrees_with_a_full_rebuild() {
    // The loader APPENDS rows, and the table is extended from the join
    // rather than rebuilt. A wrong prefix is wrong scrolling, so this drives
    // both paths over the same sequence of appends and compares — the same
    // form as the single-row test above, for the same reason.
    let mut ed = test_ed("seed");
    ed.show_line_numbers = false;
    ed.text_w = 12;
    ed.ensure_wrap_prefix();

    for batch in 0..8 {
        let was = ed.bs().buf.row_count();
        let added: Vec<Vec<char>> = (0..7)
            .map(|i| {
                format!("b{batch}r{i}{}", "x".repeat(i * 5))
                    .chars()
                    .collect()
            })
            .collect();
        ed.bs_mut().buf.extend_rows(added);
        // What the loader does: only the first batch overlaps the seed.
        ed.bs_mut().wrap_extend_from = Some(if was <= 1 { 0 } else { was });
        ed.bs_mut().edit_gen = ed.bs().edit_gen.wrapping_add(1);
        ed.ensure_wrap_prefix();
        let extended_prefix = ed.bs().wrap_prefix.clone();
        let extended_rows = ed.bs().wrap_rows.clone();

        // Now the same buffer, forced down the full-rebuild path.
        ed.bs_mut().wrap_extend_from = None;
        ed.bs_mut().wrap_lines = 0;
        ed.bs_mut().wrap_dirty_row = None;
        ed.ensure_wrap_prefix();
        assert_eq!(
            ed.bs().wrap_prefix,
            extended_prefix,
            "batch {batch}: the extended table disagrees with a full rebuild"
        );
        assert_eq!(
            ed.bs().wrap_rows,
            extended_rows,
            "batch {batch}: row geometry differs"
        );
        assert_eq!(
            ed.bs().wrap_prefix.len(),
            ed.bs().buf.row_count() + 1,
            "batch {batch}: prefix length"
        );
    }
}

#[test]
fn the_incremental_wrap_table_agrees_with_a_full_rebuild() {
    // Typing re-measures ONE row and re-sums the rest arithmetically; a
    // multi-row edit rebuilds everything. The two must not disagree, so
    // this drives both and compares — the fast path is only ever a
    // shorter way to compute the same table.
    let text: String = (0..40)
        .map(|i| format!("{}{}\n", i, "x".repeat(i * 3)))
        .collect();
    let mut ed = test_ed(&text);
    ed.show_line_numbers = false;
    ed.text_w = 12;
    ed.ensure_wrap_prefix();

    for (row, extra) in [(0usize, 30usize), (17, 40), (39, 60)] {
        ed.bs_mut().cursor = Pos { row, col: 0 };
        for _ in 0..extra {
            ed.insert_char('y');
        }
        ed.ensure_wrap_prefix();
        let incremental = ed.bs().wrap_prefix.clone();
        let rows = ed.bs().wrap_rows.clone();
        // The same buffer, forced down the full-rebuild path.
        ed.bs_mut().wrap_dirty_row = None;
        ed.bs_mut().wrap_lines = 0;
        ed.ensure_wrap_prefix();
        assert_eq!(
            ed.bs().wrap_prefix,
            incremental,
            "row {row}: the incremental table disagrees with a full rebuild"
        );
        assert_eq!(ed.bs().wrap_rows, rows, "row {row}: row geometry differs");
    }
}

#[test]
fn a_row_that_stops_wrapping_is_still_measured_once() {
    // The dirty-row path must notice a row that shrinks back under the
    // viewport, not just one that grows past it.
    let mut ed = test_ed(&format!("{}\nshort", "a".repeat(30)));
    ed.show_line_numbers = false;
    ed.text_w = 10;
    ed.ensure_wrap_prefix();
    assert_eq!(
        ed.bs().wrap_prefix,
        vec![0, 3, 4],
        "30 cols → 3 visual rows"
    );
    ed.bs_mut().cursor = Pos { row: 0, col: 30 };
    for _ in 0..25 {
        press(&mut ed, KeyCode::Backspace, KeyModifiers::NONE);
    }
    ed.ensure_wrap_prefix();
    assert_eq!(ed.bs().wrap_prefix, vec![0, 1, 2], "5 cols → 1 visual row");
}

/// **Scrolling must re-highlight.** The style grid is a WINDOW over the
/// viewport, and until now nothing recomputed it when the view scrolled:
/// `highlight_dirty` is set by edits and loads and by no scroll path, so
/// `ensure_highlight` returned early for ever after the first frame and every
/// newly revealed row was drawn plain.
///
/// Found by comparing the operator's window (rano's own `TODO.md`, scrolled
/// to line 229) against a fresh open of the same file: everything above
/// ~row 260 was coloured, everything below was plain, and the boundary was
/// `first viewport + text_h + HIGHLIGHT_MARGIN`.
#[test]
fn scrolling_far_re_highlights_the_new_viewport() {
    let text: String = (0..600)
        .map(|i| format!("## Section {i}\n\n- [ ] task {i}\n\n"))
        .collect();
    let mut ed = named_ed(&text, "TODO.md");
    ed.show_line_numbers = false;
    ed.text_w = 60;
    ed.text_h = 20;
    ed.ensure_wrap_prefix();
    // The frame at the top of the file.
    ed.ensure_highlight();
    // Row 4 is `## Section 1` — the `##` is a keyword, so an answer here is
    // the grid actually being built. (`row 5` is the blank line after it,
    // where `None` is the correct answer and asserting on it would have been
    // a test that could not fail.)
    assert!(
        ed.bs().hl.style_at(Pos { row: 4, col: 0 }).is_some(),
        "the first viewport is highlighted"
    );

    // Scroll a long way WITHOUT touching the buffer, which is what PgDn and
    // the wheel do — no edit, so `highlight_dirty` stays false.
    let far = 1500;
    ed.bs_mut().scroll = far;
    ed.bs_mut().cursor = Pos { row: far, col: 0 };
    ed.ensure_wrap_prefix();
    ed.ensure_highlight();

    // The window must cover the viewport. This is the assertion that fails
    // without the fix: the window would still be the one built for the top.
    let win = ed
        .bs()
        .hl
        .styled_window()
        .expect("a window, not a whole-document refresh");
    assert!(
        win.rows.0 <= far && far + ed.text_h <= win.rows.1 + 1,
        "the highlight window {win:?} does not cover the viewport at \
         {far}..{} — those rows would be drawn plain",
        far + ed.text_h
    );
}

#[test]
fn an_edit_highlights_on_the_frame_not_per_key() {
    // The open cost, and the reason a burst of typing costs one highlight
    // rather than one per keystroke. A big buffer is highlighted by
    // viewport window and only when a frame asks for it, so nothing is
    // parsed at open and nothing is parsed by the edit itself.
    let text: String = (0..60_000)
        .map(|i| format!("pub fn f{i}(x: usize) -> usize {{ x + {i} }}\n"))
        .collect();
    assert!(text.len() > 2 << 20, "{} bytes", text.len());
    let mut ed = test_ed(&text);
    // A name, so the highlighter has a language at all (`detect` works
    // from the name and the shebang).
    ed.bs_mut().buf.name = Some(std::path::PathBuf::from("big.rs"));
    ed.show_line_numbers = false;
    ed.text_w = 100;
    ed.text_h = 40;

    // Opening parses nothing: the grid is empty until a frame asks.
    assert_eq!(
        ed.bs().hl.styled_window(),
        None,
        "open must not highlight a whole big file"
    );
    assert_eq!(ed.bs().hl.style_at(Pos { row: 5, col: 0 }), None);

    // The frame's call: a viewport window, not the document.
    ed.ensure_highlight();
    let win = ed
        .bs()
        .hl
        .styled_window()
        .expect("a window, not the whole buffer");
    let rows = win.rows.1 - win.rows.0;
    assert!(
        rows < 500,
        "the window must be the viewport plus a margin, got {rows} rows of {}",
        ed.bs().buf.row_count()
    );
    assert!(ed.bs().hl.style_at(Pos { row: 5, col: 0 }).is_some());

    // An edit marks the grid stale and does NOT re-highlight; the next
    // frame does, once, however many keys arrived in between.
    ed.bs_mut().cursor = Pos { row: 3, col: 0 };
    for _ in 0..5 {
        ed.insert_char('y');
    }
    // Five keystrokes, one highlight — and it happens when asked.
    ed.ensure_highlight();
    assert!(ed.bs().hl.styled_window().is_some());
    // Idempotent: asking twice is free and changes nothing.
    let before = ed.bs().hl.styled_window();
    ed.ensure_highlight();
    assert_eq!(ed.bs().hl.styled_window(), before);
}

#[test]
fn wrap_disables_horizontal_scroll() {
    let mut ed = test_ed(&"a".repeat(40));
    ed.show_line_numbers = false;
    ed.text_w = 10;
    ed.bs_mut().cursor = Pos { row: 0, col: 40 };
    ed.adjust_scroll_x();
    assert_eq!(
        ed.bs().scroll_x,
        0,
        "wrap on: lines wrap, no sideways scroll"
    );
    ed.wrap = false;
    ed.adjust_scroll_x();
    assert_eq!(ed.bs().scroll_x, 31);
}

#[test]
fn adjust_scroll_keeps_cursor_visible_with_wrap() {
    // 10 rows × 30 cols, view 10 wide → 3 visual rows per row, 30 total.
    let text: String = (0..10)
        .map(|i| format!("{}{}\n", i, "a".repeat(29)))
        .collect();
    let mut ed = test_ed(&text);
    ed.show_line_numbers = false;
    ed.text_w = 10;
    ed.text_h = 6;
    ed.bs_mut().cursor = Pos { row: 5, col: 29 }; // visual row 5*3 + 2 = 17
    ed.adjust_scroll(ed.text_h);
    // max_scroll = 30 - 6 = 24; scroll = 17 - 6 + 1 = 12
    assert_eq!(ed.bs().scroll, 12);
}

#[test]
fn wheel_scrolls_visual_rows_with_wrap() {
    let text: String = (0..10)
        .map(|i| format!("{}{}\n", i, "a".repeat(29)))
        .collect();
    let mut ed = test_ed(&text);
    ed.show_line_numbers = false;
    ed.text_w = 10;
    ed.text_h = 6;
    // 30 visual rows; the cursor (visual 0) is pinned to the top edge.
    assert!(ed.handle_mouse(me(MouseEventKind::ScrollDown, 0, 0)));
    assert_eq!(ed.bs().scroll, 3);
    assert_eq!(
        ed.bs().cursor,
        Pos { row: 1, col: 0 },
        "pinned to the viewport top"
    );
    assert!(ed.handle_mouse(me(MouseEventKind::ScrollDown, 0, 0)));
    assert_eq!(ed.bs().scroll, 6);
    assert_eq!(ed.bs().cursor, Pos { row: 2, col: 0 });
}

#[test]
fn move_up_down_cross_wrap_segments() {
    let mut ed = test_ed(&format!("{}\n{}", "a".repeat(25), "b".repeat(5)));
    ed.show_line_numbers = false;
    ed.text_w = 10;
    // row 0 occupies visual rows 0..3; the cursor starts on row 1 (visual 3).
    ed.bs_mut().cursor = Pos { row: 1, col: 2 };
    press(&mut ed, KeyCode::Up, KeyModifiers::NONE);
    assert_eq!(
        ed.bs().cursor,
        Pos { row: 0, col: 22 },
        "up: same col on the visual row above"
    );
    press(&mut ed, KeyCode::Up, KeyModifiers::NONE);
    assert_eq!(ed.bs().cursor, Pos { row: 0, col: 12 });
    press(&mut ed, KeyCode::Up, KeyModifiers::NONE);
    assert_eq!(ed.bs().cursor, Pos { row: 0, col: 2 });
    press(&mut ed, KeyCode::Up, KeyModifiers::NONE);
    assert_eq!(
        ed.bs().cursor,
        Pos { row: 0, col: 2 },
        "top of the buffer: no move"
    );
    press(&mut ed, KeyCode::Down, KeyModifiers::NONE);
    assert_eq!(ed.bs().cursor, Pos { row: 0, col: 12 });
    press(&mut ed, KeyCode::Down, KeyModifiers::NONE);
    assert_eq!(ed.bs().cursor, Pos { row: 0, col: 22 });
    press(&mut ed, KeyCode::Down, KeyModifiers::NONE);
    assert_eq!(
        ed.bs().cursor,
        Pos { row: 1, col: 2 },
        "down: crosses into the next buffer row"
    );
}

#[test]
fn home_end_use_visual_rows_with_wrap() {
    let mut ed = test_ed(&"a".repeat(25));
    ed.show_line_numbers = false;
    ed.text_w = 10;
    ed.bs_mut().cursor = Pos { row: 0, col: 15 }; // visual row 1
    press(&mut ed, KeyCode::Home, KeyModifiers::NONE);
    assert_eq!(
        ed.bs().cursor,
        Pos { row: 0, col: 10 },
        "Home = start of the visual row"
    );
    press(&mut ed, KeyCode::End, KeyModifiers::NONE);
    assert_eq!(
        ed.bs().cursor,
        Pos { row: 0, col: 20 },
        "End = end of the visual row"
    );
}

#[test]
fn page_keys_move_visual_rows_with_wrap() {
    let text: String = (0..10)
        .map(|i| format!("{}{}\n", i, "a".repeat(29)))
        .collect();
    let mut ed = test_ed(&text);
    ed.show_line_numbers = false;
    ed.text_w = 10;
    ed.text_h = 6;
    ed.bs_mut().cursor = Pos { row: 5, col: 0 }; // visual row 15
    press(&mut ed, KeyCode::PageUp, KeyModifiers::NONE);
    // step 5 → visual row 10 = row 3, segment 1, col 0 → char col 10
    assert_eq!(ed.bs().cursor, Pos { row: 3, col: 10 });
    press(&mut ed, KeyCode::PageDown, KeyModifiers::NONE);
    // back to visual row 15 = row 5, segment 0
    assert_eq!(ed.bs().cursor, Pos { row: 5, col: 0 });
}

#[test]
fn mouse_pos_maps_visual_rows_with_wrap() {
    let mut ed = test_ed(&format!("{}\n{}", "a".repeat(25), "b".repeat(5)));
    ed.show_line_numbers = false;
    ed.text_w = 10;
    ed.text_h = 10;
    // pane row 2 = visual row 1 = row 0, segment 1; col 3 → display col 13.
    assert!(ed.handle_mouse(me(MouseEventKind::Down(MouseButton::Left), 2, 3)));
    assert_eq!(ed.bs().cursor, Pos { row: 0, col: 13 });
    // pane row 4 = visual row 3 = row 1, segment 0.
    assert!(ed.handle_mouse(me(MouseEventKind::Down(MouseButton::Left), 4, 2)));
    assert_eq!(ed.bs().cursor, Pos { row: 1, col: 2 });
}

// D6 — tick_status reports whether it cleared visible state.

#[test]
fn tick_status_reports_clear() {
    let mut ed = test_ed("hi");
    ed.status = Some(Flash {
        text: "x".to_string(),
        until: Instant::now() - Duration::from_secs(1),
    });
    assert!(ed.tick_status());
    assert!(ed.status.is_none());
    assert!(!ed.tick_status());
}

// F1 — Editor::new takes a Config; tab_width/line_numbers seed from it.

#[test]
fn config_tab_width_used() {
    let mut buf = Buffer::new();
    buf.set_rows(vec!["a\tb".chars().collect()]);
    let cfg = config::Config {
        tab_width: 4,
        ..config::Config::default()
    };
    let mut ed = Editor::new(buf, cfg);
    assert_eq!(ed.tab_width, 4);
    ed.wrap = false; // horizontal scrolling needs wrap off
    ed.show_line_numbers = false;
    ed.text_w = 4;
    ed.bs_mut().cursor = Pos { row: 0, col: 3 };
    ed.adjust_scroll_x();
    // "a\tb" at tw 4 is 5 display cols; 5 >= 0 + 4 → scroll_x = 5 + 1 - 4
    assert_eq!(ed.bs().scroll_x, 2);
}

#[test]
fn config_line_numbers_seeded() {
    let cfg = config::Config {
        line_numbers: true,
        ..config::Config::default()
    };
    let ed = Editor::new(Buffer::new(), cfg);
    assert!(ed.show_line_numbers);
    assert_eq!(ed.config.tab_width, 8);
}

// F3 — auto-indent carries the current line's leading whitespace onto
// the new row (only when config.auto_indent is on).

#[test]
fn auto_indent_copies_indent() {
    let mut buf = Buffer::new();
    buf.set_rows(vec!["    foo".chars().collect()]);
    let cfg = config::Config {
        auto_indent: true,
        ..config::Config::default()
    };
    let mut ed = Editor::new(buf, cfg);
    ed.bs_mut().cursor = Pos { row: 0, col: 7 };
    press(&mut ed, KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(lines(&ed), vec!["    foo", "    "]);
    assert_eq!(ed.bs().cursor, Pos { row: 1, col: 4 });
}

/// **The one-pass scan must agree with the obvious one.** The early exit at
/// GCD 1 is exact (a GCD cannot rise), but "exact" is a claim, so it is
/// checked against a reference over documents chosen to reach every branch:
/// no indentation at all, a tab anywhere, a single width, a 1 that appears
/// only at the END (the case the early exit has to get right), a mixed 4/8,
/// and a unit past the 8-space fallback.
#[test]
fn indent_unit_matches_the_reference_scan() {
    /// The literal reading: collect every count, sort, fold the GCD.
    fn reference(text: &str) -> String {
        let lines: Vec<&str> = text.split('\n').collect();
        if lines.iter().any(|l| l.starts_with('\t')) {
            return "\t".to_string();
        }
        let mut counts: Vec<usize> = lines
            .iter()
            .filter_map(|l| {
                let n = l.len() - l.trim_start_matches(' ').len();
                (n > 0).then_some(n)
            })
            .collect();
        if counts.is_empty() {
            return "\t".to_string();
        }
        counts.sort();
        let mut unit = counts[0];
        for n in &counts {
            let (mut a, mut b) = (unit, *n);
            while b != 0 {
                let t = b;
                b = a % b;
                a = t;
            }
            unit = a;
        }
        if unit == 0 || unit > 8 {
            return "\t".to_string();
        }
        " ".repeat(unit)
    }

    for (label, text) in [
        ("flat", "a\nb\nc"),
        ("tab", "a\n\tb"),
        ("tab late", &format!("{}x\n\tlate", "a\n".repeat(500))),
        ("four", "    a\n    b\n        c"),
        ("eight", "        a\n        b"),
        ("mixed 4/8", "    a\n        b\n    c"),
        ("two", "  a\n    b\n  c"),
        ("one early", " a\n  b\n    c"),
        ("one last", &format!("{}\n a", "    row\n".repeat(200))),
        ("nine", "         a\n         b"),
        ("blank lines", "\n\n    a\n\n"),
        ("single", "    only"),
    ] {
        let ed = test_ed(text);
        assert_eq!(
            ed.indent_unit(),
            reference(text),
            "{label}: one-pass disagrees with the reference"
        );
    }
}

#[test]
fn auto_indent_off_by_config() {
    let mut ed = test_ed("    foo");
    ed.config.auto_indent = false;
    ed.bs_mut().cursor = Pos { row: 0, col: 7 };
    press(&mut ed, KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(lines(&ed), vec!["    foo", ""]);
    assert_eq!(ed.bs().cursor, Pos { row: 1, col: 0 });
}

#[test]
fn auto_indent_on_by_default_and_electric_after_brace() {
    // Default config: Enter carries the indent...
    let mut ed = test_ed("    foo");
    ed.bs_mut().cursor = Pos { row: 0, col: 7 };
    press(&mut ed, KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(lines(&ed), vec!["    foo", "    "]);
    assert_eq!(ed.bs().cursor, Pos { row: 1, col: 4 });
    // ...and an opening brace indents one unit deeper.
    let mut ed = test_ed("    if x {");
    ed.bs_mut().cursor = Pos { row: 0, col: 10 };
    press(&mut ed, KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(lines(&ed), vec!["    if x {", "        "]);
    assert_eq!(ed.bs().cursor, Pos { row: 1, col: 8 });
}

#[test]
fn mouse_click_drag_and_wheel() {
    let mut ed = test_ed("hello\nworld\n3\n4\n5\n6\n");
    ed.show_line_numbers = true;
    ed.text_w = 40;
    ed.text_h = 10;
    // Click on "world" (pane row 2, col 2; gutter is 3 wide).
    assert!(ed.handle_mouse(me(MouseEventKind::Down(MouseButton::Left), 2, 5)));
    assert_eq!(ed.bs().cursor, Pos { row: 1, col: 2 });
    assert_eq!(ed.bs().mark, Some(Pos { row: 1, col: 2 }));
    // Drag extends the selection (pane col 4 → disp 1 → char col 1).
    assert!(ed.handle_mouse(me(MouseEventKind::Drag(MouseButton::Left), 2, 4)));
    assert_eq!(ed.bs().cursor, Pos { row: 1, col: 1 });
    assert_eq!(ed.bs().mark, Some(Pos { row: 1, col: 2 }));
    // Title row and status/bar rows are ignored.
    assert!(!ed.handle_mouse(me(MouseEventKind::Down(MouseButton::Left), 0, 3)));
    assert!(!ed.handle_mouse(me(MouseEventKind::Down(MouseButton::Left), 22, 3)));
}

// Wheel — viewport scrolls without moving the edit point; the cursor is
// pulled along only when the scroll would push it out of the view.
#[test]
fn mouse_wheel_scrolls_view_not_cursor() {
    let text: String = (1..=30).map(|i| format!("L{i}\n")).collect();
    let mut ed = test_ed(&text);
    ed.text_h = 10; // max_scroll = 30 - 10 = 20
    ed.bs_mut().cursor = Pos { row: 4, col: 1 };
    // Wheel down: the viewport moves, the edit point stays.
    assert!(ed.handle_mouse(me(MouseEventKind::ScrollDown, 0, 0)));
    assert_eq!(ed.bs().scroll, 3);
    assert_eq!(ed.bs().cursor.row, 4, "wheel must not move the cursor");
    // Scrolling past the cursor pins it to the top edge of the view.
    assert!(ed.handle_mouse(me(MouseEventKind::ScrollDown, 0, 0)));
    assert_eq!(ed.bs().scroll, 6);
    assert_eq!(ed.bs().cursor.row, 6, "cursor pinned to the viewport top");
    // Wheel up: the viewport moves back, the cursor stays.
    assert!(ed.handle_mouse(me(MouseEventKind::ScrollUp, 0, 0)));
    assert_eq!(ed.bs().scroll, 3);
    assert_eq!(ed.bs().cursor.row, 6);
    // Cursor on the bottom row of the view: scrolling up pins it there.
    ed.bs_mut().cursor = Pos { row: 12, col: 1 };
    assert!(ed.handle_mouse(me(MouseEventKind::ScrollUp, 0, 0)));
    assert_eq!(ed.bs().scroll, 0);
    assert_eq!(
        ed.bs().cursor.row,
        9,
        "cursor pinned to the viewport bottom"
    );
    // Clamped at the top of the file.
    assert!(ed.handle_mouse(me(MouseEventKind::ScrollUp, 0, 0)));
    assert_eq!(ed.bs().scroll, 0);
    assert_eq!(ed.bs().cursor.row, 9);
    // Clamped at the end of the file.
    ed.bs_mut().scroll = 20;
    ed.bs_mut().cursor = Pos { row: 25, col: 1 };
    assert!(ed.handle_mouse(me(MouseEventKind::ScrollDown, 0, 0)));
    assert_eq!(ed.bs().scroll, 20, "scroll clamped at end of file");
    assert_eq!(ed.bs().cursor.row, 25);
}

// E4a — expand_tilde: only a leading ~ (exactly "~" or "~/") expands.

#[test]
fn expand_tilde_home() {
    let home = std::env::var("HOME").unwrap_or_default();
    assert_eq!(expand_tilde("~"), home);
    assert_eq!(expand_tilde("~/x"), format!("{home}/x"));
}

#[test]
fn expand_tilde_leaves_paths() {
    assert_eq!(expand_tilde("/a"), "/a");
    assert_eq!(expand_tilde("x~"), "x~");
    assert_eq!(expand_tilde("~user/x"), "~user/x");
    assert_eq!(expand_tilde(""), "");
}

// E4b — prompt history: Up cycles back, Down forward, past the newest
// entry restores the text captured when cycling began.

#[test]
fn search_history_cycles() {
    let mut ed = test_ed("bar");
    press(&mut ed, KeyCode::Char('f'), KeyModifiers::CONTROL);
    press_text(&mut ed, "foo");
    press(&mut ed, KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(ed.search_hist, vec!["foo".to_string()]);
    // no matches → ^F re-opens the prompt seeded with the old query
    press(&mut ed, KeyCode::Char('f'), KeyModifiers::CONTROL);
    for _ in 0..3 {
        press(&mut ed, KeyCode::Backspace, KeyModifiers::NONE);
    }
    press(&mut ed, KeyCode::Up, KeyModifiers::NONE);
    let p = ed.prompt.as_ref().unwrap();
    assert_eq!(p.text, "foo");
    assert_eq!(p.cursor, 3);
    press(&mut ed, KeyCode::Down, KeyModifiers::NONE);
    let p = ed.prompt.as_ref().unwrap();
    assert_eq!(p.text, "", "past the newest entry restores the draft");
    press(&mut ed, KeyCode::Up, KeyModifiers::NONE);
    press(&mut ed, KeyCode::Up, KeyModifiers::NONE);
    let p = ed.prompt.as_ref().unwrap();
    assert_eq!(p.text, "foo", "stays on the oldest entry");
    assert!(ed.prompt.is_some());
}

#[test]
fn prompt_history_dedupes_consecutive() {
    let mut ed = test_ed("");
    for _ in 0..2 {
        press(&mut ed, KeyCode::Char('t'), KeyModifiers::CONTROL);
        press_text(&mut ed, "true");
        press(&mut ed, KeyCode::Enter, KeyModifiers::NONE);
    }
    assert_eq!(ed.exec_hist, vec!["true".to_string()]);
}

#[test]
fn prompt_history_skips_empty() {
    let mut ed = test_ed("");
    press(&mut ed, KeyCode::Char('o'), KeyModifiers::CONTROL);
    press(&mut ed, KeyCode::Enter, KeyModifiers::NONE);
    assert!(ed.file_hist.is_empty());
}

// E4c — Tab completes file paths in the path prompts.

#[test]
fn complete_path_fixture() {
    let d = temp_dir("cmpl");
    fs::write(d.0.join("alpha.txt"), "").unwrap();
    fs::create_dir_all(d.0.join("alphabet")).unwrap();
    let base = format!("{}/", d.0.display());
    let (c, opts) = complete_path(&format!("{base}alpha.txt")).unwrap();
    assert_eq!(c, format!("{base}alpha.txt"));
    assert_eq!(opts, vec!["alpha.txt".to_string()]);
    let (c, opts) = complete_path(&format!("{base}alphabet")).unwrap();
    assert_eq!(
        c,
        format!("{base}alphabet/"),
        "unique dir gains a trailing /"
    );
    assert_eq!(opts, vec!["alphabet/".to_string()]);
    let (c, opts) = complete_path(&format!("{base}alp")).unwrap();
    assert_eq!(c, format!("{base}alpha"));
    assert_eq!(opts, vec!["alpha.txt".to_string(), "alphabet/".to_string()]);
    let (c, opts) = complete_path(&format!("{base}a")).unwrap();
    assert_eq!(c, format!("{base}alpha"));
    assert_eq!(opts.len(), 2);
    assert!(complete_path(&format!("{base}zzz")).is_none());
}

#[test]
fn tab_completes_in_prompt() {
    let d = temp_dir("cmpl_prompt");
    fs::write(d.0.join("alpha.txt"), "").unwrap();
    fs::create_dir_all(d.0.join("alphabet")).unwrap();
    let mut ed = test_ed("");
    press(&mut ed, KeyCode::Char('o'), KeyModifiers::CONTROL);
    press_text(&mut ed, &format!("{}/alp", d.0.display()));
    press(&mut ed, KeyCode::Tab, KeyModifiers::NONE);
    let p = ed.prompt.as_ref().unwrap();
    assert_eq!(p.text, format!("{}/alpha", d.0.display()));
    assert!(ed.prompt.is_some());
}

// E4d — M-b / M-f / Ctrl+Left / Ctrl+Right are word motion inside
// prompts (they must not type letters or move one char).

#[test]
fn prompt_word_motion() {
    let mut ed = test_ed("");
    press(&mut ed, KeyCode::Char('t'), KeyModifiers::CONTROL);
    press_text(&mut ed, "foo bar_baz");
    press(&mut ed, KeyCode::Home, KeyModifiers::NONE);
    press(&mut ed, KeyCode::Char('f'), KeyModifiers::ALT);
    assert_eq!(ed.prompt.as_ref().unwrap().cursor, 4);
    press(&mut ed, KeyCode::Char('f'), KeyModifiers::ALT);
    assert_eq!(ed.prompt.as_ref().unwrap().cursor, 11);
    press(&mut ed, KeyCode::Char('b'), KeyModifiers::ALT);
    assert_eq!(ed.prompt.as_ref().unwrap().cursor, 4);
    press(&mut ed, KeyCode::Char('b'), KeyModifiers::ALT);
    assert_eq!(ed.prompt.as_ref().unwrap().cursor, 0);
    assert!(ed.prompt.is_some());
}

#[test]
fn prompt_ctrl_arrow_word_motion() {
    let mut ed = test_ed("");
    press(&mut ed, KeyCode::Char('t'), KeyModifiers::CONTROL);
    press_text(&mut ed, "foo bar_baz");
    press(&mut ed, KeyCode::Home, KeyModifiers::NONE);
    press(&mut ed, KeyCode::Right, KeyModifiers::CONTROL);
    assert_eq!(ed.prompt.as_ref().unwrap().cursor, 4);
    press(&mut ed, KeyCode::Left, KeyModifiers::CONTROL);
    assert_eq!(ed.prompt.as_ref().unwrap().cursor, 0);
    assert!(ed.prompt.is_some());
}

// F5 — search wiring: Matcher semantics (regex + case toggle), invalid
// regex flashes, M-C / M-R toggles in the search prompt only.

#[test]
fn regex_search_matches_line_starts() {
    let mut ed = test_ed("ba\naba\nbar");
    ed.search_regex = true;
    press(&mut ed, KeyCode::Char('f'), KeyModifiers::CONTROL);
    press_text(&mut ed, "^ba");
    press(&mut ed, KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(ed.bs().cursor, Pos { row: 0, col: 0 });
    press(&mut ed, KeyCode::Char('f'), KeyModifiers::ALT);
    assert_eq!(
        ed.bs().cursor,
        Pos { row: 2, col: 0 },
        "aba must be skipped"
    );
}

#[test]
fn case_toggle_search() {
    let mut ed = test_ed("foo\nFOO");
    press(&mut ed, KeyCode::Char('f'), KeyModifiers::CONTROL);
    press_text(&mut ed, "FOO");
    press(&mut ed, KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(
        ed.bs().cursor,
        Pos { row: 0, col: 0 },
        "default is case-insensitive"
    );
    press(&mut ed, KeyCode::Char('f'), KeyModifiers::ALT);
    assert_eq!(ed.bs().cursor, Pos { row: 1, col: 0 });
    ed.bs_mut().search_matches = None; // let ^F re-open the prompt
    press(&mut ed, KeyCode::Char('f'), KeyModifiers::CONTROL);
    press(&mut ed, KeyCode::Char('c'), KeyModifiers::ALT);
    assert!(ed.search_case_sensitive);
    press(&mut ed, KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(ed.bs().cursor, Pos { row: 1, col: 0 });
    assert_eq!(
        ed.bs().search_matches.as_ref().unwrap().len(),
        1,
        "case-sensitive search sees only FOO"
    );
}

#[test]
fn invalid_regex_flashes() {
    let mut ed = test_ed("abc");
    ed.search_regex = true;
    press(&mut ed, KeyCode::Char('f'), KeyModifiers::CONTROL);
    press_text(&mut ed, "[");
    press(&mut ed, KeyCode::Enter, KeyModifiers::NONE);
    let st = ed.status_text().unwrap();
    assert!(st.contains("regex"), "status: {st}");
    assert_eq!(
        ed.bs().cursor,
        Pos { row: 0, col: 0 },
        "no jump on invalid regex"
    );
    assert!(ed.bs().search_matches.is_none());
}

#[test]
fn m_c_m_r_toggle_in_prompt() {
    let mut ed = test_ed("");
    press(&mut ed, KeyCode::Char('f'), KeyModifiers::CONTROL);
    press_text(&mut ed, "q");
    press(&mut ed, KeyCode::Char('c'), KeyModifiers::ALT);
    assert!(ed.search_case_sensitive);
    assert!(
        ed.status_text().unwrap().contains("Case sensitive: on"),
        "status: {}",
        ed.status_text().unwrap()
    );
    press(&mut ed, KeyCode::Char('c'), KeyModifiers::ALT);
    assert!(!ed.search_case_sensitive);
    press(&mut ed, KeyCode::Char('r'), KeyModifiers::ALT);
    assert!(ed.search_regex);
    assert!(ed.status_text().unwrap().contains("Regex: on"));
    press(&mut ed, KeyCode::Char('r'), KeyModifiers::ALT);
    assert!(!ed.search_regex);
    let p = ed.prompt.as_ref().unwrap();
    assert_eq!(p.text, "q", "toggle leaves prompt text unchanged");
    assert_eq!(p.cursor, 1);
    // Toggles must not leak into other prompt kinds: M-C types 'c' there.
    press(&mut ed, KeyCode::Enter, KeyModifiers::NONE);
    press(&mut ed, KeyCode::Char('o'), KeyModifiers::CONTROL);
    press(&mut ed, KeyCode::Char('c'), KeyModifiers::ALT);
    assert!(!ed.search_case_sensitive);
    let p = ed.prompt.as_ref().unwrap();
    assert!(p.text.contains('c'), "M-C must type in a WriteName prompt");
}

// F8 — multi-buffer: open pushes/replaces, M-</M-> switch with per-buffer
// state, title index, and ^X cycling over modified buffers.

fn buf_with(text: &str) -> Buffer {
    let mut b = Buffer::new();
    b.set_rows(text.lines().map(|l| l.chars().collect()).collect());
    b
}

#[test]
fn open_file_multibuffer_pushes() {
    let d = temp_dir("open_multi");
    let f1 = d.0.join("a.txt");
    let f2 = d.0.join("b.txt");
    fs::write(&f1, "one\ntwo").unwrap();
    fs::write(&f2, "x\ny\nz").unwrap();
    let cfg = config::Config {
        multibuffer: true,
        ..config::Config::default()
    };
    let mut ed = Editor::new(buf_with("seed"), cfg);
    press(&mut ed, KeyCode::F(8), KeyModifiers::NONE);
    assert!(ed.prompt.is_some());
    press_text(&mut ed, &f2.display().to_string());
    press(&mut ed, KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(ed.buffers.len(), 2);
    assert_eq!(ed.cur, 1);
    assert_eq!(lines(&ed), vec!["x", "y", "z"]);
    assert_eq!(ed.bs().buf.name, Some(f2));
}

#[test]
fn open_file_replaces_when_disabled() {
    let d = temp_dir("open_single");
    let f1 = d.0.join("a.txt");
    fs::write(&f1, "x\ny\nz").unwrap();
    let mut ed = test_ed("seed");
    press(&mut ed, KeyCode::F(8), KeyModifiers::NONE);
    press_text(&mut ed, &f1.display().to_string());
    press(&mut ed, KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(ed.buffers.len(), 1);
    assert_eq!(ed.cur, 0);
    assert_eq!(lines(&ed), vec!["x", "y", "z"]);
    assert_eq!(ed.bs().buf.name, Some(f1));
}

#[test]
fn open_file_keeps_a_buffer_with_unsaved_edits_when_disabled() {
    let d = temp_dir("open_single_modified");
    let f1 = d.0.join("a.txt");
    fs::write(&f1, "x\ny\nz").unwrap();
    let mut ed = test_ed("seed");
    press_text(&mut ed, "EDIT ");
    assert!(ed.bs().buf.modified);
    press(&mut ed, KeyCode::F(8), KeyModifiers::NONE);
    press_text(&mut ed, &f1.display().to_string());
    press(&mut ed, KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(ed.buffers.len(), 2, "the edited buffer was not replaced");
    assert_eq!(lines(&ed), vec!["x", "y", "z"]);
    assert!(ed.status_text().unwrap().contains("unsaved edits"));
    assert!(!ed.config.multibuffer, "the setting is unchanged");
    assert_eq!(
        ed.buffers[0].buf.row(0).iter().collect::<String>(),
        "EDIT seed"
    );
    // Back in an unmodified buffer, the next open replaces as before.
    let f2 = d.0.join("b.txt");
    fs::write(&f2, "b").unwrap();
    press(&mut ed, KeyCode::F(8), KeyModifiers::NONE);
    press_text(&mut ed, &f2.display().to_string());
    press(&mut ed, KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(ed.buffers.len(), 2);
    assert_eq!(lines(&ed), vec!["b"]);
}

#[test]
fn open_missing_file_flashes() {
    let mut ed = test_ed("seed");
    press(&mut ed, KeyCode::F(8), KeyModifiers::NONE);
    press_text(&mut ed, "/no/such/file/rano_open_test.txt");
    press(&mut ed, KeyCode::Enter, KeyModifiers::NONE);
    let st = ed.status_text().unwrap();
    assert!(st.contains("Error:"), "status: {st}");
    assert!(ed.prompt.is_some(), "the open prompt stays open on error");
    assert_eq!(lines(&ed), vec!["seed"]);
}

#[test]
fn switch_buffers_wrap() {
    let mut ed = test_ed("a");
    ed.buffers.push(BufferState::new(buf_with("b")));
    press(&mut ed, KeyCode::Char('>'), KeyModifiers::ALT);
    assert_eq!(ed.cur, 1);
    let st = ed.status_text().unwrap();
    assert!(st.contains("Buffer:"), "status: {st}");
    press(&mut ed, KeyCode::Char('>'), KeyModifiers::ALT);
    assert_eq!(ed.cur, 0, "wraps past the last");
    press(&mut ed, KeyCode::Char('<'), KeyModifiers::ALT);
    assert_eq!(ed.cur, 1, "wraps back past the first");
}

#[test]
fn per_buffer_state_isolated() {
    let mut ed = test_ed("a\nb");
    ed.buffers.push(BufferState::new(buf_with("x\ny")));
    ed.cur = 1;
    press(&mut ed, KeyCode::Down, KeyModifiers::NONE);
    press(&mut ed, KeyCode::Right, KeyModifiers::NONE);
    assert_eq!(ed.bs().cursor, Pos { row: 1, col: 1 });
    press(&mut ed, KeyCode::Char('>'), KeyModifiers::ALT);
    assert_eq!(
        ed.bs().cursor,
        Pos { row: 0, col: 0 },
        "buffer 1 keeps its own cursor"
    );
    press(&mut ed, KeyCode::Char('<'), KeyModifiers::ALT);
    assert_eq!(
        ed.bs().cursor,
        Pos { row: 1, col: 1 },
        "buffer 2's cursor preserved across the switch"
    );
    // Undo stacks are per buffer: each M-U hits only the current one.
    press_text(&mut ed, "Z");
    press(&mut ed, KeyCode::Char('>'), KeyModifiers::ALT);
    press_text(&mut ed, "Q");
    press(&mut ed, KeyCode::Char('u'), KeyModifiers::ALT);
    assert_eq!(lines(&ed), vec!["a", "b"], "buffer 1 undoes only its edit");
    press(&mut ed, KeyCode::Char('<'), KeyModifiers::ALT);
    press(&mut ed, KeyCode::Char('u'), KeyModifiers::ALT);
    assert_eq!(lines(&ed), vec!["x", "y"], "buffer 2 undoes only its edit");
}

#[test]
fn title_shows_index_when_multibuffer() {
    let mut ed = test_ed("a");
    ed.bs_mut().buf.name = Some(PathBuf::from("/tmp/a.txt"));
    assert_eq!(ed.title_text(), "/tmp/a.txt");
    let mut b2 = buf_with("b");
    b2.name = Some(PathBuf::from("/tmp/b.txt"));
    ed.buffers.push(BufferState::new(b2));
    assert_eq!(ed.title_text(), "[1/2] /tmp/a.txt");
    press(&mut ed, KeyCode::Char('>'), KeyModifiers::ALT);
    assert_eq!(ed.title_text(), "[2/2] /tmp/b.txt");
}

#[test]
fn quit_cycles_modified_buffers() {
    let mut ed = test_ed("a");
    ed.buffers.push(BufferState::new(buf_with("b")));
    ed.buffers[0].buf.modified = true;
    ed.buffers[1].buf.modified = true;
    press(&mut ed, KeyCode::Char('x'), KeyModifiers::CONTROL);
    assert!(matches!(
        ed.prompt.as_ref().map(|p| p.kind),
        Some(PromptKind::ConfirmSave)
    ));
    assert_eq!(ed.cur, 0);
    press(&mut ed, KeyCode::Char('n'), KeyModifiers::NONE);
    assert!(!ed.buffers[0].buf.modified, "'n' discards this buffer");
    assert_eq!(ed.cur, 1, "next modified buffer becomes current");
    assert!(matches!(
        ed.prompt.as_ref().map(|p| p.kind),
        Some(PromptKind::ConfirmSave)
    ));
    assert!(!ed.quit);
    press(&mut ed, KeyCode::Char('n'), KeyModifiers::NONE);
    assert!(!ed.buffers[1].buf.modified);
    assert!(ed.quit, "all modified buffers dealt with → quit");
}

#[test]
fn try_quit_other_modified() {
    let mut ed = test_ed("a");
    ed.buffers.push(BufferState::new(buf_with("b")));
    ed.buffers[1].buf.modified = true;
    press(&mut ed, KeyCode::Char('x'), KeyModifiers::CONTROL);
    assert_eq!(ed.cur, 1, "the modified buffer becomes current");
    assert!(matches!(
        ed.prompt.as_ref().map(|p| p.kind),
        Some(PromptKind::ConfirmSave)
    ));
    assert!(!ed.quit);
    press(&mut ed, KeyCode::Esc, KeyModifiers::NONE);
    assert!(!ed.quit, "cancel abandons the quit");
    assert!(ed.prompt.is_none());
}

#[test]
fn quit_save_cycles_to_next_modified() {
    let d = temp_dir("quit_cycle_save");
    let f1 = d.0.join("a.txt");
    let f2 = d.0.join("b.txt");
    let mut ed = test_ed("a");
    ed.bs_mut().buf.name = Some(f1.clone());
    ed.bs_mut().buf.modified = true;
    let mut b2 = buf_with("b");
    b2.name = Some(f2.clone());
    b2.modified = true;
    ed.buffers.push(BufferState::new(b2));
    press(&mut ed, KeyCode::Char('x'), KeyModifiers::CONTROL);
    press(&mut ed, KeyCode::Char('y'), KeyModifiers::NONE);
    assert!(f1.exists());
    assert!(!ed.buffers[0].buf.modified);
    assert_eq!(ed.cur, 1, "saved → next modified buffer prompted");
    assert!(matches!(
        ed.prompt.as_ref().map(|p| p.kind),
        Some(PromptKind::ConfirmSave)
    ));
    press(&mut ed, KeyCode::Char('y'), KeyModifiers::NONE);
    assert!(f2.exists());
    assert!(ed.quit, "last buffer saved → quit");
}

// ---------- buffers: close, CLI files, reuse by jumps ----------

fn named_buffers(names: &[&str]) -> Editor {
    let mut ed = test_ed("0");
    ed.config.multibuffer = true;
    ed.bs_mut().buf.name = Some(PathBuf::from(names[0]));
    for (i, n) in names.iter().enumerate().skip(1) {
        let mut b = buf_with(&i.to_string());
        b.name = Some(PathBuf::from(n));
        ed.buffers.push(BufferState::new(b));
    }
    ed
}

#[test]
fn close_buffer_removes_it_and_lands_on_the_next() {
    let mut ed = named_buffers(&["/tmp/rano_c0", "/tmp/rano_c1", "/tmp/rano_c2"]);
    ed.cur = 1;
    press(&mut ed, KeyCode::Char('w'), KeyModifiers::ALT);
    assert_eq!(ed.buffers.len(), 2);
    assert_eq!(ed.cur, 1);
    assert_eq!(
        lines(&ed),
        vec!["2"],
        "the following buffer takes its place"
    );
    press(&mut ed, KeyCode::Char('w'), KeyModifiers::ALT);
    assert_eq!(ed.cur, 0, "closing the last one lands on the new last");
    press(&mut ed, KeyCode::Char('w'), KeyModifiers::ALT);
    assert_eq!(ed.buffers.len(), 1, "the last buffer stays");
    assert!(ed.status_text().unwrap().contains("^X"));
}

#[test]
fn close_modified_buffer_asks_and_n_discards() {
    let mut ed = named_buffers(&["/tmp/rano_m0", "/tmp/rano_m1"]);
    ed.buffers[0].buf.modified = true;
    press(&mut ed, KeyCode::Char('w'), KeyModifiers::ALT);
    assert!(matches!(
        ed.prompt.as_ref().map(|p| p.kind),
        Some(PromptKind::ConfirmClose)
    ));
    press(&mut ed, KeyCode::Esc, KeyModifiers::NONE);
    assert_eq!(ed.buffers.len(), 2, "cancel keeps it");
    press(&mut ed, KeyCode::Char('w'), KeyModifiers::ALT);
    press(&mut ed, KeyCode::Char('n'), KeyModifiers::NONE);
    assert_eq!(ed.buffers.len(), 1);
    assert_eq!(lines(&ed), vec!["1"]);
}

#[test]
fn close_modified_buffer_y_saves_then_closes() {
    let d = temp_dir("close_save");
    let f = d.0.join("a.txt");
    let mut ed = named_buffers(&[f.to_str().unwrap(), "/tmp/rano_cs1"]);
    ed.buffers[0].buf.modified = true;
    press(&mut ed, KeyCode::Char('w'), KeyModifiers::ALT);
    press(&mut ed, KeyCode::Char('y'), KeyModifiers::NONE);
    assert_eq!(fs::read_to_string(&f).unwrap().trim_end(), "0");
    assert_eq!(ed.buffers.len(), 1);
    assert!(!ed.close_after_save);
}

#[test]
fn cancelling_the_file_name_of_a_close_forgets_the_close() {
    let mut ed = test_ed("scratch");
    ed.buffers.push(BufferState::new(buf_with("other")));
    ed.bs_mut().buf.modified = true;
    press(&mut ed, KeyCode::Char('w'), KeyModifiers::ALT);
    press(&mut ed, KeyCode::Char('y'), KeyModifiers::NONE);
    assert!(matches!(
        ed.prompt.as_ref().map(|p| p.kind),
        Some(PromptKind::WriteName)
    ));
    press(&mut ed, KeyCode::Esc, KeyModifiers::NONE);
    assert!(!ed.close_after_save, "a later ^O must not close the buffer");
    assert_eq!(ed.buffers.len(), 2);
}

#[test]
fn closing_a_buffer_fixes_the_jump_back_stack() {
    let mut ed = named_buffers(&["/tmp/rano_j0", "/tmp/rano_j1", "/tmp/rano_j2"]);
    ed.def_back.push(DefBack {
        buf: None,
        idx: Some(1),
        pos: Pos { row: 0, col: 0 },
    });
    ed.def_back.push(DefBack {
        buf: None,
        idx: Some(2),
        pos: Pos { row: 0, col: 1 },
    });
    ed.cur = 1;
    press(&mut ed, KeyCode::Char('w'), KeyModifiers::ALT);
    assert_eq!(
        ed.def_back.len(),
        1,
        "entries into the closed buffer are gone"
    );
    assert_eq!(ed.def_back[0].idx, Some(1), "later indices shift down");
    ed.cur = 0;
    press(&mut ed, KeyCode::Char(','), KeyModifiers::ALT);
    assert_eq!(lines(&ed), vec!["2"]);
}

#[test]
fn extra_files_are_named_now_and_read_when_visited() {
    let d = temp_dir("deferred");
    let b = d.0.join("b.txt");
    fs::write(&b, "bee\n").unwrap();
    let missing = d.0.join("new.txt");
    let mut ed = test_ed("a");
    ed.bs_mut().buf.name = Some(d.0.join("a.txt"));
    ed.add_deferred_buffers(&[b.clone(), missing.clone(), b.clone()]);
    assert_eq!(ed.buffers.len(), 3, "a file named twice gets one buffer");
    assert_eq!(ed.buffers[1].pending_load.as_ref(), Some(&b));
    assert!(
        ed.buffers[2].pending_load.is_none(),
        "a new file has nothing to read"
    );
    assert!(!ed.start_pending_load(), "buffer 0 has nothing pending");
    press(&mut ed, KeyCode::Char('>'), KeyModifiers::ALT);
    assert!(ed.start_pending_load());
    assert!(ed.bs().pending_load.is_none());
    for _ in 0..200 {
        ed.load_poll();
        if !ed.loading() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert_eq!(lines(&ed), vec!["bee"]);
}

#[test]
fn open_file_switches_to_an_already_open_buffer() {
    let d = temp_dir("open_dup");
    let f = d.0.join("a.txt");
    fs::write(&f, "disk").unwrap();
    let mut ed = named_buffers(&[f.to_str().unwrap(), "/tmp/rano_od1"]);
    ed.cur = 1;
    assert!(ed.open_file(f.to_str().unwrap()));
    assert_eq!(ed.buffers.len(), 2, "no second copy");
    assert_eq!(ed.cur, 0);
    assert_eq!(lines(&ed), vec!["0"], "the open buffer, not the disk text");
}

#[test]
fn definition_into_an_open_buffer_reuses_it() {
    let d = temp_dir("def_reuse");
    let src = d.0.join("src.rs");
    let tgt = d.0.join("tgt.rs");
    fs::write(&tgt, "fn t() {}\n").unwrap();
    let mut ed = named_buffers(&[src.to_str().unwrap(), tgt.to_str().unwrap()]);
    // An unsaved edit in the target buffer must be what the jump lands in.
    ed.buffers[1]
        .buf
        .set_rows(vec!["fn t() { edited }".chars().collect()]);
    let loc = lsp::DefLocation {
        uri: lsp::path_to_uri(&tgt),
        line: 0,
        character: 3,
    };
    ed.goto_location(loc, Pos { row: 0, col: 0 });
    assert_eq!(ed.buffers.len(), 2);
    assert_eq!(ed.cur, 1);
    assert_eq!(lines(&ed), vec!["fn t() { edited }"]);
    assert_eq!(ed.bs().cursor, Pos { row: 0, col: 3 });
    press(&mut ed, KeyCode::Char(','), KeyModifiers::ALT);
    assert_eq!(ed.cur, 0);
}

#[test]
fn definition_into_a_deferred_buffer_reads_it_first() {
    let d = temp_dir("def_deferred");
    let tgt = d.0.join("tgt.rs");
    fs::write(&tgt, "// one\n// two\nfn t() {}\n").unwrap();
    let mut ed = named_ed("fn a() {}\n", "/tmp/rano_def_deferred_src.rs");
    ed.config.multibuffer = true;
    ed.add_deferred_buffers(std::slice::from_ref(&tgt));
    let loc = lsp::DefLocation {
        uri: lsp::path_to_uri(&tgt),
        line: 2,
        character: 3,
    };
    ed.goto_location(loc, Pos { row: 0, col: 0 });
    assert_eq!(ed.cur, 1);
    assert!(ed.bs().pending_load.is_none());
    assert_eq!(ed.bs().cursor, Pos { row: 2, col: 3 });
    assert_eq!(lines(&ed).len(), 3);
}

#[test]
fn a_buffer_made_current_is_highlighted() {
    let dir = std::env::temp_dir().join("rano_hl_switch_fixture");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let a = dir.join("a.rs");
    let b = dir.join("b.rs");
    std::fs::write(&a, "fn a() {}\n").unwrap();
    std::fs::write(&b, "fn b() {}\n").unwrap();
    let mut ed = test_ed("");
    ed.config.multibuffer = true;
    ed.text_h = 10;
    let colored = |ed: &Editor| ed.bs().hl.style_at(Pos { row: 0, col: 0 }).is_some();
    // F8 into a new buffer.
    assert!(ed.open_file(a.to_str().unwrap()));
    ed.ensure_highlight();
    assert!(colored(&ed), "F8 opened a.rs uncoloured");
    assert!(ed.open_file(b.to_str().unwrap()));
    ed.ensure_highlight();
    assert!(colored(&ed), "F8 opened b.rs uncoloured");
    // Back to a.rs through the buffer switch, after its grid was dropped.
    let ia = ed.find_buffer(&a).unwrap();
    ed.buffers[ia].hl = Default::default();
    ed.set_current(ia);
    ed.ensure_highlight();
    assert!(colored(&ed), "switching to a.rs left it uncoloured");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn open_prompt_shows_live_path_hints() {
    let dir = std::env::temp_dir().join("rano_hints_fixture");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("sub")).unwrap();
    std::fs::write(dir.join("alpha.rs"), "").unwrap();
    std::fs::write(dir.join("beta.rs"), "").unwrap();
    let mut ed = test_ed("hidden text");
    let base = format!("{}/", dir.display());
    ed.prompt = Some(crate::prompt::Prompt {
        kind: PromptKind::OpenName,
        cursor: base.chars().count(),
        text: base.clone(),
    });
    assert!(ed.refresh_prompt_hints());
    assert!(!ed.refresh_prompt_hints());
    let backend = ratatui::backend::TestBackend::new(60, 12);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| ui::draw(f, &ed)).unwrap();
    let buf = term.backend().buffer().clone();
    let row = |y: u16| -> String { (0..60).map(|x| buf[(x, y)].symbol().to_string()).collect() };
    // status row is 12 - 3 = 9; the hints sit on the row above it.
    assert!(row(8).contains("alpha.rs  beta.rs  sub/"), "{}", row(8));
    // Typing narrows them.
    ed.prompt.as_mut().unwrap().text = format!("{base}b");
    assert!(ed.refresh_prompt_hints());
    assert_eq!(
        ed.prompt_hints.as_ref().unwrap().2,
        vec!["beta.rs".to_string()]
    );
    ed.prompt = None;
    assert!(ed.refresh_prompt_hints());
    assert!(ed.prompt_hints.is_none());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn hint_rows_cap_and_count_the_rest() {
    let names: Vec<String> = (0..20).map(|i| format!("file{i:02}.rs")).collect();
    let rows = ui::hint_rows(&names, 30, 2);
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|r| r.chars().count() == 30));
    assert!(rows[1].trim_end().ends_with("(+16)"), "{:?}", rows);
}

#[test]
fn picker_draws_over_the_text() {
    let mut ed = test_ed("hidden text");
    ed.bs_mut().buf.name = Some(PathBuf::from("/tmp/rano_draw_a"));
    let mut b = buf_with("x");
    b.name = Some(PathBuf::from("/tmp/rano_draw_b"));
    ed.buffers.push(BufferState::new(b));
    ed.open_buffer_list();
    let backend = ratatui::backend::TestBackend::new(60, 12);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| ui::draw(f, &ed)).unwrap();
    let buf = term.backend().buffer().clone();
    let row = |y: u16| -> String { (0..60).map(|x| buf[(x, y)].symbol().to_string()).collect() };
    assert!(row(1).contains("Buffers (2)"), "{}", row(1));
    assert!(row(2).contains("rano_draw_a"), "{}", row(2));
    assert!(row(3).contains("rano_draw_b"), "{}", row(3));
    assert!(!(1..9).any(|y| row(y).contains("hidden text")));
}
