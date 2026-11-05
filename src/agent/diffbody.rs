//! A command's output that reads as a diff or a diffstat, painted line by line. The text is a
//! command's stdout, so git sent no colour; the head adds it and the model's copy stays plain.

use crate::render::{Line, Span};
use crate::style::Role;

/// Lines left in the hunk being read: old side, new side.
#[derive(Default)]
struct Hunk {
    old: usize,
    new: usize,
}

impl Hunk {
    fn open(&self) -> bool {
        self.old > 0 || self.new > 0
    }
}

/// `-a[,b] +c[,d]` of a `@@ ... @@` header.
fn hunk_counts(header: &str) -> Option<Hunk> {
    let rest = header.strip_prefix("@@ -")?;
    let (old, rest) = rest.split_once(" +")?;
    let new = rest.split(' ').next()?;
    let count = |s: &str| -> Option<usize> {
        match s.split_once(',') {
            Some((_, n)) => n.parse().ok(),
            None => s.parse::<usize>().ok().map(|_| 1),
        }
    };
    Some(Hunk {
        old: count(old)?,
        new: count(new)?,
    })
}

/// ` path | 12 +++---`: the shape of a `--stat` line. The count is digits and the bar only `+` and `-`.
fn stat_line(t: &str) -> Option<Line> {
    if !t.starts_with(' ') || t.starts_with("  ") {
        return None;
    }
    let at = t.rfind(" | ")?;
    let (left, right) = (&t[..at + 3], &t[at + 3..]);
    let right = right.trim_start();
    let digits = right.bytes().take_while(u8::is_ascii_digit).count();
    if digits == 0 || left.trim().len() < 2 {
        return None;
    }
    let (count, bar) = right.split_at(digits);
    let bar = bar.strip_prefix(' ').unwrap_or(bar);
    if !bar.bytes().all(|b| b == b'+' || b == b'-') {
        return None;
    }
    let gap = &t[at + 3..t.len() - right.len()];
    let mut spans = vec![Span::raw(format!("{left}{gap}{count}"))];
    if !bar.is_empty() {
        spans.push(Span::raw(" "));
        let plus = bar.bytes().take_while(|&b| b == b'+').count();
        // A bar is `+` then `-`; any other mix keeps each run its own colour.
        let mut rest = bar;
        let mut first = plus > 0;
        while !rest.is_empty() {
            let run = rest
                .bytes()
                .take_while(|&b| (b == b'+') == first)
                .count()
                .max(1);
            let role = if first { Role::Success } else { Role::Failure };
            spans.push(Span::role(&rest[..run], role));
            rest = &rest[run..];
            first = !first;
        }
    }
    Some(Line::new(spans))
}

/// One painted [`Line`] per input line that reads as part of a diff, `None` for the rest.
pub(crate) fn paint(lines: &[&str]) -> Vec<Option<Line>> {
    let whole = |t: &str, r: Role| Some(Line::new(vec![Span::role(t, r)]));
    let mut out = Vec::with_capacity(lines.len());
    let mut hunk = Hunk::default();
    for (i, t) in lines.iter().enumerate() {
        let t = *t;
        if hunk.open() {
            match t.as_bytes().first() {
                Some(b'+') => {
                    hunk.new -= 1;
                    out.push(whole(t, Role::Success));
                    continue;
                }
                Some(b'-') => {
                    hunk.old -= 1;
                    out.push(whole(t, Role::Failure));
                    continue;
                }
                Some(b' ') | None => {
                    hunk.old = hunk.old.saturating_sub(1);
                    hunk.new = hunk.new.saturating_sub(1);
                    out.push(None);
                    continue;
                }
                Some(b'\\') => {
                    out.push(None);
                    continue;
                }
                // Not a hunk line: the diff was cut short, so read it afresh.
                _ => hunk = Hunk::default(),
            }
        }
        let next_is_new = lines.get(i + 1).is_some_and(|n| n.starts_with("+++ "));
        let after_old = i > 0 && lines[i - 1].starts_with("--- ");
        let file_header = t.starts_with("diff --git ")
            || (t.starts_with("--- ") && next_is_new)
            || (t.starts_with("+++ ") && after_old);
        let painted = if file_header {
            whole(t, Role::Strong)
        } else if let Some(h) = hunk_counts(t) {
            hunk = h;
            whole(t, Role::Subheading)
        } else {
            stat_line(t)
        };
        out.push(painted);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::testing::role_of;

    fn roles(text: &str) -> Vec<Option<Role>> {
        let lines: Vec<&str> = text.lines().collect();
        paint(&lines)
            .into_iter()
            .map(|l| l.map(|l| l.spans[0].style.top()))
            .collect()
    }

    #[test]
    fn a_unified_diff_is_painted_by_hunk() {
        let r = roles(
            "diff --git a/x b/x\nindex 1..2 100644\n--- a/x\n+++ b/x\n@@ -1,2 +1,2 @@ fn f\n ctx\n-old\n+new\n",
        );
        assert_eq!(r[0], Some(Role::Strong));
        assert_eq!(r[1], None);
        assert_eq!(r[2], Some(Role::Strong));
        assert_eq!(r[3], Some(Role::Strong));
        assert_eq!(r[4], Some(Role::Subheading));
        assert_eq!(r[5], None);
        assert_eq!(r[6], Some(Role::Failure));
        assert_eq!(r[7], Some(Role::Success));
    }

    #[test]
    fn a_commit_message_dash_before_the_diff_is_not_a_removal() {
        let r =
            roles("commit abc\n\n    - a bullet\n-not in a hunk\n@@ -1 +1 @@\n-a\n+b\n-after\n");
        assert_eq!(&r[..4], &[None, None, None, None]);
        assert_eq!(r[5], Some(Role::Failure));
        assert_eq!(r[6], Some(Role::Success));
        assert_eq!(r[7], None);
    }

    #[test]
    fn a_stat_line_has_its_bars_painted() {
        let lines = [" src/a.rs | 12 ++++---"];
        let l = paint(&lines).remove(0).expect("stat line");
        assert_eq!(l.plain(), " src/a.rs | 12 ++++---");
        assert_eq!(role_of(&l, "++++"), Some(Role::Success));
        assert_eq!(role_of(&l, "---"), Some(Role::Failure));
    }

    #[test]
    fn ls_l_and_a_flag_list_are_not_painted() {
        let r = roles(
            "-rw-r--r--  1 u g 12 Nov  5 a\n-h, --help   show help\n--- not a pair\n+ item\n - x | y\ntotal 4\n",
        );
        assert!(r.iter().all(Option::is_none), "{r:?}");
    }
}
