//! The non-interactive path: what a second caller — a harness completing a task, or the model
//! doing it — has to work with. Nobody has a cursor, a viewport or an open buffer, and the row it
//! wants arrives as a wire `TodoEntry { content, status }`.

use rano::todo::{Doc, State, apply};

const FILE: &str = "\
# TODO

A description line that is not a task.

## Content

- [ ] Add readme file with newline #example
- [x] Create Pull Request
- [ ] Parent ~3d #feat @john 2020-03-20
  - [ ] sub one
  - [x] sub two

## Release

- [ ] Publish project on GitHub @janikvonrotz
";

/// No interactive state is needed to drive this: one `&str` in, one `String` out — the property
/// the embed-the-schema decision rests on.
#[test]
fn driving_it_needs_no_cursor_or_buffer() {
    // Nothing but the file's bytes.
    let doc = Doc::parse(FILE);
    let edits = doc.set_subtree(None, State::Done);
    let after = apply(FILE, &edits);

    // Every task is done, and the document is the same length.
    assert_eq!(after.len(), FILE.len());
    let done = Doc::parse(&after);
    assert!(
        done.items().iter().all(|i| i.state == State::Done),
        "{:?}",
        done.items().iter().map(|i| i.state).collect::<Vec<_>>()
    );
}

/// What a caller holding `TodoEntry { content }` is up against: the wire's `content` is a trimmed
/// title, and the file's text is not — measured here, not guessed.
#[test]
fn the_file_text_is_not_the_wire_content() {
    let doc = Doc::parse(FILE);
    let raw: Vec<&str> = doc
        .items()
        .iter()
        .map(|it| &doc.src()[it.text.clone()])
        .collect();

    // The raw text carries a leading space and the whole metadata tail, neither
    // of which a wire `content` has.
    assert_eq!(raw[0], " Add readme file with newline #example");
    assert_eq!(raw[2], " Parent ~3d #feat @john 2020-03-20");

    // **A parent's text is its own title, not its subtree** — the ITEM range
    // covers the children, which is what a cascade needs.
    assert!(
        !raw[2].contains("sub one"),
        "a parent's text must not contain its children: {raw:?}"
    );
    assert!(
        doc.src()[doc.items()[2].item.clone()].contains("sub one"),
        "but the ITEM range does cover them, which is what a cascade needs"
    );

    // A wire-style content is the trimmed title with the tail stripped.
    let wire_content = "Add readme file with newline";
    assert!(
        raw[0].trim() != wire_content,
        "trim() alone is NOT enough — the `#example` tag is still there"
    );
    assert!(
        raw[0].contains(wire_content),
        "but it is a prefix, so matching is possible with the tail split off"
    );
}

/// The gap, stated as a failing lookup: no supported way to go from the content a
/// non-interactive caller holds to the item it means. Documents the gap rather than asserting a
/// behaviour; replaced by a positive test when a lookup exists.
#[test]
fn a_caller_holding_content_has_no_lookup() {
    let doc = Doc::parse(FILE);
    let wire_content = "Create Pull Request";

    // What a caller can do today: linear search on its own, written HERE because
    // the module does not offer it.
    let found: Vec<usize> = doc
        .items()
        .iter()
        .enumerate()
        .filter(|(_, it)| doc.src()[it.text.clone()].contains(wire_content))
        .map(|(i, _)| i)
        .collect();
    assert_eq!(
        found,
        vec![1],
        "a linear search works, but it is the caller's"
    );

    // And it is ambiguous when two tasks share a title — the case a naive
    // implementation gets wrong by taking the first.
    let dup = "# TODO\n\n## S\n\n- [ ] same title\n- [ ] same title\n";
    let doc = Doc::parse(dup);
    let hits: Vec<usize> = doc
        .items()
        .iter()
        .enumerate()
        .filter(|(_, it)| doc.src()[it.text.clone()].contains("same title"))
        .map(|(i, _)| i)
        .collect();
    assert_eq!(
        hits,
        vec![0, 1],
        "two tasks with one title — the caller must decide, and the module \
         cannot know which the model meant"
    );
}

/// The operation a non-interactive caller needs — complete the task with this content — works end
/// to end by an index found however the caller likes, and is still three bytes per changed marker.
/// Nothing about the operation needs interactivity; only addressing does.
#[test]
fn completing_by_content_is_surgical() {
    let doc = Doc::parse(FILE);
    // Address by content, the way a wire row would arrive.
    let i = doc
        .items()
        .iter()
        .position(|it| doc.src()[it.text.clone()].contains("Publish project"))
        .expect("the task");
    let marker = doc.items()[i].marker.clone();

    let edits = doc.set_subtree(Some(i), State::Done);
    let allowed: Vec<std::ops::Range<usize>> = edits.iter().map(|e| e.range.clone()).collect();
    let after = apply(FILE, &edits);

    assert_eq!(after.len(), FILE.len(), "same length");
    for (b, (x, y)) in FILE.bytes().zip(after.bytes()).enumerate() {
        if x != y {
            assert!(
                allowed.iter().any(|r| r.contains(&b)),
                "byte {b} changed outside the markers"
            );
        }
    }
    assert_eq!(&after[marker.start..marker.end], "[x]");
    // And the file's prose is untouched, which is the whole reason for surgery.
    assert!(after.contains("A description line that is not a task."));
    assert!(after.contains("- [ ] Parent ~3d #feat @john 2020-03-20"));
}
