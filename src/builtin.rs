//! Builtins: `.wasm` scripts inside the binary, unpacked on first use into
//! `scripts/built-in/` and loaded from there like any hand-installed plugin.

use std::io::Write;
use std::path::{Path, PathBuf};

/// A script that ships in the binary.
pub struct Builtin {
    /// Its name under `scripts/built-in/`.
    pub name: &'static str,
    /// The bytes, embedded from the example crate's build.
    pub wasm: &'static [u8],
    /// The capabilities it ships with: a builtin comes from this binary, so what
    /// it needs is decided here, not demanded of the reader's config.
    pub caps: &'static [&'static str],
}

/// rano's manual: `examples/plugin-manual`, answering `provider_open("rano:manual")`
/// with `docs/*.md` as one document.
pub const MANUAL: Builtin = Builtin {
    name: "manual.wasm",
    wasm: include_bytes!("../examples/plugin-manual/plugin.wasm"),
    caps: &[],
};

/// The shipped PDF reader: `rano paper.pdf` opens a page with no config.
pub const PDF: Builtin = Builtin {
    name: "pdf.wasm",
    wasm: include_bytes!("../examples/plugin-pdf/plugin.wasm"),
    caps: &["convert-page"],
};

/// Everything that ships, loaded at startup so one can claim a file nobody configured.
pub const ALL: &[&Builtin] = &[&MANUAL, &PDF];

/// The marker recording what was unpacked and from which release.
pub const MARKER: &str = ".rano-version";

/// The release the shipped builtins come from.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// `$XDG_CONFIG_HOME/rano/scripts`, beside the config file.
pub fn scripts_dir() -> PathBuf {
    let config = crate::config::config_path();
    match config.parent() {
        Some(dir) => dir.join("scripts"),
        None => PathBuf::from("scripts"),
    }
}

/// Where the builtins are unpacked: `scripts/built-in/`.
pub fn builtin_dir(scripts: &Path) -> PathBuf {
    scripts.join("built-in")
}

/// Unpack `b` under `scripts/built-in/`, never over an existing file; `Ok(path)`
/// is where it is, written or found.
pub fn unpack(scripts: &Path, b: &Builtin) -> Result<PathBuf, String> {
    let dir = builtin_dir(scripts);
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let at = dir.join(b.name);
    // `create_new` is the "never overwrite" rule and the race with another rano at
    // once: whoever creates the file owns it.
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&at)
    {
        Ok(mut f) => {
            f.write_all(b.wasm)
                .map_err(|e| format!("{}: {e}", at.display()))?;
            record(scripts, b);
            Ok(at)
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(at),
        Err(e) => Err(format!("{}: {e}", at.display())),
    }
}

/// The release the marker says `b.name` was unpacked from, or `None` when rano did
/// not put the file there.
pub fn recorded(scripts: &Path, b: &Builtin) -> Option<(String, Option<u64>)> {
    let text = std::fs::read_to_string(builtin_dir(scripts).join(MARKER)).ok()?;
    text.lines().find_map(|line| {
        let mut parts = line.split(' ');
        let name = parts.next()?;
        let version = parts.next()?.trim().to_string();
        let sum = parts.next().and_then(|s| s.trim().parse::<u64>().ok());
        (name == b.name).then_some((version, sum))
    })
}

/// The note an upgrade owes the reader: the shipped copy is newer and the unpacked
/// file was left alone. `None` when rano never wrote it, the releases agree, or the
/// binary is older.
pub fn newer_note(scripts: &Path, b: &Builtin) -> Option<String> {
    let (was, was_sum) = recorded(scripts, b)?;
    let newer = crate::update::is_newer(&was, VERSION);
    // A rebuild at the same version is a different plugin: the marker records the
    // bytes, so a stale unpacked copy is still caught.
    let changed = was_sum.is_some_and(|sum| sum != sum_of(b.wasm));
    (newer || changed).then(|| {
        let why = if newer {
            format!("is newer ({VERSION}, from {was})")
        } else {
            "has changed since it was unpacked".to_string()
        };
        format!(
            "the shipped {} {why}; the one in {} was left alone. Delete it to take the shipped one.",
            b.name,
            builtin_dir(scripts).display()
        )
    })
}

/// A cheap fingerprint of the bytes (FNV-1a), enough to tell builds apart.
fn sum_of(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// Add or replace `name`'s line in the marker; a marker that cannot be written is
/// not a failure to unpack — only the upgrade note is lost.
fn record(scripts: &Path, b: &Builtin) {
    let name = b.name;
    let sum = sum_of(b.wasm);
    let at = builtin_dir(scripts).join(MARKER);
    let mut lines: Vec<String> = std::fs::read_to_string(&at)
        .map(|t| {
            t.lines()
                .filter(|l| l.split_once(' ').map(|(n, _)| n) != Some(name))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    lines.push(format!("{name} {VERSION} {sum}"));
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

    #[test]
    fn unpacking_records_the_release_and_never_overwrites() {
        let d = temp("unpack");
        let at = unpack(&d, &MANUAL).unwrap();
        assert_eq!(std::fs::read(&at).unwrap(), MANUAL.wasm);
        let (version, sum) = recorded(&d, &MANUAL).expect("recorded");
        assert_eq!(version, VERSION);
        assert_eq!(sum, Some(sum_of(MANUAL.wasm)), "the bytes are recorded too");
        assert_eq!(
            newer_note(&d, &MANUAL),
            None,
            "the release and the bytes agree"
        );
        // A rebuild at the same version is still a different plugin.
        std::fs::write(
            builtin_dir(&d).join(MARKER),
            format!("manual.wasm {VERSION} 1\n"),
        )
        .unwrap();
        let note = newer_note(&d, &MANUAL).expect("the bytes changed");
        assert!(note.contains("changed"), "{note}");
        // An older unpacking: the note names the shipped one as newer.
        std::fs::write(builtin_dir(&d).join(MARKER), "manual.wasm 0.0.1 0\n").unwrap();
        let note = newer_note(&d, &MANUAL).expect("the shipped one is newer");
        assert!(note.contains("newer") && note.contains("0.0.1"), "{note}");
        std::fs::write(&at, b"mine").unwrap();
        assert_eq!(unpack(&d, &MANUAL).unwrap(), at);
        assert_eq!(std::fs::read(&at).unwrap(), b"mine", "never overwritten");
        let _ = std::fs::remove_dir_all(&d);
    }
}
