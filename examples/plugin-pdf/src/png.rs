//! **PNG to RGB**, because that is the one conversion the ABI asks of this plugin: the host
//! rasterizes a page and hands back PNG bytes, and `provider_picture` answers
//! `[width][height][rgb…]`. The decoder is deliberately small:
//!
//! - 8-bit, non-interlaced, colour types 0 (gray), 2 (RGB), 4 (gray+alpha) and 6 (RGBA) — what
//!   `pdftoppm`, `pdftocairo` and the pdfium bindings actually emit. Palette and 16-bit images are
//!   refused with a sentence rather than guessed at, and a page from a rasterizer is never one.
//! - **CRCs are not checked.** The bytes come from the host's own rasterizer on this machine, not
//!   from a network, and a page that arrives corrupt fails the inflate or the filter check anyway.
//! - Nothing is cached here; [`crate::cache`] holds the pages, and this module holds no state.

use miniz_oxide::inflate::stream::InflateState;
use miniz_oxide::{DataFormat, MZFlush, MZStatus};

/// The eight bytes every PNG starts with.
const SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];

/// The largest side this decoder will believe. A page is at most a few thousand pixels wide at
/// any dpi a terminal can show; a header claiming more is a file that is not a page.
pub const MAX_SIDE: u32 = 20_000;

/// What an IHDR says, and all `provider_picture` needs to know before it decides whether a page
/// fits: the geometry is in the first 33 bytes, so a page too big for the arena is refused without
/// ever being inflated.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    pub width: u32,
    pub height: u32,
}

impl Header {
    /// The RGB bytes this header promises.
    pub fn rgb_len(&self) -> usize {
        self.width as usize * self.height as usize * 3
    }
}

/// **A decoded page**: RGB, three bytes a pixel, row-major.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub rgb: Vec<u8>,
}

/// **The geometry, without inflating anything.** `None` when the bytes are not a PNG this decoder
/// would read — a signature or an IHDR that is missing, or a side that is not a page's.
pub fn header(bytes: &[u8]) -> Option<Header> {
    if bytes.len() < 33 || bytes[..8] != SIGNATURE || &bytes[12..16] != b"IHDR" {
        return None;
    }
    let width = u32::from_be_bytes(bytes[16..20].try_into().ok()?);
    let height = u32::from_be_bytes(bytes[20..24].try_into().ok()?);
    if width == 0 || height == 0 || width > MAX_SIDE || height > MAX_SIDE {
        return None;
    }
    Some(Header { width, height })
}

/// **Decode a page.** Every failure is a sentence, because the caller has a buffer to put it in.
///
/// **The work is one pass and no per-pixel copies**, because a hook has 50 million units of fuel
/// for a whole call and a page at 96 dpi is a million pixels: the inflated scanlines are unfiltered
/// **in place**, each row sliding down over its filter byte, and a colour-type-2 page (what every
/// rasterizer emits) is then already the answer — the RGB bytes are the unfiltered buffer, and
/// nothing is copied or converted at all. Only grey and RGBA need a per-pixel pass.
pub fn decode(bytes: &[u8]) -> Result<Image, String> {
    let head = header(bytes).ok_or("not a PNG, or not one with a page's geometry")?;
    let depth = bytes[24];
    let color = bytes[25];
    let interlace = bytes[28];
    if depth != 8 {
        return Err(format!("a {depth}-bit PNG; only 8 bits a channel is read"));
    }
    if interlace != 0 {
        return Err("an interlaced PNG; only a plain one is read".to_string());
    }
    // Bytes a pixel, which is also the distance a Sub/Paeth filter looks back.
    let bpp = match color {
        0 => 1,
        2 => 3,
        4 => 2,
        6 => 4,
        // 1, 2, 3, 7: bit depths packed into a byte, and a palette. A rasterizer emits none.
        _ => {
            return Err(format!(
                "a PNG with colour type {color}, which a page never is"
            ));
        }
    };
    let w = head.width as usize;
    let h = head.height as usize;
    let stride = w * bpp;
    let mut raw = inflate_into(bytes, h * (stride + 1))?;
    unfilter_rows(&mut raw, h, stride, bpp)?;
    // Colour type 2 is what a rasterizer emits, and its unfiltered rows *are* the answer: the RGB
    // bytes with the filter bytes gone. Truncating is a length change, not a copy.
    raw.truncate(h * stride);
    if bpp != 3 {
        raw = to_rgb(&raw, bpp);
    }
    Ok(Image {
        width: head.width,
        height: head.height,
        rgb: raw,
    })
}

/// **Grey and alpha into RGB**: the one pass a page of a rasterizer never needs. Grey (with or
/// without alpha) repeats its one channel; alpha is dropped, because the terminal's background is
/// the page's background.
fn to_rgb(pixels: &[u8], bpp: usize) -> Vec<u8> {
    let mut rgb = vec![0u8; pixels.len() / bpp * 3];
    let (out, _) = rgb.as_chunks_mut::<3>();
    for (px, out) in pixels.chunks_exact(bpp).zip(out.iter_mut()) {
        out[0] = px[0];
        out[1] = if bpp <= 2 { px[0] } else { px[1] };
        out[2] = if bpp <= 2 { px[0] } else { px[2] };
    }
    rgb
}

/// **Unfilter in place.** Row `y` arrives as a filter byte and `stride` bytes; after this it is at
/// `y * stride`, and the filter bytes are gone. The row above is already unfiltered, one `stride`
/// back, which is what the Up, Average and Paeth filters read.
///
/// The row and the row above are two disjoint slices of the same buffer, so each filter runs over
/// slices rather than through indices: a filter is most of a page's work and this is the difference
/// between ~35 units of fuel a byte and a handful.
fn unfilter_rows(raw: &mut [u8], h: usize, stride: usize, bpp: usize) -> Result<(), String> {
    let mut src = 0;
    let mut dst = 0;
    for y in 0..h {
        let filter = raw[src];
        raw.copy_within(src + 1..src + 1 + stride, dst);
        let (before, rest) = raw.split_at_mut(dst);
        let row = &mut rest[..stride];
        // The row above, already unfiltered: the `stride` bytes before this one. Empty on row 0,
        // which is the row that has nothing above it.
        let up: &[u8] = if y > 0 { &before[dst - stride..] } else { &[] };
        match filter {
            // None: the bytes are already the pixels. A rasterizer picks this for a blank row,
            // and it is the one filter that costs nothing.
            0 => {}
            1 => {
                for i in bpp..stride {
                    row[i] = row[i].wrapping_add(row[i - bpp]);
                }
            }
            2 => {
                for (byte, above) in row.iter_mut().zip(up) {
                    *byte = byte.wrapping_add(*above);
                }
            }
            3 => {
                for i in 0..stride {
                    let left = if i >= bpp { row[i - bpp] as u16 } else { 0 };
                    let above = if y > 0 { up[i] as u16 } else { 0 };
                    row[i] = row[i].wrapping_add(((left + above) / 2) as u8);
                }
            }
            4 => {
                for i in 0..stride {
                    let left = if i >= bpp { row[i - bpp] as i16 } else { 0 };
                    let above = if y > 0 { up[i] as i16 } else { 0 };
                    let corner = if y > 0 && i >= bpp {
                        up[i - bpp] as i16
                    } else {
                        0
                    };
                    row[i] = row[i].wrapping_add(paeth(left, above, corner));
                }
            }
            other => return Err(format!("a row filter ({other}) that is not a PNG filter")),
        }
        src += stride + 1;
        dst += stride;
    }
    Ok(())
}

/// **The concatenated IDAT payload, inflated into the buffer it is going to be.** The size is known
/// from the header, so the output is allocated once and the inflater writes into it: miniz's
/// `decompress_to_vec_zlib` grows a `Vec` a byte at a time, which is 40 units of fuel a byte in
/// wasm — more than the whole rest of the decode.
fn inflate_into(bytes: &[u8], want: usize) -> Result<Vec<u8>, String> {
    let idat = idat(bytes)?;
    let mut out = vec![0u8; want];
    let mut state = InflateState::new(DataFormat::Zlib);
    let (mut used, mut written) = (0, 0);
    while written < want {
        let step = miniz_oxide::inflate::stream::inflate(
            &mut state,
            &idat[used..],
            &mut out[written..],
            MZFlush::None,
        );
        used += step.bytes_consumed;
        written += step.bytes_written;
        match step.status {
            Ok(MZStatus::StreamEnd) => break,
            Ok(_) => {}
            Err(e) => return Err(format!("a PNG whose data does not inflate: {e:?}")),
        }
        // No progress and not done: the stream is short, or it wants more input than there is.
        if step.bytes_consumed == 0 && step.bytes_written == 0 {
            return Err(format!("a PNG that stops after {written} of {want} bytes"));
        }
    }
    if written < want {
        return Err(format!("short image data: {written} bytes for {want}"));
    }
    Ok(out)
}

/// The IDAT payloads, concatenated. Every other chunk is skipped: the pages this reads come from a
/// rasterizer, which writes one IHDR, the IDATs and an IEND.
fn idat(bytes: &[u8]) -> Result<Vec<u8>, String> {
    let mut idat: Vec<u8> = Vec::new();
    let mut at = 8;
    while at + 8 <= bytes.len() {
        let len = u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
        let kind = &bytes[at + 4..at + 8];
        let data = at + 8;
        let Some(end) = data.checked_add(len).filter(|e| *e <= bytes.len()) else {
            return Err("a truncated PNG chunk".to_string());
        };
        if kind == b"IDAT" {
            idat.extend_from_slice(&bytes[data..end]);
        } else if kind == b"IEND" {
            break;
        }
        // The four CRC bytes after the data, which are not checked (see the module docs).
        at = end + 4;
    }
    if idat.is_empty() {
        return Err("a PNG with no IDAT".to_string());
    }
    Ok(idat)
}

/// The Paeth predictor: whichever of left, up and corner is nearest `left + up - corner`.
fn paeth(left: i16, up: i16, corner: i16) -> u8 {
    let p = left + up - corner;
    let (pa, pb, pc) = ((p - left).abs(), (p - up).abs(), (p - corner).abs());
    if pa <= pb && pa <= pc {
        left as u8
    } else if pb <= pc {
        up as u8
    } else {
        corner as u8
    }
}
