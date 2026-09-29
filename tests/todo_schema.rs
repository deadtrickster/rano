//! The TODO.md schema, against the standard.
//!
//! Written as an INTEGRATION test on purpose: this is the path a library caller
//! takes (`rano::todo`), so a pass is also evidence that the schema is reachable
//! from leticl rather than only from the binary — which is the requirement the
//! whole feature rests on.
//!
//! The first test is deliberately not "the toggle worked". It is *"the file
//! after equals the file before except for the bytes I meant to change"*, and
//! every other test here keeps that property or chooses a different answer
//! explicitly.

use rano::todo::{Doc, Edit, State, apply};

/// A realistic TODO.md: the standard's own example, plus the two shapes that
/// matter — a nested sub-task list, and a metadata tail.
const SRC: &str = "\
# TODO

This is the markdown todo file for project a.

## Content

Tasks related to new content.

- [ ] Add readme file with newline #example
- [ ] Create Pull Request
- [ ] Parent task ~3d #feat @john 2020-03-20
  - [ ] sub one
  - [x] sub two

## Release

- [x] Init project repository http://github.com/todo-md/todo-md
- [ ] Publish project on GitHub @janikvonrotz

## DONE

- [x] Create GitHub organization todo-md
";

fn index_of(doc: &Doc, needle: &str) -> usize {
    doc.items()
        .iter()
        .position(|it| doc.src()[it.text.clone()].contains(needle))
        .unwrap_or_else(|| panic!("no item containing {needle:?}"))
}

/// Assert the document did not change length, and that **every** byte that
/// differs lies inside one of `allowed`.
///
/// Note the asymmetry with `Edit`: an edit's RANGE is always three bytes,
/// because a marker is three bytes — but the DIFF need not be. `[ ]` → `[x]`
/// changes one byte, because only the middle one moves. So the property to
/// assert is containment, not equality, and the equality version of this test
/// was wrong when it was written.
fn assert_changed_only_within(before: &str, after: &str, allowed: &[std::ops::Range<usize>]) {
    assert_eq!(
        before.len(),
        after.len(),
        "a state change must not change the document's length"
    );
    for (i, (a, b)) in before.bytes().zip(after.bytes()).enumerate() {
        if a != b {
            assert!(
                allowed.iter().any(|r| r.contains(&i)),
                "byte {i} changed but is outside every range we meant to change: {:?}",
                &before[i.saturating_sub(14)..(i + 14).min(before.len())]
            );
        }
    }
}

/// **The hard requirement.** The file after equals the file before except for
/// the bytes inside the marker that was meant to change.
#[test]
fn a_toggle_changes_exactly_the_marker_bytes() {
    let doc = Doc::parse(SRC);
    let i = index_of(&doc, "Create Pull Request");
    let marker = doc.items()[i].marker.clone();

    let edits = doc.set_subtree(Some(i), State::Done);
    assert_eq!(edits.len(), 1, "one task, one edit: {edits:?}");
    assert_eq!(edits[0].range, marker);

    let after = apply(SRC, &edits);
    assert_changed_only_within(SRC, &after, std::slice::from_ref(&marker));
    assert_eq!(&after[marker.start..marker.end], "[x]");
}

/// The same property under a cascade over a subtree: every changed byte is
/// inside one of the markers the rule named, and nothing else moved.
#[test]
fn a_cascade_changes_only_marker_bytes() {
    let doc = Doc::parse(SRC);
    let p = index_of(&doc, "Parent task");

    let edits = doc.set_subtree(Some(p), State::Done);
    for e in &edits {
        assert_eq!(e.range.len(), 3, "every edit is a three-byte marker: {e:?}");
    }
    let allowed: Vec<std::ops::Range<usize>> = edits.iter().map(|e| e.range.clone()).collect();

    let after = apply(SRC, &edits);
    assert_changed_only_within(SRC, &after, &allowed);

    // The subtree is done, and the two sub-tasks with it.
    let after_doc = Doc::parse(&after);
    for needle in ["Parent task", "sub one", "sub two"] {
        let i = index_of(&after_doc, needle);
        assert_eq!(after_doc.items()[i].state, State::Done, "{needle}");
    }
    // And the rest of the document is untouched.
    let q = index_of(&after_doc, "Add readme");
    assert_eq!(after_doc.items()[q].state, State::Open);
    assert!(after.contains("Publish project on GitHub @janikvonrotz"));
}

/// Running the same cascade twice does nothing the second time — no edits at
/// all, which is what keeps the undo stack honest and a save byte-identical.
#[test]
fn a_cascade_is_idempotent() {
    let doc = Doc::parse(SRC);
    let p = index_of(&doc, "Parent task");
    let once = apply(SRC, &doc.set_subtree(Some(p), State::Done));

    let doc2 = Doc::parse(&once);
    let p2 = index_of(&doc2, "Parent task");
    let second = doc2.set_subtree(Some(p2), State::Done);
    assert!(
        second.is_empty(),
        "a second cascade must produce no edits, got {second:?}"
    );
    assert_eq!(apply(&once, &second), once);
}

/// **Up, written.** A parent whose sub-tasks are all done becomes done, using
/// only the standard's own markers so a dumb renderer sees it.
#[test]
fn a_parent_is_derived_from_its_children() {
    let src = "\
# TODO

## S

- [ ] parent
  - [x] one
  - [x] two
";
    let doc = Doc::parse(src);
    let p = index_of(&doc, "parent");
    assert_eq!(doc.items()[p].state, State::Open, "as written in the file");

    let edits = doc.derive_up();
    assert_eq!(edits.len(), 1, "only the parent: {edits:?}");
    let marker = edits[0].range.clone();
    let after = apply(src, &edits);
    assert_changed_only_within(src, &after, std::slice::from_ref(&marker));
    assert_eq!(&after[marker.start..marker.end], "[x]");
    let after_doc = Doc::parse(&after);
    let p = index_of(&after_doc, "parent");
    assert_eq!(after_doc.items()[p].state, State::Done);
}

/// An ancestor already correct yields **no** edit. Without this, every child
/// toggle would dirty the whole path to the root.
#[test]
fn an_ancestor_already_correct_produces_no_edit() {
    let src = "\
# TODO

## S

- [x] parent
  - [x] one
  - [x] two
";
    let doc = Doc::parse(src);
    assert!(
        doc.derive_up().is_empty(),
        "nothing to derive: the parent already agrees with its children"
    );
}

/// Marking one child leaves the parent open — a parent with mixed children is
/// open, and the file must not say `[x]` above an unchecked child.
#[test]
fn a_partly_done_parent_is_open_not_done() {
    let src = "\
# TODO

## S

- [ ] parent
  - [x] one
  - [ ] two
";
    let doc = Doc::parse(src);
    assert!(doc.derive_up().is_empty(), "parent is already open");

    // Now complete it: the parent follows.
    let one = index_of(&doc, "two");
    let edits = doc.set_subtree(Some(one), State::Done);
    let after = apply(src, &edits);
    let after_doc = Doc::parse(&after);
    let p = index_of(&after_doc, "parent");
    assert_eq!(
        after_doc.items()[p].state,
        State::Done,
        "the parent follows its last child: {after}"
    );
}

/// All three states parse, including the one the grammar has no node for.
#[test]
fn all_three_states_parse() {
    let src = "# TODO\n\n## S\n\n- [ ] open\n- [-] declined\n- [x] done\n";
    let doc = Doc::parse(src);
    assert_eq!(doc.items().len(), 3, "{:?}", doc.items());
    assert_eq!(doc.items()[0].state, State::Open);
    assert_eq!(
        doc.items()[1].state,
        State::Declined,
        "`[-]` has no grammar node and must still be named"
    );
    assert_eq!(doc.items()[2].state, State::Done);
    // And `[-]` is declined, NOT in progress: it is its own marker, and the
    // round trip writes it back.
    assert!(doc.check().is_empty(), "declined is a defined state");
    let i = index_of(&doc, "open");
    let after = apply(src, &doc.set_subtree(Some(i), State::Declined));
    assert!(after.contains("- [-] declined") && after.contains("- [-] open"));
}

/// Every ordered pair of state changes, each exactly three bytes.
#[test]
fn every_state_change_is_three_bytes() {
    let src = "# TODO\n\n## S\n\n- [ ] task\n";
    for from in State::ALL {
        let start = apply(src, &Doc::parse(src).set_subtree(None, from));
        let marker_at = start
            .find("[ ]")
            .or_else(|| start.find("[-]"))
            .or_else(|| start.find("[x]"));
        let at = marker_at.expect("a marker");
        for to in State::ALL {
            let doc = Doc::parse(&start);
            let edits = doc.set_subtree(None, to);
            let after = apply(&start, &edits);
            assert_eq!(
                &after[at..at + 3],
                to.marker(),
                "{from:?} -> {to:?} did not land"
            );
            // Containment, not equality: some transitions move one byte and
            // some move two (see `assert_changed_only_within`).
            let only_marker = at..at + 3;
            assert_changed_only_within(&start, &after, std::slice::from_ref(&only_marker));
        }
    }
}

/// **A marker the standard does not define is refused by name**, not treated as
/// a plain row. `[-]` names declined; `[/]` names nothing, and the grammar is
/// silent about both.
#[test]
fn an_unknown_marker_is_refused_by_name() {
    let src = "# TODO\n\n## S\n\n- [ ] fine\n- [/] in progress\n- [~] odd\n";
    let doc = Doc::parse(src);
    // `[/]` and `[~]` are not tasks at all: the standard defines three markers
    // and neither is one, so they do not appear as items.
    let names: Vec<String> = doc
        .items()
        .iter()
        .map(|it| doc.src()[it.text.clone()].trim().to_string())
        .collect();
    assert_eq!(names, vec!["fine"], "{names:?}");

    // `check` is what names them. The grammar reports nothing here (no ERROR
    // node, has_error false), so this is the only place they can be surfaced.
    let found = doc.check();
    assert_eq!(found.len(), 2, "{found:?}");
    assert!(
        found
            .iter()
            .all(|(_, _, _, m)| m.contains("unknown task marker"))
    );
    assert!(
        found.iter().any(|(_, _, _, m)| m.contains("[/]")),
        "the offending marker is named: {found:?}"
    );
    let (line, col, end, _) = &found[0];
    assert_eq!(
        (*line, *col, *end),
        (5, 2, 5),
        "line 5 (`- [/] ...`), cols 2..5"
    );
}

/// The metadata tail and awkward prose come out byte-identical — which is
/// leticl's actual complaint, and the reason there is no re-render.
#[test]
fn prose_and_metadata_survive_untouched() {
    let src = "\
# TODO

A description  with two spaces and a trailing space.   

## Section

- [ ] Task with  ~3d #feat @john 2020-03-20  and  double  spaces  
- [x] Task with a trailing space 
- [ ] Task with [JIRA-345] a ticket id
- [ ] Task with a `code span` and a **strong** bit
";
    let doc = Doc::parse(src);
    assert_eq!(doc.items().len(), 4, "{:?}", doc.items());

    // Complete the first, and check that EVERY other byte is untouched.
    let i = index_of(&doc, "~3d");
    let marker = doc.items()[i].marker.clone();
    let after = apply(src, &doc.set_subtree(Some(i), State::Done));
    assert_changed_only_within(src, &after, std::slice::from_ref(&marker));

    // Stated positively, on the awkward parts. The toggled line is now `[x]`,
    // so its tail is asserted without the marker prefix — the point is that the
    // tail itself is untouched.
    assert!(after.contains("A description  with two spaces and a trailing space.   "));
    assert!(
        after.contains("Task with  ~3d #feat @john 2020-03-20  and  double  spaces  "),
        "the metadata tail survives: {after}"
    );
    assert!(after.contains("- [x] Task with  ~3d"), "the toggled line");
    assert!(after.contains("- [x] Task with a trailing space "));
    assert!(after.contains("- [ ] Task with [JIRA-345] a ticket id"));
    assert!(after.contains("- [ ] Task with a `code span` and a **strong** bit"));
}

/// An `id`-shaped tail is neither parsed nor disturbed, so a future `id` or
/// `dependsOn` field can be added without this module having to change.
#[test]
fn an_id_in_the_tail_is_left_alone() {
    let src = "# TODO\n\n## S\n\n- [ ] task id:abc123 dependsOn:def456\n";
    let doc = Doc::parse(src);
    assert_eq!(doc.items().len(), 1);
    let i = index_of(&doc, "task");
    let after = apply(src, &doc.set_subtree(Some(i), State::Done));
    assert!(
        after.contains("id:abc123 dependsOn:def456"),
        "the tail is not ours to touch: {after}"
    );
}

/// Sections are the standard's grouping, and a task belongs to the innermost
/// one that contains it.
#[test]
fn sections_group_tasks() {
    let src = "\
# TODO

## One

- [ ] a
- [x] b

## Two

- [ ] c

### Two-and-a-half

- [ ] d
";
    let doc = Doc::parse(src);
    let titles: Vec<&str> = doc
        .headings()
        .iter()
        .map(|h| doc.src()[h.title.clone()].trim())
        .collect();
    assert!(titles.contains(&"One"), "{titles:?}");
    assert!(titles.contains(&"Two"), "{titles:?}");
    assert!(titles.contains(&"Two-and-a-half"), "{titles:?}");

    let one = doc
        .headings()
        .iter()
        .position(|h| doc.src()[h.title.clone()].trim() == "One")
        .expect("the One section");
    assert_eq!(doc.progress(one), (1, 2), "one of two done under One");
    assert_eq!(doc.items_in(one).len(), 2);

    let two = doc
        .headings()
        .iter()
        .position(|h| doc.src()[h.title.clone()].trim() == "Two")
        .expect("the Two section");
    assert_eq!(
        doc.items_in(two).len(),
        1,
        "`d` belongs to the nested section, not to Two"
    );
}

/// An empty edit list leaves the document identical — the trivial case of the
/// byte-identity rule, and the one a save path hits on every no-op frame.
#[test]
fn no_edits_is_no_change() {
    assert_eq!(apply(SRC, &[]), SRC);
    let edits: Vec<Edit> = Vec::new();
    assert_eq!(apply(SRC, &edits), SRC);

    // And the real case that produces an empty list: a document whose parents
    // already agree with their children has nothing to derive, so a load-time
    // roll-up pass is free on it.
    let doc = Doc::parse(SRC);
    let derived = doc.derive_up();
    assert!(
        derived.is_empty(),
        "nothing to derive in this file: {derived:?}"
    );
    assert_eq!(apply(SRC, &derived), SRC);
}
