//! The reader: which page, at what zoom, as a picture or as text, and the keys
//! that move it. A fresh instance per call means no guest state, so the state lives
//! in the path's fragment (`#p=4&z=144&m=text`) and every function is pure of it.

use crate::cache;

/// The zoom a document opens at: 96 dpi, the width of a CSS inch.
pub const DEFAULT_DPI: u32 = 96;

/// The zoom's floor and ceiling: below the floor a page is a smudge; above the
/// ceiling it is more pixels than one answer may carry.
pub const MIN_DPI: u32 = 48;
pub const MAX_DPI: u32 = 300;

/// The dpi a too-big page is asked at first: low enough that anything real fits
/// the answer's cap, and above the host's floor — a page asked below the floor is
/// refused for the dpi, which reads as "no such page".
pub const FIT_DPI: u32 = 24;

/// The most one picture answer may carry, header included — the ABI's own cap,
/// `MAX_HOOK_BYTES`. A page whose zoom needs more is re-rendered at the dpi that
/// fits rather than refused.
pub const PAGE_BYTES: usize = 16 << 20;

/// The hint line: every key the reader has is named here, because a view that
/// hides its keys is a view nobody uses. `q` is the host's and is listed too.
pub const HINT: &str =
    "n next · p prev · g<N> goto · s spread · z/Z zoom · 0 reset · t text · q close";

/// What a build with neither import can say — the whole reason the imports are
/// optional: a module importing what the host cannot provide would not
/// instantiate.
const NO_RENDERER: &str = "this build of the plugin has no page renderer. It needs two host \
imports, rano.convert_page (a page as a picture) and rano.convert_page_text (a page as text), and \
this build was compiled without them. The file was still claimed, so a PDF does not open as binary \
noise: the page is simply not there.";

const NO_TEXT_LAYER: &str = "this page has no text layer (a scan, or a page of pictures): the \
picture is the page. Press t for the picture.";

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct State {
    pub page: u32,
    pub dpi: u32,
    pub mode: Mode,
    /// Two pages side by side, which is what a wide pane is for.
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

    pub fn prev(self) -> State {
        State {
            page: self.page.saturating_sub(1).max(1),
            ..self
        }
    }

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

    pub fn zoom_out(self) -> State {
        State {
            dpi: (self.dpi.saturating_mul(2) / 3).max(MIN_DPI),
            ..self
        }
    }

    pub fn reset_zoom(self) -> State {
        State {
            dpi: DEFAULT_DPI,
            ..self
        }
    }

    pub fn toggle_text(self) -> State {
        State {
            mode: match self.mode {
                Mode::Text => Mode::Auto,
                Mode::Auto => Mode::Text,
            },
            ..self
        }
    }

    pub fn toggle_spread(self) -> State {
        State {
            spread: !self.spread,
            ..self
        }
    }

    /// The fragment this state is written as, defaults left out.
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    pub path: String,
    pub state: State,
}

impl Request {
    /// **Parse `path[#p=N&z=DPI&m=text]`.** A suffix that is not exactly one of
    /// those keys with a value this reader would have written is part of the
    /// file's name: `a#b.pdf` and `a.pdf#p=x` are files.
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

    /// The extension is the whole test: guessing from the first bytes would mean
    /// reading the file, which a plugin may not do.
    pub fn claim(&self) -> bool {
        std::path::Path::new(&self.path)
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("pdf"))
    }

    pub fn name(&self) -> String {
        std::path::Path::new(&self.path)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.path.clone())
    }
}

/// **The host's two imports, as this plugin sees them** — the only seam that is
/// not a pure function, which is what lets the reader be tested on the host
/// with no wasm, no PDF and no converter.
pub trait Render {
    fn can_picture(&self) -> bool {
        false
    }

    fn can_text(&self) -> bool {
        false
    }

    /// One page, rasterized at `dpi`, written into `out` as `[width][height][rgb…]`.
    fn page_picture(&mut self, page: u32, dpi: u32, out: &mut [u8]) -> Answer {
        let _ = (page, dpi, out);
        Answer::Nothing
    }

    fn page_text(&mut self, page: u32) -> Option<String> {
        let _ = page;
        None
    }
}

/// What one call for a page came back as: the bytes written, the bytes it would
/// need, or a refusal — which is both "past the end" and "above the cap", told
/// apart only by asking again at the floor dpi.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Answer {
    Page(usize),
    /// The page needs `n` bytes and the buffer had fewer; nothing was written.
    TooBig(usize),
    Nothing,
}

enum Body {
    Rows(Vec<String>),
    Note(String),
}

/// The reader's first view of a path: state line, hint line, page. `None` is "not
/// mine".
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

/// The picture of the page the path names, written into `out` as
/// `[width][height][rgb…]` — the answer itself, so a page at the cap is never
/// copied. The zoom is a dpi because the cell box is the host's business.
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
    // Two pages side by side, when there is room to hold both. A pane that cannot
    // give the scratch is shown one page, not a failure.
    if request.state.spread
        && let Some(len) = spread_bytes(&request, cols, render, out, scratch)
    {
        return Some(len);
    }
    let len = page_bytes(request.state.page, request.state.dpi, render, out)?;
    // A page too small to read is shown as its text instead: the kitty placeholder
    // scheme paints at most ~42 cells, a thumbnail over the page's own text. `m=text`
    // forces the other way.
    if let Some((w, h)) = header(out).filter(|(w, h)| *w > 0 && *h > 0) {
        let box_cells = (2 * rows as u64 * w as u64 / h as u64) as usize;
        if box_cells.min(cols) < cols / 2 && render.can_text() {
            return None;
        }
    }
    // At the default dpi a page can be wider than the pane: A4 at 96 dpi is 794
    // pixels where a 100-column box is 200, so the terminal downscales bytes nobody
    // sees. Only at the default — a zoom asks for more detail than the box holds.
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

/// The dark the rasterizer is cleared with, so the gap beside a short page is the
/// page's own.
const BACKGROUND: u8 = 0x14;

/// Two pages composed into one picture: each rendered into its own half of the
/// scratch, then their rows interleaved into `out`. A shorter page leaves the
/// background colour where it would have been.
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
    // Each page fills half the pane, so the pair fills it. The first render
    // says how big the page is; a half that cannot hold its page is fitted down
    // by `page_bytes`.
    let dpi = request.state.dpi.max(MIN_DPI);
    let n1 = page_bytes(request.state.page, dpi, render, left_at)?;
    // **Each page fills its half of the pane**: one render per page is the
    // ordinary case; two when it does not fit.
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

/// One page's row into the composed picture, or the background where that page
/// has no row.
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

fn header(out: &[u8]) -> Option<(u32, u32)> {
    let w = u32::from_le_bytes(out.get(0..4)?.try_into().ok()?);
    let h = u32::from_le_bytes(out.get(4..8)?.try_into().ok()?);
    Some((w, h))
}

/// The dpi at which a page `width` pixels wide fills `target` pixels: linear in the
/// dpi, clamped to the host's floor of 24.
fn fit_dpi(dpi: u32, width: u32, target: usize) -> u32 {
    if target == 0 || width == 0 {
        return dpi;
    }
    let scaled = (u64::from(dpi) * target as u64 / u64::from(width)) as u32;
    scaled.clamp(24, dpi)
}

/// The whole document as markdown — a PDF's text is what a terminal renders best.
/// The page count is discovered by asking, and a document longer than [`MAX_PAGES`]
/// is cut there.
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
        // Prose, not a dump: a fence around every page would read as a log. A page
        // laid out in columns keeps its layout, because re-flowing scrambles it.
        out.push_str(&format!("\n## Page {page}\n\n"));
        out.push_str(&page_markdown(text.as_str()));
        pages = page;
    }
    if pages == 0 {
        return None;
    }
    Some(out)
}

/// One page's text as markdown: paragraphs, with numbered lines as headings — the
/// structure a document has and a text dump does not.
pub fn page_markdown(text: &str) -> String {
    // The invisible characters an extractor leaves behind, which `trim` does not
    // remove and a renderer draws nothing on.
    let clean;
    let text = if text.contains(INVISIBLE) {
        clean = text.replace(INVISIBLE, "");
        clean.as_str()
    } else {
        text
    };
    let lines: Vec<&str> = text.lines().collect();
    // A run of spaces inside a line, not leading indentation: `-layout` indents
    // prose too, so only an inner run makes a page tabular.
    let laid_out = lines
        .iter()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .filter(|l| l.split_whitespace().count() > 1 && l.split("  ").count() > 2)
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
        // Code is code: put it where a renderer shows it monospaced.
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
    // A blank line is the only paragraph break, because it is the only one the
    // extractor actually saw.
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

fn bullet(t: &str) -> Option<&str> {
    let rest = t
        .strip_prefix('●')
        .or_else(|| t.strip_prefix('•'))
        .or_else(|| t.strip_prefix('·'))
        .or_else(|| t.strip_prefix("- "))
        .or_else(|| t.strip_prefix("* "))?;
    Some(rest.trim_start())
}

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

/// Whether a line is a section heading: `1. Introduction`, `2.3.4 The rule` —
/// numbered, and short enough to be a title.
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

const INVISIBLE: [char; 5] = ['\u{200b}', '\u{200c}', '\u{200d}', '\u{feff}', '\u{00ad}'];

/// The most pages one view converts: a thousand-page manual would take the host
/// a while and show one buffer, and the reader can open it again.
pub const MAX_PAGES: u32 = 500;

/// One key, answered as the path to open next; `None` is "not mine", `q`
/// included, because closing a view is the host's. The key map is [`HINT`] and the
/// transitions are [`State`]'s methods, so the hint line cannot drift.
pub fn key(input: &str) -> Option<String> {
    // The separator is the last newline: a key is one short line, and a path is
    // far more likely to hold a newline than to end with one.
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
            // `g`, then a page number: `g12`, `g 12`, `g:12`, `g=12`.
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

/// The body of the view: the page's text, else the most useful sentence about why
/// there is none.
fn body(request: &Request, cols: usize, render: &mut impl Render) -> Body {
    let doc = cache::id(&request.path);
    if render.can_text() {
        match page_text(doc, request.state.page, render) {
            // A page with a text layer: this is the view, in both modes — the
            // host draws the picture over it while search and copy still work
            // on the text.
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

fn state_line(request: &Request, body: &Body, rows: usize, picture: bool) -> String {
    // `auto`, not `picture`: the build offers a picture and the terminal decides;
    // claiming "picture" where there is no protocol is a lie.
    let mode = if picture { "auto" } else { "text" };
    let spread = if request.state.spread { " · 2-up" } else { "" };
    let mut line = format!(
        "{} · page {} · {} dpi · {}{spread}",
        request.name(),
        request.state.page,
        request.state.dpi,
        mode
    );
    // A page taller than the pane says so, so the reader knows it is not broken.
    if let Body::Rows(lines) = body
        && lines.len() > rows.saturating_sub(2)
    {
        line.push_str(&format!(" · {} rows", lines.len()));
    }
    line
}

/// A page's text through the cache. Only text is kept: a picture is up to 16 MiB
/// written by the host, and the ladder asks for a different dpi every time it
/// climbs.
fn page_text(doc: u64, page: u32, render: &mut impl Render) -> Option<String> {
    let key = cache::Key::new(doc, page);
    if let Some(text) = cache::with(|c| c.get(key).cloned()) {
        return Some(text);
    }
    let text = render.page_text(page)?;
    cache::with(|c| c.put(key, text.clone()));
    Some(text)
}

/// The page at the dpi asked for, or the largest that fits, written into `out`. A
/// refusal is "no such page" and "above the cap" at once, so it is asked again at
/// [`FIT_DPI`]; only a page whose zoom is the problem costs more than one render.
fn page_bytes(page: u32, dpi: u32, render: &mut impl Render, out: &mut [u8]) -> Option<usize> {
    match render.page_picture(page, dpi, out) {
        Answer::Page(len) => return Some(len),
        // A buffer under the cap, which in this plugin is a test's: the answer's
        // own size says which dpi fits.
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
    // A refusal: the floor dpi separates a page too big for one answer from one
    // that is not there at all.
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
        // The page turned out bigger than its floor suggested: render the floor
        // again rather than trust it to have survived a refused write.
        _ => match render.page_picture(page, FIT_DPI, out) {
            Answer::Page(len) => Some(len),
            _ => None,
        },
    }
}

/// The dpi at which a page needing `need` bytes at `dpi` fits `cap`: RGB grows
/// with the square of the dpi, so the scale is a square root, a shade
/// conservative because rasterizers round up.
fn fit(dpi: u32, need: usize, cap: usize) -> u32 {
    let scale = (cap as f64 / need as f64).sqrt() * 0.98;
    let fitted = (dpi as f64 * scale).floor() as u32;
    fitted.clamp(FIT_DPI, MAX_DPI)
}

/// The answer, checked: a header that describes exactly the bytes that came with
/// it — refused rather than cropped, since a picture is drawn from it.
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

/// Why a page is not here, covering every reason on purpose: the ABI answers a
/// page or nothing, so "past the end", "unreadable file" and "too big at this
/// zoom" are the same silence, and `Z` rules the last one out.
fn missing_page(page: u32) -> String {
    format!(
        "no page {page} from the renderer. It answered nothing for it, which means the page is \
         past the end of this document, the file is not one the converter can read, or the page is \
         above what one answer may carry at this zoom (press Z). Press p to go back."
    )
}

pub fn page_rows(text: &str, cols: usize) -> Vec<String> {
    let mut out = Vec::new();
    for line in text.split('\n') {
        out.extend(wrap(line, cols));
    }
    // A trailing newline is not a row: the buffer's last row would be empty.
    while out.last().is_some_and(|line| line.is_empty()) {
        out.pop();
    }
    out
}

/// Wrap at `cols`, at a space where there is one and mid-word where there is not.
/// Inner runs of spaces are kept, because a PDF's columns are spaces.
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

/// Where to break a line wider than `cols`: the last space within the width, else
/// the character at it. Both are char boundaries, which is what makes the slice safe.
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
