//! A picture read off the disk, for M-P's picture view and a markdown preview's `![…](path)`: the
//! bytes the kitty protocol carries and the cell size they are cut to. The terminal decodes PNG and
//! nothing else, so another format goes through a converter on the machine.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// How long a converter may take — a convenience, not a service, and the budget for a
/// whole document's conversions too (`Editor::document_images`). It runs on the frame's
/// own thread, so this is the longest one may hold the editor up.
pub const CONVERT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

/// How much memory a converter may hold, sampled as it runs: a 16 MiB file can be a
/// decompression bomb, and a preview that takes the machine down is worse than none.
const CONVERT_RSS_CAP: u64 = 256 * 1024 * 1024;

/// The address space a converter may map, not its memory budget: a threaded encoder
/// maps far more than it holds, and the RSS sampler is what bounds memory.
const CONVERT_ADDRESS_SPACE: u64 = 4 * 1024 * 1024 * 1024;

/// The most a converter may write: the box it was asked to fit is under two
/// megapixels, so a file past this is a converter that ignored the request.
const CONVERT_OUTPUT_CAP: u64 = 32 * 1024 * 1024;

/// The most a picture may be: the terminal decodes it into RGBA and holds it, and this
/// also keeps a conversion bounded — a file nobody would draw is not handed over.
const MAX_BYTES: u64 = 16 * 1024 * 1024;

/// A picture ready to hand over: the file's bytes (a PNG), and the pixel size its
/// header states.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pic {
    pub png: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Png,
    /// An image in another format: drawn if a converter can turn it into a PNG, and
    /// said to be undrawable when none can.
    Other,
}

/// The converters rano knows, in the order it tries them: `{in}`, `{out}`, `{w}`,
/// `{h}` and `{max}` are replaced, and every argument goes to the program directly,
/// never through a shell. ImageMagick first, then `sips` (which rasterizes SVG too).
pub const CONVERTERS: &[(&str, &[&str])] = &[
    (
        "magick",
        &[
            "{in}[0]",
            "-resize",
            "{w}x{h}",
            "-background",
            "none",
            "{out}",
        ],
    ),
    (
        "convert",
        &[
            "{in}[0]",
            "-resize",
            "{w}x{h}",
            "-background",
            "none",
            "{out}",
        ],
    ),
    (
        "sips",
        &[
            "-s", "format", "png", "-Z", "{max}", "{in}", "--out", "{out}",
        ],
    ),
    (
        "ffmpeg",
        &[
            "-y",
            "-loglevel",
            "error",
            "-i",
            "{in}",
            "-frames:v",
            "1",
            "-vf",
            "scale={w}:{h}:force_original_aspect_ratio=decrease",
            "{out}",
        ],
    ),
    (
        "python3",
        &[
            "-c",
            "from PIL import Image; im = Image.open(__import__('sys').argv[1]); im.thumbnail((int(__import__('sys').argv[3]), int(__import__('sys').argv[4]))); im.convert('RGBA').save(__import__('sys').argv[2], 'PNG')",
            "{in}",
            "{out}",
            "{w}",
            "{h}",
        ],
    ),
];

/// What `name` looks like, by its extension: this decides whether to try at all,
/// and the bytes decide in [`read`].
pub fn kind(name: Option<&Path>) -> Option<Kind> {
    let ext = name
        .and_then(Path::extension)
        .and_then(|e| e.to_str())?
        .to_ascii_lowercase();
    match ext.as_str() {
        "png" => Some(Kind::Png),
        "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "tiff" | "tif" | "heic" | "avif" | "svg"
        | "svgz" | "ico" => Some(Kind::Other),
        _ => None,
    }
}

pub fn is_png(name: Option<&Path>) -> bool {
    kind(name) == Some(Kind::Png)
}

/// `target` as a picture: absolute, `~/`, or relative to `dir`. A PNG is read as it
/// is, anything else a converter turns into one fitted to `max_px`. The `Err` is a
/// sentence, because a picture that does not appear cannot be diagnosed from the screen.
pub fn read(target: &str, dir: Option<&Path>, max_px: u32) -> Result<Pic, String> {
    read_with(target, dir, max_px, None)
}

/// [`read`] with the converter named: `RANO_IMAGE_CONVERT`'s value, or a test's own
/// stand-in. `None` asks the machine.
pub fn read_with(
    target: &str,
    dir: Option<&Path>,
    max_px: u32,
    named: Option<&str>,
) -> Result<Pic, String> {
    let path = resolve(target, dir);
    let meta = std::fs::metadata(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    if !meta.is_file() {
        return Err(format!("{} is not a file", path.display()));
    }
    if meta.len() > MAX_BYTES {
        return Err(format!(
            "{} is {} MiB: too large to draw (the cap is {} MiB)",
            path.display(),
            meta.len() / (1024 * 1024),
            MAX_BYTES / (1024 * 1024)
        ));
    }
    let bytes = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let shown = || path.display().to_string();
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return png(&bytes).map_err(|e| format!("{}: {e}", shown()));
    }
    let converted = convert(&path, max_px, named).map_err(|e| format!("{}: {e}", shown()))?;
    png(&converted).map_err(|e| format!("{}: {e}", shown()))
}

/// What a document's reference needs: its own directory, and the pixel box the picture
/// may fill at this view width.
pub fn box_px(width: usize) -> u32 {
    // The box in cells, at the same `CELL_PX`-sized cells a rasterized picture is cut
    // for. It is an upper bound rather than an exact fit: a converter is asked to fit
    // the *longest* side, and the cells are then cut from what it wrote.
    crate::term::graphics::image_box(width) * 16
}

/// The caps a conversion runs under, as a value so a test can tighten one. `rss` and
/// `address_space` are separate because the sampler and the kernel are separate guards.
#[derive(Debug, Clone, Copy)]
struct Limits {
    timeout: std::time::Duration,
    /// The child's resident memory as the sampler reads it.
    rss: u64,
    address_space: u64,
    output: u64,
}

impl Default for Limits {
    fn default() -> Limits {
        Limits {
            timeout: CONVERT_TIMEOUT,
            rss: CONVERT_RSS_CAP,
            address_space: CONVERT_ADDRESS_SPACE,
            output: CONVERT_OUTPUT_CAP,
        }
    }
}

/// A converter: a program and its arguments, placeholders unfilled. A table entry
/// keeps its arguments as written (one is a Python program with spaces in it); a
/// command line is split, because a person wrote it as one.
#[derive(Debug, Clone)]
pub struct Tool {
    pub program: String,
    pub args: Vec<String>,
}

impl Tool {
    /// A tool from a command line, whitespace-separated: what `RANO_IMAGE_CONVERT` and a
    /// test's stand-in are.
    pub fn line(line: &str) -> Tool {
        let mut words = line.split_whitespace();
        Tool {
            program: words.next().unwrap_or_default().to_string(),
            args: words.map(str::to_string).collect(),
        }
    }

    pub fn named(name: &str) -> Option<Tool> {
        CONVERTERS
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(n, args)| Tool {
                program: n.to_string(),
                args: args.iter().map(|a| a.to_string()).collect(),
            })
    }

    /// This tool's argv for one picture: the placeholders replaced, arg by arg.
    /// `{w}`×`{h}` is the box at 4:3 for a format with no size of its own (an SVG).
    pub fn spelled(&self, input: &Path, out: &Path, max_px: u32) -> (String, Vec<String>) {
        let (w, h) = ((max_px as u64).max(1), (max_px as u64) * 3 / 4);
        let fill = |a: &str| {
            a.replace("{in}", &input.to_string_lossy())
                .replace("{out}", &out.to_string_lossy())
                .replace("{w}", &w.to_string())
                .replace("{h}", &h.to_string())
                .replace("{max}", &max_px.to_string())
        };
        (
            self.program.clone(),
            self.args.iter().map(|a| fill(a)).collect(),
        )
    }
}

/// The converters to try, in order: the one that worked last time first, then the
/// table's entries on PATH. Present is not the same as working (`python3` without
/// Pillow), so [`convert_with`] keeps the first that reads the picture.
fn candidates() -> Vec<Tool> {
    let last = WORKED
        .get_or_init(|| std::sync::Mutex::new(None))
        .lock()
        .ok()
        .and_then(|w| w.clone());
    let mut out: Vec<Tool> = Vec::new();
    if let Some(name) = last
        && let Some(tool) = Tool::named(&name)
    {
        out.push(tool);
    }
    for (name, _) in CONVERTERS {
        let already = out.iter().any(|t| t.program == *name);
        if !already && on_path(name) {
            out.push(Tool::named(name).expect("a table entry"));
        }
    }
    out
}

/// Remember which converter read a picture, so the next one starts there.
fn remember(tool: &Tool) {
    if CONVERTERS.iter().any(|(n, _)| *n == tool.program)
        && let Ok(mut w) = WORKED.get_or_init(|| std::sync::Mutex::new(None)).lock()
    {
        *w = Some(tool.program.clone());
    }
}

static WORKED: OnceLock<std::sync::Mutex<Option<String>>> = OnceLock::new();

/// `path` as a PNG through a converter on the machine; `named` is a command line to
/// use instead of the machine's own. Bounded on every side — input, output, memory,
/// time — and a converter that runs past any of them is killed and reported.
pub fn convert(path: &Path, max_px: u32, named: Option<&str>) -> Result<Vec<u8>, String> {
    convert_with(path, max_px, named, Limits::default())
}

/// [`convert`] under caps a caller chooses. See [`Limits`].
fn convert_with(
    path: &Path,
    max_px: u32,
    named: Option<&str>,
    limits: Limits,
) -> Result<Vec<u8>, String> {
    let out = temp_png();
    let tools = match named {
        Some(line) => vec![Tool::line(line)],
        None => match std::env::var("RANO_IMAGE_CONVERT") {
            // An empty one is a person saying *no conversions here* — the same
            // answer as a machine with no converter on it.
            Ok(v) if !v.trim().is_empty() => vec![Tool::line(&v)],
            Ok(_) => Vec::new(),
            Err(_) => candidates(),
        },
    };
    if tools.is_empty() {
        let _ = std::fs::remove_file(&out);
        return Err(format!(
            "no converter found: none of {} is on PATH (RANO_IMAGE_CONVERT names one)",
            CONVERTERS
                .iter()
                .map(|(name, _)| *name)
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    let mut last = String::new();
    for tool in tools {
        match run(&tool, path, &out, max_px, limits) {
            Ok(()) => {
                remember(&tool);
                let bytes = match std::fs::read(&out) {
                    Ok(b) if !b.is_empty() => Ok(b),
                    Ok(_) => Err("the converter wrote an empty file".to_string()),
                    Err(e) => Err(format!("the converter wrote no picture: {e}")),
                };
                let _ = std::fs::remove_file(&out);
                return bytes;
            }
            // The next candidate is asked, so a present-but-useless tool does not
            // stand in the way of one that works.
            Err(e) => last = e,
        }
    }
    let _ = std::fs::remove_file(&out);
    Err(last)
}

/// Run one converter, watched: a box to fit, a memory cap and a deadline, sampled and
/// killed as it runs. No shell — an argument with a space, a quote or a `;` in it is
/// still an argument.
fn run(tool: &Tool, input: &Path, out: &Path, max_px: u32, limits: Limits) -> Result<(), String> {
    let (program, args) = tool.spelled(input, out, max_px);
    if program.is_empty() {
        return Err("the converter command is empty".to_string());
    }
    let mut cmd = std::process::Command::new(&program);
    cmd.args(&args)
        .stdin(std::process::Stdio::null())
        // Its own words go nowhere, and a pipe nobody drains is a child that
        // blocks for ever on a full one.
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    // The kernel's half of the cap, best effort: a platform that refuses still has
    // the sampler, and the sampler the deadline.
    #[cfg(unix)]
    unsafe {
        use std::os::unix::process::CommandExt;
        cmd.pre_exec(move || {
            // No type on `what`: the RLIMIT_ constants are `c_int` on macOS and glibc's own
            // unsigned `__rlimit_resource_t` on Linux, and an annotation is a compile error on one.
            let limit = |what, value: u64| {
                let rl = libc::rlimit {
                    rlim_cur: value,
                    rlim_max: value,
                };
                libc::setrlimit(what, &rl);
            };
            limit(libc::RLIMIT_AS, limits.address_space);
            limit(libc::RLIMIT_FSIZE, limits.output);
            // CPU seconds, so a spinning converter dies even if this process is not
            // scheduled to notice it.
            limit(libc::RLIMIT_CPU, limits.timeout.as_secs().max(1));
            Ok(())
        });
    }
    let mut child = cmd.spawn().map_err(|e| format!("{program}: {e}"))?;
    let started = std::time::Instant::now();
    let mut sampled = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Ok(()),
            Ok(Some(status)) => {
                return Err(format!(
                    "{program} could not read it ({})",
                    status
                        .code()
                        .map_or("killed".to_string(), |c| format!("exit {c}"))
                ));
            }
            Ok(None) => {}
            Err(e) => return Err(format!("{program}: {e}")),
        }
        // Sampled, not assumed: the kernel will not stop a runaway converter on every
        // platform.
        if sampled.elapsed() >= std::time::Duration::from_millis(25) {
            sampled = std::time::Instant::now();
            if let Some(rss) = rss(child.id())
                && rss > limits.rss
            {
                stop(&mut child);
                return Err(format!(
                    "{program} wanted {} MiB (the cap is {} MiB)",
                    rss / (1024 * 1024),
                    limits.rss / (1024 * 1024)
                ));
            }
        }
        if started.elapsed() >= limits.timeout {
            stop(&mut child);
            return Err(format!(
                "{program} took more than {:?} (a picture is a convenience here, and one nobody can draw in that time is not worth the wait)",
                limits.timeout
            ));
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

/// Kill the converter and reap it, so a timed-out one is not left running behind us.
fn stop(child: &mut std::process::Child) {
    let _ = child.kill();
    let _ = child.wait();
}

/// A process's resident memory, or `None` where the platform will not say
/// (`/proc/<pid>/statm` on Linux, `proc_pidinfo` on macOS); a `None` leaves the
/// kernel's cap and the deadline as the guards.
fn rss(pid: u32) -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        let statm = std::fs::read_to_string(format!("/proc/{pid}/statm")).ok()?;
        // `size resident shared …`, and resident is in PAGES, which are not always 4 KiB.
        let pages: u64 = statm.split_whitespace().nth(1)?.parse().ok()?;
        let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        let page = if page > 0 { page as u64 } else { 4096 };
        Some(pages * page)
    }
    #[cfg(target_os = "macos")]
    {
        let mut info: libc::proc_taskinfo = unsafe { std::mem::zeroed() };
        let want = std::mem::size_of::<libc::proc_taskinfo>() as libc::c_int;
        // SAFETY: the buffer is this struct and `want` exactly its size; anything
        // short of `want` is a process this cannot describe.
        let got = unsafe {
            libc::proc_pidinfo(
                pid as libc::c_int,
                libc::PROC_PIDTASKINFO,
                0,
                &mut info as *mut libc::proc_taskinfo as *mut libc::c_void,
                want,
            )
        };
        (got == want).then_some(info.pti_resident_size)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = pid;
        None
    }
}

/// Whether `program` is on PATH and executable; a name with a `/` is tried as
/// a path.
fn on_path(program: &str) -> bool {
    let is_exe = |p: &Path| {
        let Ok(meta) = std::fs::metadata(p) else {
            return false;
        };
        if !meta.is_file() {
            return false;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            meta.permissions().mode() & 0o111 != 0
        }
        #[cfg(not(unix))]
        {
            true
        }
    };
    if program.contains('/') {
        return is_exe(Path::new(program));
    }
    std::env::var_os("PATH")
        .map(|paths| std::env::split_paths(&paths).any(|dir| is_exe(&dir.join(program))))
        .unwrap_or(false)
}

/// The temp file a conversion is written to: pid and counter, so two previews
/// cannot write over one another.
fn temp_png() -> PathBuf {
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    std::env::temp_dir().join(format!("rano-picture-{}-{n}.png", std::process::id()))
}

/// A file read off the disk as a picture: the PNG's bytes and its size. The bytes
/// decide, never the name.
pub fn png(bytes: &[u8]) -> Result<Pic, String> {
    if !bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Err("not a PNG: the terminal draws PNG, and this is something else".to_string());
    }
    // IHDR is the first chunk: 8 bytes of signature, 4 of length, 4 of type,
    // then width and height, big-endian.
    if bytes.len() < 24 {
        return Err("not a PNG: the header is truncated".to_string());
    }
    let be =
        |at: usize| u32::from_be_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]);
    let (width, height) = (be(16), be(20));
    if width == 0 || height == 0 {
        return Err("the PNG has no size: nothing to draw".to_string());
    }
    Ok(Pic {
        png: bytes.to_vec(),
        width,
        height,
    })
}

/// **Where a reference points**: `~/` is home, an absolute path is itself,
/// anything else is relative to `dir`.
pub fn resolve(target: &str, dir: Option<&Path>) -> PathBuf {
    if let Some(rest) = target.strip_prefix("~/")
        && let Some(home) = std::env::var_os("HOME")
    {
        return PathBuf::from(home).join(rest);
    }
    let path = Path::new(target);
    match (path.is_absolute(), dir) {
        (true, _) | (false, None) => path.to_path_buf(),
        (false, Some(dir)) => dir.join(path),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A PNG-shaped buffer: signature, then a real IHDR carrying `w` and `h`. The
    /// bytes need not decode, because nothing here decodes a PNG.
    fn png_header(w: u32, h: u32) -> Vec<u8> {
        let mut v = b"\x89PNG\r\n\x1a\n".to_vec();
        v.extend_from_slice(&13u32.to_be_bytes()); // IHDR length
        v.extend_from_slice(b"IHDR");
        v.extend_from_slice(&w.to_be_bytes());
        v.extend_from_slice(&h.to_be_bytes());
        v.extend_from_slice(&[8, 2, 0, 0, 0]); // depth, colour type, … — unread
        v.extend_from_slice(&[0, 0, 0, 0]); // crc — unread
        v
    }

    struct TempDir(PathBuf);
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn temp_dir(tag: &str) -> TempDir {
        let d = std::env::temp_dir().join(format!("rano_image_{}_{}", tag, std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        TempDir(d)
    }

    #[test]
    fn the_bytes_decide_and_the_name_only_says_where_to_look() {
        // A name that says nothing is not read at all; a name that says PNG is
        // still refused when the bytes are not one.
        assert_eq!(kind(Some(Path::new("notes.txt"))), None);
        assert_eq!(kind(Some(Path::new("sun.PNG"))), Some(Kind::Png));
        assert_eq!(kind(Some(Path::new("sun.jpg"))), Some(Kind::Other));
        assert!(!is_png(Some(Path::new("sun.svg"))));
        assert!(is_png(Some(Path::new("/w/sun.png"))));
        assert_eq!(kind(None), None);

        let p = png(&png_header(640, 480)).expect("a png");
        assert_eq!((p.width, p.height), (640, 480));
        assert!(png(b"\xff\xd8\xff\xe0 a jpeg").is_err());
        assert!(png(b"\x89PNG\r\n\x1a\nshort").is_err(), "a cut header");
        let e = png(&png_header(0, 10)).unwrap_err();
        assert!(e.contains("no size"), "{e}");
    }

    #[test]
    fn a_reference_resolves_beside_the_file_that_named_it() {
        let d = temp_dir("resolve");
        let home = std::env::var("HOME").unwrap_or_else(|_| "/nonexistent".to_string());
        assert_eq!(resolve("/a/b.png", Some(&d.0)), PathBuf::from("/a/b.png"));
        assert_eq!(resolve("sun.png", Some(&d.0)), d.0.join("sun.png"));
        assert_eq!(resolve("sun.png", None), PathBuf::from("sun.png"));
        assert_eq!(
            resolve("~/sun.png", None),
            PathBuf::from(home).join("sun.png")
        );

        let f = d.0.join("sun.png");
        std::fs::write(&f, png_header(12, 7)).unwrap();
        let pic = read("sun.png", Some(&d.0), 1280).expect("read");
        assert_eq!((pic.width, pic.height), (12, 7));
        // What is not there is said by name, not drawn as nothing.
        let e = read("moon.png", Some(&d.0), 1280).unwrap_err();
        assert!(e.contains("moon.png"), "{e}");
        // A directory is not a picture.
        let e = read(".", Some(&d.0), 1280).unwrap_err();
        assert!(e.contains("is not a file"), "{e}");
    }

    /// A converter of our own: a shell script, so the machinery can be exercised
    /// where ImageMagick, `sips` and Python are not. `sh`, because both platforms have it.
    fn fake_converter(d: &TempDir, name: &str, body: &str) -> String {
        use std::os::unix::fs::PermissionsExt;
        let path = d.0.join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path.to_string_lossy().into_owned()
    }

    /// A template's placeholders all land where they were spelled, and an argument is
    /// an argument: a name with a space is one argument.
    #[test]
    fn a_converter_line_spells_the_picture_the_output_and_the_box() {
        let tool = Tool::line("tool {in} {out} {w} {h} {max}");
        let (program, args) = tool.spelled(Path::new("/a b/c.jpg"), Path::new("/tmp/o.png"), 1280);
        assert_eq!(program, "tool");
        assert_eq!(
            args,
            vec!["/a b/c.jpg", "/tmp/o.png", "1280", "960", "1280"]
        );
        // **The table's own arguments are kept whole**: one is a Python program,
        // and splitting it is how a `-c` script stops being a script.
        let python = Tool::named("python3").expect("the table has one");
        assert_eq!(python.args[0], "-c");
        assert!(python.args[1].contains("from PIL import Image"));
        assert_eq!(python.args.len(), 6, "-c, script, in, out, w, h");
        // Each table entry spells a name, an input, an output and a size.
        for (name, args) in CONVERTERS {
            assert!(!name.is_empty());
            assert!(!args.is_empty(), "{name} has no arguments");
            assert!(
                args.iter().any(|a| a.contains("{in}")),
                "{name} does not name its input"
            );
            assert!(
                args.iter().any(|a| a.contains("{out}")),
                "{name} does not name its output"
            );
            assert!(
                args.iter()
                    .any(|a| a.contains("{max}") || a.contains("{w}")),
                "{name} is not told what size to write"
            );
        }
        // `on_path` answers for a program and for a path, and a bare name that is nowhere.
        assert!(on_path("/bin/sh"));
        assert!(!on_path("definitely-not-a-program-xyz"));
        assert!(!on_path("/nonexistent/never"));
    }

    /// A picture that is not a PNG is converted and read back: what comes out is the
    /// converter's PNG, and the temp file it wrote does not survive the call.
    #[test]
    fn a_converter_turns_a_picture_into_the_png_the_terminal_takes() {
        let d = temp_dir("convert");
        let source = d.0.join("sun.jpg");
        std::fs::write(&source, b"\xff\xd8\xff\xe0 not really a jpeg").unwrap();
        let ready = d.0.join("ready.png");
        std::fs::write(&ready, png_header(200, 150)).unwrap();
        // The stand-in: write the prepared picture where it was told to — `$2`
        // is the output — found beside the script itself.
        let script = fake_converter(&d, "fake.sh", "cp \"$(dirname \"$0\")/ready.png\" \"$2\"");
        let line = format!("{script} {{in}} {{out}} {{max}}");
        let bytes = convert(&source, 320, Some(&line)).expect("converted");
        let pic = png(&bytes).expect("a png");
        assert_eq!((pic.width, pic.height), (200, 150));
        // The whole path a preview takes, from the reference to the picture.
        let pic = read_with(&source.to_string_lossy(), None, 320, Some(&line)).expect("read");
        assert_eq!((pic.width, pic.height), (200, 150));
        // Nothing of ours is left in the temp directory.
        let leftovers: Vec<String> = std::fs::read_dir(std::env::temp_dir())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with(&format!("rano-picture-{}", std::process::id())))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
        // **A PNG is never handed to a converter**: this one does not exist, so
        // being read as it is proves the fast path.
        let real = d.0.join("real.png");
        std::fs::write(&real, png_header(9, 4)).unwrap();
        let pic = read_with(
            &real.to_string_lossy(),
            None,
            320,
            Some("/nonexistent/never-run {in} {out}"),
        )
        .expect("read as it is");
        assert_eq!((pic.width, pic.height), (9, 4));
        // And a converter that fails says which tool and that it could not read it.
        let broken = fake_converter(&d, "broken.sh", "exit 3");
        let e = read_with(&source.to_string_lossy(), None, 320, Some(&broken)).unwrap_err();
        assert!(
            e.contains("broken.sh") && e.contains("could not read it"),
            "{e}"
        );
    }

    /// A converter that hangs is killed and said to have been — a sentence, not a
    /// frozen editor.
    #[test]
    fn a_converter_that_hangs_is_stopped_by_the_clock() {
        let d = temp_dir("hang");
        let source = d.0.join("slow.gif");
        std::fs::write(&source, b"GIF89a").unwrap();
        let script = fake_converter(&d, "slow.sh", "sleep 30");
        let caps = Limits {
            timeout: std::time::Duration::from_millis(300),
            ..Limits::default()
        };
        let started = std::time::Instant::now();
        let e = convert_with(&source, 320, Some(&script), caps).unwrap_err();
        assert!(e.contains("took more than"), "{e}");
        assert!(
            started.elapsed() < std::time::Duration::from_secs(10),
            "the deadline was the guard, not the sleep: {:?}",
            started.elapsed()
        );
    }

    /// A converter that goes for memory is killed before the machine notices; the
    /// kernel's cap is lifted out of the way, so the sampler is the guard.
    #[test]
    fn a_converter_that_runs_away_with_memory_is_stopped() {
        let d = temp_dir("fat");
        let source = d.0.join("bomb.tiff");
        std::fs::write(&source, b"II*\0").unwrap();
        // A shell holding 8 MB in a variable, via tools both platforms have.
        let script = fake_converter(
            &d,
            "fat.sh",
            "x=$(dd if=/dev/zero bs=1024 count=8192 2>/dev/null | tr '\\0' x); sleep 5",
        );
        let caps = Limits {
            rss: 2 * 1024 * 1024,
            address_space: 64 * 1024 * 1024 * 1024,
            timeout: std::time::Duration::from_secs(20),
            ..Limits::default()
        };
        let e = convert_with(&source, 320, Some(&script), caps).unwrap_err();
        assert!(e.contains("(the cap is 2 MiB)"), "{e}");
    }

    /// The sampler answers on the platforms rano runs on, and says nothing
    /// about a process it cannot see.
    #[test]
    fn the_memory_sampler_reads_a_process_it_can_see() {
        let me = rss(std::process::id()).expect("this process's own memory");
        assert!(me > 1024 * 1024, "{me} bytes of RSS for a test binary");
        assert_eq!(rss(u32::MAX), None, "a pid that is not a process");
    }

    /// The real thing, on a machine that has one: `cargo test -- --ignored
    /// a_converter_on_this_machine`. An SVG is the strongest form — not a raster at all.
    #[test]
    #[ignore = "needs a converter on PATH (sips, ImageMagick, ffmpeg or Python with Pillow)"]
    fn a_converter_on_this_machine_draws_a_document_that_is_not_a_raster() {
        let d = temp_dir("real");
        let svg = d.0.join("box.svg");
        std::fs::write(
            &svg,
            r##"<svg xmlns="http://www.w3.org/2000/svg" width="120" height="60">
<rect width="120" height="60" fill="#345"/></svg>"##,
        )
        .unwrap();
        let available: Vec<&str> = CONVERTERS
            .iter()
            .map(|(n, _)| *n)
            .filter(|n| on_path(n))
            .collect();
        assert!(
            !available.is_empty(),
            "no converter on this machine: {}",
            CONVERTERS
                .iter()
                .map(|(n, _)| *n)
                .collect::<Vec<_>>()
                .join(", ")
        );
        let pic = read(&svg.to_string_lossy(), None, 320).expect("converted");
        // **The box reached the tool, and the document's own shape came back.**
        // `sips -Z` *resamples to* the longest side, and a vector drawn at the
        // size it will be shown is sharper than one scaled up by the terminal.
        assert_eq!(
            pic.png[..8],
            b"\x89PNG\r\n\x1a\n"[..],
            "a PNG, whatever wrote it"
        );
        assert_eq!(pic.width.max(pic.height), 320, "the box it was asked for");
        assert_eq!(
            pic.width * 60,
            pic.height * 120,
            "and the document's own aspect"
        );
        assert!(!CONVERTERS.is_empty());
    }
}
