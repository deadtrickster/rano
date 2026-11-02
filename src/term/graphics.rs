//! Pictures through the kitty protocol's Unicode placeholders: upload, virtual placement, and
//! the rows of placeholder text the terminal fills — placeholders, because the painter diffs
//! rows of text.

use crate::render::{Line, Raw, Span, Style};

/// The protocol's row diacritics: the combining mark after a placeholder that names its row;
/// entry 0 is also column 0's mark. Caps at [`IMAGE_MAX_ROWS`].
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

/// How wide an image may be on a frame `width` columns wide: half of it, clamped to 20..80 and
/// never wider than the frame — the floor of 20 would overflow a narrower pane.
pub fn image_box(width: usize) -> u32 {
    let frame = (width as u32).max(1);
    ((width / 2) as u32).clamp(20, 80).min(frame)
}

/// An image's id from the row it belongs to, so any renderer computes the same number. 24 bits
/// (it rides a colour), never zero.
pub fn image_id(item_id: &str) -> u32 {
    let mut h: u32 = 0x811c_9dc5;
    for b in item_id.bytes() {
        h = (h ^ b as u32).wrapping_mul(0x0100_0193);
    }
    (h & 0x00ff_ffff).max(1)
}

/// Cells an image takes in a box `box_cols` wide: as tall as its aspect says (a cell is ~2×
/// taller than wide), capped, narrowed to keep its shape.
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

/// The rows of text that ARE the image: the placeholder repeated, the first cell carrying its
/// row mark. To every renderer and diff, ordinary text.
pub fn image_rows(id: u32, cols: u32, rows: u32) -> Vec<String> {
    // The image id in the foreground and the placement id in the underline colour — without it
    // a terminal with two placements picks one itself.
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

/// The style of image `id`'s placeholders; see [`image_rows`] for the two colours.
pub fn placeholder_style(id: u32) -> Style {
    Style::new().raw(Raw {
        fg_rgb: Some([(id >> 16) as u8, (id >> 8) as u8, id as u8]),
        underline: Some(PLACEMENT as u8),
    })
}

/// Whether a row is a picture's placeholder row — first cell is [`PLACEHOLDER`]; style it with
/// [`placeholder_style`].
pub fn is_placeholder_row(row: &[char]) -> bool {
    row.first() == Some(&PLACEHOLDER)
}

/// [`image_rows`] as lines for a cell buffer, in [`placeholder_style`].
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

/// Bytes that put a PNG in the terminal's memory under `id`; where it goes is
/// [`image_place`]'s, which a resize repeats without re-sending this.
pub fn image_upload(id: u32, png_base64: &str) -> Vec<u8> {
    upload(&format!("a=t,f=100,i={id}"), png_base64)
}

/// Raw RGB under `id` (`f=24`), for providers that draw pixels and would gain nothing from a
/// PNG encoder. `s=`/`v=` is what the payload is measured against.
pub fn image_upload_rgb(id: u32, width: u32, height: u32, rgb: &[u8]) -> Vec<u8> {
    upload(
        &format!("a=t,f=24,i={id},s={width},v={height}"),
        &crate::term::terminal::base64(rgb),
    )
}

/// One transmission: `head` on the first chunk, 4096-byte chunks, `m=1` on all but the last,
/// `q=2` to keep the terminal quiet.
fn upload(head: &str, base64: &str) -> Vec<u8> {
    let mut out = String::new();
    let chunks: Vec<&[u8]> = base64.as_bytes().chunks(4096).collect();
    for (k, chunk) in chunks.iter().enumerate() {
        let more = if k + 1 < chunks.len() { 1 } else { 0 };
        let data = std::str::from_utf8(chunk).unwrap_or("");
        if k == 0 {
            out.push_str(&format!("\x1b_G{head},q=2,m={more};{data}\x1b\\"));
        } else {
            out.push_str(&format!("\x1b_Gm={more};{data}\x1b\\"));
        }
    }
    out.into_bytes()
}

/// The one placement id this head uses for every image, which its placeholders name.
const PLACEMENT: u32 = 1;

/// Drops an image and every placement — `d=I` frees the data too: an undrawn picture is
/// terminal memory nothing else can free.
pub fn image_delete(id: u32) -> Vec<u8> {
    format!("\x1b_Ga=d,d=I,i={id},q=2\x1b\\").into_bytes()
}

/// The virtual placement the placeholders draw. The image's old placements are deleted first,
/// so one left at another size cannot sit beside the new.
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
        assert_eq!(image_cells(Some(100), Some(1000), 40), (6, IMAGE_MAX_ROWS));
        // Every row is exactly as wide as the placement, at every row the table reaches.
        let rows = image_rows(7, 40, IMAGE_MAX_ROWS);
        assert_eq!(rows.len(), IMAGE_MAX_ROWS as usize);
        for r in rows {
            assert_eq!(crate::width::text::width(&r), 40, "{r:?}");
        }
        let up = String::from_utf8(image_upload(9, &"A".repeat(9000))).unwrap();
        assert_eq!(up.matches("\x1b_G").count(), 3, "three chunks");
        assert!(up.contains("m=1;") && up.contains("\x1b_Gm=0;"));
        assert_eq!(
            String::from_utf8(image_place(9, 40, 5)).unwrap(),
            "\x1b_Ga=d,d=i,i=9,q=2\x1b\\\x1b_Ga=p,U=1,i=9,p=1,c=40,r=5,q=2\x1b\\",
            "the image's old placements go first, then the one the rows name"
        );
        assert!(image_rows(9, 4, 1)[0].contains("\x1b[58;5;1m"));
        assert_ne!(image_id("a"), image_id("b"));
        assert!(image_id("") > 0 && image_id("x") <= 0xff_ffff);
        assert_eq!(
            String::from_utf8(image_delete(9)).unwrap(),
            "\x1b_Ga=d,d=I,i=9,q=2\x1b\\"
        );
    }

    #[test]
    fn an_rgb_upload_states_its_size_and_chunks_like_the_png() {
        let (w, h) = (64u32, 48u32);
        let rgb = vec![0x7fu8; (w * h * 3) as usize];
        let up = String::from_utf8(image_upload_rgb(9, w, h, &rgb)).unwrap();
        assert!(
            up.starts_with("\x1b_Ga=t,f=24,i=9,s=64,v=48,q=2,m=1;"),
            "{:?}",
            &up[..40.min(up.len())]
        );
        let chunks: Vec<&str> = up.split("\x1b\\").filter(|c| !c.is_empty()).collect();
        assert_eq!(chunks.len(), 3, "three chunks");
        fn data(c: &str) -> &str {
            c.rsplit_once(';').unwrap().1
        }
        assert_eq!(data(chunks[0]).len(), 4096);
        assert_eq!(data(chunks[1]).len(), 4096);
        assert_eq!(data(chunks[2]).len(), 4096);
        let joined: String = chunks.iter().map(|c| data(c)).collect();
        assert_eq!(joined.len(), 12288);
        assert_eq!(joined, crate::term::terminal::base64(&rgb), "the pixels");
        assert!(up.contains("\x1b_Gm=1;") && up.contains("\x1b_Gm=0;"));
        assert_eq!(up.matches("a=t,").count(), 1, "one header, the first chunk");
        let png = String::from_utf8(image_upload(9, &"A".repeat(12288))).unwrap();
        assert_eq!(png.matches("\x1b_G").count(), 3);
        assert!(png.starts_with("\x1b_Ga=t,f=100,i=9,q=2,m=1;"));
        assert_eq!(png.matches("\x1b_Gm=").count(), 2, "the continuations");
        let one = String::from_utf8(image_upload_rgb(9, 1, 1, &[1, 2, 3])).unwrap();
        assert_eq!(
            one, "\x1b_Ga=t,f=24,i=9,s=1,v=1,q=2,m=0;AQID\x1b\\",
            "3 bytes is 4 base64 characters, one chunk"
        );
    }

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
            let strip = |s: &str| {
                crate::width::text::cells(s)
                    .iter()
                    .map(|c| c.text)
                    .collect::<String>()
            };
            assert_eq!(strip(emitted).trim_start(), strip(&strings[r]), "row {r}");
            let first = b.cell(2, r as u16).unwrap();
            assert_eq!(first.symbol.chars().count(), 3, "{:?}", first.symbol);
        }
    }
}
