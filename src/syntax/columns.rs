//! The columnar system databases: `fstab`, `passwd`, `hosts` and their kin.
//!
//! No tree-sitter grammar covers them, so each is coloured by one rule - comment faint,
//! first field strong, separator faint - with the separator and the comment character
//! taken from its own man page.

use crate::render::Style;
use crate::style::Role;
use std::path::Path;

/// One system database, by its documented shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Format {
    /// `fstab(5)`: device, mountpoint, fstype, options, dump, pass.
    Fstab,
    /// `passwd(5)`: name, password, uid, gid, gecos, home, shell.
    Passwd,
    /// `group(5)`: name, password, gid, members.
    Group,
    /// `shadow(5)`: login, password, then the aging fields.
    Shadow,
    /// `hosts(5)`: address, then names.
    Hosts,
    /// `services(5)`: name, port/protocol, aliases.
    Services,
    /// `protocols(5)`: name, number, aliases.
    Protocols,
    /// `crontab(5)`: five time fields, then the command.
    Crontab,
}

/// What separates one field from the next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Sep {
    /// A run of spaces and/or tabs (`fstab`, `hosts`, `services`, `crontab`).
    Whitespace,
    /// One `:`, with empty fields allowed (`passwd`, `group`, `shadow`).
    Colon,
}

impl Format {
    /// The name a status line or a test calls this.
    pub fn name(self) -> &'static str {
        match self {
            Format::Fstab => "fstab",
            Format::Passwd => "passwd",
            Format::Group => "group",
            Format::Shadow => "shadow",
            Format::Hosts => "hosts",
            Format::Services => "services",
            Format::Protocols => "protocols",
            Format::Crontab => "crontab",
        }
    }

    fn sep(self) -> Sep {
        match self {
            Format::Passwd | Format::Group | Format::Shadow => Sep::Colon,
            _ => Sep::Whitespace,
        }
    }

    /// Whether a `#` starts a comment. The colon databases have none: a `#` in a password
    /// field is a character, not an annotation.
    fn has_comments(self) -> bool {
        !matches!(self, Format::Passwd | Format::Group | Format::Shadow)
    }

    /// How many fields the man page documents. Used by the table's own test, and worth
    /// keeping beside the format so a reader can check the rule against the page.
    pub fn fields(self) -> usize {
        match self {
            Format::Fstab => 6,
            Format::Passwd => 7,
            Format::Group => 4,
            // login, password, then `lastchg`, `min`, `max`, `warn`, `inactive`, `expire`,
            // `flag` — the tail is optional, which is exactly why it is not styled by
            // position.
            Format::Shadow => 9,
            Format::Hosts => 2,
            Format::Services => 3,
            Format::Protocols => 3,
            Format::Crontab => 6,
        }
    }
}

/// The format a file is, by its name, or `None`.
///
/// **By name, and only for these standards.** That is a list, which is the thing this
/// codebase is otherwise careful about — the difference is that every entry here is a
/// documented database (`fstab(5)`, `passwd(5)`, …) and not one machine's sample. The
/// trailing-dash forms are the backups the tools themselves write beside the original.
pub fn detect_format(name: &Path) -> Option<Format> {
    let file = name.file_name()?.to_str()?.to_ascii_lowercase();
    Some(match file.as_str() {
        "fstab" => Format::Fstab,
        "passwd" | "passwd-" | "passwd.edit" => Format::Passwd,
        "shadow" | "shadow-" => Format::Shadow,
        "group" | "group-" => Format::Group,
        "hosts" => Format::Hosts,
        "services" => Format::Services,
        "protocols" => Format::Protocols,
        "crontab" => Format::Crontab,
        _ => return None,
    })
}

/// A style grid for `src`, one cell per character, in the shape [`crate::syntax`]'s
/// windowed refresh installs.
///
/// The whole of the rule is here: comments in the comment role, field 0 strong,
/// separators faint.
pub fn grid(src: &str, fmt: Format) -> Vec<Vec<Style>> {
    let faint = Style::of(Role::Faint);
    let strong = Style::of(Role::Strong);
    // The same role a `# ...` in code wears, so a theme colours both.
    let comment = Style::of(Role::Comment);
    let mut out = Vec::with_capacity(src.lines().count().max(1));
    for line in src.split('\n') {
        let chars: Vec<char> = line.chars().collect();
        let mut row = vec![Style::new(); chars.len()];
        // Comments run to end of line; the field walk must stop there.
        let body = if fmt.has_comments() {
            match chars.iter().position(|c| *c == '#') {
                Some(at) => {
                    for cell in &mut row[at..] {
                        *cell = comment.clone();
                    }
                    at
                }
                None => row.len(),
            }
        } else {
            row.len()
        };
        // Field 0 starts at the first non-blank; a line that is only a comment has none.
        let start = chars.iter().take(body).position(|c| !c.is_whitespace());
        let Some(start) = start else {
            out.push(row);
            continue;
        };
        let mut field = 0usize;
        let mut i = start;
        while i < body {
            match fmt.sep() {
                Sep::Whitespace => {
                    if chars[i].is_whitespace() {
                        row[i] = faint.clone();
                        i += 1;
                        continue;
                    }
                    let from = i;
                    while i < body && !chars[i].is_whitespace() {
                        i += 1;
                    }
                    if field == 0 {
                        for cell in &mut row[from..i] {
                            *cell = strong.clone();
                        }
                    }
                    field += 1;
                }
                Sep::Colon => {
                    if chars[i] == ':' {
                        row[i] = faint.clone();
                        i += 1;
                        field += 1;
                        continue;
                    }
                    let from = i;
                    while i < body && chars[i] != ':' {
                        i += 1;
                    }
                    if field == 0 {
                        for cell in &mut row[from..i] {
                            *cell = strong.clone();
                        }
                    }
                }
            }
        }
        out.push(row);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn styles(src: &str, fmt: Format) -> Vec<Vec<Style>> {
        grid(src, fmt)
    }

    fn at(row: &[Style], i: usize) -> Style {
        row[i].clone()
    }

    /// `fstab(5)`: six fields; the device is strong, the separator faint, the options
    /// plain — and the `=` inside an option is a character of that field, not a
    /// key/value split (this is the line that used to read as a shell assignment).
    #[test]
    fn fstab_is_six_whitespace_fields() {
        let line = "UUID=41A6-A18F   /boot/efi   vfat   umask=0077   0   1";
        let r = styles(line, Format::Fstab);
        let strong = Style::of(Role::Strong);
        let faint = Style::of(Role::Faint);
        // The strong run is exactly the device field — counted, not hand-indexed.
        let run = r[0].iter().take_while(|s| **s == strong).count();
        assert_eq!(
            run,
            "UUID=41A6-A18F".chars().count(),
            "the device is the strong run"
        );
        assert_eq!(at(&r[0], run), faint, "the separator after it is faint");
        // The mountpoint is plain, and the `=` inside `umask=0077` did not split its
        // field into a key and a value.
        let mount = line.find("/boot/efi").expect("fixture");
        assert_eq!(at(&r[0], mount), Style::new(), "a mountpoint is plain");
        let umask = line.find("umask=0077").expect("fixture");
        assert_eq!(at(&r[0], umask), Style::new(), "an option is one field");
        // Every separator is faint, and there are five between six fields.
        let plain = Style::new();
        let runs = r[0]
            .windows(2)
            .filter(|w| w[0] == faint && w[1] != faint)
            .count();
        assert_eq!(runs, 5, "five separators between six fields");
        assert!(r[0].contains(&plain) || r[0].is_empty());
    }

    /// `passwd(5)`: seven colon fields, the name strong, the colons faint.
    #[test]
    fn passwd_is_seven_colon_fields() {
        let line = "root:x:0:0:root:/root:/bin/bash";
        let r = styles(line, Format::Passwd);
        assert_eq!(at(&r[0], 0), Style::of(Role::Strong), "the login");
        assert_eq!(at(&r[0], 4), Style::of(Role::Faint), "the first colon");
        assert_eq!(at(&r[0], 5), Style::new(), "the password field");
    }

    /// `hosts(5)`: the address is strong, the names plain.
    #[test]
    fn hosts_leads_with_the_address() {
        let r = styles("127.0.0.1\tlocalhost localhost.localdomain", Format::Hosts);
        assert_eq!(at(&r[0], 0), Style::of(Role::Strong));
        assert_eq!(at(&r[0], 9), Style::of(Role::Faint), "the tab");
        assert_eq!(at(&r[0], 10), Style::new(), "a name");
    }

    /// A comment wears the COMMENT role to the end of the line — the same role a `# …` in
    /// code wears, so a theme that colours comments colours both — and a comments-only line
    /// has no strong column at all.
    #[test]
    fn a_comment_runs_to_the_end_of_the_line() {
        let comment = Style::of(Role::Comment);
        let r = styles(
            "# /etc/fstab: static file system information\n",
            Format::Fstab,
        );
        assert!(r[0].iter().all(|style| *style == comment));
        // The colon databases have no comment syntax, so `#` there is content.
        let r = styles("#x:0:0:root:/root:/bin/sh", Format::Passwd);
        assert_eq!(
            at(&r[0], 0),
            Style::of(Role::Strong),
            "a `#` login, not a comment"
        );
        assert_eq!(
            at(&r[0], 2),
            Style::of(Role::Faint),
            "and its colon still separates"
        );
    }

    /// The names, and the negatives: a `.bak` beside one of them is not the format.
    #[test]
    fn the_formats_are_recognised_by_name() {
        for (name, want) in [
            ("fstab", Format::Fstab),
            ("/etc/fstab", Format::Fstab),
            ("passwd", Format::Passwd),
            ("/etc/passwd-", Format::Passwd),
            ("shadow", Format::Shadow),
            ("/etc/shadow-", Format::Shadow),
            ("group", Format::Group),
            ("hosts", Format::Hosts),
            ("/etc/services", Format::Services),
            ("protocols", Format::Protocols),
            ("crontab", Format::Crontab),
        ] {
            assert_eq!(detect_format(Path::new(name)), Some(want), "{name}");
        }
        // A backup or a document beside one of them is not the format.
        for name in [
            "fstab.bak",
            "hosts.txt",
            "my-crontab",
            "fstab.20260918",
            "gshadow-",
        ] {
            assert_eq!(detect_format(Path::new(name)), None, "{name}");
        }
    }

    /// The grid is one row per line and one cell per character — the shape the windowed
    /// refresh installs, so `style_at` needs nothing new.
    #[test]
    fn the_grid_is_cells_for_every_character() {
        let g = grid("a b\ncd\n", Format::Fstab);
        assert_eq!(
            g.len(),
            3,
            "two lines and the empty piece after the final newline"
        );
        assert_eq!(g[0].len(), 3);
        assert_eq!(g[1].len(), 2);
        assert_eq!(g[2].len(), 0);
    }
}
