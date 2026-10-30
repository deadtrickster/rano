//! **The reader, tested natively.** `reader::view`, `reader::picture` and `reader::key` are what
//! the three exports call one line after reading the host's pointer, so testing them is testing
//! the plugin: what the exports add on top is the arena, and that needs wasm32 addresses.
//!
//! The fake renderer is the point of the [`Render`] seam: a document with a known number of pages,
//! a known text layer and a known rasterizer, so "a page past the end" is a case a test can *make*
//! rather than a case it has to find a PDF for. It also writes `[width][height][rgb]` into the
//! buffer it is handed, because that is what the host does: the picture's bytes are the answer.

use rano_plugin_pdf::cache;
use rano_plugin_pdf::reader::{self, Answer, HINT, Mode, Render, State, view};

/// **The page cache is one static in the plugin, so the tests that read it take turns and start
/// clean.** Without this a page one test rendered is another test's cache hit, and a test that
/// pins "rendered once" would be pinning the scheduler.
fn fresh() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    cache::clear();
    guard
}

// ---------------------------------------------------------------- a fake host

/// The geometry a rasterizer answers for one page at one dpi, or `None` for a page the document
/// does not have.
type ShotFn = Box<dyn Fn(u32, u32) -> Option<(u32, u32)>>;

struct Fake {
    /// One entry a page, page 1 first. `None` is a page the converter does not answer.
    texts: Vec<Option<String>>,
    shots: Option<ShotFn>,
    /// The most one answer may carry: the host's own cap, which it refuses a page above before it
    /// even looks at the buffer the guest offered.
    cap: usize,
    /// The colour every page of this document is.
    rgb: [u8; 3],
    can_picture: bool,
    can_text: bool,
    asked_picture: Vec<(u32, u32)>,
    asked_text: Vec<u32>,
}

impl Fake {
    /// A document of `pages` pages, each with a text layer.
    fn pdf(pages: u32) -> Fake {
        Fake {
            texts: (1..=pages)
                .map(|n| Some(format!("page {n} of the document\nsecond line")))
                .collect(),
            shots: None,
            cap: reader::PAGE_BYTES,
            rgb: [10, 20, 30],
            can_picture: false,
            can_text: true,
            asked_picture: Vec::new(),
            asked_text: Vec::new(),
        }
    }

    /// The same, with a rasterizer that answers a `w`×`h` page at any dpi — and answers nothing
    /// for a page the document does not have, which is how "past the end" reaches the picture path
    /// at all.
    fn rasterized(pages: u32, w: u32, h: u32) -> Fake {
        let mut fake = Fake::pdf(pages);
        fake.can_picture = true;
        fake.shots = Some(Box::new(move |page, _| {
            (1..=pages).contains(&page).then_some((w, h))
        }));
        fake
    }

    /// **A page of paper**, as a rasterizer sees it: the raster grows with the dpi, so a page that
    /// is too big at one zoom is a different size at another.
    fn paper(pages: u32, w: f64, h: f64) -> Fake {
        let mut fake = Fake::pdf(pages);
        fake.can_picture = true;
        fake.shots = Some(Box::new(move |page, dpi| {
            (1..=pages).contains(&page).then(|| {
                let dpi = f64::from(dpi);
                ((w * dpi).ceil() as u32, (h * dpi).ceil() as u32)
            })
        }));
        fake
    }
}

impl Render for Fake {
    fn can_picture(&self) -> bool {
        self.can_picture
    }

    fn can_text(&self) -> bool {
        self.can_text
    }

    /// **The host's answer, in the host's order**: a page above the cap is a refusal, a buffer that
    /// is merely too small is `-needed`, and only then are the bytes written.
    fn page_picture(&mut self, page: u32, dpi: u32, out: &mut [u8]) -> Answer {
        self.asked_picture.push((page, dpi));
        // **The host's dpi floor, which this fake lacked.** `pdf::checked_dpi` refuses below
        // MIN_DPI = 24 and above MAX_DPI = 600, and a fake without the floor answers where the
        // real host refuses - so the suite pinned a sequence the host cannot serve and the
        // reader's fallback read as working when it was not.
        if !(24..=600).contains(&dpi) {
            return Answer::Nothing;
        }
        let Some(shots) = self.shots.as_ref() else {
            return Answer::Nothing;
        };
        let Some((w, h)) = shots(page, dpi) else {
            return Answer::Nothing;
        };
        let len = 8 + (w as usize) * (h as usize) * 3;
        if len > self.cap {
            return Answer::Nothing;
        }
        if len > out.len() {
            return Answer::TooBig(len);
        }
        out[..4].copy_from_slice(&w.to_le_bytes());
        out[4..8].copy_from_slice(&h.to_le_bytes());
        let (pixels, _) = out[8..len].as_chunks_mut::<3>();
        for pixel in pixels {
            pixel.copy_from_slice(&self.rgb);
        }
        Answer::Page(len)
    }

    fn page_text(&mut self, page: u32) -> Option<String> {
        self.asked_text.push(page);
        let at = page.checked_sub(1)? as usize;
        self.texts.get(at).cloned().flatten()
    }
}

/// A host with no page renderer at all: what a build without the two imports looks like, and what
/// a test of the degraded path needs.
struct Nothing;

impl Render for Nothing {}

// ---------------------------------------------------------------- the door

#[test]
fn a_pdf_is_claimed_and_anything_else_is_refused() {
    let _fresh = fresh();
    let mut fake = Fake::pdf(3);
    assert!(view("notes.txt", 80, 24, &mut fake).is_none());
    assert!(view("/tmp/a.pdf.bak", 80, 24, &mut fake).is_none());
    // The extension is the whole test, and it is not case-sensitive: a `.PDF` is a PDF.
    let v = view("/tmp/paper.PDF", 80, 24, &mut fake).expect("a PDF is claimed");
    assert!(v.contains("paper.PDF · page 1"), "{v}");
}

#[test]
fn the_first_view_names_the_page_the_zoom_the_mode_and_every_key() {
    let _fresh = fresh();
    let mut fake = Fake::pdf(3);
    let v = view("/home/me/paper.pdf", 80, 24, &mut fake).unwrap();
    let first = v.lines().next().unwrap();
    assert_eq!(first, "paper.pdf · page 1 · 96 dpi · text");
    assert!(v.contains(HINT), "{v}");
    // The page itself is in the view, so search, copy and M-P work over it.
    assert!(v.contains("page 1 of the document"), "{v}");
}

#[test]
fn a_page_past_the_end_is_a_message_in_the_view_and_not_a_panic() {
    let _fresh = fresh();
    let mut fake = Fake::pdf(12);
    let v = view("past-the-end.pdf#p=13", 80, 24, &mut fake).unwrap();
    assert!(v.contains("page 13"), "{v}");
    assert!(v.contains("past the end"), "{v}");
    // Still a view with keys, so the reader can press `p` and be somewhere. The sentence is wrapped
    // to the viewport, so it is read with the wrapping taken out.
    let flat = v.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(v.contains("p prev"), "{v}");
    assert!(flat.contains("Press p to go back"), "{v}");
}

#[test]
fn a_page_with_no_text_layer_says_so_and_points_at_the_picture() {
    let _fresh = fresh();
    let mut fake = Fake::pdf(2);
    fake.texts[0] = Some(String::new());
    let v = view("scan.pdf#m=text", 80, 24, &mut fake).unwrap();
    assert!(v.contains("no text layer"), "{v}");
    assert!(v.contains("Press t"), "{v}");
}

// ---------------------------------------------------------------- text mode

#[test]
fn text_mode_produces_rows_laid_out_at_the_viewport_width() {
    let _fresh = fresh();
    let mut fake = Fake::pdf(1);
    fake.texts[0] = Some(format!("{}\nshort line\n\nlast", "x".repeat(200)));
    let v = view("wide.pdf#m=text", 40, 6, &mut fake).unwrap();
    let lines: Vec<&str> = v.lines().collect();
    // State line, two hint lines, then the page: a 200-character row is five 40-column rows, and
    // the page has three more rows after it.
    assert!(
        lines.iter().skip(1).all(|line| line.chars().count() <= 40),
        "{v}"
    );
    assert_eq!(lines[0], "wide.pdf · page 1 · 96 dpi · text · 8 rows");
    assert!(v.contains("short line"), "{v}");
    assert!(v.ends_with("last"), "{v}");
    // Blank rows survive: a page's spacing is content.
    assert!(lines.iter().any(|line| line.is_empty()), "{v}");
}

#[test]
fn a_page_taller_than_the_pane_says_how_many_rows_it_has() {
    let _fresh = fresh();
    let mut fake = Fake::pdf(1);
    fake.texts[0] = Some(
        (1..=40)
            .map(|n| format!("row {n}"))
            .collect::<Vec<_>>()
            .join("\n"),
    );
    let v = view("tall.pdf", 80, 24, &mut fake).unwrap();
    assert!(v.lines().next().unwrap().ends_with("· 40 rows"), "{v}");
    // And a page that fits says nothing about rows.
    let mut small = Fake::pdf(1);
    small.texts[0] = Some("one row".to_string());
    let v = view("short.pdf", 80, 24, &mut small).unwrap();
    assert!(!v.lines().next().unwrap().contains("rows"), "{v}");
}

#[test]
fn auto_mode_still_carries_the_text_of_a_page_it_will_draw() {
    let _fresh = fresh();
    let mut fake = Fake::rasterized(2, 20, 20);
    let v = view("drawn.pdf", 80, 24, &mut fake).unwrap();
    assert!(v.contains("· picture"), "{v}");
    // A terminal with no picture protocol sees the page anyway, and a terminal with one gets the
    // picture over text that search and copy still reach.
    assert!(v.contains("page 1 of the document"), "{v}");
}

// ---------------------------------------------------------------- no renderer

#[test]
fn a_build_with_no_renderer_answers_with_the_message_instead_of_failing() {
    let _fresh = fresh();
    let v =
        view("no-renderer.pdf", 80, 24, &mut Nothing).expect("still claimed, so no binary noise");
    assert!(v.contains("no page renderer"), "{v}");
    assert!(v.contains("rano.convert_page"), "{v}");
    assert!(v.contains(HINT), "{v}");
    // Nothing to draw, and that is the sentinel rather than a trap.
    let mut out = vec![0u8; 4096];
    assert!(reader::picture("no-renderer.pdf", 80, 24, &mut Nothing, &mut out).is_none());
}

#[test]
fn a_build_with_a_rasterizer_but_no_text_says_where_the_text_went() {
    let _fresh = fresh();
    let mut fake = Fake::pdf(1);
    fake.can_text = false;
    fake.can_picture = true;
    fake.shots = Some(Box::new(|_, _| Some((2, 2))));
    let v = view("textless.pdf", 80, 24, &mut fake).unwrap();
    assert!(v.contains("drawn by the host"), "{v}");
}

// ---------------------------------------------------------------- pictures

#[test]
fn a_picture_is_width_height_then_rgb() {
    let _fresh = fresh();
    let mut fake = Fake::rasterized(2, 3, 2);
    let mut out = vec![0u8; 4096];
    let len =
        reader::picture("picture.pdf", 80, 24, &mut fake, &mut out).expect("a page comes back");
    assert_eq!(len, 8 + 3 * 2 * 3);
    assert_eq!(u32::from_le_bytes(out[0..4].try_into().unwrap()), 3);
    assert_eq!(u32::from_le_bytes(out[4..8].try_into().unwrap()), 2);
    assert_eq!(&out[8..len], [10u8, 20, 30].repeat(6).as_slice());
    // The zoom is asked for, not applied here: the dpi is the reader's state.
    assert_eq!(fake.asked_picture, vec![(1, 96)]);
}

#[test]
fn a_page_too_big_for_the_buffer_is_rendered_again_smaller() {
    let _fresh = fresh();
    // A buffer under the ABI's cap, which is what makes the host answer "it needs this many
    // bytes" instead of refusing: the reader reads the page's area off that and asks again lower.
    let mut fake = Fake::paper(1, 8.27, 11.69);
    // Room for the FLOOR dpi, which is the host's minimum (24): A4 at 24 dpi is ~166 KiB, so a
    // 64 KiB buffer makes the floor itself too big and the reader gives up instead of fitting.
    let mut out = vec![0u8; 256 << 10];
    let len = reader::picture("huge.pdf", 80, 24, &mut fake, &mut out).expect("a smaller page");
    assert_eq!(fake.asked_picture.len(), 2, "{:?}", fake.asked_picture);
    assert_eq!(fake.asked_picture[0], (1, 96));
    assert!(fake.asked_picture[1].1 < 96, "{:?}", fake.asked_picture);
    // A4 at 96 dpi is 794 px wide, so anything narrower is the smaller render.
    let width = u32::from_le_bytes(out[0..4].try_into().unwrap());
    assert!(width < 794, "a {width}px page is not a smaller one");
    assert!(
        len <= out.len(),
        "{len} bytes in a {} byte buffer",
        out.len()
    );
}

#[test]
fn a_page_above_the_cap_is_rendered_at_the_dpi_that_fits() {
    let _fresh = fresh();
    // A4 at 300 dpi is 26 MiB of RGB, above what one answer may carry, and the host refuses it
    // exactly as it refuses a page that is not there. The floor dpi is where a page that exists
    // always fits, so asking there separates the two, and the floor's own area says which dpi
    // does fit: the page is sharp, not a thumbnail.
    let mut fake = Fake::paper(1, 8.27, 11.69);
    let mut out = vec![0u8; reader::PAGE_BYTES];
    reader::picture("a4.pdf#z=300", 80, 24, &mut fake, &mut out).expect("a sharp page");
    assert_eq!(fake.asked_picture[0], (1, 300));
    assert_eq!(fake.asked_picture[1], (1, reader::FIT_DPI));
    assert_eq!(fake.asked_picture.len(), 3, "{:?}", fake.asked_picture);
    // The cap is 16 MiB, so A4 fits up to 240 dpi and the ladder has to land under that.
    let sharp = fake.asked_picture[2].1;
    assert!(
        (200..=240).contains(&sharp),
        "{sharp} dpi is not the fit for A4"
    );
    let width = u32::from_le_bytes(out[0..4].try_into().unwrap());
    assert!(width > 1900, "a {width}px page is not a sharp one");
}

#[test]
fn text_mode_answers_no_picture_and_a_page_the_host_misses_answers_none() {
    let _fresh = fresh();
    let mut fake = Fake::rasterized(1, 4, 4);
    let mut out = vec![0u8; 4096];
    assert!(reader::picture("miss.pdf#m=text", 80, 24, &mut fake, &mut out).is_none());
    assert!(reader::picture("miss.pdf#p=9", 80, 24, &mut fake, &mut out).is_none());
    assert!(reader::picture("notes.txt", 80, 24, &mut fake, &mut out).is_none());
    // A page with no pixels is not a picture either: the header has to describe a real one.
    let mut empty = Fake::rasterized(1, 0, 0);
    assert!(reader::picture("empty.pdf", 80, 24, &mut empty, &mut out).is_none());
}

// ---------------------------------------------------------------- the keys

#[test]
fn every_key_moves_the_state_and_answers_the_next_path() {
    let cases: &[(&str, Option<&str>)] = &[
        ("doc.pdf\nn", Some("doc.pdf#p=2")),
        ("doc.pdf#p=2\nn", Some("doc.pdf#p=3")),
        // Page 1 is the wall, and the default state is written as no fragment at all.
        ("doc.pdf\np", Some("doc.pdf")),
        ("doc.pdf#p=2\np", Some("doc.pdf")),
        ("doc.pdf\ng12", Some("doc.pdf#p=12")),
        ("doc.pdf\ng 12", Some("doc.pdf#p=12")),
        ("doc.pdf#p=4\ng:7", Some("doc.pdf#p=7")),
        ("doc.pdf\ng", None),
        ("doc.pdf\ng0", None),
        ("doc.pdf\nz", Some("doc.pdf#z=144")),
        ("doc.pdf#z=216\nz", Some("doc.pdf#z=300")),
        ("doc.pdf#z=300\nz", Some("doc.pdf#z=300")),
        ("doc.pdf\nZ", Some("doc.pdf#z=64")),
        ("doc.pdf#z=48\nZ", Some("doc.pdf#z=48")),
        ("doc.pdf#z=144\nZ", Some("doc.pdf")),
        ("doc.pdf#z=144&p=3\n0", Some("doc.pdf#p=3")),
        ("doc.pdf\nt", Some("doc.pdf#m=text")),
        ("doc.pdf#m=text\nt", Some("doc.pdf")),
        // `q` closes the view, which is the host's business; anything else is not this plugin's.
        ("doc.pdf\nq", None),
        ("doc.pdf\nx", None),
        ("doc.pdf", None),
        ("notes.txt\nn", None),
    ];
    for (input, want) in cases {
        assert_eq!(reader::key(input).as_deref(), *want, "key {input:?}");
    }
}

#[test]
fn the_hint_line_names_every_key_the_plugin_answers() {
    // One row a key: what it does, and the words the hint line has to use for it. A key added to
    // `reader::key` without a row here is a key the hint line does not name, which is the one way
    // this plugin's UI can lie to the reader.
    let keys: &[(&str, &str)] = &[
        ("n", "n next"),
        ("p", "p prev"),
        ("g3", "g<N> goto"),
        ("z", "z/Z zoom"),
        ("Z", "z/Z zoom"),
        ("0", "0 reset"),
        ("t", "t text"),
        ("q", "q close"),
    ];
    for (key, words) in keys {
        assert!(HINT.contains(words), "the hint line does not name {key:?}");
        if *key == "q" {
            // The host's key: the plugin must not claim it, and the hint must still name it.
            assert!(reader::key(&format!("doc.pdf\n{key}")).is_none());
        } else {
            assert!(
                reader::key(&format!("doc.pdf\n{key}")).is_some(),
                "the hint line names {key:?} but the plugin does not answer it"
            );
        }
    }
}

// ---------------------------------------------------------------- the path

#[test]
fn the_state_fragment_round_trips_through_a_path() {
    let states = [
        State::default(),
        State::default().next(),
        State::default().zoom_in(),
        State::default().toggle_text(),
        State::default().goto(12).zoom_in().toggle_text(),
        State::default().zoom_out().next().next(),
    ];
    for state in states {
        let path = format!("papers/a.pdf{}", state.fragment());
        let back = reader::Request::parse(&path);
        assert_eq!(back.path, "papers/a.pdf", "{path}");
        assert_eq!(back.state, state, "{path}");
        assert_eq!(back.state.mode, state.mode, "{path}");
    }
    assert_eq!(State::default().fragment(), "");
    assert_eq!(State::default().goto(4).fragment(), "#p=4");
    assert_eq!(State::default().toggle_text().fragment(), "#m=text");
}

#[test]
fn a_suffix_that_is_not_state_is_part_of_the_file_name() {
    for path in [
        "a#b.pdf",
        "a.pdf#p=x",
        "a.pdf#p=0",
        "a.pdf#q=2",
        "a.pdf#",
        "a.pdf#p=2&",
        "a.pdf#p=2&junk",
    ] {
        let request = reader::Request::parse(path);
        assert_eq!(request.path, path, "{path}");
        assert_eq!(request.state, State::default(), "{path}");
    }
    // A zoom outside the range is clamped rather than refused: a path is a request, not an error.
    assert_eq!(
        reader::Request::parse("a.pdf#z=9999").state.dpi,
        reader::MAX_DPI
    );
    // The last `#` is the fragment's, so a path that holds one still works.
    assert_eq!(reader::Request::parse("a#b.pdf#p=2").path, "a#b.pdf");
    let request = reader::Request::parse("a.pdf#m=auto&p=2");
    assert_eq!(request.state.page, 2);
    assert_eq!(request.state.mode, Mode::Auto);
}

// ---------------------------------------------------------------- the cache

#[test]
fn the_page_cache_keeps_the_last_few_pages_and_evicts_the_oldest() {
    let doc = cache::id("cache-test.pdf");
    let mut cache = cache::Cache::new(cache::PAGES);
    assert!(cache.is_empty());
    for page in 1..=cache::PAGES as u32 {
        cache.put(cache::Key::new(doc, page), format!("{page}"));
    }
    assert_eq!(cache.len(), cache::PAGES);
    assert_eq!(cache.keys()[0], cache::Key::new(doc, 3));

    // A hit is a hit, and it makes the entry the most recent: page 1 survives the next insert.
    assert_eq!(
        cache.get(cache::Key::new(doc, 1)).map(String::as_str),
        Some("1")
    );
    assert_eq!(cache.keys()[0], cache::Key::new(doc, 1));
    cache.put(cache::Key::new(doc, 4), "4".into());
    assert_eq!(cache.len(), cache::PAGES);
    let keys = cache.keys();
    assert_eq!(keys[0], cache::Key::new(doc, 4));
    assert!(keys.contains(&cache::Key::new(doc, 1)));
    // Page 2 was the least recently used and is the one that went.
    assert!(!keys.contains(&cache::Key::new(doc, 2)), "{keys:?}");
    // Two documents never share an entry.
    assert_ne!(
        cache::Key::new(doc, 1),
        cache::Key::new(cache::id("other.pdf"), 1)
    );
}

#[test]
fn a_page_asked_for_twice_is_rendered_once() {
    // The cache is the reason: a second look inside one call is a hit.
    let _fresh = fresh();
    let mut fake = Fake::pdf(1);
    let v = view("twice.pdf#p=1", 80, 24, &mut fake).unwrap();
    assert!(v.contains("page 1 of the document"));
    let v = view("twice.pdf#p=1", 80, 24, &mut fake).unwrap();
    assert!(v.contains("page 1 of the document"));
    assert_eq!(
        fake.asked_text,
        vec![1],
        "the second view came from the cache"
    );
}
