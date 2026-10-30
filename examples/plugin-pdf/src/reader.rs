//! **The reader**: which page, at what zoom, as a picture or as text — and the keys that move it.
//!
//! # Where the state lives, and why
//!
//! The ABI hands a provider a path and no session, and `plans/S10-wasm-hooks.md` §2.1 is explicit
//! that a fresh instance per call implies no guest state between calls. So the reader's whole
//! state lives **in the path**, as a fragment:
//!
//! ```text
//! /home/me/paper.pdf#p=4&z=144&m=text
//! ```
//!
//! `p` is the page (1-based), `z` the zoom in dpi, `m` the mode (`text`, or `auto` and absent for
//! the default). Everything in this module is then a pure function of that string: parse, move,
//! draw. Three consequences, all of them wanted:
//!
//! - **A key is a state transition that answers the next path** ([`key`]), so the plugin owns the
//!   experience without a key hook in the ABI: the host calls `on_key` with `"<path>\n<key>"` and
//!   opens the path it answers. A reader with no such wiring types the same fragment.
//! - **Nothing is remembered wrongly**: two panes on the same document cannot disagree about which
//!   page they are on, because neither remembers anything.
//! - **A path that is not a fragment is a file.** A document literally called `a#b.pdf` opens as
//!   page 1: the suffix only counts as state if every `k=v` in it is one this reader wrote.
//!
//! # What the plugin cannot know
//!
//! The number of pages, and the size of a page. The ABI's two imports take a page number and
//! answer a page (or nothing), so a document's extent is discovered by asking past the end and
//! being refused — which is why [`missing_page`] is a sentence in the view and not an error: the
//! reader sees "no page 13" and presses `p`. Printing "4/12" would need one more thing from the
//! host, and it is named in `docs/pdf.md` as the first thing to add.
//!
//! # What a page is, as it arrives
//!
//! **Pixels, not a file.** The host renders the page it was asked about and writes
//! `[width u32 LE][height u32 LE][rgb…]` into the buffer this plugin offers — the same bytes
//! `provider_picture` answers with, so a page is never decoded, converted or copied on the way
//! through. What that costs is a copy and the host's raster maths rather than an inflate, which is
//! why the bound on a page is the ABI's own ([`PAGE_BYTES`]) and not the hook's fuel budget.

use crate::cache;

/// The zoom a document opens at: 96 dpi, the width of a CSS inch.
pub const DEFAULT_DPI: u32 = 96;

/// The zoom's floor and ceiling. Below the floor a page is a smudge; above the ceiling it is more
/// pixels than one answer may carry for any page worth reading.
pub const MIN_DPI: u32 = 48;
pub const MAX_DPI: u32 = 300;

/// **The dpi the render ladder may fall to** when a page is too big to carry. It is below
/// [`MIN_DPI`] because it is not a zoom the reader asked for: it is the size at which a page always
/// fits, and a small picture of the whole page beats no picture at all. It is also the one question
/// a refusal cannot answer, so it is what tells a page that is too big from a page that is not
/// there.
/// The dpi a too-big page is asked at first: low enough that anything which exists fits the
/// answer's cap, and **not below the host's floor** — a page asked at 12 dpi is refused for the
/// dpi, which reads as "no such page" and sends the reader down the wrong branch.
pub const FIT_DPI: u32 = 24;

/// **The most one picture answer may carry**: the ABI's own cap, `MAX_HOOK_BYTES` in
/// `src/plugin.rs` — 16 MiB on the whole payload, the 8-byte `[width][height]` header included.
///
/// It used to be 512 KiB, and the binding constraint was the guest's **fuel**, not memory:
/// decoding a PNG cost ~61 units per RGB byte against a hook's 50 M. The host now hands over the
/// pixels it had anyway, so a page costs a copy and the host's raster maths, and what binds it is
/// the ABI. The arithmetic for the page a reader is most likely to open: A4 is 8.27×11.69 in, so a
/// page is `8.27 × 11.69 × 3 × dpi²` bytes — **6.2 MiB at 150 dpi**, and the cap is reached at
/// **240 dpi**. A page asked for above that is re-rendered at the dpi that fits rather than
/// refused.
pub const PAGE_BYTES: usize = 16 << 20;

/// **The hint line**, wrapped to the viewport like everything else. Every key the reader has is
/// named here, which is the rule the plugin dired set: a plugin view that hides its keys is a
/// plugin view nobody uses. `q` is the host's (it closes the view), and it is listed with the rest
/// because the reader does not care which side of the ABI a key lives on.
pub const HINT: &str =
    "n next · p prev · g<N> goto · s spread · z/Z zoom · 0 reset · t text · q close";

/// What a build with neither import can say. A module that imported what the host cannot provide
/// would not instantiate at all, so this sentence is the whole reason the imports are optional.
const NO_RENDERER: &str = "this build of the plugin has no page renderer. It needs two host \
imports, rano.convert_page (a page as a picture) and rano.convert_page_text (a page as text), and \
this build was compiled without them. The file was still claimed, so a PDF does not open as binary \
noise: the page is simply not there.";

/// A page that exists but carries no text: a scan, or a page of pictures.
const NO_TEXT_LAYER: &str = "this page has no text layer (a scan, or a page of pictures): the \
picture is the page. Press t for the picture.";

/// A page the host drew and the plugin has no text for.
const DRAWN_BY_HOST: &str = "the page is drawn by the host. This build has no text renderer, so \
there is nothing here to search or copy.";

/// The mode the reader is in. `Auto` is the default and means "the picture when this build can
/// draw one, and the page's text in the buffer either way".
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    #[default]
    Auto,
    Text,
}

/// Which page, at what zoom, in which mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct State {
    pub page: u32,
    pub dpi: u32,
    pub mode: Mode,
    /// **Two pages side by side**, which is what a wide pane is for.
    pub spread: bool,
}

impl Default for State {
    fn default() -> State {
        State {
            page: 1,
            dpi: DEFAULT_DPI,
            mode: Mode::Auto,
            spread: false,
        }
    }
}

impl State {
    /// One page on. Past the last page the view says so; the state does not know where that is.
    pub fn next(self) -> State {
        State {
            page: self.page.saturating_add(1),
            ..self
        }
    }

    /// One page back, and never to page 0: the first page is the wall.
    pub fn prev(self) -> State {
        State {
            page: self.page.saturating_sub(1).max(1),
            ..self
        }
    }

    /// The page number a reader asked for. `0` is not a page, and the caller refuses it.
    pub fn goto(self, page: u32) -> State {
        State {
            page: page.max(1),
            ..self
        }
    }

    /// Sharper: ×1.5 a step, in integers so the same key gives the same dpi everywhere.
    pub fn zoom_in(self) -> State {
        State {
            dpi: (self.dpi.saturating_mul(3) / 2).min(MAX_DPI),
            ..self
        }
    }

    /// Smaller: ÷1.5 a step, down to [`MIN_DPI`].
    pub fn zoom_out(self) -> State {
        State {
            dpi: (self.dpi.saturating_mul(2) / 3).max(MIN_DPI),
            ..self
        }
    }

    /// Back to the zoom the document opened at.
    pub fn reset_zoom(self) -> State {
        State {
            dpi: DEFAULT_DPI,
            ..self
        }
    }

    /// The page's text, or the picture: the one key that changes what the view *is*.
    pub fn toggle_text(self) -> State {
        State {
            mode: match self.mode {
                Mode::Text => Mode::Auto,
                Mode::Auto => Mode::Text,
            },
            ..self
        }
    }

    /// Two pages, or one.
    pub fn toggle_spread(self) -> State {
        State {
            spread: !self.spread,
            ..self
        }
    }

    /// **The fragment this state is written as**, with everything at its default left out so a
    /// path stays short: `""`, `#p=4`, `#p=4&z=144`, `#m=text`.
    pub fn fragment(self) -> String {
        let mut parts: Vec<String> = Vec::new();
        if self.page != 1 {
            parts.push(format!("p={}", self.page));
        }
        if self.dpi != DEFAULT_DPI {
            parts.push(format!("z={}", self.dpi));
        }
        if self.mode != Mode::Auto {
            parts.push("m=text".to_string());
        }
        if self.spread {
            parts.push("s=2".to_string());
        }
        if parts.is_empty() {
            String::new()
        } else {
            format!("#{}", parts.join("&"))
        }
    }
}

/// **A path, and the state its fragment asks for.**
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    /// The path with the fragment taken off: what the file is.
    pub path: String,
    pub state: State,
}

impl Request {
    /// **Parse `path[#p=N&z=DPI&m=text]`.** A suffix that is not exactly one of those keys with a
    /// value this reader would have written is part of the file's name, not state: `a#b.pdf` is a
    /// file, and so is `a.pdf#p=x`.
    pub fn parse(text: &str) -> Request {
        let text = text.trim();
        let plain = |path: &str| Request {
            path: path.to_string(),
            state: State::default(),
        };
        let Some((path, fragment)) = text.rsplit_once('#') else {
            return plain(text);
        };
        if path.is_empty() || fragment.is_empty() {
            return plain(text);
        }
        let mut state = State::default();
        for part in fragment.split('&') {
            let Some((key, value)) = part.split_once('=') else {
                return plain(text);
            };
            match key {
                "p" => match value.parse::<u32>() {
                    Ok(n) if n >= 1 => state.page = n,
                    _ => return plain(text),
                },
                "z" => match value.parse::<u32>() {
                    Ok(n) => state.dpi = n.clamp(MIN_DPI, MAX_DPI),
                    _ => return plain(text),
                },
                "m" if value == "text" => state.mode = Mode::Text,
                "m" if value == "auto" => state.mode = Mode::Auto,
                // Only the two pages this reader can show. Any other count is not state.
                "s" if value == "2" => state.spread = true,
                "s" if value == "1" => state.spread = false,
                _ => return plain(text),
            }
        }
        Request {
            path: path.to_string(),
            state,
        }
    }

    /// **Whether this is a document this plugin opens.** The extension is the whole test: a
    /// provider is asked about every path rano opens, and guessing from the first bytes would mean
    /// reading the file, which a plugin may not do.
    pub fn claim(&self) -> bool {
        std::path::Path::new(&self.path)
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("pdf"))
    }

    /// The name to show: the file's own, not the whole path.
    pub fn name(&self) -> String {
        std::path::Path::new(&self.path)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.path.clone())
    }
}

/// **The host's two imports, as this plugin sees them.** Everything above this line is a pure
/// function; this trait is the only seam that is not, which is what lets the reader be tested on
/// the host with no wasm, no PDF and no converter.
pub trait Render {
    /// Whether a page can come back as a picture.
    fn can_picture(&self) -> bool {
        false
    }

    /// Whether a page can come back as text.
    fn can_text(&self) -> bool {
        false
    }

    /// One page, rasterized at `dpi`, written into `out` as `[width][height][rgb…]`.
    fn page_picture(&mut self, page: u32, dpi: u32, out: &mut [u8]) -> Answer {
        let _ = (page, dpi, out);
        Answer::Nothing
    }

    /// One page's text, as the host's converter answers it.
    fn page_text(&mut self, page: u32) -> Option<String> {
        let _ = page;
        None
    }
}

/// **What one call for a page came back as**, which is the ABI's count read as three things: the
/// bytes written, the bytes it would need (the buffer was short), or a refusal — for a picture
/// that is a page past the end of the document and a page above [`PAGE_BYTES`] at once, and the
/// two are only told apart by asking again at the floor dpi.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Answer {
    /// `n` bytes, written into the buffer the reader offered.
    Page(usize),
    /// The page needs `n` bytes and the buffer had fewer; nothing was written.
    TooBig(usize),
    /// No page: past the end, a file the converter will not read, or a page above the cap.
    Nothing,
}

/// What a view's body turned out to be.
enum Body {
    /// The page's text, laid out at the viewport's width.
    Rows(Vec<String>),
    /// A sentence: why there is no page here, or where the page went.
    Note(String),
}

/// **The reader's first view of a path**: the state line, the hint line, and the page.
///
/// `None` means "not mine" — the path is not a PDF, and rano opens it the ordinary way.
pub fn view(text: &str, cols: usize, rows: usize, render: &mut impl Render) -> Option<String> {
    let request = Request::parse(text);
    if !request.claim() {
        return None;
    }
    let cols = cols.clamp(16, 512);
    let body = body(&request, cols, render);
    let picture = request.state.mode == Mode::Auto && render.can_picture();
    let mut out: Vec<String> = Vec::new();
    out.push(state_line(&request, &body, rows, picture));
    out.extend(wrap(HINT, cols));
    match body {
        Body::Rows(lines) => out.extend(lines),
        Body::Note(note) => out.extend(wrap(&note, cols)),
    }
    Some(out.join("\n"))
}

/// **The picture of the page the path names**, written into `out` as `[width u32 LE][height u32
/// LE][rgb…]`, and the length of that answer — or `None` for "not mine": a path that is not a
/// PDF, a view in text mode, a build with no rasterizer, or a page the host did not answer.
///
/// **`out` is the answer, not a scratch buffer.** The host writes the page where
/// `provider_picture` is going to point, so a page as large as the cap is never copied; what the
/// reader does is decide *which* page and *which* dpi, and check the header the host wrote.
///
/// The cell box is the host's business: it is told `cols`×`rows` when it draws and it scales what
/// it is given. That is why the zoom is a **dpi** and not a size: `z` buys detail, not centimetres.
pub fn picture(
    text: &str,
    cols: usize,
    rows: usize,
    render: &mut impl Render,
    out: &mut [u8],
    scratch: &mut [u8],
) -> Option<usize> {
    let request = Request::parse(text);
    if !request.claim() || request.state.mode == Mode::Text || !render.can_picture() {
        return None;
    }
    // **Two pages side by side**, when there is room to hold both before composing them. A pane
    // that cannot give the scratch is shown one page: a reader who pressed `s` on a narrow
    // terminal gets a page, not a failure.
    if request.state.spread
        && let Some(len) = spread_bytes(&request, cols, render, out, scratch)
    {
        return Some(len);
    }
    let len = page_bytes(request.state.page, request.state.dpi, render, out)?;
    // **A page too small to read is shown as its text instead.** The kitty placeholder scheme
    // encodes a picture's row in one of thirty diacritics, so a whole A4 page can occupy at most
    // ~42 cells - a thumbnail, painted over the page's own text, which is the reading experience
    // at the terminal's font size. A picture is drawn only where it can be read: a wide pane, a
    // landscape page, or a scan with no text layer at all (`m=text` forces the other way).
    if let Some((w, h)) = header(out).filter(|(w, h)| *w > 0 && *h > 0) {
        let box_cells = (2 * rows as u64 * w as u64 / h as u64) as usize;
        if box_cells.min(cols) < cols / 2 && render.can_text() {
            return None;
        }
    }
    // **A page at the default dpi does not fit a pane.** A4 at 96 dpi is 794 pixels wide and a
    // 100-column box is 200, so the terminal downscales it: bytes paid for detail nobody sees. The
    // first render says how wide the page is at the dpi asked, and the dpi that fills `cols` cells
    // (two pixels a cell) is arithmetic. Only at the default: a zoom is a reader asking for more
    // detail than the box holds, which is what makes `z` sharp rather than merely bigger.
    if request.state.dpi == DEFAULT_DPI
        && let Some((width, _)) = header(out)
        && width > 0
    {
        let want = fit_dpi(DEFAULT_DPI, width, cols.saturating_mul(2));
        if want < DEFAULT_DPI
            && let Some(len) = page_bytes(request.state.page, want, render, out)
        {
            return checked(len, out);
        }
    }
    checked(len, out)
}

/// The dark the rasterizer is cleared with, so the gap beside a short page is the page's own.
const BACKGROUND: u8 = 0x14;

/// **Two pages composed into one picture**: each rendered into its own half of the scratch, then
/// their rows interleaved into `out` — the reader shows one picture, so a spread is one picture
/// twice as wide. A page shorter than the other leaves the background colour where it would have
/// been, which is what a book's two pages look like.
fn spread_bytes(
    request: &Request,
    cols: usize,
    render: &mut impl Render,
    out: &mut [u8],
    scratch: &mut [u8],
) -> Option<usize> {
    if scratch.len() < 2 * (8 + 1024) {
        return None;
    }
    let half = scratch.len() / 2;
    let (left_at, right_at) = scratch.split_at_mut(half);
    // Each page gets half the pane, so the pair fills it. The dpi is the reader's, and the page's
    // own cap does the rest: a half that cannot hold its page is fitted down by `page_bytes`.
    let dpi = request.state.dpi.max(MIN_DPI);
    let n1 = page_bytes(request.state.page, dpi, render, left_at)?;
    // **Each page fills its half of the pane**, and the first render is what says how big the page
    // is: a spread that only fitted the cap drew 1018 pixels into a 320 pixel box, which the
    // terminal then threw away. One render per page is the ordinary case; two when it does not fit.
    let (w1_0, _) = header(left_at)?;
    let want = fit_dpi(dpi, w1_0, cols);
    let (n1, n2) = if want < dpi {
        (
            page_bytes(request.state.page, want, render, left_at)?,
            page_bytes(request.state.page + 1, want, render, right_at)?,
        )
    } else {
        (
            n1,
            page_bytes(request.state.page + 1, dpi, render, right_at)?,
        )
    };
    let (w1, h1) = header(left_at)?;
    let (w2, h2) = header(right_at)?;
    let (w, h) = (w1 + w2, h1.max(h2));
    let len = 8 + (w as usize) * (h as usize) * 3;
    if len > out.len() || left_at.len() < n1 || right_at.len() < n2 {
        return None;
    }
    out[0..4].copy_from_slice(&w.to_le_bytes());
    out[4..8].copy_from_slice(&h.to_le_bytes());
    let stride = (w as usize) * 3;
    for row in 0..h as usize {
        let at = 8 + row * stride;
        copy_row(out, at, left_at, row, h1, w1);
        copy_row(out, at + (w1 as usize) * 3, right_at, row, h2, w2);
    }
    Some(len)
}

/// One page's row into the composed picture, or the background where that page has no row.
fn copy_row(out: &mut [u8], at: usize, page: &[u8], row: usize, rows: u32, width: u32) {
    let w = (width as usize) * 3;
    let dst = &mut out[at..at + w];
    if row < rows as usize {
        let src = 8 + row * w;
        dst.copy_from_slice(&page[src..src + w]);
    } else {
        dst.fill(BACKGROUND);
    }
}

/// The `[width][height]` a picture starts with, if the buffer holds one.
fn header(out: &[u8]) -> Option<(u32, u32)> {
    let w = u32::from_le_bytes(out.get(0..4)?.try_into().ok()?);
    let h = u32::from_le_bytes(out.get(4..8)?.try_into().ok()?);
    Some((w, h))
}

/// **The dpi at which a page `width` pixels wide fills `target` pixels**: linear in the dpi, and
/// clamped to the host's floor of 24, because a reader cannot ask for less than that.
fn fit_dpi(dpi: u32, width: u32, target: usize) -> u32 {
    if target == 0 || width == 0 {
        return dpi;
    }
    let scaled = (u64::from(dpi) * target as u64 / u64::from(width)) as u32;
    scaled.clamp(24, dpi)
}

/// **The whole document as markdown.** A PDF's text is what a terminal renders best - at the
/// reader's own font size, searchable, scrollable - so the view is the document's text as *prose*
/// the host already knows how to render, rather than a page squeezed into thirty placeholder rows.
///
/// The page count is discovered by asking: `convert_page_text` answers a page or nothing, so the
/// loop stops at the first refusal. A document longer than [`MAX_PAGES`] is cut there rather than
/// converting a thousand pages to show one buffer.
pub fn markdown(path_text: &str, render: &mut impl Render) -> Option<String> {
    let request = Request::parse(path_text);
    if !request.claim() || !render.can_text() {
        return None;
    }
    let mut out = format!("# {}\n", request.name());
    let mut pages = 0u32;
    for page in 1..=MAX_PAGES {
        let Some(text) = render.page_text(page) else {
            break;
        };
        // **Prose, not a dump.** A fence around every page is a bordered box with a language
        // label and the page's line breaks kept: read as a document it looks like a log. A page
        // that is laid out in columns is the exception, because re-flowing it would scramble it.
        out.push_str(&format!("\n## Page {page}\n\n"));
        out.push_str(&page_markdown(text.as_str()));
        pages = page;
    }
    if pages == 0 {
        return None;
    }
    Some(out)
}

/// **One page's text as markdown**: paragraphs, with numbered lines as headings - the structure a
/// document has and a text dump does not. A page that is laid out in columns keeps its layout in a
/// fence, because re-flowing a table scrambles it.
pub fn page_markdown(text: &str) -> String {
    // **The invisible characters an extractor leaves behind.** This document's every line is
    // wrapped in U+200B, which `trim` does not remove because it is a format character rather
    // than whitespace - and a renderer counting widths on it draws nothing at all.
    let clean;
    let text = if text.contains(INVISIBLE) {
        clean = text.replace(INVISIBLE, "");
        clean.as_str()
    } else {
        text
    };
    let lines: Vec<&str> = text.lines().collect();
    let laid_out = lines
        .iter()
        .filter(|l| !l.trim().is_empty())
        .filter(|l| l.contains("  ") || l.contains('\t'))
        .count();
    let filled = lines.iter().filter(|l| !l.trim().is_empty()).count().max(1);
    // A fifth of the lines holding a run of spaces is a page with columns or a table.
    if laid_out * 5 > filled {
        return format!("```text\n{}\n```\n", text.trim_end_matches(['\n', '\r']));
    }
    let mut out = String::new();
    let mut para: Vec<&str> = Vec::new();
    let mut code: Vec<&str> = Vec::new();
    for line in lines {
        let trimmed = line.trim();
        // **Code is code.** `originalData := DataInput{…}` is a wall of prose unless it is put
        // where a renderer will show it monospaced.
        if is_code(trimmed) {
            flush_para(&mut para, &mut out);
            code.push(line.trim_end());
            continue;
        }
        if !code.is_empty() {
            flush_code(&mut code, &mut out);
        }
        if trimmed.is_empty() {
            flush_para(&mut para, &mut out);
            continue;
        }
        // **A bullet is a bullet**, whatever glyph the document draws it with.
        if let Some(item) = bullet(trimmed) {
            flush_para(&mut para, &mut out);
            out.push_str("- ");
            out.push_str(item);
            out.push('\n');
            continue;
        }
        if is_heading(trimmed) {
            flush_para(&mut para, &mut out);
            out.push_str("### ");
            out.push_str(trimmed);
            out.push_str("\n\n");
            continue;
        }
        // **Join only across a line break that interrupted a sentence.** PDF text is wrapped at
        // the page's width, so a line end is not a paragraph end - but a line ending in a full
        // stop, or a line starting with a capital after one, usually is a new thought.
        let continues = para.last().is_some_and(|prev: &&str| {
            !prev.ends_with(['.', ':', '!', '?']) && !starts_block(trimmed)
        });
        if !continues {
            flush_para(&mut para, &mut out);
        }
        para.push(trimmed);
    }
    if !code.is_empty() {
        flush_code(&mut code, &mut out);
    }
    flush_para(&mut para, &mut out);
    out
}

fn flush_para(para: &mut Vec<&str>, out: &mut String) {
    if para.is_empty() {
        return;
    }
    out.push_str(&para.join(" "));
    out.push_str("\n\n");
    para.clear();
}

fn flush_code(code: &mut Vec<&str>, out: &mut String) {
    if code.is_empty() {
        return;
    }
    out.push_str("```\n");
    out.push_str(&code.join("\n"));
    out.push_str("\n```\n\n");
    code.clear();
}

/// A line that is a bullet, and its text without the marker.
fn bullet(t: &str) -> Option<&str> {
    let rest = t
        .strip_prefix('●')
        .or_else(|| t.strip_prefix('•'))
        .or_else(|| t.strip_prefix('·'))
        .or_else(|| t.strip_prefix("- "))
        .or_else(|| t.strip_prefix("* "))?;
    Some(rest.trim_start())
}

/// A line that is code rather than prose: Go-shaped, or a brace, or an assignment.
fn is_code(t: &str) -> bool {
    t.starts_with("func ")
        || t.starts_with("var ")
        || t.starts_with("return ")
        || t.starts_with("type ")
        || t.starts_with('}')
        || t == "{"
        || t.ends_with('{')
        || t.contains(":=")
        || (t.contains('{') && t.contains('}'))
}

/// A line that starts a block rather than continuing a sentence.
fn starts_block(t: &str) -> bool {
    t.starts_with(|c: char| c.is_ascii_digit())
        || t.starts_with(['(', '[', '"', '“'])
        || t.chars().next().is_some_and(|c| c.is_uppercase())
}

/// **Whether a line is a section heading**: `1. Introduction`, `2.3.4 The rule`, `4.2.` - numbered,
/// short enough to be a title, and not a sentence that happens to start with a number.
fn is_heading(line: &str) -> bool {
    if line.len() > 72 || line.ends_with('.') && !line.contains(". ") {
        return false;
    }
    let mut parts = line.splitn(2, ' ');
    let Some(number) = parts.next() else {
        return false;
    };
    let number = number.trim_end_matches('.');
    !number.is_empty()
        && number.len() <= 12
        && number.starts_with(|c: char| c.is_ascii_digit())
        && number.chars().all(|c| c.is_ascii_digit() || c == '.')
        && parts.next().is_some()
}

/// The zero-width characters a PDF's text layer carries and a reader must not see.
const INVISIBLE: [char; 5] = ['\u{200b}', '\u{200c}', '\u{200d}', '\u{feff}', '\u{00ad}'];

/// The most pages one view converts: a thousand-page manual would take the host a while and show
/// one buffer, and the reader can open it again.
pub const MAX_PAGES: u32 = 500;

/// **One key, answered as the path to open next.** The input is `"<path>\n<key>"`, which is the one
/// shape a text hook has; the answer is a path, which the host opens (and `provider_open` then
/// draws). `None` is "this key is not mine", which includes `q`: closing a view is the host's.
///
/// The key map is [`HINT`], and the transitions are [`State`]'s methods — one place, so the hint
/// line cannot drift from what the keys do.
pub fn key(input: &str) -> Option<String> {
    // The separator is the last newline: a key is one short line, and a path may hold anything
    // but is far more likely to hold a newline than to end with one.
    let (path, pressed) = input.rsplit_once('\n')?;
    let request = Request::parse(path);
    if !request.claim() {
        return None;
    }
    let pressed = pressed.trim();
    let state = match pressed {
        "n" | "N" | ">" => request.state.next(),
        "p" | "P" | "<" => request.state.prev(),
        "z" => request.state.zoom_in(),
        "Z" => request.state.zoom_out(),
        "0" => request.state.reset_zoom(),
        "t" | "T" => request.state.toggle_text(),
        "s" | "S" => request.state.toggle_spread(),
        _ => {
            // `g`, then a page number: `g12`, `g 12`, `g:12`, `g=12`. The prompt that asks for
            // the number is the host's; this is what it hands the plugin when it has one.
            let page: u32 = pressed
                .strip_prefix('g')?
                .trim_start_matches([' ', ':', '='])
                .parse()
                .ok()?;
            if page == 0 {
                return None;
            }
            request.state.goto(page)
        }
    };
    Some(format!("{}{}", request.path, state.fragment()))
}

/// The body of the view: the page's text if there is any, and otherwise the most useful sentence
/// about why there is not.
fn body(request: &Request, cols: usize, render: &mut impl Render) -> Body {
    let doc = cache::id(&request.path);
    if render.can_text() {
        match page_text(doc, request.state.page, render) {
            // A page with a text layer: this is the view, in both modes. A terminal with no
            // picture protocol gets the page anyway, and on one with a protocol the host draws the
            // picture over it while search, copy and `M-P` still work on the text.
            Some(text) if !text.trim().is_empty() => return Body::Rows(page_rows(&text, cols)),
            Some(_) => return Body::Note(NO_TEXT_LAYER.to_string()),
            None if request.state.mode == Mode::Auto && render.can_picture() => {
                return Body::Note(format!(
                    "{}\n\nThe picture is not there either: the rasterizer answered nothing for \
                     the same page.",
                    missing_page(request.state.page)
                ));
            }
            None => return Body::Note(missing_page(request.state.page)),
        }
    }
    if request.state.mode == Mode::Auto && render.can_picture() {
        return Body::Note(DRAWN_BY_HOST.to_string());
    }
    Body::Note(NO_RENDERER.to_string())
}

/// The state line: what this view is, in one line, above the hint.
fn state_line(request: &Request, body: &Body, rows: usize, picture: bool) -> String {
    // **`auto`, not `picture`.** The build offers a picture and the terminal decides; claiming
    // "picture" on a terminal with no protocol is a line that lies about what is on the glass.
    let mode = if picture { "auto" } else { "text" };
    let spread = if request.state.spread { " · 2-up" } else { "" };
    let mut line = format!(
        "{} · page {} · {} dpi · {}{spread}",
        request.name(),
        request.state.page,
        request.state.dpi,
        mode
    );
    // A page taller than the pane says so, because the buffer is about to be taller than the pane
    // and the reader should know the page is not broken. Two lines are the state line and the hint.
    if let Body::Rows(lines) = body
        && lines.len() > rows.saturating_sub(2)
    {
        line.push_str(&format!(" · {} rows", lines.len()));
    }
    line
}

/// **A page's text, through the cache.** A hit is a page already rendered in this call; a miss asks
/// the host and keeps the answer. Only text is kept: a picture is up to 16 MiB written into the
/// arena by the host, and the ladder asks for a different dpi every time it climbs, so there is
/// nothing a picture entry could save.
fn page_text(doc: u64, page: u32, render: &mut impl Render) -> Option<String> {
    let key = cache::Key::new(doc, page);
    if let Some(text) = cache::with(|c| c.get(key).cloned()) {
        return Some(text);
    }
    let text = render.page_text(page)?;
    cache::with(|c| c.put(key, text.clone()));
    Some(text)
}

/// **The page at the dpi the reader asked for, or at the largest one that fits**, written into
/// `out`; the length of the answer.
///
/// Three things can come back and each has its own next step. An answer is the page. A buffer too
/// small comes with the size the page needs, which is the area, which is the dpi that fits. And a
/// refusal is "no such page" and "a page above one answer's cap" at once — the host refuses both
/// the same way — which is why a refusal is asked again at [`FIT_DPI`], where a page that exists
/// always fits. Only a page whose zoom is the problem costs more than one render; every other page
/// is rendered once.
fn page_bytes(page: u32, dpi: u32, render: &mut impl Render, out: &mut [u8]) -> Option<usize> {
    match render.page_picture(page, dpi, out) {
        Answer::Page(len) => return Some(len),
        // A buffer under the cap, which in this plugin is a test's: the answer's own size says
        // which dpi fits, and one render there is the whole ladder.
        Answer::TooBig(need) => {
            let smaller = fit(dpi, need, out.len());
            if smaller >= dpi {
                return None;
            }
            return match render.page_picture(page, smaller, out) {
                Answer::Page(len) => Some(len),
                _ => None,
            };
        }
        Answer::Nothing => {}
    }
    // A refusal. The floor dpi separates a page that is too big for one answer from one that is
    // not there at all, and it is the whole page when the zoom is the only thing wrong.
    let floor = match render.page_picture(page, FIT_DPI, out) {
        Answer::Page(len) => len,
        _ => return None,
    };
    // The floor's own area says which dpi fits: a page's bytes grow with the square of its dpi.
    let sharper = fit(FIT_DPI, floor, out.len());
    if sharper <= FIT_DPI {
        return Some(floor);
    }
    match render.page_picture(page, sharper, out) {
        Answer::Page(len) => Some(len),
        // The page turned out bigger than its floor suggested: the floor is still the whole page,
        // and it is rendered once more rather than trusted to have survived a refused write.
        _ => match render.page_picture(page, FIT_DPI, out) {
            Answer::Page(len) => Some(len),
            _ => None,
        },
    }
}

/// **The dpi at which a page needing `need` bytes at `dpi` fits `cap`**: a page's RGB grows with the
/// square of its dpi, so the scale is a square root. It answers in either direction — down when a
/// page was too big for the buffer, up when the measurement came from the floor — and a shade
/// conservative, because a rasterizer rounds its raster up.
fn fit(dpi: u32, need: usize, cap: usize) -> u32 {
    let scale = (cap as f64 / need as f64).sqrt() * 0.98;
    let fitted = (dpi as f64 * scale).floor() as u32;
    fitted.clamp(FIT_DPI, MAX_DPI)
}

/// **The answer, checked**: a header that describes exactly the bytes that came with it. The host
/// is the one that wrote it, and this is what a picture is drawn from, so a header that lies is
/// refused rather than cropped.
fn checked(len: usize, out: &[u8]) -> Option<usize> {
    let head = out.get(..8)?;
    let width = u32::from_le_bytes(head[..4].try_into().ok()?);
    let height = u32::from_le_bytes(head[4..8].try_into().ok()?);
    if width == 0 || height == 0 {
        return None;
    }
    let rgb = (width as usize)
        .checked_mul(height as usize)?
        .checked_mul(3)?;
    (rgb.checked_add(8)? == len).then_some(len)
}

/// Why a page is not here. It covers every reason on purpose: the ABI answers a page or nothing, so
/// "past the end", "not a file the converter can read" and "too big for one answer at this zoom"
/// are the same silence, and `Z` is the key that rules the last one out.
fn missing_page(page: u32) -> String {
    format!(
        "no page {page} from the renderer. It answered nothing for it, which means the page is \
         past the end of this document, the file is not one the converter can read, or the page is \
         above what one answer may carry at this zoom (press Z). Press p to go back."
    )
}

/// **A page's text as buffer rows**: laid out at the width it will be drawn at, right-trimmed (a
/// converter pads), and wrapped where it is too wide.
pub fn page_rows(text: &str, cols: usize) -> Vec<String> {
    let mut out = Vec::new();
    for line in text.split('\n') {
        out.extend(wrap(line, cols));
    }
    // A trailing newline is not a row: the buffer's last row would otherwise be empty.
    while out.last().is_some_and(|line| line.is_empty()) {
        out.pop();
    }
    out
}

/// **Wrap at `cols`**, at a space where there is one and mid-word where there is not. Inner runs of
/// spaces are kept (a PDF's columns are spaces), so the break drops the one space it broke at and
/// nothing else.
pub fn wrap(text: &str, cols: usize) -> Vec<String> {
    let cols = cols.max(1);
    let mut out = Vec::new();
    for line in text.split('\n') {
        let mut rest = line;
        loop {
            if rest.chars().count() <= cols {
                out.push(rest.trim_end().to_string());
                break;
            }
            let cut = break_at(rest, cols);
            out.push(rest[..cut].trim_end().to_string());
            rest = &rest[cut..];
            rest = rest.trim_start_matches(' ');
        }
    }
    out
}

/// Where to break a line wider than `cols`: the last space within the width, else the character at
/// the width. Both are character boundaries, which is what makes the slice safe.
fn break_at(line: &str, cols: usize) -> usize {
    let mut last_space = None;
    for (width, (at, ch)) in line.char_indices().enumerate() {
        if width == cols {
            return last_space.unwrap_or(at);
        }
        if ch == ' ' {
            last_space = Some(at);
        }
    }
    last_space.unwrap_or(line.len())
}
