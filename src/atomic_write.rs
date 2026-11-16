//! One door for every byte rano writes over a file. The editor's save and its signal recovery
//! both come here, so the rule lives in exactly one place rather than in each caller.
//!
//! The contract:
//!
//! - **Whole or not at all**: a temp file beside the target, fsynced, then renamed over it.
//!   A crash mid-write cannot truncate what was there. The directory itself is not fsynced.
//! - **An existing file keeps its mode**: the rename must not change how it opens. A new file
//!   takes the process's default mode (`0666 & !umask`), as `File::create` makes it.
//! - **A symlink stays a link**: the bytes land on the file it names, never replacing the
//!   link with a regular file.
//! - **A writable file in an unwritable directory is written in place** (nano's own fallback):
//!   no temp file is possible there, and refusing would break a save that used to work.
//!   That one path is NOT atomic, which is why it is named here rather than hidden.
//! - **A failed write removes its temp file.**

use std::fs;
use std::path::{Path, PathBuf};

/// Write `bytes` over `path` under the module's contract; see the module doc.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;
    // A symlink stays a link: the bytes go through it to the file it names.
    let target = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let dir = match target.parent() {
        Some(d) if !d.as_os_str().is_empty() => d.to_path_buf(),
        _ => PathBuf::from("."),
    };
    let name = target
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "rano".to_string());
    let tmp = dir.join(format!(".{name}.{}.rano-tmp", std::process::id()));
    // The mode the file had: a rename must not change how it opens.
    let mode = fs::metadata(&target).ok().map(|m| m.permissions().mode());
    // A writable file in a directory we cannot write: no temp file is possible, so
    // write in place as nano does rather than refuse a save that used to work.
    let mut f = match fs::File::create(&tmp) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            return fs::write(&target, bytes);
        }
        Err(e) => return Err(e),
    };
    let mut write = || -> std::io::Result<()> {
        f.write_all(bytes)?;
        f.sync_all()?;
        if let Some(mode) = mode {
            fs::set_permissions(&tmp, fs::Permissions::from_mode(mode))?;
        }
        fs::rename(&tmp, &target)
    };
    let done = write();
    if done.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    done
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn tempdir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "rano_atomic_{}_{}_{:?}",
            tag,
            std::process::id(),
            std::time::Instant::now()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_save_is_whole_and_keeps_the_files_mode() {
        let dir = tempdir("mode");
        let file = dir.join("a.txt");
        fs::write(&file, "old").unwrap();
        fs::set_permissions(&file, fs::Permissions::from_mode(0o600)).unwrap();
        write_atomic(&file, b"new bytes").unwrap();
        assert_eq!(fs::read_to_string(&file).unwrap(), "new bytes");
        assert_eq!(
            fs::metadata(&file).unwrap().permissions().mode() & 0o777,
            0o600,
            "the rename kept the mode"
        );
        // A new file is created, and no temp is left behind either way.
        write_atomic(&dir.join("new.txt"), b"first").unwrap();
        assert_eq!(fs::read_to_string(dir.join("new.txt")).unwrap(), "first");
        let temps = fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains("rano-tmp"))
            .count();
        assert_eq!(temps, 0, "no temp files left");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_symlink_stays_a_link_and_the_bytes_reach_its_target() {
        let dir = tempdir("symlink");
        let target = dir.join("real.txt");
        let link = dir.join("link.txt");
        fs::write(&target, "old").unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();
        write_atomic(&link, b"through").unwrap();
        assert_eq!(fs::read_to_string(&target).unwrap(), "through");
        assert!(
            fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink(),
            "the link was not replaced by a regular file"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_failed_write_removes_the_temp_file() {
        let dir = tempdir("failed");
        // The target being a directory makes the rename fail after a good temp write.
        let target = dir.join("subdir");
        fs::create_dir(&target).unwrap();
        assert!(write_atomic(&target, b"bytes").is_err());
        let temps = fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains("rano-tmp"))
            .count();
        assert_eq!(temps, 0, "the temp is cleaned up on failure");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_unwritable_directory_falls_back_to_writing_in_place() {
        // Root ignores the permission bits this test depends on.
        if unsafe { libc::geteuid() } == 0 {
            return;
        }
        let dir = tempdir("fallback");
        let file = dir.join("a.txt");
        fs::write(&file, "old").unwrap();
        fs::set_permissions(&file, fs::Permissions::from_mode(0o644)).unwrap();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o555)).unwrap();
        let result = write_atomic(&file, b"in place");
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
        result.unwrap();
        assert_eq!(fs::read_to_string(&file).unwrap(), "in place");
        let _ = fs::remove_dir_all(&dir);
    }
}
