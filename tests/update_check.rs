//! The update check's logic, with no network in it.
//!
//! Split out because the interesting parts — is this version newer, is the
//! feature on, what does a release payload mean — are pure functions, and the
//! only untestable piece is the HTTP call itself. That is deliberately the
//! smallest part of the module.

use rano::update::{self, Get};

/// A `Get` that returns a canned body, so no test here touches the network.
struct Canned(&'static str);
impl Get for Canned {
    fn get(&self, _url: &str) -> Result<String, String> {
        Ok(self.0.to_string())
    }
}

struct Offline;
impl Get for Offline {
    fn get(&self, url: &str) -> Result<String, String> {
        Err(format!("no network for {url}"))
    }
}

const RELEASE: &str = r#"{
  "tag_name": "v99.0.0",
  "name": "99",
  "assets": [{"name": "rano-x86_64-unknown-linux-gnu.tar.gz"}]
}"#;

/// The request must never carry a secret: this is an unauthenticated GET of a
/// public release.
#[test]
fn a_newer_release_is_offered() {
    let u = update::check_with(true, &Canned(RELEASE)).expect("a newer release");
    assert_eq!(u.latest, "99.0.0");
    assert_eq!(u.current, update::CURRENT);
    assert!(u.message().contains("99.0.0"));
    assert!(u.message().contains(update::CURRENT));
}

/// **The gate.** Off means not even a request, so a user who turned it off can
/// see there is no network traffic. Asserted with an url-recording stub.
#[test]
fn disabled_does_not_even_ask() {
    struct Recording(std::cell::Cell<bool>);
    impl Get for Recording {
        fn get(&self, _url: &str) -> Result<String, String> {
            self.0.set(true);
            Ok(RELEASE.to_string())
        }
    }
    let r = Recording(std::cell::Cell::new(false));
    assert!(update::check_with(false, &r).is_none());
    assert!(
        !r.0.get(),
        "disabled must not make the request at all, not merely discard it"
    );

    // And enabled does ask, so the assertion above is not vacuous.
    let r = Recording(std::cell::Cell::new(false));
    assert!(update::check_with(true, &r).is_some());
    assert!(r.0.get());
}

/// A network that is down is not the editor's problem.
#[test]
fn a_failed_request_is_silent() {
    assert!(update::check_with(true, &Offline).is_none());
}

/// Already current: no notice, and in particular no download offered.
#[test]
fn a_current_build_is_not_offered() {
    let body = format!(r#"{{"tag_name": "v{}"}}"#, update::CURRENT);
    assert!(update::check_with(true, &Canned(Box::leak(body.into_boxed_str()))).is_none());
}

/// Versions are compared numerically, not as text — the bug where `0.10.0` looks
/// older than `0.9.0`.
#[test]
fn versions_compare_numerically() {
    assert!(update::is_newer("0.9.0", "0.10.0"), "10 > 9 numerically");
    assert!(!update::is_newer("0.10.0", "0.9.0"));
    assert!(
        update::is_newer("v1.2.3", "v1.2.4"),
        "the leading v is optional"
    );
    assert!(!update::is_newer("1.2.3", "1.2.3"), "equal is not newer");
    assert!(update::is_newer("0.1.0", "1.0.0"));
}

/// A version that is not `MAJOR.MINOR.PATCH` is not newer than anything.
///
/// Strict on purpose: ordering `0.2.0-rc1` by accident would download a
/// pre-release binary to someone who asked for stable.
#[test]
fn odd_versions_do_not_trigger_an_update() {
    for odd in [
        "0.2.0-rc1",
        "0.2.0+build",
        "nightly",
        "1.0",
        "1.0.0.0",
        "v",
        "",
    ] {
        assert_eq!(update::parse_version(odd), None, "{odd:?} parsed");
        assert!(
            !update::is_newer("0.1.0", odd),
            "{odd:?} must not be treated as newer"
        );
    }

    // **A date-shaped tag IS a triple and does compare.** `2026.09.30` is three
    // numbers, so it parses and is newer than `0.1.0`. Accepted rather than
    // special-cased: it is a valid version, and a tag like it would only be cut
    // deliberately. Noted so the behaviour is a decision, not a surprise.
    assert_eq!(update::parse_version("2026.09.30"), Some((2026, 9, 30)));
    assert!(update::is_newer("0.1.0", "2026.09.30"));
    // Leading zeros are fine, and the `v` prefix is optional.
    assert_eq!(update::parse_version("v0.10.0"), Some((0, 10, 0)));
}

/// A payload without a usable tag is not an update.
#[test]
fn a_payload_without_a_tag_is_not_an_update() {
    for body in [
        "{}",
        r#"{"tag_name": ""}"#,
        r#"{"message": "Not Found"}"#,
        "",
        "not json at all",
    ] {
        assert_eq!(update::tag_name(body), None, "{body:?}");
        assert!(
            update::check_with(true, &Canned(body)).is_none(),
            "{body:?}"
        );
    }
}

/// The download URL is built from a constant host, never from the response.
#[test]
fn the_asset_url_is_the_repo_we_are() {
    let u = update::check_with(true, &Canned(RELEASE)).expect("an update");
    let asset = u.asset.clone().expect("an asset for this platform");
    let url = u.asset_url(&asset);
    assert!(
        url.starts_with("https://github.com/"),
        "https and a fixed host: {url}"
    );
    assert!(
        url.starts_with("https://github.com/deadtrickster/rano/releases/download/v99.0.0/"),
        "{url}"
    );
    assert!(url.ends_with(&asset));
}

/// The asset names are the ones `scripts/make-dist.sh` writes and `install.sh`
/// asks for. `scripts/check-dist-names.sh` compares the shell sides with the
/// workflow matrix; this is the Rust side of the same contract.
#[test]
fn asset_names_match_the_published_ones() {
    assert_eq!(
        update::asset_name("linux", "x86_64"),
        Some("rano-x86_64-unknown-linux-gnu.tar.gz")
    );
    assert_eq!(
        update::asset_name("linux", "aarch64"),
        Some("rano-aarch64-unknown-linux-gnu.tar.gz")
    );
    assert_eq!(
        update::asset_name("macos", "aarch64"),
        Some("rano-aarch64-apple-darwin.tar.gz")
    );
    assert_eq!(
        update::asset_name("macos", "x86_64"),
        Some("rano-x86_64-apple-darwin.tar.gz")
    );
    // A platform we do not build for gets no asset, and therefore no offer to
    // install — rather than an offer that downloads a 404.
    assert_eq!(update::asset_name("windows", "x86_64"), None);
    assert_eq!(update::asset_name("freebsd", "x86_64"), None);
    assert_eq!(update::asset_name("linux", "riscv64"), None);
}

/// The gate's precedence, and its default. **On by default** — normal software
/// checks for updates unless told not to — with the config value winning over the
/// environment.
#[test]
fn the_gate_is_on_by_default_and_the_config_wins() {
    assert!(
        update::enabled(None, None),
        "ON with no config and no env: the normal-software default"
    );
    assert!(update::enabled(Some(true), None));
    assert!(!update::enabled(Some(false), None));
    // Config beats environment, in both directions — so a project config saying
    // false cannot be overridden by an environment that says true.
    assert!(update::enabled(Some(true), Some("0")));
    assert!(!update::enabled(Some(false), Some("1")));
    // The environment is a SWITCH, not a truthiness test: `RANO_AUTOUPDATE=0`
    // turns it off for one command, and reading `"0"` as true would make the
    // switch a lie. Anything outside the off-words leaves the default (on).
    for off in ["0", "false", "no", "off", " OFF "] {
        assert!(!update::enabled(None, Some(off)), "{off:?} must mean off");
    }
    for on in ["1", "true", "yes", "on", ""] {
        assert!(update::enabled(None, Some(on)), "{on:?} must leave it on");
    }
}

/// A non-HTTPS URL is refused by the code, not only by curl's flags.
#[test]
fn a_non_https_url_is_refused() {
    let err = update::Curl
        .get("http://example.com/rano.json")
        .expect_err("http must be refused");
    assert!(err.contains("non-https"), "{err}");
}

/// **A tarball is only accepted in the shape we publish — and the reader must not
/// hang.**
///
/// This is the test that caught a deadlock in `gunzip`: it wrote to the child's
/// stdin without closing it, so `gzip` waited for input that never came and the
/// suite sat here for twelve minutes with a live child. Running the real
/// extraction on a thread with a deadline is what makes that failure a FAILURE
/// next time instead of a hang.
#[test]
fn a_tarball_is_only_accepted_in_the_shape_we_publish() {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        // Small on purpose: this test is about the SHAPE rules (symlink, empty,
        // missing). Size is `a_large_asset_extracts`' job.
        let good = tar_with(&[("rano", b"#!/bin/sh\necho hi\n", b'0')]);
        let _ = tx.send((
            update::extract_binary(&gzip(&good)).ok(),
            update::extract_binary(&gzip(&tar_with(&[("rano", b"target", b'2')]))).is_err(),
            update::extract_binary(&gzip(&tar_with(&[("something", b"x", b'0')]))).is_err(),
            update::extract_binary(&gzip(&tar_with(&[("rano", b"", b'0')]))).is_err(),
            update::extract_binary(b"not a tarball").is_err(),
        ));
    });
    let got = rx.recv_timeout(std::time::Duration::from_secs(30)).expect(
        "extraction did not finish in 30 s — a pipe deadlock, most likely a child \
         whose stdin is never closed",
    );
    assert_eq!(
        got.0.as_deref(),
        Some(&b"#!/bin/sh\necho hi\n"[..]),
        "a regular `rano` member extracts"
    );
    assert!(got.1, "a symlink must be refused");
    assert!(got.2, "a tar with no `rano` must be refused");
    assert!(got.3, "an empty member must be refused");
    assert!(got.4, "input that is not gzip must be refused");
}

/// Build a tar with the given members. 512-byte headers, octal fields.
fn tar_with(members: &[(&str, &[u8], u8)]) -> Vec<u8> {
    let mut out = Vec::new();
    for (name, body, typeflag) in members {
        let mut h = [0u8; 512];
        h[..name.len()].copy_from_slice(name.as_bytes());
        h[100..108].copy_from_slice(b"0000755\0");
        h[108..116].copy_from_slice(b"0000000\0");
        h[116..124].copy_from_slice(b"0000000\0");
        let size = format!("{:011o}\0", body.len());
        h[124..136].copy_from_slice(size.as_bytes());
        h[136..148].copy_from_slice(b"00000000000\0");
        h[148..156].copy_from_slice(b"        ");
        h[156] = *typeflag;
        h[257..263].copy_from_slice(b"ustar\0");
        h[263..265].copy_from_slice(b"00");
        // The checksum is not verified by the reader; fill it plausibly so a
        // stricter reader later would still accept this fixture.
        h[148..156].copy_from_slice(b"0000000\0");
        out.extend_from_slice(&h);
        out.extend_from_slice(body);
        let pad = (512 - body.len() % 512) % 512;
        out.extend(std::iter::repeat_n(0u8, pad));
    }
    out.extend(std::iter::repeat_n(0u8, 1024));
    out
}

/// gzip a fixture, through the same library the module reads with — so the test
/// cannot pass on a fixture the module could not actually handle.
fn gzip(data: &[u8]) -> Vec<u8> {
    use flate2::Compression;
    use flate2::write::GzEncoder;
    use std::io::Write;
    let mut e = GzEncoder::new(Vec::new(), Compression::default());
    e.write_all(data).unwrap();
    e.finish().unwrap()
}

/// **A fixture LARGER than a pipe buffer, because the small one hid a deadlock.**
///
/// The extraction used to shell out to `gzip` and pump its pipes by hand, and it
/// deadlocked on this repo's real 3.4 MB asset: the decompressed tar is ~20 MB,
/// the stdout pipe holds 64 KB, and once it filled, `gzip` blocked writing while
/// the caller blocked writing to *its* stdin. The fixture here was a few hundred
/// bytes — inside the pipe buffer — so the deadlock was unreachable in the test.
/// This uses 300 KB of compressible payload, which decompresses well past the
/// buffer, so the shape that broke is exercised rather than assumed away.
#[test]
fn a_large_asset_extracts() {
    let payload = vec![b'a'; 300 * 1024];
    let tar = tar_with(&[("rano", &payload, b'0')]);
    let gz = gzip(&tar);
    // Compressible, so the ARCHIVE is small even though the member is not —
    // which is exactly the real asset\'s shape: 3.4 MB in, 20 MB out.
    assert!(
        gz.len() < payload.len(),
        "the archive should be smaller than the member: {} vs {}",
        gz.len(),
        payload.len()
    );

    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(update::extract_binary(&gz).map(|b| b.len()));
    });
    let got = rx.recv_timeout(std::time::Duration::from_secs(30)).expect(
        "a large asset did not extract in 30 s — a pipe deadlock, most likely \
         writing input without draining output",
    );
    assert_eq!(got.expect("extract"), payload.len());
}
