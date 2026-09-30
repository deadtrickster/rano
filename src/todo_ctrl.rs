//! TODO.md in the editor: reading the buffer as a todo file, changing a task's
//! state, cascading over a subtree, and reporting what the format cannot express.
//!
//! The schema itself is [`crate::todo`] — a library module that reads byte ranges
//! and returns edits, and never writes a document. This is the binary-side half:
//! it maps the editor's rows and columns onto those byte ranges, applies the
//! edits through `Buffer` so they land in the undo stack, and feeds `check()` to
//! the gutter that already renders diagnostics.
//!
//! # Why the byte mapping is done by hand
//!
//! [`crate::todo`] speaks **byte offsets into `Buffer::text()`**; the editor
//! speaks `(row, char col)`. The two are not the same number and the difference
//! is not a formality: a task line may contain multibyte text before its marker
//! (`- [ ] 日本語`), so a byte offset is not a char index. [`row_chars`] is the one
//! conversion, and it is exact rather than assumed.
//!
//! The conversion is safe to do once, before any edit is applied, because every
//! edit the schema produces is **three ASCII bytes replaced by three**, so no
//! edit moves another one's offsets and no row changes length.
//!
//! # Recognition
//!
//! A buffer is a *todo file* when its name is `TODO.md`, case-insensitively —
//! that is what the standard is about, and it is what gates `check()` reporting
//! unknown markers. **Toggling does not require it**: any row with a task marker
//! toggles, in any markdown file, because being able to tick a box in a scratch
//! note is useful and costs nothing.

use crate::buffer::Buffer;
use crate::editor::{ActionKind, Editor};
// The LIBRARY's schema, not a second copy compiled into the binary. `src/todo.rs`
// is declared once, in `lib.rs`, so there is one implementation of the format and
// leticl sees exactly what the editor uses. Only `&str`, ranges and `String` cross
// the boundary, so the binary's `Buffer` and the library's stay independent — as
// they already were for every other module.
use rano::todo::{self, State};

/// The byte range of `text()` that row `r` occupies, and how many bytes it is.
///
/// `text()` joins rows with `\n` and appends a final `\n` when the last row is
/// non-empty, so a row's start is the sum of every earlier row's bytes plus one
/// separator each. Computed rather than cached because it is only ever needed on
/// a keypress, never per frame.
fn row_byte_range(buf: &Buffer, r: usize) -> Option<(usize, usize)> {
    if r >= buf.row_count() {
        return None;
    }
    let mut at = 0usize;
    for i in 0..r {
        at += buf.row(i).iter().map(|c| c.len_utf8()).sum::<usize>() + 1;
    }
    let len: usize = buf.row(r).iter().map(|c| c.len_utf8()).sum();
    Some((at, at + len))
}

/// A byte range that lies inside ONE row, as `(row, char_start, char_end)`.
///
/// `None` when the range straddles a row boundary, which for this module is a
/// bug rather than a case: every range it converts is a three-byte marker.
fn row_chars(buf: &Buffer, range: &std::ops::Range<usize>) -> Option<(usize, usize, usize)> {
    for r in 0..buf.row_count() {
        let (start, end) = row_byte_range(buf, r)?;
        if range.start >= start && range.end <= end {
            // Byte offset within the row -> char index, by walking the row. A
            // task line can carry multibyte text before its marker, so this is
            // not the identity and must not be treated as one.
            let to_char = |off: usize| -> usize {
                let mut bytes = 0usize;
                let mut chars = 0usize;
                for ch in buf.row(r) {
                    if bytes >= off - start {
                        break;
                    }
                    bytes += ch.len_utf8();
                    chars += 1;
                }
                chars
            };
            return Some((r, to_char(range.start), to_char(range.end)));
        }
    }
    None
}

/// The state the cursor is inside, and its index, if the cursor's row is a task.
///
/// `row` rather than a position: a task is a line, and being anywhere on that
/// line means the key acts on it. That is what makes the binding usable without
/// aiming at the checkbox.
fn task_at(doc: &todo::Doc, row: usize) -> Option<usize> {
    doc.items().iter().position(|it| it.line == row)
}

impl Editor {
    /// This buffer as a TODO.md.
    ///
    /// Parsed on demand rather than cached: `Doc::parse` is 2.1 ms on a 200-item
    /// file (measured), which is imperceptible for a keypress and would be a
    /// liability per frame. If it ever needs to be per-frame, cache it against
    /// `edit_gen` — the way the wrap table is — and not before.
    pub(crate) fn todo_doc(&self) -> todo::Doc {
        todo::Doc::parse(&self.bs().buf.text())
    }

    /// Whether this buffer is a TODO.md by name.
    pub(crate) fn is_todo_buffer(&self) -> bool {
        self.bs()
            .buf
            .name
            .as_ref()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.eq_ignore_ascii_case("todo.md"))
    }

    /// Replace the buffer text inside `range` with `replacement`, as chars.
    ///
    /// The caller has already converted the byte range to a row and char range.
    /// Assumes `replacement` is the same char count as the range — true for every
    /// marker, and asserted in the tests rather than hoped for.
    fn todo_replace(buf: &mut Buffer, row: usize, chars: std::ops::Range<usize>, text: &str) {
        buf.row_mut(row)
            .splice(chars, text.chars().collect::<Vec<_>>());
    }

    /// Apply the schema's edits, as ONE undo step.
    ///
    /// Descending by offset, so if a future edit is not length-preserving the
    /// earlier ones cannot have moved the later ones' positions. Today every edit
    /// is three bytes for three, so the order does not matter — which is exactly
    /// why it is not relied on.
    fn apply_todo_edits(&mut self, edits: &[todo::Edit], label: &str) -> usize {
        let mut located: Vec<(usize, std::ops::Range<usize>, String)> = Vec::new();
        {
            let buf = &self.bs().buf;
            for e in edits {
                match row_chars(buf, &e.range) {
                    Some((row, a, b)) => located.push((row, a..b, e.replacement.clone())),
                    None => {
                        // A range that is not inside one row means the offsets do
                        // not match this buffer — a stale parse, not a bad edit.
                        // Refusing to write is the only safe answer.
                        self.flash("Todo edit does not line up with this buffer");
                        return 0;
                    }
                }
            }
        }
        if located.is_empty() {
            return 0;
        }
        located.sort_by_key(|(_, chars, _)| std::cmp::Reverse(chars.start));

        let first = located.iter().map(|(r, _, _)| *r).min().unwrap_or(0);
        let last = located.iter().map(|(r, _, _)| *r).max().unwrap_or(0);
        self.begin_action(ActionKind::Todo, first, last + 1);
        for (row, chars, text) in &located {
            let bs = self.bs_mut();
            Self::todo_replace(&mut bs.buf, *row, chars.clone(), text);
        }
        self.finish_step();
        // Every row's content changed but none its count, and a marker is the
        // same width before and after — so the wrap table is still right. The
        // multi-row invalidate is for the diagnostics and the highlight, which
        // do need to know the document changed.
        self.edit_invalidate();
        self.flash(label);
        located.len()
    }

    /// M-T: toggle the task on the cursor's row between done and not done.
    ///
    /// Ancestors follow — that is [`todo::Doc::derive_up`], and it is the half of
    /// "auto done for the whole subtree" that goes UP: completing the last
    /// sub-task completes the parent, because the FILE is the transport and a
    /// parent left `[ ]` above finished children is the file disagreeing with
    /// itself.
    ///
    /// Children are **not** touched. Marking a parent is not a claim about its
    /// children, and a toggle that quietly rewrote a whole subtree would be a
    /// surprise; that is what M-C is for.
    pub(crate) fn todo_toggle(&mut self) {
        let row = self.bs().cursor.row;
        let doc = self.todo_doc();
        let Some(i) = task_at(&doc, row) else {
            self.flash("Not a task on this line");
            return;
        };
        let from = doc.items()[i].state;
        let to = match from {
            State::Done => State::Open,
            // Declined is not a resting state: any other press means "not done
            // any more", and Open is what that is. Reaching Declined is M-X.
            State::Open | State::Declined => State::Done,
        };
        let edits = doc.set_subtree(Some(i), to);
        let label = state_word(to).to_string();
        self.apply_todo_edits(&edits, &label);
    }

    /// M-X: toggle the task on the cursor's row between declined and not.
    ///
    /// `[-]` is the standard's third state, and it means **declined** — not "in
    /// progress", which is the reading it usually gets. Keeping it on its own key
    /// means the common press never passes through it by accident.
    pub(crate) fn todo_decline(&mut self) {
        let row = self.bs().cursor.row;
        let doc = self.todo_doc();
        let Some(i) = task_at(&doc, row) else {
            self.flash("Not a task on this line");
            return;
        };
        let to = if doc.items()[i].state == State::Declined {
            State::Open
        } else {
            State::Declined
        };
        let edits = doc.set_subtree(Some(i), to);
        self.apply_todo_edits(&edits, state_word(to));
    }

    /// M-C: **the whole unit the cursor is in**, as one command.
    ///
    /// On a heading row it targets that heading's whole `section` — every task
    /// under it, subsections included. On a task row it targets that task's
    /// subtree. Either way every task inside gets the same state, and the
    /// ancestors are derived afterwards.
    ///
    /// The direction comes from what is there: a unit that is not already all
    /// done is marked done, one that is all done is cleared. One key both
    /// completes a section and takes it back.
    ///
    /// This is the key for the shape these files actually have. rano's own
    /// `TODO.md` is 49 headings over 67 flat items — the hierarchy is the
    /// HEADINGS, and no single item owns it.
    pub(crate) fn todo_cascade(&mut self) {
        let row = self.bs().cursor.row;
        let doc = self.todo_doc();
        let (span, what) = match doc.section_at(row) {
            Some(section) => (section, "section"),
            None => match task_at(&doc, row) {
                Some(i) => (doc.items()[i].item.clone(), "task"),
                None => {
                    self.flash("Not a task or a heading on this line");
                    return;
                }
            },
        };
        let inside: Vec<&todo::Item> = doc
            .items()
            .iter()
            .filter(|it| it.item.start >= span.start && it.item.end <= span.end)
            .collect();
        if inside.is_empty() {
            self.flash("Nothing to mark here");
            return;
        }
        let all_done = inside.iter().all(|it| it.state == State::Done);
        let to = if all_done { State::Open } else { State::Done };
        let edits = doc.set_span(span, to);
        // "Done 2 tasks in this section", not "Done 2 sections" — the count is
        // of TASKS, and reading it as a count of sections is a lie the wording
        // made easy.
        let scope = if what == "section" {
            " in this section"
        } else {
            ""
        };
        let label = format!(
            "{} {} task{}{}",
            state_word(to),
            inside.len(),
            plural(inside.len()),
            scope
        );
        self.apply_todo_edits(&edits, &label);
    }

    /// The task diagnostics for this buffer: its `check()` output, in the same
    /// tuple shape a tree-sitter error arrives in, so the gutter that already
    /// renders those renders these.
    ///
    /// Empty unless the buffer is a todo file BY NAME. A `[-]` in an ordinary
    /// markdown file is a legitimate thing to write (Obsidian uses it for
    /// cancelled), and reporting it there would be the editor inventing a rule
    /// for somebody else's file.
    pub(crate) fn todo_check(&self) -> Vec<(usize, usize, usize, String)> {
        if !self.is_todo_buffer() {
            return Vec::new();
        }
        self.todo_doc().check()
    }
}

/// The word for a state, for the status line.
fn state_word(s: State) -> &'static str {
    match s {
        State::Open => "Open",
        State::Done => "Done",
        State::Declined => "Declined",
    }
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

/// A `Pos` for a byte offset — the inverse of what `row_chars` does, for a
/// caller that wants to move the cursor to a place the schema named.
///
/// `cfg(test)`: the mapping is the one thing in this file that could be silently
/// wrong, so it is asserted directly (see the tests), and nothing in the editor
/// needs to convert this way yet — a byte offset from the schema is only ever
/// *edited*, never navigated to.
#[cfg(test)]
pub(crate) fn pos_of_byte(buf: &Buffer, off: usize) -> Option<crate::buffer::Pos> {
    for r in 0..buf.row_count() {
        let (start, end) = row_byte_range(buf, r)?;
        if off >= start && off <= end {
            let mut bytes = 0usize;
            let mut chars = 0usize;
            for ch in buf.row(r) {
                if bytes >= off - start {
                    break;
                }
                bytes += ch.len_utf8();
                chars += 1;
            }
            return Some(crate::buffer::Pos { row: r, col: chars });
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::buffer::{Buffer, Pos};
    use crate::config;
    use std::path::PathBuf;

    fn ed_named(name: &str, text: &str) -> Editor {
        let mut buf = Buffer::new();
        buf.name = Some(PathBuf::from(name));
        buf.set_rows(text.lines().map(|l| l.chars().collect()).collect());
        if buf.rows_is_empty() {
            buf.push_row(Vec::new());
        }
        Editor::new(buf, config::Config::default())
    }

    fn ed(text: &str) -> Editor {
        ed_named("TODO.md", text)
    }

    fn rows(ed: &Editor) -> Vec<String> {
        ed.bs().buf.rows().map(|l| l.iter().collect()).collect()
    }

    fn at(ed: &mut Editor, row: usize) {
        ed.bs_mut().cursor = Pos { row, col: 0 };
    }

    const DOC: &str = "\
# TODO

Prose that belongs to nobody.

## Now

- [ ] one
- [x] two
- [ ] parent
  - [ ] child a
  - [ ] child b

## Later

- [ ] three
";

    /// **The byte-to-row mapping, which is the one thing here that could be
    /// silently wrong.** `todo` speaks byte offsets into `text()`; the editor
    /// speaks rows and char columns, and they differ the moment a line holds a
    /// multibyte character.
    #[test]
    fn byte_offsets_map_to_char_columns_through_multibyte_text() {
        let ed = ed("- [ ] 日本語 tail\n");
        let buf = &ed.bs().buf;
        // The marker is ASCII and sits at bytes 2..5 == chars 2..5.
        assert_eq!(pos_of_byte(buf, 2), Some(Pos { row: 0, col: 2 }));
        assert_eq!(pos_of_byte(buf, 5), Some(Pos { row: 0, col: 5 }));
        // After the three CJK characters (3 bytes each) the numbers diverge:
        // byte 6 is the first 日, byte 9 is the space after it.
        assert_eq!(
            pos_of_byte(buf, 9),
            Some(Pos { row: 0, col: 7 }),
            "3 bytes in, 1 char in — the mapping must fold, not add"
        );
    }

    /// The same mapping across rows, where each row costs its bytes plus one for
    /// the `\n` the join inserts.
    #[test]
    fn byte_offsets_map_across_rows() {
        let ed = ed("ab\n- [ ] x\n");
        let buf = &ed.bs().buf;
        // Row 0 is 2 bytes + 1 separator, so row 1 starts at byte 3.
        assert_eq!(pos_of_byte(buf, 0), Some(Pos { row: 0, col: 0 }));
        assert_eq!(pos_of_byte(buf, 3), Some(Pos { row: 1, col: 0 }));
        assert_eq!(pos_of_byte(buf, 5), Some(Pos { row: 1, col: 2 }));
    }

    /// The headline action: one press marks the line's task done.
    #[test]
    fn a_toggle_marks_the_task_on_the_cursor_line() {
        let mut ed = ed(DOC);
        at(&mut ed, 6); // "- [ ] one"
        ed.todo_toggle();
        let r = rows(&ed);
        assert_eq!(r[6], "- [x] one");
        assert_eq!(r[7], "- [x] two", "the neighbouring task is untouched");
        assert!(ed.status_text().unwrap_or_default().contains("Done"));
    }

    /// It is one undo step, and undo puts every marker back.
    #[test]
    fn a_toggle_is_one_undo_step() {
        let mut ed = ed(DOC);
        at(&mut ed, 6);
        let before = rows(&ed);
        ed.todo_toggle();
        assert_ne!(rows(&ed), before);
        ed.undo();
        assert_eq!(rows(&ed), before, "one undo restores it exactly");
    }

    /// **Up: completing the last child completes the parent**, because the file
    /// leaves `[ ]` above finished children otherwise.
    #[test]
    fn completing_the_last_child_completes_the_parent() {
        let mut ed = ed(DOC);
        at(&mut ed, 9); // child a
        ed.todo_toggle();
        assert_eq!(rows(&ed)[8], "- [ ] parent", "one child left, so not done");
        at(&mut ed, 10); // child b — the last one
        ed.todo_toggle();
        let r = rows(&ed);
        assert_eq!(r[8], "- [x] parent", "the parent follows its last child");
        assert_eq!(r[9], "  - [x] child a");
        assert_eq!(r[10], "  - [x] child b");
    }

    /// **Up goes both ways**: un-completing a child un-completes the parent, so
    /// the file cannot say `[x]` above an open child.
    #[test]
    fn unchecking_a_child_unchecks_the_parent() {
        let mut ed = ed(DOC);
        at(&mut ed, 9);
        ed.todo_toggle();
        at(&mut ed, 10);
        ed.todo_toggle();
        assert_eq!(rows(&ed)[8], "- [x] parent");
        at(&mut ed, 9);
        ed.todo_toggle(); // child a back to open
        let r = rows(&ed);
        assert_eq!(r[8], "- [ ] parent", "the parent is open again");
        assert_eq!(r[9], "  - [ ] child a");
    }

    /// **Down, as one command**: M-C marks the whole subtree, and leaves
    /// everything outside it alone.
    #[test]
    fn a_cascade_marks_the_subtree_only() {
        let mut ed = ed(DOC);
        at(&mut ed, 8); // "parent"
        ed.todo_cascade();
        let r = rows(&ed);
        assert_eq!(r[8], "- [x] parent");
        assert_eq!(r[9], "  - [x] child a");
        assert_eq!(r[10], "  - [x] child b");
        assert_eq!(r[6], "- [ ] one", "the previous section is untouched");
        assert_eq!(r[14], "- [ ] three", "and so is a later one");
        assert!(
            ed.status_text().unwrap_or_default().contains("3 task"),
            "the status names what it did: {:?}",
            ed.status_text()
        );
    }

    /// A cascade is one undo step, however many markers it rewrote — which is
    /// what the whole subtree of edits being one `begin_action` buys.
    #[test]
    fn a_cascade_is_one_undo_step() {
        let mut ed = ed(DOC);
        at(&mut ed, 8);
        let before = rows(&ed);
        ed.todo_cascade();
        assert_ne!(rows(&ed), before);
        ed.undo();
        assert_eq!(rows(&ed), before);
    }

    /// The same key takes a completed subtree back, so one binding covers both
    /// directions.
    #[test]
    fn a_cascade_clears_a_subtree_that_is_all_done() {
        let mut ed = ed(DOC);
        at(&mut ed, 8);
        ed.todo_cascade();
        assert_eq!(rows(&ed)[8], "- [x] parent");
        ed.todo_cascade();
        let r = rows(&ed);
        assert_eq!(r[8], "- [ ] parent");
        assert_eq!(r[9], "  - [ ] child a");
        assert!(ed.status_text().unwrap_or_default().contains("Open"));
    }

    /// Declined is its own key, and it is a toggle in its own right — so the
    /// common press never passes through it.
    #[test]
    fn decline_toggles_the_third_state() {
        let mut ed = ed(DOC);
        at(&mut ed, 6);
        ed.todo_decline();
        assert_eq!(rows(&ed)[6], "- [-] one");
        assert!(ed.status_text().unwrap_or_default().contains("Declined"));
        ed.todo_decline();
        assert_eq!(rows(&ed)[6], "- [ ] one", "and back");
    }

    /// **The byte-identity property, end to end through the editor**: the
    /// document after equals the one before except for the marker, so the prose,
    /// the metadata tails and the blank lines are untouched.
    #[test]
    fn a_toggle_touches_nothing_but_markers() {
        let src = "\
# TODO

A description  with two spaces.   

## Now

- [ ] Task with  ~3d #feat @john 2020-03-20  and  double  spaces  
- [ ] parent
  - [ ] child
";
        // A LEAF task: exactly one marker moves, so the assertion is arithmetic.
        // (`leaf`, not `ed`: a local called `ed` would shadow the helper below.)
        let mut leaf = ed(src);
        let before = leaf.bs().buf.text();
        at(&mut leaf, 6);
        leaf.todo_toggle();
        let after = leaf.bs().buf.text();
        assert_eq!(after.len(), before.len(), "length unchanged");
        let differing: Vec<usize> = before
            .bytes()
            .zip(after.bytes())
            .enumerate()
            .filter(|(_, (a, b))| a != b)
            .map(|(i, _)| i)
            .collect();
        assert_eq!(
            differing.len(),
            1,
            "`[ ]` -> `[x]` moves exactly the middle byte: {differing:?}"
        );
        assert!(after.contains("- [x] Task with  ~3d"));
        // The awkward prose is verbatim.
        assert!(after.contains("A description  with two spaces.   "));
        assert!(after.contains("  ~3d #feat @john 2020-03-20  and  double  spaces  "));

        // Now the PARENT, which has one child. `derive_up` forbids `[x]` above an
        // open child, so ticking it necessarily carries the child — two markers,
        // two bytes, and still nothing else. That is the rule doing its job, and
        // it is asserted rather than left as a surprise.
        let mut parented = ed(src);
        at(&mut parented, 7);
        parented.todo_toggle();
        let after2 = parented.bs().buf.text();
        let differing2: Vec<usize> = before
            .bytes()
            .zip(after2.bytes())
            .enumerate()
            .filter(|(_, (a, b))| a != b)
            .map(|(i, _)| i)
            .collect();
        assert_eq!(
            differing2.len(),
            2,
            "parent and child both moved: {differing2:?}"
        );
        assert!(after2.contains("- [x] parent"));
        assert!(after2.contains("  - [x] child"));
        // Two bytes, one per marker, and no prose touched.
        for i in &differing2 {
            let b = after2.as_bytes()[*i];
            assert!(
                b == b'x' || b == b' ',
                "byte {i} is {b:?}, which is not inside a marker"
            );
        }
    }

    /// **M-C on a HEADING marks its whole section** — the shape these files
    /// actually have, where the hierarchy is the headings and no single item owns
    /// it. This is the operator's ask read at file scale: rano's own `TODO.md` is
    /// 49 headings over 67 flat items.
    #[test]
    fn a_cascade_on_a_heading_marks_its_whole_section() {
        let mut ed = ed(DOC);
        at(&mut ed, 4); // "## Now"
        ed.todo_cascade();
        let r = rows(&ed);
        assert_eq!(r[6], "- [x] one", "flat items under the heading");
        assert_eq!(r[7], "- [x] two");
        assert_eq!(r[8], "- [x] parent");
        assert_eq!(r[9], "  - [x] child a", "and nested ones");
        assert_eq!(r[10], "  - [x] child b");
        assert_eq!(
            r[14], "- [ ] three",
            "the next section is a different unit and stays alone"
        );
        let st = ed.status_text().unwrap_or_default();
        assert!(
            st.contains("5 tasks in this section"),
            "the status names what it did and on how many: {st:?}"
        );
    }

    /// A section cascade covers SUBSECTIONS too — `###` inside `##` is still
    /// under it, which is what a `section` range means.
    #[test]
    fn a_section_cascade_reaches_subsections() {
        let src = "\
# TODO

## Top

- [ ] a

### Inner

- [ ] b

## Other

- [ ] c
";
        let mut ed = ed(src);
        at(&mut ed, 2); // "## Top"
        ed.todo_cascade();
        let r = rows(&ed);
        assert_eq!(r[4], "- [x] a");
        assert_eq!(r[8], "- [x] b", "a subsection is still inside Top");
        assert_eq!(r[12], "- [ ] c", "but Other is not");
    }

    /// M-C takes a section back too, so one key covers both directions.
    #[test]
    fn a_cascade_on_a_heading_clears_an_all_done_section() {
        let mut ed = ed(DOC);
        at(&mut ed, 4);
        ed.todo_cascade();
        assert_eq!(rows(&ed)[6], "- [x] one");
        ed.todo_cascade();
        let r = rows(&ed);
        assert_eq!(r[6], "- [ ] one");
        assert_eq!(r[8], "- [ ] parent");
        assert_eq!(r[9], "  - [ ] child a");
    }

    /// A line that is not a task says so rather than doing something surprising.
    #[test]
    fn a_non_task_line_flashes_and_changes_nothing() {
        let mut ed = ed(DOC);
        let before = ed.bs().buf.text();
        at(&mut ed, 2); // prose
        ed.todo_toggle();
        assert_eq!(ed.bs().buf.text(), before, "nothing written");
        assert!(
            ed.status_text().unwrap_or_default().contains("Not a task"),
            "and it says why"
        );
    }

    /// **`check()` is reported only for a TODO.md.** `[-]` in an ordinary
    /// markdown file is a legitimate thing to write — Obsidian uses it for
    /// cancelled — and the editor must not invent a rule for somebody else's
    /// file.
    #[test]
    fn an_unknown_marker_is_reported_only_in_a_todo_file() {
        let src = "# TODO\n\n## S\n\n- [ ] fine\n- [/] in progress\n";
        let todos = ed_named("TODO.md", src);
        let found = todos.todo_check();
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].3.contains("unknown task marker"), "{found:?}");

        let plain = ed_named("notes.md", src);
        assert!(plain.todo_check().is_empty(), "not our file, not our rule");
        // Case does not matter: the standard's file is `TODO.md`.
        let lower = ed_named("todo.md", src);
        assert_eq!(lower.todo_check().len(), 1);
    }

    /// The toggle works in ANY markdown file — being able to tick a box in a
    /// scratch note is useful and costs nothing — even though `check()` is
    /// scoped to TODO.md.
    #[test]
    fn toggling_works_outside_a_todo_file_too() {
        let mut ed = ed_named("notes.md", "# Notes\n\n- [ ] a thought\n");
        at(&mut ed, 2);
        ed.todo_toggle();
        assert_eq!(rows(&ed)[2], "- [x] a thought");
    }

    /// Every state change lands as exactly three characters, so a row's width
    /// and the document's length are unchanged — which is what lets the
    /// conversion be computed once, before any edit is applied.
    #[test]
    fn every_marker_is_three_characters_before_and_after() {
        for from in ["- [ ] t\n", "- [x] t\n", "- [-] t\n", "- [X] t\n"] {
            for to in [State::Open, State::Declined, State::Done] {
                let mut ed = ed(&format!("# TODO\n\n## S\n\n{from}"));
                let doc = ed.todo_doc();
                let edits = doc.set_subtree(Some(0), to);
                for e in &edits {
                    assert_eq!(e.range.len(), 3, "{from:?} -> {to:?}: {e:?}");
                    assert_eq!(e.replacement.chars().count(), 3, "{e:?}");
                }
                at(&mut ed, 4);
                ed.todo_decline();
                let len = ed.bs().buf.text().len();
                assert_eq!(
                    len,
                    format!("# TODO\n\n## S\n\n{from}").len(),
                    "{from:?} changed the document length"
                );
            }
        }
    }

    /// A CRLF buffer parses and toggles the same way: `text()` joins with `\n`
    /// regardless, so the offsets the schema works in are unaffected — and this
    /// pins that, because it is the kind of thing that would silently shift every
    /// offset by the number of preceding rows.
    #[test]
    fn a_crlf_buffer_toggles_by_the_same_offsets() {
        let mut ed = ed("# TODO\r\n\r\n- [ ] a\r\n- [ ] b\r\n");
        // `from_text` strips the \r, so the rows hold no carriage returns.
        assert!(ed.bs().buf.crlf || !ed.bs().buf.text().contains('\r'));
        at(&mut ed, 2);
        ed.todo_toggle();
        assert_eq!(rows(&ed)[2], "- [x] a");
        assert_eq!(rows(&ed)[3], "- [ ] b");
    }
}
