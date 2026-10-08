//! Syntax roles: **[`crate::syntax`]'s captures, [`crate::style`]'s roles.**
//!
//! Ported from letibot's `crates/ui/src/highlight.rs`. The engine is
//! [`crate::syntax`]; what lives here is the other half, `capture name → Role`,
//! for everything that paints syntax: the diffs in [`crate::diff`] and
//! [`crate::sidediff`], rendered markdown's fences, and the editor's own theme,
//! which adds only the names its queries use beyond these. One table: two copies
//! would drift, and the drift would be invisible — the same Rust coloured
//! differently in two panes of one screen.

use crate::style::Role;
use crate::syntax::{Highlighter, Lang};

/// Capture names onto the six syntax roles.
///
/// A name the table does not list falls back to its prefix (`type.builtin` →
/// `type`), and only a name with no known prefix at all (`variable`,
/// `punctuation.bracket`) is plain — which is honest, because those are the
/// tokens a reader does not need coloured.
///
/// # Why six
///
/// A terminal has expensive colour and cheap structure, and six roles is what a
/// reader holds: a keyword, a type, a function name, a string, a number, a
/// comment. What is not mapped is not lost: [`Role::Plain`] is the reader's own
/// foreground, which is the right colour for punctuation.
pub fn role_for_capture(name: &str) -> Role {
    match name {
        "comment" => Role::Comment,
        "string" | "escape" => Role::StringLit,
        "number" | "constant" | "property" => Role::NumberLit,
        "type" | "constructor" | "label" => Role::TypeName,
        "keyword" | "include" | "preproc" | "variable.builtin" => Role::Keyword,
        "function" => Role::FuncName,
        // A diff file's own lines (`syntax`'s diff query names them for what they
        // are rather than borrowing `@keyword`). Without these a ```diff fence
        // in rendered markdown came out plain: every name here starts `diff.`,
        // and `diff` is no role. Green and red are the sign colours the diff
        // renderers already give `+` and `-`; a hunk header is cyan, as the
        // editor's theme draws it; a file header anchors a scan.
        "diff.plus" => Role::Success,
        "diff.minus" => Role::Failure,
        "diff.hunk" => Role::Code,
        "diff.file" => Role::Strong,
        _ => match name.split_once('.') {
            Some((prefix, _)) => role_for_capture(prefix),
            None => Role::Plain,
        },
    }
}

/// One role per character, one row per line, for `lines` read as one text in
/// `lang`; rows of nothing (plain) when there is no language. The whole text is
/// parsed at once, so a construct spanning lines keeps its scope.
///
/// The grid can come back shorter than `lines` — [`Highlighter::classes`] is
/// empty when a parser will not take the language — so callers index it with
/// `get` and read a missing row as plain.
pub fn role_grid(lines: &[&str], lang: Option<Lang>) -> Vec<Vec<Role>> {
    let Some(lang) = lang else {
        return vec![Vec::new(); lines.len()];
    };
    let src = lines.join("\n");
    Highlighter::new()
        .classes(&src, lang)
        .into_iter()
        .map(|row| {
            row.into_iter()
                .map(|c| c.map(|n| role_for_capture(&n)).unwrap_or(Role::Plain))
                .collect()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_common_captures_land_on_a_role() {
        for (name, want) in [
            ("keyword", Role::Keyword),
            ("string", Role::StringLit),
            ("comment", Role::Comment),
            ("number", Role::NumberLit),
            ("type", Role::TypeName),
            ("function", Role::FuncName),
            ("constructor", Role::TypeName),
            ("escape", Role::StringLit),
            ("property", Role::NumberLit),
            ("include", Role::Keyword),
        ] {
            assert_eq!(role_for_capture(name), want, "{name}");
        }
    }

    #[test]
    fn a_diff_file_s_lines_have_roles() {
        assert_eq!(role_for_capture("diff.plus"), Role::Success);
        assert_eq!(role_for_capture("diff.minus"), Role::Failure);
        assert_eq!(role_for_capture("diff.hunk"), Role::Code);
        assert_eq!(role_for_capture("diff.file"), Role::Strong);
        let g = role_grid(
            &["--- a/f", "+++ b/f", "@@ -1 +1 @@", "-old", "+new"],
            Some(Lang::Diff),
        );
        assert_eq!(g[3].first(), Some(&Role::Failure), "{g:?}");
        assert_eq!(g[4].first(), Some(&Role::Success), "{g:?}");
    }

    /// A dotted name falls back to its prefix, one level at a time — which is what
    /// keeps a grammar's new sub-capture from rendering plain the day it appears.
    #[test]
    fn a_dotted_name_falls_back_to_its_prefix() {
        assert_eq!(role_for_capture("type.builtin"), Role::TypeName);
        assert_eq!(role_for_capture("function.method"), Role::FuncName);
        assert_eq!(
            role_for_capture("keyword.control.conditional"),
            Role::Keyword
        );
        assert_eq!(role_for_capture("string.escape"), Role::StringLit);
        assert_eq!(role_for_capture("variable.builtin"), Role::Keyword);
    }

    /// What is not drawn is plain, not guessed at.
    #[test]
    fn punctuation_and_variables_are_plain() {
        for name in [
            "punctuation.bracket",
            "punctuation.delimiter",
            "variable",
            "variable.parameter",
            "operator",
            "",
        ] {
            assert_eq!(role_for_capture(name), Role::Plain, "{name:?}");
        }
    }
}
