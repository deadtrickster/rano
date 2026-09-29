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

/// Every marker node's byte range, in document order.
fn markers(src: &str) -> Vec<(String, std::ops::Range<usize>)> {
    caps(src)
        .into_iter()
        .filter(|(name, _, _)| name.starts_with("checked") || name.starts_with("unchecked"))
        .map(|(name, a, b)| (name, a..b))
        .collect()
}

/// Every `list_item` node, depth first.
fn list_items(n: &Node, out: &mut Vec<Node>) {
    if n.kind == "list_item" {
        out.push(n.clone());
    }
    for c in &n.children {
        list_items(c, out);
    }
}

/// Every `section` node, depth first.
fn sections_of(n: &Node, out: &mut Vec<Node>) {
    if n.kind == "section" {
        out.push(n.clone());
    }
    for c in &n.children {
        sections_of(c, out);
    }
}

/// Mark every marker inside `span` done, and return the new text plus the
/// ranges that were actually REWRITTEN. This is the whole cascade, and it is
/// deliberately this small: it is a cell edit per marker, not a re-render.
///
/// Only the markers whose text is not already the target are returned. That is
/// not an optimisation — it is what idempotence means. A cascade over an
/// already-done subtree must produce NO edits, or the undo stack fills with
/// steps that change nothing and the document gets rewritten by a byte-identical
/// save.
fn cascade(src: &str, span: &std::ops::Range<usize>) -> (String, Vec<std::ops::Range<usize>>) {
    let inside: Vec<_> = markers(src)
        .into_iter()
        .map(|(_, r)| r)
        .filter(|r| r.start >= span.start && r.end <= span.end)
        .filter(|r| &src[r.clone()] != "[x]")
        .collect();
    let mut out = src.to_string();
    for r in &inside {
        out.replace_range(r.clone(), "[x]");
    }
    (out, inside)
}

/// Assert the file after equals the file before except for bytes inside
/// `allowed`. Every state is three bytes, so a cascade is a same-length
/// substitution and this can be checked per byte.
fn assert_only_these_bytes_changed(before: &str, after: &str, allowed: &[std::ops::Range<usize>]) {
    assert_eq!(
        before.len(),
        after.len(),
        "a cascade must not change the document length"
    );
    for (i, (a, b)) in before.bytes().zip(after.bytes()).enumerate() {
        if a != b {
            assert!(
                allowed.iter().any(|r| r.contains(&i)),
                "byte {i} changed but is outside every range we meant to change: {:?}",
                &before[i.saturating_sub(12)..(i + 12).min(before.len())]
            );
        }
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

/// **The cascade property, and the reason this is byte-identical by
/// construction.** Marking a subtree done rewrites each descendant marker's own
/// three bytes and NOTHING else — every marker is inside a range the tree handed
/// us, so there is no re-render and nothing to lose.
///
/// This is what leticl had to hand-build source-line tracking to approximate.
#[test]
fn a_cascade_changes_only_marker_bytes() {
    let src = "\
# Top

## Now

- [ ] parent
  - [ ] child one
  - [x] child two

### Deep

- [ ] deeper

## Other

- [ ] untouched
";
    let root = tree(src);
    let mut items = Vec::new();
    list_items(&root, &mut items);
    let parent_start = src.find("- [ ] parent").expect("the parent item");
    let parent = items
        .iter()
        .find(|n| n.start == parent_start)
        .expect("the parent list_item");

    // **The containment the cascade rides on**, asserted rather than trusted:
    // the nested items fall inside the parent's range, the deeper heading's and
    // the next section's do not. And child two is already `[x]`, so it is NOT in
    // the edit set — a cascade reports the markers it will rewrite, which is why
    // the count here is 2 and not 3.
    let span = parent.start..parent.end;
    let (after, inside) = cascade(src, &span);
    let all = markers(src);
    let outside: Vec<_> = all
        .iter()
        .filter(|(_, r)| !(r.start >= span.start && r.end <= span.end))
        .collect();
    assert_eq!(
        inside.len(),
        2,
        "parent and child one need changing; child two does not, got {inside:?}"
    );
    assert!(
        inside.iter().all(|r| &src[r.clone()] != "[x]"),
        "a done marker must not be in the edit set: {inside:?}"
    );
    assert_eq!(
        outside.len(),
        2,
        "deeper and untouched are not, got {outside:?}"
    );
    assert_eq!(
        after,
        "\
# Top

## Now

- [x] parent
  - [x] child one
  - [x] child two

### Deep

- [ ] deeper

## Other

- [ ] untouched
"
    );
    assert_only_these_bytes_changed(src, &after, &inside);
}

/// Doing it twice is doing it once. This is the property that makes it safe to
/// run on a file somebody else may also be editing — and the one leticl called
/// out as the reason for line surgery in the first place.
#[test]
fn a_cascade_is_idempotent() {
    let src = "- [ ] a\n  - [x] b\n  - [ ] c\n";
    let span = 0..src.len();
    let (once, _) = cascade(src, &span);
    assert_eq!(once, "- [x] a\n  - [x] b\n  - [x] c\n");
    // Re-parse the RESULT: a second cascade starts from the new document.
    let (twice, second) = cascade(&once, &span);
    assert_eq!(once, twice, "a second cascade must be a no-op");
    assert_only_these_bytes_changed(&once, &twice, &second);
    assert!(second.is_empty(), "nothing left to change, got {second:?}");
}

/// A `section` is a cascade target too, so "done for the whole subtree" works
/// over headings — which is how a checklist written as sections is marked.
#[test]
fn a_section_is_a_cascade_target_too() {
    let src = "\
## Now

- [ ] one
- [x] two

## Other

- [ ] three
";
    let root = tree(src);
    let mut secs = Vec::new();
    sections_of(&root, &mut secs);
    let now = src.find("## Now").expect("the Now heading");
    let sec = secs
        .iter()
        .find(|s| s.start == now)
        .expect("a section starting at Now");
    let (after, inside) = cascade(src, &(sec.start..sec.end));
    assert_eq!(
        inside.len(),
        1,
        "only `one` needs changing under Now; two is already done, got {inside:?}"
    );
    assert_eq!(
        after,
        "\
## Now

- [x] one
- [x] two

## Other

- [ ] three
"
    );
    assert_only_these_bytes_changed(src, &after, &inside);
}

/// The measurement the derived-progress (`up`) decision rests on: what does it
/// cost to have a CURRENT tree and markers, for a realistic document?
///
/// Ignored because it is a measurement, not an assertion. Run with
/// `cargo test --release --test markdown_grammar -- --ignored --nocapture
/// measure_the_cost`.
#[test]
#[ignore = "measurement; run with --release --ignored --nocapture"]
fn measure_the_cost_of_a_current_tree() {
    use std::time::Instant;

    let build = |items: usize| {
        let mut s = String::new();
        for i in 0..items {
            if i % 25 == 0 {
                s.push_str(&format!("\n## Section {}\n\n", i / 25));
            }
            let indent = "  ".repeat(i % 3);
            let state = if i % 4 == 0 { "x" } else { " " };
            s.push_str(&format!(
                "{indent}- [{state}] item {i} with a little prose, to be a real line\n"
            ));
        }
        s
    };

    for items in [200usize, 1_000, 4_000] {
        let doc = build(items);
        let rounds = 20;
        let t = Instant::now();
        let mut caps_n = 0;
        for _ in 0..rounds {
            let mut s = Stream::new(Lang::Markdown);
            s.push(&doc);
            caps_n = s.captures(QUERY).len();
        }
        let each = t.elapsed().as_secs_f64() / rounds as f64;
        println!(
            "{items:>5} items, {:>7} bytes: parse+query {:>8.3} ms  ({} captures)",
            doc.len(),
            each * 1e3,
            caps_n
        );
    }
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
