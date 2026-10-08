//! The two-panel diff: before on the left, after on the right.
//!
//! # Provenance
//!
//! Ported from letibot's `crates/ui/src/sidediff.rs`, whose shape is
//! **opencode**'s (MIT) diff viewer: a `split | unified` view, line numbers both
//! sides, the change carried by the sign column, and syntax colouring over the
//! whole panel. The pairing, the geometry and the edit script are letibot's,
//! unchanged; the medium is [`crate::render`] [`Line`]s instead of ANSI strings.
//!
//! That change removes a whole class of defect the ANSI version had to repair by
//! hand. There, a syntax span closed with a reset, and a reset ended the row's
//! tint at the first keyword — so every reset inside a tinted cell had to be
//! rewritten into "reset, then reopen the tint". Here a cell's spans are the
//! tint's [`Style`] with the syntax role *stacked over it*: the background is
//! simply still there.
//!
//! - The change is carried by the **sign**, a glyph, so it survives
//!   [`Palette::None`]. On a colour palette the row is also tinted inside its own
//!   role to the panel's full width — an added line green, a removed one red —
//!   while the text keeps its syntax foregrounds.
//! - Syntax colour is [`crate::syntax`]'s captures mapped through
//!   [`crate::highlight::role_for_capture`]: the capture → token decision is the
//!   grammar's, the capture → colour decision is the palette's.
//! - Split or unified is the **caller's** choice ([`edit_view`]), never the
//!   width's: a narrow pane gets a narrow split rather than no diff. The renderer
//!   degrades under [`MIN_BODY`]; it does not refuse.
//!
//! # Everything here is a pure function of its inputs
//!
//! No clock, no filesystem, no terminal. The tree-sitter parse is deterministic
//! in the source text, so the output is too.

use crate::render::{Line, Span, Style};

use crate::diff::{
    DiffConfig, GAVE_UP, Row, TAB_STOP, diff_lines, expand_tabs, faint_line, hunk_header, hunks,
    spans_width, wrap_cells,
};
use crate::highlight::role_grid;
use crate::style::Role;
use crate::syntax::{self, Lang};

/// How the two panels are coloured, and where their numbers start.
pub struct SplitConfig<'a> {
    /// Width, palette, line numbers, context and the row cap come from the
    /// same config the unified renderer uses; `width` is the **full** row and
    /// the panels split it.
    pub cfg: &'a DiffConfig,
    /// 1-based line of the old file the left panel starts at, so the gutter
    /// numbers the file and not the excerpt.
    pub before_start: usize,
    /// 1-based line of the new file the right panel starts at.
    pub after_start: usize,
    /// The language both panels are coloured in, from the file's name. `None`
    /// renders plain.
    pub lang: Option<Lang>,
}

/// The separator between the panels, and the air either side of it.
const SEP: &str = " │ ";
const SEP_W: usize = 3;
/// A panel body narrower than this cannot show code and its gutter at the
/// same time; the renderer degrades rather than overprints.
const MIN_BODY: usize = 8;

/// Render the two-panel view of `old` → `new`.
///
/// One output row per terminal row: left gutter, sign and code, the separator,
/// right gutter, sign and code. A line that wraps keeps its continuation under
/// its own panel and blanks the other, so the eye reads the pair and not the
/// wrap.
pub fn render_split(old: &[&str], new: &[&str], sc: &SplitConfig) -> Vec<Line> {
    let p = sc.cfg.palette;
    // **Tabs first, once, before anything colours or measures a line**, so the
    // class grid (built from these same strings) and the line are one grid.
    let old_x: Vec<String> = old.iter().map(|l| expand_tabs(l, TAB_STOP)).collect();
    let new_x: Vec<String> = new.iter().map(|l| expand_tabs(l, TAB_STOP)).collect();
    let old: Vec<&str> = old_x.iter().map(String::as_str).collect();
    let new: Vec<&str> = new_x.iter().map(String::as_str).collect();
    let (old, new) = (&old[..], &new[..]);
    let d = diff_lines(old, new);
    let hs = hunks(&d, sc.cfg.context);
    let mut out = Vec::new();
    if hs.is_empty() {
        out.push(faint_line(p, "no change"));
        return out;
    }
    if d.degraded {
        out.push(Line::from(Span::styled(GAVE_UP, p.style(Role::Attention))));
    }

    let g = Geometry::of(sc, old, new);
    let old_classes = role_grid(old, sc.lang);
    let new_classes = role_grid(new, sc.lang);

    let mut budget = sc.cfg.max_rows;
    let mut dropped = 0usize;
    for h in hs.iter() {
        // A header before every hunk, including the only one.
        if budget == 0 {
            dropped += 1;
        } else {
            budget -= 1;
            out.push(faint_line(
                p,
                &hunk_header(h, sc.before_start, sc.after_start),
            ));
        }
        for pair in pair_rows(&h.rows) {
            let lines = render_pair(&pair, old, new, &old_classes, &new_classes, sc, &g);
            if budget >= lines.len() {
                budget -= lines.len();
                out.extend(lines);
            } else {
                // The pair does not fit whole, and half a pair is worse than
                // none: an aligned row with one panel missing reads as a
                // deletion or an insertion that never happened.
                dropped += lines.len();
                budget = 0;
            }
        }
    }
    if dropped > 0 {
        out.push(faint_line(
            p,
            &format!("… {dropped} more diff lines not shown"),
        ));
    }
    out
}

/// The row geometry, computed once per render.
struct Geometry {
    /// Visible width of the LEFT panel.
    ///
    /// **The two panels are not the same width when the division is odd**: the
    /// remainder goes to the right panel, so the two and the separator sum to
    /// exactly the width asked for — 48 + 3 + 49 = 100 — instead of throwing a
    /// column away.
    panel_w: usize,
    /// Visible width of the RIGHT panel.
    panel_w_right: usize,
    /// Visible width of the code part of a cell.
    body_w: usize,
    /// Digits in the largest line number either panel can show.
    numw: usize,
}

impl Geometry {
    fn of(sc: &SplitConfig, old: &[&str], new: &[&str]) -> Geometry {
        let available = sc.cfg.width.saturating_sub(SEP_W);
        let panel_w = available / 2;
        let panel_w_right = available - panel_w;
        let numw = if sc.cfg.line_numbers {
            (sc.before_start + old.len())
                .max(sc.after_start + new.len())
                .max(1)
                .to_string()
                .len()
        } else {
            0
        };
        let body_w = panel_w
            .saturating_sub(Self::gutter_w_for(sc, numw))
            .max(MIN_BODY);
        Geometry {
            panel_w,
            panel_w_right,
            body_w,
            numw,
        }
    }

    /// The gutter a cell carries before its code: number, space, sign, space —
    /// or just sign and space when line numbers are off.
    fn gutter_w_for(sc: &SplitConfig, numw: usize) -> usize {
        if sc.cfg.line_numbers { numw + 3 } else { 2 }
    }
}

/// One aligned pair: what the left panel shows and what the right shows.
/// `None` is an empty panel — a pure insertion has no left, a pure deletion
/// no right, and a wrap continuation blanks whichever side ran out.
struct Pair {
    left: Option<Half>,
    right: Option<Half>,
}

struct Half {
    /// 0-based index into the excerpt's lines.
    line: usize,
    sign: char,
    role: Role,
}

/// Align a hunk's rows into side-by-side pairs.
///
/// A run of removals pairs index-wise with the run of additions beside it, and
/// a context row pairs with itself. Leftovers pad with an empty panel, which is
/// what makes an insertion read as *appeared* rather than as *changed*.
fn pair_rows(rows: &[Row]) -> Vec<Pair> {
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < rows.len() {
        match rows[i] {
            Row::Context { a, b } => {
                out.push(Pair {
                    left: Some(Half {
                        line: a,
                        sign: ' ',
                        role: Role::Plain,
                    }),
                    right: Some(Half {
                        line: b,
                        sign: ' ',
                        role: Role::Plain,
                    }),
                });
                i += 1;
            }
            _ => {
                let mut removed = Vec::new();
                let mut added = Vec::new();
                while i < rows.len() {
                    match rows[i] {
                        Row::Removed { a } => removed.push(a),
                        Row::Added { b } => added.push(b),
                        Row::Context { .. } => break,
                    }
                    i += 1;
                }
                let n = removed.len().max(added.len());
                for k in 0..n {
                    out.push(Pair {
                        left: removed.get(k).map(|&a| Half {
                            line: a,
                            sign: '-',
                            role: Role::Removed,
                        }),
                        right: added.get(k).map(|&b| Half {
                            line: b,
                            sign: '+',
                            role: Role::Added,
                        }),
                    });
                }
            }
        }
    }
    out
}

/// One pair into one or more terminal rows.
fn render_pair(
    pair: &Pair,
    old: &[&str],
    new: &[&str],
    old_classes: &[Vec<Role>],
    new_classes: &[Vec<Role>],
    sc: &SplitConfig,
    g: &Geometry,
) -> Vec<Line> {
    // **Both lookups are guarded.** The class grid can be shorter than the
    // lines — `Highlighter::classes` returns an empty grid when a parser will
    // not take the language or a query will not compile — and a bare index
    // there panics on the first line of the first hunk. A short grid renders
    // plain instead.
    let left = pair.left.as_ref().map(|h| {
        (
            h,
            old.get(h.line).copied().unwrap_or(""),
            old_classes.get(h.line).map(Vec::as_slice).unwrap_or(&[]),
            sc.before_start + h.line,
        )
    });
    let right = pair.right.as_ref().map(|h| {
        (
            h,
            new.get(h.line).copied().unwrap_or(""),
            new_classes.get(h.line).map(Vec::as_slice).unwrap_or(&[]),
            sc.after_start + h.line,
        )
    });
    let left_lines = side_lines(left, sc, g, g.panel_w);
    let right_lines = side_lines(right, sc, g, g.panel_w_right);

    let rows = left_lines.len().max(right_lines.len());
    let sep = Span::styled(SEP, sc.cfg.palette.style(Role::Faint));
    (0..rows)
        .map(|k| {
            // A side that has run out of lines is blank, not a repeat of its
            // last line: a repeated row would read as content that is there
            // twice. Each half pads to its OWN width.
            let mut spans = left_lines
                .get(k)
                .cloned()
                .unwrap_or_else(|| blank(g.panel_w));
            spans.push(sep.clone());
            spans.extend(
                right_lines
                    .get(k)
                    .cloned()
                    .unwrap_or_else(|| blank(g.panel_w_right)),
            );
            Line::from(spans)
        })
        .collect()
}

fn blank(w: usize) -> Vec<Span> {
    vec![Span::raw(" ".repeat(w))]
}

/// One side of a pair: gutter, sign, code — styled, wrapped, one span list per
/// terminal row, each padded to `panel_w` (this side's own width). An absent
/// half is one blank row so the opposite side's wrap still has somewhere to go.
///
/// The whole cell sits **inside the line's own role** on a colour palette: the
/// tint is the base of every span in it — gutter, sign, code and the padding to
/// the panel edge — so an added line is green to its full width, not just where
/// its text reaches. A context row has no tint and pads plain.
fn side_lines(
    half: Option<(&Half, &str, &[Role], usize)>,
    sc: &SplitConfig,
    g: &Geometry,
    panel_w: usize,
) -> Vec<Vec<Span>> {
    let p = sc.cfg.palette;
    let Some((h, text, classes, num)) = half else {
        return vec![blank(panel_w)];
    };
    let tinted = h.role != Role::Plain && p.is_colour();
    let base = if tinted {
        p.style(h.role)
    } else {
        Style::new()
    };
    let cells: Vec<(char, Style)> = text
        .chars()
        .enumerate()
        .map(|(i, c)| {
            let role = classes.get(i).copied().unwrap_or(Role::Plain);
            (c, base.patch(&p.style(role)))
        })
        .collect();
    let gutter_w = Geometry::gutter_w_for(sc, g.numw) - 2;
    wrap_cells(&cells, g.body_w)
        .into_iter()
        .enumerate()
        .map(|(k, body)| {
            let mut spans: Vec<Span> = Vec::with_capacity(body.len() + 4);
            if k > 0 {
                // A continuation keeps its panel's colour and loses its number
                // and sign, exactly as the unified view's continuation does.
                spans.push(Span::styled(
                    " ".repeat(gutter_w + 1),
                    base.patch(&p.style(Role::Faint)),
                ));
            } else {
                if sc.cfg.line_numbers {
                    // The number takes the line's own foreground on a changed
                    // row, which makes the gutter read as part of the change; a
                    // context row keeps it dim.
                    let fg = if tinted {
                        h.role.foreground()
                    } else {
                        Role::Faint
                    };
                    spans.push(Span::styled(
                        format!("{:>numw$} ", num, numw = g.numw),
                        base.patch(&p.style(fg)),
                    ));
                }
                // The sign keeps the role's green or red; the cell's base is
                // background-only, so the code keeps its own foregrounds.
                spans.push(Span::styled(
                    h.sign.to_string(),
                    base.patch(&p.style(h.role.foreground())),
                ));
            }
            spans.push(Span::styled(" ", base.clone()));
            spans.extend(body);
            let vis = spans_width(&spans);
            if vis < panel_w {
                spans.push(Span::styled(" ".repeat(panel_w - vis), base.clone()));
            }
            spans
        })
        .collect()
}

/// The language of a file, by the same table the editor uses.
pub fn lang_for(path: &str) -> Option<Lang> {
    syntax::detect(Some(std::path::Path::new(path)), None)
}

/// The two-panel view of a file's `before` → `after`, in one call: the file's
/// name picks the language.
pub fn render_edit(
    path: &str,
    before: &str,
    after: &str,
    before_start: usize,
    after_start: usize,
    cfg: &DiffConfig,
) -> Vec<Line> {
    let old: Vec<&str> = before.lines().collect();
    let new: Vec<&str> = after.lines().collect();
    let sc = SplitConfig {
        cfg,
        before_start,
        after_start,
        lang: lang_for(path),
    };
    render_split(&old, &new, &sc)
}

/// Which of the two shapes a diff is drawn in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditView {
    /// Two panels, before left and after right.
    Split,
    /// One panel, `-`/`+` signed, the file's own line numbers in the gutter.
    Unified,
}

/// The view a diff gets, from what was asked for and nothing else — never from
/// the width: a width gate's two answers are a cramped diff or no diff at all,
/// and no diff at all is a change approved blind.
pub fn edit_view(split_wanted: bool) -> EditView {
    if split_wanted {
        EditView::Split
    } else {
        EditView::Unified
    }
}

/// [`render_edit`] in either view, with the file's own name on the first row —
/// in the one seam both views pass through, so they cannot disagree about
/// whether the name is there.
pub fn render_edit_view(
    path: &str,
    before: &str,
    after: &str,
    before_start: usize,
    after_start: usize,
    cfg: &DiffConfig,
    view: EditView,
) -> Vec<Line> {
    let mut out = vec![faint_line(cfg.palette, path)];
    match view {
        EditView::Split => out.extend(render_edit(
            path,
            before,
            after,
            before_start,
            after_start,
            cfg,
        )),
        EditView::Unified => {
            let old: Vec<&str> = before.lines().collect();
            let new: Vec<&str> = after.lines().collect();
            out.extend(crate::diff::render_in(
                &old,
                &new,
                cfg,
                before_start,
                after_start,
                lang_for(path),
            ))
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::style::Palette;

    /// Whether a span sits in a diff tint, and which.
    fn tint(s: &Span) -> Option<Role> {
        s.style
            .roles()
            .find(|r| matches!(r, Role::Added | Role::Removed))
    }

    fn sc(
        width: usize,
        palette: Palette,
        before_start: usize,
        after_start: usize,
    ) -> SplitConfig<'static> {
        // A leaked config is fine in a test: it keeps `SplitConfig<'a>`
        // lifetimes out of every assertion.
        let cfg = Box::leak(Box::new(DiffConfig {
            width,
            palette,
            context: 1,
            line_numbers: true,
            intra_line: false,
            max_rows: 60,
        }));
        SplitConfig {
            cfg,
            before_start,
            after_start,
            lang: None,
        }
    }

    /// What a row says, styles dropped.
    fn plain(rows: &[Line]) -> Vec<String> {
        rows.iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_str()).collect())
            .collect()
    }

    /// The right panel's spans of a row: everything after the separator.
    fn right_of(row: &Line) -> &[Span] {
        let i = row
            .spans
            .iter()
            .position(|s| s.content == SEP)
            .expect("two panels");
        &row.spans[i + 1..]
    }

    /// **A class grid shorter than the excerpt renders plain instead of
    /// panicking.** An empty grid is the exact shape `classes` returns when a
    /// parser will not take the language.
    #[test]
    fn a_grid_shorter_than_the_lines_renders_plain_rather_than_panicking() {
        let cfg = sc(100, Palette::None, 0, 0);
        let old = vec!["fn main() {}", "let x = 1;"];
        let new = vec!["fn main() {}", "let x = 2;"];
        let g = Geometry::of(&cfg, &old, &new);
        let pair = Pair {
            left: Some(Half {
                line: 1,
                sign: '-',
                role: Role::Removed,
            }),
            right: Some(Half {
                line: 1,
                sign: '+',
                role: Role::Added,
            }),
        };
        let rows = render_pair(&pair, &old, &new, &[], &[], &cfg, &g);
        let text = plain(&rows).join("\n");
        assert!(text.contains("let x = 1;"), "{text}");
        assert!(text.contains("let x = 2;"), "{text}");
        let short = vec![Vec::new()];
        let rows = render_pair(&pair, &old, &new, &short, &short, &cfg, &g);
        let text = plain(&rows).join("\n");
        assert!(text.contains("let x = 1;"), "{text}");
        assert!(text.contains("let x = 2;"), "{text}");
    }

    #[test]
    fn a_change_reads_side_by_side_with_its_context() {
        let old = ["fn a() {", "    old();", "}"];
        let new = ["fn a() {", "    new();", "}"];
        let p = plain(&render_split(&old, &new, &sc(120, Palette::None, 1, 1)));
        assert_eq!(p.len(), 4, "{p:?}");
        assert!(p[0].starts_with("@@"), "the hunk is unheaded: {p:?}");
        assert!(p[1].contains("fn a() {") && p[1].contains("│"), "{p:?}");
        assert!(p[2].contains('-') && p[2].contains("old();"), "{p:?}");
        assert!(p[2].contains('+') && p[2].contains("new();"), "{p:?}");
        assert!(p[3].contains('}'), "{p:?}");
    }

    #[test]
    fn an_insertion_has_no_left_and_a_deletion_no_right() {
        let old = ["a", "b"];
        let new = ["a", "X", "b"];
        let rows = plain(&render_split(&old, &new, &sc(80, Palette::None, 1, 1)));
        let ins = rows.iter().find(|r| r.contains('X')).expect("shown");
        let (left, right) = ins.split_once('│').unwrap();
        assert!(right.contains('+'), "{ins:?}");
        assert!(left.trim().is_empty(), "{ins:?}");

        let rows = plain(&render_split(&new, &old, &sc(80, Palette::None, 1, 1)));
        let del = rows.iter().find(|r| r.contains('X')).expect("shown");
        let (left, right) = del.split_once('│').unwrap();
        assert!(left.contains('-'), "{del:?}");
        assert!(right.trim().is_empty(), "{del:?}");
    }

    #[test]
    fn a_created_file_is_all_right_panel() {
        let rows = plain(&render_split(
            &[],
            &["x", "y"],
            &sc(80, Palette::None, 1, 1),
        ));
        assert_eq!(rows.len(), 3, "header plus the two lines: {rows:?}");
        assert!(rows[0].starts_with("@@"), "{rows:?}");
        for r in rows.iter().skip(1) {
            let (left, right) = r.split_once('│').unwrap();
            assert!(left.trim().is_empty(), "{r:?}");
            assert!(right.contains('+'), "{r:?}");
        }
    }

    #[test]
    fn the_gutter_numbers_the_file_not_the_excerpt() {
        let old = ["keep"];
        let new = ["keep", "added"];
        let rows = plain(&render_split(&old, &new, &sc(80, Palette::None, 41, 41)));
        assert!(
            rows.iter().any(|r| r.contains("42") && r.contains('+')),
            "{rows:?}"
        );
    }

    /// Every row is exactly the width asked for — both panels padded to their
    /// own widths, the odd column on the right — and a long line wraps.
    #[test]
    fn a_row_is_exactly_the_full_width_and_wraps_instead_of_overflowing() {
        let long = "x".repeat(200);
        let old = [long.as_str()];
        let new = [long.as_str(), "short"];
        for w in [60usize, 100, 101, 160, 210] {
            let rows = render_split(&old, &new, &sc(w, Palette::Colour, 1, 1));
            for r in rows
                .iter()
                .filter(|r| r.spans.iter().any(|s| s.content == SEP))
            {
                assert_eq!(spans_width(&r.spans), w, "{w}: {r:?}");
            }
        }
    }

    /// An added line is tinted to its **full width** and a removed one too, the
    /// sign and number carry green/red, the code keeps its own foreground, and
    /// the separator is never inside a tint.
    #[test]
    fn a_changed_row_is_tinted_to_its_full_width_and_the_separator_is_not() {
        let old = ["fn a() {", "    old();", "}"];
        let new = ["fn a() {", "    new();", "}"];
        let rows = render_split(&old, &new, &sc(60, Palette::Colour, 1, 1));
        let changed = rows
            .iter()
            .find(|r| plain(std::slice::from_ref(*r))[0].contains("new();"))
            .expect("the changed row");
        let right = right_of(changed);
        assert!(
            right.iter().all(|s| tint(s) == Some(Role::Added)),
            "{right:?}"
        );
        let sign = right.iter().find(|s| s.content == "+").unwrap();
        assert_eq!(sign.style.top(), Role::Success);
        let num = right.iter().find(|s| s.content.trim() == "2").unwrap();
        assert_eq!(num.style.top(), Role::Success);
        let code = right.iter().find(|s| s.content.contains("new();")).unwrap();
        assert_eq!(
            code.style.top(),
            Role::Added,
            "the code keeps its own foreground"
        );
        let sep_at = changed.spans.iter().position(|s| s.content == SEP).unwrap();
        assert!(
            changed.spans[..sep_at]
                .iter()
                .all(|s| tint(s) == Some(Role::Removed))
        );
        assert_eq!(tint(&changed.spans[sep_at]), None);

        // A context row is untinted.
        let ctx = rows
            .iter()
            .find(|r| plain(std::slice::from_ref(*r))[0].contains("fn a() {"))
            .unwrap();
        assert!(ctx.spans.iter().all(|s| tint(s).is_none()), "{ctx:?}");

        // No palette, no style: the glyph alone says which is which.
        for r in render_split(&old, &new, &sc(60, Palette::None, 1, 1)) {
            assert!(r.spans.iter().all(|s| s.style == Style::new()), "{r:?}");
        }
    }

    #[test]
    fn a_wrapped_line_keeps_its_panel_and_blanks_the_other() {
        let long = "y".repeat(120);
        let old = ["a"];
        let new = [long.as_str()];
        let rows = plain(&render_split(&old, &new, &sc(60, Palette::None, 1, 1)));
        assert!(rows.len() >= 3, "{rows:?}");
        let cont = &rows[2];
        let (left, right) = cont.split_once('│').unwrap();
        assert!(left.trim().is_empty(), "{cont:?}");
        assert!(right.contains('y'), "{cont:?}");
        // The continuation's code starts in the first row's code column.
        let first = rows[1].split_once('│').unwrap().1;
        assert_eq!(first.find('y'), right.find('y'), "{rows:?}");
    }

    #[test]
    fn the_row_cap_stops_the_panels_and_says_what_it_dropped() {
        let old: Vec<String> = (0..20).map(|i| format!("old {i}")).collect();
        let new: Vec<String> = (0..20).map(|i| format!("new {i}")).collect();
        let old: Vec<&str> = old.iter().map(String::as_str).collect();
        let new: Vec<&str> = new.iter().map(String::as_str).collect();
        let cfg = Box::leak(Box::new(DiffConfig {
            width: 120,
            palette: Palette::None,
            context: 0,
            line_numbers: true,
            intra_line: false,
            max_rows: 6,
        }));
        let sc = SplitConfig {
            cfg,
            before_start: 1,
            after_start: 1,
            lang: None,
        };
        let rows = plain(&render_split(&old, &new, &sc));
        assert!(
            rows.iter().any(|r| r.contains("more diff lines not shown")),
            "{rows:?}"
        );
        assert!(rows.len() <= 8, "{rows:?}");
    }

    #[test]
    fn identical_sides_say_so() {
        let old = ["same"];
        let rows = plain(&render_split(&old, &old, &sc(80, Palette::None, 1, 1)));
        assert_eq!(rows, vec!["no change"]);
    }

    #[test]
    fn rust_code_arrives_coloured_and_unknown_extensions_do_not() {
        let old = ["fn a() {}"];
        let new = ["fn a() { let x = 1; }"];
        let cfg = Box::leak(Box::new(DiffConfig {
            width: 160,
            palette: Palette::Colour,
            context: 1,
            line_numbers: true,
            intra_line: false,
            max_rows: 60,
        }));
        let mut sc = SplitConfig {
            cfg,
            before_start: 1,
            after_start: 1,
            lang: lang_for("a.rs"),
        };
        assert_eq!(sc.lang, Some(Lang::Rust));
        let keyword = |rows: &[Line]| {
            rows.iter().any(|r| {
                r.spans
                    .iter()
                    .any(|s| s.content.contains("fn") && s.style.top() == Role::Keyword)
            })
        };
        assert!(keyword(&render_split(&old, &new, &sc)));
        sc.lang = lang_for("a.txt");
        assert_eq!(sc.lang, None);
        assert!(!keyword(&render_split(&old, &new, &sc)));
    }

    #[test]
    fn a_language_the_editor_knows_is_detected_by_the_same_table() {
        assert_eq!(lang_for("patch.lisp"), Some(Lang::CommonLisp));
        assert_eq!(lang_for("app.ts"), Some(Lang::TypeScript));
        assert_eq!(lang_for("README.md"), Some(Lang::Markdown));
        assert_eq!(lang_for("Makefile"), Some(Lang::Make));
    }

    /// **A multi-byte character shifts no class but its own**: the grid is one
    /// cell per char, and an em-dash must not push the next line's classes.
    #[test]
    fn an_em_dash_shifts_no_class_but_its_own() {
        let lines = ["let s = \"a—b\"; // dash", "let done = build(); // tail"];
        let col_of = |line: &str, needle: &str| {
            let b = line.find(needle).unwrap();
            line[..b].chars().count()
        };
        let grid = role_grid(&lines, Some(Lang::Rust));
        let l0 = lines[0];
        assert_eq!(grid[0][col_of(l0, "—")], Role::StringLit, "{:?}", grid[0]);
        assert_eq!(grid[0][col_of(l0, "b\"") + 1], Role::StringLit);
        assert_eq!(grid[0][col_of(l0, "// dash")], Role::Comment);
        let l1 = lines[1];
        assert_eq!(grid[1][col_of(l1, "let")], Role::Keyword, "{:?}", grid[1]);
        assert_eq!(grid[1][col_of(l1, "build")], Role::FuncName);
        assert_eq!(grid[1][col_of(l1, "// tail")], Role::Comment);
    }

    #[test]
    fn tabs_expand_to_the_same_stops_the_unified_renderer_uses() {
        let old = ["\tfn a() {}"];
        let new = ["\tfn b() {}"];
        let rows = plain(&render_split(&old, &new, &sc(120, Palette::None, 1, 1)));
        let changed = rows
            .iter()
            .find(|r| r.contains('-') && r.contains("fn a"))
            .unwrap();
        let (left, right) = changed.split_once('│').unwrap();
        assert_eq!(
            left.find("fn").unwrap(),
            right.trim_start().find("fn").unwrap(),
            "{changed:?}"
        );
        // A RUN of tabs: eight columns, not seven — anchored at the sign,
        // because a bare `contains` cannot tell seven spaces from eight next to
        // the gutter's own space.
        let lined = ["\t\treturn x", "\t\treturn y"];
        let rows = plain(&render_split(
            &lined[..1],
            &lined[1..],
            &sc(120, Palette::None, 1, 1),
        ));
        let code = rows.iter().find(|r| r.contains("return x")).unwrap();
        let body = code.split_once("- ").expect("a deletion sign").1;
        assert!(body.starts_with("        return x"), "{body:?}");
    }

    #[test]
    fn both_views_name_the_file_first() {
        let cfg = DiffConfig {
            palette: Palette::None,
            ..Default::default()
        };
        for view in [EditView::Split, EditView::Unified] {
            let rows = plain(&render_edit_view(
                "src/a.rs", "a\n", "b\n", 1, 1, &cfg, view,
            ));
            assert_eq!(rows[0], "src/a.rs", "{view:?}");
            assert!(rows[1].starts_with("@@"), "{view:?}: {rows:?}");
        }
    }
}
