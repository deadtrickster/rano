//! Blocks to terminal rows.
//!
//! The shapes are the same as the letibot renderer this was ported from — a
//! code fence in a frame with the grammar's name on it, a list indented by
//! the column the model wrote it at, a table whose columns give way widest
//! first — but the output is role-tagged [`Line`]s instead of ANSI strings.
//! [`RenderOptions::max_block_lines`] bounds a long block as a title, an
//! elision count and its last N lines, a size the host passes.

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
    /// its spans (see [`super::line::base`]). `None` is the top level;
    /// `Some(Role::Reasoning)` is a model's working-out. The register sits
    /// under every span rather than being re-opened after each, so a themed
    /// block cannot fall back to the terminal default mid-way.
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

/// A code block's highlighter: a tree-sitter parse of the fence, painted
/// with [`crate::highlight`]'s syntax roles. A fence grows at its end a few
/// characters at a time, so this is a [`Stream`] fed the delta rather than a
/// fresh parse — the parse is incremental even though the capture walk is
/// not. A fence long enough for the walk to matter would need the window
/// discipline [`super::parse`] uses for the conversation.
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

/// A fence labelled with a file and a line range — `path:N` or `path:N-M` as
/// the label's first word, the rest a note — answered as the path and the
/// first line. Such a block is a citation: its lines are the file's own, so
/// they carry the file's line numbers and are coloured by the file's
/// extension. The path must look like one (a `/` or a `.` in it), so
/// `rust:2` stays a language.
pub fn citation(info: &str) -> Option<(&str, u32)> {
    let word = info.split_whitespace().next()?;
    let (path, range) = word.rsplit_once(':')?;
    if path.is_empty() || !(path.contains('/') || path.contains('.')) {
        return None;
    }
    let first = range.split_once('-').map_or(range, |(a, _)| a);
    let start: u32 = first.parse().ok()?;
    if let Some((_, b)) = range.split_once('-') {
        let end: u32 = b.parse().ok()?;
        if end < start {
            return None;
        }
    }
    (start >= 1).then_some((path, start))
}

impl CodePaint {
    pub(crate) fn new(lang: &str) -> CodePaint {
        let resolved = match citation(lang) {
            Some((path, _)) => crate::syntax::detect(Some(std::path::Path::new(path)), None),
            None => Lang::from_token(lang),
        };
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
    /// Tabs become spaces here, before anything parses or measures: a raw
    /// tab handed to the terminal is expanded at that terminal's stop while
    /// the width layer counts one column. The stop is the diff renderers';
    /// per line, so the first tab of line 2 does not depend on how wide
    /// line 1 was; and here rather than in `lines()`, so every branch — the
    /// no-grammar fallback included — gets expanded text. The text only
    /// grows at its end while the fence is open, so the delta `pushed`
    /// tracks stays a prefix: expansion is a left-to-right fold, so the
    /// expansion of a prefix is a prefix of the expansion.
    fn feed(&mut self, lines: &[String], closed: bool) {
        let mut src = lines
            .iter()
            .map(|l| crate::diff::expand_tabs(l, TAB_STOP))
            .collect::<Vec<_>>()
            .join("\n");
        if closed {
            src.push('\n');
        }
        // The delta is only a delta if what was pushed is still a prefix of
        // the fence; when the parse window has moved a different fence under
        // this block's index, the parse starts again rather than appending
        // one fence to another.
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
    /// A fence's info string arrives a few bytes at a time — "```" first,
    /// then "```rust" — so a highlighter kept from the first frame was built
    /// for the wrong language and the fence stayed plain for its life.
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
        // No lines in, no rows out: an empty fence — the shape the citation
        // convention writes — must not draw a blank row, because a block
        // that says N lines in and draws N + 1 out lies about its contents.
        if src.is_empty() {
            return Vec::new();
        }
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
                // Each capture its own span, even beside one of the same
                // role: a host printing strings writes them as two painted
                // runs, and those bytes are what its frames are compared by.
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

/// Render one block to rows, unbounded. One-shot: a fresh highlighter per
/// call, right for a block rendered once; [`super::view::MarkdownView`]
/// keeps the highlighter between frames. The output of the two paths is
/// identical — the same parser fed the same bytes in the same order.
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
        // Both heads letibot surveyed drop the hashes and colour the text;
        // that is wrong without colour, because the level is then
        // unrecoverable — and no colour is a replay, a pipe, CI. So the
        // hashes stay, faint, and carry the level for the monochrome reader.
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
            // The fence's own info string when there is no grammar for it:
            // naming a language we are not colouring is honest, and
            // inventing one is not. A recognised one is named by the grammar
            // that actually ran. A fence the model did not label gets no
            // label at all — the frame is still drawn, so a bare block is
            // visibly a block.
            // A citation names its file and range, and its lines carry the
            // file's own numbers, so `pgdump_schema.go:242-248` reads as
            // lines 242 to 248 of that file.
            let cited = citation(lang);
            let head = match (cited, paint.lang) {
                (Some(_), _) => format!("┌─ {}", lang.trim()),
                (None, Some(l)) => format!("┌─ {}", l.name()),
                (None, None) if !lang.is_empty() => format!("┌─ {lang}"),
                (None, None) => "┌─".to_string(),
            };
            out.push(faint(head));
            // A citation with nothing in it yet (its host has not filled it) numbers nothing.
            let filled = lines.iter().any(|l| !l.trim().is_empty());
            let numbers = cited.filter(|_| filled).map(|(_, start)| {
                let last = start as usize + painted.len().saturating_sub(1);
                (start as usize, last.to_string().len())
            });
            for (i, l) in painted.into_iter().enumerate() {
                let mut spans = vec![span("│ ", Role::Faint)];
                if let Some((start, w)) = numbers {
                    spans.push(span(format!("{:>w$}  ", start + i), Role::Faint));
                }
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
                // `·` rather than `•`: a bullet the same weight as the prose
                // competes with it down a long list. An ordered list's number
                // is content — it is what the prose refers back to — so it is
                // not de-emphasised; a bullet is pure structure and is.
                let (marker, marker_role) = if *ordered {
                    // `start + i`, not `i + 1`: numbering from the index
                    // inside the block made every item of a six-point answer
                    // read `1.`.
                    (format!("{}. ", start + i), Role::Plain)
                } else {
                    ("· ".to_string(), Role::Faint)
                };
                // Columns, not bytes — `"· "` is two columns and three — and
                // the nest goes inside this arithmetic so a wrapped
                // sub-item's continuation lines line up under its own text.
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

/// A pipe table, at the width the terminal actually has. What a table owes
/// its reader is the column: columns are as wide as their content wants
/// until they do not fit, then the wide ones give way first (water-filling);
/// a cell too narrow wraps rather than getting cut, a row being as tall as
/// its tallest cell; and there is no outer box — one faint rule under the
/// header and a faint `│` between columns, the same weight as the quote
/// rail and the code fence.
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

/// Give each column its natural width if they all fit; otherwise let the
/// wide ones give way first. Water-filling: every column narrower than an
/// equal share keeps what it wants and the slack is shared among the rest —
/// an equal cut would take as much off a two-character `n` as off a
/// sixty-character `status`.
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

/// Inline runs as spans inside a container drawn as `container`. Nothing to
/// scan for here — the inline grammar already parsed the markers away; what
/// is left is the mapping from inline style to role. The container's role
/// stays under the run's own: bold in a quote is still faint, bold in a
/// heading is still the heading. A code span takes the code role and keeps
/// the weight of the container it sits in.
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
        // A link's label carries its destination, which the emitter writes as OSC 8.
        let style = match &r.link {
            Some(url) => style.link(url.as_str()),
            None => style,
        };
        push_span(&mut out, Span::styled(r.text.as_str(), style));
    }
    out
}

/// One block's lines as one run list, joined with a plain space: the
/// renderer wraps the whole thing to the display width, so the source's
/// newlines are a wrap the model did not mean.
pub fn joined_runs(lines: &[Vec<Run>]) -> Vec<Run> {
    let mut out: Vec<Run> = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        if i > 0 {
            out.push(Run {
                text: " ".to_string(),
                style: InlineStyle::Plain,
                link: None,
            });
        }
        out.extend(line.iter().cloned());
    }
    out
}

#[cfg(test)]
mod tests;
