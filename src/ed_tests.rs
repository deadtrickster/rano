//! The editor's behaviour tests: keys in, buffer and screen out.

use crate::BufferState;
use crate::buffer::Buffer;
use crate::buffer::Pos;
use crate::config;
use crate::editor::Editor;
use crate::editor::{DefBack, Flash};
use crate::prompt::{PromptKind, complete_path, expand_tilde};
use crate::render::Style;
use crate::style::Role;
use crate::term::{KeyCode, KeyEvent, Mods, MouseButton, MouseEvent, MouseKind};
use crate::{editor, lsp, ui};
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

fn press(ed: &mut Editor, code: KeyCode, mods: Mods) {
    ed.handle_key(KeyEvent::new(code, mods));
}

fn me(kind: MouseKind, row: u16, col: u16) -> MouseEvent {
    MouseEvent {
        kind,
        x: col,
        y: row,
        mods: Mods::NONE,
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

#[test]
fn move_left_bol_clamps_to_prev_row_end() {
    let mut ed = test_ed("ab\ncdef");
    ed.bs_mut().cursor = Pos { row: 1, col: 0 };
    press(&mut ed, KeyCode::Left, Mods::NONE);
    assert_eq!(ed.bs().cursor, Pos { row: 0, col: 2 });
    press(&mut ed, KeyCode::Backspace, Mods::NONE);
    assert_eq!(lines(&ed), vec!["a", "cdef"]);
}

#[test]
fn prompt_multibyte_backspace_no_panic() {
    let mut ed = test_ed("");
    press(&mut ed, KeyCode::Char('f'), Mods::CTRL);
    assert!(ed.prompt.is_some());
    press(&mut ed, KeyCode::Char('é'), Mods::NONE);
    press(&mut ed, KeyCode::Char('a'), Mods::NONE);
    press(&mut ed, KeyCode::Backspace, Mods::NONE);
    let p = ed.prompt.as_ref().unwrap();
    assert_eq!(p.text, "é");
    assert_eq!(p.cursor, 1);
}

#[test]
fn prompt_multibyte_insert_no_panic() {
    let mut ed = test_ed("");
    press(&mut ed, KeyCode::Char('f'), Mods::CTRL);
    press(&mut ed, KeyCode::Char('é'), Mods::NONE);
    press(&mut ed, KeyCode::Char('x'), Mods::NONE);
    let p = ed.prompt.as_ref().unwrap();
    assert_eq!(p.text, "éx");
    assert_eq!(p.cursor, 2);
}

#[test]
fn prompt_mid_string_multibyte_edit() {
    let mut ed = test_ed("");
    press(&mut ed, KeyCode::Char('f'), Mods::CTRL);
    press(&mut ed, KeyCode::Char('é'), Mods::NONE);
    press(&mut ed, KeyCode::Char('a'), Mods::NONE);
    press(&mut ed, KeyCode::Left, Mods::NONE);
    press(&mut ed, KeyCode::Char('x'), Mods::NONE);
    let p = ed.prompt.as_ref().unwrap();
    assert_eq!(p.text, "éxa");
    assert_eq!(p.cursor, 2);
}

#[test]
fn prompt_ascii_edit_still_works() {
    let mut ed = test_ed("");
    press(&mut ed, KeyCode::Char('f'), Mods::CTRL);
    for c in ['a', 'b', 'c'] {
        press(&mut ed, KeyCode::Char(c), Mods::NONE);
    }
    press(&mut ed, KeyCode::Backspace, Mods::NONE);
    press(&mut ed, KeyCode::Left, Mods::NONE);
    press(&mut ed, KeyCode::Char('x'), Mods::NONE);
    let p = ed.prompt.as_ref().unwrap();
    assert_eq!(p.text, "axb");
    assert_eq!(p.cursor, 2);
    assert!(ed.prompt.is_some());
}

#[test]
fn show_loc_expires() {
    let mut ed = test_ed("hi");
    press(&mut ed, KeyCode::Char('c'), Mods::CTRL);
    assert_eq!(ed.status_text(), Some("Line 1, Col 1".to_string()));
    ed.loc_until = Some(Instant::now() - Duration::from_secs(1));
    ed.tick_status();
    assert_eq!(ed.status_text(), None);
}

#[test]
fn goto_still_shows_loc() {
    let mut ed = test_ed("a\nb\nc");
    press(&mut ed, KeyCode::Char('7'), Mods::CTRL);
    press(&mut ed, KeyCode::Char('3'), Mods::NONE);
    press(&mut ed, KeyCode::Enter, Mods::NONE);
    assert_eq!(ed.bs().cursor, Pos { row: 2, col: 0 });
    let st = ed.status_text().unwrap();
    assert!(st.contains("Line 3"), "status: {st}");
}

#[test]
fn prompt_reopen_multibyte_query_cursor_ok() {
    // ^F re-opens the prompt only after a search that found nothing; the cursor
    // is a char index, so a byte length would start past EOL.
    let mut ed = test_ed("abc");
    press(&mut ed, KeyCode::Char('f'), Mods::CTRL);
    for c in "éé".chars() {
        press(&mut ed, KeyCode::Char(c), Mods::NONE);
    }
    press(&mut ed, KeyCode::Enter, Mods::NONE);
    assert_eq!(ed.bs().search.query, "éé");
    press(&mut ed, KeyCode::Char('f'), Mods::CTRL);
    let p = ed.prompt.as_ref().unwrap();
    assert_eq!(p.text, "éé");
    assert_eq!(p.cursor, 2);
}

#[test]
fn sort_lines_case_insensitive_and_region() {
    let mut ed = test_ed("b\nA\nc\na\nB");
    press(&mut ed, KeyCode::Char('a'), Mods::ALT);
    press(&mut ed, KeyCode::Down, Mods::NONE);
    press(&mut ed, KeyCode::Down, Mods::NONE);
    press(&mut ed, KeyCode::F(9), Mods::NONE);
    assert_eq!(lines(&ed), vec!["A", "b", "c", "a", "B"]);
    assert!(ed.bs().mark.is_none());
    let mut ed = test_ed("B\na\nA\nb");
    press(&mut ed, KeyCode::F(9), Mods::NONE);
    assert_eq!(lines(&ed), vec!["a", "A", "B", "b"]);
}

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

#[test]
fn a_save_keeps_the_files_mode() {
    use std::os::unix::fs::PermissionsExt;
    let d = temp_dir("save_mode");
    let f = d.0.join("run.sh");
    fs::write(&f, "#!/bin/sh\n").unwrap();
    fs::set_permissions(&f, fs::Permissions::from_mode(0o750)).unwrap();
    let mut ed = Editor::new(Buffer::from_file(&f).unwrap(), config::Config::default());
    press_text(&mut ed, "X");
    ed.save_to(f.clone());
    assert_eq!(fs::read(&f).unwrap(), b"X#!/bin/sh\n");
    let mode = fs::metadata(&f).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o750, "the mode survives the rename");
}

/// A directory nobody can write into, unless this runs as root (which writes anyway).
fn read_only_dir(d: &TempDir) -> bool {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&d.0, fs::Permissions::from_mode(0o555)).unwrap();
    let root = fs::write(d.0.join("probe"), b"x").is_ok();
    if root {
        let _ = fs::remove_file(d.0.join("probe"));
        fs::set_permissions(&d.0, fs::Permissions::from_mode(0o755)).unwrap();
    }
    !root
}

#[test]
fn a_writable_file_in_a_read_only_directory_still_saves() {
    use std::os::unix::fs::PermissionsExt;
    let d = temp_dir("save_ro_dir");
    let f = d.0.join("f.txt");
    fs::write(&f, "one\ntwo\n").unwrap();
    let mut ed = Editor::new(Buffer::from_file(&f).unwrap(), config::Config::default());
    press_text(&mut ed, "X");
    if !read_only_dir(&d) {
        return;
    }
    ed.save_to(f.clone());
    fs::set_permissions(&d.0, fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(fs::read(&f).unwrap(), b"Xone\ntwo\n", "written in place");
    assert!(!ed.bs().buf.modified, "saved");
}

#[test]
fn a_failed_save_leaves_the_file_and_no_temp_behind() {
    use std::os::unix::fs::PermissionsExt;
    let d = temp_dir("save_readonly");
    let f = d.0.join("f.txt");
    fs::write(&f, "one\ntwo\n").unwrap();
    let mut ed = Editor::new(Buffer::from_file(&f).unwrap(), config::Config::default());
    press_text(&mut ed, "X");
    fs::set_permissions(&f, fs::Permissions::from_mode(0o444)).unwrap();
    if !read_only_dir(&d) {
        return;
    }
    ed.save_to(f.clone());
    fs::set_permissions(&d.0, fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(
        fs::read(&f).unwrap(),
        b"one\ntwo\n",
        "the original is intact"
    );
    let left: Vec<_> = fs::read_dir(&d.0)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(left.len(), 1, "no temp file left behind: {left:?}");
    assert!(ed.bs().buf.modified, "still unsaved");
}

#[test]
fn a_save_through_a_symlink_keeps_the_link() {
    let d = temp_dir("save_symlink");
    let real = d.0.join("real.txt");
    fs::write(&real, "one\n").unwrap();
    let link = d.0.join("link.txt");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let mut ed = Editor::new(Buffer::from_file(&link).unwrap(), config::Config::default());
    press_text(&mut ed, "X");
    ed.save_to(link.clone());
    assert_eq!(
        fs::read(&real).unwrap(),
        b"Xone\n",
        "the bytes went through"
    );
    let meta = fs::symlink_metadata(&link).unwrap();
    assert!(meta.file_type().is_symlink(), "the link is still a link");
}

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
    press(&mut ed, KeyCode::Char('y'), Mods::NONE);
    assert_eq!(fs::read_to_string(&f).unwrap(), "Xone\ntwo\n");
    press_text(&mut ed, "Y");
    ed.save_to(f.clone());
    assert_eq!(prompt_kind(&ed), None);
    assert_eq!(fs::read_to_string(&f).unwrap(), "XYone\ntwo\n");
}

#[test]
fn no_to_the_external_change_keeps_the_file_and_disarms_quit() {
    let (_d, f, mut ed) = externally_changed("ext_no");
    press(&mut ed, KeyCode::Char('x'), Mods::CTRL);
    press(&mut ed, KeyCode::Char('y'), Mods::NONE);
    assert_eq!(prompt_kind(&ed), Some(PromptKind::ConfirmExternal));
    press(&mut ed, KeyCode::Char('n'), Mods::NONE);
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
    press(&mut ed, KeyCode::Char('d'), Mods::NONE);
    let text = |ed: &Editor| -> Vec<String> {
        ed.diff_view
            .as_ref()
            .expect("diff view")
            .lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_str()).collect())
            .collect()
    };
    let rows = text(&ed);
    assert!(rows[0].ends_with("f.txt"), "{rows:?}");
    assert!(rows[1].starts_with("@@"), "{rows:?}");
    assert!(rows.contains(&"1 -one".to_string()), "{rows:?}");
    assert!(rows.contains(&"1 +Xone".to_string()), "{rows:?}");
    assert!(
        rows.contains(&"3 -three from elsewhere".to_string()),
        "{rows:?}"
    );
    press(&mut ed, KeyCode::Char('s'), Mods::NONE);
    assert!(ed.diff_split, "the choice is remembered");
    let rows = text(&ed);
    let pair = rows.iter().find(|r| r.contains("Xone")).expect("the pair");
    let (left, right) = pair.split_once('│').expect("two panels");
    assert!(left.contains("- one"), "{pair:?}");
    assert!(right.contains("+ Xone"), "{pair:?}");
    ed.text_w = 60;
    assert!(ed.refresh_diff_view());
    assert!(!ed.refresh_diff_view());
    ed.text_h = 8;
    let buf = ui::Screen::of(&ed, 60, 12);
    let row = |y: u16| -> String { (0..60).map(|x| buf[(x, y)].symbol.clone()).collect() };
    assert!(row(1).contains("Saving would change"), "{}", row(1));
    assert!((2..9).any(|y| row(y).contains("Xone")));
    press(&mut ed, KeyCode::Esc, Mods::NONE);
    assert!(ed.diff_view.is_none());
    assert_eq!(prompt_kind(&ed), Some(PromptKind::ConfirmExternal));
    press(&mut ed, KeyCode::Char('d'), Mods::NONE);
    assert!(ed.diff_view.is_some());
    press(&mut ed, KeyCode::Char('y'), Mods::NONE);
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
    press(&mut ed, KeyCode::Char('p'), Mods::ALT);
    let rows = diff_view_text(&ed);
    assert!(rows.contains(&"src/a.rs".to_string()), "{rows:?}");
    assert!(rows.contains(&"8 -    old();".to_string()), "{rows:?}");
    assert!(rows.contains(&"8 +    new();".to_string()), "{rows:?}");
    assert!(ed.diff_view.as_ref().unwrap().header().contains("Patch"));
    press(&mut ed, KeyCode::Char('y'), Mods::NONE);
    assert!(ed.diff_view.is_some());
    press(&mut ed, KeyCode::Char('p'), Mods::ALT);
    assert!(ed.diff_view.is_none());
    assert!(ed.prompt.is_none());
    assert_eq!(lines(&ed)[0], "diff --git a/src/a.rs b/src/a.rs");
}

#[test]
fn m_p_previews_a_markdown_buffer_as_prose() {
    let mut ed = test_ed("# Title\n\nsome **bold** text and `code`\n\n- one\n- two");
    ed.bs_mut().buf.name = Some(PathBuf::from("/tmp/rano_preview.md"));
    ed.text_w = 100;
    ed.text_h = 20;
    press(&mut ed, KeyCode::Char('p'), Mods::ALT);
    let rows = diff_view_text(&ed);
    assert_eq!(rows[0], "# Title", "{rows:?}");
    assert!(
        rows.contains(&"some bold text and code".to_string()),
        "{rows:?}"
    );
    assert!(rows.contains(&"· one".to_string()), "{rows:?}");
    let header = ed.diff_view.as_ref().unwrap().header();
    assert!(header.contains("Markdown /tmp/rano_preview.md"), "{header}");
    press(&mut ed, KeyCode::Char('s'), Mods::NONE);
    assert!(ed.diff_view.is_some());
    assert!(
        ed.status.as_ref().unwrap().text.contains("one column"),
        "{:?}",
        ed.status.as_ref().map(|f| f.text.clone())
    );
    press(&mut ed, KeyCode::Char('p'), Mods::ALT);
    assert!(ed.diff_view.is_none());
    assert!(ed.prompt.is_none());
    assert_eq!(lines(&ed)[0], "# Title");
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
    press(&mut ed, KeyCode::Char('p'), Mods::ALT);
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
    press(&mut ed, KeyCode::Esc, Mods::NONE);
    assert!(ed.diff_view.is_none());
}

const CONFLICTED: &str = "fn main() {\n<<<<<<< HEAD\n    let total = 1;\n=======\n    let sum = 1;\n>>>>>>> feature\n    mid();\n<<<<<<< HEAD\n    one();\n||||||| base\n    zero();\n=======\n    two();\n>>>>>>> feature\n}";

fn conflict_ed() -> Editor {
    let mut ed = test_ed(CONFLICTED);
    ed.bs_mut().buf.name = Some(PathBuf::from("/tmp/rano_resolve.rs"));
    ed.text_w = 120;
    ed.text_h = 30;
    press(&mut ed, KeyCode::Char('p'), Mods::ALT);
    assert!(ed.diff_view.is_some());
    ed
}

#[test]
fn the_conflict_view_moves_between_conflicts_and_compares_with_the_base() {
    let mut ed = conflict_ed();
    ed.text_h = 6;
    assert!(
        ed.diff_view
            .as_ref()
            .unwrap()
            .header()
            .contains("Conflict 1/2")
    );
    press(&mut ed, KeyCode::Char('n'), Mods::NONE);
    let v = ed.diff_view.as_ref().unwrap();
    assert!(v.header().contains("Conflict 2/2"), "{}", v.header());
    let first: String = v.lines[v.top]
        .spans
        .iter()
        .map(|s| s.content.as_str())
        .collect();
    assert!(first.starts_with("▶ Conflict 2 of 2"), "{first:?}");
    press(&mut ed, KeyCode::Char('n'), Mods::NONE);
    assert!(
        ed.diff_view
            .as_ref()
            .unwrap()
            .header()
            .contains("Conflict 2/2")
    );
    press(&mut ed, KeyCode::Char('c'), Mods::NONE);
    let rows = diff_view_text(&ed);
    assert!(
        ed.diff_view
            .as_ref()
            .unwrap()
            .header()
            .contains("base/ours")
    );
    assert!(rows.iter().any(|r| r.contains("zero()")), "{rows:?}");
    press(&mut ed, KeyCode::Char('p'), Mods::NONE);
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
    press(&mut ed, KeyCode::Char('t'), Mods::NONE);
    assert_eq!(
        lines(&ed)[..3],
        ["fn main() {", "    let sum = 1;", "    mid();"],
        "{:?}",
        lines(&ed)
    );
    let v = ed.diff_view.as_ref().expect("still open");
    assert!(v.header().contains("Conflict 1/1"), "{}", v.header());
    assert!(ed.status_text().unwrap().contains("1 conflict left"));
    press(&mut ed, KeyCode::Char('b'), Mods::NONE);
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
    press(&mut ed, KeyCode::Char('u'), Mods::ALT);
    assert!(lines(&ed).contains(&"<<<<<<< HEAD".to_string()));
    assert!(lines(&ed).contains(&"    let sum = 1;".to_string()));
    press(&mut ed, KeyCode::Char('u'), Mods::ALT);
    assert_eq!(lines(&ed).join("\n"), CONFLICTED);
}

fn screen(ed: &Editor, w: u16, h: u16) -> Vec<String> {
    let s = ui::Screen::of(ed, w, h);
    (0..h).map(|y| s.row(y)).collect()
}

#[test]
fn a_prefix_waits_for_its_next_key_and_esc_abandons_it() {
    let mut ed = test_ed("- [ ] task");
    press(&mut ed, KeyCode::Char('t'), Mods::ALT);
    assert_eq!(ed.pending.keys.len(), 1, "M-t is a prefix");
    assert!(ed.pending_card().is_none(), "its card waits a moment");
    assert!(ed.card_due().is_some());
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
    assert!(ed.bar_items().iter().any(|(k, t)| k == "t" && t == "Tick"));
    press(&mut ed, KeyCode::Char('t'), Mods::NONE);
    assert!(ed.pending.keys.is_empty());
    assert_eq!(lines(&ed)[0], "- [x] task", "M-t t ticked it");
    press(&mut ed, KeyCode::Char('t'), Mods::ALT);
    press(&mut ed, KeyCode::Esc, Mods::NONE);
    assert_eq!(ed.status_text().as_deref(), Some("Quit"));
    press(&mut ed, KeyCode::Char('t'), Mods::ALT);
    press(&mut ed, KeyCode::Char('q'), Mods::NONE);
    assert_eq!(ed.status_text().as_deref(), Some("M-t q is undefined"));
    assert_eq!(lines(&ed)[0], "- [x] task", "nothing was typed");
}

#[test]
fn the_help_key_shows_its_card_at_once_and_opens_pages() {
    let mut ed = test_ed("text");
    ed.text_w = 100;
    ed.text_h = 30;
    press(&mut ed, KeyCode::Char('g'), Mods::CTRL);
    assert!(ed.pending_card().is_some(), "help shows at once");
    press(&mut ed, KeyCode::Char('b'), Mods::NONE);
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
    press(&mut ed, KeyCode::Char('z'), Mods::NONE);
    assert_eq!(lines(&ed), vec!["text"]);
    press(&mut ed, KeyCode::Char('q'), Mods::NONE);
    assert!(ed.info.is_none());
    press(&mut ed, KeyCode::Char('g'), Mods::CTRL);
    press(&mut ed, KeyCode::Char('g'), Mods::CTRL);
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
    press(&mut ed, KeyCode::Char('g'), Mods::CTRL);
    press(&mut ed, KeyCode::Char('k'), Mods::NONE);
    press(&mut ed, KeyCode::Char('k'), Mods::CTRL);
    let p = page(&ed);
    assert!(p.contains("C-k runs Cut") && p.contains("(cut)"), "{p}");
    assert_eq!(lines(&ed), vec!["text"], "described, not run");
    press(&mut ed, KeyCode::Esc, Mods::NONE);
    press(&mut ed, KeyCode::Char('g'), Mods::CTRL);
    press(&mut ed, KeyCode::Char('k'), Mods::NONE);
    press(&mut ed, KeyCode::Char('t'), Mods::ALT);
    press(&mut ed, KeyCode::Char('x'), Mods::NONE);
    assert!(page(&ed).contains("M-t x runs Decline"), "{}", page(&ed));
    press(&mut ed, KeyCode::Esc, Mods::NONE);
    press(&mut ed, KeyCode::Char('g'), Mods::CTRL);
    press(&mut ed, KeyCode::Char('k'), Mods::NONE);
    press(&mut ed, KeyCode::Char('z'), Mods::CTRL);
    assert!(page(&ed).contains("C-z is not bound"), "{}", page(&ed));
}

#[test]
fn m_x_runs_a_command_by_name_and_offers_recent_ones_first() {
    let mut ed = test_ed("text");
    press(&mut ed, KeyCode::Char('x'), Mods::ALT);
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
    press(&mut ed, KeyCode::Enter, Mods::NONE);
    assert!(ed.palette.is_none());
    assert_eq!(
        ed.prompt.as_ref().map(|p| p.kind),
        Some(PromptKind::WriteName)
    );
    ed.prompt = None;
    press(&mut ed, KeyCode::Char('x'), Mods::ALT);
    assert_eq!(ed.palette.as_ref().unwrap().items[0], "write-out");
    assert!(ed.palette.as_ref().unwrap().items.len() > 40);
    press(&mut ed, KeyCode::Esc, Mods::NONE);
    assert!(ed.palette.is_none());
    press(&mut ed, KeyCode::Char('x'), Mods::ALT);
    press_text(&mut ed, "qqqq");
    press(&mut ed, KeyCode::Enter, Mods::NONE);
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
    press(&mut ed, KeyCode::Char('z'), Mods::NONE);
    assert_eq!(lines(&ed).join("\n"), CONFLICTED);
    press(&mut ed, KeyCode::Esc, Mods::NONE);
    assert!(
        ed.bar_items()
            .iter()
            .any(|(k, t)| k == "C-x" && t == "Exit")
    );
}

const SUN_PNG_W: u32 = 200;
const SUN_PNG_H: u32 = 100;

/// A PNG-shaped file: signature, then a real IHDR. Nothing decodes it — the
/// terminal does.
fn write_png(path: &std::path::Path, w: u32, h: u32) {
    let mut v = b"\x89PNG\r\n\x1a\n".to_vec();
    v.extend_from_slice(&13u32.to_be_bytes());
    v.extend_from_slice(b"IHDR");
    v.extend_from_slice(&w.to_be_bytes());
    v.extend_from_slice(&h.to_be_bytes());
    v.extend_from_slice(&[8, 2, 0, 0, 0, 0, 0, 0, 0]);
    fs::write(path, v).unwrap();
}

fn picture_rows(ed: &Editor) -> Vec<String> {
    ed.diff_view
        .as_ref()
        .expect("a view")
        .lines
        .iter()
        .map(|l| l.plain())
        .collect()
}

fn graphics(ed: &mut Editor) -> Vec<String> {
    ed.take_graphics()
        .iter()
        .map(|b| String::from_utf8_lossy(b).into_owned())
        .collect()
}

/// M-P on a PNG draws it: placeholder rows plus the upload, before the frame.
#[test]
fn m_p_on_a_png_hands_the_terminal_a_picture_to_fill_the_cells_with() {
    let d = temp_dir("png");
    let file = d.0.join("sun.png");
    write_png(&file, SUN_PNG_W, SUN_PNG_H);
    let mut ed = test_ed("the buffer is the file's bytes, and they are not the picture");
    ed.bs_mut().buf.name = Some(file.clone());
    ed.text_w = 120;
    ed.text_h = 20;
    ed.images = true;
    press(&mut ed, KeyCode::Char('p'), Mods::ALT);
    let rows = picture_rows(&ed);
    let cells = crate::term::graphics::image_cells(
        Some(SUN_PNG_W),
        Some(SUN_PNG_H),
        crate::term::graphics::image_box(120),
    );
    assert_eq!(rows.len(), cells.1 as usize, "one row per row of the box");
    assert!(rows[0].starts_with('\u{10EEEE}'), "{:?}", rows[0]);
    assert_eq!(crate::width::text::width(&rows[0]), cells.0 as usize);
    let header = ed.diff_view.as_ref().unwrap().header();
    assert!(header.contains("Picture"), "{header}");
    assert!(header.contains("sun.png"), "{header}");
    assert_eq!(
        lines(&ed)[0],
        "the buffer is the file's bytes, and they are not the picture"
    );
    let out = graphics(&mut ed);
    assert_eq!(out.len(), 2, "{out:?}");
    assert!(out[0].starts_with("\x1b_Ga=t,f=100,i="), "{:?}", out[0]);
    assert!(out[1].contains("a=p,U=1"), "{:?}", out[1]);
    assert!(out[1].contains(&format!("c={},", cells.0)), "{:?}", out[1]);
    assert!(graphics(&mut ed).is_empty());
    ed.text_w = 200;
    assert!(ed.refresh_diff_view());
    let out = graphics(&mut ed);
    assert_eq!(out.len(), 2, "the picture again, at the new box: {out:?}");
    assert!(graphics(&mut ed).is_empty(), "and then quiet again");
    ed.text_w = 15;
    assert!(ed.refresh_diff_view());
    let rows = picture_rows(&ed);
    assert_eq!(crate::width::text::width(&rows[0]), 15, "{:?}", rows[0]);
    assert!(rows.len() <= crate::term::graphics::IMAGE_MAX_ROWS as usize);
    let out = graphics(&mut ed);
    assert!(out[1].contains("c=15,"), "placed 15 wide: {:?}", out[1]);
    // M-P on a picture is the way out of it, as Esc is.
    press(&mut ed, KeyCode::Char('p'), Mods::ALT);
    assert!(ed.diff_view.is_some(), "the only buffer is the picture");
    assert!(ed.status_text().unwrap().contains("Last buffer"));
    let other = d.0.join("notes.txt");
    fs::write(&other, "text\n").unwrap();
    ed.config.multibuffer = true;
    assert!(ed.open_at(&other, 1, None).is_ok());
    ed.set_current(0);
    assert!(graphics(&mut ed).len() >= 3, "back to the picture");
    press(&mut ed, KeyCode::Char('p'), Mods::ALT);
    assert!(ed.diff_view.is_none(), "the picture's buffer went with it");
    let out = graphics(&mut ed);
    assert_eq!(out.len(), 1, "and the bytes go with it: {out:?}");
    assert!(out[0].contains("a=d,d=I"), "{:?}", out[0]);
}

/// A markdown preview draws the pictures the document names, under the row that
/// names them.
#[test]
fn a_markdown_preview_in_a_wide_pane_keeps_a_readable_column_centred() {
    let long = "word ".repeat(60);
    let mut ed = test_ed(&format!("# Title\n\n{long}"));
    ed.bs_mut().buf.name = Some(PathBuf::from("/tmp/rano_wide.md"));
    ed.text_w = 240;
    ed.text_h = 20;
    press(&mut ed, KeyCode::Char('p'), Mods::ALT);
    let rows = diff_view_text(&ed);
    assert_eq!(rows[0], format!("{}# Title", " ".repeat(70)), "{rows:?}");
    let prose: Vec<&String> = rows.iter().filter(|r| r.contains("word")).collect();
    assert!(
        prose.len() >= 3,
        "wrapped to the column, not the pane: {rows:?}"
    );
    for r in prose {
        assert!(r.starts_with(&" ".repeat(70)), "{r:?}");
        assert!(r.trim_end().chars().count() <= 170, "{r:?}");
    }
}

#[test]
fn a_wide_code_block_gives_up_margin_to_stay_on_screen() {
    let code = format!("{}\n{}", "x".repeat(20), "y".repeat(200));
    let mut ed = test_ed(&format!("Prose.\n\n```\n{code}\n```\n\n```\nshort\n```\n"));
    ed.bs_mut().buf.name = Some(PathBuf::from("/tmp/rano_code.md"));
    ed.text_w = 240;
    ed.text_h = 30;
    press(&mut ed, KeyCode::Char('p'), Mods::ALT);
    let rows = diff_view_text(&ed);
    let margin = |needle: &str| {
        let r = rows.iter().find(|r| r.contains(needle)).unwrap();
        r.len() - r.trim_start().len()
    };
    assert_eq!(margin("Prose."), 70, "{rows:?}");
    assert_eq!(margin("yyyy"), 240 - 202, "fits the frame: {rows:?}");
    assert_eq!(margin("xxxx"), margin("yyyy"), "one block: {rows:?}");
    assert_eq!(
        margin("short"),
        70,
        "a narrow block keeps the column: {rows:?}"
    );
}

#[test]
fn a_markdown_preview_draws_the_pictures_the_document_names() {
    let d = temp_dir("md_img");
    write_png(&d.0.join("sun.png"), SUN_PNG_W, SUN_PNG_H);
    let text = "Intro, and sun.png named early.\n\n![the sun](sun.png)\n\nAfter.\n";
    let mut ed = test_ed(text);
    ed.bs_mut().buf.name = Some(d.0.join("notes.md"));
    ed.text_w = 120;
    ed.text_h = 24;
    ed.images = true;
    press(&mut ed, KeyCode::Char('p'), Mods::ALT);
    let rows = picture_rows(&ed);
    let reference = rows
        .iter()
        .position(|r| r.contains("the sun"))
        .unwrap_or_else(|| panic!("{rows:?}"));
    assert!(
        rows[reference + 1].starts_with(&format!("{}\u{10EEEE}", " ".repeat(10))),
        "centred with the prose: {rows:?}"
    );
    assert!(rows[0].contains("Intro"), "{rows:?}");
    let out = graphics(&mut ed);
    assert_eq!(out.len(), 2, "{out:?}");
    assert!(out[0].starts_with("\x1b_Ga=t,f=100,i="), "{:?}", out[0]);
    assert!(out[1].contains("a=p,U=1"), "{:?}", out[1]);
    let wide = crate::width::text::width(&rows[reference + 1]);
    // A narrower frame re-splices the picture narrower: the rows are the box.
    ed.text_w = 40;
    assert!(ed.refresh_diff_view());
    let rows = picture_rows(&ed);
    let narrow = crate::width::text::width(&rows[reference + 1]);
    assert!(
        narrow < wide,
        "{narrow} < {wide}: the box followed the frame"
    );
    assert!(rows.iter().any(|r| r.contains("After")), "{rows:?}");
    let out = graphics(&mut ed);
    assert_eq!(out[1].matches("a=p,U=1").count(), 1, "{:?}", out[1]);
    press(&mut ed, KeyCode::Char('p'), Mods::ALT);
    assert_eq!(graphics(&mut ed).len(), 1, "and dropped with the view");
}

/// A reference that is not there is not an error: the alt text is drawn.
#[test]
fn a_reference_to_a_missing_file_draws_nothing() {
    let d = temp_dir("md_missing");
    let mut ed = test_ed("Before.\n\n![gone](nowhere.png)\n");
    ed.bs_mut().buf.name = Some(d.0.join("notes.md"));
    ed.text_w = 100;
    ed.text_h = 20;
    ed.images = true;
    press(&mut ed, KeyCode::Char('p'), Mods::ALT);
    assert!(ed.diff_view.is_some(), "the preview still opens");
    let rows = picture_rows(&ed);
    assert!(rows.iter().any(|r| r.contains("gone")), "{rows:?}");
    assert!(!rows.iter().any(|r| r.starts_with('\u{10EEEE}')));
    assert!(graphics(&mut ed).is_empty(), "and nothing is queued for it");
    assert!(ed.status_text().is_none(), "and nothing is said about it");
}

#[test]
fn a_picture_says_why_when_the_terminal_takes_none_or_the_file_is_not_a_picture() {
    let d = temp_dir("png_why");
    let file = d.0.join("sun.png");
    write_png(&file, 20, 10);
    let mut ed = test_ed("x");
    ed.bs_mut().buf.name = Some(file.clone());
    ed.images = false;
    press(&mut ed, KeyCode::Char('p'), Mods::ALT);
    assert!(ed.diff_view.is_none());
    let msg = ed.status_text().unwrap();
    assert!(msg.contains("does not take inline pictures"), "{msg}");
    assert!(graphics(&mut ed).is_empty());
    // A file that is not a picture is refused by name; which refusal depends on
    // the machine.
    for nonsense in ["liar.png", "sun.jpg"] {
        let path = d.0.join(nonsense);
        fs::write(&path, b"not a picture at all, whatever the name says").unwrap();
        let mut ed = test_ed("x");
        ed.bs_mut().buf.name = Some(path);
        ed.images = true;
        press(&mut ed, KeyCode::Char('p'), Mods::ALT);
        assert!(ed.diff_view.is_none());
        let msg = ed.status_text().unwrap();
        assert!(msg.contains(nonsense), "{msg}");
        assert!(
            msg.contains("not a PNG")
                || msg.contains("could not read it")
                || msg.contains("no converter found"),
            "{msg}"
        );
        assert!(graphics(&mut ed).is_empty());
    }
}

/// A picture file opens as the picture, not as the text of its bytes. The buffer
/// underneath is the file as it is, so Esc lands in that text.
#[test]
fn opening_a_picture_shows_the_picture() {
    let d = temp_dir("open_png");
    let file = d.0.join("sun.png");
    write_png(&file, SUN_PNG_W, SUN_PNG_H);
    let mut ed = test_ed("");
    ed.text_w = 120;
    ed.text_h = 20;
    ed.images = true;
    assert!(ed.open_at(&file, 1, None).is_ok());
    let rows = picture_rows(&ed);
    assert!(rows[0].starts_with('\u{10EEEE}'), "{:?}", rows[0]);
    assert!(ed.diff_view.as_ref().unwrap().header().contains("Picture"));
    assert_eq!(graphics(&mut ed).len(), 2);
    // Escaping the picture leaves the file, not the view; with nothing behind it
    // the picture stays.
    press(&mut ed, KeyCode::Esc, Mods::NONE);
    assert!(ed.diff_view.is_some(), "the picture is the only thing here");
    assert!(ed.status_text().unwrap().contains("Last buffer"));
    assert!(!ed.bs().buf.modified, "and the file was never touched");
    press(&mut ed, KeyCode::F(8), Mods::NONE);
    assert_eq!(
        ed.prompt.as_ref().map(|p| p.kind),
        Some(PromptKind::OpenName)
    );
    press(&mut ed, KeyCode::Esc, Mods::NONE);
    assert!(ed.prompt.is_none());
    assert!(ed.diff_view.is_some(), "and the picture is still up");
    ed.config.multibuffer = true;
    let other = d.0.join("notes.txt");
    fs::write(&other, "plain text\n").unwrap();
    assert!(ed.open_at(&other, 1, None).is_ok());
    assert!(ed.diff_view.is_none(), "a text file opens as text");
    ed.set_current(0);
    assert!(
        ed.diff_view.is_some(),
        "back to the picture, as the picture"
    );
    let out = graphics(&mut ed);
    assert!(out.iter().any(|c| c.contains("a=d,d=I")), "{out:?}");
    assert!(out.iter().any(|c| c.contains("a=t,f=100")), "{out:?}");
    assert!(out.last().unwrap().contains("a=p,U=1"), "{out:?}");
    press(&mut ed, KeyCode::Esc, Mods::NONE);
    assert!(ed.diff_view.is_none(), "the picture's buffer went with it");
    assert_eq!(ed.buffers.len(), 1);
    assert_eq!(
        ed.bs().buf.name.as_deref(),
        Some(other.as_path()),
        "and the file beside it is current"
    );
    // A terminal that takes no pictures says nothing on the way in; the file is
    // read as text.
    let mut ed = test_ed("");
    ed.images = false;
    assert!(ed.open_at(&file, 1, None).is_ok());
    assert!(ed.diff_view.is_none());
    let msg = ed.status_text().unwrap_or_default();
    assert!(!msg.contains("pictures"), "{msg}");
    assert!(msg.starts_with("Read"), "the ordinary reading: {msg}");
}

/// A copy must reach the system clipboard, not only rano's own cutbuffer.
#[test]
fn a_copy_reaches_the_terminal_clipboard() {
    let mut ed = test_ed("one\ntwo\nthree\n");
    // M-6 over a marked region: exactly the characters.
    ed.bs_mut().mark = Some(Pos { row: 0, col: 1 });
    ed.bs_mut().cursor = Pos { row: 1, col: 1 };
    press(&mut ed, KeyCode::Char('6'), Mods::ALT);
    assert_eq!(ed.take_clipboard().as_deref(), Some("ne\nt"));
    assert!(ed.take_clipboard().is_none(), "and it is taken once");
    assert_eq!(ed.cut.len(), 2);
    // A selection that starts at a row's first column is line-wise: the newline
    // comes back.
    ed.bs_mut().mark = Some(Pos { row: 0, col: 0 });
    ed.bs_mut().cursor = Pos { row: 1, col: 2 };
    press(&mut ed, KeyCode::Char('6'), Mods::ALT);
    assert_eq!(ed.take_clipboard().as_deref(), Some("one\ntw\n"));
    // ^K cuts what is selected, and the clipboard takes it too.
    press(&mut ed, KeyCode::Char('k'), Mods::CTRL);
    assert_eq!(ed.take_clipboard().as_deref(), Some("one\ntw\n"));
    assert_eq!(lines(&ed), vec!["o", "three"]);
    let mut ed = test_ed("first\nsecond\n");
    press(&mut ed, KeyCode::Char('k'), Mods::CTRL);
    assert_eq!(ed.take_clipboard().as_deref(), Some("first\n"));
    assert_eq!(lines(&ed)[0], "second");
    let mut ed = test_ed("alpha beta\nsecond line\n");
    ed.text_w = 40;
    ed.text_h = 10;
    // No gutter, so a pane column is a character column: 0 to 5 is `alpha`.
    ed.show_line_numbers = false;
    assert!(ed.handle_mouse(me(MouseKind::Press(MouseButton::Left), 1, 0)));
    assert!(ed.handle_mouse(me(MouseKind::Drag(MouseButton::Left), 1, 5)));
    ed.handle_mouse(me(MouseKind::Release(MouseButton::Left), 1, 5));
    assert_eq!(ed.take_clipboard().as_deref(), Some("alpha"));
    ed.bs_mut().mark = None;
    assert!(ed.handle_mouse(me(MouseKind::Press(MouseButton::Left), 1, 2)));
    ed.handle_mouse(me(MouseKind::Release(MouseButton::Left), 1, 2));
    assert!(ed.take_clipboard().is_none(), "a click is not a copy");
}

/// A save into a directory that is not there is offered, not failed.
#[test]
fn saving_into_a_missing_directory_offers_to_create_it() {
    let d = temp_dir("mkdir");
    let deep = d.0.join("one/two/three");
    let file = deep.join("notes.txt");
    let mut ed = test_ed("hello\n");
    ed.bs_mut().buf.modified = true;
    ed.do_write(file.display().to_string());
    assert_eq!(
        ed.prompt.as_ref().map(|p| p.kind),
        Some(PromptKind::ConfirmMkdir),
        "the question is asked"
    );
    assert!(!deep.exists(), "and nothing is created before the answer");
    // n keeps everything as it was: no directory, no file, still modified.
    press(&mut ed, KeyCode::Char('n'), Mods::NONE);
    assert!(ed.prompt.is_none());
    assert!(!deep.exists());
    assert!(ed.bs().buf.modified, "the buffer is still unsaved");
    assert!(
        ed.status_text().unwrap().contains("Not saved"),
        "and says so"
    );
    // y makes every directory on the way and retries the write.
    let mut ed = test_ed("hello\n");
    ed.bs_mut().buf.modified = true;
    ed.do_write(file.display().to_string());
    press(&mut ed, KeyCode::Char('y'), Mods::NONE);
    assert!(ed.prompt.is_none());
    assert_eq!(fs::read_to_string(&file).unwrap(), "hello\n");
    assert!(!ed.bs().buf.modified, "saved");
    assert_eq!(ed.bs().buf.name.as_deref(), Some(file.as_path()));
    // Esc answers no like n, and the quit a save was waiting for goes with it.
    let mut ed = test_ed("x\n");
    ed.bs_mut().buf.modified = true;
    ed.quit_after_save = true;
    // A directory that is still missing: the `y` above created the first one.
    ed.do_write(d.0.join("four/five/other.txt").display().to_string());
    assert_eq!(
        ed.prompt.as_ref().map(|p| p.kind),
        Some(PromptKind::ConfirmMkdir)
    );
    press(&mut ed, KeyCode::Esc, Mods::NONE);
    assert!(ed.prompt.is_none());
    assert!(!ed.quit_after_save, "the quit is off");
    assert!(
        !ed.wants_quit(),
        "and a save that could not write does not exit"
    );
}

/// gofmt on save: the buffer is formatted before the write, as one undo step.
#[test]
fn saving_a_go_file_runs_gofmt_first() {
    if crate::lsp::find_bin("gofmt") == "gofmt" {
        eprintln!("skip: gofmt not found");
        return;
    }
    let d = temp_dir("gofmt");
    let file = d.0.join("main.go");
    let spaces = "package main\n\nfunc main() {\n    println(1)\n}\n";
    fs::write(&file, spaces).unwrap();
    let mut ed = test_ed(spaces);
    ed.bs_mut().buf.name = Some(file.clone());
    ed.text_w = 60;
    ed.text_h = 20;
    press(&mut ed, KeyCode::Char('o'), Mods::CTRL);
    press(&mut ed, KeyCode::Enter, Mods::NONE);
    let written = fs::read_to_string(&file).unwrap();
    assert!(
        written.contains("\tprintln(1)"),
        "gofmt wrote tabs: {written:?}"
    );
    assert!(!written.contains("    println"), "{written:?}");
    assert_eq!(lines(&ed).join("\n") + "\n", written);
    assert!(!ed.bs().buf.modified, "and the save landed");
    press(&mut ed, KeyCode::Char('u'), Mods::ALT);
    assert!(
        lines(&ed).join("\n").contains("    println(1)"),
        "M-U undoes it"
    );
    let mut ed = test_ed(&written);
    ed.bs_mut().buf.name = Some(file.clone());
    press(&mut ed, KeyCode::Char('o'), Mods::CTRL);
    press(&mut ed, KeyCode::Enter, Mods::NONE);
    assert_eq!(lines(&ed).join("\n") + "\n", written);
    let rs = d.0.join("main.rs");
    let rust = "fn main() {\n    let x = 1;\n}\n";
    fs::write(&rs, rust).unwrap();
    let mut ed = test_ed(rust);
    ed.bs_mut().buf.name = Some(rs.clone());
    press(&mut ed, KeyCode::Char('o'), Mods::CTRL);
    press(&mut ed, KeyCode::Enter, Mods::NONE);
    assert_eq!(fs::read_to_string(&rs).unwrap(), rust);
}

/// A folder opens as a listing (the dired), read-only; `RET` enters the cursor's
/// entry.
#[test]
fn a_folder_opens_as_a_listing_and_ret_enters_it() {
    let d = temp_dir("dired");
    let src = d.0.join("src");
    std::fs::create_dir_all(src.join("deep")).unwrap();
    fs::write(d.0.join("README.md"), "hello\n").unwrap();
    fs::write(src.join("main.rs"), "fn main() {}\n").unwrap();
    let mut ed = test_ed("");
    ed.text_h = 20;
    assert!(ed.open_at(&d.0, 1, None).is_ok(), "a folder is an open");
    assert_eq!(ed.bs().buf.name.as_deref(), Some(d.0.as_path()));
    assert_eq!(ed.bs().dir.as_deref(), Some(d.0.as_path()));
    assert!(ed.bs().read_only, "a listing is a view, not a document");
    assert_eq!(lines(&ed), vec!["src/", "README.md"]);
    assert_eq!(ed.bs().cursor, Pos { row: 0, col: 0 });
    press(&mut ed, KeyCode::Enter, Mods::NONE);
    assert_eq!(ed.bs().buf.name.as_deref(), Some(src.as_path()));
    assert_eq!(lines(&ed), vec!["deep/", "main.rs"]);
    press(&mut ed, KeyCode::Char('-'), Mods::NONE);
    assert_eq!(ed.bs().buf.name.as_deref(), Some(d.0.as_path()));
    ed.bs_mut().cursor = Pos { row: 1, col: 0 };
    press(&mut ed, KeyCode::Enter, Mods::NONE);
    assert!(ed.bs().dir.is_none(), "a file is not a listing");
    assert_eq!(lines(&ed), vec!["hello"]);
    assert_eq!(
        ed.bs().buf.name.as_deref(),
        Some(d.0.join("README.md").as_path())
    );
}

#[test]
fn a_listing_refreshes_keeps_its_place_and_refuses_edits() {
    let d = temp_dir("dired_r");
    fs::write(d.0.join("b.txt"), "x").unwrap();
    fs::write(d.0.join("d.txt"), "x").unwrap();
    let mut ed = test_ed("");
    ed.text_h = 20;
    assert!(ed.open_at(&d.0, 1, None).is_ok());
    ed.bs_mut().cursor = Pos { row: 1, col: 0 };
    assert_eq!(lines(&ed)[1], "d.txt");
    // A file appears while the listing is open: r sees it, and the cursor stays
    // on its entry.
    fs::write(d.0.join("c.txt"), "x").unwrap();
    press(&mut ed, KeyCode::Char('r'), Mods::NONE);
    assert_eq!(lines(&ed), vec!["b.txt", "c.txt", "d.txt"]);
    assert_eq!(
        lines(&ed)[ed.bs().cursor.row],
        "d.txt",
        "the cursor followed it"
    );
    // It is a view: an edit is refused with a line saying so. (A bare letter
    // types nothing — a mode's map is on top; `^K` is an edit.)
    press(&mut ed, KeyCode::Char('k'), Mods::CTRL);
    assert_eq!(lines(&ed), vec!["b.txt", "c.txt", "d.txt"]);
    assert!(!ed.bs().buf.modified);
    let msg = ed.status_text().unwrap_or_default();
    assert!(msg.contains("Read-only"), "{msg}");
}

#[test]
fn a_folder_that_is_not_there_says_so_and_changes_nothing() {
    let mut ed = test_ed("the buffer stays");
    ed.do_dir("/no/such/folder/at/all".to_string());
    assert!(ed.bs().dir.is_none());
    assert_eq!(lines(&ed), vec!["the buffer stays"]);
    assert!(ed.status_text().unwrap().contains("No such folder"));
    let d = temp_dir("dired_f8");
    fs::write(d.0.join("one.txt"), "x").unwrap();
    assert!(ed.open_file(&d.0.display().to_string()));
    assert_eq!(lines(&ed), vec!["one.txt"]);
}

/// A WAT plugin for the editor tests: `hook` answers a fixed text, without a wasm
/// toolchain.
fn wat_hook(hook: &str, out: &str) -> Vec<u8> {
    let mut body = String::new();
    for (i, b) in out.bytes().enumerate() {
        body.push_str(&format!(
            "(i32.store8 (i32.const {}) (i32.const {}))",
            1024 + i,
            b
        ));
    }
    let src = format!(
        r#"(module
            (memory (export "memory") 4)
            (func (export "rano_abi") (result i32) i32.const 1)
            (func (export "rano_alloc") (param i32) (result i32) i32.const 1024)
            (func (export "{hook}") (param i32 i32) (result i32)
              {body}
              (i32.store (i32.const 2048) (i32.const 1024))
              (i32.store (i32.const 2052) (i32.const {n}))
              i32.const 2048))"#,
        n = out.len()
    );
    wat::parse_str(&src).expect("valid wat")
}

/// The open hook: the hook's answer is the text, and opening is a reading, not an
/// edit.
#[test]
fn opening_a_file_runs_the_open_hook() {
    let d = temp_dir("plugin_open");
    let f = d.0.join("notes.txt");
    fs::write(&f, "original\n").unwrap();
    let mut ed = test_ed("");
    ed.set_plugin_bytes(
        &wat_hook("on_open", "hooked\n"),
        PathBuf::from("plugin.wasm"),
    );
    assert!(ed.open_at(&f, 1, None).is_ok());
    assert_eq!(lines(&ed), vec!["hooked"]);
    assert_eq!(
        fs::read_to_string(&f).unwrap(),
        "original\n",
        "the file is untouched"
    );
    assert!(!ed.bs().buf.modified, "reading is not an edit");
}

/// The save hook: what is written is the hook's answer, as one undo step.
#[test]
fn saving_runs_the_save_hook_and_the_buffer_is_what_was_written() {
    let d = temp_dir("plugin_save");
    let f = d.0.join("out.txt");
    fs::write(&f, "before\n").unwrap();
    let mut ed = test_ed("before\n");
    ed.bs_mut().buf.name = Some(f.clone());
    ed.set_plugin_bytes(
        &wat_hook("on_save", "after\n"),
        PathBuf::from("plugin.wasm"),
    );
    press(&mut ed, KeyCode::Char('o'), Mods::CTRL);
    press(&mut ed, KeyCode::Enter, Mods::NONE);
    assert_eq!(fs::read_to_string(&f).unwrap(), "after\n");
    assert_eq!(lines(&ed), vec!["after"]);
    assert!(!ed.bs().buf.modified);
    press(&mut ed, KeyCode::Char('u'), Mods::ALT);
    assert_eq!(lines(&ed), vec!["before"], "M-U undoes the plugin's step");
}

/// A hook that fails is skipped, never a failed save; the line names the plugin.
#[test]
fn a_trapping_plugin_leaves_the_save_intact() {
    let d = temp_dir("plugin_trap");
    let f = d.0.join("out.txt");
    fs::write(&f, "mine\n").unwrap();
    let mut ed = test_ed("mine\n");
    ed.bs_mut().buf.name = Some(f.clone());
    let trap = wat::parse_str(
        r#"(module
            (memory (export "memory") 1)
            (func (export "rano_abi") (result i32) i32.const 1)
            (func (export "rano_alloc") (param i32) (result i32) i32.const 1024)
            (func (export "on_save") (param i32 i32) (result i32) unreachable))"#,
    )
    .unwrap();
    ed.set_plugin_bytes(&trap, PathBuf::from("trap.wasm"));
    press(&mut ed, KeyCode::Char('o'), Mods::CTRL);
    press(&mut ed, KeyCode::Enter, Mods::NONE);
    assert_eq!(
        fs::read_to_string(&f).unwrap(),
        "mine\n",
        "the save went through"
    );
    assert!(!ed.bs().buf.modified);
    let msg = ed.status_text().unwrap_or_default();
    assert!(msg.contains("trap.wasm"), "{msg}");
    assert!(
        msg.contains("Wrote"),
        "the save's own line is still there: {msg}"
    );
}

/// A binary never reaches a plugin: it is read-only and its text is not the file.
#[test]
fn a_binary_file_never_reaches_the_plugin() {
    let d = temp_dir("plugin_binary");
    let f = d.0.join("blob.bin");
    let mut bytes = b"\x89PNG\r\n\x1a\n".to_vec();
    bytes.extend_from_slice(&[0u8; 64]);
    fs::write(&f, &bytes).unwrap();
    let mut ed = test_ed("");
    ed.set_plugin_bytes(
        &wat_hook("on_open", "hooked\n"),
        PathBuf::from("plugin.wasm"),
    );
    assert!(ed.open_at(&f, 1, None).is_ok());
    assert!(ed.bs().read_only, "a binary is a view");
    assert!(
        !lines(&ed).iter().any(|l| l == "hooked"),
        "the hook did not run: {:?}",
        lines(&ed).first()
    );
}

/// A folder opens through the plugin: `provider_open` is asked first, and its
/// answer becomes the listing.
#[test]
fn a_folder_opens_through_the_plugin() {
    let wasm: &[u8] = include_bytes!("../examples/plugin-dired/plugin.wasm");
    let d = temp_dir("plugin_dired");
    std::fs::create_dir_all(d.0.join("sub")).unwrap();
    fs::write(d.0.join("one.txt"), "one\n").unwrap();
    let mut ed = test_ed("");
    ed.text_h = 20;
    ed.set_plugin_bytes_with(
        wasm,
        PathBuf::from("dired.wasm"),
        crate::plugin::Capabilities {
            read_dir: true,
            ..Default::default()
        },
    );
    assert!(ed.open_at(&d.0, 1, None).is_ok());
    assert_eq!(lines(&ed), vec!["sub/", "one.txt"]);
    assert!(ed.bs().dir.is_some(), "a folder keeps the listing keys");
    assert!(ed.bs().read_only);
    press(&mut ed, KeyCode::Enter, Mods::NONE);
    assert_eq!(ed.bs().buf.name.as_deref(), Some(d.0.join("sub").as_path()));
    press(&mut ed, KeyCode::Char('-'), Mods::NONE);
    assert_eq!(ed.bs().buf.name.as_deref(), Some(d.0.as_path()));
    ed.bs_mut().cursor = Pos { row: 1, col: 0 };
    press(&mut ed, KeyCode::Enter, Mods::NONE);
    assert!(ed.bs().dir.is_none());
    assert_eq!(lines(&ed), vec!["one"]);
}

/// The same folder through the TypeScript plugin: the editor cannot tell the two
/// apart.
#[test]
fn a_folder_opens_through_the_typescript_plugin() {
    let wasm: &[u8] = include_bytes!("../examples/plugin-dired-as/plugin.wasm");
    let d = temp_dir("plugin_dired_as");
    fs::write(d.0.join("one.txt"), "one\n").unwrap();
    let mut ed = test_ed("");
    ed.text_h = 20;
    ed.set_plugin_bytes_with(
        wasm,
        PathBuf::from("dired-as.wasm"),
        crate::plugin::Capabilities {
            read_dir: true,
            ..Default::default()
        },
    );
    assert!(ed.open_at(&d.0, 1, None).is_ok());
    assert_eq!(lines(&ed), vec!["one.txt"]);
    assert!(ed.bs().dir.is_some());
    press(&mut ed, KeyCode::Enter, Mods::NONE);
    assert!(ed.bs().dir.is_none(), "a file opens as a file");
    assert_eq!(lines(&ed), vec!["one"]);
}

/// The manual is `docs/*.md`, or the build is stale: the text is rebuilt from the
/// files on disk, so a doc edited without rebuilding fails here.
#[test]
fn the_manual_is_the_docs() {
    let p = manual_plugin();
    let got = p
        .provider_open("rano:manual", 80, 24)
        .expect("the provider ran")
        .expect("the manual is mine");
    assert_eq!(got, docs_manual());
    assert!(got.starts_with("# Configuration\n"), "{:.40}", got);
}

/// `provider_open` says "not mine" for an ordinary path, so a file still opens as
/// a file.
#[test]
fn the_manual_is_not_mine_for_an_ordinary_path() {
    let p = manual_plugin();
    for path in [
        "/tmp/notes.txt",
        "src/main.rs",
        "docs/keys.md",
        "rano:manuals",
    ] {
        assert_eq!(
            p.provider_open(path, 80, 24).expect("the provider ran"),
            None,
            "{path} was claimed"
        );
    }
}

/// `M-x manual` opens the manual: the builtin unpacked and loaded from the
/// reader's folder.
#[test]
fn the_manual_opens_read_only_with_the_preview_on() {
    let d = temp_dir("manual_open");
    let mut ed = test_ed("");
    ed.text_w = 80;
    ed.text_h = 24;
    ed.open_manual_from(&d.0);
    assert_eq!(
        ed.bs().buf.name.as_deref(),
        Some(std::path::Path::new("rano:manual"))
    );
    assert!(ed.bs().read_only, "a view is read-only");
    assert!(ed.bs().synthetic, "a view wears no line numbers");
    assert_eq!(
        ed.bs().buf.text(),
        docs_manual(),
        "the buffer holds the docs"
    );
    match ed.diff_view.as_ref().map(|v| &v.source) {
        Some(crate::diffview::Source::Markdown { text, .. }) => {
            assert_eq!(text, &docs_manual(), "the preview is the manual")
        }
        _ => panic!("the manual opens rendered, not as raw markdown"),
    }
    // Unpacked, not overwritten: a second use leaves whatever is there.
    let at = d.0.join("built-in").join("manual.wasm");
    assert_eq!(
        fs::read(&at).unwrap(),
        include_bytes!("../examples/plugin-manual/plugin.wasm"),
        "the shipped build is unpacked byte for byte"
    );
    fs::write(&at, b"my own build").unwrap();
    // Editable means editable: a later editor loads the replacement and says so.
    let mut later = test_ed("");
    later.open_manual_from(&d.0);
    assert_eq!(fs::read(&at).unwrap(), b"my own build", "never overwritten");
    let said = later.status_text().unwrap_or_default();
    assert!(said.contains("not loaded"), "{said}");
}

/// The manual's plugin, loaded from the committed build the binary ships.
fn manual_plugin() -> crate::plugin::Plugin {
    crate::plugin::Plugin::from_bytes(
        include_bytes!("../examples/plugin-manual/plugin.wasm"),
        PathBuf::from("manual.wasm"),
    )
    .expect("the committed plugin loads")
}

/// `docs/*.md` as one document, in name order: the plugin's rule, built from the files on disk.
fn docs_manual() -> String {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("docs");
    let mut docs: Vec<PathBuf> = fs::read_dir(&dir)
        .expect("docs/")
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "md"))
        .collect();
    docs.sort();
    let mut out = String::new();
    for (i, p) in docs.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        out.push_str(fs::read_to_string(p).unwrap().trim_end_matches('\n'));
        out.push('\n');
    }
    out
}

/// A short file in a tall window draws: the viewport's last row is one past the
/// end, and the highlighter must not index it.
#[test]
fn a_short_file_in_a_tall_window_draws() {
    for wrap in [true, false] {
        for rows in 1..=5usize {
            for text_h in [1usize, 3, 40] {
                let text: String = (0..rows).map(|i| format!("line {i}\n")).collect();
                let mut ed = test_ed(&text);
                ed.wrap = wrap;
                ed.text_h = text_h;
                ed.text_w = 40;
                ed.ensure_wrap_prefix();
                ed.ensure_highlight();
                let rect = crate::render::Rect::new(0, 0, 40, text_h as u16 + 4);
                let mut buf = crate::render::Buffer::empty(rect);
                crate::ui::draw_in(&mut buf, rect, &ed);
            }
        }
    }
    // And scrolled past the end, which is the same index from the other direction.
    let mut ed = test_ed("one\ntwo\nthree");
    ed.wrap = false;
    ed.text_h = 10;
    ed.text_w = 40;
    ed.bs_mut().scroll = 9;
    ed.ensure_highlight();
    let rect = crate::render::Rect::new(0, 0, 40, 14);
    let mut buf = crate::render::Buffer::empty(rect);
    crate::ui::draw_in(&mut buf, rect, &ed);
}

/// A view wears no line numbers, starts no language server, and says rows, not
/// lines.
#[test]
fn a_view_wears_no_line_numbers_and_starts_no_server() {
    let d = temp_dir("view_gutter");
    fs::write(d.0.join("one.txt"), "one\n").unwrap();
    let mut ed = test_ed("");
    ed.show_line_numbers = true;
    ed.text_h = 20;
    assert!(ed.open_at(&d.0, 1, None).is_ok());
    assert!(ed.bs().synthetic);
    assert_eq!(ed.gutter_w(), 0, "a listing has no gutter");
    assert!(ed.bs().lsp.is_none(), "a view starts no server");
}

/// A send from a listing carries the entry under the cursor, not a line of the
/// listing.
#[test]
fn sending_from_a_listing_sends_the_entry_under_the_cursor() {
    use std::cell::RefCell;
    use std::rc::Rc;
    let d = temp_dir("view_send");
    fs::create_dir_all(d.0.join("src")).unwrap();
    fs::write(d.0.join("one.txt"), "one\n").unwrap();
    let mut ed = test_ed("");
    ed.text_h = 20;
    assert!(ed.open_at(&d.0, 1, None).is_ok());
    let got: Rc<RefCell<Vec<crate::send::SendEvent>>> = Rc::default();
    let sink = got.clone();
    ed.on_send = Some(Box::new(move |e| {
        sink.borrow_mut().push(e.clone());
        Ok("sent".into())
    }));
    press(&mut ed, KeyCode::Char('s'), Mods::ALT);
    {
        let g = got.borrow();
        assert_eq!(g.len(), 1);
        assert_eq!(g[0].path.as_deref(), Some(d.0.join("src").as_path()));
        assert_eq!(
            g[0].cursor.line, 1,
            "a view has no line of the target in it"
        );
        assert!(!g[0].modified);
    }
    ed.bs_mut().cursor = Pos { row: 1, col: 0 };
    press(&mut ed, KeyCode::Char('s'), Mods::ALT);
    {
        let g = got.borrow();
        assert_eq!(g.len(), 2);
        assert_eq!(
            g[1].path.as_deref(),
            Some(d.0.join("one.txt").as_path()),
            "the entry under the cursor"
        );
    }
}

/// The example plugin end to end through the editor: a committed wasm build, no
/// toolchain.
#[test]
fn the_example_plugin_transforms_through_the_editor() {
    let wasm: &[u8] = include_bytes!("../examples/plugin-upper/plugin.wasm");
    let d = temp_dir("example_plugin");
    let f = d.0.join("notes.txt");
    fs::write(&f, "hello rano\n").unwrap();
    let mut ed = test_ed("");
    ed.set_plugin_bytes(wasm, PathBuf::from("plugin-upper.wasm"));
    assert!(ed.open_at(&f, 1, None).is_ok());
    assert_eq!(lines(&ed), vec!["HELLO RANO"]);
    assert_eq!(fs::read_to_string(&f).unwrap(), "hello rano\n");
    let note = ed.status_text().unwrap_or_default();
    assert!(note.contains("transformed"), "the open says so: {note}");
    // Type lowercase, then save: the hook's answer becomes the buffer too, one
    // undo step.
    press(&mut ed, KeyCode::End, Mods::NONE);
    press_text(&mut ed, "abc");
    press(&mut ed, KeyCode::Char('o'), Mods::CTRL);
    press(&mut ed, KeyCode::Enter, Mods::NONE);
    assert_eq!(fs::read_to_string(&f).unwrap(), "HELLO RANOABC\n");
    assert_eq!(lines(&ed), vec!["HELLO RANOABC"]);
    press(&mut ed, KeyCode::Char('u'), Mods::ALT);
    assert_eq!(lines(&ed), vec!["HELLO RANOabc"]);
}

/// **A preview's text is selectable with the mouse**, and letting go puts *that* text — the
/// rendered text, not the buffer's — on the clipboard.
#[test]
fn a_mouse_drag_in_a_markdown_preview_copies_the_rendered_text() {
    let text = "# Title\n\nSome prose here.\n\n- a bullet\n";
    let mut ed = named_ed(text, "notes.md");
    ed.text_w = 60;
    ed.text_h = 20;
    press(&mut ed, KeyCode::Char('p'), Mods::ALT);
    assert!(ed.diff_view.is_some(), "a markdown preview");
    let rows = picture_rows(&ed);
    let line = rows
        .iter()
        .position(|r| r.contains("Some prose here."))
        .unwrap_or_else(|| panic!("{rows:?}"));
    // The view draws a header on the text area's first row, then one rendered line per row.
    let pane_row = (crate::editor::Chrome::default().top() as usize + 1 + line) as u16;
    assert!(ed.handle_mouse(me(MouseKind::Press(MouseButton::Left), pane_row, 0)));
    assert!(ed.handle_mouse(me(MouseKind::Drag(MouseButton::Left), pane_row, 4)));
    ed.handle_mouse(me(MouseKind::Release(MouseButton::Left), pane_row, 4));
    assert_eq!(
        ed.take_clipboard().as_deref(),
        Some("Some"),
        "the rendered characters the drag covered"
    );
    // A click with no drag clears it, and queues nothing.
    assert!(ed.handle_mouse(me(MouseKind::Press(MouseButton::Left), pane_row, 2)));
    ed.handle_mouse(me(MouseKind::Release(MouseButton::Left), pane_row, 2));
    assert!(ed.take_clipboard().is_none());
    assert_eq!(ed.diff_view.as_ref().unwrap().sel, None);
}

/// **The selection is drawn where it was made** — on the view's rendered line, in the theme's
/// selection look, on top of the syntax styles.
#[test]
fn a_preview_selection_is_painted() {
    let text = "# Title\n\nSome prose here.\n";
    let mut ed = named_ed(text, "notes.md");
    ed.text_w = 60;
    ed.text_h = 20;
    press(&mut ed, KeyCode::Char('p'), Mods::ALT);
    let rows = picture_rows(&ed);
    let line = rows
        .iter()
        .position(|r| r.contains("prose"))
        .expect("the prose line");
    let v = ed.diff_view.as_mut().unwrap();
    v.sel = Some((
        crate::diffview::ViewPos { line, col: 5 },
        crate::diffview::ViewPos { line, col: 10 },
    ));
    let rect = crate::render::Rect::new(0, 0, 60, 26);
    let mut buf = crate::render::Buffer::empty(rect);
    crate::ui::draw_in(&mut buf, rect, &ed);
    let pane_row = (crate::editor::Chrome::default().top() as usize + 1 + line) as u16;
    let sel = crate::render::Style::of(crate::style::Role::Selection);
    let inside = buf.cell(7, pane_row).expect("a cell").style.clone();
    assert_eq!(inside, sel, "the selected cell wears the selection style");
    let outside = buf.cell(1, pane_row).expect("a cell").style.clone();
    assert_ne!(outside, sel, "the rest of the line does not");
}

/// **M-6 in a preview copies the selection** and says so; with nothing selected it says that
/// instead of copying a line nobody can see.
#[test]
fn m_six_copies_a_preview_selection() {
    let text = "# Title\n\nSome prose here.\n";
    let mut ed = named_ed(text, "notes.md");
    ed.text_w = 60;
    ed.text_h = 20;
    press(&mut ed, KeyCode::Char('p'), Mods::ALT);
    press(&mut ed, KeyCode::Char('6'), Mods::ALT);
    assert!(
        ed.status_text().unwrap().contains("Nothing selected"),
        "{:?}",
        ed.status_text()
    );
    let rows = picture_rows(&ed);
    let line = rows
        .iter()
        .position(|r| r.contains("prose"))
        .expect("the prose line");
    let v = ed.diff_view.as_mut().unwrap();
    v.sel = Some((
        crate::diffview::ViewPos { line, col: 0 },
        crate::diffview::ViewPos { line, col: 4 },
    ));
    press(&mut ed, KeyCode::Char('6'), Mods::ALT);
    assert_eq!(ed.take_clipboard().as_deref(), Some("Some"));
}

/// **A picture has no text to select**: a drag over it does nothing rather than selecting
/// placeholder cells.
#[test]
fn a_picture_view_takes_no_selection() {
    let d = temp_dir("view_picture_sel");
    let file = d.0.join("sun.png");
    write_png(&file, SUN_PNG_W, SUN_PNG_H);
    let mut ed = test_ed("");
    ed.text_w = 60;
    ed.text_h = 20;
    ed.images = true;
    assert!(ed.open_at(&file, 1, None).is_ok());
    assert!(ed.diff_view.is_some());
    assert!(!ed.handle_mouse(me(MouseKind::Press(MouseButton::Left), 2, 1)));
    assert_eq!(ed.diff_view.as_ref().unwrap().sel, None);
}

#[test]
fn m_p_on_plain_text_says_there_is_nothing_to_render() {
    let mut ed = test_ed("just text");
    press(&mut ed, KeyCode::Char('p'), Mods::ALT);
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
    press(&mut ed, KeyCode::Char('d'), Mods::NONE);
    assert!(ed.diff_view.is_none());
    assert_eq!(prompt_kind(&ed), Some(PromptKind::ConfirmExternal));
    assert!(
        ed.status_text()
            .unwrap()
            .contains("only the file's timestamp")
    );
}

fn press_text(ed: &mut Editor, s: &str) {
    for c in s.chars() {
        press(ed, KeyCode::Char(c), Mods::NONE);
    }
}

fn replace_via_prompt(ed: &mut Editor, find: &str, with: &str, answer: char) {
    press(ed, KeyCode::Char('4'), Mods::CTRL);
    press_text(ed, find);
    press(ed, KeyCode::Enter, Mods::NONE);
    press_text(ed, with);
    press(ed, KeyCode::Enter, Mods::NONE);
    press(ed, KeyCode::Char(answer), Mods::NONE);
}

#[test]
fn undo_types_coalesce() {
    let mut ed = test_ed("");
    press_text(&mut ed, "abc");
    assert_eq!(lines(&ed), vec!["abc"]);
    press(&mut ed, KeyCode::Char('u'), Mods::ALT);
    assert_eq!(lines(&ed), vec![""]);
}

#[test]
fn undo_backspace_run() {
    let mut ed = test_ed("ab");
    press(&mut ed, KeyCode::End, Mods::NONE);
    press(&mut ed, KeyCode::Backspace, Mods::NONE);
    press(&mut ed, KeyCode::Backspace, Mods::NONE);
    assert_eq!(lines(&ed), vec![""]);
    press(&mut ed, KeyCode::Char('u'), Mods::ALT);
    assert_eq!(lines(&ed), vec!["ab"]);
}

#[test]
fn undo_redo_roundtrip() {
    let mut ed = test_ed("ab");
    press(&mut ed, KeyCode::End, Mods::NONE);
    press(&mut ed, KeyCode::Char('c'), Mods::NONE);
    press(&mut ed, KeyCode::Char('u'), Mods::ALT);
    assert_eq!(lines(&ed), vec!["ab"]);
    assert_eq!(ed.bs().cursor, Pos { row: 0, col: 2 });
    press(&mut ed, KeyCode::Char('e'), Mods::ALT);
    assert_eq!(lines(&ed), vec!["abc"]);
    assert_eq!(ed.bs().cursor, Pos { row: 0, col: 3 });
}

#[test]
fn undo_repeated_cut_coalesce() {
    let mut ed = test_ed("1\n2\n3\n4");
    for _ in 0..3 {
        press(&mut ed, KeyCode::Char('k'), Mods::CTRL);
    }
    assert_eq!(lines(&ed), vec!["4"]);
    press(&mut ed, KeyCode::Char('u'), Mods::ALT);
    assert_eq!(lines(&ed), vec!["1", "2", "3", "4"]);
}

#[test]
fn undo_partial_then_full_cut() {
    let mut ed = test_ed("hello");
    press(&mut ed, KeyCode::Right, Mods::NONE);
    press(&mut ed, KeyCode::Right, Mods::NONE);
    press(&mut ed, KeyCode::Char('k'), Mods::CTRL);
    assert_eq!(lines(&ed), vec!["he"]);
    press(&mut ed, KeyCode::Char('k'), Mods::CTRL);
    assert_eq!(lines(&ed), vec![""]);
    press(&mut ed, KeyCode::Char('u'), Mods::ALT);
    assert_eq!(lines(&ed), vec!["hello"]);
}

#[test]
fn undo_paste_roundtrip() {
    let mut ed = test_ed("ab\ncd");
    press(&mut ed, KeyCode::Char('k'), Mods::CTRL);
    press(&mut ed, KeyCode::Char('u'), Mods::CTRL);
    assert_eq!(lines(&ed), vec!["abcd"]);
    press(&mut ed, KeyCode::Char('u'), Mods::ALT);
    assert_eq!(lines(&ed), vec!["cd"]);
    assert_eq!(ed.bs().cursor, Pos { row: 0, col: 0 });
    press(&mut ed, KeyCode::Char('e'), Mods::ALT);
    assert_eq!(lines(&ed), vec!["abcd"]);
    assert_eq!(ed.bs().cursor, Pos { row: 0, col: 2 });
}

#[test]
fn undo_replace_all_one_step() {
    let mut ed = test_ed("aa\naa");
    replace_via_prompt(&mut ed, "aa", "x", 'a');
    assert_eq!(lines(&ed), vec!["x", "x"]);
    press(&mut ed, KeyCode::Char('u'), Mods::ALT);
    assert_eq!(lines(&ed), vec!["aa", "aa"]);
}

#[test]
fn undo_limit_trims() {
    let mut ed = test_ed("a");
    for _ in 0..600 {
        press(&mut ed, KeyCode::Enter, Mods::NONE);
    }
    // A run of Enters coalesces into one step; the "500 steps max" contract
    // holds either way.
    assert!(ed.bs().undo.len() <= 500);
}

#[test]
fn undo_selection_overwrite() {
    let mut ed = test_ed("abcd");
    press(&mut ed, KeyCode::Right, Mods::NONE);
    press(&mut ed, KeyCode::Char('a'), Mods::ALT);
    press(&mut ed, KeyCode::Right, Mods::NONE);
    press(&mut ed, KeyCode::Right, Mods::NONE);
    press(&mut ed, KeyCode::Char('X'), Mods::NONE);
    assert_eq!(lines(&ed), vec!["aXd"]);
    press(&mut ed, KeyCode::Char('u'), Mods::ALT);
    assert_eq!(lines(&ed), vec!["abcd"]);
    assert!(ed.bs().mark.is_none());
}

#[test]
fn undo_delete_selection() {
    let mut ed = test_ed("abcd");
    press(&mut ed, KeyCode::Right, Mods::NONE);
    press(&mut ed, KeyCode::Char('a'), Mods::ALT);
    press(&mut ed, KeyCode::Right, Mods::NONE);
    press(&mut ed, KeyCode::Right, Mods::NONE);
    press(&mut ed, KeyCode::Backspace, Mods::NONE);
    assert_eq!(lines(&ed), vec!["ad"]);
    press(&mut ed, KeyCode::Char('u'), Mods::ALT);
    assert_eq!(lines(&ed), vec!["abcd"]);
}

#[test]
fn undo_newline_join() {
    let mut ed = test_ed("ab");
    press(&mut ed, KeyCode::End, Mods::NONE);
    press(&mut ed, KeyCode::Enter, Mods::NONE);
    press(&mut ed, KeyCode::Backspace, Mods::NONE);
    assert_eq!(lines(&ed), vec!["ab"]);
    press(&mut ed, KeyCode::Char('u'), Mods::ALT);
    assert_eq!(lines(&ed), vec!["ab", ""]);
    press(&mut ed, KeyCode::Char('u'), Mods::ALT);
    assert_eq!(lines(&ed), vec!["ab"]);
}

#[test]
fn undo_read_empty_replace() {
    let d = temp_dir("read_undo");
    let src = d.0.join("in.txt");
    fs::write(&src, "x\ny\n").unwrap();
    let mut ed = test_ed("");
    press(&mut ed, KeyCode::Char('r'), Mods::CTRL);
    press_text(&mut ed, &src.display().to_string());
    press(&mut ed, KeyCode::Enter, Mods::NONE);
    assert_eq!(lines(&ed), vec!["x", "y"]);
    press(&mut ed, KeyCode::Char('u'), Mods::ALT);
    assert_eq!(lines(&ed), vec![""]);
}

#[test]
fn undo_redo_after_new_edit() {
    let mut ed = test_ed("ab");
    press(&mut ed, KeyCode::End, Mods::NONE);
    press(&mut ed, KeyCode::Char('c'), Mods::NONE);
    press(&mut ed, KeyCode::Char('u'), Mods::ALT);
    press(&mut ed, KeyCode::Char('x'), Mods::NONE);
    assert_eq!(lines(&ed), vec!["abx"]);
    press(&mut ed, KeyCode::Char('e'), Mods::ALT);
    assert_eq!(lines(&ed), vec!["abx"]);
}

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

// didChange is debounced: the flag clears only after the 300 ms window.

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
    // Exact prefix matches come first in the server's order; subsequence matches
    // follow.
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
    // "into_raw_parts" is a subsequence match (i…s), after the exact ones;
    // "as_bytes" has no `i` and is dropped.
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
    // A `.`-context answered with path fallback (rust-analyzer still scanning) is
    // not applied; the same items in a word context are legitimate.
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
    // Timer elapsed: the poll consumes it and keeps the popup (no LSP here, so a
    // no-op).
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
    // Response for an older keystroke: dropped, popup untouched.
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
    press(&mut ed, KeyCode::Down, Mods::NONE);
    assert_eq!(ed.completion.as_ref().unwrap().sel, 1);
    assert_eq!(ed.bs().cursor.col, 3, "cursor must not move");
    press(&mut ed, KeyCode::Up, Mods::NONE);
    assert_eq!(ed.completion.as_ref().unwrap().sel, 0);
    press(&mut ed, KeyCode::Char('n'), Mods::CTRL);
    assert_eq!(ed.completion.as_ref().unwrap().sel, 1);
    press(&mut ed, KeyCode::Esc, Mods::NONE);
    assert!(ed.completion.is_none());
}

#[test]
fn completion_enter_accepts_not_newline() {
    let mut ed = test_ed("pri\n");
    ed.bs_mut().cursor = Pos { row: 0, col: 3 };
    ed.completion = Some(popup(vec![citem("print!", 3)], 0, 0));
    press(&mut ed, KeyCode::Enter, Mods::NONE);
    assert_eq!(lines(&ed), vec!["print!"]);
    assert_eq!(ed.bs().cursor.col, 6);
}

#[test]
fn completion_typing_backspace_and_space_lifecycle() {
    // No LSP: the popup cannot reopen, but typing inside the word keeps it alive.
    let mut ed = test_ed("pr x\n");
    ed.bs_mut().cursor = Pos { row: 0, col: 2 };
    ed.completion = Some(popup(vec![citem("pr", 6)], 0, 0));
    press_text(&mut ed, "i");
    assert!(ed.completion.is_some(), "identifier char keeps popup");
    press(&mut ed, KeyCode::Backspace, Mods::NONE);
    assert!(ed.completion.is_some(), "backspace inside word keeps popup");
    press(&mut ed, KeyCode::Char(' '), Mods::NONE);
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
    press(&mut ed, KeyCode::Char('.'), Mods::ALT);
    assert_eq!(ed.status_text(), Some("No LSP server".to_string()));
    press(&mut ed, KeyCode::Char(','), Mods::ALT);
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
    press(&mut ed, KeyCode::Char(','), Mods::ALT);
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
    press(&mut ed, KeyCode::Char(','), Mods::ALT);
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
    assert_eq!(s, Style::of(Role::DiagError).underline());
    let s = ed.char_style_with(Pos { row: 0, col: 3 }, &ed.all_diags());
    assert!(!s.roles().any(|r| r == Role::DiagError));
    assert!(!s.attrs.contains(crate::style::Attrs::UNDERLINE));
    ed.bs_mut().lsp_diags = vec![diag(0, 0, 2, 2)];
    assert_eq!(
        ed.char_style_with(Pos { row: 0, col: 1 }, &ed.all_diags())
            .top(),
        Role::DiagWarning
    );
    ed.bs_mut().lsp_diags = vec![diag(0, 0, 2, 3)];
    assert_eq!(
        ed.char_style_with(Pos { row: 0, col: 1 }, &ed.all_diags())
            .top(),
        Role::DiagNote
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
        Style::of(Role::Selection)
    );
    ed.bs_mut().mark = None;
    ed.bs_mut().search.query = "fn".to_string();
    ed.bs_mut().search_matches = Some(vec![(Pos { row: 0, col: 0 }, 2)]);
    ed.bs_mut().search.current = 0;
    assert_eq!(
        ed.char_style_with(Pos { row: 0, col: 1 }, &ed.all_diags()),
        Style::of(Role::Match)
    );
}

// Indentation: Tab follows the buffer's own indent style.

#[test]
fn tab_matches_space_indent() {
    let mut ed = test_ed("    a\n\n");
    ed.bs_mut().cursor = Pos { row: 1, col: 0 };
    press(&mut ed, KeyCode::Tab, Mods::NONE);
    assert_eq!(lines(&ed), vec!["    a", "    "]);
    assert_eq!(ed.bs().cursor.col, 4);
}

#[test]
fn tab_aligns_to_next_unit_mid_line() {
    let mut ed = test_ed("    a\n  x");
    ed.bs_mut().cursor = Pos { row: 1, col: 2 };
    press(&mut ed, KeyCode::Tab, Mods::NONE);
    assert_eq!(lines(&ed), vec!["    a", "    x"]);
}

#[test]
fn tab_uses_tab_char_when_file_does() {
    let mut ed = test_ed("\ta\n\tb\n\n");
    ed.config.indent = config::Indent::Auto;
    ed.bs_mut().cursor = Pos { row: 2, col: 0 };
    press(&mut ed, KeyCode::Tab, Mods::NONE);
    assert_eq!(lines(&ed), vec!["\ta", "\tb", "\t"]);
}

#[test]
fn tab_inserts_the_configured_unit_by_default() {
    let mut ed = test_ed("\ta\n\tb\n\n");
    ed.bs_mut().cursor = Pos { row: 2, col: 0 };
    press(&mut ed, KeyCode::Tab, Mods::NONE);
    assert_eq!(lines(&ed), vec!["\ta", "\tb", "    "]);
    // `indent = tabs` is the other way to ask for one, and it is one character.
    let mut ed = test_ed("");
    ed.config.indent = config::Indent::Tabs;
    press(&mut ed, KeyCode::Tab, Mods::NONE);
    press(&mut ed, KeyCode::Char('x'), Mods::NONE);
    assert_eq!(lines(&ed), vec!["\tx"]);
}

#[test]
fn backspace_deletes_indent_run() {
    let mut ed = test_ed("    a\n    x");
    ed.bs_mut().cursor = Pos { row: 1, col: 4 };
    press(&mut ed, KeyCode::Backspace, Mods::NONE);
    assert_eq!(ed.bs().cursor.col, 0);
    assert_eq!(lines(&ed), vec!["    a", "x"]);
}

#[test]
fn backspace_mid_text_still_one_char() {
    let mut ed = test_ed("    ab");
    ed.bs_mut().cursor = Pos { row: 0, col: 6 };
    press(&mut ed, KeyCode::Backspace, Mods::NONE);
    assert_eq!(lines(&ed), vec!["    a"]);
}

// Syntax errors surface without a language server, and zero-width ranges become
// visible.

#[test]
fn syntax_error_diag_without_lsp() {
    let mut buf = Buffer::new();
    buf.name = Some(std::path::PathBuf::from("t.rs"));
    buf.set_rows(vec!["fn main() {".chars().collect()]);
    let mut ed = Editor::new(buf, config::Config::default());
    ed.edit_invalidate();
    // Diagnostics are debounced: do what the run loop does — highlight, then flush.
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
    press(&mut ed, KeyCode::Char('d'), Mods::ALT);
    let st = ed.status_text().unwrap();
    assert!(st.contains("No diagnostics"), "status: {st}");
    assert_eq!(ed.bs().cursor, Pos { row: 1, col: 0 });
}

// Exec is async: output lands below the spawn row as one undo step; a failure
// records none.

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
    press(&mut ed, KeyCode::Char('u'), Mods::ALT);
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

// Bracketed paste: one undo step, \r stripped, cursor at the end.

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
    press(&mut ed, KeyCode::Char('u'), Mods::ALT);
    assert_eq!(lines(&ed), vec![""]);
}

#[test]
fn paste_text_strips_cr() {
    let mut ed = test_ed("");
    ed.paste_text("x\r\ny");
    assert_eq!(lines(&ed), vec!["x", "y"]);
}

// M-| filters the selected rows through an external command.

#[test]
fn filter_region_uppercases() {
    let mut ed = test_ed("hello\nworld");
    ed.bs_mut().cursor = Pos { row: 0, col: 5 };
    ed.bs_mut().mark = Some(Pos { row: 0, col: 0 });
    press(&mut ed, KeyCode::Char('|'), Mods::ALT);
    assert!(ed.prompt.is_some());
    press_text(&mut ed, "tr a-z A-Z");
    press(&mut ed, KeyCode::Enter, Mods::NONE);
    assert_eq!(lines(&ed), vec!["HELLO", "world"]);
    assert_eq!(ed.bs().cursor, Pos { row: 0, col: 0 });
    assert!(ed.bs().mark.is_none());
    press(&mut ed, KeyCode::Char('u'), Mods::ALT);
    assert_eq!(lines(&ed), vec!["hello", "world"]);
}

#[test]
fn filter_no_selection_flashes() {
    let mut ed = test_ed("hello");
    press(&mut ed, KeyCode::Char('|'), Mods::ALT);
    let st = ed.status_text().unwrap();
    assert!(st.contains("No selection"), "status: {st}");
    assert!(ed.prompt.is_none());
}

#[test]
fn filter_failure_no_undo() {
    let mut ed = test_ed("hello\nworld");
    ed.bs_mut().cursor = Pos { row: 0, col: 5 };
    ed.bs_mut().mark = Some(Pos { row: 0, col: 0 });
    press(&mut ed, KeyCode::Char('|'), Mods::ALT);
    press_text(&mut ed, "false");
    press(&mut ed, KeyCode::Enter, Mods::NONE);
    let st = ed.status_text().unwrap();
    assert!(st.contains("Exit code"), "status: {st}");
    assert_eq!(lines(&ed), vec!["hello", "world"]);
    assert_eq!(ed.bs().mark, Some(Pos { row: 0, col: 0 }), "mark unchanged");
    assert!(ed.bs().undo.is_empty(), "failure must record no undo step");
}

#[test]
fn alt_d_jumps_diag_alt_arrows_words() {
    let mut ed = test_ed("ab cd ef");
    ed.bs_mut().lsp_diags = vec![diag(0, 3, 5, 1)];
    press(&mut ed, KeyCode::Char('d'), Mods::ALT);
    assert_eq!(ed.bs().cursor, Pos { row: 0, col: 3 });
    let mut ed = test_ed("ab cd");
    press(&mut ed, KeyCode::Right, Mods::ALT);
    assert_eq!(ed.bs().cursor, Pos { row: 0, col: 2 });
    press(&mut ed, KeyCode::Left, Mods::ALT);
    assert_eq!(ed.bs().cursor, Pos { row: 0, col: 0 });
}

// Horizontal scroll follows the cursor in display cols.

#[test]
fn scroll_x_follows_cursor() {
    let mut ed = test_ed(&"a".repeat(40));
    ed.wrap = false; // horizontal scrolling needs wrap off
    ed.show_line_numbers = false;
    ed.text_w = 10;
    press(&mut ed, KeyCode::End, Mods::NONE);
    ed.adjust_scroll_x();
    assert_eq!(ed.bs().scroll_x, 31);
    press(&mut ed, KeyCode::Home, Mods::NONE);
    ed.adjust_scroll_x();
    assert_eq!(ed.bs().scroll_x, 0);
}

#[test]
fn scroll_x_with_tabs() {
    let mut ed = test_ed("a\tb");
    ed.tab_width = 8;
    ed.wrap = false; // horizontal scrolling needs wrap off
    ed.show_line_numbers = false;
    ed.text_w = 4;
    press(&mut ed, KeyCode::End, Mods::NONE);
    ed.adjust_scroll_x();
    assert_eq!(ed.bs().scroll_x, 6);
}

// M-N toggles the line-number gutter.

#[test]
fn m_n_toggles_line_numbers() {
    let mut ed = test_ed("hi");
    assert!(ed.show_line_numbers, "line numbers default to on");
    press(&mut ed, KeyCode::Char('n'), Mods::ALT);
    assert!(!ed.show_line_numbers);
    press(&mut ed, KeyCode::Char('n'), Mods::ALT);
    assert!(ed.show_line_numbers);
}

// M-\ — soft line wrap: scroll, motion and mouse work in visual rows.

#[test]
fn m_backslash_toggles_wrap() {
    let mut ed = test_ed("hi");
    assert!(ed.wrap, "soft wrap defaults to on (nano)");
    press(&mut ed, KeyCode::Char('\\'), Mods::ALT);
    assert!(!ed.wrap);
    press(&mut ed, KeyCode::Char('\\'), Mods::ALT);
    assert!(ed.wrap);
}

#[test]
fn wrap_table_and_motion_handle_wide_characters() {
    // 5 CJK characters = 10 display columns in a 4-column view: three visual rows
    // of 2, 2 and 1. Nothing may split a glyph.
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
    assert_eq!(ed.seg_chars(0, 0), (0, 2));
    assert_eq!(ed.seg_chars(0, 1), (2, 4));
    assert_eq!(ed.seg_chars(0, 2), (4, 5));
    assert_eq!(ed.seg_disp(0, 0), (0, 4));
    assert_eq!(ed.seg_disp(0, 1), (4, 8));
    assert_eq!(ed.seg_disp(0, 2), (8, 10));
    assert_eq!(ed.visual_pos(Pos { row: 0, col: 0 }), 0);
    assert_eq!(ed.visual_pos(Pos { row: 0, col: 1 }), 0);
    assert_eq!(ed.visual_pos(Pos { row: 0, col: 2 }), 1);
    assert_eq!(ed.visual_pos(Pos { row: 0, col: 4 }), 2);
    assert_eq!(ed.disp_in_seg(0, 0), 0);
    assert_eq!(ed.disp_in_seg(0, 1), 2, "one glyph in, on row 0");
    assert_eq!(ed.disp_in_seg(0, 4), 0, "start of the third row");
    // End goes to the end of the visual row, never mid-glyph; Down carries the
    // offset across.
    ed.bs_mut().cursor = Pos { row: 0, col: 0 };
    press(&mut ed, KeyCode::End, Mods::NONE);
    assert_eq!(ed.bs().cursor, Pos { row: 0, col: 2 });
    press(&mut ed, KeyCode::Home, Mods::NONE);
    ed.bs_mut().cursor = Pos { row: 0, col: 1 };
    press(&mut ed, KeyCode::Down, Mods::NONE);
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
    assert!(ed.handle_mouse(me(MouseKind::Press(MouseButton::Left), 1, 1)));
    assert_eq!(ed.bs().cursor, Pos { row: 0, col: 0 });
    assert!(ed.handle_mouse(me(MouseKind::Press(MouseButton::Left), 1, 3)));
    assert_eq!(ed.bs().cursor, Pos { row: 0, col: 1 });
    assert!(ed.handle_mouse(me(MouseKind::Press(MouseButton::Left), 2, 3)));
    assert_eq!(ed.bs().cursor, Pos { row: 0, col: 3 });
    assert!(ed.handle_mouse(me(MouseKind::Press(MouseButton::Left), 3, 4)));
    assert_eq!(ed.bs().cursor, Pos { row: 0, col: 5 });
}

#[test]
fn wheel_scrolls_wide_rows_by_visual_row() {
    let mut ed = test_ed("中文字语言\nx");
    ed.show_line_numbers = false;
    ed.text_w = 4;
    ed.text_h = 1; // otherwise the sheet fits and there is nowhere to scroll
    ed.ensure_wrap_prefix();
    assert!(ed.handle_mouse(me(MouseKind::WheelDown, 0, 0)));
    assert_eq!(ed.bs().scroll, 3, "one buffer row is three visual rows");
    assert_eq!(ed.bs().cursor, Pos { row: 1, col: 0 });
}

#[test]
fn wrap_prefix_counts_visual_rows() {
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
    press_text(&mut ed, "X");
    ed.open_at(&b, 3, Some(2)).expect("open");
    assert_eq!(ed.buffers.len(), 2, "the edited buffer was kept");
    assert_eq!(ed.bs().buf.name.as_deref(), Some(b.as_path()));
    assert_eq!(ed.bs().cursor, Pos { row: 2, col: 1 });
    assert!(!ed.config.multibuffer, "the config is as it was");
    ed.open_at(&a, 2, None).expect("switch");
    assert_eq!(ed.bs().buf.name.as_deref(), Some(a.as_path()));
    assert_eq!(lines(&ed)[0], "Xa1", "the edit is still there");
    assert_eq!(ed.bs().cursor, Pos { row: 1, col: 0 });
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
    press(&mut ed, KeyCode::Enter, Mods::NONE);
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
    press(&mut ed, KeyCode::Char('s'), Mods::ALT);
    assert!(ed.status_text().unwrap().starts_with("Nowhere to send"));
    let got: Rc<RefCell<Vec<crate::send::SendEvent>>> = Rc::default();
    let sink = got.clone();
    ed.on_send = Some(Box::new(move |e| {
        sink.borrow_mut().push(e.clone());
        Ok("sent".into())
    }));
    ed.bs_mut().cursor = Pos { row: 1, col: 4 };
    press(&mut ed, KeyCode::Char('s'), Mods::ALT);
    // A selection from (0, 3) to (1, 8): `a() {\n    body`.
    ed.bs_mut().cursor = Pos { row: 0, col: 3 };
    ed.bs_mut().mark = Some(Pos { row: 0, col: 3 });
    ed.bs_mut().cursor = Pos { row: 1, col: 8 };
    press(&mut ed, KeyCode::Char('s'), Mods::ALT);
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
    // 500 lines in a 20-row viewport; the target has room above and below, which
    // is what makes centring mean anything — at the file's end the clamp wins.
    let mut ed = ed_with_lines(500);
    ed.text_h = 20;
    ed.show_line_numbers = false;
    ed.text_w = 40;
    ed.bs_mut().goto = Some(Pos { row: 249, col: 0 });
    ed.ensure_wrap_prefix();
    ed.apply_startup_pos();
    assert_eq!(ed.bs().cursor, Pos { row: 249, col: 0 });
    assert_eq!(ed.bs().goto, None, "applied once");
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
    // Near the start there is nothing above, so it is as centred as the file allows.
    let mut ed = ed_with_lines(100);
    ed.text_h = 20;
    ed.bs_mut().goto = Some(Pos { row: 1, col: 0 });
    ed.ensure_wrap_prefix();
    ed.apply_startup_pos();
    assert_eq!(ed.bs().scroll, 0, "no blank space above the first line");
    let mut ed = ed_with_lines(100);
    ed.text_h = 20;
    ed.bs_mut().goto = Some(Pos { row: 99, col: 0 });
    ed.ensure_wrap_prefix();
    ed.apply_startup_pos();
    assert_eq!(ed.bs().scroll, 80, "100 rows - 20 visible");
}

#[test]
fn centring_counts_visual_rows_when_wrapped() {
    // One long line above the target occupies several visual rows, so the scroll is
    // counted in those and centring must use the visual row.
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
    // The buffer row is 20, the visual row far below: using the buffer row would
    // put the view `seg` rows off.
    assert_eq!(
        vis,
        seg + 19,
        "the long line's segments plus the rows between"
    );
    assert_eq!(ed.bs().scroll, vis - 5, "centred in VISUAL rows");
}

#[test]
fn a_position_waits_for_its_row_to_arrive() {
    // A row a million lines in arrives long after the first frame; clamping to
    // what has arrived would open the file in the wrong place.
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
    // The loader appends rows and the table is extended, not rebuilt: drive both
    // paths over the same appends and compare.
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
        press(&mut ed, KeyCode::Backspace, Mods::NONE);
    }
    ed.ensure_wrap_prefix();
    assert_eq!(ed.bs().wrap_prefix, vec![0, 1, 2], "5 cols → 1 visual row");
}

/// Scrolling must re-highlight: the grid is a window over the viewport, and rows
/// revealed after the first frame would otherwise be drawn plain.
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
    ed.ensure_highlight();
    assert!(
        ed.bs().hl.style_at(Pos { row: 4, col: 0 }).is_some(),
        "the first viewport is highlighted"
    );

    let far = 1500;
    ed.bs_mut().scroll = far;
    ed.bs_mut().cursor = Pos { row: far, col: 0 };
    ed.ensure_wrap_prefix();
    ed.ensure_highlight();

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
    let text: String = (0..60_000)
        .map(|i| format!("pub fn f{i}(x: usize) -> usize {{ x + {i} }}\n"))
        .collect();
    assert!(text.len() > 2 << 20, "{} bytes", text.len());
    let mut ed = test_ed(&text);
    ed.bs_mut().buf.name = Some(std::path::PathBuf::from("big.rs"));
    ed.show_line_numbers = false;
    ed.text_w = 100;
    ed.text_h = 40;

    assert_eq!(
        ed.bs().hl.styled_window(),
        None,
        "open must not highlight a whole big file"
    );
    assert_eq!(ed.bs().hl.style_at(Pos { row: 5, col: 0 }), None);

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

    ed.bs_mut().cursor = Pos { row: 3, col: 0 };
    for _ in 0..5 {
        ed.insert_char('y');
    }
    ed.ensure_highlight();
    assert!(ed.bs().hl.styled_window().is_some());
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
    let text: String = (0..10)
        .map(|i| format!("{}{}\n", i, "a".repeat(29)))
        .collect();
    let mut ed = test_ed(&text);
    ed.show_line_numbers = false;
    ed.text_w = 10;
    ed.text_h = 6;
    ed.bs_mut().cursor = Pos { row: 5, col: 29 }; // visual row 5*3 + 2 = 17
    ed.adjust_scroll(ed.text_h);
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
    assert!(ed.handle_mouse(me(MouseKind::WheelDown, 0, 0)));
    assert_eq!(ed.bs().scroll, 3);
    assert_eq!(
        ed.bs().cursor,
        Pos { row: 1, col: 0 },
        "pinned to the viewport top"
    );
    assert!(ed.handle_mouse(me(MouseKind::WheelDown, 0, 0)));
    assert_eq!(ed.bs().scroll, 6);
    assert_eq!(ed.bs().cursor, Pos { row: 2, col: 0 });
}

#[test]
fn move_up_down_cross_wrap_segments() {
    let mut ed = test_ed(&format!("{}\n{}", "a".repeat(25), "b".repeat(5)));
    ed.show_line_numbers = false;
    ed.text_w = 10;
    ed.bs_mut().cursor = Pos { row: 1, col: 2 };
    press(&mut ed, KeyCode::Up, Mods::NONE);
    assert_eq!(
        ed.bs().cursor,
        Pos { row: 0, col: 22 },
        "up: same col on the visual row above"
    );
    press(&mut ed, KeyCode::Up, Mods::NONE);
    assert_eq!(ed.bs().cursor, Pos { row: 0, col: 12 });
    press(&mut ed, KeyCode::Up, Mods::NONE);
    assert_eq!(ed.bs().cursor, Pos { row: 0, col: 2 });
    press(&mut ed, KeyCode::Up, Mods::NONE);
    assert_eq!(
        ed.bs().cursor,
        Pos { row: 0, col: 2 },
        "top of the buffer: no move"
    );
    press(&mut ed, KeyCode::Down, Mods::NONE);
    assert_eq!(ed.bs().cursor, Pos { row: 0, col: 12 });
    press(&mut ed, KeyCode::Down, Mods::NONE);
    assert_eq!(ed.bs().cursor, Pos { row: 0, col: 22 });
    press(&mut ed, KeyCode::Down, Mods::NONE);
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
    press(&mut ed, KeyCode::Home, Mods::NONE);
    assert_eq!(
        ed.bs().cursor,
        Pos { row: 0, col: 10 },
        "Home = start of the visual row"
    );
    press(&mut ed, KeyCode::End, Mods::NONE);
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
    press(&mut ed, KeyCode::PageUp, Mods::NONE);
    assert_eq!(ed.bs().cursor, Pos { row: 3, col: 10 });
    press(&mut ed, KeyCode::PageDown, Mods::NONE);
    assert_eq!(ed.bs().cursor, Pos { row: 5, col: 0 });
}

#[test]
fn mouse_pos_maps_visual_rows_with_wrap() {
    let mut ed = test_ed(&format!("{}\n{}", "a".repeat(25), "b".repeat(5)));
    ed.show_line_numbers = false;
    ed.text_w = 10;
    ed.text_h = 10;
    assert!(ed.handle_mouse(me(MouseKind::Press(MouseButton::Left), 2, 3)));
    assert_eq!(ed.bs().cursor, Pos { row: 0, col: 13 });
    assert!(ed.handle_mouse(me(MouseKind::Press(MouseButton::Left), 4, 2)));
    assert_eq!(ed.bs().cursor, Pos { row: 1, col: 2 });
}

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
    assert_eq!(ed.config.tab_width, 4, "a tab is drawn four columns wide");
    assert_eq!(ed.config.indent, config::Indent::Spaces);
    assert_eq!(ed.config.indent_size, 4, "and a Tab inserts four spaces");
}

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
    press(&mut ed, KeyCode::Enter, Mods::NONE);
    assert_eq!(lines(&ed), vec!["    foo", "    "]);
    assert_eq!(ed.bs().cursor, Pos { row: 1, col: 4 });
}

/// The one-pass scan must agree with the obvious one: GCD 1 exits early, and that
/// claim is checked against a reference reaching every branch.
#[test]
fn indent_unit_matches_the_reference_scan() {
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
            return "    ".to_string();
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
        // Nothing to detect: the fallback is the configured size.
        if unit <= 1 || unit > 8 {
            return "    ".to_string();
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
        // The scan is what `indent = auto` asks for; the default is a setting.
        let mut ed = test_ed(text);
        ed.config.indent = config::Indent::Auto;
        ed.config.indent_size = 4;
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
    press(&mut ed, KeyCode::Enter, Mods::NONE);
    assert_eq!(lines(&ed), vec!["    foo", ""]);
    assert_eq!(ed.bs().cursor, Pos { row: 1, col: 0 });
}

#[test]
fn auto_indent_on_by_default_and_electric_after_brace() {
    let mut ed = test_ed("    foo");
    ed.bs_mut().cursor = Pos { row: 0, col: 7 };
    press(&mut ed, KeyCode::Enter, Mods::NONE);
    assert_eq!(lines(&ed), vec!["    foo", "    "]);
    assert_eq!(ed.bs().cursor, Pos { row: 1, col: 4 });
    let mut ed = test_ed("    if x {");
    ed.bs_mut().cursor = Pos { row: 0, col: 10 };
    press(&mut ed, KeyCode::Enter, Mods::NONE);
    assert_eq!(lines(&ed), vec!["    if x {", "        "]);
    assert_eq!(ed.bs().cursor, Pos { row: 1, col: 8 });
}

#[test]
fn mouse_click_drag_and_wheel() {
    let mut ed = test_ed("hello\nworld\n3\n4\n5\n6\n");
    ed.show_line_numbers = true;
    ed.text_w = 40;
    ed.text_h = 10;
    assert!(ed.handle_mouse(me(MouseKind::Press(MouseButton::Left), 2, 5)));
    assert_eq!(ed.bs().cursor, Pos { row: 1, col: 2 });
    assert_eq!(ed.bs().mark, Some(Pos { row: 1, col: 2 }));
    assert!(ed.handle_mouse(me(MouseKind::Drag(MouseButton::Left), 2, 4)));
    assert_eq!(ed.bs().cursor, Pos { row: 1, col: 1 });
    assert_eq!(ed.bs().mark, Some(Pos { row: 1, col: 2 }));
    ed.handle_mouse(me(MouseKind::Release(MouseButton::Left), 2, 4));
    assert_eq!(ed.bs().mark, Some(Pos { row: 1, col: 2 }));
    assert!(!ed.handle_mouse(me(MouseKind::Press(MouseButton::Left), 0, 3)));
    assert!(!ed.handle_mouse(me(MouseKind::Press(MouseButton::Left), 22, 3)));
}

/// A click places the cursor and arms nothing: leaving the anchor armed made a
/// later scroll redraw a selection.
#[test]
fn a_click_arms_no_selection_for_the_wheel_to_grow() {
    let text: String = (1..=40).map(|i| format!("L{i}\n")).collect();
    let mut ed = test_ed(&text);
    ed.text_w = 40;
    ed.text_h = 10;
    ed.bs_mut().cursor = Pos { row: 20, col: 0 };
    assert!(ed.handle_mouse(me(MouseKind::Press(MouseButton::Left), 3, 1)));
    assert_eq!(
        ed.bs().cursor,
        Pos { row: 2, col: 0 },
        "the click placed it"
    );
    ed.handle_mouse(me(MouseKind::Release(MouseButton::Left), 3, 1));
    assert_eq!(ed.bs().mark, None, "a click is not a selection");
    for _ in 0..4 {
        assert!(ed.handle_mouse(me(MouseKind::WheelDown, 0, 0)));
    }
    assert_eq!(ed.bs().cursor.row, 12, "the wheel pinned the edit point");
    assert!(ed.sel_span().is_none(), "and nothing got selected");
    for _ in 0..6 {
        ed.handle_mouse(me(MouseKind::WheelUp, 0, 0));
    }
    assert!(ed.sel_span().is_none(), "nor on the way back");
    assert_eq!(ed.bs().mark, None);
}

/// nano's other half of the click: on the cursor itself it toggles the mark.
#[test]
fn a_click_on_the_cursor_itself_toggles_the_mark() {
    let mut ed = test_ed("hello\nworld\n");
    ed.text_w = 40;
    ed.text_h = 10;
    let c = ed.bs().cursor;
    assert!(ed.handle_mouse(me(MouseKind::Press(MouseButton::Left), 1, c.col as u16)));
    assert_eq!(ed.bs().mark, Some(c), "clicking the cursor arms the mark");
    ed.handle_mouse(me(MouseKind::Release(MouseButton::Left), 1, c.col as u16));
    assert_eq!(
        ed.bs().mark,
        Some(c),
        "and the release keeps what was asked for"
    );
    assert!(ed.handle_mouse(me(MouseKind::Press(MouseButton::Left), 1, c.col as u16)));
    assert_eq!(ed.bs().mark, None);
    ed.handle_mouse(me(MouseKind::Release(MouseButton::Left), 1, c.col as u16));
    assert_eq!(ed.bs().mark, None);
}

/// The wheel works in a view drawn over the text, and only on the view: a click
/// there must not move the text's cursor.
#[test]
fn the_wheel_scrolls_a_preview_and_leaves_the_text_under_it_alone() {
    let text: String = (1..=60).map(|i| format!("para {i}\n\n")).collect();
    let mut ed = test_ed(&text);
    ed.bs_mut().buf.name = Some(PathBuf::from("/tmp/rano_wheel.md"));
    ed.text_w = 40;
    ed.text_h = 10;
    press(&mut ed, KeyCode::Char('p'), Mods::ALT);
    assert!(ed.diff_view.is_some(), "a markdown preview");
    let was = (ed.bs().scroll, ed.bs().cursor, ed.bs().mark);
    assert!(ed.handle_mouse(me(MouseKind::WheelDown, 3, 3)));
    assert_eq!(ed.diff_view.as_ref().unwrap().top, 3, "the view scrolled");
    assert!(ed.handle_mouse(me(MouseKind::WheelUp, 3, 3)));
    assert_eq!(ed.diff_view.as_ref().unwrap().top, 0, "and back");
    assert_eq!(
        (ed.bs().scroll, ed.bs().cursor, ed.bs().mark),
        was,
        "the text under it did not move"
    );
    // A press in a view now starts a selection (it says so by returning true); what it must not
    // do is move the text's cursor, which is what the rest of this test checks.
    let cursor_was = ed.bs().cursor;
    let _ = ed.handle_mouse(me(MouseKind::Press(MouseButton::Left), 3, 3));
    assert_eq!(
        ed.bs().cursor,
        cursor_was,
        "the text under the view keeps its cursor"
    );
    ed.handle_mouse(me(MouseKind::Release(MouseButton::Left), 3, 3));
    assert_eq!(ed.bs().mark, None);
    assert_eq!(ed.bs().cursor, was.1);
}

#[test]
fn mouse_wheel_scrolls_view_not_cursor() {
    let text: String = (1..=30).map(|i| format!("L{i}\n")).collect();
    let mut ed = test_ed(&text);
    ed.text_h = 10; // max_scroll = 30 - 10 = 20
    ed.bs_mut().cursor = Pos { row: 4, col: 1 };
    assert!(ed.handle_mouse(me(MouseKind::WheelDown, 0, 0)));
    assert_eq!(ed.bs().scroll, 3);
    assert_eq!(ed.bs().cursor.row, 4, "wheel must not move the cursor");
    assert!(ed.handle_mouse(me(MouseKind::WheelDown, 0, 0)));
    assert_eq!(ed.bs().scroll, 6);
    assert_eq!(ed.bs().cursor.row, 6, "cursor pinned to the viewport top");
    assert!(ed.handle_mouse(me(MouseKind::WheelUp, 0, 0)));
    assert_eq!(ed.bs().scroll, 3);
    assert_eq!(ed.bs().cursor.row, 6);
    ed.bs_mut().cursor = Pos { row: 12, col: 1 };
    assert!(ed.handle_mouse(me(MouseKind::WheelUp, 0, 0)));
    assert_eq!(ed.bs().scroll, 0);
    assert_eq!(
        ed.bs().cursor.row,
        9,
        "cursor pinned to the viewport bottom"
    );
    assert!(ed.handle_mouse(me(MouseKind::WheelUp, 0, 0)));
    assert_eq!(ed.bs().scroll, 0);
    assert_eq!(ed.bs().cursor.row, 9);
    ed.bs_mut().scroll = 20;
    ed.bs_mut().cursor = Pos { row: 25, col: 1 };
    assert!(ed.handle_mouse(me(MouseKind::WheelDown, 0, 0)));
    assert_eq!(ed.bs().scroll, 20, "scroll clamped at end of file");
    assert_eq!(ed.bs().cursor.row, 25);
}

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

#[test]
fn search_history_cycles() {
    let mut ed = test_ed("bar");
    press(&mut ed, KeyCode::Char('f'), Mods::CTRL);
    press_text(&mut ed, "foo");
    press(&mut ed, KeyCode::Enter, Mods::NONE);
    assert_eq!(ed.search_hist, vec!["foo".to_string()]);
    press(&mut ed, KeyCode::Char('f'), Mods::CTRL);
    for _ in 0..3 {
        press(&mut ed, KeyCode::Backspace, Mods::NONE);
    }
    press(&mut ed, KeyCode::Up, Mods::NONE);
    let p = ed.prompt.as_ref().unwrap();
    assert_eq!(p.text, "foo");
    assert_eq!(p.cursor, 3);
    press(&mut ed, KeyCode::Down, Mods::NONE);
    let p = ed.prompt.as_ref().unwrap();
    assert_eq!(p.text, "", "past the newest entry restores the draft");
    press(&mut ed, KeyCode::Up, Mods::NONE);
    press(&mut ed, KeyCode::Up, Mods::NONE);
    let p = ed.prompt.as_ref().unwrap();
    assert_eq!(p.text, "foo", "stays on the oldest entry");
    assert!(ed.prompt.is_some());
}

#[test]
fn prompt_history_dedupes_consecutive() {
    let mut ed = test_ed("");
    for _ in 0..2 {
        press(&mut ed, KeyCode::Char('t'), Mods::CTRL);
        press_text(&mut ed, "true");
        press(&mut ed, KeyCode::Enter, Mods::NONE);
    }
    assert_eq!(ed.exec_hist, vec!["true".to_string()]);
}

#[test]
fn prompt_history_skips_empty() {
    let mut ed = test_ed("");
    press(&mut ed, KeyCode::Char('o'), Mods::CTRL);
    press(&mut ed, KeyCode::Enter, Mods::NONE);
    assert!(ed.file_hist.is_empty());
}

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
    press(&mut ed, KeyCode::Char('o'), Mods::CTRL);
    press_text(&mut ed, &format!("{}/alp", d.0.display()));
    press(&mut ed, KeyCode::Tab, Mods::NONE);
    let p = ed.prompt.as_ref().unwrap();
    assert_eq!(p.text, format!("{}/alpha", d.0.display()));
    assert!(ed.prompt.is_some());
}

#[test]
fn prompt_word_motion() {
    let mut ed = test_ed("");
    press(&mut ed, KeyCode::Char('t'), Mods::CTRL);
    press_text(&mut ed, "foo bar_baz");
    press(&mut ed, KeyCode::Home, Mods::NONE);
    press(&mut ed, KeyCode::Char('f'), Mods::ALT);
    assert_eq!(ed.prompt.as_ref().unwrap().cursor, 4);
    press(&mut ed, KeyCode::Char('f'), Mods::ALT);
    assert_eq!(ed.prompt.as_ref().unwrap().cursor, 11);
    press(&mut ed, KeyCode::Char('b'), Mods::ALT);
    assert_eq!(ed.prompt.as_ref().unwrap().cursor, 4);
    press(&mut ed, KeyCode::Char('b'), Mods::ALT);
    assert_eq!(ed.prompt.as_ref().unwrap().cursor, 0);
    assert!(ed.prompt.is_some());
}

#[test]
fn prompt_ctrl_arrow_word_motion() {
    let mut ed = test_ed("");
    press(&mut ed, KeyCode::Char('t'), Mods::CTRL);
    press_text(&mut ed, "foo bar_baz");
    press(&mut ed, KeyCode::Home, Mods::NONE);
    press(&mut ed, KeyCode::Right, Mods::CTRL);
    assert_eq!(ed.prompt.as_ref().unwrap().cursor, 4);
    press(&mut ed, KeyCode::Left, Mods::CTRL);
    assert_eq!(ed.prompt.as_ref().unwrap().cursor, 0);
    assert!(ed.prompt.is_some());
}

#[test]
fn regex_search_matches_line_starts() {
    let mut ed = test_ed("ba\naba\nbar");
    ed.search_regex = true;
    press(&mut ed, KeyCode::Char('f'), Mods::CTRL);
    press_text(&mut ed, "^ba");
    press(&mut ed, KeyCode::Enter, Mods::NONE);
    assert_eq!(ed.bs().cursor, Pos { row: 0, col: 0 });
    press(&mut ed, KeyCode::Char('f'), Mods::ALT);
    assert_eq!(
        ed.bs().cursor,
        Pos { row: 2, col: 0 },
        "aba must be skipped"
    );
}

#[test]
fn case_toggle_search() {
    let mut ed = test_ed("foo\nFOO");
    press(&mut ed, KeyCode::Char('f'), Mods::CTRL);
    press_text(&mut ed, "FOO");
    press(&mut ed, KeyCode::Enter, Mods::NONE);
    assert_eq!(
        ed.bs().cursor,
        Pos { row: 0, col: 0 },
        "default is case-insensitive"
    );
    press(&mut ed, KeyCode::Char('f'), Mods::ALT);
    assert_eq!(ed.bs().cursor, Pos { row: 1, col: 0 });
    ed.bs_mut().search_matches = None; // let ^F re-open the prompt
    press(&mut ed, KeyCode::Char('f'), Mods::CTRL);
    press(&mut ed, KeyCode::Char('c'), Mods::ALT);
    assert!(ed.search_case_sensitive);
    press(&mut ed, KeyCode::Enter, Mods::NONE);
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
    press(&mut ed, KeyCode::Char('f'), Mods::CTRL);
    press_text(&mut ed, "[");
    press(&mut ed, KeyCode::Enter, Mods::NONE);
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
    press(&mut ed, KeyCode::Char('f'), Mods::CTRL);
    press_text(&mut ed, "q");
    press(&mut ed, KeyCode::Char('c'), Mods::ALT);
    assert!(ed.search_case_sensitive);
    assert!(
        ed.status_text().unwrap().contains("Case sensitive: on"),
        "status: {}",
        ed.status_text().unwrap()
    );
    press(&mut ed, KeyCode::Char('c'), Mods::ALT);
    assert!(!ed.search_case_sensitive);
    press(&mut ed, KeyCode::Char('r'), Mods::ALT);
    assert!(ed.search_regex);
    assert!(ed.status_text().unwrap().contains("Regex: on"));
    press(&mut ed, KeyCode::Char('r'), Mods::ALT);
    assert!(!ed.search_regex);
    let p = ed.prompt.as_ref().unwrap();
    assert_eq!(p.text, "q", "toggle leaves prompt text unchanged");
    assert_eq!(p.cursor, 1);
    press(&mut ed, KeyCode::Enter, Mods::NONE);
    press(&mut ed, KeyCode::Char('o'), Mods::CTRL);
    press(&mut ed, KeyCode::Char('c'), Mods::ALT);
    assert!(!ed.search_case_sensitive);
    let p = ed.prompt.as_ref().unwrap();
    assert!(p.text.contains('c'), "M-C must type in a WriteName prompt");
}

// Multi-buffer: open pushes or replaces, M-</M-> switch, ^X cycles over modified
// buffers.
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
    press(&mut ed, KeyCode::F(8), Mods::NONE);
    assert!(ed.prompt.is_some());
    press_text(&mut ed, &f2.display().to_string());
    press(&mut ed, KeyCode::Enter, Mods::NONE);
    assert_eq!(ed.buffers.len(), 2);
    assert_eq!(ed.cur, 1);
    assert_eq!(lines(&ed), vec!["x", "y", "z"]);
    assert_eq!(ed.bs().buf.name, Some(f2));
}

/// An untouched scratch is replaced even with multibuffer on: the empty unnamed
/// buffer of a bare `rano` is not kept beside the file.
#[test]
fn open_file_multibuffer_replaces_an_untouched_scratch() {
    let d = temp_dir("open_scratch");
    let f = d.0.join("b.txt");
    fs::write(&f, "x\ny").unwrap();
    let cfg = config::Config {
        multibuffer: true,
        ..config::Config::default()
    };
    let mut ed = Editor::new(buf_with(""), cfg);
    ed.open_at(&f, 2, None).unwrap();
    assert_eq!(ed.buffers.len(), 1);
    assert_eq!(ed.bs().buf.name, Some(f.clone()));
    let cfg = config::Config {
        multibuffer: true,
        ..config::Config::default()
    };
    let mut ed = Editor::new(buf_with("notes"), cfg);
    ed.open_at(&f, 1, None).unwrap();
    assert_eq!(ed.buffers.len(), 2);
}

#[test]
fn open_file_replaces_when_disabled() {
    let d = temp_dir("open_single");
    let f1 = d.0.join("a.txt");
    fs::write(&f1, "x\ny\nz").unwrap();
    let mut ed = test_ed("seed");
    press(&mut ed, KeyCode::F(8), Mods::NONE);
    press_text(&mut ed, &f1.display().to_string());
    press(&mut ed, KeyCode::Enter, Mods::NONE);
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
    press(&mut ed, KeyCode::F(8), Mods::NONE);
    press_text(&mut ed, &f1.display().to_string());
    press(&mut ed, KeyCode::Enter, Mods::NONE);
    assert_eq!(ed.buffers.len(), 2, "the edited buffer was not replaced");
    assert_eq!(lines(&ed), vec!["x", "y", "z"]);
    assert!(ed.status_text().unwrap().contains("unsaved edits"));
    assert!(!ed.config.multibuffer, "the setting is unchanged");
    assert_eq!(
        ed.buffers[0].buf.row(0).iter().collect::<String>(),
        "EDIT seed"
    );
    let f2 = d.0.join("b.txt");
    fs::write(&f2, "b").unwrap();
    press(&mut ed, KeyCode::F(8), Mods::NONE);
    press_text(&mut ed, &f2.display().to_string());
    press(&mut ed, KeyCode::Enter, Mods::NONE);
    assert_eq!(ed.buffers.len(), 2);
    assert_eq!(lines(&ed), vec!["b"]);
}

#[test]
fn open_missing_file_flashes() {
    let mut ed = test_ed("seed");
    press(&mut ed, KeyCode::F(8), Mods::NONE);
    press_text(&mut ed, "/no/such/file/rano_open_test.txt");
    press(&mut ed, KeyCode::Enter, Mods::NONE);
    let st = ed.status_text().unwrap();
    assert!(st.contains("Error:"), "status: {st}");
    assert!(ed.prompt.is_some(), "the open prompt stays open on error");
    assert_eq!(lines(&ed), vec!["seed"]);
}

#[test]
fn switch_buffers_wrap() {
    let mut ed = test_ed("a");
    ed.buffers.push(BufferState::new(buf_with("b")));
    press(&mut ed, KeyCode::Char('>'), Mods::ALT);
    assert_eq!(ed.cur, 1);
    let st = ed.status_text().unwrap();
    assert!(st.contains("Buffer:"), "status: {st}");
    press(&mut ed, KeyCode::Char('>'), Mods::ALT);
    assert_eq!(ed.cur, 0, "wraps past the last");
    press(&mut ed, KeyCode::Char('<'), Mods::ALT);
    assert_eq!(ed.cur, 1, "wraps back past the first");
}

#[test]
fn per_buffer_state_isolated() {
    let mut ed = test_ed("a\nb");
    ed.buffers.push(BufferState::new(buf_with("x\ny")));
    ed.cur = 1;
    press(&mut ed, KeyCode::Down, Mods::NONE);
    press(&mut ed, KeyCode::Right, Mods::NONE);
    assert_eq!(ed.bs().cursor, Pos { row: 1, col: 1 });
    press(&mut ed, KeyCode::Char('>'), Mods::ALT);
    assert_eq!(
        ed.bs().cursor,
        Pos { row: 0, col: 0 },
        "buffer 1 keeps its own cursor"
    );
    press(&mut ed, KeyCode::Char('<'), Mods::ALT);
    assert_eq!(
        ed.bs().cursor,
        Pos { row: 1, col: 1 },
        "buffer 2's cursor preserved across the switch"
    );
    press_text(&mut ed, "Z");
    press(&mut ed, KeyCode::Char('>'), Mods::ALT);
    press_text(&mut ed, "Q");
    press(&mut ed, KeyCode::Char('u'), Mods::ALT);
    assert_eq!(lines(&ed), vec!["a", "b"], "buffer 1 undoes only its edit");
    press(&mut ed, KeyCode::Char('<'), Mods::ALT);
    press(&mut ed, KeyCode::Char('u'), Mods::ALT);
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
    press(&mut ed, KeyCode::Char('>'), Mods::ALT);
    assert_eq!(ed.title_text(), "[2/2] /tmp/b.txt");
}

#[test]
fn quit_cycles_modified_buffers() {
    let mut ed = test_ed("a");
    ed.buffers.push(BufferState::new(buf_with("b")));
    ed.buffers[0].buf.modified = true;
    ed.buffers[1].buf.modified = true;
    press(&mut ed, KeyCode::Char('x'), Mods::CTRL);
    assert!(matches!(
        ed.prompt.as_ref().map(|p| p.kind),
        Some(PromptKind::ConfirmSave)
    ));
    assert_eq!(ed.cur, 0);
    press(&mut ed, KeyCode::Char('n'), Mods::NONE);
    assert!(!ed.buffers[0].buf.modified, "'n' discards this buffer");
    assert_eq!(ed.cur, 1, "next modified buffer becomes current");
    assert!(matches!(
        ed.prompt.as_ref().map(|p| p.kind),
        Some(PromptKind::ConfirmSave)
    ));
    assert!(!ed.quit);
    press(&mut ed, KeyCode::Char('n'), Mods::NONE);
    assert!(!ed.buffers[1].buf.modified);
    assert!(ed.quit, "all modified buffers dealt with → quit");
}

#[test]
fn try_quit_other_modified() {
    let mut ed = test_ed("a");
    ed.buffers.push(BufferState::new(buf_with("b")));
    ed.buffers[1].buf.modified = true;
    press(&mut ed, KeyCode::Char('x'), Mods::CTRL);
    assert_eq!(ed.cur, 1, "the modified buffer becomes current");
    assert!(matches!(
        ed.prompt.as_ref().map(|p| p.kind),
        Some(PromptKind::ConfirmSave)
    ));
    assert!(!ed.quit);
    press(&mut ed, KeyCode::Esc, Mods::NONE);
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
    press(&mut ed, KeyCode::Char('x'), Mods::CTRL);
    press(&mut ed, KeyCode::Char('y'), Mods::NONE);
    assert!(f1.exists());
    assert!(!ed.buffers[0].buf.modified);
    assert_eq!(ed.cur, 1, "saved → next modified buffer prompted");
    assert!(matches!(
        ed.prompt.as_ref().map(|p| p.kind),
        Some(PromptKind::ConfirmSave)
    ));
    press(&mut ed, KeyCode::Char('y'), Mods::NONE);
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
    press(&mut ed, KeyCode::Char('w'), Mods::ALT);
    assert_eq!(ed.buffers.len(), 2);
    assert_eq!(ed.cur, 1);
    assert_eq!(
        lines(&ed),
        vec!["2"],
        "the following buffer takes its place"
    );
    press(&mut ed, KeyCode::Char('w'), Mods::ALT);
    assert_eq!(ed.cur, 0, "closing the last one lands on the new last");
    press(&mut ed, KeyCode::Char('w'), Mods::ALT);
    assert_eq!(ed.buffers.len(), 1, "the last buffer stays");
    assert!(ed.status_text().unwrap().contains("^X"));
}

#[test]
fn close_modified_buffer_asks_and_n_discards() {
    let mut ed = named_buffers(&["/tmp/rano_m0", "/tmp/rano_m1"]);
    ed.buffers[0].buf.modified = true;
    press(&mut ed, KeyCode::Char('w'), Mods::ALT);
    assert!(matches!(
        ed.prompt.as_ref().map(|p| p.kind),
        Some(PromptKind::ConfirmClose)
    ));
    press(&mut ed, KeyCode::Esc, Mods::NONE);
    assert_eq!(ed.buffers.len(), 2, "cancel keeps it");
    press(&mut ed, KeyCode::Char('w'), Mods::ALT);
    press(&mut ed, KeyCode::Char('n'), Mods::NONE);
    assert_eq!(ed.buffers.len(), 1);
    assert_eq!(lines(&ed), vec!["1"]);
}

#[test]
fn close_modified_buffer_y_saves_then_closes() {
    let d = temp_dir("close_save");
    let f = d.0.join("a.txt");
    let mut ed = named_buffers(&[f.to_str().unwrap(), "/tmp/rano_cs1"]);
    ed.buffers[0].buf.modified = true;
    press(&mut ed, KeyCode::Char('w'), Mods::ALT);
    press(&mut ed, KeyCode::Char('y'), Mods::NONE);
    assert_eq!(fs::read_to_string(&f).unwrap().trim_end(), "0");
    assert_eq!(ed.buffers.len(), 1);
    assert!(!ed.close_after_save);
}

#[test]
fn cancelling_the_file_name_of_a_close_forgets_the_close() {
    let mut ed = test_ed("scratch");
    ed.buffers.push(BufferState::new(buf_with("other")));
    ed.bs_mut().buf.modified = true;
    press(&mut ed, KeyCode::Char('w'), Mods::ALT);
    press(&mut ed, KeyCode::Char('y'), Mods::NONE);
    assert!(matches!(
        ed.prompt.as_ref().map(|p| p.kind),
        Some(PromptKind::WriteName)
    ));
    press(&mut ed, KeyCode::Esc, Mods::NONE);
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
    press(&mut ed, KeyCode::Char('w'), Mods::ALT);
    assert_eq!(
        ed.def_back.len(),
        1,
        "entries into the closed buffer are gone"
    );
    assert_eq!(ed.def_back[0].idx, Some(1), "later indices shift down");
    ed.cur = 0;
    press(&mut ed, KeyCode::Char(','), Mods::ALT);
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
    press(&mut ed, KeyCode::Char('>'), Mods::ALT);
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
    press(&mut ed, KeyCode::Char(','), Mods::ALT);
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
    let buf = ui::Screen::of(&ed, 60, 12);
    let row = |y: u16| -> String { (0..60).map(|x| buf[(x, y)].symbol.clone()).collect() };
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
    let buf = ui::Screen::of(&ed, 60, 12);
    let row = |y: u16| -> String { (0..60).map(|x| buf[(x, y)].symbol.clone()).collect() };
    assert!(row(1).contains("Buffers (2)"), "{}", row(1));
    assert!(row(2).contains("rano_draw_a"), "{}", row(2));
    assert!(row(3).contains("rano_draw_b"), "{}", row(3));
    assert!(!(1..9).any(|y| row(y).contains("hidden text")));
}

// ---------- read-only mode ----------

/// Nothing in read-only mode changes the buffer, and it says why.
#[test]
fn read_only_refuses_every_way_to_change_the_buffer() {
    // Each of these records an undo step except `undo`/`redo`: a new mutation that
    // forgets the gate is a hole this test makes visible.
    for (label, act) in [
        ("insert_char", 0usize),
        ("newline", 1),
        ("backspace", 2),
        ("delete_at", 3),
        ("cut", 4),
        ("paste", 5),
        ("paste_text", 6),
        ("delete_char_cut", 7),
        ("undo", 8),
        ("redo", 9),
        ("justify", 10),
        ("sort", 11),
        ("read", 12),
        ("insert_str_plain", 13),
    ] {
        let mut ed = test_ed("alpha\nbeta\ngamma\n");
        ed.set_read_only(true);
        // Somewhere in the middle, so every action has something to do.
        ed.bs_mut().cursor = crate::buffer::Pos { row: 1, col: 2 };
        ed.bs_mut().mark = Some(crate::buffer::Pos { row: 0, col: 0 });
        let before = lines(&ed);
        match act {
            0 => ed.insert_char('X'),
            1 => ed.newline(),
            2 => ed.backspace(),
            3 => ed.delete_at(),
            4 => ed.cut(),
            5 => ed.paste(),
            6 => ed.paste_text("pasted"),
            7 => ed.delete_char_cut(),
            8 => ed.undo(),
            9 => ed.redo(),
            10 => ed.justify(),
            11 => ed.sort_lines(),
            12 => ed.do_read("some-file.txt".to_string()),
            _ => ed.paste_text("plain"),
        };
        assert_eq!(lines(&ed), before, "{label} changed the buffer");
        let said = ed.status_text().unwrap_or_default();
        assert!(
            said.contains("Read-only"),
            "{label} did not say why: {said:?}"
        );
    }
}

/// A save is refused and the file on disk is untouched: the point of refusing at
/// the keystroke.
#[test]
fn read_only_refuses_the_save_and_leaves_the_file_alone() {
    let d = temp_dir("ro_save");
    let path = d.0.join("log.txt");
    fs::write(&path, "one\ntwo\n").unwrap();
    let buf = Buffer::from_file(&path).unwrap();
    let mut ed = Editor::new(buf, config::Config::default());

    ed.set_read_only(true);
    ed.bs_mut().buf.modified = true;
    ed.save_to(path.clone());
    assert_eq!(fs::read(&path).unwrap(), b"one\ntwo\n", "the file changed");
    let said = ed.status_text().unwrap_or_default();
    assert!(said.contains("Read-only"), "no reason given: {said:?}");

    // `start_write` is refused too, so no name is even asked for.
    ed.start_write();
    assert!(
        ed.prompt.is_none(),
        "a write prompt opened in read-only mode"
    );

    // And turning it off restores both editing and saving.
    ed.set_read_only(false);
    ed.insert_char('!');
    assert!(lines(&ed)[0].starts_with('!'), "editing did not come back");
    ed.save_to(path.clone());
    assert_eq!(
        fs::read(&path).unwrap(),
        b"!one\ntwo\n".to_vec().as_slice().to_vec()
    );
}

/// Navigation and search are not editing, and must still work in a file you will
/// not change.
#[test]
fn read_only_still_moves_and_searches() {
    let mut ed = test_ed("alpha\nbeta\ngamma\n");
    ed.set_read_only(true);
    ed.bs_mut().cursor = crate::buffer::Pos { row: 0, col: 0 };
    ed.handle_key(KeyEvent::new(KeyCode::Down, Mods::NONE));
    assert_eq!(ed.bs().cursor.row, 1, "movement works");
    ed.bs_mut().cursor = crate::buffer::Pos { row: 2, col: 0 };
    assert_eq!(ed.bs().cursor.row, 2);
    // The mark is view state, not document state: it may be set.
    ed.toggle_mark();
    assert!(ed.bs().mark.is_some(), "a mark is not an edit");
    assert_eq!(lines(&ed), vec!["alpha", "beta", "gamma"]);
}

/// The title says the buffer cannot be written — what a reader must know before
/// typing.
#[test]
fn the_title_says_view_when_read_only() {
    let mut ed = test_ed("x\n");
    let t = ui::title_for_test(&mut ed, 80);
    assert!(!t.contains("VIEW"), "{t:?}");
    ed.set_read_only(true);
    let t = ui::title_for_test(&mut ed, 80);
    assert!(t.contains("VIEW"), "{t:?}");
    // And it sits beside the mark rather than replacing it.
    ed.toggle_mark();
    let t = ui::title_for_test(&mut ed, 80);
    assert!(t.contains("VIEW") && t.contains('M'), "{t:?}");
}

// ---------- tail mode (-f/--follow) ----------

/// Pump the loader until the file has arrived: the same bounded loop `run` makes,
/// because a test that waits forever hangs CI.
fn settle(ed: &mut Editor) {
    for _ in 0..500 {
        ed.load_poll();
        if !ed.loading() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    panic!("the load never finished");
}

/// A `.log` of `n` numbered lines, in its own scratch directory.
fn numbered_log(tag: &str, n: usize) -> (TempDir, PathBuf) {
    let d = temp_dir(tag);
    let path = d.0.join("big.log");
    let text: String = (1..=n).map(|i| format!("line {i}\n")).collect();
    fs::write(&path, text).unwrap();
    (d, path)
}

/// A tail opens at the end: the rows it shows are the file's last ones.
#[test]
fn a_tail_open_shows_the_end_of_the_file() {
    let n = crate::loader::TAIL_ROWS + 300;
    let (_d, path) = numbered_log("tail_end", n);
    let mut ed = test_ed("");
    ed.start_tail(&path).expect("tail open");
    settle(&mut ed);

    let rows = lines(&ed);
    assert_eq!(
        rows.len(),
        crate::loader::TAIL_ROWS,
        "a tail is the last few screens, not the {n}-line file"
    );
    assert_eq!(
        rows[0],
        format!("line {}", n - crate::loader::TAIL_ROWS + 1)
    );
    assert_eq!(rows[rows.len() - 1], format!("line {n}"));
}

/// The view lands at the bottom: the cursor is on the last row, so the scroll that
/// follows it leaves the end of the file at the foot of the screen.
#[test]
fn a_tail_lands_at_the_bottom() {
    let (_d, path) = numbered_log("tail_bottom", crate::loader::TAIL_ROWS + 300);
    let mut ed = test_ed("");
    ed.text_h = 24;
    ed.start_tail(&path).expect("tail open");
    settle(&mut ed);
    // `tick` is what runs the scroll, so ask for it the way the run loop does.
    ed.tick(Instant::now());

    let rows = lines(&ed);
    assert_eq!(ed.bs().cursor.row, rows.len() - 1, "not on the last row");
    assert_eq!(
        ed.bs().scroll,
        rows.len() - ed.text_h,
        "not scrolled to the end"
    );
}

/// A tail is a view of somebody else's file: read-only, refusal at the keystroke.
#[test]
fn a_tail_is_read_only() {
    let (_d, path) = numbered_log("tail_ro", crate::loader::TAIL_ROWS + 300);
    let mut ed = test_ed("");
    ed.start_tail(&path).expect("tail open");
    settle(&mut ed);
    assert!(ed.bs().read_only, "a tail opened editable");

    let before = lines(&ed);
    ed.bs_mut().cursor = Pos { row: 0, col: 0 };
    ed.insert_char('X');
    assert_eq!(lines(&ed), before, "typing changed a tail");
    let said = ed.status_text().unwrap_or_default();
    assert!(said.contains("Read-only"), "it did not say why: {said:?}");
}

/// A tail follows: a `-f` view shows the new lines as they arrive, in order.
#[test]
fn a_tail_follows_the_growth() {
    use std::io::Write as _;
    let (_d, path) = numbered_log("tail_follow", 5);
    let mut ed = test_ed("");
    ed.text_h = 24;
    ed.start_tail(&path).expect("tail open");
    settle(&mut ed);
    assert_eq!(lines(&ed).len(), 5);
    assert_eq!(ed.bs().cursor.row, 4, "it opened at the bottom");

    // Something else appends, a row at a time.
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .expect("append");
    writeln!(f, "line 6").expect("write");
    f.flush().expect("flush");
    let deadline = Instant::now() + Duration::from_secs(5);
    while lines(&ed).len() < 6 {
        assert!(Instant::now() < deadline, "the growth never arrived");
        ed.load_poll();
        std::thread::sleep(Duration::from_millis(2));
    }
    // And the view came with it, because it was at the end.
    ed.tick(Instant::now());
    assert_eq!(ed.bs().cursor.row, 5, "the view did not follow the end");
    assert_eq!(lines(&ed).last().unwrap(), "line 6");

    // A burst arrives whole and in order.
    writeln!(f, "line 7").expect("write");
    writeln!(f, "line 8").expect("write");
    f.flush().expect("flush");
    let deadline = Instant::now() + Duration::from_secs(5);
    while lines(&ed).len() < 8 {
        assert!(Instant::now() < deadline, "the burst never arrived");
        ed.load_poll();
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(lines(&ed).last().unwrap(), "line 8");
    assert_eq!(ed.bs().cursor.row, 7);
}

/// A follow does not yank a reader: growth must not pull a view that is not at the
/// end.
#[test]
fn a_follow_leaves_a_scrolled_away_view_alone() {
    use std::io::Write as _;
    let n = crate::loader::TAIL_ROWS + 1000; // more than a screen, so it can scroll
    let want = crate::loader::TAIL_ROWS + 1; // the tail, plus the one that arrives
    let (_d, path) = numbered_log("tail_follow_scroll", n);
    let mut ed = test_ed("");
    ed.text_h = 24;
    ed.start_tail(&path).expect("tail open");
    settle(&mut ed);
    ed.tick(Instant::now());
    assert_eq!(lines(&ed).len(), crate::loader::TAIL_ROWS);
    // The user scrolls back to read the top of the tail.
    {
        let bs = ed.bs_mut();
        bs.scroll = 0;
        bs.cursor = Pos { row: 0, col: 0 };
    }

    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .expect("append");
    writeln!(f, "line {}", n + 1).expect("write");
    f.flush().expect("flush");
    let deadline = Instant::now() + Duration::from_secs(5);
    while lines(&ed).len() < want {
        assert!(Instant::now() < deadline, "the growth never arrived");
        ed.load_poll();
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(lines(&ed).last().unwrap(), &format!("line {}", n + 1));
    assert_eq!(ed.bs().scroll, 0, "the view was yanked to the bottom");
    assert_eq!(ed.bs().cursor.row, 0, "the cursor was moved");
}

/// A plain open is not a tail: the whole file, editable, from row 1.
#[test]
fn a_plain_open_is_not_a_tail() {
    let n = crate::loader::TAIL_ROWS + 300;
    let (_d, path) = numbered_log("tail_none", n);
    let mut ed = test_ed("");
    ed.start_load(&path).expect("open");
    settle(&mut ed);

    assert!(!ed.bs().read_only);
    assert!(ed.bs().tail.is_none());
    let rows = lines(&ed);
    assert_eq!(rows.len(), n, "a plain open shows the whole file");
    assert_eq!(rows[0], "line 1");
}

/// Pump the line count the way the run loop does.
fn settle_count(ed: &mut Editor) {
    for _ in 0..1000 {
        ed.line_count_poll();
        if !ed.counting() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    panic!("the line count never finished");
}

/// A tail's rows are not the file's lines, and the editor says so: the count is
/// what turns the buffer's row 0 into the file's line 1001.
#[test]
fn a_tail_numbering_is_unknown_until_the_count_answers() {
    // 1200 rows: the tail is 1001..1200, and until the count lands there is no
    // number at all.
    let n = crate::loader::TAIL_ROWS + 1000;
    let (_d, path) = numbered_log("tail_count", n);
    let mut ed = test_ed("");
    ed.start_tail(&path).expect("tail open");
    settle(&mut ed);

    assert!(
        ed.bs().line_number(0).is_none(),
        "a line number before anything counted it"
    );
    assert!(ed.counting(), "no count was started");
    let said = ed.counting_text().unwrap_or_default();
    assert!(said.contains("Counting lines"), "{said:?}");
    // A flash wins over the count: a refusal must never be hidden by a late number.
    ed.flash("something happened");
    assert_eq!(ed.status_text().as_deref(), Some("something happened"));

    settle_count(&mut ed);
    assert_eq!(ed.bs().line_number(0), Some(1001));
    assert_eq!(
        ed.bs().line_number(crate::loader::TAIL_ROWS - 1),
        Some(n as u64)
    );
    assert!(!ed.counting(), "the job outlived its answer");

    // An ordinary buffer names its own rows from 1.
    let ed = test_ed("a\nb\nc\n");
    assert_eq!(ed.bs().line_number(0), Some(1));
    assert_eq!(ed.bs().line_number(2), Some(3));
}

/// The gutter and the status line are blank while the count is in flight, and the
/// file's numbers when it lands, the gutter's width included.
#[test]
fn the_gutter_and_the_status_line_wait_for_the_count() {
    let n = crate::loader::TAIL_ROWS + 1000; // 1200 rows → 4 digits
    let (_d, path) = numbered_log("tail_render", n);
    let mut ed = test_ed("");
    ed.show_line_numbers = true;
    ed.text_w = 100;
    ed.text_h = 24;
    ed.start_tail(&path).expect("tail open");
    settle(&mut ed);
    // Not `tick` first: `tick` adopts the count, and this is the frame before it.

    let (before_first, before_status) = {
        let screen = ui::Screen::of(&ed, 100, 30);
        let row = |y: u16| -> String { (0..100).map(|x| screen[(x, y)].symbol.clone()).collect() };
        (row(1), row(27))
    };
    assert!(
        before_first.starts_with("    line 1001"),
        "the gutter claimed a number: {before_first:?}"
    );
    assert!(
        before_status.contains("Ln ?"),
        "the status line claimed a number: {before_status:?}"
    );

    settle_count(&mut ed);
    // The tail lands at the bottom: put the view and the cursor on the first row.
    {
        let bs = ed.bs_mut();
        bs.scroll = 0;
        bs.cursor = Pos { row: 0, col: 0 };
    }
    let (after_first, after_status) = {
        let screen = ui::Screen::of(&ed, 100, 30);
        let row = |y: u16| -> String { (0..100).map(|x| screen[(x, y)].symbol.clone()).collect() };
        (row(1), row(27))
    };
    assert!(
        after_first.starts_with("1001 line 1001"),
        "the gutter is not the file's numbering, at the file's width: {after_first:?}"
    );
    assert!(
        after_status.contains("Ln 1001"),
        "the status line is not the file's numbering: {after_status:?}"
    );
}

/// Rows past the settled boundary are never parsed, asserted on the parse window.
/// A `.rs` name: `syntax::detect` has no language for `.log`.
#[test]
fn a_tail_does_not_parse_rows_past_the_settled_boundary() {
    use std::io::Write as _;
    let d = temp_dir("tail_settled");
    let path = d.0.join("settled.rs");
    let text: String = (0..300).map(|i| format!("let x{i} = {i};\n")).collect();
    fs::write(&path, &text).unwrap();

    let mut ed = test_ed("");
    ed.text_h = 24;
    ed.show_line_numbers = true;
    ed.start_tail(&path).expect("tail open");
    settle(&mut ed);
    ed.tick(Instant::now());

    let settled = ed.bs().tail.expect("a tail").rows_at_open.expect("settled");
    assert_eq!(
        settled,
        crate::loader::TAIL_ROWS,
        "the tail read is the settled set"
    );
    let have = ed.bs().hl.styled_window().expect("a window");
    assert_eq!(
        have.rows.1,
        settled - 1,
        "the window reached past the boundary"
    );

    // Growth: the new row must not be parsed, nor dirty the highlight.
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .expect("append");
    writeln!(f, "fn main() {{}}").expect("write");
    f.flush().expect("flush");
    let deadline = Instant::now() + Duration::from_secs(5);
    while lines(&ed).len() < settled + 1 {
        assert!(Instant::now() < deadline, "the growth never arrived");
        ed.load_poll();
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(
        !ed.highlight_dirty,
        "an append to a settled tail asked for a re-highlight"
    );
    // Forced, so the assertion is about the window, not about a refresh not happening.
    ed.highlight_dirty = true;
    ed.tick(Instant::now());
    let have = ed.bs().hl.styled_window().expect("still a window");
    assert!(
        have.rows.1 < settled,
        "the parser was handed a row the file is still writing: {:?}",
        have.rows
    );
    assert!(
        ed.bs()
            .hl
            .style_at(Pos {
                row: settled,
                col: 0
            })
            .is_none(),
        "a row past the boundary came back styled"
    );

    // Idle ticks on a grown, settled tail must not re-parse. Counted, not timed.
    ed.highlight_dirty = true;
    ed.tick(Instant::now());
    let before = ed.bs().hl.parses;
    for _ in 0..30 {
        ed.tick(Instant::now());
    }
    assert_eq!(
        ed.bs().hl.parses,
        before,
        "a settled tail re-parsed on idle frames"
    );
}

/// A plain open is not clamped: every row may be styled, no file is still writing
/// it.
#[test]
fn a_plain_open_styles_past_what_a_tail_would_settle() {
    let d = temp_dir("tail_not_clamped");
    let path = d.0.join("plain.rs");
    let text: String = (0..300).map(|i| format!("let x{i} = {i};\n")).collect();
    fs::write(&path, &text).unwrap();

    let mut ed = test_ed("");
    ed.text_h = 24;
    ed.start_load(&path).expect("open");
    settle(&mut ed);
    ed.bs_mut().scroll = crate::loader::TAIL_ROWS;
    ed.tick(Instant::now());

    assert_eq!(ed.bs().settled_last_row(), usize::MAX);
    let have = ed.bs().hl.styled_window().expect("a window");
    assert!(
        have.rows.1 > crate::loader::TAIL_ROWS,
        "a plain buffer was clamped to a tail's boundary: {:?}",
        have.rows
    );
}

/// A plain open has nothing to wait for: row 1 is line 1 from the first frame.
#[test]
fn a_plain_open_is_numbered_from_the_first_frame() {
    let (_d, path) = numbered_log("tail_plain_numbering", 5);
    let mut ed = test_ed("");
    ed.show_line_numbers = true;
    ed.text_w = 100;
    ed.text_h = 24;
    ed.start_load(&path).expect("open");
    settle(&mut ed);
    ed.tick(Instant::now());
    let screen = ui::Screen::of(&ed, 100, 30);
    let row = |y: u16| -> String { (0..100).map(|x| screen[(x, y)].symbol.clone()).collect() };
    assert!(row(1).starts_with(" 1 line 1"), "{:?}", row(1));
    assert!(row(27).contains("Ln 1, Col 1"), "{:?}", row(27));
}

/// The status line must not claim a line count it does not know.
#[test]
fn the_tail_says_what_it_is_instead_of_a_line_count() {
    let (_d, path) = numbered_log("tail_status", crate::loader::TAIL_ROWS + 300);
    let mut ed = test_ed("");
    ed.start_tail(&path).expect("tail open");
    settle(&mut ed);
    let said = ed.status_text().unwrap_or_default();
    assert!(said.contains("Tailing"), "{said:?}");
    assert!(
        !said.contains("Read"),
        "it claimed a line count it does not have: {said:?}"
    );

    // And a plain open still says what it always said.
    let (_d2, path2) = numbered_log("tail_status_plain", 3);
    let mut ed = test_ed("");
    ed.start_load(&path2).expect("open");
    settle(&mut ed);
    assert_eq!(ed.status_text().as_deref(), Some("Read 3 lines"));
}

/// A file shorter than a tail is shown whole, and an empty one is still a buffer.
#[test]
fn a_tail_of_a_short_or_empty_file_is_whole() {
    let (_d, path) = numbered_log("tail_short", 3);
    let mut ed = test_ed("");
    ed.start_tail(&path).expect("tail open");
    settle(&mut ed);
    assert_eq!(lines(&ed), vec!["line 1", "line 2", "line 3"]);
    assert!(ed.bs().tail.is_some(), "still a tail, just a short one");

    let d = temp_dir("tail_empty");
    let empty = d.0.join("empty.log");
    fs::write(&empty, "").unwrap();
    let mut ed = test_ed("");
    ed.start_tail(&empty).expect("tail open");
    settle(&mut ed);
    assert_eq!(lines(&ed), vec![""], "an empty tail is one empty row");
}

/// A tail keeps its own buffer's state: opening a second file plainly must not
/// leave the tail flag on it.
#[test]
fn a_plain_open_clears_the_tail_flag() {
    let (_d, path) = numbered_log("tail_clear", 5);
    let (_d2, other) = numbered_log("tail_clear2", 5);
    let mut ed = test_ed("");
    ed.start_tail(&path).expect("tail open");
    settle(&mut ed);
    assert!(ed.bs().tail.is_some());

    ed.start_load(&other).expect("open");
    settle(&mut ed);
    assert!(
        ed.bs().tail.is_none(),
        "a plain load inherited the tail flag"
    );
    assert!(
        !ed.bs().read_only,
        "read-only is per open, and this open did not ask for it"
    );
}

// ---------- a `.log` opens in log mode ----------

/// The rule is the name and nothing else: not a directory, not a content sniff.
#[test]
fn the_log_rule_is_the_name_and_nothing_else() {
    let ed = test_ed("");
    for (path, want) in [
        ("server.log", true),
        ("/var/log/syslog.log", true),
        ("/home/x/Projects/thing/out/serve.LOG", true),
        ("a.b.log", true),
        ("x.LoG", true),
        // Rotated logs are not `.log`: the extension is the whole rule.
        ("server.log.1", false),
        ("server.log.gz", false),
        ("log", false),
        ("logs", false),
        ("log.txt", false),
        ("src/log.rs", false),
        // A dotfile called `.log` is a dotfile, not a log.
        (".log", false),
        ("/", false),
    ] {
        assert_eq!(
            ed.log_mode_for(std::path::Path::new(path)),
            want,
            "{path:?} should {} be log mode",
            if want { "" } else { "not" }
        );
    }
    // And `--edit` turns every one of them off.
    let mut ed = test_ed("");
    ed.edit_override = true;
    assert!(!ed.log_mode_for(std::path::Path::new("server.log")));
}

/// One decision for every door: a `.log` tails, a `.rs` does not, and `--edit`
/// overrides.
#[test]
fn the_name_decides_as_a_log_or_as_a_file() {
    let d = temp_dir("log_named");
    let log = d.0.join("app.log");
    let rs = d.0.join("app.rs");
    let text: String = (1..=500).map(|i| format!("line {i}\n")).collect();
    fs::write(&log, &text).unwrap();
    fs::write(&rs, &text).unwrap();

    let mut ed = test_ed("");
    ed.start_named(&log).expect("open the log");
    settle(&mut ed);
    assert!(ed.bs().tail.is_some(), "a .log did not open as a tail");
    assert!(ed.bs().read_only, "a tail is a view (§20.6)");
    assert_eq!(lines(&ed).len(), crate::loader::TAIL_ROWS);
    assert_eq!(lines(&ed)[0], "line 301", "not the end of the file");

    let mut ed = test_ed("");
    ed.start_named(&rs).expect("open the source");
    settle(&mut ed);
    assert!(ed.bs().tail.is_none(), "a .rs opened as a tail");
    assert!(!ed.bs().read_only);
    assert_eq!(lines(&ed).len(), 500, "a plain open reads the whole file");

    // `--edit`: the same file, as a file.
    let mut ed = test_ed("");
    ed.edit_override = true;
    ed.start_named(&log).expect("open the log as a file");
    settle(&mut ed);
    assert!(ed.bs().tail.is_none());
    assert!(!ed.bs().read_only);
    assert_eq!(lines(&ed).len(), 500);
}

/// A `.log` opened later is not read eagerly either: `open_file` reads a file
/// whole, so a log is named and deferred instead.
#[test]
fn a_log_opened_later_is_deferred_and_then_opens_as_a_tail() {
    let d = temp_dir("log_open_file");
    let log = d.0.join("server.log");
    let rs = d.0.join("server.rs");
    let text: String = (1..=500).map(|i| format!("line {i}\n")).collect();
    fs::write(&log, &text).unwrap();
    fs::write(&rs, &text).unwrap();

    let mut ed = test_ed("");
    ed.text_h = 24;
    assert!(ed.open_file(log.to_str().unwrap()));
    assert!(
        ed.bs().pending_load.is_some(),
        "a .log was read eagerly by open_file"
    );
    assert_eq!(lines(&ed), vec![""], "nothing was read yet");
    ed.start_pending_load();
    settle(&mut ed);
    assert!(ed.bs().tail.is_some(), "it did not open as a tail");
    assert!(ed.bs().read_only);
    assert_eq!(lines(&ed)[0], "line 301");

    // The control: an ordinary file is still read here.
    let mut ed = test_ed("");
    assert!(ed.open_file(rs.to_str().unwrap()));
    assert!(ed.bs().pending_load.is_none());
    assert_eq!(lines(&ed).len(), 500, "a .rs is read on the spot");
}

// ---------- the mode, chosen rather than detected ----------

/// A chosen mode is used as it is, even where detection disagrees.
#[test]
fn a_chosen_mode_overrules_detection() {
    let d = temp_dir("mode_pick");
    let p = d.0.join("notes.txt");
    // No `=` and no `[section]`: the ladder must say nothing about this file.
    fs::write(&p, "fn main() { return 1; }\n").unwrap();
    let mut ed = test_ed("");
    ed.text_w = 80;
    ed.text_h = 24;
    ed.start_load(&p).expect("open");
    settle(&mut ed);
    ed.tick(Instant::now());
    // `.txt` detects as nothing, so nothing is coloured yet.
    let before = ed.bs().hl.style_at(Pos { row: 0, col: 0 });
    assert!(
        before.is_none() || before == Some(Style::new()),
        "{before:?}"
    );
    assert_eq!(ed.mode_name(), "plain", "a .txt is not a language");

    ed.do_setmode("rust");
    assert_eq!(
        ed.bs().hl.mode,
        crate::syntax::Mode::Lang(crate::syntax::Lang::Rust)
    );
    assert_eq!(ed.mode_name(), "rust");
    ed.tick(Instant::now());
    let after = ed.bs().hl.style_at(Pos { row: 0, col: 0 });
    assert!(
        after.is_some() && after != Some(Style::new()),
        "choosing rust coloured nothing: {after:?}"
    );
}

/// `plain` stops the guessing; `auto` puts the ladder back.
#[test]
fn plain_stops_the_guessing_and_auto_puts_it_back() {
    let mut ed = named_ed("fn main() {}\n", "/tmp/rano_mode_auto.rs");
    assert_eq!(ed.mode_name(), "rust", "detected from the name");
    ed.do_setmode("plain");
    assert_eq!(ed.bs().hl.mode, crate::syntax::Mode::Plain);
    assert_eq!(ed.mode_name(), "plain");
    ed.do_setmode("auto");
    assert_eq!(ed.bs().hl.mode, crate::syntax::Mode::Detected);
    assert_eq!(ed.mode_name(), "rust", "the ladder is back");
}

/// The prompt opens on the mode in effect, and an unknown name says so and changes
/// nothing.
#[test]
fn the_mode_prompt_opens_on_what_is_in_effect() {
    let mut ed = named_ed("fn main() {}\n", "/tmp/rano_mode_open.rs");
    ed.start_setmode();
    let p = ed.prompt.as_ref().expect("a prompt");
    assert_eq!(p.kind, PromptKind::SetMode);
    assert_eq!(p.text, "rust", "opens on the mode in effect");
    assert_eq!(p.cursor, 4, "with the cursor at its end");

    ed.do_setmode("nonsense");
    assert_eq!(
        ed.bs().hl.mode,
        crate::syntax::Mode::Detected,
        "an unknown name changed the mode"
    );
    let said = ed.status_text().unwrap_or_default();
    assert!(said.contains("No mode called"), "{said:?}");

    // A columnar format is a mode by its own name.
    ed.do_setmode("fstab");
    assert_eq!(
        ed.bs().hl.mode,
        crate::syntax::Mode::Lang(crate::syntax::Lang::Columns(
            crate::syntax::columns::Format::Fstab
        ))
    );
    assert_eq!(ed.mode_name(), "fstab");
}

/// A binary is not text: not read into rows, not highlighted, read-only.
#[test]
fn a_binary_opens_read_only_and_unread() {
    let d = temp_dir("binary_open");
    let p = d.0.join("thing.pyc");
    // A NUL in the first bytes: git's test, and the cheapest decisive one there is.
    fs::write(&p, b"\x0d\x0d\x0a\x00 not text at all \x00").unwrap();
    let mut ed = test_ed("");
    ed.text_w = 80;
    ed.text_h = 24;
    ed.start_load(&p).expect("open");
    settle(&mut ed);
    assert!(ed.bs().buf.binary, "the byte mark was not seen");
    assert!(ed.bs().read_only, "a binary is a view");
    assert_eq!(lines(&ed).len(), 1, "nothing was read into rows");
    assert!(!ed.loading(), "the loader is still running");

    // Refused at the keystroke, not at the save: there is no correct save to make.
    let before = ed.bs().buf.text();
    press(&mut ed, KeyCode::Char('x'), Mods::NONE);
    assert_eq!(ed.bs().buf.text(), before, "a binary accepted a keystroke");

    // And no grammar: nothing describes decoded noise.
    ed.tick(Instant::now());
    assert_eq!(ed.mode_name(), "plain", "a binary was given a language");
}

/// A builtin claims the file nobody configured it for; the door is asserted, not
/// the rendering, since a machine may have no `pdftoppm`.
#[test]
fn a_shipped_builtin_claims_a_pdf_from_the_command_line() {
    let d = temp_dir("builtin_pdf");
    let scripts = d.0.join("scripts");
    let pdf = d.0.join("paper.pdf");
    // A minimal one-page PDF: enough for the provider to claim, and nothing for it to render.
    fs::write(
        &pdf,
        b"%PDF-1.4\n1 0 obj<</Type/Catalog/Pages 2 0 R>>endobj\n2 0 obj<</Type/Pages/Kids[3 0 R]/Count 1>>endobj\n\
          3 0 obj<</Type/Page/Parent 2 0 R/MediaBox[0 0 200 200]>>endobj\ntrailer<</Root 1 0 R>>\n%%EOF\n",
    )
    .unwrap();
    let mut ed = test_ed("");
    ed.load_builtins_from(&scripts);
    assert!(
        !ed.builtins.is_empty(),
        "the shipped builtins did not load from {}",
        scripts.display()
    );
    // The binary mark is what a PDF hits without a provider: NUL bytes in its first 8 KiB.
    ed.open_file(&pdf.to_string_lossy());
    assert_eq!(
        ed.bs().buf.name.as_deref(),
        Some(pdf.as_path()),
        "the provider did not claim the pdf"
    );
    assert!(ed.bs().read_only, "a provider's view is read-only");
    assert!(
        !ed.bs().buf.binary,
        "a claimed file is not the binary fallback"
    );
}

/// M-P on a provider's answer asks the text, not only the name: a rendered PDF is
/// `spec.pdf` and opens with `# title`, which the ladder calls nothing.
#[test]
fn a_rendered_document_toggles_in_and_out_of_prose() {
    let d = temp_dir("pdf_toggle");
    let p = d.0.join("spec.pdf");
    let md = "# spec.pdf\n\n## Page 1\n\nprose, and a fence:\n\n```text\nlaid out\n```\n";
    // The provider's answer is the buffer; the view is drawn over the text.
    let mut ed = test_ed(md);
    ed.text_w = 80;
    ed.text_h = 24;
    ed.bs_mut().buf.name = Some(p);
    ed.preview_answered(md);
    assert!(ed.diff_view.is_some(), "the prose view is up");
    // Out through the view's own door, and back in through M-P.
    ed.close_view();
    assert!(ed.diff_view.is_none(), "the view closed");
    ed.toggle_rendered_view();
    assert!(
        ed.diff_view.is_some(),
        "M-P did not come back: {}",
        ed.status
            .as_ref()
            .map(|s| s.text.clone())
            .unwrap_or_default()
    );
}

/// A view is not a place with no way out: `C-x` must work inside a preview.
#[test]
fn the_exit_key_reaches_inside_a_view() {
    let md = "# title\n\n## Page 1\n\nprose\n";
    let mut ed = test_ed(md);
    ed.text_w = 80;
    ed.text_h = 24;
    ed.preview_answered(md);
    assert!(ed.diff_view.is_some(), "a view is up");

    press(&mut ed, KeyCode::Char('x'), Mods::CTRL);
    assert!(
        ed.wants_quit(),
        "C-x does nothing inside a view: {}",
        ed.status
            .as_ref()
            .map(|s| s.text.clone())
            .unwrap_or_default()
    );
}
