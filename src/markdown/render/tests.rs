//! letibot's painter tests, ported.
//!
//! letibot asserted on escape bytes (`\x1b[1m` for bold, `\x1b[36m` for code)
//! because escape bytes were its output. Here the output says what each span
//! means, so the same claims are made about text and roles: "nothing came out
//! bold" is "no span is bold", and "the keyword is coloured" is "a span carries
//! [`Role::Keyword`]". Where a test's point was the visible text — a table's
//! columns, a list's indent — it asserts on the plain text exactly as before.

use super::*;
use crate::markdown::line::{base, spans_width};
use crate::markdown::parse::{IncrementalMarkdown, lex};
use crate::markdown::testing::MARKDOWN;
use crate::style::Attrs;

fn opts() -> RenderOptions {
    RenderOptions::default()
}

fn bounded(limit: usize) -> RenderOptions {
    RenderOptions {
        max_block_lines: limit,
        ..RenderOptions::default()
    }
}

fn text(lines: &[Line]) -> Vec<String> {
    lines.iter().map(Line::plain).collect()
}

fn joined(lines: &[Line]) -> String {
    text(lines).join("\n")
}

fn spans(lines: &[Line]) -> impl Iterator<Item = &Span> {
    lines.iter().flat_map(|l| l.spans.iter())
}

fn has_role(lines: &[Line], r: Role) -> bool {
    spans(lines).any(|s| s.style.top() == r)
}

/// The spans whose text is exactly `t`.
fn span_of<'a>(lines: &'a [Line], t: &str) -> Vec<&'a Span> {
    spans(lines).filter(|s| s.content == t).collect()
}

fn render(src: &str, width: usize) -> Vec<Line> {
    lex(src)
        .iter()
        .flat_map(|b| render_block(b, width, &opts()))
        .collect()
}

/// **A tab-indented fence expands its tabs, in every branch.**
///
/// MEASURED 2026-10-02, comparing notes with leticl: the fence path expanded
/// tabs NOWHERE, so a raw `\t` reached the terminal and was expanded at ITS stop
/// — conventionally eight — while the width layer counted it as ZERO columns. A
/// Go body indented four deep in the diff view was eight deep in a fence.
///
/// Three branches, and the reason all three are asserted: the highlighted path,
/// the path for a language there is no grammar for, and a bare fence. Fixing
/// only the first is the mistake that leaves ```text and unlabelled blocks
/// still broken.
#[test]
fn a_fenced_block_expands_its_tabs_however_it_is_highlighted() {
    let body: Vec<String> = vec![
        "func f() {".into(),
        "\tif x {".into(),
        "\t\treturn".into(),
        "\t}".into(),
        "}".into(),
    ];
    // go = a grammar rano has; text = one it does not; "" = a bare fence.
    for lang in ["go", "text", ""] {
        let block = Block::Code {
            lang: lang.into(),
            lines: body.clone(),
            closed: true,
        };
        let lines = text(&render_block(&block, 72, &opts()));
        let all = lines.join("\n");
        assert!(
            !all.contains('\t'),
            "a raw tab reached the terminal in a {lang:?} fence: {all:?}"
        );
        // One tab is four columns, two are eight — the run is the case a
        // source-index stop gets wrong, so it is asserted and not assumed.
        assert!(
            lines.iter().any(|l| l.contains("│     if x {")),
            "one tab is four spaces in a {lang:?} fence: {lines:?}"
        );
        assert!(
            lines.iter().any(|l| l.contains("│         return")),
            "two tabs are eight spaces in a {lang:?} fence: {lines:?}"
        );
    }
}

#[test]
fn a_long_block_becomes_a_title_a_count_and_a_tail() {
    let block = Block::Code {
        lang: "rust".into(),
        lines: (0..100).map(|i| format!("line {i}")).collect(),
        closed: true,
    };
    let lines = text(&render_bounded(&block, 72, &bounded(10)));
    assert_eq!(lines.len(), 10);
    assert!(lines[0].contains("rust · 100 lines"), "{:?}", lines[0]);
    assert!(lines[1].contains("lines elided"), "{:?}", lines[1]);
    // The tail is the end, because the end is the interesting part of a
    // streaming block.
    assert!(lines.last().unwrap().contains("└─"));
    assert!(lines[lines.len() - 2].contains("line 99"));
}

#[test]
fn the_bound_is_configurable_not_hardcoded() {
    let block = Block::Code {
        lang: String::new(),
        lines: (0..100).map(|i| format!("line {i}")).collect(),
        closed: true,
    };
    assert_eq!(render_bounded(&block, 72, &bounded(5)).len(), 5);
    assert_eq!(render_bounded(&block, 72, &bounded(30)).len(), 30);
    assert_eq!(render_bounded(&block, 72, &bounded(usize::MAX)).len(), 102);
    // …and unbounded is the default.
    assert_eq!(render_bounded(&block, 72, &opts()).len(), 102);
}

/// The register a reply is drawn in reaches every row, the frozen and the
/// summarised ones included.
#[test]
fn the_register_is_on_every_row() {
    let o = RenderOptions {
        base: Some(Role::Reasoning),
        max_block_lines: 4,
    };
    let lines = render_blocks(&lex(MARKDOWN), 72, &o);
    assert!(lines.iter().all(|l| base(l) == Some(Role::Reasoning)));
}

#[test]
fn wrapping_counts_columns_not_markup() {
    let runs = vec![
        Run {
            text: "a ".into(),
            style: InlineStyle::Plain,
        },
        Run {
            text: "code".into(),
            style: InlineStyle::Code,
        },
        Run {
            text: " b".into(),
            style: InlineStyle::Plain,
        },
    ];
    let s = runs_spans(&runs, Role::Plain);
    assert_eq!(spans_width(&s), "a code b".len());
    let block = Block::Paragraph { lines: vec![runs] };
    assert_eq!(render_block(&block, 20, &opts()).len(), 1);
}

/// A marker in the *hand-written* inline syntax is literal text now.
///
/// The point of the tree-sitter projection is that `**bold**` is `Run { text:
/// "bold", style: Bold }` and the asterisks are gone before the renderer sees
/// them. A `**` that is not emphasis is therefore also literal, and this pins
/// the two halves of that: the styles map to meanings, and nothing is scanned
/// for.
#[test]
fn the_renderer_sees_styles_not_markers() {
    let runs = vec![
        Run {
            text: "plain ".into(),
            style: InlineStyle::Plain,
        },
        Run {
            text: "bold".into(),
            style: InlineStyle::Bold,
        },
        Run {
            text: " and ".into(),
            style: InlineStyle::Plain,
        },
        Run {
            text: "code".into(),
            style: InlineStyle::Code,
        },
    ];
    let s = runs_spans(&runs, Role::Plain);
    let l = [Line::new(s)];
    assert_eq!(joined(&l), "plain bold and code");
    assert!(span_of(&l, "bold")[0].style.attrs.contains(Attrs::BOLD));
    assert_eq!(span_of(&l, "code")[0].style.top(), Role::Code);
    assert!(!span_of(&l, "plain ")[0].style.attrs.contains(Attrs::BOLD));
}

#[test]
fn painting_a_code_block_never_changes_the_text_in_it() {
    // The invariant that makes a highlighter safe to put underneath a wrapper:
    // drop the roles and the source comes back.
    let block = Block::Code {
        lang: "rust".into(),
        lines: vec![
            "fn main() {".into(),
            "    let s = \"a string\"; // and a comment".into(),
            "}".into(),
        ],
        closed: true,
    };
    let lines = render_block(&block, 100, &opts());
    let body: Vec<String> = text(&lines)
        .into_iter()
        .filter_map(|l| l.strip_prefix("│ ").map(str::to_string))
        .collect();
    assert_eq!(
        body,
        vec![
            "fn main() {",
            "    let s = \"a string\"; // and a comment",
            "}"
        ]
    );
    // …and it is actually painted.
    assert!(has_role(&lines, Role::Keyword), "{lines:?}");
    assert!(has_role(&lines, Role::StringLit), "{lines:?}");
    assert!(has_role(&lines, Role::Comment), "{lines:?}");
}

#[test]
fn every_block_kind_renders() {
    for b in lex(MARKDOWN) {
        assert!(!render_block(&b, 72, &opts()).is_empty(), "{b:?}");
    }
}

/// `render_blocks` is what a host draws a finished reply with: a blank row
/// between blocks, none at the end, and the same text a streamed render ends
/// with.
#[test]
fn a_whole_document_has_one_blank_row_between_blocks() {
    let blocks = lex(MARKDOWN);
    let lines = render_blocks(&blocks, 72, &opts());
    assert!(!lines.last().unwrap().is_empty());
    let blanks = lines.iter().filter(|l| l.is_empty()).count();
    assert_eq!(blanks, blocks.len() - 1, "{:#?}", text(&lines));
    let mut md = IncrementalMarkdown::new();
    md.push(MARKDOWN);
    assert_eq!(render_blocks(md.blocks(), 72, &opts()), lines);
}

mod tables {
    //! **The operator's own table, on the operator's own terminal.** 2026-09-17:
    //! *"table rendering is broken"* — a GFM table had no block of its own, so
    //! it lexed as a paragraph, joined with spaces and wrapped as prose.
    use super::*;
    use crate::markdown::parse::runs_text;

    /// The table from the session that reported this, verbatim.
    const BOARD: &str = "| branch | commits | status |\n\
        |---|---|---|\n\
        | `autocompact` | `1129111`, `aeee854`, `2c0c6c4` | done, tested, unmerged |\n\
        | `webfetch` | `f363cb0` | done, tested (18 + tools 509 + harnessd offline), unmerged |\n\
        | `main` | moved to `7056c64` (your intent + plan commits) | — |\n";

    fn rows(src: &str, width: usize) -> Vec<String> {
        text(&render(src, width))
    }

    /// A cell's plain text. The model holds runs; a test asserting on a table's
    /// contents means the text in it.
    fn cells(v: &[Vec<Run>]) -> Vec<String> {
        v.iter().map(|c| runs_text(c)).collect()
    }

    #[test]
    fn a_nested_list_is_indented_under_the_item_it_belongs_to() {
        // §2.7's missing half. A sub-bullet used to render in its parent's
        // column, because the walk that flattens nested lists into one vector
        // threw the depth away and the renderer had a single indent level.
        let r = rows("- top\n  - sub\n    - deeper\n- top again\n", 80);
        let lead = |l: &str| l.len() - l.trim_start().len();
        let at: Vec<usize> = r.iter().map(|l| lead(l)).collect();
        assert_eq!(at, vec![0, 2, 4, 0], "{r:#?}");
        assert!(r[1].contains("sub"), "{r:#?}");
        assert!(r[2].contains("deeper"), "{r:#?}");

        // **An `1. ` sub-list written at three or four spaces lands on the same
        // step as a `- ` one at two** — that is what rounding to an even column
        // is for.
        let two = rows("- a\n  1. one\n", 80);
        let four = rows("- a\n    1. one\n", 80);
        assert_eq!(lead(&two[1]), lead(&four[1]), "{two:#?} {four:#?}");
        assert_eq!(lead(&two[1]), 2, "{two:#?}");

        // And the step is capped, so a deeply nested list cannot walk off the
        // width. Six levels deep, which is past the cap: the last two flatten
        // onto 8 rather than continuing 10 and 12.
        let deep = rows(
            "- a\n  - b\n    - c\n      - d\n        - e\n          - f\n",
            80,
        );
        let at: Vec<usize> = deep.iter().map(|l| lead(l)).collect();
        assert_eq!(at, vec![0, 2, 4, 6, 8, 8], "{deep:#?}");
    }

    /// A wrapped item's continuation lines up under its own text, not under
    /// the marker, measured in columns (`·` is two bytes and one column).
    #[test]
    fn a_wrapped_item_continues_under_its_text() {
        let r = rows(
            "- one two three four five six seven eight nine ten eleven twelve\n",
            20,
        );
        assert!(r.len() > 1, "{r:#?}");
        assert!(r[0].starts_with("· one"), "{r:#?}");
        for l in &r[1..] {
            assert!(l.starts_with("  ") && !l.starts_with("   "), "{r:#?}");
        }
    }

    /// **The §2.7 ordering CONFLICT, ruled and pinned.**
    ///
    /// A model that writes `1.` for every item: every markdown renderer numbers
    /// those 1, 2, 3 — the repeated `1.` is an idiom, not a claim — so a head
    /// that echoed it would be showing something no other reader of that message
    /// sees. `start + i` is therefore the right rule.
    #[test]
    fn a_loose_ordered_list_counts_up_rather_than_echoing_a_repeated_marker() {
        let r = rows("1. a\n\n1. b\n\n1. c\n", 80);
        let starts: Vec<&str> = r.iter().map(|l| &l[..2.min(l.len())]).collect();
        assert_eq!(starts, vec!["1.", "2.", "3."], "{r:#?}");
        // The written number of the FIRST item is kept, so a list that starts at
        // seven stays at seven — the model phrasing its own list is not
        // corrected away.
        let r = rows("7. seven\n8. eight\n", 80);
        assert!(r[0].starts_with("7. "), "{r:#?}");
        assert!(r[1].starts_with("8. "), "{r:#?}");
    }

    /// A bullet is structure and is faint; an ordered list's number is content
    /// and is not.
    #[test]
    fn a_bullet_is_faint_and_a_number_is_not() {
        let l = render("- a\n", 80);
        assert_eq!(span_of(&l, "· ")[0].style.top(), Role::Faint);
        let l = render("1. a\n", 80);
        assert!(l[0].spans[0].content.starts_with("1. "), "{l:?}");
        assert_eq!(l[0].spans[0].style.top(), Role::Plain);
    }

    #[test]
    fn a_table_is_a_table_and_not_a_paragraph_of_pipes() {
        let blocks = lex(BOARD);
        assert_eq!(blocks.len(), 1, "{blocks:#?}");
        let Block::Table { head, align, rows } = &blocks[0] else {
            panic!("not a table: {blocks:#?}");
        };
        assert_eq!(cells(head), ["branch", "commits", "status"]);
        assert_eq!(align, &[Align::Left, Align::Left, Align::Left]);
        assert_eq!(rows.len(), 3);
        assert_eq!(
            cells(&rows[2]),
            ["main", "moved to 7056c64 (your intent + plan commits)", "—"]
        );
    }

    #[test]
    fn the_columns_line_up_and_nothing_is_lost() {
        let lines = render(BOARD, 120);
        let out = text(&lines);
        let screen = out.join("\n");
        for want in [
            "branch",
            "autocompact",
            "1129111",
            "webfetch",
            "f363cb0",
            "harnessd offline",
            "7056c64",
        ] {
            assert!(screen.contains(want), "{want} missing:\n{screen}");
        }
        // The separator column sits at the same place on the header and on the
        // first body row — which is the whole claim a table makes.
        let bar = |l: &str| {
            l.char_indices()
                .filter(|(_, c)| *c == '│')
                .map(|(i, _)| i)
                .collect::<Vec<_>>()
        };
        assert!(!bar(&out[0]).is_empty(), "no column separators:\n{screen}");
        assert_eq!(
            bar(&out[0]),
            bar(&out[2]),
            "header and first row disagree:\n{screen}"
        );
        for l in &lines {
            assert!(l.width() <= 120, "{} columns: {:?}", l.width(), l.plain());
        }
        // The header is strong and the frame is faint.
        assert_eq!(span_of(&lines, "branch")[0].style.top(), Role::Strong);
        assert_eq!(span_of(&lines, " │ ")[0].style.top(), Role::Faint);
    }

    #[test]
    fn a_narrow_terminal_wraps_the_wide_column_and_keeps_the_short_ones() {
        let lines = render(BOARD, 60);
        let screen = joined(&lines);
        for l in &lines {
            assert!(l.width() <= 60, "{} columns: {:?}", l.width(), l.plain());
        }
        // The long status text is wrapped, not cut: every word still there.
        assert!(screen.contains("harnessd"), "{screen}");
        assert!(screen.contains("unmerged"), "{screen}");
        // And the narrow `branch` column was not taken down with it.
        assert!(screen.contains("autocompact"), "{screen}");
    }

    #[test]
    fn alignment_and_escaped_pipes_are_honoured() {
        let src = "| n | name | size |\n|--:|:----:|:-----|\n| 1 | a\\|b | wide |\n";
        let blocks = lex(src);
        let Block::Table { align, rows, .. } = &blocks[0] else {
            panic!("{blocks:#?}")
        };
        assert_eq!(align, &[Align::Right, Align::Center, Align::Left]);
        assert_eq!(
            rows[0][1],
            vec![Run {
                text: "a|b".into(),
                style: InlineStyle::Plain
            }],
            "an escaped pipe is a pipe, not a cell break"
        );
        let out = super::tables::rows(src, 40);
        // Right-aligned `n`: the digit sits at the column's right edge, under
        // the header's own right edge.
        let col = |l: &str| l.find('│').unwrap_or(0);
        assert_eq!(col(&out[0]), col(&out[2]), "{out:#?}");
    }

    #[test]
    fn a_paragraph_with_a_pipe_in_it_is_still_a_paragraph() {
        for src in [
            "run `a | b` to pipe it\n",
            "| this looks like a row |\nbut the next line is prose\n",
            "|---|---|\n",
        ] {
            let blocks = lex(src);
            assert!(
                !blocks.iter().any(|b| matches!(b, Block::Table { .. })),
                "{src:?} lexed as a table: {blocks:#?}"
            );
        }
    }

    /// A table arriving a few bytes at a time renders the same as one that
    /// arrived whole — the window freezes only at blank lines, and a table has
    /// none inside it.
    #[test]
    fn a_streamed_table_is_the_same_table() {
        let mut md = IncrementalMarkdown::new();
        for chunk in BOARD.as_bytes().chunks(7) {
            md.push(std::str::from_utf8(chunk).unwrap());
        }
        let streamed: Vec<&Block> = md.blocks().collect();
        assert_eq!(streamed.len(), 1, "{streamed:#?}");
        assert!(matches!(streamed[0], Block::Table { .. }), "{streamed:#?}");
    }
}

mod inline_render {
    use super::*;

    /// The whole point of the workstream, asserted on the rendered output: a
    /// model's `**bold**` is bold on the screen and the asterisks are not.
    #[test]
    fn markers_are_styles_on_the_screen_and_not_characters() {
        let l = render("plain **bold** and `code` and ~~struck~~ end\n", 60);
        assert_eq!(joined(&l), "plain bold and code and struck end");
        assert!(
            span_of(&l, "bold")[0].style.attrs.contains(Attrs::BOLD),
            "{l:?}"
        );
        assert_eq!(span_of(&l, "code")[0].style.top(), Role::Code, "{l:?}");
        assert!(
            span_of(&l, "struck")[0].style.attrs.contains(STRUCK),
            "{l:?}"
        );
    }

    /// A `*` that is not emphasis is text, on the screen as in the model.
    #[test]
    fn a_literal_asterisk_survives_the_renderer() {
        let l = render("2 * 3 = 6\n", 40);
        assert_eq!(joined(&l), "2 * 3 = 6");
        assert!(
            !spans(&l).any(|s| s.style.attrs.contains(Attrs::ITALIC)),
            "{l:?}"
        );
    }

    /// Emphasis inside a container keeps the container: bold in a quote is
    /// still faint, and a code span in a heading keeps the heading's weight.
    #[test]
    fn inline_styles_compose_with_their_block() {
        let l = render("> a **b** c\n", 40);
        let b = span_of(&l, "b")[0];
        assert_eq!(b.style.top(), Role::Faint);
        assert!(b.style.attrs.contains(Attrs::BOLD));
        assert_eq!(span_of(&l, " c")[0].style.top(), Role::Faint);
        let l = render("## the `x` thing\n", 40);
        assert_eq!(span_of(&l, "x")[0].style.top(), Role::Code);
        assert!(span_of(&l, "x")[0].style.attrs.contains(Attrs::BOLD));
        assert_eq!(span_of(&l, " thing")[0].style.top(), Role::Subheading);
    }

    /// A code span in a quote stays faint under its colour, as letibot drew it: the
    /// quote is set back, the code in it is set back with it.
    #[test]
    fn a_code_span_in_a_quote_keeps_the_quote_under_it() {
        let l = render("> see `x` here\n", 40);
        let x = span_of(&l, "x")[0];
        assert_eq!(x.style.top(), Role::Code);
        assert_eq!(
            x.style.roles().collect::<Vec<_>>(),
            vec![Role::Faint, Role::Code]
        );
        let l = render("see `x` here\n", 40);
        assert_eq!(
            span_of(&l, "x")[0].style.roles().collect::<Vec<_>>(),
            vec![Role::Code]
        );
    }

    /// Two captures side by side are two spans even when they mean the same thing:
    /// `assert_eq` and `!` are separate runs on the row a string host prints.
    #[test]
    fn adjacent_captures_of_one_role_stay_separate_spans() {
        let l = render("```rust\nassert_eq!(a, b);\n```\n", 60);
        let row = &l[1];
        let kw: Vec<&str> = row
            .spans
            .iter()
            .filter(|s| s.style.top() != Role::Plain && s.style.top() != Role::Faint)
            .map(|s| s.content.as_str())
            .collect();
        assert!(
            kw.starts_with(&["assert_eq", "!"]),
            "the macro name and its bang were merged: {kw:?}"
        );
    }

    /// Heading levels are different roles, and the hashes stay, faint.
    #[test]
    fn heading_levels_carry_their_role_and_their_hashes() {
        for (src, role) in [
            ("# a\n", Role::Heading),
            ("## a\n", Role::Subheading),
            ("### a\n", Role::Strong),
        ] {
            let l = render(src, 40);
            assert_eq!(joined(&l), src.trim_end());
            assert_eq!(span_of(&l, "a")[0].style.top(), role);
            assert_eq!(l[0].spans[0].style.top(), Role::Faint);
        }
    }

    /// Everything a real answer contains, rendered without losing a word.
    ///
    /// The failure this guards is the interesting one: a projection that drops a
    /// block, or a run, produces a *plausible* screen. Only comparing against
    /// the source's words catches it.
    #[test]
    fn a_whole_answer_renders_every_word_it_was_written_with() {
        let src = "\
## Why the cache missed

The short answer is `reasoning_content`. Three things had to line up:

1. The dialect replays prior reasoning.
2. The ledger appends **ids**, never re-derived text.

```rust
let a = 1;
```

> Note that **committed** tokens are what matters.

| n | name |
|--:|:-----|
| 1 | a\\|b |

---

done
";
        let lines = render(src, 72);
        let text = joined(&lines);
        for word in [
            "Why the cache missed",
            "reasoning_content",
            "line up",
            "The dialect replays",
            "never re-derived text",
            "let a = 1;",
            "committed",
            "tokens are what matters",
            "a|b",
            "done",
        ] {
            assert!(text.contains(word), "{word:?} is missing from: {text}");
        }
        assert!(!text.contains("**"), "{text}");
        assert!(
            !text.contains("|--:"),
            "the delimiter row reached the screen: {text}"
        );
        // The hashes *do* stay, and deliberately: the heading arm keeps them
        // faint so the level survives a monochrome palette. What must not reach
        // the screen is a marker *inside* prose.
        assert!(
            text.lines().next().is_some_and(|l| l.starts_with("## Why")),
            "the level marker is drawn: {text}"
        );
        // The markers that did not reach the screen are meanings that did.
        assert!(
            spans(&lines).any(|s| s.style.attrs.contains(Attrs::BOLD)),
            "nothing came out bold"
        );
        assert!(has_role(&lines, Role::Code), "nothing came out as code");
    }

    /// The whole of a real message, rendered: the one the operator saw as cyan
    /// prose.
    ///
    /// The fixture is the exact bytes from the store, and the failure it pins
    /// was visible rather than structural — ten numbered items rendered with
    /// their `**` showing and the whole message in code cyan, because a code
    /// span in the first paragraph closed three kilobytes later.
    #[test]
    fn a_real_message_renders_without_its_markers() {
        const REAL: &str = include_str!("../../../tests/fixtures/markdown/streamed-message.md");
        let lines = render(REAL, 200);
        let text = joined(&lines);
        for n in 1..=10 {
            assert!(
                !text.contains(&format!("**{n}.")),
                "item {n} kept its markers:\n{text}"
            );
        }
        assert!(
            spans(&lines).any(|s| s.style.attrs.contains(Attrs::BOLD)),
            "nothing came out bold"
        );
        assert!(has_role(&lines, Role::Code), "nothing came out as code");
        // Most of the message is not code: the swallowed-message bug made it all
        // code-coloured.
        let code_cols: usize = spans(&lines)
            .filter(|s| s.style.top() == Role::Code)
            .map(|s| s.content.len())
            .sum();
        assert!(code_cols * 4 < text.len(), "{code_cols} of {}", text.len());
        // Every item is still on the screen, and the last line of the 100-line
        // fence.
        for n in 1..=10 {
            assert!(text.contains(&format!("\n{n}. ")), "item {n} is missing");
        }
        assert!(text.contains("fn f99()"), "the long code block is missing");
        assert!(text.contains("a|b"), "the escaped pipe is missing");
    }
}

mod a_fence_is_coloured_only_if_it_names_a_language {
    use super::*;

    /// The language in the info string is what gets coloured, and nothing else
    /// is.
    ///
    /// A fence that names no language renders its frame with no label and its
    /// body plain, because there is nothing to colour with, and guessing one
    /// from the shape of the text is the kind of invention a head should not do.
    /// The operator read that as "colourisation is gone" (2026-09-20) while
    /// looking at a message whose illustrative fences were bare; this test is
    /// here so the next reader can tell the difference between a plain block and
    /// a broken painter in one line.
    ///
    /// **And the rule carries no label either** (R51 item 12): it was `┌─ code`
    /// until the operator asked for the word to go.
    ///
    /// The *content* being a fence does not make it a fence: inside a
    /// four-backtick block the line ```rust is the bytes a model wrote about a
    /// fence.
    #[test]
    fn a_fence_with_a_language_is_coloured_and_one_without_is_not() {
        let named = render("```rust\nfn main() {}\n```\n", 60);
        assert!(joined(&named).contains("┌─ rust"));
        assert!(has_role(&named, Role::Keyword), "{named:?}");

        let bare = render("```\nfn main() {}\n```\n", 60);
        let out = joined(&bare);
        assert!(!out.contains("┌─ code"), "invented a label: {out:?}");
        assert!(out.contains("┌─"), "the frame is drawn: {out:?}");
        assert!(!has_role(&bare, Role::Keyword), "invented a language");
        assert!(!has_role(&bare, Role::StringLit), "invented a language");
        assert!(!out.contains("┌─ "), "a label follows the frame: {out:?}");
        assert!(out.contains("└─"), "{out:?}");

        // A four-backtick block holding a three-backtick fence. **This is the
        // case where the fence's own length is the author's signal**: a model
        // teaching how to write a markdown rust block, where the backticks are
        // the content, not a marker. So the demonstration must be *visible*.
        let quoted = lex("````\n```rust\nlet a = 1;\n```\n````\n");
        assert_eq!(quoted.len(), 1, "{quoted:#?}");
        let out = joined(&render_block(&quoted[0], 60, &opts()));
        assert!(
            !out.contains("┌─ rust"),
            "the inner fence was interpreted: {out:?}"
        );
        for line in ["```rust", "let a = 1;", "```"] {
            assert!(out.contains(line), "{line:?} is missing from {out:?}");
        }
        // No box inside the box: one frame, not two.
        assert_eq!(out.matches("┌─").count(), 1, "{out:?}");
        assert_eq!(out.matches("└─").count(), 1, "{out:?}");

        // The outer fence can name the language being demonstrated, and then
        // the *label* says so even though the body is plain.
        let teaching = lex("````markdown\n```rust\nlet a = 1;\n```\n````\n");
        assert_eq!(teaching.len(), 1, "{teaching:#?}");
        let out = joined(&render_block(&teaching[0], 60, &opts()));
        assert!(out.contains("┌─ markdown"), "{out:?}");
        assert!(out.contains("```rust"), "the demo was eaten: {out:?}");
        assert!(!out.contains("┌─ rust"), "{out:?}");
    }

    /// An open fence says so in its frame: the last block of a streaming reply
    /// is usually one, and it has to render without waiting.
    #[test]
    fn an_open_fence_says_it_is_still_being_written() {
        let out = joined(&render("```rust\nfn a() {\n", 60));
        assert!(out.ends_with("└─ (still writing…)"), "{out:?}");
    }
}

mod code_fences_are_coloured_by_rano {
    use super::*;

    fn painted(fence: &str, body: &str) -> Vec<Line> {
        let src = format!("```{fence}\n{body}```\n");
        let b = lex(&src);
        let Some(Block::Code { .. }) = b.first() else {
            panic!("not a code block: {b:#?}")
        };
        render_block(&b[0], 72, &opts())
    }

    fn syntax(lines: &[Line]) -> bool {
        spans(lines).any(|s| !matches!(s.style.top(), Role::Plain | Role::Faint))
    }

    /// **The whole point of R18.1.** These languages were rendered plain by the
    /// hand-written lexer letibot used before — it knew ten, and none of these
    /// was among them.
    #[test]
    fn languages_the_old_lexer_did_not_know_are_coloured() {
        for (fence, body, header) in [
            ("tsx", "const App = () => <div>hi</div>;\n", "tsx"),
            ("lua", "local function f(x)\n  return x + 1\nend\n", "lua"),
            (
                "ruby",
                "def greet(name)\n  puts \"hi #{name}\"\nend\n",
                "ruby",
            ),
            (
                "diff",
                "--- a/f.rs\n+++ b/f.rs\n@@ -1 +1 @@\n-old\n+new\n",
                "diff",
            ),
            ("clojure", "(defn f [x] (+ x 1))\n", "clojure"),
            ("sql", "SELECT id FROM t WHERE n > 1;\n", "sql"),
        ] {
            let out = painted(fence, body);
            assert!(
                joined(&out).contains(header),
                "{fence}: no `{header}` in the header: {out:?}"
            );
            assert!(syntax(&out), "{fence} was rendered with no colour: {out:?}");
            // And the text survives the painting, which is the invariant
            // everything else rests on.
            assert!(
                joined(&out).contains(body.lines().next().unwrap()),
                "{fence}"
            );
        }
    }

    /// The header names the grammar **that ran**, not the fence's spelling —
    /// so a fence that says `ts` is not claiming to be tsx, and one thing is
    /// named once.
    #[test]
    fn the_header_names_the_grammar_that_ran() {
        assert!(joined(&painted("rust", "fn main() {}\n")).contains("┌─ rust"));
        assert!(
            joined(&painted("rs", "fn main() {}\n")).contains("┌─ rust"),
            "`rs` is rust"
        );
        assert!(joined(&painted("ts", "const x = 1;\n")).contains("┌─ typescript"));
        assert!(
            joined(&painted("sh", "echo hi\n")).contains("┌─ bash"),
            "`sh` is bash"
        );
        // A language with no grammar keeps the fence's own word, uncoloured —
        // naming a language we are not colouring is honest, inventing one is
        // not.
        let out = painted("brainfuck", "+[->+<]\n");
        assert!(!syntax(&out), "invented a grammar: {out:?}");
        assert_eq!(joined(&out), "┌─ brainfuck\n│ +[->+<]\n└─");
    }

    /// A fence that names no language renders a rule with **no label in it** —
    /// `┌─` and `└─`, the frame the reader needs and nothing the author did not
    /// write. (`3c5540d` in leticl, R51 item 12.)
    #[test]
    fn a_bare_fence_draws_a_rule_with_no_label_in_it() {
        let out = painted("", "fn main() {}\n");
        assert_eq!(joined(&out), "┌─\n│ fn main() {}\n└─");
        assert_eq!(out[0].spans[0].style.top(), Role::Faint);
    }

    /// **Structure rather than shape.** These are the cases letibot's
    /// hand-written lexer got wrong, and each is a fact about the code that was
    /// deleted rather than a guess: its quote set included `'`, its type test
    /// was `word.starts_with(uppercase)`, and its function test was "a `(`
    /// follows the word".
    #[test]
    fn what_the_heuristics_got_wrong_is_right_now() {
        // **A Rust lifetime opened a string literal.** The grammar knows a
        // lifetime from a character literal, and `str` is a type.
        let out = painted("rust", "fn f<'a>(x: &'a str) -> &'a str { x }\n");
        assert!(
            span_of(&out, "a")
                .iter()
                .any(|s| s.style.top() == Role::TypeName),
            "the lifetime is not a type: {out:?}"
        );
        assert!(
            span_of(&out, "str")
                .iter()
                .any(|s| s.style.top() == Role::TypeName),
            "`str` is not a type: {out:?}"
        );
        assert!(
            !has_role(&out, Role::StringLit),
            "something was painted as a string in a signature with none: {out:?}"
        );

        // **A capitalised Go variable is not a type.**
        let out = painted("go", "func f() {\n\tX := 1\n\t_ = X\n}\n");
        assert!(
            !has_role(&out, Role::TypeName),
            "`X` was called a type: {out:?}"
        );

        // **A macro is a function though only `!` follows it.**
        let out = painted("rust", "println!(\"hi\");\n");
        assert!(
            has_role(&out, Role::FuncName),
            "`println!` is not a function: {out:?}"
        );

        // **A Python decorator is a function.**
        let out = painted("python", "@decorator\ndef f():\n    pass\n");
        assert!(
            spans(&out)
                .any(|s| s.style.top() == Role::FuncName && s.content.starts_with("@decorator")),
            "not a function: {out:?}"
        );
    }

    /// A block comment **closing** lines later is a comment, which the old
    /// lexer needed a carried state for and the grammar simply knows.
    #[test]
    fn a_construct_spanning_lines_keeps_its_colour() {
        let out = painted("rust", "/* one\ntwo\nthree */\nlet x = 1;\n");
        for (i, l) in out.iter().enumerate().skip(1).take(3) {
            assert!(
                l.spans.iter().any(|s| s.style.top() == Role::Comment),
                "line {i} lost its colour: {l:?}"
            );
        }
        // And a multi-line string.
        let out = joined(&painted("rust", "let s = r#\"one\ntwo\";\n"));
        assert!(out.contains("one"), "{out:?}");
        assert!(out.contains("two"), "{out:?}");
    }
}
