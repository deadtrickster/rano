//! Incremental markdown for a document that only grows: what has settled
//! into complete blocks is frozen into `stable` and never parsed again; only
//! the still-growing tail is parsed, per delta.
//!
//! The bound is a window, not tree-sitter's incremental reuse: markdown's
//! external scanner blocks token reuse, so a push into one growing parser
//! re-lexes the whole document — quadratic over a stream. [`crate::syntax::Stream`]
//! is still the engine underneath, fed a bounded window.
//!
//! A block settles when a later block starts after it. A single block that
//! keeps growing never settles; `max_unfrozen` caps it, cutting the window at
//! the last [`stable_boundary`] inside it. `bytes_lexed` counts what was
//! handed to a parser, so a quadratic regression is answerable after the fact.

use crate::syntax::{Lang, Node, Stream};

/// One block. Line-oriented on purpose: a head renders lines, and a block
/// model finer than that is cost with no buyer. The text-bearing fields are
/// [`Run`]s, not `String`s — the inline grammar has already parsed bold,
/// italic, `code` and links, so the renderer need not scan for markers again.
/// `Code` keeps raw lines: its markers are source code, not emphasis.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Block {
    Heading {
        level: u8,
        runs: Vec<Run>,
    },
    Paragraph {
        lines: Vec<Vec<Run>>,
    },
    /// `closed` is false while the fence is still open — a head must render an
    /// unterminated code block without waiting.
    Code {
        lang: String,
        lines: Vec<String>,
        closed: bool,
    },
    List {
        ordered: bool,
        /// The number the first item was written with: numbering from the
        /// block index would restart every block of a loose list at `1.`
        start: usize,
        items: Vec<Vec<Run>>,
        /// How far each item is indented, in columns, parallel to `items`:
        /// the marker's own column, rounded to even and capped at 8.
        indents: Vec<usize>,
    },
    Quote {
        lines: Vec<Vec<Run>>,
    },
    /// A GFM pipe table. Held as cells, never as lines: the renderer decides
    /// the columns for the width it has.
    Table {
        head: Vec<Vec<Run>>,
        /// One per column of `head`, from the delimiter row.
        align: Vec<Align>,
        /// Ragged by construction: what the model wrote is what is shown.
        rows: Vec<Vec<Vec<Run>>>,
    },
    Rule,
}

/// A run of text with one inline style. The markers are gone: `**bold**`
/// is one `Run` whose text is `bold`; a `*` that is not emphasis stays
/// literal because the grammar said so.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Run {
    pub text: String,
    pub style: InlineStyle,
    /// Where this text links to, when it is a link's label. `None` for text.
    pub link: Option<String>,
}

/// The inline styles the conversation renderer knows. A link is drawn as
/// its label, with its destination on [`Run::link`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InlineStyle {
    Plain,
    Bold,
    Italic,
    /// `***both***` — nesting, not a third delimiter.
    BoldItalic,
    Code,
    Strikethrough,
}

impl InlineStyle {
    /// The style of text inside a container of this style. Bold and italic
    /// commute; beyond that the container being entered decides.
    fn nest(self, inner: InlineStyle) -> InlineStyle {
        match (self, inner) {
            (InlineStyle::Bold, InlineStyle::Italic) | (InlineStyle::Italic, InlineStyle::Bold) => {
                InlineStyle::BoldItalic
            }
            // Already both: a third marker does not add a fourth attribute.
            (InlineStyle::BoldItalic, _) => InlineStyle::BoldItalic,
            _ => inner,
        }
    }
}

/// What the delimiter row's colons asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Align {
    Left,
    Center,
    Right,
}

impl Block {
    /// A one-line name for the block.
    pub fn title(&self) -> String {
        match self {
            Block::Heading { runs, .. } => runs_text(runs),
            Block::Paragraph { lines } => lines.first().map(|l| runs_text(l)).unwrap_or_default(),
            Block::Code { lang, lines, .. } => {
                let lang = if lang.is_empty() { "code" } else { lang };
                format!("{lang} · {} lines", lines.len())
            }
            Block::List { items, .. } => items.first().map(|i| runs_text(i)).unwrap_or_default(),
            Block::Quote { lines } => lines.first().map(|l| runs_text(l)).unwrap_or_default(),
            Block::Table { head, rows, .. } => {
                format!("table · {} × {}", rows.len(), head.len())
            }
            Block::Rule => "───".into(),
        }
    }
}

/// The plain text of a run list, markers already gone.
pub fn runs_text(runs: &[Run]) -> String {
    let mut out = String::new();
    for r in runs {
        out.push_str(&r.text);
    }
    out
}

/// How large the unsettled window may grow before it is cut at a text
/// boundary — about a screen of prose, past which the worst single block
/// costs under half a millisecond to parse.
pub const DEFAULT_MAX_UNFROZEN: usize = 4 * 1024;

/// How many times one `push` may cut the window before giving up and rendering what
/// it has. Each round settles at least one byte off the front, so this is a guard
/// against a pathological input, not a limit a real message reaches.
const MAX_SETTLE_ROUNDS: usize = 8;

/// A markdown document that grows only at the end.
#[derive(Default)]
pub struct IncrementalMarkdown {
    raw: String,
    /// Byte offset into [`Self::raw`] where the unsettled window starts. Everything
    /// before it is in [`Self::stable`] and will never be parsed again.
    window_start: usize,
    stable: Vec<Block>,
    tail: Vec<Block>,
    max_unfrozen: usize,
    bytes_lexed: u64,
    lex_calls: u64,
}

impl IncrementalMarkdown {
    pub fn new() -> Self {
        IncrementalMarkdown {
            max_unfrozen: DEFAULT_MAX_UNFROZEN,
            ..IncrementalMarkdown::default()
        }
    }

    pub fn with_max_unfrozen(max_unfrozen: usize) -> Self {
        IncrementalMarkdown {
            max_unfrozen,
            ..IncrementalMarkdown::new()
        }
    }

    /// Append an increment. This is the only mutator: there is no
    /// `set_content` to call with the whole message.
    pub fn push(&mut self, delta: &str) {
        if delta.is_empty() {
            return;
        }
        self.raw.push_str(delta);
        self.refresh();
    }

    /// Blocks in order: the settled prefix, then the live tail.
    pub fn blocks(&self) -> impl Iterator<Item = &Block> {
        self.stable.iter().chain(self.tail.iter())
    }

    /// How many blocks have settled. A renderer caches exactly this many.
    pub fn stable_count(&self) -> usize {
        self.stable.len()
    }

    pub fn stable(&self) -> &[Block] {
        &self.stable
    }

    pub fn tail(&self) -> &[Block] {
        &self.tail
    }

    pub fn raw(&self) -> &str {
        &self.raw
    }

    pub fn is_empty(&self) -> bool {
        self.raw.is_empty()
    }

    /// Bytes handed to a parser over this document's life — the number a
    /// regression test watches: if it grows quadratically, so does the renderer.
    pub fn bytes_lexed(&self) -> u64 {
        self.bytes_lexed
    }

    pub fn lex_calls(&self) -> u64 {
        self.lex_calls
    }

    /// How long the unsettled window is, in bytes — assertable without timing.
    pub fn window_len(&self) -> usize {
        self.raw.len() - self.window_start
    }

    /// Bring the parse up to date with `raw`, then settle what has completed.
    /// The text guards ([`cut_point`]) say which `\n\n` boundaries are safe
    /// (two blocks can become one loose list), the fences ([`fences_in`])
    /// where a cut is not allowed at all; settling re-parses the prefix on
    /// its own, so its blocks are right by construction.
    fn refresh(&mut self) {
        for _ in 0..MAX_SETTLE_ROUNDS {
            let win = self.raw[self.window_start..].to_string();
            let (blocks, fences) = self.project();
            let Some(cut) = cut_point(&win, self.max_unfrozen, &fences) else {
                self.tail = blocks;
                return;
            };
            let prefix = parse_standalone(&win[..cut], &mut self.bytes_lexed, &mut self.lex_calls);
            self.stable.extend(prefix);
            self.window_start += cut;
            // Round again: the window is now smaller, and there is usually nothing more to
            // do because what is left is one trailing block.
        }
        // Unreachable for any real input: each round moves `window_start`
        // forward. Rendering the window as-is is the safe fallback.
        self.tail = self.project().0;
    }

    /// The window's blocks, and the byte ranges a cut may not land inside.
    fn project(&mut self) -> (Vec<Block>, Vec<(usize, usize)>) {
        let src = self.raw[self.window_start..].to_string();
        let fences = fences_in(&src);
        let (blocks, spans) =
            parse_blocks(&src, &fences, &mut self.bytes_lexed, &mut self.lex_calls);
        let _ = spans;
        (blocks, fence_spans(&src))
    }
}

/// The blocks of `src`, given the fences already found in it.
fn parse_blocks(
    src: &str,
    fences: &[Fence],
    bytes_lexed: &mut u64,
    lex_calls: &mut u64,
) -> (Vec<Block>, Vec<Span>) {
    let text = mask(src, fences);
    *bytes_lexed += text.len() as u64;
    *lex_calls += 1;
    let mut stream = Stream::new(Lang::Markdown);
    stream.push(&text);
    let Some(root) = stream.root() else {
        return (Vec::new(), Vec::new());
    };
    let spans = inline_spans(src, &root, bytes_lexed, lex_calls);
    (blocks_of(&root, src, &spans, fences), spans)
}

/// Parse a standalone span of markdown into blocks, both passes.
///
/// Used for a window prefix cut at a text boundary, and by [`lex`].
fn parse_standalone(src: &str, bytes_lexed: &mut u64, lex_calls: &mut u64) -> Vec<Block> {
    let fences = fences_in(src);
    parse_blocks(src, &fences, bytes_lexed, lex_calls).0
}

/// `src` with every fence blanked out, byte for byte: length and newlines unchanged, so a
/// node's byte offset still points into `src` even where [`slice`] must refuse the cut. A
/// fence the grammar would mis-delimit never reaches it (see [`fences_in`]).
fn mask(src: &str, fences: &[Fence]) -> String {
    if fences.is_empty() {
        return src.to_string();
    }
    let mut bytes = src.as_bytes().to_vec();
    for f in fences {
        let end = f.end.min(bytes.len());
        for b in &mut bytes[f.open.min(end)..end] {
            if *b != b'\n' {
                *b = b' ';
            }
        }
    }
    String::from_utf8(bytes).unwrap_or_else(|_| src.to_string())
}

/// The text at `a..b`, when that range is one this source can be cut at. Offsets come from a
/// tree over [`mask`]'s copy — same length, not the same text — so one can land inside a
/// character of `src`. Skipping that span costs its styling; slicing it costs the editor.
fn slice(src: &str, a: usize, b: usize) -> Option<&str> {
    src.get(a..b)
}

/// Lex a self-contained span of markdown into blocks: the one-shot form of
/// [`IncrementalMarkdown`], same engine and model.
pub fn lex(s: &str) -> Vec<Block> {
    let mut bytes = 0;
    let mut calls = 0;
    parse_standalone(s, &mut bytes, &mut calls)
}

/// The images a document names — `![alt](target)` — as `(alt, target)`, in
/// order; the rendered rows carry only the alt text. Local targets only:
/// neither renderer reaches the network.
pub fn images(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(at) = rest.find("![") {
        rest = &rest[at + 2..];
        let Some(close) = rest.find("](") else { break };
        // The alt text is one line; a `![` whose `](` is paragraphs away is not an image.
        if rest[..close].contains('\n') {
            continue;
        }
        let alt = rest[..close].trim().to_string();
        rest = &rest[close + 2..];
        let Some(end) = rest.find(')') else { break };
        // `(path "title")`: the title is not part of the path.
        let target = rest[..end].split(" \"").next().unwrap_or("").trim();
        let target = target.trim_start_matches('<').trim_end_matches('>');
        if !target.is_empty() && !target.contains("://") && !target.contains('\n') {
            out.push((alt, target.to_string()));
        }
        rest = &rest[end + 1..];
    }
    out
}

// ---------------------------------------------------------------------------
// The inline pass
// ---------------------------------------------------------------------------

/// A style over a byte range, before it is cut into lines. `style: None` is
/// deliberately not text — a marker, a link destination. The walk covers
/// every byte, so no uncovered stretch is left to guess about.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Span {
    start: usize,
    end: usize,
    style: Option<InlineStyle>,
    /// The destination of the link this text is the label of.
    link: Option<std::sync::Arc<str>>,
}

/// Run the inline grammar over the parts of `root` that hold inline content,
/// one parse per range: tree-sitter concatenates included ranges, so one
/// parse over all of them would let a delimiter pair across ranges. A range
/// holding nothing that can start a construct is skipped.
fn inline_spans(src: &str, root: &Node, bytes_lexed: &mut u64, lex_calls: &mut u64) -> Vec<Span> {
    let mut spans = Vec::new();
    for (a, b) in inline_ranges(root) {
        // A range can be stale — [`slice`] is where the whole story is, and every site that
        // slices `src` by a tree offset now goes through it.
        let Some(text) = slice(src, a, b) else {
            continue;
        };
        if !has_inline_syntax(text) {
            continue;
        }
        let mut stream = Stream::new(Lang::MarkdownInline);
        stream.push(text);
        *bytes_lexed += text.len() as u64;
        *lex_calls += 1;
        let Some(root) = stream.root() else {
            continue;
        };
        let mut local = Vec::new();
        collect_spans(&root, text, InlineStyle::Plain, &mut local);
        // Offsets are relative to the slice; the rest of this module works in offsets
        // into the whole document.
        for mut s in local {
            s.start += a;
            s.end += a;
            spans.push(s);
        }
    }
    spans
}

/// Whether `text` holds anything the inline grammar could read as a
/// construct; if none, the range can be left alone. `&` is not in the set —
/// an entity reference shows as written either way.
fn has_inline_syntax(text: &str) -> bool {
    text.bytes()
        .any(|b| matches!(b, b'`' | b'*' | b'_' | b'~' | b'[' | b'<' | b'\\' | b'!'))
}

/// The byte ranges that are inline content, with markdown's own block markers
/// removed: every `inline` and `pipe_table_cell` node, split around its
/// `block_continuation` children (a quote's second line keeps its `> `, which
/// the inline grammar would render literally).
fn inline_ranges(node: &Node) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    fn walk(n: &Node, out: &mut Vec<(usize, usize)>) {
        if n.kind == "inline" || n.kind == "pipe_table_cell" {
            push_around_continuations(n, out);
            return;
        }
        for c in &n.children {
            walk(c, out);
        }
    }
    walk(node, &mut out);
    out
}

/// A node's range, split around its `block_continuation` children.
fn push_around_continuations(n: &Node, out: &mut Vec<(usize, usize)>) {
    let mut cursor = n.start;
    for c in &n.children {
        if c.kind != "block_continuation" {
            continue;
        }
        if cursor < c.start {
            out.push((cursor, c.start));
        }
        cursor = cursor.max(c.end);
    }
    if cursor < n.end {
        out.push((cursor, n.end));
    }
}

/// Walk an inline tree, emitting a [`Span`] per styled piece of text. The
/// rule is gaps are text — this grammar puts the content of `**bold**` in no
/// node at all, so recursing over children alone would produce literal
/// asterisks. Markers and link destinations are named nodes, dropped by kind.
fn collect_spans(node: &Node, src: &str, style: InlineStyle, out: &mut Vec<Span>) {
    match node.kind.as_str() {
        // Markers and link targets: covered, and not text.
        "emphasis_delimiter"
        | "code_span_delimiter"
        | "link_destination"
        | "link_title"
        | "link_label" => dropped(node, out),
        // A code span's content is raw: the inline grammar does not parse inside it,
        // so the whole interior is one span and the backticks are gone.
        "code_span" => {
            dropped(node, out);
            if let Some((a, b)) = code_span_inner(node) {
                out.push(Span {
                    start: a,
                    end: b,
                    style: Some(InlineStyle::Code),
                    link: None,
                });
            }
        }
        // A hard break is the newline the model meant; the marker that made it hard —
        // two trailing spaces, or a backslash — goes, and the break stays.
        "hard_line_break" => {
            let end = node.end;
            if end > node.start && src.as_bytes().get(end - 1) == Some(&b'\n') {
                out.push(Span {
                    start: node.start,
                    end: end - 1,
                    style: None,
                    link: None,
                });
                out.push(Span {
                    start: end - 1,
                    end,
                    style: Some(style),
                    link: None,
                });
            } else {
                dropped(node, out);
            }
        }
        // An autolink shows its URL and not the angle brackets that made it a link:
        // the whole node is covered first, then the interior on top of it.
        "uri_autolink" | "email_autolink" => {
            dropped(node, out);
            if node.end > node.start + 1 && src.as_bytes().get(node.start) == Some(&b'<') {
                out.push(Span {
                    start: node.start + 1,
                    end: node.end - 1,
                    style: Some(style),
                    link: None,
                });
            }
        }
        "emphasis" => gaps(node, src, style.nest(InlineStyle::Italic), out),
        "strong_emphasis" => gaps(node, src, style.nest(InlineStyle::Bold), out),
        "strikethrough" => gaps(node, src, InlineStyle::Strikethrough, out),
        // Only the label is shown; the destination and title were dropped above. The
        // label's own text is its gap, so it gets the same walk.
        "inline_link"
        | "collapsed_reference_link"
        | "full_reference_link"
        | "shortcut_link"
        | "image" => {
            // The destination, when the link writes one inline — what its label links to.
            let dest: Option<std::sync::Arc<str>> = (node.kind == "inline_link")
                .then(|| node.children.iter().find(|c| c.kind == "link_destination"))
                .flatten()
                .and_then(|d| slice(src, d.start, d.end))
                .map(|d| d.trim_start_matches('<').trim_end_matches('>').into());
            for c in &node.children {
                if c.kind == "link_text" || c.kind == "image_description" {
                    let from = out.len();
                    gaps(c, src, style, out);
                    if let Some(d) = &dest {
                        for s in out[from..].iter_mut().filter(|s| s.style.is_some()) {
                            s.link = Some(d.clone());
                        }
                    }
                } else {
                    dropped(c, out);
                }
            }
        }
        _ => gaps(node, src, style, out),
    }
}

/// Mark a node's whole range as not text.
fn dropped(node: &Node, out: &mut Vec<Span>) {
    if node.end > node.start {
        out.push(Span {
            start: node.start,
            end: node.end,
            style: None,
            link: None,
        });
    }
}

/// The text between `node`'s children, at `style`, with each child deciding for
/// itself. Covers every byte of the node.
fn gaps(node: &Node, src: &str, style: InlineStyle, out: &mut Vec<Span>) {
    let mut cursor = node.start;
    for c in &node.children {
        if cursor < c.start {
            out.push(Span {
                start: cursor,
                end: c.start,
                style: Some(style),
                link: None,
            });
        }
        collect_spans(c, src, style, out);
        cursor = cursor.max(c.end);
    }
    if cursor < node.end {
        out.push(Span {
            start: cursor,
            end: node.end,
            style: Some(style),
            link: None,
        });
    }
}

/// The interior of a `` `code` `` span: past the opening delimiter run and before the
/// closing one.
fn code_span_inner(node: &Node) -> Option<(usize, usize)> {
    let mut delims = node
        .children
        .iter()
        .filter(|c| c.kind == "code_span_delimiter");
    let first = delims.next()?;
    let last = delims.next_back().unwrap_or(first);
    Some((first.end, last.start.max(first.end)))
}

// ---------------------------------------------------------------------------
// The block pass
// ---------------------------------------------------------------------------

/// A fenced code block found in the text.
#[derive(Debug, Clone)]
struct Fence {
    /// Offset of the opening delimiter — the line's start ordinarily, or the
    /// run itself when a model puts it after prose. Everything before it on
    /// that line stays prose: [`mask`] blanks from here.
    open: usize,
    /// The container syntax the opening line carries — `"> "` in a quote, an item's indent
    /// in a list, `""` at the top level. Every line of the block repeats it.
    prefix: String,
    /// Offset of the body: just after the opening line's newline.
    body: usize,
    /// Offset of the closing line's start, or the end of the text when there is none.
    close: usize,
    /// Offset just after the closing line.
    end: usize,
    /// The info string, trimmed.
    lang: String,
    /// Whether a closing fence was found.
    closed: bool,
}

/// Every fenced code block in `src`, in order. This is not read off the tree:
/// `tree-sitter-md` accepts a run of backticks anywhere on a line as a closing
/// fence, where CommonMark requires a line of its own — so the extent of a
/// fence is computed here, from the text.
fn fences_in(src: &str) -> Vec<Fence> {
    let mut out = Vec::new();
    let mut off = 0;
    while off < src.len() {
        let line_end = match src[off..].find('\n') {
            Some(i) => off + i,
            None => src.len(),
        };
        let line = &src[off..line_end];
        let (prefix, marker) = split_container(line);
        let body = (line_end + 1).min(src.len());
        let column = off + (line.len() - marker.len());
        if let Some(open) = opening_run(marker, src, body) {
            let (at, ch, n) = (open.at, open.ch, open.n);
            // The label ends at the next run of the same character — a title
            // is never a delimiter.
            let label_end = open.trail.map_or(marker.len(), |(j, _)| j);
            let lang = marker[at + n..label_end].trim().to_string();
            // A trailing run at least as long as the opening one closes the
            // fence on the same line: ` ```src/a.rs:3-9``` ` is an empty
            // citation, which is what the harness convention means by it.
            let same_line = open
                .trail
                .filter(|&(_, len)| len >= n)
                .map(|(j, len)| (column + j, column + j + len));
            let (body, close, end, closed) = match same_line {
                Some((start, after)) => (start, start, after, true),
                None => match closing_fence(src, body, ch, n) {
                    Some((start, after)) => (body, start, after, true),
                    None => (body, src.len(), src.len(), false),
                },
            };
            out.push(Fence {
                // The line's start when the run starts the content column — the container syntax
                // in front of it is not prose — and the run itself when prose precedes it.
                open: if at == 0 { off } else { column + at },
                prefix: prefix.to_string(),
                body,
                close,
                end,
                lang,
                closed,
            });
            off = end.max(line_end + 1);
            continue;
        }
        off = line_end + 1;
    }
    out
}

/// Where on a line's content a fence opens, and where its label stops.
struct Opening {
    /// Offset of the delimiter run within the line's content.
    at: usize,
    /// The delimiter byte.
    ch: u8,
    /// How many of them.
    n: usize,
    /// The next run of the same character on this line, if there is one — `(offset, length)`.
    /// The label stops there, and a run at least as long as the opening one closes the fence.
    trail: Option<(usize, usize)>,
}

/// Where on a line's content a fence opens. A run that starts the line's
/// content is CommonMark's fence, open whether or not its closing delimiter
/// has arrived. A run after prose is not a fence to CommonMark, but reading
/// it as prose lets the closing delimiter open a block that never closes, so
/// such a run is a fence when the document closes it and prose otherwise.
/// The label stops at the next run of the same character; a second run after
/// prose is an inline code span, not a fence.
fn opening_run(marker: &str, src: &str, body: usize) -> Option<Opening> {
    let bytes = marker.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let Some(&ch @ (b'`' | b'~')) = bytes.get(i) else {
            i += 1;
            continue;
        };
        let n = bytes[i..].iter().take_while(|b| **b == ch).count();
        if n < 3 {
            i += n;
            continue;
        }
        let rest = &bytes[i + n..];
        let trail = rest.iter().position(|b| *b == ch).map(|j| {
            (
                i + n + j,
                rest[j..].iter().take_while(|b| **b == ch).count(),
            )
        });
        if i == 0 {
            return Some(Opening {
                at: 0,
                ch,
                n,
                trail,
            });
        }
        // After prose: not glued to a word, the line's only run, and paid
        // off by a closing delimiter that belongs to it — a sentence that
        // happens to end in backticks read as a fence loses the reply's
        // second half. `a```b` is inline code, not a fence.
        let word = i > 0 && !bytes[i - 1].is_ascii_alphanumeric();
        if word
            && trail.is_none()
            && closing_fence(src, body, ch, n)
                .is_some_and(|(close, _)| !swallows_a_fence(src, body, close))
        {
            return Some(Opening {
                at: i,
                ch,
                n,
                trail: None,
            });
        }
        i += n;
    }
    None
}

/// Whether the text from `body` to `close` holds a line that opens a fence
/// of its own. A mid-line run is a guess, and a guess whose closing delimiter
/// is another fence's is the wrong one: the paragraph would eat two blocks.
fn swallows_a_fence(src: &str, body: usize, close: usize) -> bool {
    let mut off = body;
    while off < close && off < src.len() {
        let line_end = match src[off..].find('\n') {
            Some(i) => off + i,
            None => src.len(),
        };
        let (_, marker) = split_container(&src[off..line_end]);
        if let Some(&ch @ (b'`' | b'~')) = marker.as_bytes().first()
            && marker.bytes().take_while(|b| *b == ch).count() >= 3
        {
            return true;
        }
        off = line_end + 1;
    }
    false
}

/// Split a line into the container syntax it carries and what follows:
/// `"> "` for a fence inside a quote, blanks of a list marker's width for one
/// after a bullet, `""` for a plain line. The prefix is the content column,
/// what every line of the block repeats; a marker comes back as blanks
/// because it does not repeat on continuations. [`closing_fence`] strips it
/// to recognise a close, [`fence_lines`] to get at the code.
fn split_container(line: &str) -> (String, &str) {
    let mut prefix = String::new();
    let mut cut = 0;
    loop {
        let rest = &line[cut..];
        let ws = rest.len() - rest.trim_start_matches([' ', '\t']).len();
        prefix.push_str(&rest[..ws]);
        cut += ws;
        if line[cut..].starts_with('>') {
            prefix.push('>');
            cut += 1;
            continue;
        }
        if let Some(len) = list_marker_len(&line[cut..]) {
            prefix.push_str(&" ".repeat(len));
            cut += len;
            continue;
        }
        break;
    }
    (prefix, &line[cut..])
}

/// The byte length of the list marker at the start of `s` — `"- "`, `"1. "` — or `None`.
///
/// The trailing space is part of it, because that is what separates the marker from the
/// item's content and so what decides the content column.
fn list_marker_len(s: &str) -> Option<usize> {
    for m in ["- ", "* ", "+ "] {
        if s.starts_with(m) {
            return Some(m.len());
        }
    }
    let digits = s.bytes().take_while(|b| b.is_ascii_digit()).count();
    if digits > 0 && digits <= 9 {
        let rest = &s[digits..];
        if rest.starts_with(". ") || rest.starts_with(") ") {
            return Some(digits + 2);
        }
    }
    None
}

/// The closing fence of a block opened with `n` of `ch`: `(its line's start,
/// offset just after it)`. Each candidate line has its own container syntax
/// stripped before the test — a list marker does not repeat, so the closing
/// line's prefix differs from the opening one's.
fn closing_fence(src: &str, from: usize, ch: u8, n: usize) -> Option<(usize, usize)> {
    let mut off = from;
    while off < src.len() {
        let line_end = match src[off..].find('\n') {
            Some(i) => off + i,
            None => src.len(),
        };
        let (_, marker) = split_container(&src[off..line_end]);
        let t = marker.trim();
        if t.len() >= n && t.bytes().all(|b| b == ch) {
            return Some((off, (line_end + 1).min(src.len())));
        }
        off = line_end + 1;
    }
    None
}

/// A fence's code lines, with the block's own prefix stripped from each —
/// not whatever each line happens to start with: a code line that
/// legitimately begins with `"> "` inside a top-level fence must keep it.
fn fence_lines(src: &str, f: &Fence) -> Vec<String> {
    let body = &src[f.body..f.close];
    let mut lines: Vec<String> = body
        .split('\n')
        .map(|l| {
            // The content column, or — for a line that does not carry it, which a list
            // item's continuation need not — its own leading whitespace.
            match l.strip_prefix(f.prefix.as_str()) {
                Some(rest) => rest.to_string(),
                None => l.trim_start().to_string(),
            }
        })
        .collect();
    // `"a\n"` splits to `["a", ""]`; the empty tail is the newline, not a line.
    if lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }
    lines
}

/// The byte ranges a cut may not land inside: one per fence, opening to
/// closing delimiter — or to the window's end when there is none. Both ends
/// are allowed; the middle splits one code block in two. An unclosed fence
/// reaching the window's end keeps a settle out of an open code block.
fn fence_spans(src: &str) -> Vec<(usize, usize)> {
    fences_in(src).iter().map(|f| (f.open, f.end)).collect()
}

/// Is `at` inside a fence rather than at one of its ends?
fn inside_a_fence(fences: &[(usize, usize)], at: usize) -> bool {
    fences.iter().any(|&(s, e)| at > s && at < e)
}

/// Map the parsed tree's block children to blocks, with the fences in their
/// place: the grammar wraps the document in `document`/`section` containers,
/// and the fences are not in the tree at all ([`mask`] blanked them), so the
/// two lists are merged by offset.
fn blocks_of(root: &Node, src: &str, spans: &[Span], fences: &[Fence]) -> Vec<Block> {
    let mut tree: Vec<(usize, Block)> = Vec::new();
    collect_blocks(root, src, spans, fences, &mut tree);
    let mut out = Vec::new();
    let mut it = tree.into_iter().peekable();
    for f in fences {
        while it.peek().is_some_and(|(start, _)| *start < f.open) {
            out.push(it.next().unwrap().1);
        }
        out.push(Block::Code {
            lang: f.lang.clone(),
            lines: fence_lines(src, f),
            closed: f.closed,
        });
    }
    out.extend(it.map(|(_, b)| b));
    out
}

/// Containers the block grammar wraps blocks in. They carry no text of their own.
fn is_container(kind: &str) -> bool {
    matches!(kind, "document" | "section")
}

fn collect_blocks(
    node: &Node,
    src: &str,
    spans: &[Span],
    fences: &[Fence],
    blocks: &mut Vec<(usize, Block)>,
) {
    for c in &node.children {
        if !c.named {
            continue;
        }
        if is_container(&c.kind) {
            collect_blocks(c, src, spans, fences, blocks);
            continue;
        }
        // A block reported *inside* a fence is an artefact of the grammar having glued
        // itself back together around one; the fence is emitted from the scan.
        if fences.iter().any(|f| c.start >= f.open && c.start < f.end) {
            continue;
        }
        if let Some(block) = block_of(c, src, spans) {
            blocks.push((c.start, block));
        }
    }
}

fn block_of(node: &Node, src: &str, spans: &[Span]) -> Option<Block> {
    match node.kind.as_str() {
        "atx_heading" | "setext_heading" => {
            let level = heading_level(node);
            let runs = content_ranges(node)
                .first()
                .map(|&(a, b)| single_line(src, spans, a, b))
                .unwrap_or_default();
            Some(Block::Heading { level, runs })
        }
        "paragraph" => Some(Block::Paragraph {
            lines: lines_of_node(node, src, spans),
        }),
        "block_quote" => {
            let mut lines = Vec::new();
            subtree_lines(node, src, spans, &mut lines);
            if lines.is_empty() {
                lines.push(Vec::new());
            }
            Some(Block::Quote { lines })
        }
        // Only an indented block can reach here: a *fenced* one was masked away before the
        // parse, and `collect_blocks` emits it from the scan.
        "indented_code_block" => Some(code_block(node, src)),
        "list" => Some(list_block(node, src, spans)),
        "pipe_table" => Some(table_block(node, src, spans)),
        "thematic_break" => Some(Block::Rule),
        // Raw HTML and anything the grammar grew that this projection does not know:
        // shown as the text it is rather than dropped. A head that silently loses a
        // block is worse than one that shows it unstyled.
        _ => {
            let text = slice(src, node.start, node.end)?;
            if text.trim().is_empty() {
                return None;
            }
            Some(Block::Paragraph {
                lines: text
                    .lines()
                    .map(|l| vec![run(l.to_string(), InlineStyle::Plain)])
                    .collect(),
            })
        }
    }
}

fn heading_level(node: &Node) -> u8 {
    for c in &node.children {
        match c.kind.as_str() {
            "atx_h1_marker" | "setext_h1_underline" => return 1,
            "atx_h2_marker" | "setext_h2_underline" => return 2,
            "atx_h3_marker" => return 3,
            "atx_h4_marker" => return 4,
            "atx_h5_marker" => return 5,
            "atx_h6_marker" => return 6,
            _ => {}
        }
    }
    1
}

/// The ranges of the `inline` nodes under a block node, in document order —
/// the block's text, which is not the block's own range: `### title` starts
/// at the hashes and a list item starts at its marker.
fn content_ranges(node: &Node) -> Vec<(usize, usize)> {
    inline_ranges(node)
}

/// Every line of a block's inline content. One inline node covers several
/// source lines, so the text is cut at `\n` into the lines the renderer
/// wraps; a shared accumulator, because a quote's content arrives as two
/// ranges with the `> ` cut out.
fn lines_of_node(node: &Node, src: &str, spans: &[Span]) -> Vec<Vec<Run>> {
    let mut out = Vec::new();
    lines_of_ranges(src, spans, &content_ranges(node), &mut out);
    if out.is_empty() {
        out.push(Vec::new());
    }
    out
}

/// A block's reader-facing lines, whatever kind of blocks it holds:
/// [`lines_of_node`] sees only inline content, so a quote holding a fenced
/// block would come out empty. A quoted fence is therefore quote prose —
/// the flat model has no room for a code box in a quote.
fn subtree_lines(node: &Node, src: &str, spans: &[Span], out: &mut Vec<Vec<Run>>) {
    for c in &node.children {
        match c.kind.as_str() {
            // Structure and link targets: not text.
            "block_quote_marker"
            | "block_continuation"
            | "list_marker_minus"
            | "list_marker_plus"
            | "list_marker_star"
            | "list_marker_dot"
            | "list_marker_parenthesis"
            | "task_list_marker_checked"
            | "task_list_marker_unchecked"
            | "fenced_code_block_delimiter"
            | "info_string"
            | "link_destination"
            | "link_title"
            | "link_label" => {}
            // Text, with inline styling.
            "inline" => lines_of_ranges(src, spans, &inline_ranges(c), out),
            // No arm for `fenced_code_block` or `code_fence_content`: after
            // [`mask`] the tree holds neither, and a fence that belongs to a
            // container is emitted from the scan, so a container reaching
            // here holds prose only.
            // A nested table: one line per row, cells joined. The flat model
            // has no columns to give it here.
            "pipe_table" => {
                let rows = c
                    .children
                    .iter()
                    .filter(|g| g.kind == "pipe_table_header" || g.kind == "pipe_table_row");
                for row in rows {
                    let mut line: Vec<Run> = Vec::new();
                    for cell in row.children.iter().filter(|g| g.kind == "pipe_table_cell") {
                        if !line.is_empty() {
                            push_text(&mut line, " ", InlineStyle::Plain, None);
                        }
                        let (a, b) = trim_range(src, cell.start, cell.end);
                        if let Some(t) = slice(src, a, b) {
                            push_text(&mut line, t, InlineStyle::Plain, None);
                        }
                    }
                    if !line.is_empty() {
                        out.push(line);
                    }
                }
            }
            _ => subtree_lines(c, src, spans, out),
        }
    }
}

/// The lines of a run of ranges, cut at `\n`, appended to `out`.
fn lines_of_ranges(src: &str, spans: &[Span], ranges: &[(usize, usize)], out: &mut Vec<Vec<Run>>) {
    let mut cur: Vec<Run> = out.pop().unwrap_or_default();
    for &(a, b) in ranges {
        for r in runs_of(src, spans, a, b) {
            for (i, piece) in r.text.split('\n').enumerate() {
                if i > 0 {
                    out.push(std::mem::take(&mut cur));
                }
                if !piece.is_empty() {
                    let mut p = run(piece.to_string(), r.style);
                    p.link = r.link.clone();
                    cur.push(p);
                }
            }
        }
    }
    out.push(cur);
}

/// One list item's text from its body nodes, folded into the single run list
/// the model has room for. `subtree_lines_one` per node: the fallback for a
/// detached leaf has to apply to each node, not to the item as a whole.
fn item_runs(body: &[Node], src: &str, spans: &[Span]) -> Vec<Run> {
    let mut lines = Vec::new();
    for n in body {
        subtree_lines_one(n, src, spans, &mut lines);
    }
    let mut out: Vec<Run> = Vec::new();
    for (i, line) in lines.into_iter().enumerate() {
        if i > 0 {
            push_text(&mut out, " ", InlineStyle::Plain, None);
        }
        for r in line {
            push_text(&mut out, &r.text, r.style, r.link.as_deref());
        }
    }
    out
}

/// [`subtree_lines`] for a single detached node: the node's own contribution, then
/// each of its children's.
fn subtree_lines_one(node: &Node, src: &str, spans: &[Span], out: &mut Vec<Vec<Run>>) {
    let before = out.len();
    subtree_lines(node, src, spans, out);
    // A node with no children of its own contributes its whole text — the case a
    // detached leaf (a `paragraph` stripped of its `inline`) falls into.
    if out.len() == before {
        let text = text_without_continuations(node, src);
        for line in text.trim_end_matches('\n').lines() {
            out.push(vec![run(line.to_string(), InlineStyle::Plain)]);
        }
    }
}

/// A code block from the tree: an indented one, four spaces and no fence.
/// The only kind that reaches the tree — a fenced block is masked away and
/// emitted by `collect_blocks` from the scan.
fn code_block(node: &Node, src: &str) -> Block {
    let mut lines: Vec<String> = text_without_continuations(node, src)
        .trim_end_matches('\n')
        .lines()
        .map(str::to_string)
        .collect();
    // An indented block has no delimiters, and CommonMark runs it to the end of the input
    // like a fence does at EOF — so it is never "still being written".
    if lines.last().is_some_and(|l| l.trim().is_empty()) {
        lines.pop();
    }
    Block::Code {
        lang: String::new(),
        lines,
        closed: true,
    }
}

/// A node's text with its `block_continuation` children cut out.
fn text_without_continuations(node: &Node, src: &str) -> String {
    let mut out = String::new();
    let mut cursor = node.start;
    for c in &node.children {
        if c.kind != "block_continuation" {
            continue;
        }
        if cursor < c.start
            && let Some(t) = slice(src, cursor, c.start)
        {
            out.push_str(t);
        }
        cursor = cursor.max(c.end);
    }
    if cursor < node.end
        && let Some(t) = slice(src, cursor, node.end)
    {
        out.push_str(t);
    }
    out
}

fn list_block(node: &Node, src: &str, spans: &[Span]) -> Block {
    let mut items = Vec::new();
    let mut indents = Vec::new();
    let mut ordered = false;
    let mut start = 1;
    let mut first = true;
    collect_items(
        node,
        src,
        spans,
        &mut items,
        &mut indents,
        &mut ordered,
        &mut start,
        &mut first,
    );
    Block::List {
        ordered,
        start,
        items,
        indents,
    }
}

/// Flatten a list's items, nested lists included, each with the column it
/// was written at, rounded to even and capped at 8.
// A recursive walk that fills five accumulators at once (the items, their
// indents and the list's ordered/start/first facts); bundling them into a
// struct would be a rewrite of the walk rather than a fix, so the argument
// count is allowed here.
#[allow(clippy::too_many_arguments)]
fn collect_items(
    node: &Node,
    src: &str,
    spans: &[Span],
    items: &mut Vec<Vec<Run>>,
    indents: &mut Vec<usize>,
    ordered: &mut bool,
    start: &mut usize,
    first: &mut bool,
) {
    for c in &node.children {
        match c.kind.as_str() {
            "list_item" => {
                let marker = c
                    .children
                    .iter()
                    .find(|g| g.kind.starts_with("list_marker"));
                // Everything in the item except a nested list, which follows as its own
                // items. `subtree_runs` rather than a byte range: an item can hold a
                // fenced block, and a byte sweep of the range turned the fence markers
                // into item text.
                let rest: Vec<Node> = c
                    .children
                    .iter()
                    .filter(|g| {
                        g.kind != "list"
                            && g.kind != "block_continuation"
                            && !g.kind.starts_with("list_marker")
                            && g.kind != "task_list_marker_checked"
                            && g.kind != "task_list_marker_unchecked"
                    })
                    .cloned()
                    .collect();
                if *first {
                    *first = false;
                    if let Some(m) = marker {
                        let is_ordered =
                            m.kind == "list_marker_dot" || m.kind == "list_marker_parenthesis";
                        *ordered = is_ordered;
                        if is_ordered {
                            // The marker node's text includes the delimiter and the
                            // space that follows it — `"7. "` — so the trailing
                            // punctuation comes off before the digits are read.
                            *start = slice(src, m.start, m.end)
                                .and_then(|t| t.trim().trim_end_matches(['.', ')']).parse().ok())
                                .unwrap_or(1);
                        }
                    }
                }
                let item = item_runs(&rest, src, spans);
                if !item.is_empty() {
                    items.push(item);
                    indents.push(marker_indent(src, marker));
                }
                for g in &c.children {
                    if g.kind == "list" {
                        collect_items(g, src, spans, items, indents, ordered, start, first);
                    }
                }
            }
            "list" => collect_items(c, src, spans, items, indents, ordered, start, first),
            _ => {}
        }
    }
}

/// The column an item's marker was written at, rounded down to even and
/// capped at 8 — even, so `- ` and `1. ` sub-lists land on the same step;
/// capped, so a deep list stays in width. `None` is column 0.
fn marker_indent(src: &str, marker: Option<&Node>) -> usize {
    let Some(m) = marker else { return 0 };
    let Some(head) = slice(src, 0, m.start) else {
        return 0;
    };
    let line_start = head.rfind('\n').map(|i| i + 1).unwrap_or(0);
    let col = slice(src, line_start, m.start)
        .map(|t| t.chars().count())
        .unwrap_or(0);
    (col / 2 * 2).min(8)
}

fn table_block(node: &Node, src: &str, spans: &[Span]) -> Block {
    let mut head = Vec::new();
    let mut align = Vec::new();
    let mut rows = Vec::new();
    for c in &node.children {
        match c.kind.as_str() {
            "pipe_table_header" => head = cells_of(c, src, spans),
            "pipe_table_delimiter_row" => align = align_of(c, src),
            "pipe_table_row" => rows.push(cells_of(c, src, spans)),
            _ => {}
        }
    }
    Block::Table { head, align, rows }
}

fn cells_of(row: &Node, src: &str, spans: &[Span]) -> Vec<Vec<Run>> {
    row.children
        .iter()
        .filter(|c| c.kind == "pipe_table_cell")
        .map(|c| {
            let (a, b) = trim_range(src, c.start, c.end);
            single_line(src, spans, a, b)
        })
        .collect()
}

fn align_of(row: &Node, src: &str) -> Vec<Align> {
    row.children
        .iter()
        .filter(|c| c.kind == "pipe_table_delimiter_cell")
        .map(|c| {
            let t = slice(src, c.start, c.end)
                .unwrap_or("")
                .trim()
                .trim_matches('|')
                .trim();
            match (t.starts_with(':'), t.ends_with(':')) {
                (true, true) => Align::Center,
                (false, true) => Align::Right,
                _ => Align::Left,
            }
        })
        .collect()
}

/// Tighten a range past whitespace and the pipes a cell is written between, so a
/// cell does not carry its own `|` into the rendered table.
fn trim_range(src: &str, a: usize, b: usize) -> (usize, usize) {
    // A range this source cannot be cut at is left as it is: every caller slices with the
    // same guard, so nothing reads a byte it should not — the range simply yields no text.
    let Some(text) = slice(src, a, b) else {
        return (a, b.max(a));
    };
    let start = a + (text.len() - text.trim_start_matches([' ', '|']).len());
    let end = b - (text.len() - text.trim_end_matches([' ', '|']).len());
    (start, end.max(start))
}

// ---------------------------------------------------------------------------
// Spans to runs
// ---------------------------------------------------------------------------

/// The styled runs of `src[a..b]`, with markers already gone. A span with
/// no style is a marker or link destination; a stretch no span covers is
/// text, shown rather than dropped.
fn runs_of(src: &str, spans: &[Span], a: usize, b: usize) -> Vec<Run> {
    let mut out: Vec<Run> = Vec::new();
    if a >= b {
        return out;
    }
    let mut pos = a;
    for s in spans.iter().filter(|s| s.end > a && s.start < b) {
        let (s0, s1) = (s.start.max(a), s.end.min(b));
        if s0 > pos
            && let Some(t) = slice(src, pos, s0)
        {
            push_text(&mut out, t, InlineStyle::Plain, None);
        }
        if s1 > s0
            && let Some(style) = s.style
            && let Some(t) = slice(src, s0, s1)
        {
            push_text(&mut out, t, style, s.link.as_deref());
        }
        pos = pos.max(s1);
    }
    if b > pos
        && let Some(t) = slice(src, pos, b)
    {
        push_text(&mut out, t, InlineStyle::Plain, None);
    }
    out
}

/// Add a run, undoing any `\`-escape in it, and coalescing it into the
/// previous run when the style is the same — a `\|` is its own node, so
/// without it `a\|b` arrives as three runs where the model wrote one word.
/// Not in a code span: the grammar already decided which backslashes are
/// escapes, so a `\d` in a regex keeps its backslash.
fn push_text(out: &mut Vec<Run>, text: &str, style: InlineStyle, link: Option<&str>) {
    if text.is_empty() {
        return;
    }
    let text = if style != InlineStyle::Code && text.contains('\\') {
        unescape(text)
    } else {
        text.to_string()
    };
    if let Some(last) = out.last_mut()
        && last.style == style
        && last.link.as_deref() == link
    {
        last.text.push_str(&text);
        return;
    }
    let mut r = run(text, style);
    r.link = link.map(str::to_string);
    out.push(r);
}

/// Drop the backslash from `\`-punctuation, the way CommonMark reads one.
fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\'
            && let Some(next) = chars.clone().next()
            && next.is_ascii_punctuation()
        {
            out.push(chars.next().unwrap());
            continue;
        }
        out.push(c);
    }
    out
}

/// One item or cell: its text with the newlines folded to spaces — a wrap in
/// the source was prose, not a line break.
fn single_line(src: &str, spans: &[Span], a: usize, b: usize) -> Vec<Run> {
    let mut runs = runs_of(src, spans, a, b);
    for r in &mut runs {
        if r.text.contains('\n') {
            r.text = r.text.split_whitespace().collect::<Vec<_>>().join(" ");
        }
    }
    runs.retain(|r| !r.text.is_empty());
    runs
}

fn run(text: String, style: InlineStyle) -> Run {
    Run {
        text,
        style,
        link: None,
    }
}

/// Where to settle an over-long window; `None` means nothing may settle yet.
/// [`stable_boundary_with`] first, then never inside a fence. Under the cap a
/// refusal means nothing settles; past it the guards are relaxed — the last
/// line break, or failing that the last space, never inside a fence. The
/// relaxed cuts are wrong in a bounded way (two paragraphs where the model
/// wrote one); the alternative is quadratic on the shapes with no other
/// boundary.
fn cut_point(win: &str, max: usize, fences: &[(usize, usize)]) -> Option<usize> {
    if let Some(b) = stable_boundary_with(win, max)
        && !inside_a_fence(fences, b)
    {
        return Some(b);
    }
    if win.len() < max {
        return None;
    }
    let head = &win[..max];
    for (i, _) in head.match_indices('\n').rev() {
        if !inside_a_fence(fences, i + 1) {
            return Some(i + 1);
        }
    }
    for (i, _) in head.match_indices(' ').rev() {
        if !inside_a_fence(fences, i + 1) {
            return Some(i + 1);
        }
    }
    None
}

// Hand-written because the parse state is a tree-sitter parser and tree, which have
// no `Debug` worth reading and would print a page of it per assertion failure.
impl std::fmt::Debug for IncrementalMarkdown {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IncrementalMarkdown")
            .field("raw_len", &self.raw.len())
            .field("window_start", &self.window_start)
            .field("window_len", &self.window_len())
            .field("stable", &self.stable.len())
            .field("tail", &self.tail.len())
            .finish()
    }
}

// ---------------------------------------------------------------------------
// The text boundary, kept for the one shape the tree cannot settle
// ---------------------------------------------------------------------------

/// A byte offset near the end of `src` from which the tail parses
/// standalone, with at least `min_bytes` after it — the question a head asks
/// when it attaches, where the cost being avoided is lexing megabytes to
/// draw the bottom of a small window. `lex(&src[cut..])` gives blocks whose
/// second and later entries are the document's own; the first may be ragged
/// (a list starting mid-way still renders identically — the renderer numbers
/// from the marker), so a caller asks for more than it draws. Fences are
/// consulted first, parity being the one thing a backward scan cannot decide
/// alone; then candidates backward from the floor. `None` means lex the
/// whole thing: correct and slow rather than fast and wrong.
pub fn tail_cut(src: &str, min_bytes: usize) -> Option<usize> {
    if src.len() <= min_bytes {
        return Some(0);
    }
    let fences: Vec<(usize, usize)> = fences_in(src).iter().map(|f| (f.open, f.end)).collect();
    // Line starts. Every one is a char boundary, which is what keeps a cut from landing
    // inside a multi-byte character — walking back by byte does not, and did not.
    let mut starts: Vec<usize> = vec![0];
    for (i, b) in src.bytes().enumerate() {
        if b == b'\n' {
            starts.push(i + 1);
        }
    }
    let line_at = |i: usize| -> &str {
        let from = starts[i];
        let to = starts.get(i + 1).map(|s| s - 1).unwrap_or(src.len());
        &src[from..to]
    };
    // The floor: the earliest line start that still leaves `min_bytes`.
    let floor = src.len() - min_bytes;
    let last_candidate = starts.partition_point(|&s| s <= floor).saturating_sub(1);
    // A candidate is a line whose predecessor is blank — so the cut is "a blank line,
    // then the start of real content", the same shape `stable_boundary_with` accepts.
    for i in (2..=last_candidate).rev() {
        if !line_at(i - 1).trim().is_empty() {
            continue;
        }
        let at = starts[i];
        if at == 0 || at >= src.len() {
            continue;
        }
        // Guard: not inside a fence. The one a backward scan cannot decide alone.
        if inside_a_fence(&fences, at) {
            continue;
        }
        let next = line_at(i);
        // Guard: the next line starts content, not an indented continuation.
        if next.starts_with(' ') || next.starts_with('\t') {
            continue;
        }
        // Guard: the previous content line must not be the marker of a list this line
        // continues *with the same kind of marker*, which is one list rather than two.
        // A different kind is a new list, which is what a cut there gives anyway.
        let prev = (0..i - 1).rev().map(line_at).find(|l| !l.trim().is_empty());
        if let (Some(was), Some(now)) = (prev.and_then(list_kind), list_kind(next))
            && was == now
        {
            continue;
        }
        return Some(at);
    }
    None
}

/// The offset past the last `\n\n` that is safe to freeze, if any — the
/// escape hatch, reached only when nothing has settled past `max_unfrozen`.
/// The four guards, each a case where cutting changes the parse: strictly
/// inside; balanced fences; the next character starts real content; and a
/// preceding list is provably closed, looking forward (CommonMark continues
/// a list across a blank line as one loose list). Every guard reads only
/// the two lines either side, so this stays O(window). `relax_at` drops the
/// last guard: it is unfalsifiable while the list is open, and being wrong
/// costs two tight lists where the model wrote one loose one.
pub fn stable_boundary(s: &str) -> Option<usize> {
    stable_boundary_with(s, usize::MAX)
}

pub fn stable_boundary_with(s: &str, relax_at: usize) -> Option<usize> {
    // One forward pass carrying the fence state, rather than re-deciding per
    // candidate: the per-candidate form is O(n²) per push.
    let mut best: Option<usize> = None;
    let mut fence_open = false;
    let mut pending_blank = false;
    let mut last_nonblank: Option<&str> = None;
    let mut off = 0usize;
    let relaxed = s.len() >= relax_at;

    while off < s.len() {
        let nl = s[off..].find('\n');
        let (line_end, next) = match nl {
            Some(i) => (off + i, off + i + 1),
            None => (s.len(), s.len()),
        };
        let complete = nl.is_some();
        let line = &s[off..line_end];
        let t = line.trim_start();

        if t.is_empty() {
            // Guard 2: a blank line inside a fence is content, not a boundary.
            if !fence_open {
                pending_blank = true;
            }
            off = next;
            continue;
        }

        if pending_blank && !fence_open && off > 0 {
            // Guard 1 is `off > 0` plus the fact that we are standing on real
            // content, so the boundary is strictly inside the text.
            // Guard 3: an indented line after a blank joins backwards.
            let indented = line.starts_with(' ') || line.starts_with('\t');
            let mut ok = !indented;
            // Guard 4.
            if ok
                && !relaxed
                && let Some(prev) = last_nonblank
                && let Some(was) = list_kind(prev.trim_start())
            {
                if !complete {
                    // The next line is still arriving; it cannot yet prove the
                    // list closed.
                    ok = false;
                } else if let Some(now) = list_kind(t)
                    && was == now
                {
                    ok = false;
                }
            }
            if ok {
                best = Some(off);
            }
        }

        pending_blank = false;
        if t.starts_with("```") || t.starts_with("~~~") {
            fence_open = !fence_open;
        }
        last_nonblank = Some(line);
        off = next;
    }
    best
}

/// The kind of list item this line opens: `Some(true)` ordered,
/// `Some(false)` bullet, `None` not one. Guard 4 compares the marker's
/// *kind*: `1. a\n\n2. b` is one loose list, `- a\n\n1. b` is two.
fn list_kind(t: &str) -> Option<bool> {
    let t = t.trim_start();
    if t == "-"
        || t == "*"
        || t == "+"
        || t.starts_with("- ")
        || t.starts_with("* ")
        || t.starts_with("+ ")
    {
        return Some(false);
    }
    let digits: String = t.chars().take_while(|c| c.is_ascii_digit()).collect();
    if !digits.is_empty() && digits.len() <= 9 {
        let rest = &t[digits.len()..];
        if rest.starts_with('.') || rest.starts_with(')') {
            return Some(true);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::markdown::testing::MARKDOWN;

    pub(super) fn stream(doc: &str, chunk: usize) -> IncrementalMarkdown {
        let mut md = IncrementalMarkdown::new();
        let mut buf = String::new();
        for c in doc.chars() {
            buf.push(c);
            if buf.chars().count() >= chunk {
                md.push(&buf);
                buf.clear();
            }
        }
        if !buf.is_empty() {
            md.push(&buf);
        }
        md
    }

    pub(super) fn blocks(md: &IncrementalMarkdown) -> Vec<Block> {
        md.blocks().cloned().collect()
    }

    #[test]
    fn a_documents_pictures_are_read_out_of_its_source() {
        let doc = "Intro.\n\n![the sun](sun.png)\n\n![a note](<a b.png> \"the title\")\n\n![](no-alt.png)\n";
        assert_eq!(
            images(doc),
            vec![
                ("the sun".to_string(), "sun.png".to_string()),
                ("a note".to_string(), "a b.png".to_string()),
                (String::new(), "no-alt.png".to_string()),
            ]
        );
        // A URL target is skipped, and a `![` whose `](` is paragraphs away is not an image.
        assert_eq!(images("![x](https://example.com/s.png)"), Vec::new());
        assert_eq!(images("![one\n\ntwo](s.png)"), Vec::new());
        assert_eq!(images("no pictures here"), Vec::new());
        assert_eq!(images("![unclosed](s.png"), Vec::new());
    }

    #[test]
    fn streaming_gives_the_same_blocks_as_a_single_parse() {
        for chunk in [1, 2, 3, 7, 64, 4096] {
            let md = stream(MARKDOWN, chunk);
            assert_eq!(blocks(&md), lex(MARKDOWN), "chunk size {chunk}");
        }
    }

    #[test]
    fn streaming_agrees_with_one_parse_on_every_block_shape() {
        let doc = "\
# Title

Some **bold** and *italic* and `code` and ~~struck~~ text.

- one
- two
  - nested

> quoted
> twice

| a | b |
|---|--:|
| 1 | 2 |

```rust
fn main() {}

fn other() {}
```

---

tail
";
        for chunk in [1, 3, 11, 512] {
            let md = stream(doc, chunk);
            assert_eq!(blocks(&md), lex(doc), "chunk size {chunk}");
        }
    }

    #[test]
    fn the_window_stays_bounded_as_the_document_grows() {
        // Long enough that an unbounded window is unmistakable: 400 paragraphs.
        let doc: String = (0..400)
            .map(|i| format!("Paragraph {i} with a little text in it.\n\n"))
            .collect();
        let mut md = IncrementalMarkdown::new();
        // Per push: an eighth of a paragraph, so the deltas are realistic in size.
        for c in doc.as_bytes().chunks(8) {
            md.push(std::str::from_utf8(c).unwrap());
        }
        assert!(
            md.stable_count() > 300,
            "only {} settled",
            md.stable_count()
        );
        assert!(
            md.window_len() <= DEFAULT_MAX_UNFROZEN,
            "the window grew to {} bytes on a {} byte document",
            md.window_len(),
            doc.len()
        );
        // And the bytes shown to a parser are linear in the document, not quadratic.
        let n = doc.len() as u64;
        let naive = n * n / (2 * 8);
        assert!(
            md.bytes_lexed() < naive / 10,
            "lexed {} bytes; a full re-parse per delta would lex about {naive}. \
             That ratio is the whole of §13.3.",
            md.bytes_lexed()
        );
    }

    #[test]
    fn a_message_that_is_one_long_list_is_linear_not_quadratic() {
        fn items_settled(md: &IncrementalMarkdown) -> usize {
            md.stable()
                .iter()
                .map(|b| match b {
                    Block::List { items, .. } => items.len(),
                    _ => 1,
                })
                .sum()
        }
        fn lexed(items: usize) -> u64 {
            let doc: String = (0..items).map(|i| format!("- item {i}\n\n")).collect();
            let mut md = IncrementalMarkdown::new();
            for c in doc.as_bytes().chunks(4) {
                md.push(std::str::from_utf8(c).unwrap());
            }
            assert!(
                items_settled(&md) > items / 2,
                "settled {} of {items} items",
                items_settled(&md)
            );
            assert!(
                md.window_len() <= DEFAULT_MAX_UNFROZEN,
                "{}",
                md.window_len()
            );
            md.bytes_lexed()
        }
        let small = lexed(800);
        let large = lexed(1600);
        assert!(
            large < small * 3,
            "doubling the message multiplied the parsing by {:.1}; \
             quadratic is 4, linear is 2",
            large as f64 / small as f64
        );
    }

    #[test]
    fn a_streamed_real_reply_is_lexed_linearly() {
        const REAL: &str = include_str!("../../tests/fixtures/markdown/streamed-message.md");
        fn push(copies: usize) -> (usize, u64, u64) {
            let doc = REAL.repeat(copies);
            let md = stream(&doc, 6);
            (doc.len(), md.bytes_lexed(), md.lex_calls())
        }
        let (n1, lexed1, calls1) = push(8);
        let (n2, lexed2, calls2) = push(16);
        let ratio = lexed2 as f64 / lexed1 as f64;
        eprintln!(
            "streamed-message ×8: {n1} B, lexed {lexed1} B in {calls1} parses; \
             ×16: {n2} B, lexed {lexed2} B in {calls2} parses; ratio {ratio:.2}"
        );
        assert!(
            ratio < 2.6,
            "doubling the stream ({n1} → {n2} bytes) multiplied the lexing by {ratio:.2} \
             ({lexed1} → {lexed2} bytes, {calls1} → {calls2} parses); \
             quadratic is 4, linear is 2"
        );
        // Far below the ~n²/2c a full re-parse per delta would show a parser.
        let naive = (n2 * n2 / (2 * 6)) as u64;
        assert!(
            lexed2 < naive / 20,
            "lexed {lexed2}; a re-parse per delta is {naive}"
        );
    }

    #[test]
    fn one_endless_paragraph_is_bounded_by_the_cap() {
        let mut md = IncrementalMarkdown::new();
        for i in 0..2000 {
            md.push(&format!("word{i} "));
        }
        assert!(
            md.window_len() <= DEFAULT_MAX_UNFROZEN,
            "one paragraph grew the window to {}",
            md.window_len()
        );
        assert!(md.stable_count() >= 3, "{}", md.stable_count());
    }

    #[test]
    fn the_last_block_is_never_settled_while_it_can_still_grow() {
        let mut md = IncrementalMarkdown::new();
        md.push("one\n\n");
        // One paragraph, and it is the last: nothing settles, because the next delta may continue it.
        assert_eq!(md.stable_count(), 0);
        md.push("two\n\n");
        // Now the first paragraph has a block after it, so it is settled.
        assert_eq!(md.stable_count(), 1);
        assert_eq!(md.tail().len(), 1);
    }

    // ---- the block model, read off the tree ----------------------------------

    #[test]
    fn a_heading_does_not_keep_its_hashes() {
        let b = lex("## Why the cache missed\n");
        assert_eq!(
            b,
            vec![Block::Heading {
                level: 2,
                runs: vec![run("Why the cache missed".into(), InlineStyle::Plain)],
            }]
        );
        // Setext headings come from the same arm.
        assert!(matches!(
            lex("Title\n=====\n").first(),
            Some(Block::Heading { level: 1, .. })
        ));
    }

    #[test]
    fn inline_markers_become_runs_and_do_not_reach_the_model() {
        let b = lex("plain **bold** and *italic* and `code` and ~~struck~~ end\n");
        let Block::Paragraph { lines } = &b[0] else {
            panic!("{b:#?}")
        };
        let styles: Vec<InlineStyle> = lines[0].iter().map(|r| r.style).collect();
        assert!(styles.contains(&InlineStyle::Bold), "{:?}", lines[0]);
        assert!(styles.contains(&InlineStyle::Italic), "{:?}", lines[0]);
        assert!(styles.contains(&InlineStyle::Code), "{:?}", lines[0]);
        assert!(
            styles.contains(&InlineStyle::Strikethrough),
            "{:?}",
            lines[0]
        );
        // The markers are gone from the text, and so is nothing else.
        assert_eq!(
            runs_text(&lines[0]),
            "plain bold and italic and code and struck end"
        );
        for r in &lines[0] {
            assert!(!r.text.contains('*'), "{:?}", r.text);
            assert!(!r.text.contains('`'), "{:?}", r.text);
        }
    }

    #[test]
    fn nested_emphasis_is_one_style_not_two_markers() {
        let b = lex("***both*** and **bold *and italic***\n");
        let Block::Paragraph { lines } = &b[0] else {
            panic!("{b:#?}")
        };
        let both = lines[0]
            .iter()
            .find(|r| r.text == "both")
            .expect("the run for `both`");
        assert_eq!(both.style, InlineStyle::BoldItalic);
        assert_eq!(runs_text(&lines[0]), "both and bold and italic");
    }

    #[test]
    fn a_star_that_is_not_emphasis_stays_literal() {
        let b = lex("the product 2 * 3 and 4 * 5 is 120\n");
        let Block::Paragraph { lines } = &b[0] else {
            panic!("{b:#?}")
        };
        assert_eq!(runs_text(&lines[0]), "the product 2 * 3 and 4 * 5 is 120");
        assert!(
            lines[0].iter().all(|r| r.style == InlineStyle::Plain),
            "{:?}",
            lines[0]
        );
    }

    #[test]
    fn a_link_shows_its_text_and_not_its_destination() {
        let b = lex("see [the plan](docs/plan.md) and <https://example.invalid>\n");
        let Block::Paragraph { lines } = &b[0] else {
            panic!("{b:#?}")
        };
        assert_eq!(
            runs_text(&lines[0]),
            "see the plan and https://example.invalid"
        );
    }

    #[test]
    fn a_table_is_cells_and_alignment_from_the_tree() {
        let b = lex("| n | name | size |\n|--:|:----:|:-----|\n| 1 | a\\|b | wide |\n");
        let Block::Table { head, align, rows } = &b[0] else {
            panic!("{b:#?}")
        };
        let text = |c: &Vec<Run>| runs_text(c);
        assert_eq!(
            head.iter().map(text).collect::<Vec<_>>(),
            ["n", "name", "size"]
        );
        assert_eq!(align, &[Align::Right, Align::Center, Align::Left]);
        assert_eq!(
            rows[0].iter().map(text).collect::<Vec<_>>(),
            ["1", "a|b", "wide"]
        );
    }

    #[test]
    fn an_unterminated_fence_still_renders() {
        let md = stream("```rust\nlet a = 1;\n", 3);
        let all = blocks(&md);
        let Some(Block::Code {
            lang,
            closed,
            lines,
        }) = all.last()
        else {
            panic!("{all:#?}");
        };
        assert_eq!(lang, "rust");
        assert!(!closed, "the fence is still open");
        assert_eq!(lines, &["let a = 1;"]);
        // A closed one says so, and does not carry the closing fence as content.
        let b = lex("```rust\nlet a = 1;\n```\n");
        let Some(Block::Code { closed, lines, .. }) = b.first() else {
            panic!("{b:#?}")
        };
        assert!(closed);
        assert_eq!(lines, &["let a = 1;"]);
    }

    #[test]
    fn a_loose_ordered_list_keeps_the_numbers_it_was_written_with() {
        let b = lex("1. first\n\n2. second\n\n3. third\n\ntail\n");
        let Some(Block::List {
            ordered,
            start,
            items,
            ..
        }) = b.first()
        else {
            panic!("{b:#?}");
        };
        assert!(ordered);
        assert_eq!(*start, 1);
        assert_eq!(
            items.iter().map(|i| runs_text(i)).collect::<Vec<_>>(),
            ["first", "second", "third"]
        );
        // And the renderer's numbering — `start + i` — is the model's own.
        let numbered: Vec<usize> = (0..items.len()).map(|i| start + i).collect();
        assert_eq!(numbered, vec![1, 2, 3]);
        // A list the model started at seven stays at seven.
        let b = lex("7. seven\n8. eight\n\ntail\n");
        assert!(
            matches!(b.first(), Some(Block::List { start: 7, .. })),
            "{b:#?}"
        );
        // A bullet list has no written number and starts at one.
        let b = lex("- a\n- b\n");
        assert!(matches!(
            b.first(),
            Some(Block::List {
                ordered: false,
                start: 1,
                ..
            })
        ));
    }

    #[test]
    fn a_list_item_does_not_keep_its_marker() {
        let b = lex("- one\n- two\n");
        let Some(Block::List { items, .. }) = b.first() else {
            panic!("{b:#?}")
        };
        assert_eq!(
            items.iter().map(|i| runs_text(i)).collect::<Vec<_>>(),
            ["one", "two"]
        );
    }

    #[test]
    fn a_nested_list_reads_flat() {
        let b = lex("- one\n  - sub\n- two\n");
        let Some(Block::List { items, .. }) = b.first() else {
            panic!("{b:#?}")
        };
        assert_eq!(
            items.iter().map(|i| runs_text(i)).collect::<Vec<_>>(),
            ["one", "sub", "two"]
        );
    }

    #[test]
    fn a_quote_does_not_keep_its_markers() {
        let b = lex("> one **two**\n> three\n");
        let Block::Quote { lines } = &b[0] else {
            panic!("{b:#?}")
        };
        assert_eq!(lines.len(), 2);
        assert_eq!(runs_text(&lines[0]), "one two");
        assert_eq!(runs_text(&lines[1]), "three");
        assert!(lines[0].iter().any(|r| r.style == InlineStyle::Bold));
    }

    // ---- the text boundary, which is now only the escape hatch ---------------

    #[test]
    fn a_blank_line_inside_a_fence_is_not_a_boundary() {
        let doc = "```rust\nlet a = 1;\n\nlet b = 2;\n```\n\nafter\n";
        let b = stable_boundary(doc).unwrap();
        assert!(
            doc[..b].contains("```rust") && doc[..b].contains("let b"),
            "the boundary landed inside the fence: {:?}",
            &doc[..b]
        );
    }

    #[test]
    fn a_list_that_may_still_continue_is_not_frozen() {
        // CommonMark makes `- a\n\n- b` ONE loose list, so the blank between them is not a boundary.
        let doc = "- a\n\n- b\n\ntext more\n";
        let b = stable_boundary(doc).unwrap();
        assert_eq!(&doc[..b], "- a\n\n- b\n\n");
        assert!(stable_boundary("- a\n\n- b\n").is_none());
    }

    #[test]
    fn an_indented_continuation_is_not_a_boundary() {
        let doc = "- a\n\n  still a\n\nnew\n";
        if let Some(b) = stable_boundary(doc) {
            assert!(
                !doc[b..].starts_with("  "),
                "froze before an indented continuation"
            );
        }
    }

    #[test]
    fn the_prefix_cut_is_one_the_whole_document_agrees_with() {
        // The property the window's cap relies on, checked against the whole document.
        let doc = MARKDOWN;
        let mut cut = 0;
        let mut any = false;
        while let Some(b) = stable_boundary(&doc[cut..]) {
            let abs = cut + b;
            let mut joined = lex(&doc[..abs]);
            joined.extend(lex(&doc[abs..]));
            assert_eq!(joined, lex(doc), "split at byte {abs} changed the parse");
            any = true;
            cut = abs;
        }
        assert!(any, "the fixture must contain at least one stable boundary");
    }

    #[test]
    fn cut_is_at_least_the_boundary() {
        // The prefix a cut produces must be non-empty.
        let mut md = IncrementalMarkdown::new();
        md.push("a paragraph long enough to matter\n\nand a second one\n\n");
        assert!(md.stable_count() >= 1);
        let first = md.stable()[0].title();
        assert_eq!(first, "a paragraph long enough to matter");
        assert!(!md.stable()[0].title().is_empty());
    }
}

#[cfg(test)]
mod nesting {
    use super::*;

    fn keeps(src: &str, words: &[&str]) {
        let blocks = lex(src);
        let mut got = String::new();
        fn text(blocks: &[Block], out: &mut String) {
            for b in blocks {
                match b {
                    Block::Code { lines, .. } => {
                        for l in lines {
                            out.push_str(l);
                            out.push(' ');
                        }
                    }
                    _ => {
                        out.push_str(&b.title());
                        out.push(' ');
                    }
                }
                if let Block::List { items, .. } = b {
                    for i in items {
                        out.push_str(&runs_text(i));
                        out.push(' ');
                    }
                }
                if let Block::Quote { lines } = b {
                    for l in lines {
                        out.push_str(&runs_text(l));
                        out.push(' ');
                    }
                }
                if let Block::Paragraph { lines } = b {
                    for l in lines {
                        out.push_str(&runs_text(l));
                        out.push(' ');
                    }
                }
            }
        }
        text(&blocks, &mut got);
        for w in words {
            assert!(
                got.contains(w),
                "{w:?} was dropped: {got:?} from {blocks:#?}"
            );
        }
    }

    #[test]
    fn a_fence_inside_a_quote_is_not_dropped() {
        let src = "> ```rust\n> let a = 1;\n> ```\n";
        keeps(src, &["let a = 1;"]);
        let b = lex(src);
        let Some(Block::Code {
            lang,
            lines,
            closed,
        }) = b.first()
        else {
            panic!("{b:#?}")
        };
        assert_eq!(lang, "rust");
        assert_eq!(lines, &["let a = 1;"]);
        assert!(*closed);
        assert!(!lines.iter().any(|l| l.contains('>')), "{lines:?}");
        // No empty quote left behind.
        assert_eq!(b.len(), 1, "{b:#?}");
    }

    #[test]
    fn a_fence_inside_a_list_item_is_not_markers() {
        let src = "- item\n\n  ```rust\n  let a = 1;\n  ```\n";
        keeps(src, &["item", "let a = 1;"]);
        let b = lex(src);
        let Some(Block::List { items, .. }) = b.first() else {
            panic!("{b:#?}")
        };
        let item = runs_text(&items[0]);
        assert_eq!(item, "item", "the item is the item's own prose");
        let Some(Block::Code { lang, lines, .. }) = b.get(1) else {
            panic!("{b:#?}")
        };
        assert_eq!(lang, "rust");
        assert_eq!(
            lines,
            &["let a = 1;"],
            "the item's indent is not part of the code"
        );
    }

    #[test]
    fn a_table_inside_a_quote_keeps_its_cells() {
        let src = "> | a | b |\n> |---|---|\n> | 1 | 2 |\n";
        keeps(src, &["a b", "1 2"]);
        let b = lex(src);
        let Block::Quote { lines } = &b[0] else {
            panic!("{b:#?}")
        };
        let texts: Vec<String> = lines.iter().map(|l| runs_text(l)).collect();
        assert!(texts.iter().any(|t| t.contains("1 2")), "{texts:?}");
        assert!(!texts.iter().any(|t| t.contains('|')), "{texts:?}");
    }

    #[test]
    fn a_fence_in_an_unknown_language_is_still_code() {
        let b = lex("```brainfuck\n+++\n```\n");
        let Some(Block::Code {
            lang,
            lines,
            closed,
        }) = b.first()
        else {
            panic!("{b:#?}")
        };
        assert_eq!(lang, "brainfuck");
        assert_eq!(lines, &["+++"]);
        assert!(closed);
    }

    #[test]
    fn a_fence_cannot_quote_itself() {
        let b = lex("```\nalpha\n```\nbeta\n```\n");
        let kinds: Vec<&str> = b
            .iter()
            .map(|x| match x {
                Block::Code { .. } => "code",
                _ => "text",
            })
            .collect();
        assert_eq!(kinds, ["code", "text", "code"], "{b:#?}");
        // The first closed at the inner fence, so `beta` is prose…
        let Block::Code { lines, closed, .. } = &b[0] else {
            panic!("{b:#?}")
        };
        assert_eq!(lines, &["alpha"]);
        assert!(*closed);
        // …and the trailing fence is the one still open, which is what the footer says.
        let Some(Block::Code { closed, lines, .. }) = b.last() else {
            panic!("{b:#?}")
        };
        assert!(!*closed);
        assert!(lines.is_empty());
        // Four backticks is how to quote three: then the inner fence is content.
        let b = lex("````\nalpha\n```\n````\n");
        assert_eq!(b.len(), 1, "{b:#?}");
        let Some(Block::Code { lines, closed, .. }) = b.first() else {
            panic!("{b:#?}")
        };
        assert_eq!(lines, &["alpha", "```"]);
        assert!(closed);
    }

    #[test]
    fn a_quote_with_prose_and_a_fence_keeps_both() {
        let src = "> There is `redacted` here:\n>\n> ```rust\n> let a = 1;\n> ```\n";
        keeps(src, &["There is", "redacted", "let a = 1;"]);
        let b = lex(src);
        // The quote keeps its prose ...
        let Some(Block::Quote { lines }) = b.first() else {
            panic!("{b:#?}")
        };
        let texts: Vec<String> = lines.iter().map(|l| runs_text(l)).collect();
        assert_eq!(texts, &["There is redacted here:"], "{texts:?}");
        // ... and the code is the next block, as code.
        let Some(Block::Code { lang, lines, .. }) = b.get(1) else {
            panic!("{b:#?}")
        };
        assert_eq!(lang, "rust");
        assert_eq!(lines, &["let a = 1;"]);
        assert_eq!(b.len(), 2, "{b:#?}");
    }
}

#[cfg(test)]
mod streaming_matches_one_parse {
    use super::*;
    use crate::markdown::parse::tests::{blocks, stream};

    fn agrees(doc: &str) {
        let whole = lex(doc);
        let strm = stream(doc, 1).blocks().cloned().collect::<Vec<_>>();
        assert_eq!(strm, whole, "{doc:?}");
    }

    #[test]
    fn a_settle_never_lands_inside_an_open_fence() {
        agrees("prose\n\n```\n``` inner\ncode\n```\n\ntail\n```\n\nreal tail\n");
        agrees("```\nalpha\n```\nbeta\n```\n\nPinned as a test.\n");
        agrees("para\n\n```\n```          →  code(alpha)\nalpha\n```\nbeta\n```\n\nPinned.\n");
        // The shape spelled out: the code block is one block, closed.
        let doc = "```\nalpha\n```\nbeta\n```\n\nPinned as a test.\n";
        let b = lex(doc);
        assert_eq!(b.len(), 3, "{b:#?}");
        let Some(Block::Code { lines, closed, .. }) = b.get(2) else {
            panic!("{b:#?}")
        };
        assert!(!*closed, "the last fence is open");
        assert!(
            lines.iter().any(|l| l.contains("Pinned")),
            "the text after the open fence belongs to it: {lines:?}"
        );
    }

    #[test]
    fn a_loose_ordered_list_is_one_list_however_many_times_it_wraps() {
        agrees("1. first\n\n2. second\n\n3. third\n\ntail\n");
        agrees("1. first\n\n2. second\n\ntail\n");
        // A bullet list and an ordered one are different lists and the cut is fine.
        agrees("- a\n\n1. b\n\ntail\n");
        // Six points, each a sentence, is the real shape this came from.
        let doc = "1. one\n\n2. two\n\n3. three\n\n4. four\n\n5. five\n\n6. six\n\nThat is all.\n";
        agrees(doc);
        let blocks = lex(doc);
        let Some(Block::List { items, start, .. }) = blocks.first() else {
            panic!()
        };
        assert_eq!(*start, 1);
        assert_eq!(items.len(), 6, "six points, one list");
    }

    #[test]
    fn a_long_code_block_stays_whole_and_its_window_is_the_block() {
        let doc: String = format!(
            "```rust\n{}```\n",
            (0..800)
                .map(|i| format!("fn f{i}() {{}}\n"))
                .collect::<String>()
        );
        assert!(doc.len() > 2 * DEFAULT_MAX_UNFROZEN, "{}", doc.len());
        let mut md = IncrementalMarkdown::new();
        for c in doc.as_bytes().chunks(8) {
            md.push(std::str::from_utf8(c).unwrap());
        }
        // One code block, because the closing fence arrived and closed it.
        let all = blocks(&md);
        let code: Vec<&Block> = all
            .iter()
            .filter(|b| matches!(b, Block::Code { .. }))
            .collect();
        assert_eq!(code.len(), 1, "the block was split: {code:#?}");
        // Nothing else: the code did not become prose.
        assert_eq!(all.len(), 1, "{all:#?}");
        let Block::Code { lines, closed, .. } = code[0] else {
            panic!()
        };
        assert!(*closed);
        assert_eq!(lines.len(), 800, "a line was lost");
        assert_eq!(lines[0], "fn f0() {}");
        assert_eq!(lines[799], "fn f799() {}");
        // It never settled: a window that is one fence has no legal cut in it.
        assert_eq!(md.stable_count(), 0);
        assert_eq!(md.tail().len(), 1);
        // A fence followed by a blank line does settle: the cut is past the closing delimiter.
        let mut md = IncrementalMarkdown::new();
        let with_prose = format!(
            "```rust\n{}```\n\nprose after\n",
            (0..800)
                .map(|i| format!("fn f{i}() {{}}\n"))
                .collect::<String>()
        );
        for c in with_prose.as_bytes().chunks(64) {
            md.push(std::str::from_utf8(c).unwrap());
        }
        assert!(md.stable_count() >= 1, "the finished block settled");
        let all = blocks(&md);
        assert_eq!(all.len(), 2, "{all:#?}");
        assert!(
            matches!(all[0], Block::Code { closed: true, .. }),
            "{all:#?}"
        );
    }

    #[test]
    fn every_shape_agrees_at_every_chunk_size() {
        let docs = [
            "prose\n\n```\n``` inner\ncode\n```\n\ntail\n```\n\nreal tail\n",
            "```\nalpha\n```\nbeta\n```\n\nPinned as a test.\n",
            "1. first\n\n2. second\n\n3. third\n\ntail\n",
            "- a\n\n1. b\n\ntail\n",
            "> ```rust\n> let a = 1;\n> ```\n",
            "- item\n\n  ```rust\n  let a = 1;\n  ```\n",
            "> | a | b |\n> |---|---|\n> | 1 | 2 |\n",
            "## h\n\ntext **bold** `code`\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\n---\n\ntail\n",
        ];
        for doc in docs {
            for chunk in [1, 2, 3, 7, 64, 4096] {
                let a = stream(doc, chunk).blocks().cloned().collect::<Vec<_>>();
                assert_eq!(a, lex(doc), "chunk {chunk} on {doc:?}");
            }
        }
    }
}

#[cfg(test)]
mod inline_ranges_are_not_one_document {
    use super::*;
    use crate::markdown::parse::tests::stream;

    const REAL: &str = include_str!("../../tests/fixtures/markdown/streamed-message.md");

    #[test]
    fn a_delimiter_does_not_pair_with_one_in_another_paragraph() {
        let blocks = lex(REAL);
        // The first paragraph is prose with two code spans in it ...
        let Block::Paragraph { lines } = &blocks[0] else {
            panic!("{:#?}", blocks[0])
        };
        assert!(
            runs_text(&lines[0])
                .starts_with("Here — each block below exercises one of the paths I just fixed"),
            "{:?}",
            runs_text(&lines[0])
        );
        // ... and its ``` is literal text, not a delimiter.
        let text = runs_text(&lines[0]);
        assert!(text.contains("``` markers"), "{text:?}");
        assert!(text.contains("still arriving."), "{text:?}");
        // The second block keeps its bold; under the bug it was one Code run.
        let Block::Paragraph { lines } = &blocks[1] else {
            panic!("{:#?}", blocks[1])
        };
        assert!(
            lines[0].iter().any(|r| r.style == InlineStyle::Bold),
            "{:?}",
            lines[0]
        );
        assert_eq!(
            runs_text(&lines[0]).chars().take(14).collect::<String>(),
            "1. Plain, with"
        );
        // 24 blocks: 10 numbered paragraphs, the fences, the lists, the table, two closers.
        assert_eq!(blocks.len(), 24, "{blocks:#?}");

        // The one-shot parse and the byte-at-a-time stream must agree.
        for chunk in [1, 7, 64, 4096] {
            let a = stream(REAL, chunk).blocks().cloned().collect::<Vec<_>>();
            assert_eq!(a, blocks, "chunk {chunk}");
        }
    }

    #[test]
    fn the_smallest_case_of_the_same_bug() {
        let doc = "a ``` b\n\n**bold** c\n\n```rust\nlet a = 1;\n```\n\nd ``` e\n";
        let b = lex(doc);
        let Some(Block::Paragraph { lines }) = b.first() else {
            panic!("{b:#?}")
        };
        assert_eq!(
            runs_text(&lines[0]),
            "a ``` b",
            "the ``` is text, not a delimiter"
        );
        assert!(
            lines[0].iter().all(|r| r.style == InlineStyle::Plain),
            "{:?}",
            lines[0]
        );
        let Some(Block::Paragraph { lines }) = b.get(1) else {
            panic!("{b:#?}")
        };
        assert_eq!(runs_text(&lines[0]), "bold c");
        assert!(
            lines[0].iter().any(|r| r.style == InlineStyle::Bold),
            "{:?}",
            lines[0]
        );
        let Some(Block::Paragraph { lines }) = b.last() else {
            panic!("{b:#?}")
        };
        assert_eq!(runs_text(&lines[0]), "d ``` e");
        // One-shot and streamed must be the same document.
        for chunk in [1, 3, 4096] {
            let a = stream(doc, chunk).blocks().cloned().collect::<Vec<_>>();
            assert_eq!(a, b, "chunk {chunk}");
        }
    }

    #[test]
    fn code_spans_in_different_paragraphs_stay_in_their_own() {
        let doc = "`a` here\n\nplain prose\n\n`b` there\n";
        let b = lex(doc);
        assert_eq!(b.len(), 3, "{b:#?}");
        let spans = |i: usize| -> Vec<(InlineStyle, String)> {
            let Block::Paragraph { lines } = &b[i] else {
                panic!("{b:#?}")
            };
            lines[0].iter().map(|r| (r.style, r.text.clone())).collect()
        };
        assert_eq!(
            spans(0),
            [
                (InlineStyle::Code, "a".to_string()),
                (InlineStyle::Plain, " here".to_string())
            ]
        );
        assert!(spans(1).iter().all(|(s, _)| *s == InlineStyle::Plain));
        assert!(
            spans(2)
                .iter()
                .any(|(s, t)| *s == InlineStyle::Code && t == "b")
        );
        for chunk in [1, 5, 4096] {
            let a = stream(doc, chunk).blocks().cloned().collect::<Vec<_>>();
            assert_eq!(a, b, "chunk {chunk}");
        }
    }
}

#[cfg(test)]
mod a_fence_ends_only_at_a_line_of_its_own {
    use super::*;

    #[test]
    fn a_line_that_merely_ends_in_backticks_is_content() {
        for (src, want) in [
            ("```\nabc ```\ndef\n```\n", vec!["abc ```", "def"]),
            ("```\nfoo │ ```\nbar\n```\n", vec!["foo │ ```", "bar"]),
            (
                "```\nbar                      ```\nend\n```\n",
                vec!["bar                      ```", "end"],
            ),
        ] {
            let b = lex(src);
            assert_eq!(b.len(), 1, "{src:?} gave {b:#?}");
            let Some(Block::Code { lines, closed, .. }) = b.first() else {
                panic!("{b:#?}")
            };
            assert!(closed, "{src:?}");
            assert_eq!(lines, &want, "{src:?}");
        }
    }

    #[test]
    fn masking_leaves_no_fence_in_the_tree() {
        for src in [
            "> ```rust\n> let a = 1;\n> ```\n",
            "- item\n\n  ```rust\n  let a = 1;\n  ```\n",
            "```rust\nfn main() {}\n```\n",
            "> prose\n>\n> ```rust\n> let a = 1;\n> ```\n",
            "prose\n\n```\nabc ```\ndef\n```\n",
            "````\n```rust\nlet a = 1;\n```\n````\n",
        ] {
            let masked = mask(src, &fences_in(src));
            let mut stream = Stream::new(Lang::Markdown);
            stream.push(&masked);
            let root = stream.root().expect("a parse");
            let mut kinds = Vec::new();
            fn walk(n: &Node, kinds: &mut Vec<String>) {
                if n.kind.contains("fence") || n.kind.contains("code") {
                    kinds.push(n.kind.clone());
                }
                for c in &n.children {
                    walk(c, kinds);
                }
            }
            walk(&root, &mut kinds);
            assert!(kinds.is_empty(), "{src:?} left {kinds:?} in the tree");
            // Indented code is the one exception: no backticks, never masked.
            if src.contains("    ") && !src.contains("```") {
                assert!(masked.contains("    "));
            }
        }
        // An indented block *is* still in the tree, which is what makes `code_block` live.
        let mut stream = Stream::new(Lang::Markdown);
        stream.push("    let a = 1;\n");
        let root = stream.root().unwrap();
        let mut kinds = Vec::new();
        fn walk2(n: &Node, kinds: &mut Vec<String>) {
            kinds.push(n.kind.clone());
            for c in &n.children {
                walk2(c, kinds);
            }
        }
        walk2(&root, &mut kinds);
        assert!(
            kinds.iter().any(|k| k == "indented_code_block"),
            "{kinds:?}"
        );
    }

    #[test]
    fn a_fence_on_a_list_markers_line_is_not_item_text() {
        for src in [
            "- ```rust\n  let a = 1;\n  ```\n",
            "1. ```rust\n   let a = 1;\n   ```\n",
        ] {
            let b = lex(src);
            assert_eq!(b.len(), 1, "{src:?} gave {b:#?}");
            let Some(Block::Code {
                lang,
                lines,
                closed,
            }) = b.first()
            else {
                panic!("{b:#?}")
            };
            assert_eq!(lang, "rust", "{src:?}");
            assert_eq!(lines, &["let a = 1;"], "{src:?}");
            assert!(*closed, "{src:?}");
        }
        // A marker the model wrote as `*` or `+` is the same shape.
        for marker in ["-", "*", "+"] {
            let src = format!("{marker} ```rust\n  let a = 1;\n  ```\n");
            let b = lex(&src);
            let Some(Block::Code { lang, .. }) = b.first() else {
                panic!("{src:?} gave {b:#?}")
            };
            assert_eq!(lang, "rust", "{src:?}");
        }
    }

    #[test]
    fn stripping_a_content_column_does_not_eat_content() {
        // Indentation inside a top-level fence is the code's, not the container's.
        let b = lex("```rust\n    indented();\n\n        deeper();\n```\n");
        let Some(Block::Code { lines, .. }) = b.first() else {
            panic!("{b:#?}")
        };
        assert_eq!(lines, &["    indented();", "", "        deeper();"]);

        // A list *inside* a fence is the code's too.
        let b = lex("- item\n\n  ```\n  - a\n  - b\n  ```\n");
        assert_eq!(b.len(), 2, "{b:#?}");
        let Some(Block::Code { lines, .. }) = b.get(1) else {
            panic!("{b:#?}")
        };
        assert_eq!(lines, &["- a", "- b"]);

        // And a quoted fence's code that begins with `> ` keeps it.
        let b = lex("> ```\n> > nested quote\n> ```\n");
        let Some(Block::Code { lines, .. }) = b.first() else {
            panic!("{b:#?}")
        };
        assert_eq!(lines, &["> nested quote"]);
    }

    #[test]
    fn a_four_space_indented_fence_is_read_as_a_fence() {
        // CommonMark would make this literal text: ```rust / let a = 1; / ```
        let b = lex("    ```rust\n    let a = 1;\n    ```\n");
        let Some(Block::Code {
            lang,
            lines,
            closed,
        }) = b.first()
        else {
            panic!("{b:#?}")
        };
        assert_eq!(
            lang, "rust",
            "read as a fence, not as an indented code block"
        );
        assert_eq!(lines, &["let a = 1;"]);
        assert!(*closed);
        // And it is one block, not three lines of prose with the markers showing.
        assert_eq!(b.len(), 1, "{b:#?}");
    }

    #[test]
    fn the_box_art_that_broke_it_keeps_all_six_lines() {
        let src = "```\n┌─ rust                              ┌─ code\n│ fn main() {                        │ \
                   ```rust\n│     let xs: Vec<u32> = (0..5)…     │ let a = 1;\n│     println!(\"{xs:?}\");            │ \
                   ```\n│ }                                  └─\n└─\n```\n";
        let b = lex(src);
        assert_eq!(b.len(), 1, "{b:#?}");
        let Some(Block::Code { lines, closed, .. }) = b.first() else {
            panic!("{b:#?}")
        };
        assert!(closed);
        assert_eq!(lines.len(), 6, "{lines:#?}");
        assert!(lines[0].contains("┌─ rust"), "{lines:?}");
        assert!(
            lines[3].contains("```"),
            "the art's own backticks survive: {lines:?}"
        );
        assert_eq!(lines[5], "└─");
    }

    #[test]
    fn a_longer_fence_can_quote_a_shorter_one() {
        let b = lex("````\n```rust\nlet a = 1;\n```\n````\n");
        assert_eq!(b.len(), 1, "{b:#?}");
        let Some(Block::Code {
            lang,
            lines,
            closed,
        }) = b.first()
        else {
            panic!("{b:#?}")
        };
        assert_eq!(
            lang, "",
            "the info string is the outer fence's, and it has none"
        );
        assert_eq!(lines, &["```rust", "let a = 1;", "```"]);
        assert!(closed);
    }

    #[test]
    fn a_quoted_fence_closes_on_a_prefixed_line() {
        let src = "> ```rust\n> let a = 1;\n> ```\n\nafter\n";
        let b = lex(src);
        let Some(Block::Code { lines, .. }) = b.first() else {
            panic!("{b:#?}")
        };
        assert_eq!(lines, &["let a = 1;"]);
        // The prose after it is prose, not part of the code.
        assert_eq!(b.len(), 2, "{b:#?}");
        assert!(matches!(b[1], Block::Paragraph { .. }), "{b:#?}");
    }

    #[test]
    fn the_box_art_message_parses_whole() {
        const REAL: &str = include_str!("../../tests/fixtures/markdown/box-art-message.md");
        let b = lex(REAL);
        // The art is one block with all six lines, found rather than indexed.
        let art = b
            .iter()
            .find_map(|x| match x {
                Block::Code { lines, .. } if lines.iter().any(|l| l.starts_with("┌─ rust")) => {
                    Some(lines)
                }
                _ => None,
            })
            .unwrap_or_else(|| panic!("the art is not a code block: {b:#?}"));
        assert_eq!(art.len(), 6, "{art:#?}");
        assert_eq!(art[5], "└─", "{art:#?}");
        // The four-backtick fence that quotes a fence is its own block, markers kept.
        let quoted = b
            .iter()
            .find_map(|x| match x {
                Block::Code { lines, .. }
                    if lines.first().is_some_and(|l| l.starts_with("```rust")) =>
                {
                    Some(lines)
                }
                _ => None,
            })
            .unwrap_or_else(|| panic!("the quoting fence is not a code block: {b:#?}"));
        assert_eq!(quoted.as_slice(), &["```rust", "let a = 1;", "```"]);
        // Nothing is unclosed: an open fence would mean the tail had been swallowed.
        for block in &b {
            if let Block::Code { closed, lines, .. } = block {
                assert!(*closed, "an open fence swallowed the message: {lines:?}");
            }
        }
        // And nothing the model wrote is missing.
        let all: String = b
            .iter()
            .map(|x| match x {
                Block::Code { lines, .. } => lines.join("\n"),
                other => other.title(),
            })
            .collect::<Vec<_>>()
            .join("\n");
        for word in [
            "the current build draws",
            "That second box is the message",
            "Four backticks is markdown for",
            "a nested code box",
            "Everything else from the earlier reports",
        ] {
            assert!(all.contains(word), "{word:?} is missing");
        }
        for chunk in [1, 7, 64, 4096] {
            let a = crate::markdown::parse::tests::stream(REAL, chunk)
                .blocks()
                .cloned()
                .collect::<Vec<_>>();
            assert_eq!(a, b, "chunk {chunk}");
        }
    }
}
#[cfg(test)]
mod tail_cut_is_exact {
    use super::*;

    fn exact_below_the_first(src: &str, min: usize) {
        let Some(cut) = tail_cut(src, min) else {
            return;
        };
        let whole = lex(src);
        let tail = lex(&src[cut..]);
        if cut == 0 {
            assert_eq!(tail, whole, "a cut at 0 is the whole document");
            return;
        }
        assert!(!tail.is_empty(), "min {min}: an empty tail from {cut}");
        let exact = &tail[1..];
        let n = exact.len();
        assert!(
            n <= whole.len(),
            "min {min}: a tail from {cut} is longer than the document"
        );
        assert_eq!(
            exact,
            &whole[whole.len() - n..],
            "min {min}: below its first block, the tail from {cut} is not the document's"
        );
    }

    #[test]
    fn a_tail_cut_is_exact_below_its_first_block() {
        for min in [64usize, 128, 256, 512, 1024] {
            exact_below_the_first(
                include_str!("../../tests/fixtures/markdown/streamed-message.md"),
                min,
            );
            exact_below_the_first(
                include_str!("../../tests/fixtures/markdown/box-art-message.md"),
                min,
            );
        }
    }

    #[test]
    fn the_ragged_part_is_one_block() {
        let src = "para one line one\npara one line two\n\nsecond para\n\nthird para\n";
        let Some(cut) = tail_cut(src, 12) else {
            panic!("no cut")
        };
        assert!(cut > 0, "the cut is inside the document");
        let whole = lex(src);
        let tail = lex(&src[cut..]);
        assert_eq!(&tail[1..], &whole[whole.len() - (tail.len() - 1)..]);
    }

    #[test]
    fn a_cut_never_lands_inside_a_fence() {
        let src = include_str!("../../tests/fixtures/markdown/box-art-message.md");
        let spans: Vec<(usize, usize)> = fences_in(src).iter().map(|f| (f.open, f.end)).collect();
        for min in [16usize, 48, 128, 512] {
            let Some(cut) = tail_cut(src, min) else {
                continue;
            };
            assert!(
                !inside_a_fence(&spans, cut),
                "min {min}: cut {cut} is inside a fence ({spans:?})"
            );
        }
    }

    #[test]
    fn a_cut_is_always_on_a_char_boundary() {
        let src = "日本語のテキストです\n\nsecond 段落 here\n\nthird one\n\nfourth\n";
        for min in [0usize, 4, 8, 16, 32, 100, 1000] {
            if let Some(cut) = tail_cut(src, min) {
                assert!(
                    src.is_char_boundary(cut),
                    "min {min}: cut {cut} is mid-char"
                );
                // And the result is usable, which a mid-char offset would not be.
                let _ = lex(&src[cut..]);
            }
        }
    }

    #[test]
    fn a_cut_inside_a_loose_list_still_numbers_the_items_right() {
        let src = "1. first\n\n2. second\n\n3. third\n\ntail para\n";
        let Some(cut) = tail_cut(src, 8) else {
            panic!("no cut")
        };
        let tail = lex(&src[cut..]);
        let whole = lex(src);
        let starts = |bs: &[Block]| -> Vec<usize> {
            bs.iter()
                .filter_map(|b| match b {
                    Block::List {
                        ordered: true,
                        start,
                        ..
                    } => Some(*start),
                    _ => None,
                })
                .collect()
        };
        // No list in the tail begins with a number the document never writes there.
        let written: Vec<usize> = starts(&whole);
        for s in starts(&tail) {
            assert!(
                written.contains(&s),
                "the tail numbers a list from {s}; the document writes {written:?}"
            );
        }
        // And the exact part is the document's.
        assert_eq!(&tail[1..], &whole[whole.len() - (tail.len() - 1)..]);
    }

    #[test]
    fn the_edges_are_whole_or_nothing() {
        assert_eq!(tail_cut("", 100), Some(0));
        assert_eq!(tail_cut("one line", 100), Some(0));
        assert!(tail_cut("a\n\nb\n", 0).is_some());
        let long = "para\n\n".repeat(200);
        assert_eq!(tail_cut(&long, long.len() + 1), Some(0));
        // A single enormous block has no boundary; `None` is the honest answer.
        let one_block = "x".repeat(500);
        assert_eq!(tail_cut(&one_block, 10), None);
    }

    #[test]
    #[ignore]
    fn a_tail_is_far_cheaper_than_the_whole() {
        let src = include_str!("../../tests/fixtures/markdown/streamed-message.md");
        let big = src.repeat(44); // ~150 KB
        let t = std::time::Instant::now();
        let whole = lex(&big);
        let whole_us = t.elapsed().as_secs_f64() * 1e6;
        for min in [4096usize, 16384] {
            let t = std::time::Instant::now();
            let cut = tail_cut(&big, min).expect("a cut");
            let tail = lex(&big[cut..]);
            let us = t.elapsed().as_secs_f64() * 1e6;
            eprintln!(
                "{} bytes: whole {} blocks {whole_us:.0} us; tail {min} -> {} blocks {us:.0} us ({:.1}%)",
                big.len(),
                whole.len(),
                tail.len(),
                us * 100.0 / whole_us
            );
            assert!(us < whole_us / 4.0, "the tail was not much cheaper");
        }
    }
}

#[cfg(test)]
mod markdown_cost {
    use super::*;

    #[test]
    #[ignore]
    fn what_a_big_document_costs() {
        let src = include_str!("../../tests/fixtures/markdown/streamed-message.md");
        let big = src.repeat(500); // ~1.7 MB

        let t = std::time::Instant::now();
        let whole = lex(&big);
        let whole_ms = t.elapsed().as_secs_f64() * 1e3;

        let t = std::time::Instant::now();
        let mut m = IncrementalMarkdown::new();
        let mut start = 0usize;
        for (i, c) in big.char_indices() {
            if i - start >= 4096 && c.is_ascii() {
                m.push(&big[start..i]);
                start = i;
            }
        }
        m.push(&big[start..]);
        let inc_ms = t.elapsed().as_secs_f64() * 1e3;
        let blocks = m.blocks().count();
        let ratio = m.bytes_lexed() as f64 / big.len() as f64;
        eprintln!(
            "markdown cost: {} bytes | whole {whole_ms:.1} ms / {} blocks | incremental 4 KiB deltas {inc_ms:.1} ms / {blocks} blocks | bytes_lexed {} ({ratio:.3}x the doc) | lex_calls {} | window_len {} | stable {}",
            big.len(),
            whole.len(),
            m.bytes_lexed(),
            m.lex_calls(),
            m.window_len(),
            m.stable_count()
        );
        assert!(
            (1.0..4.0).contains(&ratio),
            "bytes_lexed is {ratio:.3}x the document"
        );
    }
}

#[cfg(test)]
mod a_stale_range_is_not_a_crash {
    use super::*;

    const REPRO: &str = "- \u{2026}\n```\n\u{2026}\u{2026}\u{2026}";

    fn in_deltas(doc: &str, k: usize) -> IncrementalMarkdown {
        let mut m = IncrementalMarkdown::new();
        let mut start = 0usize;
        let mut n = 0usize;
        for (i, _) in doc.char_indices() {
            if n == k {
                m.push(&doc[start..i]);
                start = i;
                n = 0;
            }
            n += 1;
        }
        m.push(&doc[start..]);
        m
    }

    fn text_of(blocks: &[Block]) -> String {
        let mut out = String::new();
        for b in blocks {
            match b {
                Block::Heading { runs, .. } => out.push_str(&runs_text(runs)),
                Block::Paragraph { lines } | Block::Quote { lines } => {
                    for l in lines {
                        out.push_str(&runs_text(l));
                        out.push('\n');
                    }
                }
                Block::Code { lines, .. } => {
                    for l in lines {
                        out.push_str(l);
                        out.push('\n');
                    }
                }
                Block::List { items, .. } => {
                    for i in items {
                        out.push_str(&runs_text(i));
                        out.push('\n');
                    }
                }
                Block::Table { head, rows, .. } => {
                    for l in head.iter().chain(rows.iter().flatten()) {
                        out.push_str(&runs_text(l));
                        out.push('\n');
                    }
                }
                Block::Rule => out.push_str("---\n"),
            }
        }
        out
    }

    #[test]
    fn the_repro_does_not_panic_at_any_delta_size() {
        assert_eq!(REPRO.len(), 19);
        for k in 1..=8usize {
            let m = in_deltas(REPRO, k);
            let blocks: Vec<Block> = m.blocks().cloned().collect();
            // The text is shown rather than dropped: a guard that skipped everything would render nothing.
            let text = text_of(&blocks);
            assert!(
                text.contains('\u{2026}'),
                "delta {k} rendered nothing: {text:?} from {blocks:?}"
            );
        }
    }

    #[test]
    fn the_same_text_is_fine_in_one_parse() {
        let blocks = lex(REPRO);
        assert!(!blocks.is_empty());
        assert!(text_of(&blocks).contains('\u{2026}'));
    }

    #[test]
    fn the_document_survives_being_cut_at_awkward_places() {
        let doc = format!("{REPRO}\n\n- {}\n", "\u{2026}".repeat(40));
        for k in [1usize, 2, 3, 5, 7, 13, 64] {
            let m = in_deltas(&doc, k);
            assert!(
                m.blocks().count() > 0,
                "delta {k} produced no blocks at all"
            );
        }
        assert!(text_of(&lex(&doc)).contains('\u{2026}'));
    }
}
