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

use crate::cache::{self, Page};
use crate::png::{self, Image};

/// The zoom a document opens at: 96 dpi, the width of a CSS inch.
pub const DEFAULT_DPI: u32 = 96;

/// The zoom's floor and ceiling. Below the floor a page is a smudge; above the ceiling it is more
/// pixels than the plugin's arena will hold for any page worth reading.
pub const MIN_DPI: u32 = 48;
pub const MAX_DPI: u32 = 300;

/// **The dpi the render ladder may fall to** when a page is too big to decode. It is below
/// [`MIN_DPI`] because it is not a zoom the reader asked for: it is the size at which a page still
/// fits, and a small picture of the whole page beats no picture at all.
pub const FIT_DPI: u32 = 12;

/// **The most RGB bytes one answer may carry**, and the binding constraint is not memory — it is
/// the host's **fuel**.
///
/// Measured on the built module (`plans/S10-wasm-hooks.md` §5 gives a hook 50 M units): decoding a
/// page costs **~61 units per RGB byte** in wasm, inflate and unfilter together (a 405 KB page:
/// 24.5 M). So 50 M covers about 800 KB, and half of that is kept back, because a page's deflate
/// stream is not always as kind as the one measured. 512 KiB is ~175 000 pixels: an A4 page at
/// about 42 dpi, which is a page you can see the shape of, at the size a terminal's cell box shows
/// anyway. A page rendered above it is re-rendered at a lower dpi rather than refused, and a host
/// that grants a picture hook more fuel raises this one number.
pub const PICTURE_BYTES: usize = 512 << 10;

/// **The hint line**, wrapped to the viewport like everything else. Every key the reader has is
/// named here, which is the rule the plugin dired set: a plugin view that hides its keys is a
/// plugin view nobody uses. `q` is the host's (it closes the view), and it is listed with the rest
/// because the reader does not care which side of the ABI a key lives on.
pub const HINT: &str = "n next · p prev · g<N> goto · z/Z zoom · 0 reset · t text · q close";

/// What a build with neither import can say. A module that imported what the host cannot provide
/// would not instantiate at all, so this sentence is the whole reason the imports are optional.
const NO_RENDERER: &str = "this build of the plugin has no page renderer. It needs two host \
imports, rano.convert_page (a page as a picture) and rano.convert_page_text (a page as text), and \
this rano provides neither. The file was still claimed, so a PDF does not open as binary noise: \
the page is simply not there until the host half lands.";

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
}

impl Default for State {
    fn default() -> State {
        State {
            page: 1,
            dpi: DEFAULT_DPI,
            mode: Mode::Auto,
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
    fn can_png(&self) -> bool {
        false
    }

    /// Whether a page can come back as text.
    fn can_text(&self) -> bool {
        false
    }

    /// One page, rasterized at `dpi`, as the PNG bytes the host answers.
    fn page_png(&mut self, page: u32, dpi: u32) -> Option<Vec<u8>> {
        let _ = (page, dpi);
        None
    }

    /// One page's text, as the host's converter answers it.
    fn page_text(&mut self, page: u32) -> Option<String> {
        let _ = page;
        None
    }
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
    let picture = request.state.mode == Mode::Auto && render.can_png();
    let mut out: Vec<String> = Vec::new();
    out.push(state_line(&request, &body, rows, picture));
    out.extend(wrap(HINT, cols));
    match body {
        Body::Rows(lines) => out.extend(lines),
        Body::Note(note) => out.extend(wrap(&note, cols)),
    }
    Some(out.join("\n"))
}

/// **The picture of the page the path names**, as `[width u32 LE][height u32 LE][rgb…]`, or `None`
/// for "not mine" — a path that is not a PDF, a view in text mode, a build with no rasterizer, or
/// a page the host did not answer.
///
/// The cell box is the host's business: it is told `cols`×`rows` when it draws and it scales what
/// it is given. That is why the zoom is a **dpi** and not a size: `z` buys detail, not centimetres.
pub fn picture(
    text: &str,
    _cols: usize,
    _rows: usize,
    render: &mut impl Render,
) -> Option<Vec<u8>> {
    let request = Request::parse(text);
    if !request.claim() || request.state.mode == Mode::Text || !render.can_png() {
        return None;
    }
    // The ladder: the requested dpi first, and then the dpi that fits what one call may carry. The
    // geometry comes out of the PNG's header, so a page that is too big is never inflated at all.
    let doc = cache::id(&request.path);
    let mut dpi = request.state.dpi;
    for _ in 0..3 {
        let png = page_png(doc, request.state.page, dpi, render)?;
        let head = png::header(&png)?;
        if head.rgb_len() <= PICTURE_BYTES {
            let image = png::decode(&png).ok()?;
            return Some(frame(&image));
        }
        let smaller = fit(dpi, head.rgb_len());
        if smaller >= dpi {
            return None;
        }
        dpi = smaller;
    }
    None
}

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
            None if request.state.mode == Mode::Auto && render.can_png() => {
                return Body::Note(format!(
                    "{}\n\nThe picture is not there either: the rasterizer answered nothing for \
                     the same page.",
                    missing_page(request.state.page)
                ));
            }
            None => return Body::Note(missing_page(request.state.page)),
        }
    }
    if request.state.mode == Mode::Auto && render.can_png() {
        return Body::Note(DRAWN_BY_HOST.to_string());
    }
    Body::Note(NO_RENDERER.to_string())
}

/// The state line: what this view is, in one line, above the hint.
fn state_line(request: &Request, body: &Body, rows: usize, picture: bool) -> String {
    let mode = if picture { "picture" } else { "text" };
    let mut line = format!(
        "{} · page {} · {} dpi · {}",
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

/// **A hit, copied out of the cache** — the lock is not held past the answer, and the entry is made
/// the most recent by the lookup itself.
fn cached<R>(key: cache::Key, take: impl FnOnce(&Page) -> Option<R>) -> Option<R> {
    cache::with(|c| c.get(key).and_then(take))
}

/// **A page's text, through the cache.** A hit is a page already rendered in this call; a miss asks
/// the host and keeps the answer.
fn page_text(doc: u64, page: u32, render: &mut impl Render) -> Option<String> {
    let key = cache::Key::text(doc, page);
    if let Some(text) = cached(key, |page| match page {
        Page::Text(text) => Some(text.clone()),
        Page::Png(_) => None,
    }) {
        return Some(text);
    }
    let text = render.page_text(page)?;
    cache::with(|c| c.put(key, Page::Text(text.clone())));
    Some(text)
}

/// **A page's picture as the host answered it, through the cache.** The PNG is what a render
/// costs, so it is what is kept: a page asked for twice at the same dpi is rasterized once.
fn page_png(doc: u64, page: u32, dpi: u32, render: &mut impl Render) -> Option<Vec<u8>> {
    let key = cache::Key::picture(doc, page, dpi);
    if let Some(bytes) = cached(key, |page| match page {
        Page::Png(bytes) => Some(bytes.clone()),
        Page::Text(_) => None,
    }) {
        return Some(bytes);
    }
    let bytes = render.page_png(page, dpi)?;
    cache::with(|c| c.put(key, Page::Png(bytes.clone())));
    Some(bytes)
}

/// **The dpi to try when a page does not fit**: the same page scaled down to the arena, by area.
/// Never up, never below the floor.
fn fit(dpi: u32, need: usize) -> u32 {
    if dpi <= FIT_DPI {
        return dpi;
    }
    let scale = (PICTURE_BYTES as f64 / need as f64).sqrt();
    let smaller = (dpi as f64 * scale).floor() as u32;
    smaller.clamp(FIT_DPI, dpi - 1)
}

/// The answer's bytes: width, height, then RGB.
fn frame(image: &Image) -> Vec<u8> {
    let mut out = Vec::with_capacity(8 + image.rgb.len());
    out.extend_from_slice(&image.width.to_le_bytes());
    out.extend_from_slice(&image.height.to_le_bytes());
    out.extend_from_slice(&image.rgb);
    out
}

/// Why a page is not here. It covers both reasons on purpose: the ABI answers a page or nothing,
/// so "past the end" and "not a file the converter can read" are the same silence.
fn missing_page(page: u32) -> String {
    format!(
        "no page {page} from the renderer. It answered nothing for it, which means the page is \
         past the end of this document, or the file is not one the converter can read. Press p to \
         go back."
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
