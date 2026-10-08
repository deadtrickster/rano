//! Unified diffs read back in: a patch file parsed into files and hunks, and
//! drawn with the same renderers as a computed diff ([`crate::diff`],
//! [`crate::sidediff`]).
//!
//! # What is parsed
//!
//! `git diff` / `git format-patch` output and plain `diff -u`: an optional
//! preamble (a commit's headers and message), then per file an optional
//! `diff --git` line and its extended headers (`index`, `new file mode`,
//! `rename from`, `Binary files … differ`, …), the `---` / `+++` names, and
//! `@@ -a,b +c,d @@` hunks.
//!
//! **A hunk is read by its counts, not by its look.** The header says how many
//! old and new lines follow, and the body is exactly that many: a removed line
//! whose text begins `-- ` reads as `--- ` and a context line can be empty when
//! an editor stripped its leading space. Reading by the counts is what keeps
//! both inside the hunk — and what leaves `format-patch`'s trailing `-- ` and
//! version line outside it, as text.
//!
//! # How it is drawn
//!
//! Each hunk's old and new sides are an **excerpt** with known starts, which is
//! exactly what [`crate::diff::render_in`] and [`crate::sidediff::render_split`]
//! take: they re-diff the excerpt, so the drawing gets the word-level emphasis
//! and syntax colour in the file's own language, numbered by
//! the file and not by the patch. The context is the whole excerpt, so a hunk
//! made with `-U10` keeps its ten lines.

use crate::render::{Line, Span};

use crate::diff::{DiffConfig, faint_line, render_in};
use crate::sidediff::{EditView, SplitConfig, lang_for, render_split};
use crate::style::Role;

/// A parsed patch: what came before the first file, and the files.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Patch {
    /// Lines before the first file: a commit's headers and message.
    pub preamble: Vec<String>,
    pub files: Vec<FilePatch>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FilePatch {
    /// The old name, `None` for a created file (`/dev/null`).
    pub old_path: Option<String>,
    /// The new name, `None` for a deleted file.
    pub new_path: Option<String>,
    /// `diff --git` extended headers worth saying (`new file mode 100644`,
    /// `similarity index 90%`, `Binary files … differ`); `index` lines are not.
    pub notes: Vec<String>,
    pub hunks: Vec<PatchHunk>,
    /// Lines after the last hunk that belong to no hunk (a `format-patch`
    /// signature, a mail footer).
    pub trailer: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatchHunk {
    /// 1-based first line of each side, from the `@@` header. A side with no
    /// lines says 0 there; it is kept as given.
    pub old_start: usize,
    pub new_start: usize,
    /// The old side: context and removed lines, in order.
    pub old: Vec<String>,
    /// The new side: context and added lines, in order.
    pub new: Vec<String>,
}

impl FilePatch {
    /// The name a reader knows the file by: the new one, or the old one for a
    /// deletion.
    pub fn path(&self) -> &str {
        self.new_path
            .as_deref()
            .or(self.old_path.as_deref())
            .unwrap_or("")
    }
}

/// Parse a unified diff. Never fails: what it does not recognise is kept as
/// text (the preamble, a file's trailer), so nothing in the file is dropped.
pub fn parse(text: &str) -> Patch {
    let lines: Vec<&str> = text.lines().collect();
    let mut p = Patch::default();
    let mut i = 0usize;
    while i < lines.len() {
        let l = lines[i];
        if is_file_start(&lines, i) {
            let (f, next) = parse_file(&lines, i);
            p.files.push(f);
            i = next;
            continue;
        }
        match p.files.last_mut() {
            Some(f) => f.trailer.push(l.to_string()),
            None => p.preamble.push(l.to_string()),
        }
        i += 1;
    }
    p
}

/// A file starts at `diff --git`, or at a `--- ` line followed by `+++ `.
fn is_file_start(lines: &[&str], i: usize) -> bool {
    lines[i].starts_with("diff --git ")
        || (lines[i].starts_with("--- ") && lines.get(i + 1).is_some_and(|n| n.starts_with("+++ ")))
}

fn parse_file(lines: &[&str], mut i: usize) -> (FilePatch, usize) {
    let mut f = FilePatch::default();
    if let Some(rest) = lines[i].strip_prefix("diff --git ") {
        // The names on this line are the fallback: a mode change or a binary
        // file has no `---` / `+++` to say them.
        if let Some((a, b)) = split_git_names(rest) {
            f.old_path = Some(a);
            f.new_path = Some(b);
        }
        i += 1;
        while i < lines.len() && !lines[i].starts_with("--- ") && !lines[i].starts_with("@@ ") {
            let l = lines[i];
            if l.starts_with("diff --git ") {
                return (f, i);
            }
            if l.starts_with("new file mode") {
                f.old_path = None;
            } else if l.starts_with("deleted file mode") {
                f.new_path = None;
            }
            if !l.starts_with("index ") && is_git_header(l) {
                f.notes.push(l.to_string());
            } else if !is_git_header(l) {
                // Not a header: the file has ended (a mode-only change
                // followed by prose).
                return (f, i);
            }
            i += 1;
        }
    }
    if i < lines.len() && lines[i].starts_with("--- ") {
        f.old_path = name_of(&lines[i][4..], "a/");
        i += 1;
        if i < lines.len() && lines[i].starts_with("+++ ") {
            f.new_path = name_of(&lines[i][4..], "b/");
            i += 1;
        }
    }
    while i < lines.len() {
        let Some((old_start, old_n, new_start, new_n)) = hunk_header(lines[i]) else {
            break;
        };
        i += 1;
        let mut h = PatchHunk {
            old_start,
            new_start,
            old: Vec::new(),
            new: Vec::new(),
        };
        let (mut old_left, mut new_left) = (old_n, new_n);
        while i < lines.len() && (old_left > 0 || new_left > 0) {
            let l = lines[i];
            match l.as_bytes().first() {
                Some(b'+') if new_left > 0 => {
                    h.new.push(l[1..].to_string());
                    new_left -= 1;
                }
                Some(b'-') if old_left > 0 => {
                    h.old.push(l[1..].to_string());
                    old_left -= 1;
                }
                Some(b'\\') => {}
                // Context: a leading space, or an empty line whose space an
                // editor stripped.
                Some(b' ') | None if old_left > 0 && new_left > 0 => {
                    let t = l.get(1..).unwrap_or("").to_string();
                    h.old.push(t.clone());
                    h.new.push(t);
                    old_left -= 1;
                    new_left -= 1;
                }
                // The counts say more is due, and this line is not it: the
                // patch is truncated or hand-edited. Stop here rather than
                // swallow what follows.
                _ => break,
            }
            i += 1;
        }
        // "\ No newline at end of file" after the last line.
        while i < lines.len() && lines[i].starts_with('\\') {
            i += 1;
        }
        f.hunks.push(h);
    }
    (f, i)
}

/// `diff --git` extended header lines.
fn is_git_header(l: &str) -> bool {
    const HEADS: [&str; 13] = [
        "index ",
        "old mode ",
        "new mode ",
        "new file mode ",
        "deleted file mode ",
        "similarity index ",
        "dissimilarity index ",
        "rename from ",
        "rename to ",
        "copy from ",
        "copy to ",
        "Binary files ",
        "GIT binary patch",
    ];
    HEADS.iter().any(|h| l.starts_with(h))
}

/// `a/x b/y` from a `diff --git` line. Names with spaces are ambiguous there;
/// the split is at ` b/`, which is right whenever the old name has no ` b/`.
fn split_git_names(rest: &str) -> Option<(String, String)> {
    let i = rest.find(" b/")?;
    let a = rest[..i].strip_prefix("a/").unwrap_or(&rest[..i]);
    let b = &rest[i + 3..];
    Some((a.to_string(), b.to_string()))
}

/// A `---` / `+++` name: `/dev/null` is no file, a tab ends the name (plain
/// `diff -u` puts the timestamp after one), and git's `a/` / `b/` is dropped.
fn name_of(s: &str, git_prefix: &str) -> Option<String> {
    let s = s.split('\t').next().unwrap_or(s).trim_end();
    if s == "/dev/null" {
        return None;
    }
    Some(s.strip_prefix(git_prefix).unwrap_or(s).to_string())
}

/// `@@ -a[,b] +c[,d] @@…` → (a, b, c, d); a missing count is 1.
fn hunk_header(l: &str) -> Option<(usize, usize, usize, usize)> {
    let rest = l.strip_prefix("@@ -")?;
    let (ranges, _) = rest.split_once(" @@")?;
    let (old, new) = ranges.split_once(" +")?;
    let range = |r: &str| -> Option<(usize, usize)> {
        match r.split_once(',') {
            Some((s, n)) => Some((s.parse().ok()?, n.parse().ok()?)),
            None => Some((r.parse().ok()?, 1)),
        }
    };
    let (a, b) = range(old)?;
    let (c, d) = range(new)?;
    Some((a, b, c, d))
}

/// Draw a parsed patch: the preamble faint, then per file its name (and what
/// its headers say), then each hunk in `view` at the file's own line numbers.
pub fn render(p: &Patch, cfg: &DiffConfig, view: EditView) -> Vec<Line> {
    let pal = cfg.palette;
    let mut out: Vec<Line> = p.preamble.iter().map(|l| faint_line(pal, l)).collect();
    for f in &p.files {
        if !out.is_empty() {
            out.push(Line::default());
        }
        let name = match (&f.old_path, &f.new_path) {
            (Some(a), Some(b)) if a != b => format!("{a} → {b}"),
            (None, Some(b)) => format!("{b} (new)"),
            (Some(a), None) => format!("{a} (deleted)"),
            _ => f.path().to_string(),
        };
        out.push(Line::from(Span::styled(name, pal.style(Role::Strong))));
        out.extend(f.notes.iter().map(|n| faint_line(pal, n)));
        let lang = lang_for(f.path());
        for h in &f.hunks {
            let old: Vec<&str> = h.old.iter().map(String::as_str).collect();
            let new: Vec<&str> = h.new.iter().map(String::as_str).collect();
            // The whole excerpt is context: the patch already chose how much.
            let hcfg = DiffConfig {
                context: old.len().max(new.len()),
                ..*cfg
            };
            // A side with no lines says 0 in its header; the renderers want the
            // 1-based line the excerpt starts at, which is then 1.
            let (os, ns) = (h.old_start.max(1), h.new_start.max(1));
            match view {
                EditView::Unified => out.extend(render_in(&old, &new, &hcfg, os, ns, lang)),
                EditView::Split => out.extend(render_split(
                    &old,
                    &new,
                    &SplitConfig {
                        cfg: &hcfg,
                        before_start: os,
                        after_start: ns,
                        lang,
                    },
                )),
            }
        }
        out.extend(f.trailer.iter().map(|l| faint_line(pal, l)));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::style::Palette;

    fn text(rows: &[Line]) -> Vec<String> {
        rows.iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_str()).collect())
            .collect()
    }

    const GIT: &str = "\
From 4a9dee5 Mon Sep 17 00:00:00 2001
Subject: [PATCH] fix

body text

diff --git a/src/a.rs b/src/a.rs
index 111..222 100644
--- a/src/a.rs
+++ b/src/a.rs
@@ -10,4 +10,4 @@ fn main() {
 fn a() {
-    let total = 1;
+    let sum = 1;
 }

diff --git a/new.txt b/new.txt
new file mode 100644
index 0000000..333
--- /dev/null
+++ b/new.txt
@@ -0,0 +1,2 @@
+one
+two
diff --git a/img.png b/img.png
Binary files a/img.png and b/img.png differ
--\x20
2.43.0
";

    #[test]
    fn a_git_patch_parses_into_its_files_and_hunks() {
        let p = parse(GIT);
        assert_eq!(p.preamble[0], "From 4a9dee5 Mon Sep 17 00:00:00 2001");
        assert!(p.preamble.contains(&"body text".to_string()));
        assert_eq!(p.files.len(), 3, "{:#?}", p.files);

        let a = &p.files[0];
        assert_eq!(a.path(), "src/a.rs");
        assert!(
            a.notes.is_empty(),
            "index lines are not notes: {:?}",
            a.notes
        );
        assert_eq!(a.hunks.len(), 1);
        let h = &a.hunks[0];
        assert_eq!((h.old_start, h.new_start), (10, 10));
        // The empty context line (its space stripped) is still context, on
        // both sides, and the counts close the hunk exactly.
        assert_eq!(h.old, vec!["fn a() {", "    let total = 1;", "}", ""]);
        assert_eq!(h.new, vec!["fn a() {", "    let sum = 1;", "}", ""]);
        assert!(a.trailer.is_empty(), "{:?}", a.trailer);

        let n = &p.files[1];
        assert_eq!(n.old_path, None);
        assert_eq!(n.new_path.as_deref(), Some("new.txt"));
        assert_eq!(n.hunks[0].new, vec!["one", "two"]);
        assert!(n.hunks[0].old.is_empty());

        let b = &p.files[2];
        assert_eq!(b.path(), "img.png");
        assert_eq!(b.notes, vec!["Binary files a/img.png and b/img.png differ"]);
        // format-patch's signature belongs to no hunk.
        assert_eq!(b.trailer, vec!["-- ", "2.43.0"]);
    }

    /// A removed line that starts `-- ` reads like a `---` header and an added
    /// one that starts `++ ` like `+++`; the counts keep both in the hunk.
    #[test]
    fn a_hunk_is_read_by_its_counts_not_by_its_look() {
        let p = parse(
            "--- a/x\n+++ b/x\n@@ -1,2 +1,2 @@\n--- old dashes\n keep\n+++ new pluses\n keep2\n",
        );
        // Old side: "-- old dashes", "keep"; new side "keep", "++ new pluses"
        // — and " keep2" is past both counts, so it is not in the hunk.
        let h = &p.files[0].hunks[0];
        assert_eq!(h.old, vec!["-- old dashes", "keep"]);
        assert_eq!(h.new, vec!["keep", "++ new pluses"]);
        assert_eq!(p.files[0].trailer, vec![" keep2"]);
    }

    #[test]
    fn a_plain_diff_u_parses_with_its_timestamps_dropped() {
        let p = parse(
            "--- old/f.c\t2026-10-07 10:00:00\n+++ new/f.c\t2026-10-07 10:01:00\n@@ -3 +3 @@\n-int x;\n+long x;\n\\ No newline at end of file\n",
        );
        let f = &p.files[0];
        assert_eq!(f.old_path.as_deref(), Some("old/f.c"));
        assert_eq!(f.new_path.as_deref(), Some("new/f.c"));
        let h = &f.hunks[0];
        assert_eq!((h.old_start, h.new_start), (3, 3));
        assert_eq!(h.old, vec!["int x;"]);
        assert_eq!(h.new, vec!["long x;"]);
        assert!(f.trailer.is_empty());
    }

    #[test]
    fn text_with_no_diff_in_it_is_all_preamble() {
        let p = parse("just\nprose\n");
        assert!(p.files.is_empty());
        assert_eq!(p.preamble, vec!["just", "prose"]);
    }

    #[test]
    fn a_patch_draws_at_the_files_own_line_numbers_in_both_views() {
        let p = parse(GIT);
        let cfg = DiffConfig {
            width: 100,
            palette: Palette::None,
            max_rows: usize::MAX,
            ..Default::default()
        };
        let rows = text(&render(&p, &cfg, EditView::Unified));
        assert!(rows.contains(&"src/a.rs".to_string()), "{rows:?}");
        assert!(rows.contains(&"@@ -10,4 +10,4 @@".to_string()), "{rows:?}");
        assert!(
            rows.contains(&"11 -    let total = 1;".to_string()),
            "{rows:?}"
        );
        assert!(
            rows.contains(&"11 +    let sum = 1;".to_string()),
            "{rows:?}"
        );
        assert!(rows.contains(&"new.txt (new)".to_string()), "{rows:?}");
        assert!(rows.contains(&"1 +one".to_string()), "{rows:?}");
        assert!(
            rows.iter().any(|r| r.starts_with("Binary files")),
            "{rows:?}"
        );
        assert_eq!(rows.last().map(String::as_str), Some("2.43.0"));

        let rows = text(&render(&p, &cfg, EditView::Split));
        let pair = rows
            .iter()
            .find(|r| r.contains("let sum"))
            .expect("the changed pair");
        let (left, right) = pair.split_once('│').expect("two panels");
        assert!(
            left.contains("11 - ") && left.contains("let total"),
            "{pair:?}"
        );
        assert!(
            right.contains("11 + ") && right.contains("let sum"),
            "{pair:?}"
        );
    }

    /// A hunk made with more context than three keeps all of it.
    #[test]
    fn a_wide_context_hunk_keeps_its_context() {
        let mut s = String::from("--- a/x\n+++ b/x\n@@ -1,9 +1,9 @@\n");
        for i in 0..4 {
            s.push_str(&format!(" c{i}\n"));
        }
        s.push_str("-old\n+new\n");
        for i in 4..8 {
            s.push_str(&format!(" c{i}\n"));
        }
        let cfg = DiffConfig {
            palette: Palette::None,
            max_rows: usize::MAX,
            ..Default::default()
        };
        let rows = text(&render(&parse(&s), &cfg, EditView::Unified));
        assert!(rows.iter().any(|r| r.ends_with(" c0")), "{rows:?}");
        assert!(rows.iter().any(|r| r.ends_with(" c7")), "{rows:?}");
    }
}
