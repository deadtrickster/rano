//! Version check, and self-update. Off by default; `autoupdate` in the config
//! or `RANO_AUTOUPDATE` in the environment turns it on.
//!
//! **Checking is free; installing is not** — an install replaces the running
//! executable, so it only happens from an accepted prompt ([`Update::install`]).
//! What it will not do: no download unless the version is newer; no non-HTTPS
//! URL (the host is a constant here, a redirect to plain http is refused); no
//! writing to a path that cannot be replaced (the binary is replaced by rename,
//! so a failure leaves the old one in place); no proxy to a shell. Tags are
//! compared as `MAJOR.MINOR.PATCH` numerically; any other shape is not newer.
//! The gate is on by default and turned off in one place (`autoupdate = false`
//! or `RANO_AUTOUPDATE=0`), asserted in `tests/update_check.rs`.

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
    /// The URL to download `asset` from — the host is the constant above,
    /// never anything a response said.
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

/// Is `enabled`? **On by default**: the config value wins if set, otherwise the
/// environment. `RANO_AUTOUPDATE=0` is how someone turns it off for one command,
/// and reading that as `true` would make the switch a lie. A plain function so
/// a test can drive both branches without a global.
pub fn enabled(config_value: Option<bool>, env_value: Option<&str>) -> bool {
    if let Some(b) = config_value {
        return b;
    }
    match env_value {
        // Case-insensitive: `RANO_AUTOUPDATE=OFF` meaning "on" would be the
        // kind of switch that makes people distrust switches.
        Some(v) => !matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "0" | "false" | "no" | "off"
        ),
        None => true,
    }
}

/// Every URL this module fetches must be HTTPS, **checked without spawning
/// anything** so it is testable in-process (a test that spawns a process can
/// HANG; the decision is pure, the process is not).
pub fn require_https(url: &str) -> Result<(), String> {
    if url.starts_with("https://") {
        return Ok(());
    }
    Err(format!(
        "refusing a non-https URL: {url} (downloads here are TLS-only)"
    ))
}
/// Parse `MAJOR.MINOR.PATCH`. Strict on purpose: `0.2.0-rc1` and `2026.09.30`
/// are not newer than anything, so a tag this shape never triggers an update.
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
/// The same table `install.sh` uses — `scripts/check-dist-names.sh` keeps both
/// in step with the release workflow.
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
/// Returns `None` when disabled, when the request fails, or when already the
/// latest — an unavailable network must not be an editor's problem. `Get` is a
/// one-method trait so the parsing and the decision test with no network.
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

/// The `tag_name` field of a release JSON body, without a JSON dependency —
/// strict: no field or an empty value yields `None`, and no update.
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

/// Fetch a URL with `curl`, if it is present — a subprocess rather than a TLS
/// stack, which is a few hundred kilobytes of dependencies for one request per
/// launch.
///
/// **`-q` is first and it is why certificate verification is not negotiable**:
/// curl honours `~/.curlrc`, so one `insecure` line silently turns verification
/// off — with `-q` first, curlrc is not read at all. `--proto`/`--proto-redir`
/// keep request and redirects on HTTPS. The cost: a proxy configured only in
/// `~/.curlrc` is ignored; the standard `HTTPS_PROXY` variables still work.
/// Downloading and executing a binary is not a place to inherit someone's
/// debugging shortcuts.
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
        // Belt and braces with `--proto`: enforced in code as well as by flags.
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

/// Extract `rano` from a `.tar.gz` produced by `scripts/make-dist.sh` — a
/// minimal reader for a single-member gzip tar. It refuses anything but the one
/// shape we publish: a member named `rano`, regular and non-empty.
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

/// Gunzip — **a library, not a subprocess**: hand-pumped `gzip -d` pipes
/// deadlock (the child waits for stdin EOF; the 64 KB stdout pipe fills on a
/// 20 MB decompression), and a small test fixture cannot show either. `flate2`
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

/// Replace the running executable with `binary`: write a temp file next to the
/// target, then rename. Either the new binary is in place or the old one is.
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

/// Download and install `u`. Called only after a person said yes — nothing
/// here asks, and nothing here is on a timer. The URL comes from
/// [`Update::asset_url`], so the host is written down in one place.
pub fn install(u: &Update) -> Result<(), String> {
    let asset = u
        .asset
        .as_deref()
        .ok_or_else(|| format!("no build for this platform in release {}", u.latest))?;
    let bytes = Curl.get_bytes(&u.asset_url(asset))?;
    let binary = extract_binary(&bytes)?;
    replace_running(&binary, &current_exe()?)
}
