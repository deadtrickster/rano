//! A PDF page: the page count, one page as a PNG, and one page as text.
//! The rasterizing is the tools' (`pdftoppm`, `gs`, `pdftotext`), not ours.
//! Every call asks for one page, and every cap errors rather than truncating.

use std::path::{Path, PathBuf};

/// How long one page may take: past anything a page costs, short enough that a
/// wedged tool is a pause — the caller may be the frame's own thread.
pub const RENDER_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Sampled memory cap: past any single page, short of swapping a small machine.
const RENDER_RSS_CAP: u64 = 1024 * 1024 * 1024;

/// The kernel's half, twice the sampler's budget: a rasterizer maps more than it holds.
const RENDER_ADDRESS_SPACE: u64 = 2 * 1024 * 1024 * 1024;

/// The most one page may be as a PNG; past what a terminal can hold as a picture.
const RENDER_OUTPUT_CAP: u64 = 64 * 1024 * 1024;

/// The most one page may be as text; past it, the page count lied.
const TEXT_OUTPUT_CAP: u64 = 4 * 1024 * 1024;

/// The most this file reads itself, for the parse path when `pdfinfo` is absent.
const MAX_PARSE_BYTES: u64 = 64 * 1024 * 1024;

/// The low end of renderable dpi: below it a page is not legible.
pub const MIN_DPI: u32 = 24;

/// The high end: past the sharpest terminal, slower without showing more.
pub const MAX_DPI: u32 = 600;

/// Which route answered a page count; read back through [`last_count`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Counted {
    PdfInfo,
    Parsed,
}

impl Counted {
    fn code(self) -> u8 {
        match self {
            Counted::PdfInfo => 1,
            Counted::Parsed => 2,
        }
    }
}

// The route the last `page_count` took, per thread. A macro cannot carry a doc
// comment, which is why this is not one.
std::thread_local! {
    static LAST_COUNT: std::cell::Cell<u8> = const { std::cell::Cell::new(0) };
}

/// Which route the last [`page_count`] took, or `None` before one answered.
pub fn last_count() -> Option<Counted> {
    match LAST_COUNT.with(std::cell::Cell::get) {
        1 => Some(Counted::PdfInfo),
        2 => Some(Counted::Parsed),
        _ => None,
    }
}

fn record(which: Counted) {
    LAST_COUNT.with(|c| c.set(which.code()));
}

/// How many pages `path` has: `pdfinfo` when it is on PATH, the page tree's `/Count`
/// in the bytes otherwise. Which one answered is recorded for [`last_count`].
pub fn page_count(path: &Path) -> Result<u32, String> {
    let (pages, which) = count_with(path, None, Limits::text())?;
    record(which);
    Ok(pages)
}

/// One page as a PNG; `page_index` is zero-based. `pdftoppm` first, `gs` second; a
/// past-the-end index, a dpi outside [`MIN_DPI`]..=[`MAX_DPI`] and a machine with
/// neither tool are all errors naming what is wrong.
pub fn page_png(path: &Path, page_index: u32, dpi: u32) -> Result<Vec<u8>, String> {
    page_png_with(path, page_index, dpi, Limits::page(), None)
}

/// One page as text; `page_index` is zero-based, as for [`page_png`].
pub fn page_text(path: &Path, page_index: u32) -> Result<String, String> {
    page_text_with(path, page_index, Limits::text(), None)
}

/// The caps one call runs under, as a value so a test can tighten one.
#[derive(Debug, Clone, Copy)]
struct Limits {
    timeout: std::time::Duration,
    rss: u64,
    address_space: u64,
    /// The most the child may write: the page's file, or the text it prints.
    output: u64,
}

impl Limits {
    /// A page raster.
    fn page() -> Limits {
        Limits {
            timeout: RENDER_TIMEOUT,
            rss: RENDER_RSS_CAP,
            address_space: RENDER_ADDRESS_SPACE,
            output: RENDER_OUTPUT_CAP,
        }
    }

    /// A page of text: the same deadline and memory, a smaller output cap.
    fn text() -> Limits {
        Limits {
            output: TEXT_OUTPUT_CAP,
            ..Limits::page()
        }
    }
}

/// One way to answer, spelled but not run. `{out}` is the file the tool may write;
/// `{stem}` is that name without its extension (`pdftoppm -singlefile` appends `.png`).
#[derive(Debug, Clone)]
struct Cmd {
    program: String,
    args: Vec<String>,
    /// The answer is the tool's stdout (`pdfinfo`) rather than a file (`pdftoppm`).
    prints: bool,
}

/// The renderers for one page, in the order tried — both listed whether or not the
/// machine has them, so the argv can be tested without either installed.
fn renderers(input: &Path, page_index: u32, dpi: u32) -> Vec<Cmd> {
    let page = (page_index + 1).to_string();
    let file = input.to_string_lossy().into_owned();
    vec![
        Cmd {
            program: "pdftoppm".to_string(),
            args: vec![
                "-png".to_string(),
                "-r".to_string(),
                dpi.to_string(),
                "-f".to_string(),
                page.clone(),
                "-l".to_string(),
                page.clone(),
                "-singlefile".to_string(),
                file.clone(),
                "{stem}".to_string(),
            ],
            prints: false,
        },
        Cmd {
            program: "gs".to_string(),
            args: vec![
                "-q".to_string(),
                "-dNOPAUSE".to_string(),
                "-dBATCH".to_string(),
                "-sDEVICE=png16m".to_string(),
                // The same antialiasing `pdftoppm` gives by default, so the fallback is
                // not visibly the worse picture.
                "-dTextAlphaBits=4".to_string(),
                "-dGraphicsAlphaBits=4".to_string(),
                format!("-r{dpi}"),
                format!("-dFirstPage={page}"),
                format!("-dLastPage={page}"),
                "-sOutputFile={out}".to_string(),
                file,
            ],
            prints: false,
        },
    ]
}

/// The renderers, told to write PPM: a fifteen-byte header in front of raw RGB, so a
/// plugin wanting pixels needs no decoder.
fn renderers_rgb(input: &Path, page_index: u32, dpi: u32) -> Vec<Cmd> {
    let page = (page_index + 1).to_string();
    let file = input.to_string_lossy().into_owned();
    vec![
        Cmd {
            program: "pdftoppm".to_string(),
            args: vec![
                "-r".to_string(),
                dpi.to_string(),
                "-f".to_string(),
                page.clone(),
                "-l".to_string(),
                page.clone(),
                "-singlefile".to_string(),
                file.clone(),
                "{stem}".to_string(),
            ],
            prints: false,
        },
        Cmd {
            program: "gs".to_string(),
            args: vec![
                "-q".to_string(),
                "-dNOPAUSE".to_string(),
                "-dBATCH".to_string(),
                "-sDEVICE=ppmraw".to_string(),
                "-dTextAlphaBits=4".to_string(),
                "-dGraphicsAlphaBits=4".to_string(),
                format!("-r{dpi}"),
                format!("-dFirstPage={page}"),
                format!("-dLastPage={page}"),
                "-sOutputFile={out}".to_string(),
                file,
            ],
            prints: false,
        },
    ]
}

/// One page as raw RGB `(width, height, rgb)`, under the same bounds as [`page_png`].
pub fn page_rgb(path: &Path, page_index: u32, dpi: u32) -> Result<(u32, u32, Vec<u8>), String> {
    let ppm = page_raster(path, page_index, dpi, Limits::page(), None)?;
    ppm_to_rgb(&ppm)
}

/// [`page_png`] with caller-chosen caps and renderers; `None` asks the machine.
fn page_png_with(
    path: &Path,
    page_index: u32,
    dpi: u32,
    limits: Limits,
    given: Option<Vec<Cmd>>,
) -> Result<Vec<u8>, String> {
    checked(path)?;
    checked_dpi(dpi)?;
    // Checked here, not left to the tool: pdftoppm's "wrong page range" is about a
    // range, not this file.
    let pages = page_count(path)?;
    if page_index >= pages {
        return Err(page_refused(page_index, pages));
    }
    let out = temp_file("page", "png");
    let cmds: Vec<Cmd> = match given {
        Some(cmds) => cmds,
        None => renderers(path, page_index, dpi),
    }
    .into_iter()
    .filter(|c| on_path(&c.program))
    .collect();
    if cmds.is_empty() {
        let _ = std::fs::remove_file(&out);
        return Err(
            "no renderer found: neither pdftoppm nor gs is on PATH, and there is nothing \
             else to ask for a page"
                .to_string(),
        );
    }
    let mut last = String::new();
    for cmd in &cmds {
        match run(cmd, &out, limits) {
            Run::Done => {
                let bytes = read_out(&out, limits.output, "renderer");
                let _ = std::fs::remove_file(&out);
                let bytes = bytes?;
                if !bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
                    return Err(format!("{} wrote something that is not a PNG", cmd.program));
                }
                return Ok(bytes);
            }
            // The next candidate is asked, so a useless tool does not stand in the way.
            Run::Refused(e) => {
                last = e;
                let _ = std::fs::remove_file(&out);
            }
            // A deadline or a cap is not a refusal: the next tool would spend the same wait.
            Run::Stopped(e) => {
                let _ = std::fs::remove_file(&out);
                return Err(e);
            }
        }
    }
    let _ = std::fs::remove_file(&out);
    Err(last)
}

/// The whole document as markdown, in one `pdftotext -layout` call: a heading a page
/// and its laid-out text a fenced block, rendered at the reader's own font size.
pub fn document_markdown(path: &Path, title: &str) -> Result<String, String> {
    checked(path)?;
    let pages = page_count(path)?;
    let file = path.to_string_lossy().into_owned();
    let cmd = Cmd {
        program: "pdftotext".to_string(),
        // `-layout` keeps columns and tables; `-` writes to stdout, which `run` reads back.
        args: vec!["-layout".to_string(), file, "-".to_string()],
        prints: true,
    };
    if !on_path(&cmd.program) {
        return Err("no text extractor: pdftotext is not on PATH".to_string());
    }
    let out = temp_file("doc", "txt");
    let text = match run(&cmd, &out, Limits::text()) {
        Run::Done => read_out(&out, TEXT_OUTPUT_CAP, "pdftotext")
            .map(|b| String::from_utf8_lossy(&b).into_owned()),
        Run::Refused(e) | Run::Stopped(e) => Err(e),
    };
    let _ = std::fs::remove_file(&out);
    let text = text?;
    Ok(markdown_of(title, &text, pages))
}

fn markdown_of(title: &str, text: &str, pages: u32) -> String {
    let mut out = format!("# {title}\n");
    let mut seen = 0u32;
    for page in text.split('\u{c}') {
        seen += 1;
        // `pdftotext` ends the file with a form feed, which is not a page of its own.
        if seen > pages || (page.trim().is_empty() && seen == pages) {
            continue;
        }
        out.push_str(&format!("\n## Page {seen}\n\n```text\n"));
        out.push_str(page.trim_end_matches(['\n', '\r']));
        out.push_str("\n```\n");
    }
    out
}

/// [`page_rgb`] with caller-chosen caps and renderers: the bytes the renderer wrote.
fn page_raster(
    path: &Path,
    page_index: u32,
    dpi: u32,
    limits: Limits,
    given: Option<Vec<Cmd>>,
) -> Result<Vec<u8>, String> {
    checked(path)?;
    checked_dpi(dpi)?;
    let pages = page_count(path)?;
    if page_index >= pages {
        return Err(page_refused(page_index, pages));
    }
    let out = temp_file("page", "ppm");
    let cmds: Vec<Cmd> = match given {
        Some(cmds) => cmds,
        None => renderers_rgb(path, page_index, dpi),
    }
    .into_iter()
    .filter(|c| on_path(&c.program))
    .collect();
    if cmds.is_empty() {
        let _ = std::fs::remove_file(&out);
        return Err(
            "no renderer found: neither pdftoppm nor gs is on PATH, and there is nothing \
             else to ask for a page"
                .to_string(),
        );
    }
    let mut last = String::new();
    for cmd in &cmds {
        match run(cmd, &out, limits) {
            Run::Done => {
                let bytes = read_out(&out, limits.output, "renderer");
                let _ = std::fs::remove_file(&out);
                return bytes;
            }
            Run::Refused(e) => {
                last = e;
                let _ = std::fs::remove_file(&out);
            }
            Run::Stopped(e) => {
                let _ = std::fs::remove_file(&out);
                return Err(e);
            }
        }
    }
    let _ = std::fs::remove_file(&out);
    Err(last)
}

/// A P6 PPM as `(width, height, rgb)`; P3 and 16-bit `maxval` are refused by name.
fn ppm_to_rgb(bytes: &[u8]) -> Result<(u32, u32, Vec<u8>), String> {
    let mut fields: Vec<String> = Vec::new();
    let mut at = 0usize;
    while fields.len() < 4 && at < bytes.len() {
        // Whitespace between fields, comments (`#` to end of line) anywhere in the header.
        while at < bytes.len()
            && (bytes[at].is_ascii_whitespace()
                || (bytes[at] == b'#' && {
                    while at < bytes.len() && bytes[at] != b'\n' {
                        at += 1;
                    }
                    true
                }))
        {
            at += 1;
        }
        let start = at;
        while at < bytes.len() && !bytes[at].is_ascii_whitespace() {
            at += 1;
        }
        if start == at {
            break;
        }
        fields.push(String::from_utf8_lossy(&bytes[start..at]).into_owned());
    }
    if fields.len() < 4 || fields[0] != "P6" {
        return Err(format!(
            "not a binary PPM: {:?}",
            fields.first().map(String::as_str).unwrap_or("")
        ));
    }
    let width: u32 = fields[1]
        .parse()
        .map_err(|_| format!("width {:?}", fields[1]))?;
    let height: u32 = fields[2]
        .parse()
        .map_err(|_| format!("height {:?}", fields[2]))?;
    if fields[3] != "255" {
        return Err(format!(
            "a {}",
            if fields[3] == "65535" {
                "16-bit"
            } else {
                "non-8-bit"
            }
        ));
    }
    // One whitespace byte ends the header; everything after it is pixels.
    let want = (width as usize) * (height as usize) * 3;
    let data = bytes.get(at + 1..).unwrap_or_default();
    if data.len() < want {
        return Err(format!(
            "{width}x{height} pixels is {want} bytes, got {}",
            data.len()
        ));
    }
    Ok((width, height, data[..want].to_vec()))
}

/// [`page_text`] with the caps and the tool a caller chooses. `None` asks the machine.
fn page_text_with(
    path: &Path,
    page_index: u32,
    limits: Limits,
    tool: Option<Cmd>,
) -> Result<String, String> {
    checked(path)?;
    let pages = page_count(path)?;
    if page_index >= pages {
        return Err(page_refused(page_index, pages));
    }
    let page = (page_index + 1).to_string();
    let cmd = tool.unwrap_or_else(|| Cmd {
        program: "pdftotext".to_string(),
        args: vec![
            // `-layout` carries the paragraph breaks; without it the page is one wall of prose.
            "-layout".to_string(),
            "-f".to_string(),
            page.clone(),
            "-l".to_string(),
            page,
            path.to_string_lossy().into_owned(),
            "{out}".to_string(),
        ],
        prints: false,
    });
    if !on_path(&cmd.program) {
        return Err(format!(
            "no text extractor found: {} is not on PATH, and a page of text needs one",
            cmd.program
        ));
    }
    let out = temp_file("text", "txt");
    let result = match run(&cmd, &out, limits) {
        Run::Done => read_out(&out, limits.output, "extractor")
            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned()),
        Run::Refused(e) | Run::Stopped(e) => Err(e),
    };
    let _ = std::fs::remove_file(&out);
    result
}

/// The file, before any tool is asked: present, a file, and starting with `%PDF-`, so
/// a text file named `.pdf` is refused whether or not poppler is installed.
fn checked(path: &Path) -> Result<(), String> {
    use std::io::Read;
    let meta = std::fs::metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if !meta.is_file() {
        return Err(format!("{} is not a file", path.display()));
    }
    let mut head = Vec::with_capacity(1024);
    std::fs::File::open(path)
        .map_err(|e| format!("{}: {e}", path.display()))?
        .take(1024)
        .read_to_end(&mut head)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    if !head.windows(5).any(|w| w == b"%PDF-") {
        return Err(format!(
            "{} is not a PDF: the file does not start with %PDF-",
            path.display()
        ));
    }
    Ok(())
}

/// A dpi the caller may ask for. Refused, not clamped: a silent clamp draws a size
/// the caller never asked for.
fn checked_dpi(dpi: u32) -> Result<u32, String> {
    if !(MIN_DPI..=MAX_DPI).contains(&dpi) {
        return Err(format!(
            "dpi {dpi} is outside {MIN_DPI}..={MAX_DPI}: a page is rendered at this size \
             and then cut into cells, so the number is the sharpness asked for and not a \
             hint"
        ));
    }
    Ok(dpi)
}

fn page_refused(page_index: u32, pages: u32) -> String {
    if pages == 0 {
        return format!("page {page_index}: the document has no pages");
    }
    format!(
        "page {page_index}: the document has {pages} pages (pages are numbered from 0, so \
         the last is {})",
        pages - 1
    )
}

/// The count and which route said so; a `pdfinfo` that refuses (encrypted, broken)
/// falls through to the bytes.
fn count_with(path: &Path, pdfinfo: Option<Cmd>, limits: Limits) -> Result<(u32, Counted), String> {
    checked(path)?;
    let tool = match pdfinfo {
        Some(cmd) => Some(cmd),
        None if on_path("pdfinfo") => Some(Cmd {
            program: "pdfinfo".to_string(),
            args: vec![path.to_string_lossy().into_owned()],
            prints: true,
        }),
        None => None,
    };
    let mut refused = String::new();
    if let Some(cmd) = tool {
        let out = temp_file("count", "txt");
        match run(&cmd, &out, limits) {
            Run::Done => {
                let bytes = std::fs::read(&out).unwrap_or_default();
                let _ = std::fs::remove_file(&out);
                if let Some(pages) = pages_line(&bytes) {
                    return Ok((pages, Counted::PdfInfo));
                }
                refused = "pdfinfo printed no page count".to_string();
            }
            Run::Refused(e) => {
                let _ = std::fs::remove_file(&out);
                refused = e;
            }
            Run::Stopped(e) => {
                let _ = std::fs::remove_file(&out);
                return Err(e);
            }
        }
    }
    let bytes = read_for_parse(path)?;
    match count_from_bytes(&bytes) {
        Ok(pages) => Ok((pages, Counted::Parsed)),
        Err(e) => Err(if refused.is_empty() {
            format!("pdfinfo is not on PATH, and {e}")
        } else {
            format!("{refused}, and {e}")
        }),
    }
}

fn pages_line(bytes: &[u8]) -> Option<u32> {
    String::from_utf8_lossy(bytes)
        .lines()
        .find_map(|line| line.trim().strip_prefix("Pages:"))
        .and_then(|n| n.trim().parse().ok())
}

/// The document's bytes for the parse path, capped: the one place a whole PDF is held.
fn read_for_parse(path: &Path) -> Result<Vec<u8>, String> {
    let meta = std::fs::metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if meta.len() > MAX_PARSE_BYTES {
        return Err(format!(
            "{} is {}: too large to read the page tree out of (the cap is {})",
            path.display(),
            size(meta.len()),
            size(MAX_PARSE_BYTES)
        ));
    }
    std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))
}

/// The page count out of the bytes: the largest `/Count` on a `/Type /Pages` dictionary
/// (intermediate nodes count only their subtree), or the `/Type /Page` objects when the
/// tree is not in the clear.
fn count_from_bytes(bytes: &[u8]) -> Result<u32, String> {
    let mut best: Option<u32> = None;
    for at in find_all(bytes, b"/Type") {
        let after = skip_space(bytes, at + 5);
        if !bytes[after..].starts_with(b"/Pages") {
            continue;
        }
        // The enclosing dictionary, so `/Count` is found before or after `/Type`.
        let start = rfind(bytes, b"<<", at).unwrap_or(0);
        let end = find(bytes, b">>", after).unwrap_or(bytes.len());
        if let Some(n) = count_in(&bytes[start..end]) {
            best = Some(best.map_or(n, |b: u32| b.max(n)));
        }
    }
    if let Some(n) = best {
        return Ok(n);
    }
    let pages = count_page_objects(bytes);
    if pages > 0 {
        return Ok(pages);
    }
    Err(
        "the bytes have no page tree in the clear (a compressed object stream, or a file \
         that is not a PDF)"
            .to_string(),
    )
}

fn count_in(dict: &[u8]) -> Option<u32> {
    for at in find_all(dict, b"/Count") {
        let start = skip_space(dict, at + 6);
        let end = dict[start..]
            .iter()
            .position(|b| !b.is_ascii_digit())
            .map_or(dict.len(), |n| start + n);
        if let Ok(n) = std::str::from_utf8(&dict[start..end]).unwrap_or("").parse() {
            return Some(n);
        }
    }
    None
}

/// `/Type /Page` objects, counted as pages — the last resort when no `/Count` is in the
/// clear. `/Type /Pages` is not one: the character after `Page` has to end the name.
fn count_page_objects(bytes: &[u8]) -> u32 {
    find_all(bytes, b"/Type")
        .into_iter()
        .filter(|at| {
            let after = skip_space(bytes, at + 5);
            let rest = &bytes[after..];
            rest.starts_with(b"/Page") && !rest.get(5).is_some_and(|b| b.is_ascii_alphanumeric())
        })
        .count() as u32
}

fn find_all(hay: &[u8], pat: &[u8]) -> Vec<usize> {
    let mut at = Vec::new();
    let mut i = 0;
    while i + pat.len() <= hay.len() {
        if &hay[i..i + pat.len()] == pat {
            at.push(i);
            i += pat.len();
        } else {
            i += 1;
        }
    }
    at
}

fn find(hay: &[u8], pat: &[u8], from: usize) -> Option<usize> {
    find_all(&hay[from.min(hay.len())..], pat)
        .first()
        .map(|at| at + from.min(hay.len()))
}

fn rfind(hay: &[u8], pat: &[u8], to: usize) -> Option<usize> {
    find_all(&hay[..to.min(hay.len())], pat).pop()
}

fn skip_space(hay: &[u8], from: usize) -> usize {
    let mut i = from.min(hay.len());
    while i < hay.len() && matches!(hay[i], b' ' | b'\t' | b'\r' | b'\n' | b'\x0c' | b'\0') {
        i += 1;
    }
    i
}

enum Run {
    Done,
    Refused(String),
    Stopped(String),
}

/// Run one tool, watched: memory cap, file-size cap, deadline, sampled and killed as
/// it runs. No pipes and no shell — the answer goes to a file, so a pipe nobody
/// drains cannot block it.
fn run(cmd: &Cmd, out: &Path, limits: Limits) -> Run {
    let program = cmd.program.as_str();
    if program.is_empty() {
        return Run::Refused("the renderer command is empty".to_string());
    }
    let shown = out.to_string_lossy().into_owned();
    // The name without this call's extension; hard-coding one makes the tool write
    // `name.ppm.ppm`, which looks like a renderer that wrote nothing.
    let stem = std::path::Path::new(&shown)
        .with_extension("")
        .to_string_lossy()
        .into_owned();
    let mut child = std::process::Command::new(program);
    child
        .args(
            cmd.args
                .iter()
                .map(|a| a.replace("{out}", &shown).replace("{stem}", &stem)),
        )
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    if cmd.prints {
        match std::fs::File::create(out) {
            Ok(file) => {
                child.stdout(std::process::Stdio::from(file));
            }
            Err(e) => return Run::Refused(format!("{program}: {e}")),
        }
    } else {
        child.stdout(std::process::Stdio::null());
    }
    // The kernel's half, best effort: a platform that refuses still has the sampler
    // and the deadline.
    #[cfg(unix)]
    unsafe {
        use std::os::unix::process::CommandExt;
        child.pre_exec(move || {
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
            // CPU seconds, so a spinning renderer dies even if this process is not
            // scheduled to notice it.
            limit(libc::RLIMIT_CPU, limits.timeout.as_secs().max(1));
            Ok(())
        });
    }
    let mut child = match child.spawn() {
        Ok(c) => c,
        Err(e) => return Run::Refused(format!("{program}: {e}")),
    };
    let started = std::time::Instant::now();
    let mut sampled = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Run::Done,
            Ok(Some(status)) => {
                // A file at the cap is `RLIMIT_FSIZE` having fired, not a refusal.
                let wrote = std::fs::metadata(out).map(|m| m.len()).unwrap_or(0);
                if wrote >= limits.output {
                    return Run::Stopped(format!(
                        "{program} wrote past {} and was stopped: a page that big is one \
                         no terminal can show",
                        size(limits.output)
                    ));
                }
                return Run::Refused(format!(
                    "{program} could not read it ({})",
                    status
                        .code()
                        .map_or("killed".to_string(), |c| format!("exit {c}"))
                ));
            }
            Ok(None) => {}
            Err(e) => return Run::Refused(format!("{program}: {e}")),
        }
        // Sampled off the process itself: the kernel will not stop a runaway renderer
        // on every platform.
        if sampled.elapsed() >= std::time::Duration::from_millis(25) {
            sampled = std::time::Instant::now();
            if let Some(rss) = rss(child.id())
                && rss > limits.rss
            {
                stop(&mut child);
                return Run::Stopped(format!(
                    "{program} wanted {} (the cap is {})",
                    size(rss),
                    size(limits.rss)
                ));
            }
        }
        if started.elapsed() >= limits.timeout {
            stop(&mut child);
            return Run::Stopped(format!(
                "{program} took more than {:?}: a page that cannot be drawn in that time \
                 is not one to wait for",
                limits.timeout
            ));
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

/// Kill the child and reap it, so a timed-out one is not left running behind us.
fn stop(child: &mut std::process::Child) {
    let _ = child.kill();
    let _ = child.wait();
}

/// A process's resident memory in bytes, or `None` where the platform will not say.
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

/// Whether `program` is on PATH and executable; a name with a `/` is tried as a path.
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

/// What a tool wrote, refused when empty, past the cap or missing — `RLIMIT_FSIZE` is
/// best effort, so the cap is checked again here.
fn read_out(path: &Path, cap: u64, what: &str) -> Result<Vec<u8>, String> {
    let meta = std::fs::metadata(path).map_err(|e| format!("the {what} wrote no file: {e}"))?;
    if meta.len() == 0 {
        return Err(format!("the {what} wrote an empty file"));
    }
    if meta.len() > cap {
        return Err(format!(
            "the {what} wrote {} (the cap is {})",
            size(meta.len()),
            size(cap)
        ));
    }
    std::fs::read(path).map_err(|e| format!("the {what}'s file: {e}"))
}

fn size(bytes: u64) -> String {
    if bytes >= 1024 * 1024 && bytes.is_multiple_of(1024 * 1024) {
        format!("{} MiB", bytes / (1024 * 1024))
    } else if bytes >= 1024 {
        format!("{} KiB", bytes / 1024)
    } else {
        format!("{bytes} bytes")
    }
}

/// The temp file a render writes to: pid and counter, so two pages cannot collide.
fn temp_file(tag: &str, ext: &str) -> PathBuf {
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    std::env::temp_dir().join(format!("rano-pdf-{tag}-{}-{n}.{ext}", std::process::id()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A PDF built in the clear, one word a page: the parse path runs without poppler.
    pub(super) fn fixture(words: &[&str]) -> Vec<u8> {
        let font = 3 + words.len() as u32 * 2;
        let kids: Vec<String> = (0..words.len())
            .map(|i| format!("{} 0 R", 3 + i as u32 * 2))
            .collect();
        let mut body: Vec<Vec<u8>> = vec![
            b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
            format!(
                "<< /Type /Pages /Kids [{}] /Count {} >>",
                kids.join(" "),
                words.len()
            )
            .into_bytes(),
        ];
        for (i, word) in words.iter().enumerate() {
            let content = format!("BT /F1 24 Tf 20 60 Td ({word}) Tj ET\n");
            body.push(
                format!(
                    "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] /Contents {} 0 R \
                     /Resources << /Font << /F1 {font} 0 R >> >> >>",
                    4 + i as u32 * 2
                )
                .into_bytes(),
            );
            body.push(
                format!(
                    "<< /Length {} >>\nstream\n{content}endstream",
                    content.len()
                )
                .into_bytes(),
            );
        }
        body.push(b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_vec());

        let mut out = b"%PDF-1.4\n".to_vec();
        let mut offsets = Vec::new();
        for (i, object) in body.iter().enumerate() {
            offsets.push(out.len());
            out.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
            out.extend_from_slice(object);
            out.extend_from_slice(b"\nendobj\n");
        }
        let xref = out.len();
        out.extend_from_slice(format!("xref\n0 {}\n", body.len() + 1).as_bytes());
        out.extend_from_slice(b"0000000000 65535 f \n");
        for off in &offsets {
            out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
        }
        out.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
                body.len() + 1
            )
            .as_bytes(),
        );
        out
    }

    struct TempDir(PathBuf);
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn temp_dir(tag: &str) -> TempDir {
        let d = std::env::temp_dir().join(format!("rano_pdf_{}_{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        TempDir(d)
    }

    fn document(d: &TempDir, words: &[&str]) -> PathBuf {
        let path = d.0.join("doc.pdf");
        std::fs::write(&path, fixture(words)).unwrap();
        path
    }

    /// A PNG-shaped buffer: signature and a real IHDR. Nothing here decodes one.
    fn png_header(w: u32, h: u32) -> Vec<u8> {
        let mut v = b"\x89PNG\r\n\x1a\n".to_vec();
        v.extend_from_slice(&13u32.to_be_bytes());
        v.extend_from_slice(b"IHDR");
        v.extend_from_slice(&w.to_be_bytes());
        v.extend_from_slice(&h.to_be_bytes());
        v.extend_from_slice(&[8, 2, 0, 0, 0, 0, 0, 0, 0]);
        v
    }

    fn ihdr(bytes: &[u8]) -> (u32, u32) {
        let be = |at: usize| {
            u32::from_be_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
        };
        (be(16), be(20))
    }

    /// A stand-in tool: a shell script, so guards are watched without poppler or gs.
    fn script(d: &TempDir, name: &str, body: &str) -> String {
        use std::os::unix::fs::PermissionsExt;
        let path = d.0.join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path.to_string_lossy().into_owned()
    }

    fn stand_in(program: &str, args: &[&str]) -> Cmd {
        Cmd {
            program: program.to_string(),
            args: args.iter().map(|a| a.to_string()).collect(),
            prints: false,
        }
    }

    fn after<'a>(cmd: &'a Cmd, flag: &str) -> Option<&'a str> {
        let at = cmd.args.iter().position(|a| a == flag)?;
        cmd.args.get(at + 1).map(String::as_str)
    }

    /// The page tree is read in either order; the page objects answer when it is not.
    #[test]
    fn the_page_tree_is_read_in_either_order() {
        assert_eq!(
            count_from_bytes(b"<< /Type /Pages /Count 2 /Kids [] >>").unwrap(),
            2
        );
        assert_eq!(
            count_from_bytes(b"<< /Count 4 /Type /Pages /Kids [3 0 R] >>").unwrap(),
            4
        );
        // A subtree's own count is not the document's: the largest is.
        assert_eq!(
            count_from_bytes(
                b"<< /Type /Pages /Count 3 /Kids [4 0 R] >>\n\
                  4 0 obj << /Type /Pages /Count 1 /Kids [5 0 R] >> endobj"
            )
            .unwrap(),
            3
        );
        // No page tree in the clear: the page objects are counted instead.
        assert_eq!(
            count_from_bytes(
                b"1 0 obj << /Type /Page /Parent 2 0 R >> endobj\n\
                  2 0 obj << /Type /Page /Parent 3 0 R >> endobj"
            )
            .unwrap(),
            2
        );
        // `/Type /Pages` is not a page, and neither is a name that starts with one.
        let e =
            count_from_bytes(b"<< /Type /Pages /Kids [3 0 R] >> << /Type /PageX >>").unwrap_err();
        assert!(e.contains("no page tree"), "{e}");
        let e = count_from_bytes(b"not a pdf at all").unwrap_err();
        assert!(e.contains("no page tree"), "{e}");
        assert_eq!(count_from_bytes(&fixture(&["alpha"])).unwrap(), 1);
        assert_eq!(
            count_from_bytes(&fixture(&["alpha", "bravo", "charlie"])).unwrap(),
            3
        );
    }

    /// The count is right and names the route that answered, whatever the machine has.
    #[test]
    fn the_count_is_right_and_names_the_route_that_answered() {
        let d = temp_dir("count");
        let pdf = document(&d, &["alpha", "bravo", "charlie"]);
        assert_eq!(page_count(&pdf).unwrap(), 3);
        let which = last_count().expect("a route");
        assert_eq!(
            which,
            if on_path("pdfinfo") {
                Counted::PdfInfo
            } else {
                Counted::Parsed
            },
            "the route is the one this machine has"
        );
        assert_eq!(count_with(&pdf, None, Limits::text()).unwrap().0, 3);
        // One page is one page, so nothing here is a constant.
        let other = temp_dir("count-one");
        let one = document(&other, &["only"]);
        assert_eq!(count_with(&one, None, Limits::text()).unwrap().0, 1);
    }

    /// `pdfinfo` is preferred; one that cannot read the file falls through to the bytes.
    #[test]
    fn pdfinfo_is_preferred_and_the_bytes_answer_when_it_cannot() {
        let d = temp_dir("prefer");
        let pdf = document(&d, &["alpha", "bravo", "charlie"]);
        let prints = script(&d, "pdfinfo-yes.sh", "printf 'Pages:          7\\n'");
        let says = count_with(
            &pdf,
            Some(Cmd {
                program: prints,
                args: vec![pdf.to_string_lossy().into_owned()],
                prints: true,
            }),
            Limits::text(),
        )
        .unwrap();
        assert_eq!(says, (7, Counted::PdfInfo));

        let refuses = script(&d, "pdfinfo-no.sh", "exit 1");
        let fell_through = count_with(&pdf, Some(stand_in(&refuses, &[])), Limits::text()).unwrap();
        assert_eq!(fell_through, (3, Counted::Parsed));
        // And when both routes fail, the sentence carries both reasons.
        let broken = d.0.join("broken.pdf");
        std::fs::write(&broken, b"%PDF-1.4\nnothing in here\n").unwrap();
        let e = count_with(&broken, Some(stand_in(&refuses, &[])), Limits::text()).unwrap_err();
        assert!(
            e.contains("pdfinfo-no.sh") && e.contains("no page tree"),
            "{e}"
        );
    }

    /// One page is asked for, never the document; the argv is read back, installed or not.
    #[test]
    fn one_page_is_asked_for_and_never_the_document() {
        let path = Path::new("/d/a b.pdf");
        let first = renderers(path, 0, 150);
        let second = renderers(path, 1, 300);
        assert_eq!(first.len(), 2, "pdftoppm and gs");
        assert_eq!(second.len(), 2);

        let pdftoppm = first
            .iter()
            .find(|c| c.program == "pdftoppm")
            .expect("pdftoppm");
        assert_eq!(after(pdftoppm, "-f"), Some("1"));
        assert_eq!(after(pdftoppm, "-l"), Some("1"));
        assert_eq!(after(pdftoppm, "-r"), Some("150"));
        assert!(pdftoppm.args.iter().any(|a| a == "-singlefile"));
        assert!(
            pdftoppm.args.iter().any(|a| a == "/d/a b.pdf"),
            "the path is one argument, spaces and all"
        );
        assert!(pdftoppm.args.iter().any(|a| a == "{stem}"));
        let gs = first.iter().find(|c| c.program == "gs").expect("gs");
        assert!(gs.args.iter().any(|a| a == "-dFirstPage=1"));
        assert!(gs.args.iter().any(|a| a == "-dLastPage=1"));
        assert!(gs.args.iter().any(|a| a == "-sOutputFile={out}"));
        assert!(gs.args.iter().any(|a| a == "-r150"));

        // The page index reaches the tools, one-based, and the dpi with it.
        let pdftoppm = second
            .iter()
            .find(|c| c.program == "pdftoppm")
            .expect("pdftoppm");
        assert_eq!(after(pdftoppm, "-f"), Some("2"));
        assert_eq!(after(pdftoppm, "-l"), Some("2"));
        assert_eq!(after(pdftoppm, "-r"), Some("300"));
        let gs = second.iter().find(|c| c.program == "gs").expect("gs");
        assert!(gs.args.iter().any(|a| a == "-dFirstPage=2"));
        assert!(gs.args.iter().any(|a| a == "-dLastPage=2"));
    }

    /// The page asked for is the page written: each stand-in page draws a different size.
    #[test]
    fn the_page_asked_for_is_the_page_written() {
        let d = temp_dir("pages");
        let pdf = document(&d, &["alpha", "bravo", "charlie"]);
        std::fs::write(d.0.join("p1.png"), png_header(10, 10)).unwrap();
        std::fs::write(d.0.join("p2.png"), png_header(20, 20)).unwrap();
        let sh = script(&d, "pages.sh", "cp \"$(dirname \"$0\")/p$1.png\" \"$2\"");

        let one = page_png_with(
            &pdf,
            0,
            150,
            Limits::page(),
            Some(vec![stand_in(&sh, &["1", "{out}"])]),
        )
        .expect("page 1");
        let two = page_png_with(
            &pdf,
            1,
            150,
            Limits::page(),
            Some(vec![stand_in(&sh, &["2", "{out}"])]),
        )
        .expect("page 2");
        assert_eq!(ihdr(&one), (10, 10));
        assert_eq!(ihdr(&two), (20, 20), "page 2's own picture, not page 1's");
        assert_ne!(one, two);
    }

    /// The text of a page is that page's text.
    #[test]
    fn the_text_of_a_page_is_that_pages_text() {
        let d = temp_dir("text");
        let pdf = document(&d, &["alpha", "bravo", "charlie"]);
        let sh = script(&d, "text.sh", "printf 'word on page %s\\n' \"$1\" > \"$2\"");
        let one = page_text_with(
            &pdf,
            0,
            Limits::text(),
            Some(stand_in(&sh, &["1", "{out}"])),
        )
        .expect("page 1");
        let three = page_text_with(
            &pdf,
            2,
            Limits::text(),
            Some(stand_in(&sh, &["3", "{out}"])),
        )
        .expect("page 3");
        assert!(one.contains("page 1"), "{one}");
        assert!(three.contains("page 3"), "{three}");
        // A machine with no extractor says so rather than returning nothing.
        let e = page_text_with(
            &pdf,
            0,
            Limits::text(),
            Some(stand_in("definitely-not-a-program-xyz", &["{out}"])),
        )
        .unwrap_err();
        assert!(e.contains("is not on PATH"), "{e}");
    }

    /// An index past the last page is refused with the count, before any tool is asked.
    #[test]
    fn a_page_past_the_last_is_refused_with_the_count() {
        let d = temp_dir("range");
        let pdf = document(&d, &["alpha", "bravo", "charlie"]);
        let e = page_png(&pdf, 3, 150).unwrap_err();
        assert!(e.contains("page 3") && e.contains("3 pages"), "{e}");
        assert!(e.contains("the last is 2"), "{e}");
        let e = page_text(&pdf, 99).unwrap_err();
        assert!(e.contains("page 99") && e.contains("3 pages"), "{e}");
        // An empty document has no page at all, and says that rather than "0 pages".
        let empty = d.0.join("empty.pdf");
        std::fs::write(
            &empty,
            b"%PDF-1.4\n1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj\n\
              2 0 obj << /Type /Pages /Kids [] /Count 0 >> endobj\ntrailer\n<< /Root 1 0 R >>\n%%EOF\n",
        )
        .unwrap();
        let e = page_png(&empty, 0, 150).unwrap_err();
        assert!(e.contains("no pages"), "{e}");
    }

    /// A dpi outside the range is refused, not clamped.
    #[test]
    fn a_dpi_outside_the_range_is_refused() {
        let d = temp_dir("dpi");
        let pdf = document(&d, &["alpha", "bravo", "charlie"]);
        for dpi in [0, 1, MIN_DPI - 1, MAX_DPI + 1, 1200] {
            let e = page_png(&pdf, 0, dpi).unwrap_err();
            assert!(
                e.contains(&format!("dpi {dpi}")) && e.contains("is outside"),
                "{e}"
            );
        }
        // The ends themselves are allowed, and checked before any tool is asked.
        assert_eq!(checked_dpi(MIN_DPI), Ok(MIN_DPI));
        assert_eq!(checked_dpi(MAX_DPI), Ok(MAX_DPI));
        assert_eq!(checked_dpi(150), Ok(150));
    }

    /// A hanging renderer is killed by the deadline, not a frozen editor.
    #[test]
    fn a_renderer_that_hangs_is_stopped_by_the_clock() {
        let d = temp_dir("hang");
        let pdf = document(&d, &["alpha", "bravo", "charlie"]);
        let sh = script(&d, "slow.sh", "sleep 30");
        let caps = Limits {
            timeout: std::time::Duration::from_millis(300),
            ..Limits::page()
        };
        let started = std::time::Instant::now();
        let e =
            page_png_with(&pdf, 0, 150, caps, Some(vec![stand_in(&sh, &["{out}"])])).unwrap_err();
        assert!(e.contains("took more than"), "{e}");
        assert!(
            started.elapsed() < std::time::Duration::from_secs(10),
            "the deadline was the guard, not the sleep: {:?}",
            started.elapsed()
        );
    }

    /// A renderer writing past the cap is stopped, not truncated.
    #[test]
    fn a_renderer_that_writes_past_the_cap_is_stopped() {
        let d = temp_dir("big");
        let pdf = document(&d, &["alpha", "bravo", "charlie"]);
        let sh = script(
            &d,
            "big.sh",
            "dd if=/dev/zero of=\"$1\" bs=1024 count=8192 2>/dev/null",
        );
        let caps = Limits {
            output: 64 * 1024,
            ..Limits::page()
        };
        let r = page_png_with(&pdf, 0, 150, caps, Some(vec![stand_in(&sh, &["{out}"])]));
        let e = r.expect_err("a renderer that writes past the cap must not hand back a page");
        // Which of the guards says so is the OS's business; asserting one spelling
        // made this fail about one run in ten.
        assert!(
            e.contains("was stopped") || e.contains("could not read it") || e.contains("not a PNG"),
            "{e}"
        );
    }

    /// A renderer that goes for memory is stopped by the sampler; the kernel's cap is
    /// lifted out of the way for this one.
    #[test]
    fn a_renderer_that_runs_away_with_memory_is_stopped() {
        let d = temp_dir("fat");
        let pdf = document(&d, &["alpha", "bravo", "charlie"]);
        // A shell holding 8 MB in a variable, via tools both platforms have.
        let sh = script(
            &d,
            "fat.sh",
            "x=$(dd if=/dev/zero bs=1024 count=8192 2>/dev/null | tr '\\0' x); sleep 5",
        );
        let caps = Limits {
            rss: 2 * 1024 * 1024,
            address_space: 64 * 1024 * 1024 * 1024,
            timeout: std::time::Duration::from_secs(20),
            ..Limits::page()
        };
        let e =
            page_png_with(&pdf, 0, 150, caps, Some(vec![stand_in(&sh, &["{out}"])])).unwrap_err();
        assert!(e.contains("(the cap is 2 MiB)"), "{e}");
    }

    /// A refusing renderer is passed over for the next; when all refuse, the last is named.
    #[test]
    fn a_renderer_that_refuses_is_passed_over() {
        let d = temp_dir("fallback");
        let pdf = document(&d, &["alpha", "bravo", "charlie"]);
        std::fs::write(d.0.join("p1.png"), png_header(12, 8)).unwrap();
        let refuses = script(&d, "refuses.sh", "exit 3");
        let draws = script(&d, "draws.sh", "cp \"$(dirname \"$0\")/p1.png\" \"$1\"");
        let bytes = page_png_with(
            &pdf,
            0,
            150,
            Limits::page(),
            Some(vec![
                stand_in(&refuses, &["{out}"]),
                stand_in(&draws, &["{out}"]),
            ]),
        )
        .expect("the second one drew it");
        assert_eq!(ihdr(&bytes), (12, 8));

        let e = page_png_with(
            &pdf,
            0,
            150,
            Limits::page(),
            Some(vec![
                stand_in(&refuses, &["{out}"]),
                stand_in(&refuses, &["{out}"]),
            ]),
        )
        .unwrap_err();
        assert!(
            e.contains("refuses.sh") && e.contains("could not read it"),
            "{e}"
        );
        // A renderer that writes something that is not a picture is refused by name.
        let junk = script(&d, "junk.sh", "printf 'not a png' > \"$1\"");
        let e = page_png_with(
            &pdf,
            0,
            150,
            Limits::page(),
            Some(vec![stand_in(&junk, &["{out}"])]),
        )
        .unwrap_err();
        assert!(e.contains("not a PNG"), "{e}");
    }

    /// A file that is not a PDF is refused by all three; no tool is asked.
    #[test]
    fn a_file_that_is_not_a_pdf_is_refused_by_all_three() {
        let d = temp_dir("notpdf");
        let f = d.0.join("notes.pdf");
        std::fs::write(&f, b"hello, this is not a pdf at all\n").unwrap();
        let errors = [
            page_count(&f).unwrap_err(),
            page_png(&f, 0, 150).unwrap_err(),
            page_text(&f, 0).unwrap_err(),
        ];
        for e in &errors {
            assert!(e.contains("not a PDF"), "{e}");
            assert!(e.contains("notes.pdf"), "{e}");
        }
        // What is not there is named, and a directory is not a document.
        let e = page_count(&d.0.join("missing.pdf")).unwrap_err();
        assert!(e.contains("missing.pdf"), "{e}");
        let e = page_count(&d.0).unwrap_err();
        assert!(e.contains("is not a file"), "{e}");
        // An empty file is not one either, and the answer is the same sentence.
        let empty = d.0.join("empty.pdf");
        std::fs::write(&empty, b"").unwrap();
        assert!(page_count(&empty).unwrap_err().contains("not a PDF"));
    }

    /// The sampler answers on rano's platforms and says nothing about one it cannot see.
    #[test]
    fn the_memory_sampler_reads_a_process_it_can_see() {
        let me = rss(std::process::id()).expect("this process's own memory");
        assert!(me > 1024 * 1024, "{me} bytes of RSS for a test binary");
        assert_eq!(rss(u32::MAX), None, "a pid that is not a process");
    }

    /// The real thing, needs `pdftoppm`: `cargo test -- --ignored a_real_renderer`.
    /// Page two is not page one, and the size is the dpi that was asked for.
    #[test]
    #[ignore = "needs pdftoppm on PATH (poppler)"]
    fn a_real_renderer_draws_the_page_it_was_asked_for() {
        assert!(
            on_path("pdftoppm"),
            "no pdftoppm on this machine: nothing to measure"
        );
        let d = temp_dir("real");
        let pdf = document(&d, &["alpha", "bravo", "charlie"]);
        let first = page_png(&pdf, 0, 150).expect("page 1");
        let second = page_png(&pdf, 1, 150).expect("page 2");
        assert_eq!(
            first[..8],
            b"\x89PNG\r\n\x1a\n"[..],
            "a PNG, whatever wrote it"
        );
        assert_ne!(first, second, "the page index reached the renderer");
        // 300×200 pt at 150 dpi is 625×417 px: the dpi reached it too.
        assert_eq!(ihdr(&second), (625, 417));
        // And the whole document was not rendered: one page, one file, gone.
        let leftovers: Vec<String> = std::fs::read_dir(std::env::temp_dir())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with(&format!("rano-pdf-page-{}", std::process::id())))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
    }

    /// The real text of a page, needs `pdftotext`: `cargo test -- --ignored a_real_page_of_text`.
    #[test]
    #[ignore = "needs pdftotext on PATH (poppler)"]
    fn a_real_page_of_text_carries_its_word() {
        assert!(
            on_path("pdftotext"),
            "no pdftotext on this machine: nothing to measure"
        );
        let d = temp_dir("realtext");
        let pdf = document(&d, &["alpha", "bravo", "charlie"]);
        let first = page_text(&pdf, 0).expect("page 1");
        let second = page_text(&pdf, 1).expect("page 2");
        assert!(first.contains("alpha"), "{first}");
        assert!(second.contains("bravo"), "{second}");
        assert!(
            !second.contains("alpha"),
            "one page, not the document: {second}"
        );
    }

    /// Ghostscript, asked for by name so the primary renderer is not what answers:
    /// `cargo test -- --ignored a_real_page_comes_back_from_ghostscript`.
    #[test]
    #[ignore = "needs gs on PATH (ghostscript), and it is only the fallback"]
    fn a_real_page_comes_back_from_ghostscript() {
        assert!(on_path("gs"), "no gs on this machine: nothing to measure");
        let d = temp_dir("realgs");
        let pdf = document(&d, &["alpha", "bravo", "charlie"]);
        let gs: Vec<Cmd> = renderers(&pdf, 1, 150)
            .into_iter()
            .filter(|c| c.program == "gs")
            .collect();
        let bytes = page_png_with(&pdf, 1, 150, Limits::page(), Some(gs)).expect("gs drew page 2");
        assert_eq!(bytes[..8], b"\x89PNG\r\n\x1a\n"[..]);
        assert_eq!(ihdr(&bytes), (625, 417));
    }
}

#[cfg(test)]
mod ppm_tests {
    use super::*;

    fn ppm(w: u32, h: u32) -> Vec<u8> {
        let mut v = format!("P6\n{w} {h}\n255\n").into_bytes();
        v.extend(std::iter::repeat_n(7u8, (w * h * 3) as usize));
        v
    }

    #[test]
    fn a_ppm_is_its_header_and_its_pixels() {
        let (w, h, rgb) = ppm_to_rgb(&ppm(4, 3)).expect("a P6");
        assert_eq!((w, h), (4, 3));
        assert_eq!(rgb.len(), 4 * 3 * 3);
        assert!(rgb.iter().all(|b| *b == 7));
    }

    /// A comment may sit anywhere whitespace may.
    #[test]
    fn comments_in_the_header_are_skipped() {
        let mut v = b"P6\n# written by gs\n2 2\n# another\n255\n".to_vec();
        v.extend([1u8; 12]);
        let (w, h, rgb) = ppm_to_rgb(&v).expect("a commented P6");
        assert_eq!((w, h), (2, 2));
        assert_eq!(rgb.len(), 12);
    }

    /// ASCII `P3`, 16-bit rasters and short pixel runs are refused by name.
    #[test]
    fn what_is_not_a_picture_the_terminal_takes_is_refused() {
        let e = ppm_to_rgb(b"P3\n1 1\n255\n255 0 0\n").unwrap_err();
        assert!(e.contains("not a binary PPM"), "{e}");
        let e = ppm_to_rgb(b"P6\n1 1\n65535\n\0\0\0\0\0\0").unwrap_err();
        assert!(e.contains("16-bit"), "{e}");
        let mut short = ppm(2, 2);
        short.pop();
        let e = ppm_to_rgb(&short).unwrap_err();
        assert!(e.contains("2x2") && e.contains("11"), "{e}");
        assert!(ppm_to_rgb(b"").is_err());
    }

    /// The RGB renderers ask for PPM, not PNG; the argv is read back, not trusted.
    #[test]
    fn the_rgb_renderers_ask_for_ppm() {
        let cmds = renderers_rgb(Path::new("/tmp/a.pdf"), 2, 96);
        let pdftoppm = &cmds[0];
        assert_eq!(pdftoppm.program, "pdftoppm");
        assert!(
            !pdftoppm.args.iter().any(|a| a == "-png"),
            "{:?}",
            pdftoppm.args
        );
        assert_eq!(
            pdftoppm.args.iter().filter(|a| **a == "-f").count(),
            1,
            "one page asked for: {:?}",
            pdftoppm.args
        );
        let gs = &cmds[1];
        assert!(
            gs.args.iter().any(|a| a == "-sDEVICE=ppmraw"),
            "{:?}",
            gs.args
        );
    }

    /// The real thing, where a renderer is installed.
    #[test]
    #[ignore = "needs pdftoppm or gs on PATH"]
    fn a_page_renders_as_pixels_that_match_their_size() {
        let dir = std::env::temp_dir().join(format!("rano-pdf-rgb-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let path = dir.join("two.pdf");
        std::fs::write(&path, super::tests::fixture(&["alpha", "bravo"])).expect("write");
        let (w, h, rgb) = page_rgb(&path, 0, 72).expect("page 1");
        assert_eq!(rgb.len(), (w as usize) * (h as usize) * 3);
        let (w2, h2, other) = page_rgb(&path, 1, 72).expect("page 2");
        assert_eq!((w, h), (w2, h2));
        assert_ne!(rgb, other, "page 2 is not page 1");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
