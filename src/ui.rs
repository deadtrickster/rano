use crate::buffer::Pos;
use crate::editor::Editor;
use crate::lsp;
use crate::prompt::{Prompt, prompt_label};
use crate::render::{Buffer, Line, Paragraph, Rect, Span, Style, Widget};
use crate::style::Role;
use crate::width;

/// Nano decorates the title bar, prompt bar and the key combos of the
/// function bar with plain reverse video (ncurses A_REVERSE, SGR 7). That is
/// what nano itself emits and it renders fine in tmux; the gray-on-gray bug
/// came from an explicit white-on-darkgray pair, not from reverse video.
/// [`Role::Bar`] is that reverse, named.
fn rev() -> Style {
    Style::of(Role::Bar)
}

/// The current row of a list or a popup: reverse too, but its own role.
fn sel() -> Style {
    Style::of(Role::Selected)
}

/// `line` in the one-row rectangle `r`, clipped to it.
fn put(buf: &mut Buffer, r: Rect, line: &Line) {
    line.render(r, buf);
}

/// `line` in `r` with `style` filling the whole of `r` under it — a bar that is
/// reversed to the edge of the pane, not only as far as its text reaches.
fn band(buf: &mut Buffer, r: Rect, line: Line, style: Style) {
    Paragraph::new(line).style(style).render(r, buf);
}

/// The prompt row, nano-style (winio.c): the label sits fixed at column 0
/// and only the answer scrolls horizontally to keep the cursor visible.
/// Returns the row text and the column the cursor belongs in.
fn prompt_text(p: &Prompt, width: usize) -> (String, usize) {
    let label = prompt_label(p.kind);
    let label_len = label.chars().count();
    let avail = width.saturating_sub(label_len + 1);
    let chars: Vec<char> = p.text.chars().collect();
    let cur = p.cursor.min(chars.len());
    let mut start = 0usize;
    if cur > avail {
        start = cur - avail;
    }
    if cur < start {
        start = cur;
    }
    let shown: String = chars[start..].iter().take(avail).collect();
    (format!("{}{}", label, shown), label_len + (cur - start))
}

/// The title bar's left label — `nano`'s corner banner.
///
/// Composed rather than a literal, because the literal it replaced said
/// `"rano 0.1.0"` and stayed saying it through two releases: the title bar was
/// advertising a version the binary was not. Anything a user reads has to come
/// from the one place the version is written down.
fn title_left() -> String {
    format!("  rano {}", env!("CARGO_PKG_VERSION"))
}

/// Gutter columns for a buffer of `rows` lines: right-aligned number plus one
/// trailing space, minimum two digits.
/// Short type tag for a completion row, from the LSP CompletionItemKind.
pub(crate) fn kind_tag(kind: u64) -> &'static str {
    match kind {
        2..=4 => "fn",
        5 | 10 => "fld",
        6 => "var",
        7 | 13 | 22 | 25 => "typ",
        8 => "trt",
        9 => "mod",
        12 | 21 => "const",
        14 => "kw",
        20 => "enm",
        _ => "",
    }
}

pub(crate) fn gutter_width(rows: usize) -> usize {
    std::cmp::max(2, rows.to_string().len()) + 1
}

/// Rendered width of `chars` with tabs advancing to the next multiple of
/// `tab_width` (`tab_width` 0 is treated as 1) and wide characters (CJK,
/// emoji) counting two columns. See [`crate::width`].
pub(crate) fn display_width(chars: &[char], tab_width: usize) -> usize {
    if width::is_simple(chars) {
        return chars.len();
    }
    width::width(chars, tab_width)
}

/// Inverse of `display_col`: the char index whose cell contains display
/// column `disp`. Clicking past the end of the line lands on the line
/// length, clicking inside a tab lands on the tab itself, and a cell inside
/// a wide character lands on that character — never inside the cluster.
pub(crate) fn char_at_display(line: &[char], disp: usize, tab_width: usize) -> usize {
    if width::is_simple(line) {
        return disp.min(line.len());
    }
    for c in width::clusters(line, tab_width) {
        if c.w > 0 && disp < c.disp + c.w {
            return c.start;
        }
    }
    line.len()
}

/// Display col of char index `col` within `line` (col clamped to line len).
///
/// Measured over the prefix's *clusters*, so a col that points inside a
/// ZWJ sequence reports the column its glyph starts at rather than one per
/// code point.
pub(crate) fn display_col(line: &[char], col: usize, tab_width: usize) -> usize {
    let col = col.min(line.len());
    if width::is_simple(&line[..col]) {
        return col;
    }
    width::width(&line[..col], tab_width)
}

/// The window of a KNOWN-simple row — every character one column and no tab,
/// so display col == char index and the window is a plain slice.
fn simple_window<'a>(
    line: &'a [char],
    lo: usize,
    hi: usize,
) -> impl Iterator<Item = (usize, char)> + 'a {
    let start = lo.min(line.len());
    let end = hi.min(line.len()).max(start);
    line[start..end]
        .iter()
        .copied()
        .enumerate()
        .map(move |(i, c)| (start + i, c))
}

/// Visible window of `line` as display cols `[d0, d1)`: `(absolute char col,
/// rendered char)` pairs with tabs expanded to spaces and wide characters
/// kept whole. A tab straddling `d0` is clipped into leading spaces.
///
/// This is the HORIZONTAL-SCROLL window (E3/F2), where the edges are display
/// columns the operator scrolled to and can therefore land mid-cluster. A
/// wrapped row instead asks [`seg_window`] for the characters of one of its
/// segments, which the wrap table already delimits.
fn text_window(line: &[char], d0: usize, d1: usize, tab_width: usize) -> Vec<(usize, char)> {
    let mut out = Vec::new();
    // Lazy, and it stops at `d1`: a window near the end of a long line must
    // not walk the whole line to find its edge.
    for c in width::Clusters::new(line, tab_width) {
        if c.disp >= d1 {
            break; // fully right of the window
        }
        if c.w == 0 {
            // Zero-width clusters only arise at the start of a line (after a
            // base they would have joined it). Emit their characters so a
            // base is never separated from them.
            if c.disp >= d0 {
                out.extend((c.start..c.end).map(|i| (i, line[i])));
            }
            continue;
        }
        if c.disp + c.w <= d0 {
            continue; // fully left of the window
        }
        if line[c.start] == '\t' {
            for _ in c.disp.max(d0)..(c.disp + c.w).min(d1) {
                out.push((c.start, ' '));
            }
        } else {
            out.extend((c.start..c.end).map(|i| (i, line[i])));
        }
    }
    out
}

/// The characters of ONE wrap segment: `line[lo..hi)` (both cluster
/// boundaries, straight out of the wrap table), rendered as if painting began
/// at display column `d0` — which is where that segment starts, so a tab
/// inside it expands to its true remaining advance and a wide character is
/// emitted whole.
///
/// This is the wrapped-row window, and it costs one pass over the segment
/// rather than a walk from the start of the line: a 500k-character row at a
/// deep visual row must not re-scan its own head every frame.
fn seg_window(
    line: &[char],
    lo: usize,
    hi: usize,
    d0: usize,
    tab_width: usize,
) -> Vec<(usize, char)> {
    let tw = tab_width.max(1);
    let start = lo.min(line.len());
    let end = hi.min(line.len()).max(start);
    let mut out = Vec::with_capacity(end.saturating_sub(start));
    let mut disp = d0;
    for (i, &c) in line[start..end].iter().enumerate() {
        let i = start + i;
        if c == '\t' {
            let w = tw - (disp % tw);
            for _ in 0..w {
                out.push((i, ' '));
            }
            disp += w;
        } else {
            disp += width::char_width(c);
            out.push((i, c));
        }
    }
    out
}

/// One rendered text line from an already-chosen window of `(char col,
/// rendered char)` pairs. Styles are resolved at ABSOLUTE positions (abs_row,
/// char col) via `char_style`, so search/selection/diagnostic lookups never
/// see window-relative coords. `diags` is THIS row's slice of the frame's
/// merged list — draw walks the sorted list once per frame (see
/// `diag_range`) — so no per-row filter. Consecutive equal styles coalesce
/// into a single span.
///
/// The window comes from [`seg_window`] for a wrapped row and from
/// [`text_window`] for a horizontally-scrolled one; both hand back whole
/// clusters, so a base is never painted without its combining marks.
fn line_to_spans(
    abs_row: usize,
    window: impl Iterator<Item = (usize, char)>,
    ed: &Editor,
    diags: &[lsp::Diagnostic],
) -> Line {
    let mut spans: Vec<Span> = Vec::new();
    let mut run = String::new();
    let mut run_style: Option<Style> = None;
    for (col, ch) in window {
        // A control character in the file is not drawn (the width layer counts
        // it as no column), and it must not reach a span as text: an ESC there
        // would start what the render core reads as an escape sequence and
        // drops whole, taking the visible characters after it with it.
        if ch.is_control() {
            continue;
        }
        let style = ed.char_style_with(Pos { row: abs_row, col }, diags);
        match run_style.take() {
            Some(s) if s == style => {
                run.push(ch);
                run_style = Some(s);
            }
            Some(s) => {
                spans.push(Span::styled(std::mem::take(&mut run), s));
                run_style = Some(style);
                run.push(ch);
            }
            None => {
                run.push(ch);
                run_style = Some(style);
            }
        }
    }
    if let Some(s) = run_style {
        spans.push(Span::styled(run, s));
    }
    Line::new(spans)
}

/// Most rows the live path suggestions take above a file-name prompt.
const HINT_ROWS: usize = 4;

/// Lay out path suggestions in rows of `width`, two spaces apart, at most
/// `max` rows; when they do not all fit, the last row ends with how many
/// were left out. Every row is padded to the full width.
pub(crate) fn hint_rows(names: &[String], width: usize, max: usize) -> Vec<String> {
    let mut rows: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut shown = 0;
    for n in names {
        let add = if cur.is_empty() {
            format!(" {n}")
        } else {
            format!("  {n}")
        };
        if cur.chars().count() + add.chars().count() <= width {
            cur.push_str(&add);
            shown += 1;
            continue;
        }
        if rows.len() + 1 == max || cur.is_empty() {
            break;
        }
        rows.push(std::mem::take(&mut cur));
        cur = format!(" {n}");
        shown += 1;
    }
    if !cur.is_empty() {
        rows.push(cur);
    }
    let left = names.len() - shown;
    if left > 0
        && let Some(last) = rows.last_mut()
    {
        let tail = format!("  (+{left})");
        let keep = width.saturating_sub(tail.chars().count());
        if last.chars().count() > keep {
            *last = last.chars().take(keep).collect();
        }
        last.push_str(&tail);
    }
    for r in &mut rows {
        let pad = width.saturating_sub(r.chars().count());
        r.push_str(&" ".repeat(pad));
    }
    rows
}

impl From<Rect> for crate::editor::Area {
    fn from(r: Rect) -> Self {
        Self::new(r.x, r.y, r.width, r.height)
    }
}

/// Draw the editor over the whole buffer: the standalone binary's case.
/// Returns where the terminal cursor goes, as for [`draw_in`].
pub fn draw(buf: &mut Buffer, ed: &Editor) -> Option<(u16, u16)> {
    let area = buf.area();
    draw_in(buf, area, ed)
}

/// Draw the editor inside `area` only, for a host that gives it a pane.
///
/// Every row the editor paints — title, text, status, function bar, and the
/// overlays drawn over them (completion, lists, diffs, help, M-x, which-key) —
/// is laid out from `area`'s own origin and clipped to it, so cells outside
/// are left as the host drew them. The terminal cursor is placed inside
/// `area` too, or not at all.
///
/// Every cell of `area` is painted — what the editor does not draw is blanked
/// first — so a host can reuse one buffer across frames.
///
/// Returns the terminal cursor's cell as **`(column, row)`**, absolute (in the
/// buffer's coordinates, like `area`), or `None` when the cursor should be
/// hidden: over a list, a diff or a help page, or when the edit point is
/// scrolled out of view. Note the order: `term::Terminal::draw_buffer` takes
/// `(row, column)`.
///
/// Mouse events are mapped through the area last given to
/// [`Editor::set_area`], not this one: drawing takes `&Editor` and cannot
/// record it. A host passes the same rectangle to both.
pub fn draw_in(buf: &mut Buffer, area: Rect, ed: &Editor) -> Option<(u16, u16)> {
    // Pane-relative rectangles: the layout below was written for a frame at
    // (0, 0), and this keeps it that way rather than threading `area.x` and
    // `area.y` through every row. The intersection is what makes "outside
    // the area is untouched" true even where a computed width runs over
    // (a popup clamped to `width`, a status message wider than the pane).
    let at = |x: u16, y: u16, w: u16, h: u16| {
        Rect::new(area.x.saturating_add(x), area.y.saturating_add(y), w, h).intersection(area)
    };
    let mut cursor: Option<(u16, u16)> = None;
    let mut cursor_at = |x: u16, y: u16| {
        cursor = Some((area.x.saturating_add(x), area.y.saturating_add(y)));
    };
    let width = area.width;
    // ratatui started every frame from a blank buffer; a reused one has the
    // last frame in it, and the rows below only paint where they have text.
    let whole = area.intersection(buf.area());
    buf.fill(whole, &Style::new());
    if area.height < 5 {
        Paragraph::new("Terminal too small").render(whole, buf);
        return None;
    }
    // title (1) + text + status (1) + bar (2)
    let text_h = (area.height - 4) as usize;
    let bs = ed.bs();

    // F4: line-number gutter shrinks the text viewport; E3: scroll_x is the
    // left edge of the text window in display cols.
    let g = if ed.show_line_numbers {
        gutter_width(bs.buf.row_count())
    } else {
        0
    };
    let view_w = (width as usize).saturating_sub(g);

    // Merged diagnostics, computed ONCE per frame: the per-character style
    // lookup and the gutter both read it. `all_diags` clones and sorts, so
    // running it per cell made scrolling cost scale with the diagnostic
    // count (measured: ~65 ms/frame at 500 diags, ~26 µs at zero).
    let diags = ed.all_diags();

    // (buffer row, wrap segment) of each visible VISUAL row. With wrap off,
    // a visual row IS a buffer row and seg is 0, so this is the old
    // `bs.scroll..bs.scroll + text_h` range.
    let vis: Vec<(usize, usize)> = if ed.wrap {
        let mut out = Vec::with_capacity(text_h);
        let (mut r, mut seg) = ed.buf_row_of_visual(bs.scroll);
        let mut first = true;
        while out.len() < text_h && r < bs.buf.row_count() {
            // Segment count from the cached wrap table (rebuilt once per
            // edit/resize by ensure_wrap_prefix) instead of re-scanning the
            // whole line every frame — a 500k-char line must not cost 500k
            // steps per frame. The fallback covers a missing table (tests
            // that draw without the run loop's adjust_scroll).
            let count = ed.seg_count(r);
            // The top line may start MID-way (seg > 0): emit only its
            // remaining segments. Emitting all `count` would add `seg`
            // phantom rows past the line's end — blank gaps that collapse
            // when the line scrolls off.
            let emit = if first {
                count.saturating_sub(seg)
            } else {
                count
            };
            for _ in 0..emit {
                if out.len() == text_h {
                    break;
                }
                out.push((r, seg));
                seg += 1;
            }
            first = false;
            r += 1;
            seg = 0;
        }
        // Pad past the end of the buffer so both loops below always emit
        // exactly text_h rows (a short buffer must not leave stale cells).
        while out.len() < text_h {
            out.push((usize::MAX, 0));
        }
        out
    } else {
        (bs.scroll..bs.scroll + text_h).map(|r| (r, 0)).collect()
    };

    // Per visible row: the row's slice of `diags` (sorted by line) and its
    // most severe severity for the gutter. One O(diags + rows) walk instead
    // of filtering the whole list per row (O(rows × diags) per frame).
    // `vis` rows are non-decreasing, so a single forward cursor suffices.
    let mut diag_range: Vec<(usize, usize)> = Vec::with_capacity(text_h);
    let mut diag_sev: Vec<Option<u64>> = Vec::with_capacity(text_h);
    {
        let mut it = 0usize;
        for (r, _) in &vis {
            while it < diags.len() && diags[it].line < *r {
                it += 1;
            }
            let start = it;
            let mut sev: Option<u64> = None;
            while it < diags.len() && diags[it].line == *r {
                sev = Some(sev.unwrap_or(diags[it].severity).min(diags[it].severity));
                it += 1;
            }
            diag_range.push((start, it));
            diag_sev.push(sev);
        }
    }

    // ---- title bar (row 0, reversed) ----
    let name = ed.title_text();
    // **`VIEW` says the buffer cannot be written**, which is the one thing about
    // it a reader must know before typing: a tailed log is being written by
    // something else, and an edit that cannot be saved is work they will lose
    // (TODO.md §20.6). Beside the mark, which is the other piece of state worth
    // seeing at a glance.
    let flags = match (bs.read_only, bs.mark.is_some()) {
        (true, true) => "VIEW M",
        (true, false) => "VIEW",
        (false, true) => "M",
        (false, false) => "",
    };
    put(
        buf,
        at(0, 0, width, 1),
        &Line::styled(title_line(width, &name, bs.buf.modified, flags), rev()),
    );

    // ---- gutter (rows 1..text_h, dim, right-aligned numbers) ----
    if g > 0 {
        let mut nums: Vec<Line> = Vec::with_capacity(text_h);
        for (i, (r, seg)) in vis.iter().enumerate() {
            // The number sits on the first wrap segment of a row only;
            // continuation rows stay blank (nano).
            let s = if *r < bs.buf.row_count() && *seg == 0 {
                format!("{:>w$} ", r + 1, w = g - 1)
            } else {
                " ".repeat(g)
            };
            // D6: rows with diagnostics carry their severity's color (the
            // most severe wins; the list merges tree-sitter + LSP diags).
            // Every wrap segment of such a row is colored.
            let role = match diag_sev[i] {
                Some(sev) => crate::editor::diag_role(sev),
                None => Role::Faint,
            };
            nums.push(Line::styled(s, role));
        }
        for (i, l) in nums.iter().enumerate() {
            put(buf, at(0, 1 + i as u16, g as u16, 1), l);
        }
    }

    // ---- text area (rows 1..text_h) ----
    // A wrap segment is the display range the wrap table records for it,
    // which is `seg * view_w` only while every character is one column wide;
    // a horizontal-scroll viewport is the same window at `scroll_x`.
    let mut lines: Vec<Line> = Vec::with_capacity(text_h);
    for (i, (r, seg)) in vis.iter().enumerate() {
        let (a, b) = diag_range[i];
        match bs.buf.row_opt(*r) {
            Some(chars) => {
                let line = match ed.row_is_simple(*r) {
                    // A simple row: display col == char index, so the
                    // segment's display range IS its char range and the
                    // window is a slice. This is the common case (ASCII
                    // source) and the one the per-frame cost is measured on.
                    Some(true) => {
                        let (lo, hi) = if ed.wrap {
                            ed.seg_chars(*r, *seg)
                        } else {
                            (bs.scroll_x, bs.scroll_x.saturating_add(view_w))
                        };
                        line_to_spans(*r, simple_window(chars, lo, hi), ed, &diags[a..b])
                    }
                    _ if ed.wrap => {
                        let (lo, hi) = ed.seg_chars(*r, *seg);
                        let (d0, _) = ed.seg_disp(*r, *seg);
                        line_to_spans(
                            *r,
                            seg_window(chars, lo, hi, d0, ed.tab_width).into_iter(),
                            ed,
                            &diags[a..b],
                        )
                    }
                    _ => {
                        let d1 = bs.scroll_x.saturating_add(view_w);
                        line_to_spans(
                            *r,
                            text_window(chars, bs.scroll_x, d1, ed.tab_width).into_iter(),
                            ed,
                            &diags[a..b],
                        )
                    }
                };
                lines.push(line);
            }
            None => lines.push(Line::default()),
        }
    }
    for (i, l) in lines.iter().enumerate() {
        put(buf, at(g as u16, 1 + i as u16, view_w as u16, 1), l);
    }

    // ---- completion popup (LSP) ----
    // A small unadorned block below the word being completed (above it when
    // near the bottom); the selected row is reversed, nano-style.
    if let Some(p) = &ed.completion
        && !p.items.is_empty()
    {
        let vis = p.items.len().min(8);
        let label_w = p
            .items
            .iter()
            .map(|it| it.label.chars().count())
            .max()
            .unwrap_or(0)
            .min(40);
        let w = (label_w + 6).clamp(10, (width as usize).min(60));
        // M-\: anchor to the word's VISUAL row, not its buffer row.
        let drow = if ed.wrap {
            ed.visual_pos(Pos {
                row: p.row,
                col: p.col,
            }) as i64
                - bs.scroll as i64
        } else {
            p.row as i64 - bs.scroll as i64
        };
        let vis_i = vis as i64;
        // Text rows start at pane row 1 (title offset): the word sits on
        // pane row drow+1, the popup goes just below it (or above when the
        // bottom would clip).
        let word_pane = drow + 1;
        let y0 = if word_pane + vis_i <= text_h as i64 {
            word_pane + 1
        } else {
            word_pane - vis_i
        };
        if y0 >= 1 && y0 + vis_i - 1 <= text_h as i64 {
            let line = bs.buf.row_opt(p.row).map(Vec::as_slice).unwrap_or(&[]);
            let disp = display_col(line, p.col, ed.tab_width);
            let x = if ed.wrap {
                // Offset within the word's own visual row, not modulo the
                // viewport: a row of wide characters wraps short of it.
                (g + ed.disp_in_seg(p.row, p.col)) as u16
            } else {
                (g + disp.saturating_sub(bs.scroll_x)) as u16
            };
            let x = x.min(width.saturating_sub(w as u16));
            // Keep the selected row inside an 8-row window.
            let start = if p.items.len() > vis {
                (p.sel + 1).saturating_sub(vis).min(p.items.len() - vis)
            } else {
                0
            };
            for i in 0..vis {
                let it = &p.items[start + i];
                let label: String = it.label.chars().take(label_w).collect();
                let pad = w - 2 - label.chars().count() - kind_tag(it.kind).len();
                let style = if i + start == p.sel {
                    sel()
                } else {
                    Style::new()
                };
                let text = format!(" {label}{}{}", " ".repeat(pad), kind_tag(it.kind));
                put(
                    buf,
                    at(x, y0 as u16 + i as u16, w as u16, 1),
                    &Line::styled(text, style),
                );
            }
        }
    }

    // ---- list overlay (M-L buffers, M-? usages) ----
    // Covers the text area: a reversed header naming the list and its keys,
    // then one row per item with the selection reversed, scrolled so the
    // selection stays in view.
    if let Some(p) = &ed.picker {
        let area_text = at(0, 1, width, text_h as u16);
        buf.fill(area_text, &Style::new());
        let header = format!(" {}   Enter: go  Esc: close", p.title);
        band(buf, at(0, 1, width, 1), Line::from(header), rev());
        let rows = crate::picker::list_rows(text_h);
        let start = (p.sel + 1).saturating_sub(rows);
        for (k, it) in p.items.iter().skip(start).take(rows).enumerate() {
            let style = if start + k == p.sel {
                sel()
            } else {
                Style::new()
            };
            band(
                buf,
                at(0, 2 + k as u16, width, 1),
                Line::from(format!(" {}", it.label)),
                style,
            );
        }
    }

    // ---- external-change diff (over the text, like the list) ----
    // A reversed header naming the file, the view and the keys, then the
    // rendered diff lines from `top`. The lines are already styled by the
    // library's renderers; they are drawn as they are.
    if let Some(v) = &ed.diff_view {
        let area_text = at(0, 1, width, text_h as u16);
        buf.fill(area_text, &Style::new());
        band(buf, at(0, 1, width, 1), Line::from(v.header()), rev());
        let rows = crate::diffview::body_rows(text_h);
        for (k, l) in v.lines.iter().skip(v.top).take(rows).enumerate() {
            put(buf, at(0, 2 + k as u16, width, 1), l);
        }
    }

    // ---- status line (row height-3) ----
    // nano (winio.c:statusline): the prompt bar is a full reverse strip with
    // the label and answer left-aligned at column 0; a plain message sits
    // centered with only the bracketed text reversed.
    let status_row = area.height - 3;
    if let Some(p) = &ed.prompt {
        // The prompt owns the status row, so what goes with it is drawn
        // above it, over the bottom text rows: the live path suggestions,
        // or a message raised while it is open ("No match").
        let flash = ed
            .status
            .as_ref()
            .filter(|fl| fl.until > std::time::Instant::now())
            .map(|fl| vec![format!(" {}", fl.text)]);
        let hints = ed
            .prompt_hints
            .as_ref()
            .filter(|(k, t, n)| *k == p.kind && *t == p.text && !n.is_empty())
            .map(|(_, _, n)| hint_rows(n, width as usize, text_h.min(HINT_ROWS)));
        if let Some(rows) = flash.or(hints) {
            let top = status_row - rows.len() as u16;
            for (k, r) in rows.iter().enumerate() {
                band(
                    buf,
                    at(0, top + k as u16, width, 1),
                    Line::from(r.as_str()),
                    rev(),
                );
            }
        }
        let (text, _) = prompt_text(p, width as usize);
        let mut s: Vec<char> = text.chars().collect();
        s.resize(width as usize, ' ');
        put(
            buf,
            at(0, status_row, width, 1),
            &Line::styled(s.into_iter().collect::<String>(), rev()),
        );
    } else {
        // Outside prompts the cursor position sits at the right edge of the
        // status row (1-based); transient messages and the LSP summary are
        // centered in the space left of it. nano centers the message and
        // wraps it in "[ ... ]" when it fits with room to spare (start_col
        // > 1); only the text is reversed.
        let cur = &ed.bs().cursor;
        let pos_txt = format!("Ln {}, Col {}", cur.row + 1, cur.col + 1);
        let pos_w = pos_txt.chars().count();
        if let Some(msg) = ed.status_text() {
            let avail = (width as usize).saturating_sub(pos_w + 1);
            let len = msg.chars().count();
            let start = avail.saturating_sub(len) / 2;
            let (text, pos) = if start > 1 {
                (format!("[ {msg} ]"), start - 2)
            } else {
                (msg.clone(), start)
            };
            put(
                buf,
                at(0, status_row, (avail as u16).max(1), 1),
                &Line::new(vec![Span::raw(" ".repeat(pos)), Span::styled(text, rev())]),
            );
        }
        let x = (width as usize).saturating_sub(pos_w);
        put(
            buf,
            at(x as u16, status_row, pos_w as u16, 1),
            &Line::raw(pos_txt),
        );
    }

    // ---- function bar (last two rows, reversed) ----
    // nano's layout (global.c:shown_entries_for + winio.c:bottombars):
    //   total     = min(items, ((COLS + 40) / 20) * 2)
    //   per_row   = (total + 1) / 2
    //   itemwidth = COLS / per_row
    // filled column-major: item i -> row (i % 2), col (i / 2) * itemwidth.
    // The last column absorbs the leftover (COLS % itemwidth) slack.
    // What is in effect decides the bar (`Editor::bar_items`): the global
    // keys, a mode's own, or what can follow a pending prefix.
    let items = ed.bar_items();
    let total = items.len().min(((width as usize) + 40) / 20 * 2);
    if total > 0 {
        let per_row = total.div_ceil(2);
        let itemw = (width as usize) / per_row;
        if itemw > 0 {
            for r in 0..2u16 {
                let mut spans: Vec<Span> = Vec::with_capacity(per_row * 3);
                for c in 0..per_row {
                    let i = 2 * c + r as usize;
                    let w = if c + 1 >= per_row {
                        itemw + (width as usize) % itemw
                    } else {
                        itemw
                    };
                    if i < total {
                        let (k, l) = (&items[i].0, &items[i].1);
                        // nano post_one_key: the key combo is reversed, the
                        // separator space and the tag stay in the terminal's
                        // default colors; the tag is skipped when fewer than
                        // 2 columns remain for it.
                        let kw = k.chars().count();
                        let key: String = k.chars().take(w).collect();
                        spans.push(Span::styled(key, rev()));
                        let mut used = kw.min(w);
                        if w > used + 1 {
                            let tag: String = l.chars().take(w - used - 1).collect();
                            used += 1 + tag.chars().count();
                            spans.push(Span::raw(format!(" {}", tag)));
                        }
                        spans.push(Span::raw(" ".repeat(w - used)));
                    } else {
                        spans.push(Span::raw(" ".repeat(w)));
                    }
                }
                put(buf, at(0, area.height - 2 + r, width, 1), &Line::new(spans));
            }
        }
    }

    // ---- cursor ----
    // None over the list overlay (the selection is the reversed row) or over
    // the diff, which has nothing to point at.
    if let Some(p) = &ed.palette {
        let col = 4 + p.query.chars().count();
        cursor_at((col as u16).min(width.saturating_sub(1)), status_row);
    } else if ed.info.is_none() && ed.picker.is_none() && ed.diff_view.is_none() {
        if let Some(p) = &ed.prompt {
            // cursor sits right after the answer, which is left-aligned
            let (_, col) = prompt_text(p, width as usize);
            cursor_at((col as u16).min(width.saturating_sub(1)), status_row);
        } else {
            // M-\: the cursor's pane row is its VISUAL row.
            let cy = if ed.wrap {
                ed.visual_pos(bs.cursor) as i64 - bs.scroll as i64
            } else {
                bs.cursor.row as i64 - bs.scroll as i64
            };
            if cy >= 0 && (cy as u16) < text_h as u16 {
                // char col → display col → minus scroll_x (or the offset
                // within the wrap segment's own row) → plus gutter
                let line = bs
                    .buf
                    .row_opt(bs.cursor.row)
                    .map(Vec::as_slice)
                    .unwrap_or(&[]);
                let disp = match ed.row_is_simple(bs.cursor.row) {
                    Some(true) => bs.cursor.col.min(line.len()),
                    _ => display_col(line, bs.cursor.col, ed.tab_width),
                };
                let cx = if ed.wrap {
                    (g + ed.disp_in_seg(bs.cursor.row, bs.cursor.col)) as u16
                } else {
                    (g + disp.saturating_sub(bs.scroll_x)) as u16
                };
                cursor_at(cx.min(width.saturating_sub(1)), cy as u16 + 1);
            }
        }
    }

    // ---- help pages (over the text, like the list) ----
    if let Some(v) = &ed.info {
        let area_text = at(0, 1, width, text_h as u16);
        buf.fill(area_text, &Style::new());
        let header = format!(
            " {}   q: close  \u{2191}\u{2193} PgUp PgDn: scroll",
            v.title
        );
        band(buf, at(0, 1, width, 1), Line::from(header), rev());
        let rows = crate::diffview::body_rows(text_h);
        for (k, l) in v.lines.iter().skip(v.top).take(rows).enumerate() {
            put(buf, at(0, 2 + k as u16, width, 1), l);
        }
    }

    // ---- M-x: the query on the status row, candidates above it ----
    if let Some(p) = &ed.palette {
        put(
            buf,
            at(0, status_row, width, 1),
            &Line::styled(
                format!(
                    "M-x {:<w$}",
                    p.query,
                    w = (width as usize).saturating_sub(4)
                ),
                rev(),
            ),
        );
        let rows = text_h.min(14).min(p.items.len().max(1));
        let top = status_row.saturating_sub(rows as u16);
        buf.fill(at(0, top, width, rows as u16), &Style::new());
        if p.items.is_empty() {
            put(
                buf,
                at(0, top, width, 1),
                &Line::styled("  no command matches", Role::Faint),
            );
        }
        let start = (p.sel + 1).saturating_sub(rows);
        let tw = p
            .items
            .iter()
            .filter_map(|n| crate::commands::command(n))
            .map(|c| c.title.chars().count())
            .max()
            .unwrap_or(0);
        for (k, name) in p.items.iter().skip(start).take(rows).enumerate() {
            let Some(c) = crate::commands::command(name) else {
                continue;
            };
            let keys = ed.keys_for(name);
            let line = Line::new(vec![
                Span::raw(format!(" {:<tw$}  ", c.title)),
                Span::role(format!("{keys:<14} "), Role::Key),
                Span::role(c.doc.to_string(), Role::Faint),
            ]);
            // The selected row is reversed to the pane's edge, under the key
            // colour and the faint doc, which keep theirs.
            let style = if start + k == p.sel {
                sel()
            } else {
                Style::new()
            };
            band(buf, at(0, top + k as u16, width, 1), line, style);
        }
    }

    // ---- which-key: a pending prefix's card, above the status row ----
    if let Some((m, keys)) = ed.pending_card() {
        let nano = ed.config.nano_keys;
        let entries = crate::commands::entries_of(m, nano);
        let rows = crate::help::card_rows(&entries, width as usize, &Style::of(Role::Key));
        let n = rows.len().min(text_h.saturating_sub(1));
        let top = status_row.saturating_sub(n as u16 + 1);
        buf.fill(at(0, top, width, n as u16 + 1), &Style::new());
        let title = format!(
            " {} {}-   ESC: cancel",
            m.name,
            crate::help::notation(nano, &keys)
        );
        band(buf, at(0, top, width, 1), Line::from(title), rev());
        for (k, l) in rows.into_iter().take(n).enumerate() {
            put(buf, at(0, top + 1 + k as u16, width, 1), &l);
        }
    }
    cursor
}

/// Build the title bar string: program name left, file name centered in the
/// remaining space (minus a right-hand flag area), " *" after the name when
/// modified, state flags near the right edge.
fn title_line(width: u16, name: &str, modified: bool, flags: &str) -> String {
    let left = title_left();
    let mut s = vec![' '; width as usize];
    for (i, c) in left.chars().enumerate() {
        if i < s.len() {
            s[i] = c;
        }
    }
    if !name.is_empty() {
        let nl = name.chars().count();
        let region_end = s.len().saturating_sub(8);
        // Saturating: a name longer than the region would underflow here (a
        // debug-build panic, a release-build name that vanished) — it is
        // placed at the left edge and cut instead.
        let mut pos = (left.len() + region_end).saturating_sub(nl) / 2;
        if pos < left.len() {
            pos = left.len();
        }
        let take = nl.min(region_end.saturating_sub(pos));
        for (i, c) in name.chars().take(take).enumerate() {
            s[pos + i] = c;
        }
        if modified {
            let ast = pos + take + 1;
            if ast < s.len() {
                s[ast] = '*';
            }
        }
    }
    let flen = flags.chars().count();
    let fstart = s.len().saturating_sub(5).saturating_sub(flen);
    for (i, c) in flags.chars().enumerate() {
        s[fstart + i] = c;
    }
    s.into_iter().collect()
}

/// The title bar for a test, without a terminal: what `draw` would put on row 0.
#[cfg(test)]
pub(crate) fn title_for_test(ed: &mut crate::editor::Editor, width: u16) -> String {
    let bs = ed.bs();
    let flags = match (bs.read_only, bs.mark.is_some()) {
        (true, true) => "VIEW M",
        (true, false) => "VIEW",
        (false, true) => "M",
        (false, false) => "",
    };
    title_line(width, &ed.title_text(), bs.buf.modified, flags)
}

/// A frame drawn for a test: the cells and where the cursor went, read the way
/// the tests read ratatui's `TestBackend` (`screen[(x, y)].symbol`).
#[cfg(test)]
pub(crate) struct Screen {
    pub buf: Buffer,
    pub cursor: Option<(u16, u16)>,
}

#[cfg(test)]
impl Screen {
    /// `ed` drawn over a fresh `w` × `h` buffer.
    pub(crate) fn of(ed: &Editor, w: u16, h: u16) -> Screen {
        let mut buf = Buffer::empty(Rect::new(0, 0, w, h));
        let cursor = draw(&mut buf, ed);
        Screen { buf, cursor }
    }

    pub(crate) fn cell(&self, (x, y): (u16, u16)) -> Option<&crate::render::Cell> {
        self.buf.cell(x, y)
    }

    /// Row `y`'s text, full width.
    pub(crate) fn row(&self, y: u16) -> String {
        self.buf
            .to_plain_lines()
            .get((y - self.buf.area().y) as usize)
            .cloned()
            .unwrap_or_default()
    }
}

#[cfg(test)]
impl std::ops::Index<(u16, u16)> for Screen {
    type Output = crate::render::Cell;
    fn index(&self, at: (u16, u16)) -> &crate::render::Cell {
        self.cell(at).expect("a cell inside the screen")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ed(text: &str) -> Editor {
        let mut buf = crate::buffer::Buffer::new();
        if !text.is_empty() {
            buf.set_rows(text.lines().map(|l| l.chars().collect()).collect());
        }
        let mut ed = Editor::new(buf, crate::config::Config::default());
        // draw() assumes the run loop has kept the soft-wrap table fresh
        // (it calls adjust_scroll, which rebuilds it, before every frame).
        ed.ensure_wrap_prefix();
        ed
    }

    fn chars(s: &str) -> Vec<char> {
        s.chars().collect()
    }

    /// An editor over a NAMED buffer, so `detect` finds a language and the
    /// highlighter runs — the draw tests need coloured cells.
    fn ed_named(name: &str, text: &str) -> Editor {
        let mut buf = crate::buffer::Buffer::new();
        buf.set_rows(text.lines().map(|l| l.chars().collect()).collect());
        buf.name = Some(std::path::PathBuf::from(name));
        let mut ed = Editor::new(buf, crate::config::Config::default());
        ed.ensure_wrap_prefix();
        // The run loop highlights before it paints; a draw test that skipped
        // this would be asserting on an un-highlighted editor.
        ed.ensure_highlight();
        ed
    }

    /// A window for a one-column test row: display col == char index, so the
    /// range is a plain slice of the line.
    fn win(chars: &[char], lo: usize, hi: usize) -> Vec<(usize, char)> {
        simple_window(chars, lo, hi).collect()
    }

    // ---------- gutter_width ----------

    #[test]
    fn gutter_width_min_two_digits_plus_separator() {
        assert_eq!(gutter_width(0), 3);
        assert_eq!(gutter_width(1), 3);
        assert_eq!(gutter_width(9), 3);
        assert_eq!(gutter_width(10), 3);
        assert_eq!(gutter_width(99), 3);
        assert_eq!(gutter_width(100), 4);
    }

    // ---------- display_width / display_col ----------

    #[test]
    fn display_width_tabs() {
        assert_eq!(display_width(&chars("ab\t"), 8), 8);
        assert_eq!(display_width(&chars("a\t"), 4), 4);
        assert_eq!(display_width(&chars("\t\t"), 8), 16);
        assert_eq!(display_width(&chars(""), 8), 0);
        assert_eq!(display_width(&chars("a\tb"), 8), 9);
    }

    #[test]
    fn display_col_mapping() {
        let line = chars("ab\tcd");
        assert_eq!(display_col(&line, 0, 8), 0);
        assert_eq!(display_col(&line, 2, 8), 2);
        assert_eq!(display_col(&line, 3, 8), 8);
        assert_eq!(display_col(&line, 4, 8), 9);
        assert_eq!(display_col(&line, 6, 8), 10); // col clamped to len 5
        assert_eq!(display_col(&line, 100, 8), 10); // clamped to line len
    }

    // ---------- line_to_spans ----------

    #[test]
    fn line_to_spans_coalesces_unstyled() {
        let e = ed("abc");
        let line = line_to_spans(0, win(&chars("abc"), 0, 80).into_iter(), &e, &[]);
        assert_eq!(line.spans.len(), 1);
        assert_eq!(line.spans[0].content, "abc");
    }

    #[test]
    fn line_to_spans_selection_three_spans() {
        let mut e = ed("abc");
        e.bs_mut().mark = Some(Pos { row: 0, col: 1 });
        e.bs_mut().cursor = Pos { row: 0, col: 2 };
        let line = line_to_spans(0, win(&chars("abc"), 0, 80).into_iter(), &e, &[]);
        assert_eq!(line.spans.len(), 3);
        assert_eq!(line.spans[0].content, "a");
        assert_eq!(line.spans[1].content, "b");
        assert_eq!(line.spans[1].style, Style::of(Role::Selection));
        assert_eq!(line.spans[2].content, "c");
    }

    #[test]
    fn line_to_spans_empty_line() {
        let e = ed("");
        let line = line_to_spans(0, win(&chars(""), 0, 80).into_iter(), &e, &[]);
        assert_eq!(line.spans.len(), 0);
    }

    #[test]
    fn line_to_spans_clips_to_max_w() {
        let e = ed("abc");
        let line = line_to_spans(0, win(&chars("abc"), 0, 2).into_iter(), &e, &[]);
        assert_eq!(line.spans.len(), 1);
        assert_eq!(line.spans[0].content, "ab");
    }

    // The draw path hands line_to_spans the row's slice of the frame's
    // merged diagnostics (its diag_range walk); a diag on the row must
    // underline its range.
    #[test]
    fn line_to_spans_underlines_row_diagnostic() {
        let e = ed("abc");
        let diags = [
            lsp::Diagnostic {
                line: 0,
                col: 1,
                end_col: 3,
                message: "m".into(),
                severity: 1,
            },
            // A diag on another row: draw's per-row walk never hands it to
            // this row's slice.
            lsp::Diagnostic {
                line: 5,
                col: 0,
                end_col: 2,
                message: "m".into(),
                severity: 1,
            },
        ];
        let line = line_to_spans(0, win(&chars("abc"), 0, 80).into_iter(), &e, &diags[..1]);
        assert_eq!(line.spans.len(), 2);
        assert_eq!(line.spans[0].content, "a");
        assert_eq!(line.spans[0].style, Style::default());
        assert_eq!(line.spans[1].content, "bc");
        assert_eq!(line.spans[1].style, Style::of(Role::DiagError).underline());
    }

    // ---------- text_window ----------

    #[test]
    fn text_window_no_tabs_passthrough() {
        let line = chars("abcdefgh");
        let want: Vec<(usize, char)> = line.iter().enumerate().map(|(i, &c)| (i, c)).collect();
        assert_eq!(text_window(&line, 0, 80, 8), want);
        assert_eq!(
            text_window(&line, 5, 8, 8),
            vec![(5, 'f'), (6, 'g'), (7, 'h')]
        );
    }

    #[test]
    fn text_window_tab_straddling_scroll_x() {
        // a: disp 0, tab: disp 1..8, b: disp 8 — window [2, 6) is all tab
        let line = chars("a\tb");
        assert_eq!(
            text_window(&line, 2, 6, 8),
            vec![(1, ' '), (1, ' '), (1, ' '), (1, ' ')]
        );
    }

    #[test]
    fn text_window_exact_fill() {
        let line = chars("a\tb");
        let want = vec![
            (0, 'a'),
            (1, ' '),
            (1, ' '),
            (1, ' '),
            (1, ' '),
            (1, ' '),
            (1, ' '),
            (1, ' '),
            (2, 'b'),
        ];
        assert_eq!(text_window(&line, 0, 9, 8), want);
        assert_eq!(text_window(&line, 0, 100, 8), want);
    }

    // ---------- line_to_spans: window + tabs ----------

    #[test]
    fn line_to_spans_window_styles_absolute_cols() {
        let mut e = ed("abcdefgh");
        e.bs_mut().mark = Some(Pos { row: 0, col: 6 });
        e.bs_mut().cursor = Pos { row: 0, col: 7 };
        let line = line_to_spans(0, win(&chars("abcdefgh"), 5, 8).into_iter(), &e, &[]);
        assert_eq!(line.spans.len(), 3);
        assert_eq!(line.spans[0].content, "f");
        assert_eq!(line.spans[1].content, "g");
        assert_eq!(line.spans[1].style, Style::of(Role::Selection));
        assert_eq!(line.spans[2].content, "h");
    }

    // ---------- title_line / function-bar layout ----------

    #[test]
    fn title_line_survives_a_name_longer_than_the_bar() {
        let long = format!("/tmp/{}/f.txt", "d".repeat(120));
        let t = title_line(60, &long, false, "");
        assert_eq!(t.chars().count(), 60, "{t:?}");
    }

    #[test]
    fn title_line_left_center_right() {
        // Against the COMPILED version, not a literal. The assertion used to say
        // `"  rano 0.1.0"`, which is what made the drift invisible: the test and
        // the constant agreed with each other and with no release.
        let banner = format!("  rano {}", env!("CARGO_PKG_VERSION"));
        let t = title_line(80, "foo.rs", true, "auto");
        assert!(t.starts_with(&banner), "{t:?}");
        assert!(t.contains("foo.rs"));
        assert!(t.contains('*'));
        assert!(t.trim_end().ends_with("auto"));
        let t = title_line(80, "", false, "");
        assert!(t.starts_with(&banner), "{t:?}");
        assert!(!t.contains('*'));
        // A narrow terminal truncates the banner rather than panicking.
        let t = title_line(3, "x", false, "");
        assert_eq!(t.chars().count(), 3);
    }

    #[test]
    fn completion_popup_draws_with_selection() {
        let mut e = ed("pri\n");
        e.completion = Some(crate::editor::CompletionPopup {
            items: vec![
                crate::lsp::CompletionItem {
                    label: "print!".into(),
                    kind: 3,
                    insert: "print!".into(),
                    sort: "print!".into(),
                    filter: "print!".into(),
                },
                crate::lsp::CompletionItem {
                    label: "println!".into(),
                    kind: 3,
                    insert: "println!".into(),
                    sort: "println!".into(),
                    filter: "println!".into(),
                },
            ],
            sel: 1,
            row: 0,
            col: 0,
        });
        let terminal = Screen::of(&e, 40, 24);
        let buf = &terminal;
        let row = |y: u16| -> String {
            (0..40)
                .map(|x| buf.cell((x, y)).unwrap().symbol.as_str())
                .collect()
        };
        // The popup sits below the word's row (text starts at pane row 1):
        // item 0 at y=2, item 1 at y=3.
        assert!(row(2).contains("print!"));
        assert!(row(3).contains("println!"));
        // The selected row carries reverse video (popup starts at x = gutter).
        let cell = buf.cell((3, 3)).unwrap();
        assert_eq!(cell.style.top(), Role::Selected);
        // The fn kind tag renders at the row's right edge.
        assert!(row(2).contains("fn"));
    }

    #[test]
    fn idle_status_row_shows_cursor_position() {
        let mut e = ed("hello\nworld\n");
        e.bs_mut().cursor = Pos { row: 1, col: 3 };
        let terminal = Screen::of(&e, 40, 24);
        let buf = &terminal;
        let row = |y: u16| -> String {
            (0..40)
                .map(|x| buf.cell((x, y)).unwrap().symbol.as_str())
                .collect()
        };
        // Status row = height-3 = 21: "Ln 2, Col 4" right-aligned.
        assert!(row(21).ends_with("Ln 2, Col 4"), "row21={:?}", row(21));
        // A transient message centers left of it; both are visible.
        e.flash("saved");
        let terminal = Screen::of(&e, terminal.buf.area().width, terminal.buf.area().height);
        let buf = &terminal;
        let row = |y: u16| -> String {
            (0..40)
                .map(|x| buf.cell((x, y)).unwrap().symbol.as_str())
                .collect()
        };
        assert!(row(21).contains("saved"));
        assert!(row(21).ends_with("Ln 2, Col 4"), "row21={:?}", row(21));
    }

    #[test]
    fn function_bar_layout_at_width_80() {
        // nano's algorithm: total = min(29, ((80+40)/20)*2) = 12 items,
        // per_row 6, itemw 13, column-major (item 1 lands bottom-left).
        let e = ed("x");
        let terminal = Screen::of(&e, 80, 24);
        let buf = &terminal;
        let row = |y: u16| -> String {
            (0..80)
                .map(|x| buf.cell((x, y)).unwrap().symbol.as_str())
                .collect()
        };
        let top = row(22);
        let bottom = row(23);
        // Emacs notation by default; the help key is a prefix.
        assert!(top.starts_with("C-g Help…"), "{top}");
        assert!(top.contains("C-o Write Out"), "{top}");
        assert!(bottom.starts_with("C-x Exit"), "{bottom}");
        assert!(bottom.contains("C-r Read File"), "{bottom}");
        // key_notation = nano: nano's own bar.
        let mut e = ed("x");
        e.config.nano_keys = true;
        let terminal = Screen::of(&e, terminal.buf.area().width, terminal.buf.area().height);
        let buf = &terminal;
        let row = |y: u16| -> String {
            (0..80)
                .map(|x| buf.cell((x, y)).unwrap().symbol.as_str())
                .collect()
        };
        assert!(row(22).starts_with("^G Help…"), "{}", row(22));
        assert!(row(23).starts_with("^X Exit"), "{}", row(23));
    }

    #[test]
    fn draw_gutter_colors_diagnostic_rows() {
        let mut e = ed("aa\nbb\ncc");
        e.show_line_numbers = true;
        e.bs_mut().lsp_diags = vec![
            crate::lsp::Diagnostic {
                line: 0,
                col: 0,
                end_col: 1,
                message: "e".into(),
                severity: 1,
            },
            crate::lsp::Diagnostic {
                line: 1,
                col: 0,
                end_col: 1,
                message: "w".into(),
                severity: 2,
            },
        ];
        let terminal = Screen::of(&e, 40, 24);
        let buf = &terminal;
        let role = |y| buf.cell((1, y)).unwrap().style.top();
        assert_eq!(role(1), Role::DiagError);
        assert_eq!(role(2), Role::DiagWarning);
        // A row with no diagnostic is faint.
        assert_eq!(role(3), Role::Faint);
    }

    #[test]
    fn line_to_spans_tab_expands_with_tab_style() {
        // the tab's space run carries the tab char's (selection) style
        let mut e = ed("a\tb");
        e.bs_mut().mark = Some(Pos { row: 0, col: 1 });
        e.bs_mut().cursor = Pos { row: 0, col: 2 };
        let line = line_to_spans(
            0,
            text_window(&chars("a\tb"), 0, 80, 8).into_iter(),
            &e,
            &[],
        );
        assert_eq!(line.spans.len(), 3);
        assert_eq!(line.spans[0].content, "a");
        assert_eq!(line.spans[1].content, "       ");
        assert_eq!(line.spans[1].style, Style::of(Role::Selection));
        assert_eq!(line.spans[2].content, "b");
    }

    // ---------- draw (into a render::Buffer) ----------

    #[test]
    fn draw_gutter_layout_and_cursor() {
        let mut e = ed("aa\nbb\ncc\ndd\nee\nff\ngg\nhh\nii\njj\nkk\nll");
        e.show_line_numbers = true;
        e.bs_mut().cursor = Pos { row: 2, col: 1 };
        let terminal = Screen::of(&e, 40, 24);
        let g = gutter_width(12); // 3
        assert_eq!(g, 3);
        let buf = &terminal;
        assert_eq!(buf.cell((1, 1)).unwrap().symbol.as_str(), "1"); // " 1 "
        assert_eq!(buf.cell((2, 1)).unwrap().symbol.as_str(), " ");
        assert_eq!(buf.cell((3, 1)).unwrap().symbol.as_str(), "a"); // text at x = g
        assert_eq!(terminal.cursor, Some((4, 3))); // g + disp(1) - 0
    }

    #[test]
    fn draw_wraps_long_lines() {
        let mut e = ed(&format!("{}\nshort", "a".repeat(50)));
        e.show_line_numbers = true;
        e.text_w = 40; // match the backend; the run loop keeps these in sync
        e.ensure_wrap_prefix();
        e.bs_mut().cursor = Pos { row: 0, col: 40 };
        let terminal = Screen::of(&e, 40, 24);
        let buf = &terminal;
        // view_w = 40 - 3 = 37: pane row 1 renders 37 a's, pane row 2 the
        // remaining 13.
        assert_eq!(buf.cell((3, 1)).unwrap().symbol.as_str(), "a");
        assert_eq!(buf.cell((39, 1)).unwrap().symbol.as_str(), "a");
        assert_eq!(buf.cell((3, 2)).unwrap().symbol.as_str(), "a");
        assert_eq!(buf.cell((15, 2)).unwrap().symbol.as_str(), "a");
        assert_eq!(buf.cell((16, 2)).unwrap().symbol.as_str(), " ");
        // Gutter: the number sits on the first wrap segment only.
        assert_eq!(buf.cell((1, 1)).unwrap().symbol.as_str(), "1");
        assert_eq!(buf.cell((1, 2)).unwrap().symbol.as_str(), " ");
        assert_eq!(buf.cell((1, 3)).unwrap().symbol.as_str(), "2");
        // Rows past the end of the buffer stay blank (no stale cells).
        assert_eq!(buf.cell((3, 5)).unwrap().symbol.as_str(), " ");
        assert_eq!(buf.cell((1, 5)).unwrap().symbol.as_str(), " ");
        // Cursor at (0, 40): visual row 1, display col 40 → x = 3 + 40 % 37.
        assert_eq!(terminal.cursor, Some((6, 2)));
    }

    #[test]
    fn draw_no_gap_when_top_line_partially_scrolled() {
        // Line 0 is 847 chars = 11 segments at view_w 77. Scrolled to
        // segment 3, the top of the viewport is MID-line. The next line must
        // sit directly below the line's last segment — the old code emitted
        // all 11 segments (3 phantom rows past the end), leaving blank gaps
        // that collapsed when the line scrolled off.
        let mut e = ed(&format!("{}\nshort", "a".repeat(847)));
        e.show_line_numbers = true;
        e.text_w = 80;
        e.ensure_wrap_prefix();
        e.bs_mut().scroll = 3;
        let terminal = Screen::of(&e, 80, 40);
        let buf = &terminal;
        // view_w = 80 - 3 = 77. At scroll 3 the viewport shows segments
        // 3..10 (8 rows, y=1..8), then "short" at y=9 — no blank gap.
        assert_eq!(buf.cell((3, 1)).unwrap().symbol.as_str(), "a");
        assert_eq!(
            buf.cell((79, 8)).unwrap().symbol.as_str(),
            "a",
            "last segment row full"
        );
        assert_eq!(
            buf.cell((3, 9)).unwrap().symbol.as_str(),
            "s",
            "next line directly below"
        );
        assert_eq!(buf.cell((4, 9)).unwrap().symbol.as_str(), "h");
    }

    #[test]
    fn draw_wraps_tabbed_line() {
        // A tabbed row takes the slow text_window path (the wrap table says
        // "has tabs"); the tab must still expand to the next multiple of
        // tab_width and the row must wrap at the segment boundary.
        let mut e = ed(&format!("\t{}", "a".repeat(40)));
        e.show_line_numbers = true;
        e.text_w = 40;
        e.ensure_wrap_prefix();
        let terminal = Screen::of(&e, 40, 24);
        let buf = &terminal;
        // view_w = 40 - 3 = 37. Row 0: tab = display cols 0..8, then 40 a's
        // at 8..48 → segment 0 = 8 spaces + 29 a's, segment 1 = 11 a's.
        assert_eq!(buf.cell((3, 1)).unwrap().symbol.as_str(), " ");
        assert_eq!(buf.cell((10, 1)).unwrap().symbol.as_str(), " ");
        assert_eq!(buf.cell((11, 1)).unwrap().symbol.as_str(), "a");
        assert_eq!(buf.cell((39, 1)).unwrap().symbol.as_str(), "a");
        assert_eq!(buf.cell((3, 2)).unwrap().symbol.as_str(), "a");
        assert_eq!(buf.cell((13, 2)).unwrap().symbol.as_str(), "a");
        assert_eq!(buf.cell((14, 2)).unwrap().symbol.as_str(), " ");
        // Gutter number on the first segment only.
        assert_eq!(buf.cell((1, 1)).unwrap().symbol.as_str(), "1");
        assert_eq!(buf.cell((1, 2)).unwrap().symbol.as_str(), " ");
    }

    // ---------- width: what a character occupies ----------

    #[test]
    fn draw_colours_markdown_constructs_on_screen() {
        // Not just `style_at`: the palette has to reach the cells. A markdown
        // buffer with one of each construct, drawn to a buffer.
        let src = "# H\n\n**bold** `code` *it*\n\n```rust\nlet x = 1;\n```\n";
        let mut e = ed_named("x.md", src);
        e.show_line_numbers = false;
        let terminal = Screen::of(&e, 30, 24);
        let buf = &terminal;
        let cell = |x: u16, y: u16| buf.cell((x, y)).unwrap().style.clone();
        let want = |s: Style| s;
        use Style as S;
        // Row 3 (pane row 3): "**bold** `code` *it*" — the inner text of each
        // construct, and the delimiters, all distinct.
        assert_eq!(cell(2, 3), want(S::of(Role::Strong)), "strong text");
        assert_eq!(cell(10, 3), want(S::of(Role::Code)), "code span");
        assert_eq!(cell(17, 3), want(S::new().italic()), "emphasis");
        assert_eq!(cell(0, 3), want(S::of(Role::Punctuation)), "the delimiter");
        // The heading and the fenced body, on their own rows.
        assert_eq!(cell(2, 1), want(S::of(Role::TypeName)), "heading text");
        assert_eq!(cell(0, 6), want(S::of(Role::Code)), "fence body");
    }

    #[test]
    fn display_width_counts_wide_characters_as_two() {
        // A CJK line is twice as wide as its character count. Measuring one
        // column per character is what writes half of it off the screen.
        assert_eq!(display_width(&chars("中文"), 8), 4);
        assert_eq!(display_width(&chars("こんにちは"), 8), 10);
        assert_eq!(display_width(&chars("😀"), 8), 2);
        // Combining marks add nothing; a ZWJ sequence is one glyph.
        assert_eq!(display_width(&chars("e\u{301}"), 8), 1);
        assert_eq!(display_width(&chars("\u{1f468}\u{200d}\u{1f469}"), 8), 2);
        // Tabs still advance to the next stop, measured over the width so far.
        assert_eq!(display_width(&chars("中\t"), 8), 8);
    }

    #[test]
    fn display_col_and_char_at_display_agree_for_wide_rows() {
        let line = chars("中文字");
        // Char index → display col: two columns each.
        assert_eq!(display_col(&line, 0, 8), 0);
        assert_eq!(display_col(&line, 1, 8), 2);
        assert_eq!(display_col(&line, 3, 8), 6);
        // …and back: any cell of a wide character maps to that character.
        assert_eq!(char_at_display(&line, 0, 8), 0);
        assert_eq!(char_at_display(&line, 1, 8), 0, "the right half of 中");
        assert_eq!(char_at_display(&line, 2, 8), 1);
        assert_eq!(char_at_display(&line, 3, 8), 1);
        assert_eq!(char_at_display(&line, 99, 8), 3, "past EOL → line end");
    }

    #[test]
    fn char_at_display_never_lands_inside_a_cluster() {
        // "e◌́" is one column holding two code points. No display column maps
        // to the combining mark, so a click can never put the cursor between
        // the base and its accent; and col 1 (just after `e`) and col 2 (at
        // `x`) both report column 1, because there is only one cell there.
        let line = chars("e\u{301}x");
        assert_eq!(char_at_display(&line, 0, 8), 0);
        assert_eq!(
            char_at_display(&line, 1, 8),
            2,
            "the cell is `x`, not the accent"
        );
        assert_eq!(
            display_col(&line, 1, 8),
            1,
            "the cursor sits past the glyph"
        );
        assert_eq!(display_col(&line, 2, 8), 1, "…in the same cell as `x`");
    }

    #[test]
    fn draw_wraps_wide_characters_whole() {
        // 5 CJK characters = 10 columns in a 4-column view: three visual
        // rows of 2, 2 and 1 characters, and no row ever shows half a glyph.
        let mut e = ed("中文字语言\nx");
        e.show_line_numbers = false;
        e.text_w = 4;
        e.ensure_wrap_prefix();
        let terminal = Screen::of(&e, 4, 12);
        let buf = &terminal;
        let sym = |x: u16, y: u16| buf.cell((x, y)).unwrap().symbol.as_str().to_string();
        assert_eq!(sym(0, 1), "中");
        assert_eq!(sym(2, 1), "文");
        assert_eq!(sym(0, 2), "字");
        assert_eq!(sym(2, 2), "语");
        assert_eq!(sym(0, 3), "言");
        // The next buffer row starts on the row after the last segment.
        assert_eq!(sym(0, 4), "x");
    }

    #[test]
    fn draw_never_splits_a_combining_mark_from_its_base() {
        // Four one-column clusters in a ONE-column view: every row holds
        // exactly one cluster. If the wrap cut by character index instead of
        // by cluster — the old rule — a row would hold a bare accent and the
        // next would start with the base it belongs to.
        let mut e = ed(&format!("{}x", "e\u{301}".repeat(3)));
        e.show_line_numbers = false;
        e.text_w = 1;
        e.ensure_wrap_prefix();
        let terminal = Screen::of(&e, 1, 12);
        let buf = &terminal;
        // One cell per row, each holding a whole base+accent grapheme.
        for y in 1..=3 {
            assert_eq!(
                buf.cell((0, y)).unwrap().symbol.as_str(),
                "e\u{301}",
                "row {y} split the cluster"
            );
        }
        assert_eq!(buf.cell((0, 4)).unwrap().symbol.as_str(), "x");
    }

    #[test]
    fn draw_no_gutter_text_at_x0() {
        let mut e = ed("aa\nbb");
        e.show_line_numbers = false;
        let terminal = Screen::of(&e, 40, 24);
        let buf = &terminal;
        assert_eq!(buf.cell((0, 1)).unwrap().symbol.as_str(), "a");
        assert_eq!(terminal.cursor, Some((0, 1)));
    }

    // ---- drawing into a host's pane (draw_in) ----

    /// The pane the tests below draw into: away from every edge, so a row or
    /// column that ignored the origin would land visibly outside it.
    const PANE: Rect = Rect {
        x: 10,
        y: 5,
        width: 40,
        height: 20,
    };

    /// Paint the whole frame with `#`, then the editor into `PANE`, as a host
    /// with its own content around the pane would.
    fn draw_pane(e: &Editor) -> Screen {
        let mut buf = Buffer::empty(Rect::new(0, 0, 60, 30));
        for y in 0..30 {
            buf.set_str(0, y, &"#".repeat(60), &Style::new(), 60);
        }
        let cursor = draw_in(&mut buf, PANE, e);
        Screen { buf, cursor }
    }

    fn outside_is_untouched(t: &Screen) {
        let buf = t;
        for y in 0..30u16 {
            for x in 0..60u16 {
                if !PANE.contains(x, y) {
                    assert_eq!(
                        buf[(x, y)].symbol.as_str(),
                        "#",
                        "cell ({x}, {y}) outside the pane"
                    );
                }
            }
        }
    }

    #[test]
    fn draw_in_keeps_to_its_area_and_puts_the_cursor_inside() {
        let mut e = ed("aa\nbb\ncc\ndd");
        e.show_line_numbers = true;
        e.set_area(PANE.into());
        e.bs_mut().cursor = Pos { row: 2, col: 1 };
        let t = draw_pane(&e);
        outside_is_untouched(&t);
        let g = gutter_width(4) as u16;
        let buf = &t;
        // Text starts on the pane's second row, after its gutter.
        assert_eq!(buf[(PANE.x + g, PANE.y + 1)].symbol.as_str(), "a");
        assert_eq!(buf[(PANE.x + g, PANE.y + 3)].symbol.as_str(), "c");
        // The status row's position readout sits at the pane's right edge.
        let status: String = (PANE.x..PANE.right())
            .map(|x| {
                buf[(x, PANE.y + PANE.height - 3)]
                    .symbol
                    .as_str()
                    .to_string()
            })
            .collect();
        assert!(status.trim_end().ends_with("Ln 3, Col 2"), "{status:?}");
        assert_eq!(t.cursor, Some((PANE.x + g + 1, PANE.y + 1 + 2)));
    }

    #[test]
    fn draw_in_puts_a_prompt_and_its_cursor_on_the_panes_status_row() {
        let mut e = ed("text");
        e.set_area(PANE.into());
        e.prompt = Some(Prompt {
            kind: crate::prompt::PromptKind::Search,
            text: "ab".into(),
            cursor: 2,
        });
        let t = draw_pane(&e);
        outside_is_untouched(&t);
        let row = PANE.y + PANE.height - 3;
        let buf = &t;
        let line: String = (PANE.x..PANE.right())
            .map(|x| buf[(x, row)].symbol.as_str().to_string())
            .collect();
        assert!(line.starts_with("Search: ab"), "{line:?}");
        assert_eq!(t.cursor, Some((PANE.x + 10, row)));
    }

    #[test]
    fn draw_in_keeps_overlays_inside_too() {
        // The which-key card and the M-x list are drawn over the text from
        // the status row upwards; both are rows a full-frame layout put at
        // x = 0.
        let mut e = ed("text");
        e.set_area(PANE.into());
        e.handle_key(crate::term::KeyEvent::alt(crate::term::KeyCode::Char('x')));
        assert!(e.palette.is_some(), "M-x opens the palette");
        let t = draw_pane(&e);
        outside_is_untouched(&t);
    }

    #[test]
    fn a_click_is_mapped_through_the_panes_origin() {
        use crate::term::{Mods, MouseButton, MouseEvent, MouseKind};
        let mut e = ed("aa\nbb\ncc\ndd");
        e.show_line_numbers = false;
        e.set_area(PANE.into());
        let click = |x: u16, y: u16| MouseEvent {
            kind: MouseKind::Press(MouseButton::Left),
            x,
            y,
            mods: Mods::NONE,
        };
        // Pane row 3 is buffer row 2 (the title takes pane row 0).
        assert!(e.handle_mouse(click(PANE.x + 1, PANE.y + 3)));
        assert_eq!(e.bs().cursor, Pos { row: 2, col: 1 });
        // Outside the pane: not the editor's click, whatever lies under it.
        assert!(!e.handle_mouse(click(PANE.x - 1, PANE.y + 1)));
        assert!(!e.handle_mouse(click(PANE.x + 1, PANE.bottom())));
        assert_eq!(e.bs().cursor, Pos { row: 2, col: 1 });
    }

    #[test]
    fn set_area_sizes_the_text_viewport() {
        let mut e = ed("x");
        assert!(e.set_area(PANE.into()));
        assert_eq!((e.text_w, e.text_h), (40, 16));
        assert!(!e.set_area(PANE.into()), "the same area is not a change");
    }
}
