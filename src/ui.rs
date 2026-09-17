use crate::buffer::Pos;
use crate::editor::Editor;
use crate::prompt::{Prompt, prompt_label};
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
/// `tab_width` (`tab_width` 0 is treated as 1 to avoid division by zero).
pub(crate) fn display_width(chars: &[char], tab_width: usize) -> usize {
    let tw = tab_width.max(1);
    let mut w = 0;
    for &c in chars {
        if c == '\t' {
            w += tw - (w % tw);
        } else {
            w += 1;
        }
    }
    w
}

/// Inverse of `display_col`: the char index whose cell contains display
/// column `disp`. Clicking past the end of the line lands on the line
/// length; clicking inside a tab lands on the tab itself.
pub(crate) fn char_at_display(line: &[char], disp: usize, tab_width: usize) -> usize {
    let tw = tab_width.max(1);
    let mut w = 0usize;
    for (i, &c) in line.iter().enumerate() {
        let cw = if c == '\t' { tw - (w % tw) } else { 1 };
        if w + cw > disp {
            return i;
        }
        w += cw;
    }
    line.len()
}

/// Display col of char index `col` within `line` (col clamped to line len).
pub(crate) fn display_col(line: &[char], col: usize, tab_width: usize) -> usize {
    display_width(&line[..col.min(line.len())], tab_width)
}

/// Visible window of `line` as display cols `[scroll_x, scroll_x + view_w)`:
/// `(absolute char col, rendered char)` pairs with tabs expanded to spaces up
/// to the next multiple of `tab_width` (`tab_width` 0 is treated as 1). A tab
/// straddling `scroll_x` is clipped into leading spaces.
fn text_window(
    line: &[char],
    scroll_x: usize,
    view_w: usize,
    tab_width: usize,
) -> Vec<(usize, char)> {
    let tw = tab_width.max(1);
    let end = scroll_x.saturating_add(view_w);
    let mut out = Vec::new();
    let mut d = 0;
    for (i, &c) in line.iter().enumerate() {
        let w = if c == '\t' { tw - (d % tw) } else { 1 };
        let (lo, hi) = (d, d + w);
        d = hi;
        if hi <= scroll_x {
            continue; // fully left of the window
        }
        if lo >= end {
            break; // fully right of the window
        }
        if c == '\t' {
            for _ in lo.max(scroll_x)..hi.min(end) {
                out.push((i, ' '));
            }
        } else {
            out.push((i, c));
        }
        if d >= end {
            break;
        }
    }
    out
}

/// One rendered text line. The window is display cols
/// `[scroll_x, scroll_x + view_w)` of `chars` (tabs expanded, E3/F2); styles
/// are resolved at ABSOLUTE positions (abs_row, char col) via `char_style`,
/// so search/selection/diagnostic lookups never see window-relative coords.
/// Consecutive equal styles coalesce into a single span.
fn line_to_spans(
    chars: &[char],
    abs_row: usize,
    scroll_x: usize,
    view_w: usize,
    tab_width: usize,
    ed: &Editor,
) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut run = String::new();
    let mut run_style: Option<Style> = None;
    for (col, ch) in text_window(chars, scroll_x, view_w, tab_width) {
        let style = ed.char_style(Pos { row: abs_row, col });
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
        for r in bs.scroll..bs.scroll + text_h {
            let s = if r < bs.buf.lines.len() {
                format!("{:>w$} ", r + 1, w = g - 1)
            } else {
                " ".repeat(g)
            };
            // D6: rows with diagnostics carry their severity's color (the
            // most severe wins; the list merges tree-sitter + LSP diags).
            let diags = ed.all_diags();
            let fg = match diags
                .iter()
                .filter(|d| d.line == r)
                .map(|d| d.severity)
                .min()
            {
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
    let mut lines: Vec<Line> = Vec::with_capacity(text_h);
    for r in bs.scroll..bs.scroll + text_h {
        match bs.buf.lines.get(r) {
            Some(chars) => lines.push(line_to_spans(
                chars,
                r,
                bs.scroll_x,
                view_w,
                ed.tab_width,
                ed,
            )),
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
        let drow = p.row as i64 - bs.scroll as i64;
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
            let x = (g + disp.saturating_sub(bs.scroll_x)) as u16;
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
            let cy = bs.cursor.row as i64 - bs.scroll as i64;
            if cy >= 0 && (cy as u16) < text_h as u16 {
                // char col → display col → minus scroll_x → plus gutter
                let line = bs
                    .buf
                    .lines
                    .get(bs.cursor.row)
                    .map(Vec::as_slice)
                    .unwrap_or(&[]);
                let disp = display_col(line, bs.cursor.col, ed.tab_width);
                let cx = (g + disp.saturating_sub(bs.scroll_x)) as u16;
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
        Editor::new(buf, crate::config::Config::default())
    }

    fn chars(s: &str) -> Vec<char> {
        s.chars().collect()
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
        let line = line_to_spans(&chars("abc"), 0, 0, 80, 8, &e);
        assert_eq!(line.spans.len(), 1);
        assert_eq!(line.spans[0].content, "abc");
    }

    #[test]
    fn line_to_spans_selection_three_spans() {
        let mut e = ed("abc");
        e.bs_mut().mark = Some(Pos { row: 0, col: 1 });
        e.bs_mut().cursor = Pos { row: 0, col: 2 };
        let line = line_to_spans(&chars("abc"), 0, 0, 80, 8, &e);
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
        let line = line_to_spans(&chars(""), 0, 0, 80, 8, &e);
        assert_eq!(line.spans.len(), 0);
    }

    #[test]
    fn line_to_spans_clips_to_max_w() {
        let e = ed("abc");
        let line = line_to_spans(&chars("abc"), 0, 0, 2, 8, &e);
        assert_eq!(line.spans.len(), 1);
        assert_eq!(line.spans[0].content, "ab");
    }

    // ---------- text_window ----------

    #[test]
    fn text_window_no_tabs_passthrough() {
        let line = chars("abcdefgh");
        let want: Vec<(usize, char)> = line.iter().enumerate().map(|(i, &c)| (i, c)).collect();
        assert_eq!(text_window(&line, 0, 80, 8), want);
        assert_eq!(
            text_window(&line, 5, 3, 8),
            vec![(5, 'f'), (6, 'g'), (7, 'h')]
        );
    }

    #[test]
    fn text_window_tab_straddling_scroll_x() {
        // a: disp 0, tab: disp 1..8, b: disp 8 — window [2, 6) is all tab
        let line = chars("a\tb");
        assert_eq!(
            text_window(&line, 2, 4, 8),
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
        let line = line_to_spans(&chars("abcdefgh"), 0, 5, 3, 8, &e);
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
        let line = line_to_spans(&chars("a\tb"), 0, 0, 80, 8, &e);
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
