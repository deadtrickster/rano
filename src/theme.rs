//! **Themes: the role table, overridden by name.**
//!
//! Everything rano and its hosts draw is a [`Role`], and [`Palette::look`] is the one table
//! that says what a role looks like — by default in the terminal's own sixteen slots, so the
//! terminal's theme is the theme. A [`Theme`] is a set of overrides on that table, read from
//! a file of one `role = "look"` line each (the same flat `key = "value"` subset every config
//! here is written in):
//!
//! ```text
//! # ~/.config/letibot/themes/gray.toml
//! user_block = "bg:#2f363b"
//! heading    = "fg:#6fbcbf bold"
//! added      = "bg:cube:22"
//! faint      = "fg:8"
//! ```
//!
//! A role's name is its variant in snake case (`UserBlock` → `user_block`), so the list of
//! names cannot drift from the list of roles. A look is space-separated words:
//!
//! - `fg:N` / `bg:N` — a theme slot, `0`–`15`;
//! - `fg:#rrggbb` / `bg:#rrggbb` — an exact colour (truecolor), for a theme that should look
//!   the same whatever the terminal's palette;
//! - `fg:cube:N` / `bg:cube:N` — an xterm 256-colour index;
//! - `bold`, `dim`, `italic`, `underline`, `reverse`;
//! - `plain` — no look at all (only on its own).
//!
//! An override replaces the role's look whole: `heading = "fg:4"` is blue and NOT bold.
//! A line the reader cannot use is reported, never silently skipped — a typo and a deliberate
//! default must not draw the same screen.
//!
//! One theme is active per process ([`set_active`]); [`Palette::None`] ignores it, so a replay
//! diff and a CI log stay plain whatever is installed.

use std::path::{Path, PathBuf};
use std::sync::RwLock;

use crate::style::{Attrs, Hue, Look, Palette, Role};

/// A named set of role overrides.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Theme {
    pub name: String,
    pub roles: Vec<(Role, Look)>,
}

/// The theme every [`Palette::look`] consults first. `None`: the default table.
///
/// One per process — except in this crate's own tests, where it is one per THREAD: tests run
/// in parallel, and one that activated a theme would otherwise recolour every other test's
/// assertions for as long as it held it.
#[cfg(not(test))]
static ACTIVE: RwLock<Option<Theme>> = RwLock::new(None);
#[cfg(test)]
thread_local! {
    static ACTIVE: RwLock<Option<Theme>> = const { RwLock::new(None) };
}

fn with_active<T>(f: impl FnOnce(&RwLock<Option<Theme>>) -> T) -> T {
    #[cfg(not(test))]
    {
        f(&ACTIVE)
    }
    #[cfg(test)]
    {
        ACTIVE.with(f)
    }
}

/// Make `theme` the one every look consults; `None` goes back to the default table.
pub fn set_active(theme: Option<Theme>) {
    with_active(|a| {
        if let Ok(mut a) = a.write() {
            *a = theme.filter(|t| !t.roles.is_empty());
        }
    })
}

/// The active theme's name, when there is one.
pub fn active_name() -> Option<String> {
    with_active(|a| a.read().ok()?.as_ref().map(|t| t.name.clone()))
}

/// The active theme's look for `r`, when it overrides it. [`Palette::look`]'s first question.
pub(crate) fn override_for(r: Role) -> Option<Look> {
    with_active(|a| {
        let a = a.read().ok()?;
        a.as_ref()?
            .roles
            .iter()
            .rev()
            .find(|(role, _)| *role == r)
            .map(|(_, look)| *look)
    })
}

impl Role {
    /// **The role's name in a theme file**: its variant in snake case.
    pub fn name(self) -> String {
        let debug = format!("{self:?}");
        let mut out = String::with_capacity(debug.len() + 4);
        for (i, c) in debug.chars().enumerate() {
            if c.is_ascii_uppercase() {
                if i > 0 {
                    out.push('_');
                }
                out.push(c.to_ascii_lowercase());
            } else {
                out.push(c);
            }
        }
        out
    }

    /// The role a theme file names, or `None` for a name no role has.
    pub fn from_name(name: &str) -> Option<Role> {
        let want = name.trim().to_ascii_lowercase();
        Role::ALL.into_iter().find(|r| r.name() == want)
    }
}

impl Look {
    /// **A look from its spelling in a theme file** — see the module header. `Err` says
    /// which word could not be read and why.
    pub fn parse(spec: &str) -> Result<Look, String> {
        let words: Vec<&str> = spec.split_whitespace().collect();
        if words.is_empty() {
            return Err("an empty look (say `plain` for none)".into());
        }
        if words == ["plain"] {
            return Ok(Look::PLAIN);
        }
        let mut look = Look::PLAIN;
        for w in words {
            match w {
                "bold" => look.attrs = look.attrs.union(Attrs::BOLD),
                "dim" => look.attrs = look.attrs.union(Attrs::DIM),
                "italic" => look.attrs = look.attrs.union(Attrs::ITALIC),
                "underline" => look.attrs = look.attrs.union(Attrs::UNDERLINE),
                "reverse" => look.attrs = look.attrs.union(Attrs::REVERSE),
                "plain" => return Err("`plain` means no look, so it stands alone".into()),
                _ => {
                    let (side, hue) = w
                        .split_once(':')
                        .ok_or_else(|| format!("`{w}` is not a look word"))?;
                    let hue = parse_hue(hue).map_err(|e| format!("`{w}`: {e}"))?;
                    match side {
                        "fg" => look.fg = Some(hue),
                        "bg" => look.bg = Some(hue),
                        _ => return Err(format!("`{w}`: a colour is `fg:` or `bg:`")),
                    }
                }
            }
        }
        Ok(look)
    }

    /// **The look as a theme file spells it** — the inverse of [`Look::parse`].
    pub fn spec(&self) -> String {
        let mut words = Vec::new();
        for (side, hue) in [("fg", self.fg), ("bg", self.bg)] {
            if let Some(h) = hue {
                words.push(format!("{side}:{}", hue_spec(h)));
            }
        }
        for (a, name) in [
            (Attrs::BOLD, "bold"),
            (Attrs::DIM, "dim"),
            (Attrs::ITALIC, "italic"),
            (Attrs::UNDERLINE, "underline"),
            (Attrs::REVERSE, "reverse"),
        ] {
            if self.attrs.contains(a) {
                words.push(name.into());
            }
        }
        if words.is_empty() {
            "plain".into()
        } else {
            words.join(" ")
        }
    }
}

fn parse_hue(s: &str) -> Result<Hue, String> {
    if let Some(hex) = s.strip_prefix('#') {
        if hex.len() != 6 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err("a colour is #rrggbb".into());
        }
        let n = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).unwrap_or(0);
        return Ok(Hue::Rgb(n(0), n(2), n(4)));
    }
    if let Some(i) = s.strip_prefix("cube:") {
        return i
            .parse::<u8>()
            .map(Hue::Cube)
            .map_err(|_| "a cube index is 0–255".into());
    }
    match s.parse::<u8>() {
        Ok(n) if n < 16 => Ok(Hue::Slot(n)),
        _ => Err("a slot is 0–15 (or #rrggbb, or cube:N)".into()),
    }
}

fn hue_spec(h: Hue) -> String {
    match h {
        Hue::Slot(n) => n.to_string(),
        Hue::Cube(n) => format!("cube:{n}"),
        Hue::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}"),
    }
}

impl Theme {
    /// **A theme from its file's text**, and a sentence for every line it could not use.
    /// Comments (`#` to the end of a line outside quotes) and blank lines are skipped.
    pub fn parse(name: &str, text: &str) -> (Theme, Vec<String>) {
        let mut theme = Theme {
            name: name.to_string(),
            roles: Vec::new(),
        };
        let mut problems = Vec::new();
        for (n, raw) in text.lines().enumerate() {
            let line = strip_comment(raw).trim();
            if line.is_empty() {
                continue;
            }
            let at = n + 1;
            let Some((k, v)) = line.split_once('=') else {
                problems.push(format!("line {at}: `{line}` is not `role = \"look\"`"));
                continue;
            };
            let key = k.trim();
            let value = v.trim().trim_matches('"');
            let Some(role) = Role::from_name(key) else {
                problems.push(format!("line {at}: `{key}` is not a role"));
                continue;
            };
            match Look::parse(value) {
                Ok(look) => theme.roles.push((role, look)),
                Err(e) => problems.push(format!("line {at}: {key}: {e}")),
            }
        }
        (theme, problems)
    }

    /// **The theme named `name` in `dir`** (`dir/NAME.toml`), or why there is none.
    pub fn load(dir: &Path, name: &str) -> Result<(Theme, Vec<String>), String> {
        let path = theme_path(dir, name);
        let text = std::fs::read_to_string(&path)
            .map_err(|e| format!("theme `{name}`: {}: {e}", path.display()))?;
        Ok(Theme::parse(name, &text))
    }

    /// The file as [`Theme::parse`] reads it back — every role, overridden or not, so a
    /// reader has the whole table to start from.
    pub fn template() -> String {
        let mut out = String::from(
            "# Every role rano and letibot draw, with its default look. Delete the lines you\n\
             # do not change; a line overrides its role whole. See rano's `theme` module.\n",
        );
        for r in Role::ALL {
            out.push_str(&format!(
                "{} = \"{}\"\n",
                r.name(),
                Palette::Colour.base_look(r).spec()
            ));
        }
        out
    }
}

/// **The theme a config asks for**: the named file's roles (when `name` is given), then the
/// config's own `color.ROLE` lines on top, in order — and a sentence for everything that
/// could not be used, prefixed with where it was. `None` when neither says anything, which
/// leaves the default table in charge.
pub fn resolve(
    dir: &Path,
    name: Option<&str>,
    colors: &[(String, String)],
) -> (Option<Theme>, Vec<String>) {
    let mut problems = Vec::new();
    let mut theme = Theme {
        name: name.unwrap_or("config").to_string(),
        roles: Vec::new(),
    };
    if let Some(n) = name.filter(|n| !n.is_empty() && *n != "terminal") {
        match Theme::load(dir, n) {
            Ok((t, p)) => {
                theme.roles = t.roles;
                let file = theme_path(dir, n);
                problems.extend(p.into_iter().map(|p| format!("{}: {p}", file.display())));
            }
            Err(e) => problems.push(e),
        }
    }
    for (role, spec) in colors {
        let Some(r) = Role::from_name(role) else {
            problems.push(format!("color.{role}: `{role}` is not a role"));
            continue;
        };
        match Look::parse(spec) {
            Ok(l) => theme.roles.push((r, l)),
            Err(e) => problems.push(format!("color.{role}: {e}")),
        }
    }
    ((!theme.roles.is_empty()).then_some(theme), problems)
}

/// `dir/NAME.toml`, with a name that is a path refused rather than followed.
pub fn theme_path(dir: &Path, name: &str) -> PathBuf {
    let safe: String = name
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        .collect();
    dir.join(format!("{}.toml", safe.trim_start_matches('.')))
}

fn strip_comment(line: &str) -> &str {
    let mut quoted = false;
    for (i, c) in line.char_indices() {
        match c {
            '"' => quoted = !quoted,
            '#' if !quoted => return &line[..i],
            _ => {}
        }
    }
    line
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_role_has_a_name_it_is_found_by() {
        for r in Role::ALL {
            assert_eq!(Role::from_name(&r.name()), Some(r), "{r:?}");
        }
        assert_eq!(Role::UserBlock.name(), "user_block");
        assert_eq!(Role::from_name("nonsense"), None);
    }

    #[test]
    fn a_look_reads_slots_exact_colours_cube_indices_and_attributes() {
        let l = Look::parse("fg:#5aa7ff bg:0 bold underline").unwrap();
        assert_eq!(l.fg, Some(Hue::Rgb(0x5a, 0xa7, 0xff)));
        assert_eq!(l.bg, Some(Hue::Slot(0)));
        assert!(l.attrs.contains(Attrs::BOLD) && l.attrs.contains(Attrs::UNDERLINE));
        assert_eq!(Look::parse("bg:cube:22").unwrap().bg, Some(Hue::Cube(22)));
        assert_eq!(Look::parse("plain").unwrap(), Look::PLAIN);
        for bad in ["", "fg:16", "fg:#12345", "colour:3", "shiny", "plain bold"] {
            assert!(Look::parse(bad).is_err(), "{bad:?}");
        }
        // The spelling reads back as itself.
        assert_eq!(Look::parse(&l.spec()).unwrap(), l);
    }

    #[test]
    fn an_exact_colour_is_truecolor_on_the_wire() {
        let l = Look::parse("fg:#5aa7ff bg:#2f363b").unwrap();
        assert_eq!(l.sgr(), "\x1b[38;2;90;167;255;48;2;47;54;59m");
    }

    #[test]
    fn a_theme_file_overrides_its_roles_and_reports_what_it_cannot_use() {
        let (t, problems) = Theme::parse(
            "gray",
            "# a comment\n\nuser_block = \"bg:#2f363b\" # trailing\nheading = \"fg:4\"\n\
             nope = \"bold\"\nfaint = \"fg:99\"\nnot a line\n",
        );
        assert_eq!(t.roles.len(), 2);
        assert_eq!(problems.len(), 3, "{problems:?}");
        assert!(problems[0].contains("`nope` is not a role"), "{problems:?}");
        assert!(problems[1].contains("faint"), "{problems:?}");
        assert!(problems[2].contains("line 7"), "{problems:?}");
    }

    #[test]
    fn the_active_theme_is_what_a_look_is_and_palette_none_ignores_it() {
        let default = Palette::Colour.look(Role::Heading);
        let (t, _) = Theme::parse("t", "heading = \"fg:#ff0000\"\n");
        set_active(Some(t));
        assert_eq!(
            Palette::Colour.look(Role::Heading).fg,
            Some(Hue::Rgb(255, 0, 0))
        );
        // Whole, not merged: the default's bold is gone.
        assert!(
            !Palette::Colour
                .look(Role::Heading)
                .attrs
                .contains(Attrs::BOLD)
        );
        assert_eq!(
            Palette::Light.look(Role::Heading).fg,
            Some(Hue::Rgb(255, 0, 0))
        );
        assert_eq!(Palette::None.look(Role::Heading), Look::PLAIN);
        // A role the theme does not name keeps its default.
        assert_eq!(
            Palette::Colour.look(Role::Code),
            Palette::Colour.base_look(Role::Code)
        );
        assert_eq!(active_name().as_deref(), Some("t"));
        set_active(None);
        assert_eq!(Palette::Colour.look(Role::Heading), default);
        assert_eq!(active_name(), None);
    }

    #[test]
    fn the_template_reads_back_as_the_default_table() {
        let (t, problems) = Theme::parse("template", &Theme::template());
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(t.roles.len(), Role::ALL.len());
        for (r, l) in t.roles {
            assert_eq!(l, Palette::Colour.base_look(r), "{r:?}");
        }
    }

    /// A named theme, then the config's own `color.` lines on top; `terminal` is the
    /// default table by name; and what cannot be used is said with where it was.
    #[test]
    fn a_config_resolves_its_theme_then_its_own_colour_lines() {
        let d = std::env::temp_dir().join(format!("rano-theme-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("gray.toml"), "heading = \"fg:4\"\ncode = \"fg:6\"\n").unwrap();
        let (t, p) = resolve(
            &d,
            Some("gray"),
            &[
                ("code".into(), "fg:#ff0000".into()),
                ("wat".into(), "bold".into()),
            ],
        );
        let t = t.unwrap();
        assert_eq!(t.name, "gray");
        let code: Vec<_> = t.roles.iter().filter(|(r, _)| *r == Role::Code).collect();
        assert_eq!(
            code.last().unwrap().1.fg,
            Some(Hue::Rgb(255, 0, 0)),
            "the line wins"
        );
        assert_eq!(p, vec!["color.wat: `wat` is not a role".to_string()]);
        assert_eq!(resolve(&d, Some("terminal"), &[]), (None, vec![]));
        let (none, p) = resolve(&d, Some("absent"), &[]);
        assert!(none.is_none() && p[0].contains("theme `absent`"), "{p:?}");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_theme_name_is_a_file_in_the_directory_and_never_a_path() {
        let d = Path::new("/themes");
        assert_eq!(theme_path(d, "gray"), Path::new("/themes/gray.toml"));
        assert_eq!(
            theme_path(d, "../../etc/passwd"),
            Path::new("/themes/etcpasswd.toml")
        );
    }
}
