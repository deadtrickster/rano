//! A growing document's rows, with the frozen prefix rendered once.
//!
//! Ported from letibot's `BlockCache`. The parse window buys nothing if the
//! *renderer* rebuilds every row from every block on every delta — that is
//! pi's mistake (rebuild the whole message per `message_update`) moved one layer
//! down. So:
//!
//! - The frozen prefix of a document renders **once per width**. The view keeps
//!   its rows and extends them as blocks freeze; a resize (or a change of the
//!   options the rows were drawn under) is the only thing that invalidates it.
//! - Only the live tail is re-rendered per frame, and the tail is bounded by
//!   the window's frozen frontier.
//!
//! The view holds no palette: rows are role-tagged, so a palette change is the
//! host drawing the same rows differently, not a reason to re-render.

use std::collections::HashMap;

use super::parse::{Block, IncrementalMarkdown};
use super::render::{CodePaint, RenderOptions, blank, render_bounded_with};
use crate::render::{Line, Span};

/// A per-row decoration applied to everything a [`MarkdownView`] produces.
///
/// It exists so a pane can carry a rail (letibot's reasoning pane draws `┃`
/// down its left) **without** copying its frozen prefix into every frame. The
/// rail has to be on every row — that is the point of it, it is the signal that
/// survives a copy-paste when the colour does not — and the only place it can be
/// applied once per row rather than once per row per frame is at the moment the
/// row enters the cache.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Decor {
    /// Prepended to every row. Its width must be subtracted from the width the
    /// host renders at, or the block is one row taller than the space reserved
    /// for it.
    pub prefix: Vec<Span>,
}

impl Decor {
    /// Decorate one row. Public because a host that draws a single row beside a
    /// cached block — the live tail of a folded reasoning pane — has to decorate
    /// it the same way, and the only way to guarantee that is for there to be
    /// one function.
    pub fn apply(&self, l: Line) -> Line {
        if self.prefix.is_empty() {
            return l;
        }
        let mut spans = self.prefix.clone();
        spans.extend(l.spans);
        Line {
            spans,
            style: l.style,
        }
    }
}

/// Rendered rows for a growing document, with the frozen prefix cached.
///
/// ```
/// use rano::markdown::{IncrementalMarkdown, MarkdownView, RenderOptions};
///
/// let mut md = IncrementalMarkdown::new();
/// let mut view = MarkdownView::new();
/// let opts = RenderOptions::default();
/// for delta in ["# Title\n\nSome **bold", "** text.\n\n```rust\nfn main", "() {}\n```\n"] {
///     md.push(delta);
///     // Per frame: the settled rows by reference, the live tail fresh.
///     let (settled, tail) = view.split(&md, 80, &opts);
///     let _rows = settled.len() + tail.len();
/// }
/// let rows = view.lines(&md, 80, &opts);
/// assert_eq!(rows[0].plain(), "# Title");
/// ```
#[derive(Debug, Default)]
pub struct MarkdownView {
    width: usize,
    opts: Option<RenderOptions>,
    decor: Decor,
    /// One streaming highlighter per open code block, keyed by the block's
    /// **absolute** index in the document.
    ///
    /// Absolute rather than tail-relative because the tail is re-parsed from the
    /// frozen frontier on every push, so a tail block's position moves as blocks
    /// freeze while `stable_count() + tail_index` does not. An entry is dropped
    /// the moment its block freezes: the block is then in `stable_lines` and
    /// will never be rendered again.
    codes: HashMap<usize, CodePaint>,
    /// Rows for the blocks that were frozen when they were rendered.
    stable_lines: Vec<Line>,
    /// How many of the document's stable blocks are already in `stable_lines`.
    rendered_blocks: usize,
    /// Instrumentation: blocks handed to the renderer over this view's life.
    blocks_rendered: u64,
}

impl MarkdownView {
    pub fn new() -> Self {
        MarkdownView::default()
    }

    /// A view whose every row carries `decor`. See [`Decor`].
    pub fn decorated(decor: Decor) -> Self {
        MarkdownView {
            decor,
            ..MarkdownView::default()
        }
    }

    /// Set the decoration, throwing the cache away if it changed: a prefix
    /// decorated the old way is stale in exactly the way a prefix rendered at
    /// the old width is.
    pub fn set_decor(&mut self, decor: Decor) {
        if self.decor != decor {
            self.decor = decor;
            self.invalidate();
        }
    }

    fn invalidate(&mut self) {
        self.stable_lines.clear();
        self.rendered_blocks = 0;
        self.codes.clear();
    }

    /// Every row of the document: the cached prefix plus a freshly rendered
    /// tail, with no trailing blank rows.
    ///
    /// Convenience over [`MarkdownView::split`], and it **copies the prefix**.
    /// Fine for a one-shot render of a finished document; not for the per-frame
    /// path, where the copy is O(accumulated output) and a frame must not be.
    pub fn lines(
        &mut self,
        md: &IncrementalMarkdown,
        width: usize,
        opts: &RenderOptions,
    ) -> Vec<Line> {
        let (stable, tail) = self.split(md, width, opts);
        let mut out = stable.to_vec();
        out.extend(tail);
        while out.last().is_some_and(Line::is_empty) {
            out.pop();
        }
        out
    }

    /// The frozen prefix **by reference**, and the live tail freshly rendered.
    ///
    /// This is the per-frame form. The prefix is the part that grows without
    /// bound over a long answer, and handing it back borrowed is what keeps the
    /// cost of drawing a frame proportional to the tail and the window rather
    /// than to everything said so far.
    ///
    /// The prefix ends with a blank row after its last block, so the two halves
    /// concatenate into the document; the tail has no trailing blank.
    pub fn split(
        &mut self,
        md: &IncrementalMarkdown,
        width: usize,
        opts: &RenderOptions,
    ) -> (&[Line], Vec<Line>) {
        if self.width != width || self.opts != Some(*opts) {
            // A resize is the only thing that invalidates the prefix — and a
            // change of bound, which is a resize of a different axis: the same
            // block renders to a different number of rows when the bound moves,
            // so a prefix rendered under the old one is stale in exactly the
            // same way. The register is in the key because every row carries it.
            self.width = width;
            self.opts = Some(*opts);
            self.invalidate();
        }
        let stable = md.stable();
        for (i, b) in stable.iter().enumerate().skip(self.rendered_blocks) {
            let mut paint = self
                .codes
                .remove(&i)
                .filter(|p| matches!(b, Block::Code { lang, .. } if p.is_for(lang)));
            let lines = render_bounded_with(b, width, opts, paint.as_mut());
            self.stable_lines
                .extend(lines.into_iter().map(|l| self.decor.apply(l)));
            self.stable_lines.push(blank(opts));
            self.blocks_rendered += 1;
        }
        self.rendered_blocks = stable.len();

        let mut tail = Vec::new();
        for (j, b) in md.tail().iter().enumerate() {
            let abs = stable.len() + j;
            let mut paint = self.codes.remove(&abs);
            if let Block::Code { lang, .. } = b
                && !paint.as_ref().is_some_and(|p| p.is_for(lang))
            {
                paint = Some(CodePaint::new(lang));
            }
            let lines = render_bounded_with(b, width, opts, paint.as_mut());
            if let Some(p) = paint {
                self.codes.insert(abs, p);
            }
            tail.extend(lines.into_iter().map(|l| self.decor.apply(l)));
            tail.push(blank(opts));
            self.blocks_rendered += 1;
        }
        while tail.last().is_some_and(Line::is_empty) {
            tail.pop();
        }
        (&self.stable_lines, tail)
    }

    /// Blocks handed to the renderer over this view's life. With the prefix
    /// cached this grows with the tail per frame, not with the document.
    pub fn blocks_rendered(&self) -> u64 {
        self.blocks_rendered
    }

    /// Parses performed over this view's life, summed over every live code
    /// block — the window's `bytes_lexed` instrument one layer down.
    ///
    /// It counts parses rather than bytes because a [`crate::syntax::Stream`]
    /// re-uses everything its last parse built, so its cost is the *number* of
    /// parses. A frame that draws a settled fence must not add to this.
    pub fn parses(&self) -> u64 {
        self.codes.values().map(|c| c.parses()).sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::markdown::line::base;
    use crate::markdown::render::render_blocks;
    use crate::markdown::testing::MARKDOWN;
    use crate::style::Role;

    fn text(lines: &[Line]) -> Vec<String> {
        lines.iter().map(Line::plain).collect()
    }

    fn opts() -> RenderOptions {
        RenderOptions {
            max_block_lines: 40,
            ..RenderOptions::default()
        }
    }

    #[test]
    fn the_frozen_prefix_is_rendered_once() {
        let mut md = IncrementalMarkdown::new();
        let mut view = MarkdownView::new();
        let doc = MARKDOWN.repeat(4);
        let mut frames = 0;
        let mut buf = String::new();
        for ch in doc.chars() {
            buf.push(ch);
            if buf.chars().count() >= 8 {
                md.push(&buf);
                buf.clear();
                view.lines(&md, 72, &opts());
                frames += 1;
            }
        }
        let blocks = md.stable_count() + md.tail().len();
        assert!(
            view.blocks_rendered() < (blocks + frames * 3) as u64,
            "rendered {} block-renders over {frames} frames for {blocks} blocks; \
             the stable prefix is being rebuilt",
            view.blocks_rendered()
        );
    }

    #[test]
    fn a_resize_is_the_only_thing_that_invalidates_the_cache() {
        let mut md = IncrementalMarkdown::new();
        md.push(MARKDOWN);
        let mut view = MarkdownView::new();
        let a = view.lines(&md, 72, &opts());
        let after_first = view.blocks_rendered();
        let b = view.lines(&md, 72, &opts());
        assert_eq!(a, b);
        assert!(
            view.blocks_rendered() - after_first <= md.tail().len() as u64,
            "an idle frame re-rendered the prefix"
        );
        let c = view.lines(&md, 40, &opts());
        assert_ne!(a, c, "a resize must re-wrap");
    }

    /// Whatever arrived in what pieces, the view ends on the rows a one-shot
    /// render of the finished document gives.
    #[test]
    fn a_streamed_view_ends_on_the_one_shot_rows() {
        let mut md = IncrementalMarkdown::new();
        let mut view = MarkdownView::new();
        let chars: Vec<char> = MARKDOWN.chars().collect();
        for c in chars.chunks(5) {
            md.push(&c.iter().collect::<String>());
            view.split(&md, 72, &opts());
        }
        assert_eq!(
            view.lines(&md, 72, &opts()),
            render_blocks(&crate::markdown::lex(MARKDOWN), 72, &opts())
        );
    }

    /// One layer down: **a frame that draws a settled fence does not re-parse
    /// it.** The reason the view keeps a highlighter rather than calling a
    /// one-shot painter per frame.
    #[test]
    fn a_settled_code_block_is_not_parsed_again_per_frame() {
        let src: String = (0..300)
            .map(|i| format!("    let x{i} = \"value {i}\"; // comment {i}\n"))
            .collect();
        let doc = format!("```rust\n{src}```\n");
        let mut md = IncrementalMarkdown::new();
        md.push(&doc);
        // The first frame parses; the next fifty must not.
        let mut view = MarkdownView::new();
        view.split(&md, 100, &opts());
        let after_first = view.parses();
        assert!(after_first >= 1, "the block was never parsed");
        for _ in 0..50 {
            view.split(&md, 100, &opts());
        }
        assert_eq!(
            view.parses(),
            after_first,
            "a settled block was re-parsed by drawing it"
        );
    }

    /// A fence whose info string arrives after its backticks is still coloured
    /// by the language it ends up naming — in the tail and once settled. The
    /// first frame sees "```" alone, and the highlighter built then was kept.
    #[test]
    fn a_fence_named_after_its_backticks_is_still_highlighted() {
        let mut md = IncrementalMarkdown::new();
        let mut view = MarkdownView::new();
        for d in ["```", "ru", "st\nfn main", "() {}\n", "```\n", "\nafter\n"] {
            md.push(d);
            view.split(&md, 72, &opts());
        }
        assert!(md.stable_count() >= 1, "the fence settled");
        let rows = view.lines(&md, 72, &opts());
        assert_eq!(rows[0].plain(), "┌─ rust");
        assert!(
            rows.iter()
                .flat_map(|l| &l.spans)
                .any(|s| s.style.top() == Role::Keyword),
            "{rows:?}"
        );
    }

    #[test]
    fn a_decorated_view_puts_the_rail_on_every_line_including_the_frozen_ones() {
        let d = Decor {
            prefix: vec![Span::role("┃ ", Role::Faint)],
        };
        let mut md = IncrementalMarkdown::new();
        md.push("one paragraph\n\nanother paragraph\n\nand a third that is still open");
        let mut view = MarkdownView::decorated(d);
        let lines = view.lines(&md, 40, &opts());
        assert!(md.stable_count() > 0, "some of it must be frozen");
        for l in lines.iter().filter(|l| !l.is_empty()) {
            assert!(l.plain().starts_with("┃ "), "{l:?}");
        }
    }

    /// The register reaches the cached rows too, and changing it re-renders.
    #[test]
    fn the_register_is_part_of_the_cache_key() {
        let mut md = IncrementalMarkdown::new();
        md.push(MARKDOWN);
        let mut view = MarkdownView::new();
        let top = view.lines(&md, 72, &opts());
        assert!(top.iter().all(|l| base(l).is_none()));
        let inside = RenderOptions {
            base: Some(Role::Reasoning),
            ..opts()
        };
        let r = view.lines(&md, 72, &inside);
        assert!(r.iter().all(|l| base(l) == Some(Role::Reasoning)));
        assert_eq!(text(&top), text(&r));
    }

    /// **The streaming property end to end**: a real reply pushed a few bytes at
    /// a time, with a frame drawn after every push, costs work linear in its
    /// length — in the window (bytes shown to a parser) and in the painter
    /// (blocks rendered). Doubling the stream must roughly double both; a cache
    /// that rebuilt its prefix, or a window that stopped settling, shows up as
    /// a ratio heading for 4.
    #[test]
    fn streaming_a_real_reply_through_the_view_is_linear() {
        const REAL: &str = include_str!("../../tests/fixtures/markdown/streamed-message.md");
        fn run(copies: usize) -> (usize, u64, u64, usize) {
            let doc = REAL.repeat(copies);
            let mut md = IncrementalMarkdown::new();
            let mut view = MarkdownView::new();
            let mut buf = String::new();
            let mut frames = 0;
            for ch in doc.chars() {
                buf.push(ch);
                if buf.len() >= 16 {
                    md.push(&buf);
                    buf.clear();
                    view.split(&md, 100, &RenderOptions::default());
                    frames += 1;
                }
            }
            md.push(&buf);
            let rows = view.lines(&md, 100, &RenderOptions::default());
            let once = render_blocks(md.blocks(), 100, &RenderOptions::default());
            assert_eq!(rows.len(), once.len());
            if let Some(i) = (0..rows.len()).find(|&i| rows[i] != once[i]) {
                panic!("row {i} differs:\n{:?}\n{:?}", rows[i], once[i]);
            }
            (frames, md.bytes_lexed(), view.blocks_rendered(), doc.len())
        }
        let (f1, lexed1, rendered1, n1) = run(4);
        let (f2, lexed2, rendered2, n2) = run(8);
        let lex_ratio = lexed2 as f64 / lexed1 as f64;
        let render_ratio = rendered2 as f64 / rendered1 as f64;
        eprintln!(
            "view: {n1} B / {f1} frames: lexed {lexed1} B, rendered {rendered1} blocks; \
             {n2} B / {f2} frames: lexed {lexed2} B, rendered {rendered2} blocks; \
             ratios {lex_ratio:.2} / {render_ratio:.2}"
        );
        assert!(
            lex_ratio < 2.6,
            "lexing grew {lex_ratio:.2}× for 2× the text"
        );
        assert!(
            render_ratio < 2.6,
            "rendering grew {render_ratio:.2}× for 2× the text"
        );
    }
}
