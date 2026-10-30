//! **Builtins: the scripts that ride in the binary and unpack into the reader's folder.**
//!
//! plans/S10-wasm-hooks.md §2.4: a builtin is *not* compiled into the editor's paths. It is a
//! `.wasm` inside the binary (`include_bytes!`), written out on first use to
//! `$XDG_CONFIG_HOME/rano/scripts/built-in/` (else `~/.config/rano/scripts/built-in/`), and
//! loaded from there like any plugin the reader installed by hand.
//!
//! - **Never overwritten.** Unpacking writes a file only when it is absent, so an edited or
//!   replaced builtin stays put. A marker (`scripts/built-in/.rano-version`) records what was
//!   unpacked and from which release, so an upgrade can say *"the shipped manual is newer; the
//!   one in your scripts folder was left alone"* instead of silently reverting somebody's work.
//! - **Editable means editable**: the file in that folder is the plugin that runs. Replace the
//!   `.wasm` with your own build and that is the builtin; delete it and the next use unpacks the
//!   shipped one again.
//! - **Unpacking is not running**: writing a file loads nothing. The one caller today is the
//!   `manual` command, which the reader asked for by name.

use std::io::Write;
use std::path::{Path, PathBuf};

/// A script that ships in the binary.
pub struct Builtin {
    /// Its name under `scripts/built-in/`.
    pub name: &'static str,
    /// The bytes, from the example crate's own build (`examples/plugin-manual/plugin.wasm`).
    pub wasm: &'static [u8],
    /// **The capabilities it ships with.** A builtin comes from this binary, so what it needs is
    /// known here rather than demanded of the reader's config: a PDF viewer that cannot convert a
    /// page is not a viewer, and asking for three environment variables to see a document is not
    /// an editor.
    pub caps: &'static [&'static str],
}

/// **rano's manual**: `examples/plugin-manual`, answering `provider_open("rano:manual")` with
/// `docs/*.md` as one document.
pub const MANUAL: Builtin = Builtin {
    name: "manual.wasm",
    wasm: include_bytes!("../examples/plugin-manual/plugin.wasm"),
    caps: &[],
};

/// **The PDF reader**, shipped: `rano paper.pdf` opens a page, and needs no environment at all.
pub const PDF: Builtin = Builtin {
    name: "pdf.wasm",
    wasm: include_bytes!("../examples/plugin-pdf/plugin.wasm"),
    caps: &["convert-page"],
};

/// Everything that ships, loaded at startup so it can claim a file nobody configured it for.
pub const ALL: &[&Builtin] = &[&MANUAL, &PDF];

/// The marker recording what was unpacked and from which release: one `name version` line each.
pub const MARKER: &str = ".rano-version";

/// The release the shipped builtins come from.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// **`$XDG_CONFIG_HOME/rano/scripts`**, else `~/.config/rano/scripts` — beside the config file,
/// so the reader's folder is where they already keep everything else.
pub fn scripts_dir() -> PathBuf {
    let config = crate::config::config_path();
    match config.parent() {
        Some(dir) => dir.join("scripts"),
        None => PathBuf::from("scripts"),
    }
}

/// Where the builtins are unpacked: `scripts/built-in/`. The rest of `scripts/` is the reader's.
pub fn builtin_dir(scripts: &Path) -> PathBuf {
    scripts.join("built-in")
}

/// **Unpack `b` under `scripts/built-in/`, never over an existing file**, and record the write in
/// the marker. `Ok(path)` is where it is, whether this call wrote it or found it there.
pub fn unpack(scripts: &Path, b: &Builtin) -> Result<PathBuf, String> {
    let dir = builtin_dir(scripts);
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let at = dir.join(b.name);
    // `create_new` is the "never overwrite" rule and the race with another rano at once: whoever
    // creates the file owns it, and the loser reads what the winner wrote.
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&at)
    {
        Ok(mut f) => {
            f.write_all(b.wasm)
                .map_err(|e| format!("{}: {e}", at.display()))?;
            record(scripts, b.name);
            Ok(at)
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(at),
        Err(e) => Err(format!("{}: {e}", at.display())),
    }
}

/// The release the marker says `b.name` was unpacked from, or `None` when rano did not put the
/// file there (a copy the reader made themselves, or a folder from before the marker).
pub fn recorded(scripts: &Path, b: &Builtin) -> Option<String> {
    let text = std::fs::read_to_string(builtin_dir(scripts).join(MARKER)).ok()?;
    text.lines().find_map(|line| {
        let (name, version) = line.split_once(' ')?;
        (name == b.name).then(|| version.trim().to_string())
    })
}

/// **The line an upgrade owes the reader**: the shipped copy is newer than the one that was
/// unpacked, and the file in the scripts folder was left alone. `None` when rano never wrote that
/// file — "the shipped one is newer" is not a claim rano can make about somebody else's build —
/// or when the releases agree (or the binary is older: a downgrade says nothing).
pub fn newer_note(scripts: &Path, b: &Builtin) -> Option<String> {
    let was = recorded(scripts, b)?;
    crate::update::is_newer(&was, VERSION).then(|| {
        format!(
            "the shipped {} is newer ({VERSION}); the one in {} was left alone (from {was})",
            b.name,
            builtin_dir(scripts).display()
        )
    })
}

/// Add or replace `name`'s line in the marker. A marker that cannot be written is not a failure
/// to unpack — the plugin is on disk and works; only the upgrade note is lost.
fn record(scripts: &Path, name: &str) {
    let at = builtin_dir(scripts).join(MARKER);
    let mut lines: Vec<String> = std::fs::read_to_string(&at)
        .map(|t| {
            t.lines()
                .filter(|l| l.split_once(' ').map(|(n, _)| n) != Some(name))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    lines.push(format!("{name} {VERSION}"));
    let _ = std::fs::write(&at, lines.join("\n") + "\n");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("rano_builtin_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// **The marker says what was unpacked, and from which release** — so an upgrade can tell the
    /// reader the shipped copy is newer instead of reverting their file, and says nothing about a
    /// file rano did not write.
    #[test]
    fn unpacking_records_the_release_and_never_overwrites() {
        let d = temp("unpack");
        let at = unpack(&d, &MANUAL).unwrap();
        assert_eq!(std::fs::read(&at).unwrap(), MANUAL.wasm);
        assert_eq!(recorded(&d, &MANUAL).as_deref(), Some(VERSION));
        assert_eq!(newer_note(&d, &MANUAL), None, "the releases agree");
        // An older unpacking, and the same file on disk: the note names both.
        std::fs::write(builtin_dir(&d).join(MARKER), "manual.wasm 0.0.1\n").unwrap();
        let note = newer_note(&d, &MANUAL).expect("the shipped one is newer");
        assert!(note.contains("newer") && note.contains("0.0.1"), "{note}");
        std::fs::write(&at, b"mine").unwrap();
        assert_eq!(unpack(&d, &MANUAL).unwrap(), at);
        assert_eq!(std::fs::read(&at).unwrap(), b"mine", "never overwritten");
        let _ = std::fs::remove_dir_all(&d);
    }
}
