//! A directory, listed as text: the dired half of `open-dir`. `name/` marks a
//! directory, and rows sort directories first, then files, case-insensitively.

use std::path::{Path, PathBuf};

/// One entry of a listing, as the row that shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// The name alone — no path, and no marker.
    pub name: String,
    /// A directory (after following a symlink), which decides the trailing `/`.
    pub dir: bool,
}

impl Entry {
    /// The row as the buffer holds it: `name/` for a directory, `name` otherwise.
    pub fn row(&self) -> String {
        if self.dir {
            format!("{}/", self.name)
        } else {
            self.name.clone()
        }
    }
}

/// The entries of `dir`, in listing order; an unreadable directory is an error,
/// because an empty listing would read as "this folder is empty".
pub fn entries(dir: &Path) -> Result<Vec<Entry>, String> {
    let read = std::fs::read_dir(dir).map_err(|e| e.to_string())?;
    let mut out: Vec<Entry> = Vec::new();
    for e in read.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        // `.` and `..` never appear in `read_dir` output, but a name that is exactly
        // those could not be entered anyway.
        let dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false) || e.path().is_dir();
        out.push(Entry { name, dir });
    }
    out.sort_by(|a, b| {
        // Case-sensitive last so the order is total: two names differing only in
        // case would otherwise compare equal.
        b.dir
            .cmp(&a.dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            .then_with(|| a.name.cmp(&b.name))
    });
    Ok(out)
}

/// The rows of the buffer that shows `dir`.
pub fn rows(dir: &Path) -> Result<Vec<String>, String> {
    Ok(entries(dir)?.iter().map(Entry::row).collect())
}

/// The path the row names under `dir`, or `None` when the row is empty or not an
/// entry. A directory row's trailing `/` is the marker, not part of the name.
pub fn path_at(dir: &Path, row: &str) -> Option<PathBuf> {
    let name = row.strip_suffix('/').unwrap_or(row);
    if name.is_empty() || name == "." || name == ".." || name.contains('/') {
        return None;
    }
    Some(dir.join(name))
}

/// The row whose entry is `name`, for keeping the cursor on the same entry across
/// a refresh; `None` when it is gone.
pub fn row_of(dir: &Path, name: &str) -> Option<usize> {
    let want = format!("{name}/");
    rows(dir).ok()?.iter().position(|r| r == &want || r == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Temp(PathBuf);
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn tree(tag: &str) -> Temp {
        // A tag per test: tests share the pid, and a name without one had them
        // deleting each other's tree in parallel runs.
        let d = std::env::temp_dir().join(format!("rano_dir_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("src/deep")).unwrap();
        std::fs::write(d.join("README.md"), "x").unwrap();
        std::fs::write(d.join("src/main.rs"), "x").unwrap();
        std::fs::write(d.join("zeta.txt"), "x").unwrap();
        Temp(d)
    }

    #[test]
    fn a_listing_puts_directories_first_and_marks_them() {
        let d = tree("list");
        let listed = rows(&d.0).unwrap();
        assert_eq!(listed, vec!["src/", "README.md", "zeta.txt"]);
        // Case-insensitive inside a group: a capital does not sort before a folder.
        std::fs::write(d.0.join("Apple.md"), "x").unwrap();
        let listed = rows(&d.0).unwrap();
        assert_eq!(
            listed,
            vec!["src/", "Apple.md", "README.md", "zeta.txt"],
            "directories first, then names case-insensitively"
        );
    }

    #[test]
    fn a_row_names_an_entry_and_the_marker_is_not_the_name() {
        let d = tree("row");
        assert_eq!(path_at(&d.0, "src/"), Some(d.0.join("src")));
        assert_eq!(path_at(&d.0, "README.md"), Some(d.0.join("README.md")));
        // A row that is not an entry names nothing.
        assert_eq!(path_at(&d.0, ""), None);
        assert_eq!(path_at(&d.0, ".."), None);
        assert_eq!(path_at(&d.0, "../etc/passwd"), None);
        // And a marker is a marker: `src/` is `src`, never `src//`.
        assert!(!path_at(&d.0, "src/").unwrap().ends_with("//"));
    }

    #[test]
    fn a_missing_or_unreadable_folder_is_an_error_not_an_empty_listing() {
        let d = tree("missing");
        let e = rows(&d.0.join("nope")).unwrap_err();
        assert!(!e.is_empty());
        // A file is not a folder.
        assert!(rows(&d.0.join("README.md")).is_err());
    }

    #[test]
    fn a_refresh_can_find_the_row_it_was_on() {
        let d = tree("refresh");
        assert_eq!(row_of(&d.0, "zeta.txt"), Some(2));
        assert_eq!(row_of(&d.0, "src"), Some(0));
        assert_eq!(row_of(&d.0, "gone.txt"), None);
        // A new file lands in its sorted place, and the old rows keep their names.
        std::fs::write(d.0.join("beta.txt"), "x").unwrap();
        // beta.txt sorts between the folder and README.md, so zeta moves down one.
        assert_eq!(row_of(&d.0, "beta.txt"), Some(1));
        assert_eq!(row_of(&d.0, "zeta.txt"), Some(3));
    }
}
