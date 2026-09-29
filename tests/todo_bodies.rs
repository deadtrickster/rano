//! Does the schema distinguish an item's BODY from its CHILDREN?
//!
//! The question is a correctness one, not a modelling preference. If the
//! ancestor pass can mistake a body paragraph for a child, a cascade marks the
//! wrong things. leticl found the same hazard from the other side: *"continuation
//! lines are indistinguishable from a child's at that level"*, and chose not to
//! join wrapped lines because doing so pasted 25 lines of prose under one item's
//! name.
//!
//! So this file asserts the distinction rather than observing that it works.

use rano::todo::{Doc, State, apply};

/// A body is prose under an item that is NOT a sub-task.
const WITH_BODY: &str = "\
# TODO

## Content

- [ ] Parent that needs explaining
  Why this matters: the body is the reasoning that makes the item actionable.
  It continues on a second line.
  - [x] real child one
  - [ ] real child two
- [ ] A plain task
";

/// **The distinction, asserted.** The body paragraph is not a child, and the
/// `[x]`/`[ ]` bullets are.
#[test]
fn a_body_paragraph_is_not_a_child() {
    let doc = Doc::parse(WITH_BODY);
    let names: Vec<&str> = doc
        .items()
        .iter()
        .map(|it| doc.src()[it.text.clone()].trim())
        .collect();
    assert_eq!(
        names,
        vec![
            "Parent that needs explaining",
            "real child one",
            "real child two",
            "A plain task"
        ],
        "the body must not appear as a task, and must not disturb the two children"
    );

    // The parent has exactly two children, not four.
    let parent = doc
        .items()
        .iter()
        .position(|it| doc.src()[it.text.clone()].contains("Parent"))
        .expect("the parent");
    let kids: Vec<usize> = (0..doc.items().len())
        .filter(|&i| doc.items()[i].parent == Some(parent))
        .collect();
    assert_eq!(kids.len(), 2, "two children: {kids:?}");
    for &k in &kids {
        assert!(
            doc.src()[doc.items()[k].text.clone()].contains("real child"),
            "child {k} is {:?}",
            &doc.src()[doc.items()[k].text.clone()]
        );
    }
}

/// **The item's extent includes its body**, which is what a copy needs — and the
/// parent's extent still stops before the next sibling.
#[test]
fn an_items_extent_includes_its_body() {
    let doc = Doc::parse(WITH_BODY);
    let parent = doc
        .items()
        .iter()
        .position(|it| doc.src()[it.text.clone()].contains("Parent"))
        .expect("the parent");
    let span = doc.src()[doc.items()[parent].item.clone()].to_string();
    assert!(
        span.contains("Why this matters"),
        "the body belongs to the item: {span:?}"
    );
    assert!(
        span.contains("real child one") && span.contains("real child two"),
        "and so do the children: {span:?}"
    );
    assert!(
        !span.contains("A plain task"),
        "but not the next sibling: {span:?}"
    );

    // `text` is the title only — the asymmetry is deliberate and is what a
    // content-matching caller needs.
    let title = doc.src()[doc.items()[parent].text.clone()].to_string();
    assert_eq!(title.trim(), "Parent that needs explaining");
    assert!(!title.contains("Why this matters"), "{title:?}");
}

/// **The cascade marks the children and nothing in the body**, even though the
/// body sits between the parent and the children — the shape that would break a
/// line-oriented implementation.
#[test]
fn a_cascade_does_not_mark_body_text() {
    let doc = Doc::parse(WITH_BODY);
    let parent = doc
        .items()
        .iter()
        .position(|it| doc.src()[it.text.clone()].contains("Parent"))
        .expect("the parent");
    let edits = doc.set_subtree(Some(parent), State::Done);
    for e in &edits {
        assert_eq!(e.range.len(), 3, "every edit is a marker: {e:?}");
        let marker = &doc.src()[e.range.clone()];
        assert!(
            marker.starts_with('[') && marker.ends_with(']'),
            "an edit landed on {marker:?}, which is not a task marker"
        );
    }
    let after = apply(WITH_BODY, &edits);
    assert_eq!(after.len(), WITH_BODY.len(), "same length");
    // The body is untouched, byte for byte.
    assert!(
        after.contains(
            "  Why this matters: the body is the reasoning that makes the item actionable.\n"
        ),
        "{after}"
    );
    // And every task under the parent is done.
    let d = Doc::parse(&after);
    let done = d
        .items()
        .iter()
        .filter(|it| d.src()[it.text.clone()].contains("real child"))
        .all(|it| it.state == State::Done);
    assert!(done, "the children are done");
    // The parent itself is done too, derived from them.
    let p = d
        .items()
        .iter()
        .position(|it| d.src()[it.text.clone()].contains("Parent"))
        .expect("the parent");
    assert_eq!(d.items()[p].state, State::Done, "up, written: {after}");
}

/// **The hazard, pinned**: a body line that LOOKS like a task. Markdown says an
/// indented `- [ ] x` at the child level IS a child, so this is not a bug in the
/// schema — but the behaviour has to be stated so nobody is surprised by it.
#[test]
fn a_body_line_that_looks_like_a_task_is_a_task() {
    let src = "\
# TODO

## S

- [ ] parent
  A body line:
  - [ ] this is a CHILD, not body text
";
    let doc = Doc::parse(src);
    let kids: Vec<usize> = (0..doc.items().len())
        .filter(|&i| doc.items()[i].parent == Some(0))
        .collect();
    assert_eq!(
        kids.len(),
        1,
        "an indented bullet is a child in markdown, whatever a reader intended"
    );
    assert!(doc.src()[doc.items()[kids[0]].text.clone()].contains("this is a CHILD"));

    // The way to keep such a line as BODY is to not make it a list item — an
    // indented continuation of the paragraph above, or a code span.
    let as_code = "# TODO\n\n## S\n\n- [ ] parent\n  A body line:\n\n      - [ ] this is code\n";
    let d = Doc::parse(as_code);
    let kids: Vec<usize> = (0..d.items().len())
        .filter(|&i| d.items()[i].parent == Some(0))
        .collect();
    assert!(
        kids.is_empty(),
        "an indented code block is body, not a child: {:?}",
        kids.iter()
            .map(|&i| d.src()[d.items()[i].text.clone()].to_string())
            .collect::<Vec<_>>()
    );
}

/// Wrapped prose is ONE body, not one item — the thing leticl refused to join.
#[test]
fn wrapped_prose_does_not_become_items() {
    let src = "\
# TODO

## S

- [ ] an item whose body wraps
  across several lines
  and keeps going
  without becoming tasks
- [ ] next
";
    let doc = Doc::parse(src);
    assert_eq!(
        doc.items().len(),
        2,
        "two tasks, five lines: {:?}",
        doc.items()
            .iter()
            .map(|it| doc.src()[it.text.clone()].trim())
            .collect::<Vec<_>>()
    );
    // The whole wrapped body is inside the first item's extent.
    let span = doc.src()[doc.items()[0].item.clone()].to_string();
    assert!(span.contains("across several lines"));
    assert!(span.contains("and keeps going"));
    assert!(!span.contains("next"), "{span:?}");
}

/// A whole-tree copy is the ruled operation, and it is byte-identical with
/// bodies present — the property `set_subtree(None, …)` has to have.
#[test]
fn a_whole_tree_copy_keeps_bodies() {
    let doc = Doc::parse(WITH_BODY);
    let edits = doc.set_subtree(None, State::Done);
    let after = apply(WITH_BODY, &edits);
    assert_eq!(after.len(), WITH_BODY.len());
    // Every non-marker byte is identical, bodies included.
    let allowed: Vec<std::ops::Range<usize>> = edits.iter().map(|e| e.range.clone()).collect();
    for (i, (a, b)) in WITH_BODY.bytes().zip(after.bytes()).enumerate() {
        if a != b {
            assert!(
                allowed.iter().any(|r| r.contains(&i)),
                "byte {i} changed outside a marker"
            );
        }
    }
    assert!(after.contains(
        "  Why this matters: the body is the reasoning that makes the item actionable.\n"
    ));
}
