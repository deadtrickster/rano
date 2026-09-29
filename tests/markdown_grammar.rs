//! What the markdown grammar gives us, pinned.
//!
//! Both `syntax.rs`'s highlight query and the todo schema (plans/S9) name
//! specific markdown nodes: `atx_heading`, `list_item`,
//! `task_list_marker_checked`, `task_list_marker_unchecked`. A grammar upgrade
//! that renamed or dropped one of those would not fail anything today — the
//! query would simply capture fewer things and the colour would go subtly
//! plain, which is the kind of break nobody reports. This makes it loud.
//!
//! It is also the evidence the todo design rests on, which is why it asserts
//! byte offsets and not just "something was captured": the marker's exact 3
//! bytes are what a toggle rewrites, and `[-]` being invisible is the finding
//! that makes a diagnostic necessary.
//!
//! Run with `cargo test --test markdown_grammar -- --nocapture` to see the tree.

use rano::syntax::{Lang, Node, Stream};

/// The names the schema and the highlighter both depend on.
const QUERY: &str = "\
(atx_heading) @heading
(list_item) @item
(task_list_marker_checked) @checked
(task_list_marker_unchecked) @unchecked
";

fn caps(src: &str) -> Vec<(String, usize, usize)> {
    let mut s = Stream::new(Lang::Markdown);
    s.push(src);
    s.captures(QUERY)
        .into_iter()
        .map(|c| (c.name, c.start, c.end))
        .collect()
}

/// The captured text of each capture, in document order.
fn texts(src: &str) -> Vec<(String, String)> {
    caps(src)
        .into_iter()
        .map(|(name, a, b)| (name, src[a..b].to_string()))
        .collect()
}

fn node_kinds(n: &Node, out: &mut Vec<String>) {
    out.push(n.kind.clone());
    for c in &n.children {
        node_kinds(c, out);
    }
}

fn tree(src: &str) -> Node {
    let mut s = Stream::new(Lang::Markdown);
    s.push(src);
    s.root().expect("a tree")
}

#[test]
fn the_marker_is_exactly_three_bytes_at_a_grammar_given_offset() {
    // The toggle rewrites exactly this range, so the range has to be exact.
    let src = "- [ ] open item\n";
    assert_eq!(
        caps(src),
        vec![("item".to_string(), 0, 16), ("unchecked".to_string(), 2, 5),],
        "the marker is 3 bytes at 2..5"
    );
    assert_eq!(&src[2..5], "[ ]");

    let src = "- [x] done\n";
    assert_eq!(
        caps(src),
        vec![("item".to_string(), 0, 11), ("checked".to_string(), 2, 5)]
    );
    assert_eq!(&src[2..5], "[x]");
}

/// `[X]` is a checkbox; a capital X is not a different state.
#[test]
fn a_capital_x_is_checked() {
    assert_eq!(
        texts("- [X] capital\n"),
        vec![
            ("item".to_string(), "- [X] capital\n".to_string()),
            ("checked".to_string(), "[X]".to_string()),
        ]
    );
}

/// All three bullet styles carry a task marker; the bullet's own node differs.
#[test]
fn every_bullet_style_can_hold_a_marker() {
    for (src, bullet) in [
        ("- [ ] a\n", "list_marker_minus"),
        ("* [ ] a\n", "list_marker_star"),
        ("+ [ ] a\n", "list_marker_plus"),
        ("1. [ ] a\n", "list_marker_dot"),
    ] {
        let mut kinds = Vec::new();
        node_kinds(&tree(src), &mut kinds);
        assert!(
            kinds.iter().any(|k| k == "task_list_marker_unchecked"),
            "{src:?}: no unchecked marker; kinds = {kinds:?}"
        );
        assert!(
            kinds.iter().any(|k| k == bullet),
            "{src:?}: no {bullet}; kinds = {kinds:?}"
        );
    }
}

/// **The finding.** A marker the grammar does not know is not an error and is
/// not captured — it is an ordinary list item whose paragraph happens to begin
/// with brackets. Nothing in the tree reports it.
///
/// This is why the schema is not just a query, and why `check()` has to exist:
/// `- [-]` and `- [~]` and `- []` would all render as plain rows with no
/// symptom anywhere. It is also why a query cannot express org's third state:
/// there is no node to capture.
#[test]
fn an_unknown_marker_is_silent_in_the_grammar() {
    for src in ["- [-] partial\n", "- [~] weird\n", "- [] no space\n"] {
        let found = caps(src);
        assert_eq!(
            found.len(),
            1,
            "{src:?}: expected only the list item, got {found:?}"
        );
        assert_eq!(found[0].0, "item", "{src:?}");
        let root = tree(src);
        assert!(
            !root.has_error,
            "{src:?}: the grammar reported an error, which would make the \
             schema's diagnostic unnecessary — this test's premise changed"
        );
    }

    // And the specific shape of `[-]`: the paragraph starts exactly where a
    // marker would have been, which is the offset a schema would read.
    let src = "- [-] partial\n";
    let root = tree(src);
    let item = root
        .children
        .iter()
        .flat_map(|c| &c.children)
        .flat_map(|c| &c.children)
        .find(|n| n.kind == "list_item")
        .expect("a list_item");
    let para = item
        .children
        .iter()
        .find(|n| n.kind == "paragraph")
        .expect("a paragraph");
    assert_eq!(
        para.start, 2,
        "a marker would have started here, so this is where to look for one"
    );
    assert_eq!(&src[para.start..para.start + 3], "[-]");
}

/// A `section` covers a heading **and everything under it, including nested
/// sections** — which is what makes fold and subtree move free rather than a
/// scan for where a heading's body ends.
#[test]
fn a_section_is_the_whole_subtree() {
    let src = "# Top\n\ntext\n\n## Mid\n\n- [ ] x\n\n### Deep\n\n- [x] y\n\n## Other\n\nz\n";
    let root = tree(src);
    // Find the section whose heading is "## Mid", and check it contains the
    // "### Deep" section and stops before "## Other".
    let sections: Vec<&Node> = {
        let mut v = Vec::new();
        fn go<'a>(n: &'a Node, v: &mut Vec<&'a Node>) {
            if n.kind == "section" {
                v.push(n);
            }
            for c in &n.children {
                go(c, v);
            }
        }
        go(&root, &mut v);
        v
    };
    assert!(
        sections.len() >= 3,
        "expected nested sections, got {}",
        sections.len()
    );
    let mid = src.find("## Mid").expect("the Mid heading");
    let deep = src.find("### Deep").expect("the Deep heading");
    let other = src.find("## Other").expect("the Other heading");
    let mid_section = sections.iter().find(|s| s.start == mid).unwrap_or_else(|| {
        panic!("no section starting at the Mid heading; sections = {sections:?}")
    });
    assert!(
        mid_section.start <= mid && mid_section.end > deep,
        "Mid's section must contain Deep: {mid_section:?}"
    );
    assert!(
        mid_section.end <= other,
        "Mid's section must stop at Other: {} vs {other}",
        mid_section.end
    );
}

/// The heading's level is readable from its marker node, not from counting
/// characters — so `#`, `##` and `######` are the grammar's business.
#[test]
fn a_heading_carries_its_level_marker() {
    for (src, marker) in [
        ("# one\n", "atx_h1_marker"),
        ("## two\n", "atx_h2_marker"),
        ("###### six\n", "atx_h6_marker"),
    ] {
        let mut kinds = Vec::new();
        node_kinds(&tree(src), &mut kinds);
        assert!(
            kinds.iter().any(|k| k == marker),
            "{src:?}: no {marker}; kinds = {kinds:?}"
        );
    }
}
