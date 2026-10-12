//! **The sweep that found the stale-range panic**, kept so a future slice site cannot
//! reintroduce it.
//!
//! A generated corpus of fences, quotes, lists, tables and multi-byte characters, each
//! document put through a whole `lex` and through `IncrementalMarkdown` in six delta sizes —
//! 279 841 documents x 7 paths. It found the panic when nothing else did, and it is the only
//! thing that says every slice of `src` by a tree offset goes through the guard: the unit test
//! in `markdown::parse` pins the 19-byte repro, and this pins the *class*.
//!
//! `#[ignore]`d: ~15 s, which is a tool run after touching a `slice` site and not a gate.
//!
//!     cargo test --test stale_range_sweep -- --ignored --nocapture
use rano::markdown::parse::{IncrementalMarkdown, lex};

const TOKENS: &[&str] = &[
    "```\n\u{2026}\u{2026}\u{2026}\n```\n",
    "```\nunclosed \u{2026}\n",
    "```\u{2026}\n",
    "- \u{2026}\n  \u{2026}\n",
    "> ```\n> \u{2026}\n> ```\n",
    "- item \u{2026}\n  continued \u{2026}\n",
    "*a* [b](c) `d`\n",
    "\u{2026}\u{2026}\u{2026}\n",
    "\n",
    "| a | \u{2026} |\n|---|---|\n| \u{2026} | b |\n",
    "1. \u{2026}\n2. \u{2026}\n",
    "> - \u{2026}\n>   \u{2026}\n",
    "# h \u{2026}\n",
    "  \u{2026}\n",
    "```c\n\u{2026} a\n```\n",
    "\u{65e5}\u{672c}\u{8a9e} \u{1f642}\n",
    "text \u{2026} with `code` and *em*\n",
    "a\n\u{2026}\n",
    "~~~\n\u{2026}\n~~~\n",
    "    indented \u{2026} code\n",
    "> \u{2026}\n> \u{2026}\n",
    "- \u{2026}\n\n- \u{2026}\n",
    "***\n",
    // **The shape the shrinker found**, which the token combinations below cannot build:
    // a list item, an opening fence, and ellipses with no closing fence. A sweep is only as
    // good as its tokens, and the 19-byte repro was outside the original set.
    "- \u{2026}\n```\n\u{2026}\u{2026}\u{2026}",
    "- \u{2026}\n```\n\u{2026}\u{2026}\u{2026}\n",
    "```\n\u{2026}\u{2026}\u{2026}",
    "- \u{2026}\n```\u{2026}\n",
];

fn doc_for(mut i: usize) -> String {
    let mut out = String::new();
    for _ in 0..3 {
        out.push_str(TOKENS[i % TOKENS.len()]);
        i /= TOKENS.len();
    }
    out
}

fn deltas(doc: &str, k: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut start = 0usize;
    let mut n = 0usize;
    for (i, _) in doc.char_indices() {
        if n == k {
            out.push(doc[start..i].to_string());
            start = i;
            n = 0;
        }
        n += 1;
    }
    if start < doc.len() {
        out.push(doc[start..].to_string());
    }
    out
}

#[test]
#[ignore]
fn no_slice_site_panics_on_the_corpus() {
    std::panic::set_hook(Box::new(|_| {}));
    let total = TOKENS.len().pow(3);
    let mut bad = 0usize;
    for i in 0..total {
        let doc = doc_for(i);
        let d = doc.clone();
        if std::panic::catch_unwind(move || {
            let _ = lex(&d);
        })
        .is_err()
        {
            println!("PANIC lex on {doc:?}");
            bad += 1;
            if bad > 4 {
                break;
            }
            continue;
        }
        for k in [1usize, 2, 3, 5, 11, 40] {
            let parts = deltas(&doc, k);
            if std::panic::catch_unwind(move || {
                let mut m = IncrementalMarkdown::new();
                for p in &parts {
                    m.push(p);
                }
                let _ = m.blocks().count();
            })
            .is_err()
            {
                println!("PANIC push k={k} on {doc:?}");
                bad += 1;
            }
        }
        if bad > 4 {
            break;
        }
    }
    let _ = std::panic::take_hook();
    println!("SWEEP: {total} documents x 7 paths, {bad} panics");
    assert_eq!(bad, 0, "{bad} panics on the corpus");
}
