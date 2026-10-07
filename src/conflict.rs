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

use crate::diff::DiffConfig;
use crate::sidediff::{EditView, render_edit_view};
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
            && let Some((c, next)) = read_conflict(&lines, i + 1, label)
        {
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

/// Draw the conflicts of `path`'s `text`: a line saying how many there are and
/// which side is which, then ours against theirs in `view`. `None` when the
/// text has no conflict.
pub fn render(
    path: &str,
    text: &str,
    cfg: &DiffConfig,
    view: EditView,
) -> Option<Vec<Line<'static>>> {
    let s = sides(&parse(text))?;
    let pal = cfg.palette;
    let label = |l: &str, side: &str| {
        if l.is_empty() {
            side.to_string()
        } else {
            format!("{side} ({l})")
        }
    };
    let (ours, theirs) = (
        label(&s.ours_label, "ours"),
        label(&s.theirs_label, "theirs"),
    );
    let n = s.count;
    let plural = if n == 1 { "" } else { "s" };
    let which = match view {
        EditView::Split => format!("left: {ours}   right: {theirs}"),
        EditView::Unified => format!("-: {ours}   +: {theirs}"),
    };
    let mut out = vec![Line::from(vec![
        Span::styled(format!("{n} conflict{plural}"), pal.style(Role::Attention)),
        Span::styled(format!("   {which}"), pal.style(Role::Faint)),
    ])];
    out.extend(render_edit_view(path, &s.ours, &s.theirs, 1, 1, cfg, view));
    Some(out)
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
        assert!(rows[0].starts_with("2 conflicts"), "{rows:?}");
        assert!(rows[0].contains("left: ours (HEAD)"), "{rows:?}");
        assert!(rows[0].contains("right: theirs (feature/sum)"), "{rows:?}");
        let pair = rows
            .iter()
            .find(|r| r.contains("let total"))
            .expect("the first conflict");
        let (left, right) = pair.split_once('│').unwrap();
        assert!(left.contains("let total"), "{pair:?}");
        assert!(right.contains("let sum"), "{pair:?}");
        // The second conflict's extra line on their side has no partner.
        let three = rows.iter().find(|r| r.contains("three()")).unwrap();
        let (left, _) = three.split_once('│').unwrap();
        assert!(left.trim().is_empty(), "{three:?}");

        let rows = text(&render("src/main.rs", FILE, &cfg, EditView::Unified).unwrap());
        assert!(rows[0].contains("-: ours (HEAD)"), "{rows:?}");
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
}
