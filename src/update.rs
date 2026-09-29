//! Version check, and self-update. Off by default; `autoupdate` in the config
//! or `RANO_AUTOUPDATE` in the environment turns it on.
//!
//! # What it does, and what it refuses to do
//!
//! **Checking is free; installing is not.** A check reads `/releases/latest`
//! from GitHub and compares tag names. An *install* replaces the running
//! executable, which means the update notice has to have reached a person and
//! they have said yes — see [`Update::install`], which is only called from an
//! accepted prompt.
//!
//! What it will not do, deliberately:
//!
//! - **No download unless the version is newer.** The tag is compared first, so
//!   a configured-but-current install makes one small request per launch and
//!   writes nothing. That is also what makes it safe to run on every start.
//! - **No non-HTTPS URL.** The download host is a constant in this file, not
//!   something read from a response. A redirect to plain `http` is refused
//!   rather than followed.
//! - **No writing to a path we cannot replace.** The running binary is replaced
//!   by rename, so a failure leaves the old one in place; if a temp file cannot
//!   be created next to it (a read-only install prefix, a package-managed
//!   location) the update is refused with a message rather than half-done.
//! - **No proxy to a shell.** Nothing here runs a command; `curl` is not
//!   invoked. If TLS cannot be had, the update does not happen.
//!
//! # Where the version comes from
//!
//! `CARGO_PKG_VERSION` — the version compiled into this binary — and the tag of
//! the latest release. Tags are compared as `MAJOR.MINOR.PATCH`, numerically, so
//! `0.10.0` is newer than `0.9.0`. A tag that is not that shape (a pre-release
//! suffix, a branch named like a version) is not newer and does not update.
//!
//! # The gate
//!
//! **On by default, as in normal software**, and turned off in one place:
//! `autoupdate = false` in the config, or `RANO_AUTOUPDATE=0` in the
//! environment. Nothing else in the program consults the setting, so a reader
//! can satisfy themselves that a disabled check makes no request at all —
//! asserted in `tests/update_check.rs` with a stub that records being called.

use std::path::{Path, PathBuf};

/// The one place the download host is written down. Not from a response.
const API: &str = "https://api.github.com/repos/deadtrickster/rano/releases/latest";
const REPO: &str = "deadtrickster/rano";

/// This binary's version, as compiled.
pub const CURRENT: &str = env!("CARGO_PKG_VERSION");

/// Whether an update is available.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Update {
    /// The latest release's version, without the leading `v`.
    pub latest: String,
    /// The version this binary was built as.
    pub current: String,
    /// The asset for this platform, if the release has one.
    pub asset: Option<String>,
}

impl Update {
    /// The URL to download `asset` from. Built here, so the host is the constant
    /// above and never anything a response said.
    pub fn asset_url(&self, asset: &str) -> String {
        format!(
            "https://github.com/{REPO}/releases/download/v{}/{asset}",
            self.latest
        )
    }

    /// One line for the status bar.
    pub fn message(&self) -> String {
        match &self.asset {
            Some(_) => format!(
                "rano {} available (you have {}): M-V to update",
                self.latest, self.current
            ),
            None => format!(
                "rano {} available (you have {}), but not for this platform",
                self.latest, self.current
            ),
        }
    }
}

/// Is `enabled`? **On by default**, as update checks are in normal software: it
/// runs unless something turns it off. The config value wins if set, otherwise
/// the environment.
///
/// The env value is not "anything non-empty": `RANO_AUTOUPDATE=0` is how someone
/// turns it off for one command, and reading that as `true` would make the
/// switch a lie.
///
/// A plain function rather than a field cache so a test can drive both branches
/// without a global.
pub fn enabled(config_value: Option<bool>, env_value: Option<&str>) -> bool {
    if let Some(b) = config_value {
        return b;
    }
    match env_value {
        // Case-insensitive: an environment variable is not typed under
        // supervision, and `RANO_AUTOUPDATE=OFF` meaning "on" would be the kind
        // of switch that makes people distrust switches.
        Some(v) => !matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "0" | "false" | "no" | "off"
        ),
        None => true,
    }
}

/// Every URL this module fetches must be HTTPS. **Checked without spawning
/// anything**, so it is testable in-process.
///
/// This is a function rather than an inline test in `Curl::get` for a reason
/// found the hard way: the first version asserted it by calling the real
/// `Curl::get`, which spawns a subprocess — and a test that spawns a process can
/// HANG, which is exactly what happened. The decision is pure; the process is
/// not, and only the decision belongs in a test.
pub fn require_https(url: &str) -> Result<(), String> {
    if url.starts_with("https://") {
        return Ok(());
    }
    Err(format!(
        "refusing a non-https URL: {url} (downloads here are TLS-only)"
    ))
}
///
/// Strict on purpose: `0.2.0-rc1` and `2026.09.30` are not newer than anything,
/// so a tag this shape never triggers an update. Silently ordering them would
/// mean downloading a binary because of a lexicographic accident.
pub fn parse_version(s: &str) -> Option<(u64, u64, u64)> {
    let s = s.trim().strip_prefix('v').unwrap_or(s.trim());
    let mut parts = s.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next()?.parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some((major, minor, patch))
}

/// Whether `latest` is newer than `current`. `false` when either is unparseable.
pub fn is_newer(current: &str, latest: &str) -> bool {
    match (parse_version(current), parse_version(latest)) {
        (Some(c), Some(l)) => l > c,
        _ => false,
    }
}

/// The asset name for this platform, or `None` for one we do not build for.
///
/// The same table `install.sh` uses — and `scripts/check-dist-names.sh` keeps
/// both in step with the release workflow, so a rename on one side fails in CI
/// rather than silently offering an update that cannot be downloaded.
pub fn asset_name(os: &str, arch: &str) -> Option<&'static str> {
    Some(match (os, arch) {
        ("linux", "x86_64") => "rano-x86_64-unknown-linux-gnu.tar.gz",
        ("linux", "aarch64") => "rano-aarch64-unknown-linux-gnu.tar.gz",
        ("macos", "aarch64") => "rano-aarch64-apple-darwin.tar.gz",
        ("macos", "x86_64") => "rano-x86_64-apple-darwin.tar.gz",
        _ => return None,
    })
}

/// This build's asset name.
pub fn this_asset() -> Option<&'static str> {
    asset_name(std::env::consts::OS, std::env::consts::ARCH)
}

/// Ask GitHub for the latest release and decide whether it is worth offering.
///
/// Returns `None` when disabled, when the request fails, or when this binary is
/// already the latest. **Never returns an error a caller has to handle**: an
/// unavailable network must not be an editor's problem.
///
/// `Get` is a one-method trait so the parsing and the decision can be tested
/// with no network at all — the HTTP client is the one part that cannot be
/// meaningfully unit-tested, and it is kept tiny for that reason.
pub fn check_with(enabled: bool, get: &dyn Get) -> Option<Update> {
    if !enabled {
        return None;
    }
    let body = get.get(API).ok()?;
    let tag = tag_name(&body)?;
    let latest = tag.trim_start_matches('v').to_string();
    if !is_newer(CURRENT, &latest) {
        return None;
    }
    Some(Update {
        latest,
        current: CURRENT.to_string(),
        asset: this_asset().map(str::to_string),
    })
}

/// The `tag_name` field of a release JSON body, without a JSON dependency.
///
/// The one thing this needs from the payload is a flat string field, and pulling
/// `serde_json` into the startup path for it would be the tail wagging the dog.
/// It is strict: a body with no `"tag_name"`, or an empty value, yields `None`
/// and therefore no update.
pub fn tag_name(json: &str) -> Option<String> {
    let at = json.find("\"tag_name\"")?;
    let rest = &json[at + "\"tag_name\"".len()..];
    let rest = rest.trim_start().strip_prefix(':')?.trim_start();
    let rest = rest.strip_prefix('"')?;
    let end = rest.find('"')?;
    let value = &rest[..end];
    (!value.is_empty()).then(|| value.to_string())
}

/// The one thing [`check_with`] needs from the network.
pub trait Get {
    fn get(&self, url: &str) -> Result<String, String>;
}

/// Fetch a URL with `curl`, if it is present.
///
/// `curl` in a subprocess rather than a TLS stack: an HTTP client is a few
/// hundred kilobytes of dependencies and a certificate store to keep current,
/// for one request per launch.
///
/// **Certificate verification is not negotiable, and `-q` is why.** curl honours
/// `~/.curlrc`, so a single `insecure` line in a user's config file silently
/// turns verification off for every curl they run — measured, not assumed: with
/// `insecure` in a `.curlrc`, `curl` against a self-signed host exits 0, and with
/// `-q` as the first argument it exits 60. So `-q` is first (it stops curlrc
/// being read at all), `--proto`/`--proto-redir` keep both the request and any
/// redirect on HTTPS, and `--tlsv1.2` sets a floor.
///
/// The cost of `-q`: a proxy configured ONLY in `~/.curlrc` is ignored. The
/// standard `HTTPS_PROXY`/`https_proxy` environment variables are still honoured,
/// because those are not read from a config file. Downloading and executing a
/// binary is not a place to inherit someone's debugging shortcuts.
pub struct Curl;

/// The curl arguments every request here shares. `-q` MUST be first.
fn curl_args() -> Vec<String> {
    vec![
        "-q".to_string(),
        "-fsSL".to_string(),
        "--proto".to_string(),
        "=https".to_string(),
        "--proto-redir".to_string(),
        "=https".to_string(),
        "--tlsv1.2".to_string(),
        "-A".to_string(),
        format!("rano/{CURRENT}"),
    ]
}

impl Get for Curl {
    fn get(&self, url: &str) -> Result<String, String> {
        // Belt and braces with curling's `--proto`: enforced in code as well as
        // by flags, so a caller cannot reach the network over plain http even if
        // the flags were got wrong.
        require_https(url)?;
        let mut args = curl_args();
        args.extend([
            "--max-time".to_string(),
            "10".to_string(),
            "-H".to_string(),
            "Accept: application/vnd.github+json".to_string(),
            url.to_string(),
        ]);
        let out = std::process::Command::new("curl")
            .args(&args)
            .output()
            .map_err(|e| format!("curl: {e}"))?;
        if !out.status.success() {
            return Err(format!("curl exited {}", out.status));
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }
}

/// Fetch a URL's bytes, for the asset itself rather than the JSON.
impl Curl {
    pub fn get_bytes(&self, url: &str) -> Result<Vec<u8>, String> {
        require_https(url)?;
        let mut args = curl_args();
        args.extend(["--max-time".to_string(), "120".to_string(), url.to_string()]);
        let out = std::process::Command::new("curl")
            .args(&args)
            .output()
            .map_err(|e| format!("curl: {e}"))?;
        if !out.status.success() {
            return Err(format!("curl exited {}", out.status));
        }
        Ok(out.stdout)
    }
}

/// Extract `rano` from a `.tar.gz` produced by `scripts/make-dist.sh`.
///
/// A minimal reader for a single-member gzip tar, rather than a `tar` + `flate2`
/// dependency tree. It refuses anything but the one shape we publish: a member
/// named `rano`, regular and non-empty. A tar with two members, a symlink, or a
/// path with a directory component is rejected rather than guessed at.
pub fn extract_binary(tar_gz: &[u8]) -> Result<Vec<u8>, String> {
    let raw = gunzip(tar_gz)?;
    let mut at = 0usize;
    while at + 512 <= raw.len() {
        let header = &raw[at..at + 512];
        // Two zero blocks end the archive.
        if header.iter().all(|b| *b == 0) {
            break;
        }
        let name_end = header.iter().position(|b| *b == 0).unwrap_or(100);
        let name = String::from_utf8_lossy(&header[..name_end.min(100)]).into_owned();
        let size = octal(&header[124..136]).ok_or("tar: bad size field")? as usize;
        let typeflag = header[156];
        let body = at + 512;
        if body + size > raw.len() {
            return Err("tar: truncated member".to_string());
        }
        if name == "rano" {
            if typeflag != b'0' && typeflag != 0 {
                return Err(format!(
                    "tar: `rano` is not a regular file (type {:?})",
                    typeflag as char
                ));
            }
            let bytes = raw[body..body + size].to_vec();
            if bytes.is_empty() {
                return Err("tar: `rano` is empty".to_string());
            }
            return Ok(bytes);
        }
        // Advance to the next header, padded to a 512-byte boundary.
        at = body + size.div_ceil(512) * 512;
    }
    Err("tar: no `rano` member".to_string())
}

fn octal(field: &[u8]) -> Option<u64> {
    let s = String::from_utf8_lossy(field);
    let s = s.trim_matches(|c: char| c == '\0' || c == ' ');
    if s.is_empty() {
        return Some(0);
    }
    u64::from_str_radix(s, 8).ok()
}

/// Gunzip. **A library, not a subprocess, and that is a fix rather than a taste.**
///
/// The first version shelled out to `gzip -d` and pumped its pipes by hand, and
/// it deadlocked twice, both times for reasons a small test fixture could not
/// show:
///
/// 1. Writing to the child's stdin without closing it. `gzip` reads until EOF
///    before it writes anything, so it waited for input that never came while
///    the caller waited for output. A test binary sat on it for twelve minutes.
/// 2. Writing the whole input before reading any output. The decompressed tar of
///    this repo's 3.4 MB asset is ~20 MB, and the stdout pipe holds 64 KB: once
///    it filled, `gzip` blocked on the write, stopped reading stdin, and the
///    caller blocked on ITS write. The fixture that missed this was a few hundred
///    bytes — small enough to fit in the pipe buffer, so the deadlock could not
///    happen in the test at all.
///
/// Both are properties of doing process plumbing by hand, and neither can happen
/// in a decompressor called as a function. `flate2` with the pure-Rust backend
/// also removes the "is gzip installed" question from an operation that runs on
/// someone else's machine.
fn gunzip(data: &[u8]) -> Result<Vec<u8>, String> {
    use std::io::Read;
    let mut out = Vec::new();
    flate2::read::GzDecoder::new(data)
        .read_to_end(&mut out)
        .map_err(|e| format!("gzip: {e}"))?;
    Ok(out)
}

/// Replace the running executable with `binary`.
///
/// Write to a temp file next to the target, then rename over it. The rename is
/// what makes this safe: either the new binary is in place or the old one is,
/// and a failure at any point — no room, no permission, a truncated download —
/// leaves the old one working.
pub fn replace_running(binary: &[u8], exe: &Path) -> Result<(), String> {
    let dir = exe
        .parent()
        .ok_or_else(|| format!("{} has no parent directory", exe.display()))?;
    let tmp: PathBuf = dir.join(format!(".rano-new-{}", std::process::id()));
    std::fs::write(&tmp, binary).map_err(|e| {
        format!(
            "cannot write {}: {e} (is rano installed somewhere read-only?)",
            tmp.display()
        )
    })?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))
        .map_err(|e| format!("cannot make {} executable: {e}", tmp.display()))?;
    std::fs::rename(&tmp, exe).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("cannot replace {}: {e}", exe.display())
    })
}

/// Where this executable is.
pub fn current_exe() -> Result<PathBuf, String> {
    std::env::current_exe().map_err(|e| format!("cannot find my own path: {e}"))
}

/// Download and install `u`. Called only after a person said yes — nothing here
/// asks, and nothing here is on a timer.
///
/// The URL comes from [`Update::asset_url`], so the download host is written
/// down in exactly one place and the URL-building is unit-tested.
pub fn install(u: &Update) -> Result<(), String> {
    let asset = u
        .asset
        .as_deref()
        .ok_or_else(|| format!("no build for this platform in release {}", u.latest))?;
    let bytes = Curl.get_bytes(&u.asset_url(asset))?;
    let binary = extract_binary(&bytes)?;
    replace_running(&binary, &current_exe()?)
}
