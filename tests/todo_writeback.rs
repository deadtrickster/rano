//! The non-interactive path: what a second caller — harnessd completing a task,
//! or the model doing it — actually has to work with.
//!
//! The operator's write-back ruling means the surgical toggle gains a caller that
//! is not a keystroke: nobody has a cursor, a viewport or an open buffer, and the
//! row it wants to complete arrives as a wire `TodoEntry { content, status }`.
//! This file measures what that costs, against the real module, so the answer is
//! a number rather than an assurance.

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

/// **No interactive state is needed to drive this.** One `&str` in, one `String`
/// out; no cursor, no buffer, no terminal. This is the property the whole
/// embed-the-schema decision rests on, so it is asserted rather than assumed.
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

/// What a caller holding `TodoEntry { content }` is actually up against: the
/// wire's `content` is a *trimmed title*, and the file's text is not.
///
/// This is the measurement behind the gap below — it is not a guess about what
/// the text looks like.
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

    // **A parent's text is its own title, not its subtree.** This was wrong in
    // the first version: `text` ran to the item's end, which for a parent
    // includes every sub-task, so the title came out as a blob containing the
    // children. Measured here rather than assumed.
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

/// So the gap, stated as a failing lookup: there is no supported way to go from
/// the content a non-interactive caller holds to the item it means.
///
/// This test documents the gap rather than asserting a behaviour the module has.
/// It will be replaced by a positive test when a lookup exists.
#[test]
fn a_caller_holding_content_has_no_lookup() {
    let doc = Doc::parse(FILE);
    let wire_content = "Create Pull Request";

    // What a caller can do today: linear search on its own, comparing against
    // the raw range with all the caveats above. Something like this — written
    // HERE, in the test, because the module does not offer it.
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

    // And it is ambiguous when two tasks share a title, which is the case a
    // naive implementation gets wrong by taking the first.
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

/// The operation a non-interactive caller needs — complete the task with this
/// content — works end to end **by index found however the caller likes**, and
/// is still three bytes per changed marker.
///
/// The point is that nothing about the *operation* needs interactivity; only
/// addressing does.
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
