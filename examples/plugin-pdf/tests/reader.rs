//! **The reader, tested natively.** `reader::view`, `reader::picture` and `reader::key` are what
//! the three exports call one line after reading the host's pointer, so testing them is testing
//! the plugin: what the exports add on top is the arena, and that needs wasm32 addresses.
//!
//! The fake renderer is the point of the [`Render`] seam: a document with a known number of pages,
//! a known text layer and a known rasterizer, so "a page past the end" is a case a test can *make*
//! rather than a case it has to find a PDF for.

use rano_plugin_pdf::reader::{self, HINT, Mode, Render, State, view};
use rano_plugin_pdf::{cache, png};

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

type PngFn = Box<dyn Fn(u32, u32) -> Option<Vec<u8>>>;

struct Fake {
    /// One entry a page, page 1 first. `None` is a page the converter does not answer.
    texts: Vec<Option<String>>,
    png: Option<PngFn>,
    can_png: bool,
    can_text: bool,
    asked_png: Vec<(u32, u32)>,
    asked_text: Vec<u32>,
}

impl Fake {
    /// A document of `pages` pages, each with a text layer.
    fn pdf(pages: u32) -> Fake {
        Fake {
            texts: (1..=pages)
                .map(|n| Some(format!("page {n} of the document\nsecond line")))
                .collect(),
            png: None,
            can_png: false,
            can_text: true,
            asked_png: Vec::new(),
            asked_text: Vec::new(),
        }
    }

    /// The same, with a rasterizer that answers a solid page of `w`×`h` at any dpi — and answers
    /// nothing for a page the document does not have, which is how "past the end" reaches the
    /// picture path at all.
    fn rasterized(pages: u32, w: u32, h: u32) -> Fake {
        let mut fake = Fake::pdf(pages);
        fake.can_png = true;
        fake.png = Some(Box::new(move |page, _| {
            (1..=pages)
                .contains(&page)
                .then(|| png_solid(w, h, [10, 20, 30]))
        }));
        fake
    }
}

impl Render for Fake {
    fn can_png(&self) -> bool {
        self.can_png
    }

    fn can_text(&self) -> bool {
        self.can_text
    }

    fn page_png(&mut self, page: u32, dpi: u32) -> Option<Vec<u8>> {
        self.asked_png.push((page, dpi));
        self.png.as_ref()?(page, dpi)
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
    // Still a view with keys, so the reader can press `p` and be somewhere.
    assert!(v.contains("p prev"), "{v}");
    assert!(v.contains("Press p to go back"), "{v}");
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
    assert!(reader::picture("no-renderer.pdf", 80, 24, &mut Nothing).is_none());
}

#[test]
fn a_build_with_a_rasterizer_but_no_text_says_where_the_text_went() {
    let _fresh = fresh();
    let mut fake = Fake::pdf(1);
    fake.can_text = false;
    fake.can_png = true;
    fake.png = Some(Box::new(|_, _| Some(png_solid(2, 2, [1, 2, 3]))));
    let v = view("textless.pdf", 80, 24, &mut fake).unwrap();
    assert!(v.contains("drawn by the host"), "{v}");
}

// ---------------------------------------------------------------- pictures

#[test]
fn a_picture_is_width_height_then_rgb() {
    let _fresh = fresh();
    let mut fake = Fake::rasterized(2, 3, 2);
    let out = reader::picture("picture.pdf", 80, 24, &mut fake).expect("a page comes back");
    assert_eq!(out.len(), 8 + 3 * 2 * 3);
    assert_eq!(u32::from_le_bytes(out[0..4].try_into().unwrap()), 3);
    assert_eq!(u32::from_le_bytes(out[4..8].try_into().unwrap()), 2);
    assert_eq!(&out[8..11], &[10, 20, 30]);
    assert_eq!(&out[8..], [10u8, 20, 30].repeat(6).as_slice());
    // The zoom is asked for, not applied here: the dpi is the reader's state.
    assert_eq!(fake.asked_png, vec![(1, 96)]);
}

#[test]
fn a_page_too_big_for_the_arena_is_rendered_again_smaller() {
    let _fresh = fresh();
    // 2000×2000 is 12 MB of RGB, twice the arena's picture budget; anything below 96 dpi answers
    // a page that fits. The second render is the ladder doing its job.
    let mut fake = Fake::pdf(1);
    fake.can_png = true;
    fake.png = Some(Box::new(|_, dpi| {
        Some(if dpi >= 96 {
            png_solid(2000, 2000, [255, 255, 255])
        } else {
            png_solid(100, 100, [0, 0, 0])
        })
    }));
    let out = reader::picture("huge.pdf", 80, 24, &mut fake).expect("a smaller page");
    assert_eq!(u32::from_le_bytes(out[0..4].try_into().unwrap()), 100);
    assert_eq!(fake.asked_png.len(), 2, "{:?}", fake.asked_png);
    assert!(fake.asked_png[1].1 < 96, "{:?}", fake.asked_png);
}

#[test]
fn text_mode_answers_no_picture_and_a_page_the_host_misses_answers_none() {
    let _fresh = fresh();
    let mut fake = Fake::rasterized(1, 4, 4);
    assert!(reader::picture("miss.pdf#m=text", 80, 24, &mut fake).is_none());
    assert!(reader::picture("miss.pdf#p=9", 80, 24, &mut fake).is_none());
    assert!(reader::picture("notes.txt", 80, 24, &mut fake).is_none());
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
        cache.put(
            cache::Key::text(doc, page),
            cache::Page::Text(format!("{page}")),
        );
    }
    assert_eq!(cache.len(), cache::PAGES);
    assert_eq!(cache.keys()[0], cache::Key::text(doc, 3));

    // A hit is a hit, and it makes the entry the most recent: page 1 survives the next insert.
    assert!(cache.get(cache::Key::text(doc, 1)).is_some());
    assert_eq!(cache.keys()[0], cache::Key::text(doc, 1));
    cache.put(cache::Key::text(doc, 4), cache::Page::Text("4".into()));
    assert_eq!(cache.len(), cache::PAGES);
    let keys = cache.keys();
    assert_eq!(keys[0], cache::Key::text(doc, 4));
    assert!(keys.contains(&cache::Key::text(doc, 1)));
    // Page 2 was the least recently used and is the one that went.
    assert!(!keys.contains(&cache::Key::text(doc, 2)), "{keys:?}");

    // A picture and a text page are different entries, and two documents never share one.
    cache.put(cache::Key::picture(doc, 1, 96), cache::Page::Png(vec![0]));
    assert_ne!(cache::Key::text(doc, 1), cache::Key::picture(doc, 1, 96));
    assert_ne!(
        cache::Key::text(doc, 1),
        cache::Key::text(cache::id("other.pdf"), 1)
    );
}

#[test]
fn a_page_asked_for_twice_is_rendered_once() {
    // The cache is the reason: the picture ladder and a second look inside one call both hit it.
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

// ---------------------------------------------------------------- the PNG the host answers

/// **A PNG of one solid colour**: 8-bit RGB, filter 0 on every row, and zero CRCs — the decoder
/// does not check them (they would be a crc32 table in a plugin that reads its own host's output).
fn png_solid(width: u32, height: u32, rgb: [u8; 3]) -> Vec<u8> {
    let stride = width as usize * 3;
    let mut raw = vec![0u8; (stride + 1) * height as usize];
    for y in 0..height as usize {
        // Filter 0 (none) in front of every row, then the pixels.
        let at = y * (stride + 1) + 1;
        for x in 0..width as usize {
            raw[at + x * 3..at + x * 3 + 3].copy_from_slice(&rgb);
        }
    }
    png_of(width, height, 2, &raw)
}

fn png_of(width: u32, height: u32, color: u8, raw: &[u8]) -> Vec<u8> {
    use flate2::Compression;
    use flate2::write::ZlibEncoder;
    use std::io::Write;

    let mut out = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.extend_from_slice(&[8, color, 0, 0, 0]);
    chunk(&mut out, b"IHDR", &ihdr);
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::fast());
    encoder.write_all(raw).unwrap();
    chunk(&mut out, b"IDAT", &encoder.finish().unwrap());
    chunk(&mut out, b"IEND", &[]);
    out
}

fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    out.extend_from_slice(&[0, 0, 0, 0]);
}

#[test]
fn a_png_decodes_to_rgb_and_its_geometry_reads_without_inflating() {
    let bytes = png_solid(4, 3, [7, 8, 9]);
    let head = png::header(&bytes).expect("an IHDR");
    assert_eq!((head.width, head.height), (4, 3));
    assert_eq!(head.rgb_len(), 4 * 3 * 3);
    let image = png::decode(&bytes).expect("decodes");
    assert_eq!((image.width, image.height), (4, 3));
    assert_eq!(image.rgb, [7u8, 8, 9].repeat(12));

    // Not a PNG, and a PNG with no IHDR: no geometry, and no panic.
    assert!(png::header(b"not a picture at all, just bytes").is_none());
    assert!(png::decode(b"not a picture at all, just bytes").is_err());
}

#[test]
fn the_other_colour_types_and_filters_decode_to_the_same_pixels() {
    // Gray: one byte a pixel, repeated into RGB.
    let mut raw = vec![0u8, 200, 100];
    raw.extend_from_slice(&[0u8, 10, 20]);
    let image = png::decode(&png_of(2, 2, 0, &raw)).expect("gray decodes");
    assert_eq!(
        image.rgb,
        vec![200, 200, 200, 100, 100, 100, 10, 10, 10, 20, 20, 20]
    );

    // RGBA: alpha is dropped, the terminal's background is the page's.
    let raw = vec![0u8, 1, 2, 3, 255, 4, 5, 6, 0];
    let image = png::decode(&png_of(2, 1, 6, &raw)).expect("RGBA decodes");
    assert_eq!(image.rgb, vec![1, 2, 3, 4, 5, 6]);

    // The Up filter: the second row is a delta against the first, and comes out the same.
    let raw = vec![0u8, 9, 9, 9, 2, 0, 0, 0];
    let image = png::decode(&png_of(1, 2, 2, &raw)).expect("Up decodes");
    assert_eq!(image.rgb, vec![9, 9, 9, 9, 9, 9]);

    // The Sub filter: each pixel is a delta against the one to its left.
    let raw = vec![1u8, 10, 10, 10, 5, 5, 5];
    let image = png::decode(&png_of(2, 1, 2, &raw)).expect("Sub decodes");
    assert_eq!(image.rgb, vec![10, 10, 10, 15, 15, 15]);
}

#[test]
fn a_png_this_decoder_does_not_read_is_a_sentence() {
    // 16-bit, interlaced, palette, and truncated data: each is refused by name, none panics.
    let raw = vec![0u8, 1, 2, 3, 4, 5, 6];
    let mut sixteen = png_of(1, 1, 2, &raw);
    sixteen[24] = 16;
    assert!(png::decode(&sixteen).unwrap_err().contains("16-bit"));

    let mut interlaced = png_of(1, 1, 2, &raw);
    interlaced[28] = 1;
    assert!(png::decode(&interlaced).unwrap_err().contains("interlaced"));

    let palette = png_of(1, 1, 3, &raw);
    assert!(png::decode(&palette).unwrap_err().contains("colour type 3"));

    let short = &png_solid(64, 64, [1, 1, 1])[..80];
    assert!(png::decode(short).is_err());
}
