//! Merge conflicts, side by side.
//!
//! An inline conflict is two versions of a region interleaved with markers,
//! and reading it means holding one side in your head while scanning the
//! other. This module takes the markers apart: it rebuilds the file as it would
//! be with **ours** taken in every conflict and as it would be with **theirs**,
//! and diffs those two with the ordinary renderers ([`crate::sidediff`],
//! [`crate::diff`]). Everything outside the conflicts is the same in both, so
//! the diff *is* the conflicts — each one aligned line against line, the
//! changed words emphasised, with real context around it, syntax-coloured in
//! the file's own language and numbered by each side's version of the file.
//!
//! Markers are git's: `<<<<<<< label`, an optional `||||||| label` (the merge
//! base, from `merge.conflictStyle = diff3` / `zdiff3`), `=======`, and
//! `>>>>>>> label`. A conflict that never closes is kept as ordinary text, so
//! nothing in the file is dropped.

use ratatui::text::{Line, Span};

use crate::diff::{DiffConfig, render_in};
use crate::sidediff::{EditView, SplitConfig, lang_for, render_split};
use crate::style::Role;

/// One region of a file with conflicts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Segment {
    /// Lines both sides agree on.
    Common(Vec<String>),
    Conflict(Conflict),
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Conflict {
    /// What follows `<<<<<<<`: usually `HEAD`.
    pub ours_label: String,
    pub ours: Vec<String>,
    /// The merge base, when the conflict style records it.
    pub base: Option<Vec<String>>,
    /// What follows `>>>>>>>`: usually the branch or commit merged in.
    pub theirs_label: String,
    pub theirs: Vec<String>,
    /// The `<<<<<<<` line's index in the text, and one past the `>>>>>>>`
    /// line's: the rows a resolution replaces.
    pub start: usize,
    pub end: usize,
}

/// A marker line: seven of `c`, then the end of the line or a space and a label.
fn marker(line: &str, c: char) -> Option<&str> {
    let rest = line.strip_prefix(&c.to_string().repeat(7) as &str)?;
    if rest.is_empty() {
        return Some("");
    }
    rest.strip_prefix(' ').map(str::trim_end)
}

/// Split a file into common text and conflicts.
pub fn parse(text: &str) -> Vec<Segment> {
    let lines: Vec<&str> = text.lines().collect();
    let mut out: Vec<Segment> = Vec::new();
    let mut common: Vec<String> = Vec::new();
    let mut i = 0usize;
    while i < lines.len() {
        if let Some(label) = marker(lines[i], '<')
            && let Some((mut c, next)) = read_conflict(&lines, i + 1, label)
        {
            (c.start, c.end) = (i, next);
            if !common.is_empty() {
                out.push(Segment::Common(std::mem::take(&mut common)));
            }
            out.push(Segment::Conflict(c));
            i = next;
            continue;
        }
        common.push(lines[i].to_string());
        i += 1;
    }
    if !common.is_empty() {
        out.push(Segment::Common(common));
    }
    out
}

/// The body of a conflict whose `<<<<<<<` was the line before `i`, and the
/// line after its `>>>>>>>`; `None` when it never closes.
fn read_conflict(lines: &[&str], mut i: usize, label: &str) -> Option<(Conflict, usize)> {
    let mut c = Conflict {
        ours_label: label.to_string(),
        ..Default::default()
    };
    // 0: ours, 1: base, 2: theirs.
    let mut part = 0;
    while i < lines.len() {
        let l = lines[i];
        if part == 0
            && let Some(_) = marker(l, '|')
        {
            c.base = Some(Vec::new());
            part = 1;
        } else if part < 2 && l == "=======" {
            part = 2;
        } else if part == 2
            && let Some(theirs) = marker(l, '>')
        {
            c.theirs_label = theirs.to_string();
            return Some((c, i + 1));
        } else {
            match part {
                0 => c.ours.push(l.to_string()),
                1 => c.base.get_or_insert_with(Vec::new).push(l.to_string()),
                _ => c.theirs.push(l.to_string()),
            }
        }
        i += 1;
    }
    None
}

/// The file as it would be with every conflict resolved one way: `(ours,
/// theirs)`, and the labels of the first conflict. `None` when there is no
/// conflict.
pub struct Sides {
    pub ours: String,
    pub theirs: String,
    pub ours_label: String,
    pub theirs_label: String,
    pub count: usize,
}

pub fn sides(segments: &[Segment]) -> Option<Sides> {
    let mut ours: Vec<&str> = Vec::new();
    let mut theirs: Vec<&str> = Vec::new();
    let mut labels: Option<(&str, &str)> = None;
    let mut count = 0usize;
    for s in segments {
        match s {
            Segment::Common(l) => {
                ours.extend(l.iter().map(String::as_str));
                theirs.extend(l.iter().map(String::as_str));
            }
            Segment::Conflict(c) => {
                count += 1;
                labels.get_or_insert((&c.ours_label, &c.theirs_label));
                ours.extend(c.ours.iter().map(String::as_str));
                theirs.extend(c.theirs.iter().map(String::as_str));
            }
        }
    }
    let (ol, tl) = labels?;
    Some(Sides {
        ours: ours.join("\n"),
        theirs: theirs.join("\n"),
        ours_label: ol.to_string(),
        theirs_label: tl.to_string(),
        count,
    })
}

/// Whether `text` has at least one complete conflict.
pub fn has_conflicts(text: &str) -> bool {
    parse(text)
        .iter()
        .any(|s| matches!(s, Segment::Conflict(_)))
}

/// Which two versions of each conflict are set side by side.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Compare {
    /// Ours against theirs: the conflict itself.
    OursTheirs,
    /// The merge base against ours: what our side changed.
    BaseOurs,
    /// The merge base against theirs: what their side changed.
    BaseTheirs,
}

impl Compare {
    /// The next comparison, wrapping.
    pub fn next(self) -> Compare {
        match self {
            Compare::OursTheirs => Compare::BaseOurs,
            Compare::BaseOurs => Compare::BaseTheirs,
            Compare::BaseTheirs => Compare::OursTheirs,
        }
    }
}

/// The drawn conflicts, and where each one's section starts in `lines`.
pub struct View {
    pub lines: Vec<Line<'static>>,
    /// Index into `lines` of each conflict's header row, in file order.
    pub sections: Vec<usize>,
    /// Each conflict's rows in the text: its `<<<<<<<` line to one past its
    /// `>>>>>>>` line.
    pub rows: Vec<(usize, usize)>,
}

/// Unchanged lines shown either side of a conflict.
const CONTEXT: usize = 3;

/// Draw `path`'s conflicts one section each: a header naming the conflict, its
/// lines and which version is on which side, then the two versions in `view`
/// with up to three lines of the file around them. `current` is the conflict
/// whose header is marked. `None` when the text has no conflict.
///
/// Each side is numbered by its own version of the file — the file with every
/// conflict resolved that side's way — so a number says where the line will be
/// once that side is taken.
pub fn render_view(
    path: &str,
    text: &str,
    cfg: &DiffConfig,
    view: EditView,
    compare: Compare,
    current: usize,
) -> Option<View> {
    let segs = parse(text);
    let n = segs
        .iter()
        .filter(|s| matches!(s, Segment::Conflict(_)))
        .count();
    if n == 0 {
        return None;
    }
    let pal = cfg.palette;
    let lang = lang_for(path);
    let plural = if n == 1 { "" } else { "s" };
    let mut out = View {
        lines: vec![Line::from(Span::styled(
            format!("{n} conflict{plural} in {path}"),
            pal.style(Role::Attention),
        ))],
        sections: Vec::new(),
        rows: Vec::new(),
    };
    // Lines so far in each side's version of the file: ours, theirs, base.
    let (mut ours_at, mut theirs_at, mut base_at) = (0usize, 0usize, 0usize);
    let mut k = 0usize;
    for (j, s) in segs.iter().enumerate() {
        let c = match s {
            Segment::Common(l) => {
                ours_at += l.len();
                theirs_at += l.len();
                base_at += l.len();
                continue;
            }
            Segment::Conflict(c) => c,
        };
        let before: &[String] = match j.checked_sub(1).map(|i| &segs[i]) {
            Some(Segment::Common(l)) => &l[l.len().saturating_sub(CONTEXT)..],
            _ => &[],
        };
        let after: &[String] = match segs.get(j + 1) {
            Some(Segment::Common(l)) => &l[..l.len().min(CONTEXT)],
            _ => &[],
        };
        let label = |l: &str, side: &str| {
            if l.is_empty() {
                side.to_string()
            } else {
                format!("{side} ({l})")
            }
        };
        let ours = (label(&c.ours_label, "ours"), &c.ours, ours_at);
        let theirs = (label(&c.theirs_label, "theirs"), &c.theirs, theirs_at);
        // A base comparison needs the base: without it (the default conflict
        // style records none) the section says so and shows the conflict.
        let (mut note, base) = (None, c.base.as_ref());
        let (left, right) = match (compare, base) {
            (Compare::OursTheirs, _) => (ours, theirs),
            (Compare::BaseOurs, Some(b)) => (("base".to_string(), b, base_at), ours),
            (Compare::BaseTheirs, Some(b)) => (("base".to_string(), b, base_at), theirs),
            (_, None) => {
                note = Some("no base recorded (git config merge.conflictStyle diff3 records it)");
                (ours, theirs)
            }
        };
        let which = match view {
            EditView::Split => format!("left: {}   right: {}", left.0, right.0),
            EditView::Unified => format!("-: {}   +: {}", left.0, right.0),
        };
        let mark = if k == current { "▶ " } else { "  " };
        out.rows.push((c.start, c.end));
        out.lines.push(Line::default());
        out.sections.push(out.lines.len());
        out.lines.push(Line::from(vec![
            Span::styled(
                format!(
                    "{mark}Conflict {} of {n}, lines {}–{}",
                    k + 1,
                    c.start + 1,
                    c.end
                ),
                pal.style(if k == current {
                    Role::Attention
                } else {
                    Role::Strong
                }),
            ),
            Span::styled(format!("   {which}"), pal.style(Role::Faint)),
        ]));
        if let Some(note) = note {
            out.lines.push(Line::from(Span::styled(
                format!("  {note}"),
                pal.style(Role::Faint),
            )));
        }
        let excerpt = |side: &[String]| -> Vec<String> {
            before
                .iter()
                .chain(side.iter())
                .chain(after.iter())
                .cloned()
                .collect()
        };
        let (l_lines, r_lines) = (excerpt(left.1), excerpt(right.1));
        let l: Vec<&str> = l_lines.iter().map(String::as_str).collect();
        let r: Vec<&str> = r_lines.iter().map(String::as_str).collect();
        // 1-based line of the excerpt's first row in each side's version.
        let (ls, rs) = (left.2 - before.len() + 1, right.2 - before.len() + 1);
        let hcfg = DiffConfig {
            context: l.len().max(r.len()),
            ..*cfg
        };
        out.lines.extend(match view {
            EditView::Split => render_split(
                &l,
                &r,
                &SplitConfig {
                    cfg: &hcfg,
                    before_start: ls,
                    after_start: rs,
                    lang,
                },
            ),
            EditView::Unified => render_in(&l, &r, &hcfg, ls, rs, lang),
        });
        ours_at += c.ours.len();
        theirs_at += c.theirs.len();
        base_at += c.base.as_ref().map_or(c.ours.len(), Vec::len);
        k += 1;
    }
    Some(out)
}

/// [`render_view`] ours against theirs, the first conflict current: just the
/// lines.
pub fn render(
    path: &str,
    text: &str,
    cfg: &DiffConfig,
    view: EditView,
) -> Option<Vec<Line<'static>>> {
    render_view(path, text, cfg, view, Compare::OursTheirs, 0).map(|v| v.lines)
}

/// `text` with conflict `k` (0-based, file order) replaced by `lines`: the rows
/// it spans and what goes there, for the caller to apply as one edit. `None`
/// when there is no such conflict.
pub fn resolution(text: &str, k: usize, take: Take) -> Option<(usize, usize, Vec<String>)> {
    let c = parse(text)
        .into_iter()
        .filter_map(|s| match s {
            Segment::Conflict(c) => Some(c),
            _ => None,
        })
        .nth(k)?;
    let lines = match take {
        Take::Ours => c.ours,
        Take::Theirs => c.theirs,
        Take::OursThenTheirs => c.ours.into_iter().chain(c.theirs).collect(),
        Take::TheirsThenOurs => c.theirs.into_iter().chain(c.ours).collect(),
        Take::Base => c.base?,
    };
    Some((c.start, c.end, lines))
}

/// How a conflict is resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Take {
    Ours,
    Theirs,
    /// Both, ours first.
    OursThenTheirs,
    /// Both, theirs first.
    TheirsThenOurs,
    /// The merge base — neither side's change. Only when it was recorded.
    Base,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::style::Palette;

    const FILE: &str = "\
fn main() {
    setup();
<<<<<<< HEAD
    let total = compute(a, b);
=======
    let sum = compute(a, b, c);
>>>>>>> feature/sum
    report();
}

fn other() {
<<<<<<< HEAD
    one();
||||||| merged common ancestors
    zero();
=======
    two();
    three();
>>>>>>> feature/sum
}
";

    fn text(rows: &[Line]) -> Vec<String> {
        rows.iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect()
    }

    #[test]
    fn markers_split_into_common_text_and_conflicts() {
        let segs = parse(FILE);
        let conflicts: Vec<&Conflict> = segs
            .iter()
            .filter_map(|s| match s {
                Segment::Conflict(c) => Some(c),
                _ => None,
            })
            .collect();
        assert_eq!(conflicts.len(), 2);
        let a = conflicts[0];
        assert_eq!(a.ours_label, "HEAD");
        assert_eq!(a.theirs_label, "feature/sum");
        assert_eq!(a.ours, vec!["    let total = compute(a, b);"]);
        assert_eq!(a.theirs, vec!["    let sum = compute(a, b, c);"]);
        assert_eq!(a.base, None);
        let b = conflicts[1];
        assert_eq!(b.base.as_deref(), Some(&["    zero();".to_string()][..]));
        assert_eq!(b.ours, vec!["    one();"]);
        assert_eq!(b.theirs, vec!["    two();", "    three();"]);
    }

    #[test]
    fn each_side_is_the_whole_file_resolved_its_way() {
        let s = sides(&parse(FILE)).unwrap();
        assert_eq!(s.count, 2);
        assert!(s.ours.contains("let total") && !s.ours.contains("let sum"));
        assert!(s.theirs.contains("let sum") && !s.theirs.contains("let total"));
        assert!(!s.ours.contains("<<<<<<<") && !s.theirs.contains("======="));
        assert!(!s.ours.contains("zero()"), "the base is neither side");
        assert!(s.ours.contains("report();") && s.theirs.contains("report();"));
    }

    /// An unterminated conflict, or a marker-looking line with no conflict, is
    /// text: nothing in the file is dropped.
    #[test]
    fn a_conflict_that_never_closes_is_ordinary_text() {
        let t = "a\n<<<<<<< HEAD\nb\n=======\nc\n";
        assert!(!has_conflicts(t));
        assert_eq!(
            parse(t),
            vec![Segment::Common(t.lines().map(String::from).collect())]
        );
        assert!(!has_conflicts("<<<<<<<< eight is not a marker\n"));
        assert!(sides(&parse("plain\n")).is_none());
    }

    #[test]
    fn the_conflicts_draw_ours_left_and_theirs_right() {
        let cfg = DiffConfig {
            width: 120,
            palette: Palette::None,
            max_rows: usize::MAX,
            ..Default::default()
        };
        let rows = text(&render("src/main.rs", FILE, &cfg, EditView::Split).unwrap());
        assert!(
            rows[0].starts_with("2 conflicts in src/main.rs"),
            "{rows:?}"
        );
        let head = rows
            .iter()
            .find(|r| r.contains("Conflict 1 of 2"))
            .expect("a section per conflict");
        assert!(head.starts_with("▶ "), "the first is current: {head:?}");
        assert!(head.contains("lines 3–7"), "{head:?}");
        assert!(head.contains("left: ours (HEAD)   right: theirs (feature/sum)"));
        let pair = rows
            .iter()
            .find(|r| r.contains("let total"))
            .expect("the first conflict");
        let (left, right) = pair.split_once('│').unwrap();
        assert!(left.contains("let total"), "{pair:?}");
        assert!(right.contains("let sum"), "{pair:?}");
        // Context around it, from the file.
        assert!(rows.iter().any(|r| r.contains("setup();")), "{rows:?}");
        // The second conflict's extra line on their side has no partner.
        let three = rows.iter().find(|r| r.contains("three()")).unwrap();
        let (left, _) = three.split_once('│').unwrap();
        assert!(left.trim().is_empty(), "{three:?}");

        let rows = text(&render("src/main.rs", FILE, &cfg, EditView::Unified).unwrap());
        assert!(
            rows.iter().any(|r| r.contains("-: ours (HEAD)")),
            "{rows:?}"
        );
        assert!(
            rows.iter()
                .any(|r| r.ends_with("-    let total = compute(a, b);"))
        );
        assert!(
            rows.iter()
                .any(|r| r.ends_with("+    let sum = compute(a, b, c);"))
        );
        assert!(render("x", "no conflicts\n", &cfg, EditView::Split).is_none());
    }

    #[test]
    fn sections_know_where_they_start_and_which_rows_they_span() {
        let cfg = DiffConfig {
            palette: Palette::None,
            max_rows: usize::MAX,
            ..Default::default()
        };
        let v = render_view(
            "a.rs",
            FILE,
            &cfg,
            EditView::Unified,
            Compare::OursTheirs,
            1,
        )
        .unwrap();
        assert_eq!(v.rows, vec![(2, 7), (11, 19)]);
        let rows = text(&v.lines);
        assert!(rows[v.sections[0]].contains("Conflict 1 of 2"), "{rows:?}");
        assert!(
            rows[v.sections[1]].starts_with("▶ Conflict 2 of 2"),
            "{rows:?}"
        );
        // Each side numbered by its own version: in ours, `one();` is line 8
        // (the first conflict is one line there, not five); in theirs `two();` too.
        assert!(
            rows.iter().any(|r| r.ends_with(" 8 -    one();")),
            "{rows:?}"
        );
        assert!(
            rows.iter().any(|r| r.ends_with(" 8 +    two();")),
            "{rows:?}"
        );
    }

    #[test]
    fn the_base_comparisons_show_what_each_side_changed() {
        let cfg = DiffConfig {
            width: 120,
            palette: Palette::None,
            max_rows: usize::MAX,
            ..Default::default()
        };
        let v = render_view("a.rs", FILE, &cfg, EditView::Split, Compare::BaseTheirs, 1).unwrap();
        let rows = text(&v.lines);
        let pair = rows
            .iter()
            .find(|r| r.contains("zero()"))
            .expect("{rows:?}");
        let (left, right) = pair.split_once('│').unwrap();
        assert!(
            left.contains("zero()") && right.contains("two()"),
            "{pair:?}"
        );
        assert!(rows[v.sections[1]].contains("left: base   right: theirs (feature/sum)"));
        // The first conflict recorded no base: it says so and shows the conflict.
        assert!(
            rows.iter().any(|r| r.contains("no base recorded")),
            "{rows:?}"
        );
        assert_eq!(Compare::OursTheirs.next(), Compare::BaseOurs);
        assert_eq!(Compare::BaseTheirs.next(), Compare::OursTheirs);
    }

    #[test]
    fn a_resolution_is_the_rows_and_what_replaces_them() {
        assert_eq!(
            resolution(FILE, 0, Take::Theirs),
            Some((2, 7, vec!["    let sum = compute(a, b, c);".to_string()]))
        );
        let (s, e, both) = resolution(FILE, 1, Take::OursThenTheirs).unwrap();
        assert_eq!((s, e), (11, 19));
        assert_eq!(both, vec!["    one();", "    two();", "    three();"]);
        assert_eq!(
            resolution(FILE, 1, Take::Base).unwrap().2,
            vec!["    zero();"]
        );
        assert_eq!(resolution(FILE, 0, Take::Base), None, "no base recorded");
        assert_eq!(resolution(FILE, 2, Take::Ours), None);
    }
}
