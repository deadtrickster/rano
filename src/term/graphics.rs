//! **Pictures in the terminal**, through the kitty graphics protocol's Unicode placeholders:
//! the upload, the virtual placement, and the rows of placeholder text the terminal fills with
//! the image. Placeholders rather than a pixel placement because the painter draws by diffing
//! rows of text — see [`image_rows`].
//!
//! Ported from letibot's `crates/tui/src/backend/graphics.rs`. [`image_rows`] is letibot's
//! string form, kept for its string-painting callers; [`image_lines`] is the same rows as
//! [`Line`]s for a [`crate::render::Buffer`], where the placeholder and its diacritics are one
//! cell and the ids ride in the style's [`Raw`] colours.

use crate::render::{Line, Raw, Span, Style};

/// **The kitty graphics protocol's row diacritics**: the combining mark after a placeholder
/// that says which row of the image this cell is. The protocol's own table, from its start; an
/// image here is capped at [`IMAGE_MAX_ROWS`] rows, so the first that many are all it uses. The
/// first entry is also column 0's mark.
const ROW_DIACRITICS: [char; 30] = [
    '\u{0305}', '\u{030D}', '\u{030E}', '\u{0310}', '\u{0312}', '\u{033D}', '\u{033E}', '\u{033F}',
    '\u{0346}', '\u{034A}', '\u{034B}', '\u{034C}', '\u{0350}', '\u{0351}', '\u{0352}', '\u{0357}',
    '\u{035B}', '\u{0363}', '\u{0364}', '\u{0365}', '\u{0366}', '\u{0367}', '\u{0368}', '\u{0369}',
    '\u{036A}', '\u{036B}', '\u{036C}', '\u{036D}', '\u{036E}', '\u{036F}',
];

/// The placeholder the terminal replaces with a slice of the image.
const PLACEHOLDER: char = '\u{10EEEE}';

/// An image is at most this many cells tall.
pub const IMAGE_MAX_ROWS: u32 = ROW_DIACRITICS.len() as u32;

/// **How wide an image may be on a frame `width` columns wide**: half of it, never under 20
/// and never over 80 — **and never wider than the frame itself**, which the floor of 20 can
/// be: a 16-column pane (a split, a phone) would otherwise place a 20-column picture in a
/// 15-column view, and the terminal fills the cells it was told to and draws over the edge.
/// The operator, on the first picture drawn: it was 40 columns on a window five times that.
/// The uploader and every renderer ask this one function with the same width, so the rows a
/// renderer draws always match the placement the terminal holds.
pub fn image_box(width: usize) -> u32 {
    let frame = (width as u32).max(1);
    ((width / 2) as u32).clamp(20, 80).min(frame)
}

/// **An image's id, from the row it belongs to** — so any renderer can draw a row's
/// placeholders without being handed a table, and the head that uploads it computes the same
/// number. Twenty-four bits, because the id rides in a 24-bit foreground colour; never zero.
pub fn image_id(item_id: &str) -> u32 {
    let mut h: u32 = 0x811c_9dc5;
    for b in item_id.bytes() {
        h = (h ^ b as u32).wrapping_mul(0x0100_0193);
    }
    (h & 0x00ff_ffff).max(1)
}

/// The cells an image takes inside a box `box_cols` wide: the box's width, as tall as its
/// aspect says (a cell is about twice as tall as it is wide), capped — and narrowed to keep its
/// shape when the cap bites. An image whose header gave no size gets a box of fixed shape.
pub fn image_cells(width: Option<u32>, height: Option<u32>, box_cols: u32) -> (u32, u32) {
    let (Some(w), Some(h)) = (width.filter(|w| *w > 0), height.filter(|h| *h > 0)) else {
        return (box_cols, (box_cols * 3 / 10).clamp(1, IMAGE_MAX_ROWS));
    };
    let rows = (box_cols as u64 * h as u64).div_ceil(2 * w as u64) as u32;
    if rows <= IMAGE_MAX_ROWS {
        return (box_cols, rows.max(1));
    }
    let cols = (2 * IMAGE_MAX_ROWS as u64 * w as u64).div_ceil(h as u64) as u32;
    (cols.clamp(1, box_cols), IMAGE_MAX_ROWS)
}

/// **The rows of text that ARE the image**, once it has been uploaded under `id` with a
/// virtual placement of `cols`×`rows`: each row is the placeholder repeated, its first cell
/// carrying the row's mark and column 0's (the rest of the row's columns follow from it), all
/// in a foreground colour that spells the id. To every renderer, width count and diff here they
/// are ordinary text — which is why this, and not a pixel placement, is how the head draws one.
pub fn image_rows(id: u32, cols: u32, rows: u32) -> Vec<String> {
    // The image id in the foreground and **the placement id in the underline colour** — the
    // protocol's way for a placeholder to say which placement it means. Without it a terminal
    // holding two placements of one image picks one itself: the operator's second screenshot was
    // a 40×12 picture drawn by an earlier head's placement in the corner of an 80×24 box.
    let colour = format!(
        "\x1b[38;2;{};{};{}m\x1b[58;5;{PLACEMENT}m",
        (id >> 16) & 0xff,
        (id >> 8) & 0xff,
        id & 0xff
    );
    (0..rows.min(IMAGE_MAX_ROWS))
        .map(|r| {
            let mut row = colour.clone();
            row.push(PLACEHOLDER);
            row.push(ROW_DIACRITICS[r as usize]);
            row.push(ROW_DIACRITICS[0]);
            for _ in 1..cols {
                row.push(PLACEHOLDER);
            }
            row.push_str("\x1b[39m\x1b[59m");
            row
        })
        .collect()
}

/// The style every placeholder cell of image `id` is drawn in: the id in a 24-bit foreground,
/// the placement in the underline colour. See [`image_rows`] for why both.
pub fn placeholder_style(id: u32) -> Style {
    Style::new().raw(Raw {
        fg_rgb: Some([(id >> 16) as u8, (id >> 8) as u8, id as u8]),
        underline: Some(PLACEMENT as u8),
    })
}

/// [`image_rows`] as lines for a cell buffer: the same clusters, one span each row, in
/// [`placeholder_style`].
pub fn image_lines(id: u32, cols: u32, rows: u32) -> Vec<Line> {
    let st = placeholder_style(id);
    (0..rows.min(IMAGE_MAX_ROWS))
        .map(|r| {
            let mut row = String::new();
            row.push(PLACEHOLDER);
            row.push(ROW_DIACRITICS[r as usize]);
            row.push(ROW_DIACRITICS[0]);
            for _ in 1..cols {
                row.push(PLACEHOLDER);
            }
            Line::new(vec![Span::styled(row, st.clone())])
        })
        .collect()
}

/// **The bytes that put a PNG in the terminal's memory under `id`**: the base64 sent in the
/// protocol's 4096-byte chunks, quiet (`q=2`) so the terminal sends nothing back to be typed.
/// Where it is drawn is [`image_place`]'s, which a resize repeats without re-sending this.
pub fn image_upload(id: u32, png_base64: &str) -> Vec<u8> {
    let mut out = String::new();
    let chunks: Vec<&[u8]> = png_base64.as_bytes().chunks(4096).collect();
    for (k, chunk) in chunks.iter().enumerate() {
        let more = if k + 1 < chunks.len() { 1 } else { 0 };
        let data = std::str::from_utf8(chunk).unwrap_or("");
        if k == 0 {
            out.push_str(&format!("\x1b_Ga=t,f=100,i={id},q=2,m={more};{data}\x1b\\"));
        } else {
            out.push_str(&format!("\x1b_Gm={more};{data}\x1b\\"));
        }
    }
    out.into_bytes()
}

/// The one placement id this head uses for every image, which its placeholders name.
const PLACEMENT: u32 = 1;

/// **The bytes that drop an image and every placement of it** — what a picture view sends
/// on its way out, and when the box it is placed in moves. The uppercase `d=I` is the
/// protocol's "the data too": a picture nobody draws is memory in the terminal that nothing
/// else can free, and the rows that named it are gone from the screen already.
pub fn image_delete(id: u32) -> Vec<u8> {
    format!("\x1b_Ga=d,d=I,i={id},q=2\x1b\\").into_bytes()
}

/// **The virtual placement the placeholders draw**: `cols`×`rows` cells of image `id`, under
/// placement [`PLACEMENT`]. Every placement the image already has is deleted first (`d=i`,
/// which keeps the image's data) — one left by an earlier head, or by this one at another size,
/// would otherwise sit beside the new one.
pub fn image_place(id: u32, cols: u32, rows: u32) -> Vec<u8> {
    format!(
        "\x1b_Ga=d,d=i,i={id},q=2\x1b\\\x1b_Ga=p,U=1,i={id},p={PLACEMENT},c={cols},r={rows},q=2\x1b\\"
    )
    .into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_image_keeps_its_shape_inside_the_box() {
        assert_eq!(image_box(30), 20, "never under 20");
        assert_eq!(image_box(120), 60, "half the frame");
        assert_eq!(image_box(400), 80, "never over 80");
        // **But never wider than the frame**, which the 20-column floor is on a narrow view:
        // a split pane or a phone. What is drawn has to fit where it is drawn.
        assert_eq!(image_box(15), 15, "a 15-column frame takes 15");
        assert_eq!(image_box(1), 1);
        assert_eq!(
            image_box(0),
            1,
            "a frame nobody has set is not zero cells wide"
        );
        assert_eq!(image_cells(None, Some(10), 40), (40, 12));
        assert_eq!(image_cells(Some(400), Some(100), 40), (40, 5));
        assert_eq!(image_cells(Some(720), Some(420), 80), (80, 24));
        // Tall: the rows cap, and the width narrows to keep the shape.
        assert_eq!(image_cells(Some(100), Some(1000), 40), (6, IMAGE_MAX_ROWS));
        // Every row is exactly as wide as the placement, at every row the table reaches.
        let rows = image_rows(7, 40, IMAGE_MAX_ROWS);
        assert_eq!(rows.len(), IMAGE_MAX_ROWS as usize);
        for r in rows {
            assert_eq!(crate::width::text::width(&r), 40, "{r:?}");
        }
        // Long payloads go in the protocol's 4096-byte chunks; the placement is its own
        // command, under a fixed placement id so a resize replaces it.
        let up = String::from_utf8(image_upload(9, &"A".repeat(9000))).unwrap();
        assert_eq!(up.matches("\x1b_G").count(), 3, "three chunks");
        assert!(up.contains("m=1;") && up.contains("\x1b_Gm=0;"));
        assert_eq!(
            String::from_utf8(image_place(9, 40, 5)).unwrap(),
            "\x1b_Ga=d,d=i,i=9,q=2\x1b\\\x1b_Ga=p,U=1,i=9,p=1,c=40,r=5,q=2\x1b\\",
            "the image's old placements go first, then the one the rows name"
        );
        // The rows name the placement, not only the image.
        assert!(image_rows(9, 4, 1)[0].contains("\x1b[58;5;1m"));
        assert_ne!(image_id("a"), image_id("b"));
        assert!(image_id("") > 0 && image_id("x") <= 0xff_ffff);
        // Dropping one drops the data, not only where it was drawn.
        assert_eq!(
            String::from_utf8(image_delete(9)).unwrap(),
            "\x1b_Ga=d,d=I,i=9,q=2\x1b\\"
        );
    }

    /// **The placeholders pass through a cell buffer intact**: the same clusters as the
    /// string rows, the same width, and the ids in the colours the protocol reads them from.
    #[test]
    fn image_lines_through_a_buffer_are_the_image_rows() {
        use crate::render::{Buffer, Palette, Rect};
        let id = image_id("item-7");
        let (cols, rows) = (12u32, 4u32);
        let mut b = Buffer::empty(Rect::new(0, 0, 20, rows as u16));
        for (i, l) in image_lines(id, cols, rows).iter().enumerate() {
            b.set_line(2, i as u16, l, 20);
        }
        let colour = format!(
            "38;2;{};{};{};58;5;{PLACEMENT}m",
            (id >> 16) & 0xff,
            (id >> 8) & 0xff,
            id & 0xff
        );
        let strings = image_rows(id, cols, rows);
        for (r, emitted) in b.emit(Palette::Colour).iter().enumerate() {
            assert!(emitted.contains(&colour), "{emitted:?}");
            assert_eq!(crate::width::text::width(emitted), 2 + cols as usize);
            // The visible clusters are letibot's, character for character.
            let strip = |s: &str| {
                crate::width::text::cells(s)
                    .iter()
                    .map(|c| c.text)
                    .collect::<String>()
            };
            assert_eq!(strip(emitted).trim_start(), strip(&strings[r]), "row {r}");
            // The first cell holds the placeholder with both its marks.
            let first = b.cell(2, r as u16).unwrap();
            assert_eq!(first.symbol.chars().count(), 3, "{:?}", first.symbol);
        }
    }
}
