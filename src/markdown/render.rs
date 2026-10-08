//! Blocks to terminal rows.
//!
//! Ported from letibot's `crates/tui/src/ui/render.rs`. The shapes are the same
//! — a code fence in a frame with the grammar's name on it, a list indented by
//! the column the model wrote it at, a table whose columns give way widest
//! first — and so is the reasoning for each, kept beside the code it explains.
//! What changed is the output: role-tagged [`Line`]s instead of ANSI strings
//! (see [`super::line`] for why), so every `paint(role, …)` there is a span with
//! that role here, and every reset that used to end a block's colour early has
//! nothing to correspond to.
//!
//! # Bounded body
//!
//! > There is a matching known-good shape from that same work: render the head of
//! > a long block as a title and the tail as a bounded body, with the buffer size
//! > configurable rather than hardcoded.
//!
//! [`RenderOptions::max_block_lines`] is that size, and it is a value the host
//! passes, not a constant in the middle of a function. A long block shows its
//! first line as a title, a count of what was elided, and its last N lines —
//! which is what a reader of a streaming model actually wants, because the
//! interesting end is the end.

use crate::style::Role;
use crate::syntax::{Lang, Stream};

use crate::render::{Line, Span, Style};

use super::line::{STRUCK, based_line, push_span, span, spans_width};
use super::parse::{Align, Block, InlineStyle, Run};
use super::wrap::{truncate, wrap};

/// How a reply is drawn, beyond the width it is drawn at.
///
/// Width is a separate argument everywhere because it is the one thing that
/// changes under a host every time the terminal does; these change when the
/// host decides something.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RenderOptions {
    /// The register the whole reply is drawn in: every row's own style, under
    /// its spans (see [`super::line::base`]).
    ///
    /// `None` is the top level. `Some(Role::Reasoning)` is a model's
    /// working-out, and it is the whole reason the field exists: in letibot a
    /// heading or an inline code span inside a themed block used to close to the
    /// *terminal default*, so the reasoning "tries to be grey, then goes green
    /// and becomes white for several rows and then grey again" — the operator's
    /// words, from looking at the screen. Here the register is under every span
    /// rather than re-opened after each, so that cannot happen.
    pub base: Option<Role>,
    /// Rows one block may occupy before it is summarised as a title, an elision
    /// count and its tail. `usize::MAX` disables the bound, and is the default:
    /// a library that elided by default would be hiding text nobody asked it to.
    pub max_block_lines: usize,
}

impl Default for RenderOptions {
    fn default() -> Self {
        RenderOptions {
            base: None,
            max_block_lines: usize::MAX,
        }
    }
}

/// The narrowest width a block is laid out at. Narrower than this a table's
/// columns and a list's indent leave nothing for the text; the rows overflow
/// instead, which a host clips.
const MIN_WIDTH: usize = 20;

/// The tab stop inside a fence: the diff renderers' stop, so a Go body indented
/// four deep in a diff is not eight deep in a reply.
const TAB_STOP: usize = crate::diff::TAB_STOP;

/// A code block's highlighter: a tree-sitter parse of the fence, painted with
/// [`crate::highlight`]'s syntax roles.
///
/// # The decision this encodes
///
/// A fence grows at its end, a few characters at a time, and the naive thing —
/// re-parse and repaint the whole block per frame — is the O(N²) letibot's old
/// hand-written lexer was built to avoid. That lexer paid for its incrementality
/// with coverage: ten languages, and heuristics (`Vec` is a type because it is
/// capitalised) that were wrong often enough to be a known cost. A [`Stream`]
/// parses the block with a grammar instead, which is 28 languages and no
/// guessing.
///
/// What it costs is a capture walk per repaint rather than per new line:
/// measured 2026-09-20 at ~0.1 µs per byte and ~1.1 µs per line, so ~470 µs for
/// a 5.4 KB Rust block. That is a frame's budget at fence sizes and it is why
/// this is a `Stream` fed the *delta* rather than a fresh parse — the parse is
/// incremental even though the walk is not. A fence long enough for the walk to
/// matter would need the same window discipline [`super::parse`] uses for the
/// conversation.
pub(crate) struct CodePaint {
    /// `None` when the fence named no language, or one there is no grammar for.
    /// The block is then drawn plain — a wrong colour is worse than none,
    /// because it invites the reader to trust it.
    stream: Option<Stream>,
    /// The language resolved, for the block's title bar.
    lang: Option<Lang>,
    /// The info string this was built for, as the fence spelled it. A streamed
    /// fence's info string arrives a few bytes at a time — "```" first, then
    /// "```ru", then "```rust" — so the highlighter built on the first frame was
    /// built for the wrong language. See [`CodePaint::is_for`].
    info: String,
    /// The fence's text as it has been seen, and how much of it has been pushed.
    /// `text` only ever grows at its end while the fence is open, so the delta
    /// is the new bytes and nothing else.
    text: String,
    pushed: usize,
}

// Hand-written for the same reason `IncrementalMarkdown`'s is: the parse state is
// a tree-sitter parser and tree, which have no `Debug` worth reading.
impl std::fmt::Debug for CodePaint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CodePaint")
            .field("lang", &self.lang.map(Lang::name))
            .field("text_len", &self.text.len())
            .field("pushed", &self.pushed)
            .finish()
    }
}

impl CodePaint {
    pub(crate) fn new(lang: &str) -> CodePaint {
        let resolved = Lang::from_token(lang);
        CodePaint {
            stream: resolved.map(Stream::new),
            lang: resolved,
            info: lang.to_string(),
            text: String::new(),
            pushed: 0,
        }
    }

    /// Hand over whatever is new.
    ///
    /// **Tabs become spaces HERE, before anything parses or measures.** A raw
    /// tab handed to the terminal is expanded at THAT terminal's stop —
    /// conventionally eight — while the width layer counts it as one column, so
    /// the row on the glass and the host's model of it were two different
    /// lines. The stop is the one both diff renderers use: a Go body indented
    /// four deep in the diff view must not be eight deep in a fence.
    ///
    /// Per LINE, and that is not a detail. `expand_tabs` computes each stop
    /// from the column the tab lands on, and a column carried across the `\n`
    /// would make the first tab of line 2 depend on how wide line 1 was.
    ///
    /// Here rather than in `lines()` is what covers every branch. The stream
    /// parses expanded text, so its spans are in the same coordinates as the
    /// rows painted from them; and the no-grammar fallback, which returns the
    /// text untouched, gets expanded text too. Fixing only the highlighted path
    /// is precisely how the unhighlightable fence stayed broken in letibot.
    ///
    /// The text only ever grows at its end while the fence is open, so the
    /// delta `pushed` tracks stays a prefix: expansion is a left-to-right fold,
    /// so the expansion of a prefix is a prefix of the expansion, whatever a
    /// later line adds.
    fn feed(&mut self, lines: &[String], closed: bool) {
        let mut src = lines
            .iter()
            .map(|l| crate::diff::expand_tabs(l, TAB_STOP))
            .collect::<Vec<_>>()
            .join("\n");
        if closed {
            src.push('\n');
        }
        // The delta is only a delta if what was pushed is still a prefix of the
        // fence. It is, for a fence growing at its end; it is not when the
        // parse window has moved a different fence under this block's index.
        // Then the parse starts again rather than appending one fence to
        // another and colouring the rows by a document nobody wrote.
        if !src.starts_with(&self.text[..self.pushed.min(self.text.len())]) {
            self.stream = self.lang.map(Stream::new);
            self.pushed = 0;
        }
        if src.len() > self.pushed {
            // A character boundary: what was pushed is a prefix of `src`.
            if let Some(stream) = self.stream.as_mut() {
                stream.push(&src[self.pushed..]);
            }
            self.pushed = src.len();
        }
        self.text = src;
    }

    /// Whether this highlighter was built for a fence with this info string.
    ///
    /// **Found while porting, and present in letibot:** the cache kept the
    /// highlighter it built on the first frame a fence appeared, and on that
    /// frame the model had usually written only the backticks. So the fence was
    /// highlighted as "no language" — plain — for the rest of its life,
    /// including after it settled, since the settled render reuses the same
    /// highlighter. A one-shot render of the same text was coloured, which is
    /// how a test comparing the two found it.
    pub(crate) fn is_for(&self, info: &str) -> bool {
        self.info == info
    }

    /// The fence's rows, as spans.
    ///
    /// The captures are character ranges, so a row is cut by character and each
    /// run is tagged with the role its capture name maps to. A run with no
    /// capture is plain: what is not drawn is the reader's own foreground, which
    /// is right for punctuation.
    fn lines(&mut self) -> Vec<Vec<Span>> {
        // A closed fence's text ends with the newline `feed` appended, and a
        // `split` on that gives a phantom empty row in the box. The rows a
        // *fence* has is what it was written with.
        let src = self.text.strip_suffix('\n').unwrap_or(&self.text);
        let rows: Vec<Vec<char>> = src.split('\n').map(|l| l.chars().collect()).collect();
        let plain = |chars: &[char]| {
            let mut v = Vec::new();
            push_span(&mut v, Span::raw(chars.iter().collect::<String>()));
            v
        };
        let Some(stream) = self.stream.as_mut() else {
            return rows.iter().map(|r| plain(r)).collect();
        };
        let spans = stream.spans();
        let mut out = Vec::with_capacity(rows.len());
        for (row, chars) in rows.iter().enumerate() {
            let mine: Vec<&crate::syntax::Span> = spans.iter().filter(|s| s.row == row).collect();
            if mine.is_empty() {
                out.push(plain(chars));
                continue;
            }
            let mut painted: Vec<Span> = Vec::new();
            let mut cursor = 0usize;
            for s in mine {
                let (start, end) = (s.start.min(chars.len()), s.end.min(chars.len()));
                if start < cursor {
                    // Overlaps a span already drawn; the earlier one won, which
                    // keeps the ordering rule from having to know what a name
                    // means. `Highlighter::classes` paints in the same order.
                    continue;
                }
                if cursor < start {
                    push_span(&mut painted, plain_text(&chars[cursor..start]));
                }
                // **Each capture its own span**, even beside one of the same role:
                // `assert_eq` and `!` are two captures and a host printing strings
                // writes them as two painted runs, which is what letibot's rows
                // always were. Coalescing them changes no cell, only the bytes a
                // string host emits, and those are what its frames are compared by.
                let text: String = chars[start..end].iter().collect();
                if !text.is_empty() {
                    painted.push(span(text, crate::highlight::role_for_capture(&s.name)));
                }
                cursor = end;
            }
            push_span(&mut painted, plain_text(&chars[cursor.min(chars.len())..]));
            out.push(painted);
        }
        out
    }

    /// How many times this block has been parsed. A frame that draws a settled
    /// fence must not re-parse it, so this stays at the number of deltas that
    /// arrived and does not grow with the number of frames.
    pub(crate) fn parses(&self) -> u64 {
        self.stream.as_ref().map(|s| s.parse_calls()).unwrap_or(0)
    }
}

fn plain_text(chars: &[char]) -> Span {
    Span::raw(chars.iter().collect::<String>())
}

/// Render one block to rows, unbounded.
///
/// One-shot: a fresh highlighter per call, which is right for a block that is
/// rendered once (a settled transcript row, a frozen prefix) and wrong for one
/// that is rendered every frame. [`super::view::MarkdownView`] is the second
/// case and keeps the highlighter between frames; the output of the two paths is
/// identical, because it is the same parser fed the same bytes in the same order.
pub fn render_block(b: &Block, width: usize, opts: &RenderOptions) -> Vec<Line> {
    let mut paint = code_paint_for(b);
    based(render_block_with(b, width, paint.as_mut()), opts)
}

/// Render a block, summarising it if it exceeds
/// [`RenderOptions::max_block_lines`].
///
/// The shape is title, elision count, tail. Never a silent truncation: the count
/// is the disclosure.
pub fn render_bounded(b: &Block, width: usize, opts: &RenderOptions) -> Vec<Line> {
    let mut paint = code_paint_for(b);
    render_bounded_with(b, width, opts, paint.as_mut())
}

/// A whole document: every block bounded, a blank row between blocks, none
/// trailing. The one-shot form of [`super::view::MarkdownView::lines`], for text
/// rendered once.
pub fn render_blocks<'a>(
    blocks: impl IntoIterator<Item = &'a Block>,
    width: usize,
    opts: &RenderOptions,
) -> Vec<Line> {
    let mut out = Vec::new();
    for b in blocks {
        out.extend(render_bounded(b, width, opts));
        out.push(blank(opts));
    }
    while out.last().is_some_and(Line::is_empty) {
        out.pop();
    }
    out
}

/// The row between two blocks. It carries the register too, so a host that
/// fills a row's background in it does not leave a gap in a reasoning pane.
pub(crate) fn blank(opts: &RenderOptions) -> Line {
    based_line(Vec::new(), opts.base)
}

pub(crate) fn code_paint_for(b: &Block) -> Option<CodePaint> {
    match b {
        Block::Code { lang, .. } => Some(CodePaint::new(lang)),
        _ => None,
    }
}

fn based(mut lines: Vec<Line>, opts: &RenderOptions) -> Vec<Line> {
    for l in &mut lines {
        l.style = based_line(Vec::new(), opts.base).style;
    }
    lines
}

pub(crate) fn render_bounded_with(
    b: &Block,
    width: usize,
    opts: &RenderOptions,
    code: Option<&mut CodePaint>,
) -> Vec<Line> {
    let limit = opts.max_block_lines;
    let full = render_block_with(b, width, code);
    if full.len() <= limit || limit < 3 {
        return based(full, opts);
    }
    let keep = limit - 2;
    let elided = full.len() - keep;
    let mut out = Vec::with_capacity(limit);
    let mut title = vec![span("▸ ", Role::Faint)];
    for s in truncate(&[span(b.title(), Role::Faint)], width) {
        push_span(&mut title, s);
    }
    out.push(Line::new(title));
    out.push(faint(format!("  … {elided} lines elided …")));
    out.extend(full[full.len() - keep..].iter().cloned());
    based(out, opts)
}

fn faint(s: impl Into<String>) -> Line {
    Line::new(vec![span(s, Role::Faint)])
}

fn render_block_with(b: &Block, width: usize, code: Option<&mut CodePaint>) -> Vec<Line> {
    let w = width.max(MIN_WIDTH);
    match b {
        // Coloured by level, with the hashes kept and de-emphasised.
        //
        // Both heads letibot surveyed drop the hashes and colour the text; that
        // is right while there is colour and wrong without it, because the level
        // is then unrecoverable — and no colour is a replay, a pipe and CI, not a
        // theme. So the hashes stay, faint, and carry the level for the
        // monochrome reader; the colour carries it for everyone else.
        Block::Heading { level, runs } => {
            let role = match level {
                1 => Role::Heading,
                2 => Role::Subheading,
                _ => Role::Strong,
            };
            let mut spans = vec![
                span("#".repeat(*level as usize), Role::Faint),
                Span::raw(" "),
            ];
            for s in runs_spans(runs, role) {
                push_span(&mut spans, s);
            }
            vec![Line::new(truncate(&spans, w))]
        }
        Block::Paragraph { lines } => rows(wrap(&runs_spans(&joined_runs(lines), Role::Plain), w)),
        Block::Code {
            lang,
            lines,
            closed,
        } => {
            let mut owned;
            let paint = match code {
                Some(p) => p,
                None => {
                    owned = CodePaint::new(lang);
                    &mut owned
                }
            };
            paint.feed(lines, *closed);
            let painted = paint.lines();
            let mut out = Vec::with_capacity(painted.len() + 2);
            // The fence's own info string when there is no grammar for it: naming
            // a language we are not colouring is honest, and inventing one we are
            // is not. A *recognised* one is named by the grammar that actually
            // ran, so the header cannot claim TypeScript over a fence that was
            // parsed as TSX.
            //
            // **And a fence the model did not label gets NO label at all** (R51
            // item 12). It used to say `code`, which is the head inventing a word
            // the author never wrote. Every other head surveyed names a language
            // it is not colouring; none of them manufactures one. The frame is
            // still drawn, so a bare block is visibly a block: `┌─` and `└─` with
            // nothing between them but the code.
            let head = match paint.lang {
                Some(l) => format!("┌─ {}", l.name()),
                None if !lang.is_empty() => format!("┌─ {lang}"),
                None => "┌─".to_string(),
            };
            out.push(faint(head));
            for l in painted {
                let mut spans = vec![span("│ ", Role::Faint)];
                spans.extend(l);
                out.push(Line::new(spans));
            }
            out.push(faint(if *closed {
                "└─"
            } else {
                "└─ (still writing…)"
            }));
            out
        }
        Block::List {
            ordered,
            start,
            items,
            indents,
        } => {
            let mut out = Vec::new();
            for (i, it) in items.iter().enumerate() {
                // **The item's own column** (§2.7). A sub-bullet renders one step
                // in from the item it belongs to, which is the whole of what a
                // nested list has to say. `0` when `indents` is short, which is a
                // block built by hand rather than by the parser.
                let nest = indents.get(i).copied().unwrap_or(0);
                // `·` rather than `•`, from grok-build: a bullet the same weight
                // as the prose competes with it down a long list, and what the
                // marker has to do is mark the indent, not be seen.
                // An ordered list's number is content — it is what the prose
                // refers back to — so it is not de-emphasised. A bullet is pure
                // structure and is.
                let (marker, marker_role) = if *ordered {
                    // `start + i`, not `i + 1`. A loose list — one whose items
                    // are separated by blank lines, which is what a model writes
                    // as soon as an item runs past a sentence — used to arrive as
                    // one block per item, and numbering from the index inside the
                    // block made every item of a six-point answer read `1.`.
                    (format!("{}. ", start + i), Role::Plain)
                } else {
                    ("· ".to_string(), Role::Faint)
                };
                // Columns, not bytes. `"· "` is two columns and three bytes, and
                // indenting a wrapped bullet by its byte length put every
                // continuation line a column too far right. The nest goes inside
                // this arithmetic rather than being prepended to the output row,
                // so a wrapped sub-item's continuation lines line up under its own
                // text instead of under its parent's.
                let pad = nest + spans_width(&[Span::raw(marker.as_str())]);
                let body = wrap(&runs_spans(it, Role::Plain), w.saturating_sub(pad));
                for (j, line) in body.into_iter().enumerate() {
                    let mut spans = Vec::new();
                    if j == 0 {
                        push_span(&mut spans, Span::raw(" ".repeat(nest)));
                        push_span(&mut spans, span(marker.as_str(), marker_role));
                    } else {
                        push_span(&mut spans, Span::raw(" ".repeat(pad)));
                    }
                    for s in line {
                        push_span(&mut spans, s);
                    }
                    out.push(Line::new(spans));
                }
            }
            out
        }
        // The whole quote is faint, rail and text: a quote is someone else's
        // words, set back from the answer's own.
        Block::Quote { lines } => wrap(
            &runs_spans(&joined_runs(lines), Role::Faint),
            w.saturating_sub(2),
        )
        .into_iter()
        .map(|l| {
            let mut spans = vec![span("│ ", Role::Faint)];
            for s in l {
                push_span(&mut spans, s);
            }
            Line::new(spans)
        })
        .collect(),
        Block::Table { head, align, rows } => table_lines(head, align, rows, w),
        Block::Rule => vec![faint("─".repeat(w.min(60)))],
    }
}

fn rows(v: Vec<Vec<Span>>) -> Vec<Line> {
    v.into_iter().map(Line::new).collect()
}

/// A pipe table, at the width the terminal actually has.
///
/// The operator, 2026-09-17: *"table rendering is broken"*. It was not rendered
/// at all — a table lexed as a paragraph, joined with spaces and wrapped as
/// prose. What a table owes its reader is the column, so:
///
/// - **Columns are as wide as their content wants, until they do not fit.** Then
///   the wide ones give way first (water-filling): a table of three short
///   columns and one long one shrinks the long one and leaves the others alone,
///   rather than taking an equal slice off each and truncating the short ones to
///   nothing.
/// - **A cell too narrow wraps, it does not get cut.** A row is as tall as its
///   tallest cell. Truncation would lose bytes the model wrote and a table is
///   most often where the numbers are.
/// - **No outer box.** The frame is one faint rule under the header and a faint
///   `│` between columns — the same weight as the quote rail and the code fence,
///   so a table sits in a turn rather than shouting from it.
fn table_lines(head: &[Vec<Run>], align: &[Align], rows: &[Vec<Vec<Run>>], w: usize) -> Vec<Line> {
    // The column count is the header's; a row with more cells than the header
    // has is showing something the header does not name, so the table widens to
    // it rather than dropping it.
    let cols = head
        .len()
        .max(rows.iter().map(Vec::len).max().unwrap_or(0))
        .max(1);
    fn cell(r: &[Vec<Run>], i: usize) -> &[Run] {
        r.get(i).map(Vec::as_slice).unwrap_or(&[])
    }

    let head_s: Vec<Vec<Span>> = (0..cols)
        .map(|i| runs_spans(cell(head, i), Role::Strong))
        .collect();
    let rows_s: Vec<Vec<Vec<Span>>> = rows
        .iter()
        .map(|r| {
            (0..cols)
                .map(|i| runs_spans(cell(r, i), Role::Plain))
                .collect()
        })
        .collect();

    let natural: Vec<usize> = (0..cols)
        .map(|i| {
            std::iter::once(spans_width(&head_s[i]))
                .chain(rows_s.iter().map(|r| spans_width(&r[i])))
                .max()
                .unwrap_or(0)
                .max(1)
        })
        .collect();

    // Three columns per gap: `" │ "`.
    let gaps = 3 * cols.saturating_sub(1);
    let available = w.saturating_sub(gaps).max(cols);
    let widths = fit_columns(&natural, available);

    let mut out = Vec::new();
    let push_row = |cells: &[Vec<Span>], out: &mut Vec<Line>| {
        // Wrap every cell to its column, then emit one screen line per wrapped
        // line, padding the cells that ran out.
        let wrapped: Vec<Vec<Vec<Span>>> = cells
            .iter()
            .zip(&widths)
            .map(|(c, wd)| {
                let v = wrap(c, *wd);
                if v.is_empty() { vec![Vec::new()] } else { v }
            })
            .collect();
        let height = wrapped.iter().map(Vec::len).max().unwrap_or(1);
        for line in 0..height {
            let mut row: Vec<Span> = Vec::new();
            for i in 0..cols {
                if i > 0 {
                    push_span(&mut row, span(" │ ", Role::Faint));
                }
                let text = wrapped[i].get(line).cloned().unwrap_or_default();
                pad(
                    &mut row,
                    text,
                    widths[i],
                    align.get(i).copied().unwrap_or(Align::Left),
                );
            }
            // The last column's padding is trailing whitespace on the screen and
            // in a copy-paste; the columns are already established by the ones
            // before it.
            trim_end(&mut row);
            out.push(Line::new(row));
        }
    };

    push_row(&head_s, &mut out);
    let rule: String = widths
        .iter()
        .map(|wd| "─".repeat(*wd))
        .collect::<Vec<_>>()
        .join("─┼─");
    out.push(faint(rule));
    for r in &rows_s {
        push_row(r, &mut out);
    }
    out
}

/// Give each column its natural width if they all fit; otherwise let the wide
/// ones give way first.
///
/// Water-filling: every column narrower than an equal share keeps what it
/// wants, and the slack they leave is shared again among the rest. An equal cut
/// instead would take the same columns off a two-character `n` column as off a
/// sixty-character `status` one, and the short columns are the ones that cannot
/// spare it.
fn fit_columns(natural: &[usize], available: usize) -> Vec<usize> {
    let n = natural.len();
    if n == 0 {
        return Vec::new();
    }
    if natural.iter().sum::<usize>() <= available {
        return natural.to_vec();
    }
    let mut widths = vec![0usize; n];
    let mut settled = vec![false; n];
    loop {
        let taken: usize = widths
            .iter()
            .zip(&settled)
            .filter(|(_, s)| **s)
            .map(|(w, _)| *w)
            .sum();
        let free = settled.iter().filter(|s| !**s).count();
        if free == 0 {
            break;
        }
        let share = available.saturating_sub(taken) / free;
        let mut moved = false;
        for i in 0..n {
            if !settled[i] && natural[i] <= share {
                widths[i] = natural[i];
                settled[i] = true;
                moved = true;
            }
        }
        if !moved {
            // Everything left wants more than its share. A floor of four
            // columns: narrower than that and a wrapped word is one letter per
            // line, which is not a table any more.
            for i in 0..n {
                if !settled[i] {
                    widths[i] = share.max(4);
                }
            }
            break;
        }
    }
    widths
}

fn pad(row: &mut Vec<Span>, text: Vec<Span>, width: usize, align: Align) {
    let slack = width.saturating_sub(spans_width(&text));
    let (left, right) = match align {
        Align::Left => (0, slack),
        Align::Right => (slack, 0),
        Align::Center => (slack / 2, slack - slack / 2),
    };
    push_span(row, Span::raw(" ".repeat(left)));
    for s in text {
        push_span(row, s);
    }
    push_span(row, Span::raw(" ".repeat(right)));
}

/// Drop trailing whitespace, across spans.
fn trim_end(row: &mut Vec<Span>) {
    while let Some(last) = row.last_mut() {
        let t = last.content.trim_end().len();
        last.content.truncate(t);
        if last.content.is_empty() {
            row.pop();
        } else {
            break;
        }
    }
}

/// Inline runs as spans inside a container drawn as `container`.
///
/// There is nothing to scan for here. The markdown projection parsed the inline
/// grammar, so `**bold**` arrived as a [`Run`] whose text is `bold` and whose
/// style is [`InlineStyle::Bold`] — the asterisks are already gone, and a `*`
/// that is not emphasis stays literal because the grammar said so. What is left
/// is the mapping from inline style to meaning.
///
/// The container's role is kept under the run's own: bold text in a quote is
/// still faint, bold in a heading is still the heading. A code span takes the
/// code role — its colour is what says "this is code" — and keeps the weight of
/// a heading or a table header it sits in, so it does not read as a dip in the
/// middle of one.
pub fn runs_spans(runs: &[Run], container: Role) -> Vec<Span> {
    let heavy = matches!(container, Role::Heading | Role::Subheading | Role::Strong);
    let mut out = Vec::new();
    for r in runs {
        let on = |s: Style, bold: bool, italic: bool, struck: bool| {
            let s = if bold { s.bold() } else { s };
            let mut s = if italic { s.italic() } else { s };
            if struck {
                s.attrs = s.attrs | STRUCK;
            }
            s
        };
        let style = match r.style {
            InlineStyle::Plain => Style::of(container),
            InlineStyle::Bold => on(Style::of(container), true, false, false),
            InlineStyle::Italic => on(Style::of(container), false, true, false),
            InlineStyle::BoldItalic => on(Style::of(container), true, true, false),
            // The container stays under the code role: in a quote the span is faint
            // cyan, as letibot's rows drew it, and not a full-strength island in a
            // set-back passage.
            InlineStyle::Code => on(Style::of(container).role(Role::Code), heavy, false, false),
            InlineStyle::Strikethrough => on(Style::of(container), false, false, true),
        };
        push_span(&mut out, Span::styled(r.text.as_str(), style));
    }
    out
}

/// One block's lines as one run list, joined with a plain space.
///
/// A paragraph's source lines are one paragraph; the renderer wraps the whole
/// thing to the display width, so the source's newlines are a wrap the model
/// did not mean.
pub fn joined_runs(lines: &[Vec<Run>]) -> Vec<Run> {
    let mut out: Vec<Run> = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        if i > 0 {
            out.push(Run {
                text: " ".to_string(),
                style: InlineStyle::Plain,
            });
        }
        out.extend(line.iter().cloned());
    }
    out
}

#[cfg(test)]
mod tests;
