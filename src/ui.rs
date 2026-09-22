use crate::buffer::Pos;
use crate::editor::Editor;
use crate::lsp;
use crate::prompt::{Prompt, prompt_label};
use crate::width;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

/// Nano decorates the title bar, prompt bar and the key combos of the
/// function bar with plain reverse video (ncurses A_REVERSE, SGR 7). That is
/// what nano itself emits and it renders fine in tmux; the gray-on-gray bug
/// came from an explicit white-on-darkgray pair, not from reverse video.
const fn rev() -> Style {
    Style::new().add_modifier(Modifier::REVERSED)
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

const TITLE_LEFT: &str = "  rano 0.1.0";

/// Bottom bar items. Ordered to match nano's column-major pairing: item `2c`
/// renders in the top bar row and item `2c+1` in the bottom bar row, so the
/// bar reads exactly like nano's (Help/Exit, WriteOut/ReadFile, ...). The
/// entries come from bindings::BAR so the bar and the help overlay can never
/// disagree about which key does what.
fn bar_items() -> Vec<(&'static str, &'static str)> {
    crate::bindings::BAR
        .iter()
        .map(|b| (b.key, b.label))
        .collect()
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
) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut run = String::new();
    let mut run_style: Option<Style> = None;
    for (col, ch) in window {
        let style = ed.char_style_with(Pos { row: abs_row, col }, diags);
        match run_style {
            Some(s) if s == style => run.push(ch),
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
    Line::from(spans)
}

pub fn draw(f: &mut Frame, ed: &Editor) {
    let area = f.area();
    let width = area.width;
    if area.height < 5 {
        f.render_widget(Paragraph::new("Terminal too small"), area);
        return;
    }
    // title (1) + text + status (1) + bar (2)
    let text_h = (area.height - 4) as usize;
    let bs = ed.bs();

    // F4: line-number gutter shrinks the text viewport; E3: scroll_x is the
    // left edge of the text window in display cols.
    let g = if ed.show_line_numbers {
        gutter_width(bs.buf.lines.len())
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
        while out.len() < text_h && r < bs.buf.lines.len() {
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
    let flags = if bs.mark.is_some() { "M" } else { "" };
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            title_line(width, &name, bs.buf.modified, flags),
            rev(),
        ))),
        Rect::new(0, 0, width, 1),
    );

    // ---- gutter (rows 1..text_h, dim, right-aligned numbers) ----
    if g > 0 {
        let mut nums: Vec<Line> = Vec::with_capacity(text_h);
        for (i, (r, seg)) in vis.iter().enumerate() {
            // The number sits on the first wrap segment of a row only;
            // continuation rows stay blank (nano).
            let s = if *r < bs.buf.lines.len() && *seg == 0 {
                format!("{:>w$} ", r + 1, w = g - 1)
            } else {
                " ".repeat(g)
            };
            // D6: rows with diagnostics carry their severity's color (the
            // most severe wins; the list merges tree-sitter + LSP diags).
            // Every wrap segment of such a row is colored.
            let fg = match diag_sev[i] {
                Some(1) => Color::Red,
                Some(2) => Color::Yellow,
                Some(_) => Color::Blue,
                None => Color::DarkGray,
            };
            nums.push(Line::from(Span::styled(s, Style::default().fg(fg))));
        }
        f.render_widget(
            Paragraph::new(nums),
            Rect::new(0, 1, g as u16, text_h as u16),
        );
    }

    // ---- text area (rows 1..text_h) ----
    // A wrap segment is the display range the wrap table records for it,
    // which is `seg * view_w` only while every character is one column wide;
    // a horizontal-scroll viewport is the same window at `scroll_x`.
    let mut lines: Vec<Line> = Vec::with_capacity(text_h);
    for (i, (r, seg)) in vis.iter().enumerate() {
        let (a, b) = diag_range[i];
        match bs.buf.lines.get(*r) {
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
    f.render_widget(
        Paragraph::new(lines),
        Rect::new(g as u16, 1, view_w as u16, text_h as u16),
    );

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
            let line = bs.buf.lines.get(p.row).map(Vec::as_slice).unwrap_or(&[]);
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
                    rev()
                } else {
                    Style::default()
                };
                let text = format!(" {label}{}{}", " ".repeat(pad), kind_tag(it.kind));
                f.render_widget(
                    Paragraph::new(Line::from(Span::styled(text, style))),
                    Rect::new(x, y0 as u16 + i as u16, w as u16, 1),
                );
            }
        }
    }

    // ---- status line (row height-3) ----
    // nano (winio.c:statusline): the prompt bar is a full reverse strip with
    // the label and answer left-aligned at column 0; a plain message sits
    // centered with only the bracketed text reversed.
    let status_row = area.height - 3;
    if let Some(p) = &ed.prompt {
        let (text, _) = prompt_text(p, width as usize);
        let mut s: Vec<char> = text.chars().collect();
        s.resize(width as usize, ' ');
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                s.into_iter().collect::<String>(),
                rev(),
            ))),
            Rect::new(0, status_row, width, 1),
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
            f.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::raw(" ".repeat(pos)),
                    Span::styled(text, rev()),
                ])),
                Rect::new(0, status_row, (avail as u16).max(1), 1),
            );
        }
        let x = (width as usize).saturating_sub(pos_w);
        f.render_widget(
            Paragraph::new(Line::from(Span::raw(pos_txt))),
            Rect::new(x as u16, status_row, pos_w as u16, 1),
        );
    }

    // ---- function bar (last two rows, reversed) ----
    // nano's layout (global.c:shown_entries_for + winio.c:bottombars):
    //   total     = min(items, ((COLS + 40) / 20) * 2)
    //   per_row   = (total + 1) / 2
    //   itemwidth = COLS / per_row
    // filled column-major: item i -> row (i % 2), col (i / 2) * itemwidth.
    // The last column absorbs the leftover (COLS % itemwidth) slack.
    let items = bar_items();
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
                        let (k, l) = items[i];
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
                f.render_widget(
                    Paragraph::new(Line::from(spans)),
                    Rect::new(0, area.height - 2 + r, width, 1),
                );
            }
        }
    }

    // ---- cursor ----
    if !ed.help {
        if let Some(p) = &ed.prompt {
            // cursor sits right after the answer, which is left-aligned
            let (_, col) = prompt_text(p, width as usize);
            f.set_cursor_position(((col as u16).min(width.saturating_sub(1)), status_row));
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
                    .lines
                    .get(bs.cursor.row)
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
                f.set_cursor_position((cx.min(width.saturating_sub(1)), cy as u16 + 1));
            }
        }
    }

    // ---- help overlay ----
    if ed.help {
        // Single source of truth: bindings::help_lines() (matches the bar).
        let text: Vec<String> = crate::bindings::help_lines();
        let lines: Vec<Line> = text
            .iter()
            .map(|l| {
                if l.is_empty() {
                    Line::default()
                } else if l.starts_with("  Press any key") {
                    Line::from(Span::styled(
                        l.to_string(),
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD),
                    ))
                } else {
                    Line::from(Span::raw(l.to_string()))
                }
            })
            .collect();
        f.render_widget(Paragraph::new(lines), area);
    }
}

/// Build the title bar string: program name left, file name centered in the
/// remaining space (minus a right-hand flag area), " *" after the name when
/// modified, state flags near the right edge.
fn title_line(width: u16, name: &str, modified: bool, flags: &str) -> String {
    let mut s = vec![' '; width as usize];
    for (i, c) in TITLE_LEFT.chars().enumerate() {
        if i < s.len() {
            s[i] = c;
        }
    }
    if !name.is_empty() {
        let nl = name.chars().count();
        let region_end = s.len().saturating_sub(8);
        let mut pos = (TITLE_LEFT.len() + region_end - nl) / 2;
        if pos < TITLE_LEFT.len() {
            pos = TITLE_LEFT.len();
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::buffer::Buffer;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::layout::Position;

    fn ed(text: &str) -> Editor {
        let mut buf = Buffer::new();
        if !text.is_empty() {
            buf.lines = text.lines().map(|l| l.chars().collect()).collect();
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
        let mut buf = Buffer::new();
        buf.lines = text.lines().map(|l| l.chars().collect()).collect();
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
        assert_eq!(
            line.spans[1].style,
            Style::default().fg(Color::White).bg(Color::DarkGray)
        );
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
        assert_eq!(
            line.spans[1].style,
            Style::default()
                .fg(Color::Red)
                .add_modifier(Modifier::UNDERLINED)
        );
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
        assert_eq!(
            line.spans[1].style,
            Style::default().fg(Color::White).bg(Color::DarkGray)
        );
        assert_eq!(line.spans[2].content, "h");
    }

    // ---------- title_line / function-bar layout ----------

    #[test]
    fn title_line_left_center_right() {
        let t = title_line(80, "foo.rs", true, "auto");
        assert!(t.starts_with("  rano 0.1.0"));
        assert!(t.contains("foo.rs"));
        assert!(t.contains('*'));
        assert!(t.trim_end().ends_with("auto"));
        let t = title_line(80, "", false, "");
        assert!(t.starts_with("  rano 0.1.0"));
        assert!(!t.contains('*'));
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
        let mut terminal = Terminal::new(TestBackend::new(40, 24)).unwrap();
        terminal.draw(|f| draw(f, &e)).unwrap();
        let buf = terminal.backend().buffer();
        let row = |y: u16| -> String {
            (0..40)
                .map(|x| buf.cell((x, y)).unwrap().symbol())
                .collect()
        };
        // The popup sits below the word's row (text starts at pane row 1):
        // item 0 at y=2, item 1 at y=3.
        assert!(row(2).contains("print!"));
        assert!(row(3).contains("println!"));
        // The selected row carries reverse video (popup starts at x = gutter).
        let cell = buf.cell((3, 3)).unwrap();
        assert!(
            cell.style()
                .add_modifier
                .contains(ratatui::style::Modifier::REVERSED)
        );
        // The fn kind tag renders at the row's right edge.
        assert!(row(2).contains("fn"));
    }

    #[test]
    fn idle_status_row_shows_cursor_position() {
        let mut e = ed("hello\nworld\n");
        e.bs_mut().cursor = Pos { row: 1, col: 3 };
        let mut terminal = Terminal::new(TestBackend::new(40, 24)).unwrap();
        terminal.draw(|f| draw(f, &e)).unwrap();
        let buf = terminal.backend().buffer();
        let row = |y: u16| -> String {
            (0..40)
                .map(|x| buf.cell((x, y)).unwrap().symbol())
                .collect()
        };
        // Status row = height-3 = 21: "Ln 2, Col 4" right-aligned.
        assert!(row(21).ends_with("Ln 2, Col 4"), "row21={:?}", row(21));
        // A transient message centers left of it; both are visible.
        e.flash("saved");
        terminal.draw(|f| draw(f, &e)).unwrap();
        let buf = terminal.backend().buffer();
        let row = |y: u16| -> String {
            (0..40)
                .map(|x| buf.cell((x, y)).unwrap().symbol())
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
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| draw(f, &e)).unwrap();
        let buf = terminal.backend().buffer();
        let row = |y: u16| -> String {
            (0..80)
                .map(|x| buf.cell((x, y)).unwrap().symbol())
                .collect()
        };
        let top = row(22);
        let bottom = row(23);
        assert!(top.starts_with("^G Help"));
        assert!(top.contains("^O Write Out"));
        assert!(bottom.starts_with("^X Exit"));
        assert!(bottom.contains("^R Read File"));
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
        let mut terminal = Terminal::new(TestBackend::new(40, 24)).unwrap();
        terminal.draw(|f| draw(f, &e)).unwrap();
        let buf = terminal.backend().buffer();
        assert_eq!(buf.cell((1, 1)).unwrap().fg, Color::Red);
        assert_eq!(buf.cell((1, 2)).unwrap().fg, Color::Yellow);
        assert_eq!(buf.cell((1, 3)).unwrap().fg, Color::DarkGray);
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
        assert_eq!(
            line.spans[1].style,
            Style::default().fg(Color::White).bg(Color::DarkGray)
        );
        assert_eq!(line.spans[2].content, "b");
    }

    // ---------- draw (TestBackend) ----------

    #[test]
    fn draw_gutter_layout_and_cursor() {
        let mut e = ed("aa\nbb\ncc\ndd\nee\nff\ngg\nhh\nii\njj\nkk\nll");
        e.show_line_numbers = true;
        e.bs_mut().cursor = Pos { row: 2, col: 1 };
        let mut terminal = Terminal::new(TestBackend::new(40, 24)).unwrap();
        terminal.draw(|f| draw(f, &e)).unwrap();
        let g = gutter_width(12); // 3
        assert_eq!(g, 3);
        let buf = terminal.backend().buffer();
        assert_eq!(buf.cell((1, 1)).unwrap().symbol(), "1"); // " 1 "
        assert_eq!(buf.cell((2, 1)).unwrap().symbol(), " ");
        assert_eq!(buf.cell((3, 1)).unwrap().symbol(), "a"); // text at x = g
        terminal
            .backend_mut()
            .assert_cursor_position(Position::new(4, 3)); // g + disp(1) - 0
    }

    #[test]
    fn draw_wraps_long_lines() {
        let mut e = ed(&format!("{}\nshort", "a".repeat(50)));
        e.show_line_numbers = true;
        e.text_w = 40; // match the backend; the run loop keeps these in sync
        e.ensure_wrap_prefix();
        e.bs_mut().cursor = Pos { row: 0, col: 40 };
        let mut terminal = Terminal::new(TestBackend::new(40, 24)).unwrap();
        terminal.draw(|f| draw(f, &e)).unwrap();
        let buf = terminal.backend().buffer();
        // view_w = 40 - 3 = 37: pane row 1 renders 37 a's, pane row 2 the
        // remaining 13.
        assert_eq!(buf.cell((3, 1)).unwrap().symbol(), "a");
        assert_eq!(buf.cell((39, 1)).unwrap().symbol(), "a");
        assert_eq!(buf.cell((3, 2)).unwrap().symbol(), "a");
        assert_eq!(buf.cell((15, 2)).unwrap().symbol(), "a");
        assert_eq!(buf.cell((16, 2)).unwrap().symbol(), " ");
        // Gutter: the number sits on the first wrap segment only.
        assert_eq!(buf.cell((1, 1)).unwrap().symbol(), "1");
        assert_eq!(buf.cell((1, 2)).unwrap().symbol(), " ");
        assert_eq!(buf.cell((1, 3)).unwrap().symbol(), "2");
        // Rows past the end of the buffer stay blank (no stale cells).
        assert_eq!(buf.cell((3, 5)).unwrap().symbol(), " ");
        assert_eq!(buf.cell((1, 5)).unwrap().symbol(), " ");
        // Cursor at (0, 40): visual row 1, display col 40 → x = 3 + 40 % 37.
        terminal
            .backend_mut()
            .assert_cursor_position(Position::new(6, 2));
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
        let mut terminal = Terminal::new(TestBackend::new(80, 40)).unwrap();
        terminal.draw(|f| draw(f, &e)).unwrap();
        let buf = terminal.backend().buffer();
        // view_w = 80 - 3 = 77. At scroll 3 the viewport shows segments
        // 3..10 (8 rows, y=1..8), then "short" at y=9 — no blank gap.
        assert_eq!(buf.cell((3, 1)).unwrap().symbol(), "a");
        assert_eq!(
            buf.cell((79, 8)).unwrap().symbol(),
            "a",
            "last segment row full"
        );
        assert_eq!(
            buf.cell((3, 9)).unwrap().symbol(),
            "s",
            "next line directly below"
        );
        assert_eq!(buf.cell((4, 9)).unwrap().symbol(), "h");
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
        let mut terminal = Terminal::new(TestBackend::new(40, 24)).unwrap();
        terminal.draw(|f| draw(f, &e)).unwrap();
        let buf = terminal.backend().buffer();
        // view_w = 40 - 3 = 37. Row 0: tab = display cols 0..8, then 40 a's
        // at 8..48 → segment 0 = 8 spaces + 29 a's, segment 1 = 11 a's.
        assert_eq!(buf.cell((3, 1)).unwrap().symbol(), " ");
        assert_eq!(buf.cell((10, 1)).unwrap().symbol(), " ");
        assert_eq!(buf.cell((11, 1)).unwrap().symbol(), "a");
        assert_eq!(buf.cell((39, 1)).unwrap().symbol(), "a");
        assert_eq!(buf.cell((3, 2)).unwrap().symbol(), "a");
        assert_eq!(buf.cell((13, 2)).unwrap().symbol(), "a");
        assert_eq!(buf.cell((14, 2)).unwrap().symbol(), " ");
        // Gutter number on the first segment only.
        assert_eq!(buf.cell((1, 1)).unwrap().symbol(), "1");
        assert_eq!(buf.cell((1, 2)).unwrap().symbol(), " ");
    }

    // ---------- width: what a character occupies ----------

    #[test]
    fn draw_colours_markdown_constructs_on_screen() {
        // Not just `style_at`: the palette has to reach the cells. A markdown
        // buffer with one of each construct, drawn to a TestBackend.
        let src = "# H\n\n**bold** `code` *it*\n\n```rust\nlet x = 1;\n```\n";
        let mut e = ed_named("x.md", src);
        e.show_line_numbers = false;
        let mut terminal = Terminal::new(TestBackend::new(30, 24)).unwrap();
        terminal.draw(|f| draw(f, &e)).unwrap();
        let buf = terminal.backend().buffer();
        let fg = |x: u16, y: u16| buf.cell((x, y)).unwrap().fg;
        // Row 3 (pane row 3): "**bold** `code` *it*" — the inner text of each
        // construct, and the delimiters, all distinct.
        assert_eq!(fg(2, 3), Color::Rgb(0xe5, 0xc0, 0x7b), "strong text");
        assert_eq!(fg(10, 3), Color::Rgb(0x56, 0xb6, 0xc2), "code span");
        assert_eq!(fg(17, 3), Color::Rgb(0x98, 0xc3, 0x79), "emphasis");
        assert_eq!(fg(0, 3), Color::Rgb(0xab, 0xbb, 0xbf), "the `**` delimiter");
        // The heading and the fenced body, on their own rows.
        assert_eq!(fg(2, 1), Color::Rgb(0xe5, 0xc0, 0x7b), "heading text");
        assert_eq!(fg(0, 6), Color::Rgb(0x56, 0xb6, 0xc2), "fence body");
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
        let mut terminal = Terminal::new(TestBackend::new(4, 12)).unwrap();
        terminal.draw(|f| draw(f, &e)).unwrap();
        let buf = terminal.backend().buffer();
        let sym = |x: u16, y: u16| buf.cell((x, y)).unwrap().symbol().to_string();
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
        let mut terminal = Terminal::new(TestBackend::new(1, 12)).unwrap();
        terminal.draw(|f| draw(f, &e)).unwrap();
        let buf = terminal.backend().buffer();
        // One cell per row, each holding a whole base+accent grapheme.
        for y in 1..=3 {
            assert_eq!(
                buf.cell((0, y)).unwrap().symbol(),
                "e\u{301}",
                "row {y} split the cluster"
            );
        }
        assert_eq!(buf.cell((0, 4)).unwrap().symbol(), "x");
    }

    #[test]
    fn draw_no_gutter_text_at_x0() {
        let mut e = ed("aa\nbb");
        e.show_line_numbers = false;
        let mut terminal = Terminal::new(TestBackend::new(40, 24)).unwrap();
        terminal.draw(|f| draw(f, &e)).unwrap();
        let buf = terminal.backend().buffer();
        assert_eq!(buf.cell((0, 1)).unwrap().symbol(), "a");
        terminal
            .backend_mut()
            .assert_cursor_position(Position::new(0, 1));
    }
}
