//! **A picture on the glass, whichever protocol the terminal speaks**: the rows a frame draws, the
//! bytes the terminal must be told, and the decisions that keep both honest.
//!
//! [`crate::term::graphics`] is the kitty protocol's own spelling — chunks, placements,
//! placeholders, deletion. This is the layer above it: *one picture in a box*, and *the set of
//! pictures a frame draws*, answered through [`Proto`] so a caller that supports two protocols
//! writes one call and not two branches.
//!
//! # The two protocols differ in the one thing a frame cares about
//!
//! * **Kitty draws by placeholder.** The frame's rows are cells the terminal fills from bytes it
//!   holds, so a redraw costs nothing, a scroll costs nothing, and the picture can be put at any
//!   column of any row. That is why [`Picture::lines`] returns the box's rows.
//! * **iTerm2 draws at the cursor, into cells.** There is no id, so nothing to place and nothing
//!   to keep: a picture is its bytes, sent once each time the cells under it are painted. The
//!   frame's rows are the box's *blank* rows, and the bytes must be written with the cursor at the
//!   box's top-left corner ([`Picture::bytes`] says so at its own definition).
//!
//! Both have the same contract with a caller: **the frame first, the bytes after it** — the rows
//! are what the frame draws where the picture is, and the bytes are what the terminal needs to
//! fill them.
//!
//! # Why the set of pictures is a value and not a call
//!
//! A picture's bytes are the largest thing a terminal holds on this program's behalf, and the
//! reason [`Pictures`] exists is that sending them twice is the bug this layer is for: it records
//! what each id was last sent *at*, so an unchanged frame sends nothing, a box that moved sends
//! the placement and not the bytes ([`crate::term::graphics::image_place`] says why), and a picture
//! the frame no longer draws is deleted **with its data** — the terminal cannot free what nothing
//! names.
//!
//! # A picture is not always a PNG
//!
//! A file's picture is a PNG, because that is what the terminal decodes. **A plugin's picture is
//! pixels** — `provider_picture` answers `[width][height][rgb]` — and the host does not turn them
//! into a PNG: there is no encoder here, and the protocol takes raw RGB directly (`f=24`). That is
//! [`Picture::of_rgb`], and the one protocol that cannot carry it says so in
//! [`Picture::refusal`] before anything is written.

use std::collections::BTreeMap;

use crate::render::Line;
use crate::term::graphics;
use crate::term::proto::Proto;
use crate::term::terminal::base64;

/// **A picture, ready to be drawn in a box**: the id the terminal knows it by, the payload's bytes
/// as base64 when it is a PNG (what both protocols carry) or as raw RGB when it is a plugin's
/// pixels, and the box in cells.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Picture {
    /// The image id. **Stable per picture** — hashed from whatever names it upstream
    /// ([`graphics::image_id`] for a row), because an id that changes every frame is a terminal
    /// filling with copies of one picture.
    pub id: u32,
    /// The PNG, base64: what kitty's upload carries, and what iTerm2's `File=` carries. Empty for
    /// a picture that is raw pixels ([`Picture::rgb`]).
    pub png_base64: String,
    /// The picture's own size in bytes — iTerm2's `size=` argument, and the only reason it is
    /// kept: the base64's length is 4/3 of it and a terminal that is told the wrong one refuses
    /// the picture.
    pub bytes: usize,
    /// **The pixels, when the picture is not a PNG** — a plugin's `provider_picture` answer, sent
    /// as the protocol's raw-RGB transmission instead of a file.
    pub rgb: Option<Rgb>,
    /// The box, in cells: what the frame draws and what the terminal fills.
    pub cells: (u32, u32),
}

/// **A picture as pixels**: what a plugin draws, in the one form the terminal takes without an
/// image format in between — 8 bits per channel, row-major, `width × height × 3` bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rgb {
    pub width: u32,
    pub height: u32,
    /// Exactly `width × height × 3` bytes. [`Picture::of_rgb`] is what guarantees it.
    pub data: Vec<u8>,
}

/// **The header a provider's answer starts with**, before its pixels: `width` and `height` as
/// little-endian `u32`s. [`Picture::of_rgb`] is the whole of the format.
pub const PAYLOAD_HEADER: usize = 8;

impl Picture {
    /// A picture from the PNG's own bytes, cut to the box the frame will draw it in.
    pub fn of(id: u32, png: &[u8], cells: (u32, u32)) -> Picture {
        Picture {
            id,
            png_base64: base64(png),
            bytes: png.len(),
            rgb: None,
            cells,
        }
    }

    /// The same, from base64 that is already in hand (a row's `data_ref`, a converter's output):
    /// the size is the base64's own arithmetic — four characters are three bytes, less the
    /// padding.
    pub fn from_base64(id: u32, png_base64: &str, cells: (u32, u32)) -> Picture {
        let padded = png_base64.ends_with('=') as usize + png_base64.ends_with("==") as usize;
        let bytes = (png_base64.len() / 4) * 3 - padded.min(2);
        Picture {
            id,
            png_base64: png_base64.to_string(),
            bytes,
            rgb: None,
            cells,
        }
    }

    /// **A picture from the payload a plugin's `provider_picture` answered**:
    /// `[width: u32 LE][height: u32 LE][rgb: width × height × 3]`, cut to the box a frame
    /// `frame_width` columns wide gives it ([`graphics::image_cells`], the arithmetic a PNG's view
    /// cuts with).
    ///
    /// **A payload whose length disagrees with the size it states is refused**, and so is a
    /// zero-sized or unmeasurable one. What a bad header draws is not a small mistake: it is
    /// whatever the bytes happen to be — pixels of the wrong shape, at the wrong place, held by a
    /// terminal until the id is deleted.
    pub fn of_rgb(
        id: u32,
        width: u32,
        height: u32,
        rgb: &[u8],
        frame_width: usize,
    ) -> Result<Picture, String> {
        if width == 0 || height == 0 {
            return Err(format!("a picture of {width}×{height} pixels"));
        }
        if rgb.len() != (width as usize) * (height as usize) * 3 {
            return Err(format!(
                "{width}×{height} pixels is {} bytes, got {}",
                (width as usize) * (height as usize) * 3,
                rgb.len()
            ));
        }
        let cells =
            graphics::image_cells(Some(width), Some(height), graphics::image_box(frame_width));
        Ok(Picture {
            id,
            png_base64: String::new(),
            bytes: rgb.len(),
            rgb: Some(Rgb {
                width,
                height,
                data: rgb.to_vec(),
            }),
            cells,
        })
    }

    /// **The picture's own size in pixels**, when it carries it: raw RGB states it in its header,
    /// and a PNG's view keeps it beside the picture ([`crate::diffview::Source::Picture`]).
    pub fn intrinsic(&self) -> Option<(u32, u32)> {
        self.rgb.as_ref().map(|r| (r.width, r.height))
    }

    /// **Re-cut the box for a frame `width` columns wide**, returning whether it moved — what a
    /// resize asks a picture whose rows and placement both follow the frame.
    pub fn refit(&mut self, frame_width: usize) -> bool {
        let Some((w, h)) = self.intrinsic() else {
            return false;
        };
        let cells = graphics::image_cells(Some(w), Some(h), graphics::image_box(frame_width));
        if cells == self.cells {
            return false;
        }
        self.cells = cells;
        true
    }

    /// **Why this picture cannot be drawn on this terminal, or `None` when it can.** A caller asks
    /// this before it writes anything: a picture sent to a terminal that cannot carry it is not a
    /// picture that is missing, it is cells of garbage (placeholders nothing fills) or bytes typed
    /// at the prompt.
    pub fn refusal(&self, proto: Proto) -> Option<String> {
        match proto {
            Proto::None => Some(
                "This terminal does not take inline pictures: RANO_TERM_FEATURES=images forces them"
                    .to_string(),
            ),
            // iTerm2's protocol carries an image FILE (base64 PNG, JPEG, GIF), not a pixel
            // buffer, and this host has no PNG encoder — so a plugin's picture needs a terminal
            // that takes raw RGB, which is kitty's.
            Proto::Iterm if self.rgb.is_some() => Some(
                "This picture is pixels: it needs a terminal that takes raw RGB (kitty), not iTerm2's file protocol"
                    .to_string(),
            ),
            _ => None,
        }
    }

    /// **The transmission this picture needs**: the raw RGB where it is pixels, the PNG where it
    /// is a file.
    fn upload(&self) -> Vec<u8> {
        match &self.rgb {
            Some(r) => graphics::image_upload_rgb(self.id, r.width, r.height, &r.data),
            None => graphics::image_upload(self.id, &self.png_base64),
        }
    }

    /// **The rows a frame draws where the picture is** — the box's placeholder rows under kitty,
    /// the box's blank rows under iTerm2 (the bytes fill them), and nothing at all where the
    /// terminal takes no picture: the caller draws its own fallback there.
    pub fn lines(&self, proto: Proto) -> Vec<Line> {
        let (cols, rows) = self.cells;
        match proto {
            Proto::None => Vec::new(),
            Proto::Kitty | Proto::KittyPassthrough => graphics::image_lines(self.id, cols, rows),
            Proto::Iterm => (0..rows).map(|_| Line::default()).collect(),
        }
    }

    /// **The bytes the terminal must be told about this picture**, in the order they must be
    /// written, already wrapped for the multiplexer ([`Proto::wrap`]).
    ///
    /// **Kitty**: the upload, then the placement — the second is what makes the
    /// rows above show it, and a resize repeats only this one. **iTerm2**: one `OSC 1337;File=`
    /// carrying the whole picture, which the terminal draws *at the cursor and into cells* — so
    /// the caller writes it after the frame, with the cursor moved to the box's top-left corner,
    /// and writes it again whenever those cells are repainted. That is the protocol's cost and
    /// not this function's: it has no id to place by, so there is nothing cheaper to send.
    pub fn bytes(&self, proto: Proto) -> Vec<Vec<u8>> {
        let (cols, rows) = self.cells;
        match proto {
            Proto::None => Vec::new(),
            Proto::Kitty | Proto::KittyPassthrough => vec![
                proto.wrap(&self.upload()),
                proto.wrap(&graphics::image_place(self.id, cols, rows)),
            ],
            // **A picture that is pixels has no iTerm2 escape**: that protocol carries a file, so
            // there is nothing to send. [`Picture::refusal`] is what the caller asked first.
            Proto::Iterm if self.rgb.is_some() => Vec::new(),
            Proto::Iterm => vec![proto.wrap(self.iterm_escape().as_bytes())],
        }
    }

    /// **The iTerm2 escape, whole**: `ESC ] 1337 ; File=inline=1;…;size=…;width=…;height=…:<b64> BEL`.
    ///
    /// `width` and `height` in **cells** (a bare number is cells, which is what a frame knows),
    /// with `preserveAspectRatio=1` so a box that is a cell out does not squash the picture. The
    /// `size` is the PNG's byte length and not the base64's, which is the mistake a terminal
    /// answers by refusing the file.
    pub fn iterm_escape(&self) -> String {
        let (cols, rows) = self.cells;
        format!(
            "\x1b]1337;File=inline=1;preserveAspectRatio=1;size={};width={cols};height={rows}:{}\x07",
            self.bytes, self.png_base64
        )
    }

    /// **The bytes that free this picture in the terminal** — kitty deletes the image and its
    /// data; iTerm2 has nothing to free, because its picture *is* the cells it was drawn into (a
    /// frame that repaints them has already replaced it).
    pub fn delete(&self, proto: Proto) -> Vec<u8> {
        delete_cmd(proto, self.id)
    }
}

/// **The bytes that free image `id`**, in this protocol's own terms: kitty deletes the image with
/// its data, and iTerm2 has nothing to free — its picture is the cells it was drawn into, and a
/// frame that repaints them has already replaced it.
fn delete_cmd(proto: Proto, id: u32) -> Vec<u8> {
    match proto {
        Proto::None | Proto::Iterm => Vec::new(),
        Proto::Kitty | Proto::KittyPassthrough => proto.wrap(&graphics::image_delete(id)),
    }
}

/// **The pictures a frame draws, kept between frames** — what each id was last sent at, so a
/// redraw that changes nothing sends nothing.
#[derive(Debug, Clone, Default)]
pub struct Pictures {
    proto: Proto,
    held: BTreeMap<u32, (u32, u32)>,
}

impl Pictures {
    pub fn new(proto: Proto) -> Pictures {
        Pictures {
            proto,
            held: BTreeMap::new(),
        }
    }

    /// **Changed its mind about the terminal** — a resize is this, a re-attach through a pane is
    /// this. Everything the terminal was holding is forgotten (the caller is told to delete it,
    /// see [`Pictures::forget`]) because a protocol that changed is not one where the old ids can
    /// be trusted to mean the same thing.
    pub fn set_proto(&mut self, proto: Proto) -> Vec<Vec<u8>> {
        if proto == self.proto {
            return Vec::new();
        }
        let out = self.forget();
        self.proto = proto;
        out
    }

    pub fn proto(&self) -> Proto {
        self.proto
    }

    /// **The bytes are already in the terminal** — a host that sent this picture itself, under
    /// this id, before the widget was handed it (letibot draws an operator's attachment as a
    /// thumbnail with the conversation's own uploader and then opens the same picture larger in a
    /// popup). The next [`Pictures::sync`] **places** it instead of sending megabytes a second
    /// time, a box that changes still re-places, and [`Pictures::forget`] still deletes — one id is
    /// one image in the terminal whichever layer sent it.
    pub fn adopt(&mut self, id: u32, cells: (u32, u32)) {
        self.held.insert(id, cells);
    }

    /// **What the terminal must be told to make this frame whole**: a picture it does not hold
    /// uploaded and placed, one whose box moved placed again (no bytes), one the frame no longer
    /// draws deleted. Nothing at all for a frame that changed nothing.
    pub fn sync(&mut self, want: &[Picture]) -> Vec<Vec<u8>> {
        // **A picture nothing draws goes first, and takes its data with it**: the terminal's
        // memory is not this process's to leave lying about, and the rows that named it have
        // already gone.
        let stale: Vec<u32> = self
            .held
            .keys()
            .copied()
            .filter(|id| !want.iter().any(|p| p.id == *id))
            .collect();
        let mut out = Vec::new();
        for id in stale {
            self.held.remove(&id);
            let cmd = delete_cmd(self.proto, id);
            if !cmd.is_empty() {
                out.push(cmd);
            }
        }
        for p in want {
            match self.held.get(&p.id) {
                // Sent, at the box it is still drawn in: nothing to say.
                Some(cells) if *cells == p.cells => continue,
                // Sent at another box: the placement alone — the bytes are the terminal's
                // already.
                Some(_) => {
                    self.held.insert(p.id, p.cells);
                    out.extend(p.bytes_for_place(self.proto));
                }
                None => {
                    self.held.insert(p.id, p.cells);
                    out.extend(p.bytes(self.proto));
                }
            }
        }
        out
    }

    /// **Drop every picture the terminal holds** — a view that closed, a frame that is no longer
    /// drawn at all.
    pub fn forget(&mut self) -> Vec<Vec<u8>> {
        let ids: Vec<u32> = std::mem::take(&mut self.held).into_keys().collect();
        let mut out = Vec::new();
        for id in ids {
            let cmd = delete_cmd(self.proto, id);
            if !cmd.is_empty() {
                out.push(cmd);
            }
        }
        out
    }

    /// The pictures the terminal holds, for a caller that has to ask.
    pub fn held(&self) -> Vec<(u32, (u32, u32))> {
        self.held.iter().map(|(id, c)| (*id, *c)).collect()
    }
}

impl Picture {
    /// The placement alone, for a picture whose bytes the terminal already holds.
    fn bytes_for_place(&self, proto: Proto) -> Vec<Vec<u8>> {
        match proto {
            Proto::None => Vec::new(),
            Proto::Kitty | Proto::KittyPassthrough => {
                vec![proto.wrap(&graphics::image_place(self.id, self.cells.0, self.cells.1))]
            }
            // iTerm2 has no placement: a picture whose box moved is a picture to send again,
            // which is what a frame that repainted those cells costs on that terminal.
            Proto::Iterm if self.rgb.is_some() => Vec::new(),
            Proto::Iterm => vec![proto.wrap(self.iterm_escape().as_bytes())],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::Palette;

    /// A picture in a 4×2 box, from a few bytes: the escapes are what is under test and no
    /// decoder is asked anything.
    fn pic(id: u32, cells: (u32, u32)) -> Picture {
        Picture::of(id, b"png-bytes", cells)
    }

    #[test]
    fn a_picture_is_the_box_the_frame_draws_and_the_bytes_that_fill_it() {
        let p = pic(7, (4, 2));
        // Kitty: two rows of placeholders, each the box's width, in the id's colour.
        let rows = p.lines(Proto::Kitty);
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|l| l.width() == 4), "{rows:?}");
        let mut b = crate::render::Buffer::empty(crate::render::Rect::new(0, 0, 6, 2));
        for (i, l) in rows.iter().enumerate() {
            b.set_line(0, i as u16, l, 6);
        }
        // The id through a binding, not a literal: `(7 >> 16) & 0xff` is clippy's
        // `erasing_op`, and the shifts are what the colour is made of.
        let id: u32 = 7;
        let colour = format!(
            "38;2;{};{};{};58;5;1",
            (id >> 16) & 0xff,
            (id >> 8) & 0xff,
            id & 0xff
        );
        assert!(b.emit(Palette::Colour)[0].contains(&colour));

        // The bytes: the upload, then the placement — in that order, and wrapped for nobody.
        let bytes = p.bytes(Proto::Kitty);
        assert_eq!(bytes.len(), 2);
        assert_eq!(bytes[0], graphics::image_upload(7, &p.png_base64));
        assert_eq!(bytes[1], graphics::image_place(7, 4, 2));
        // Behind tmux both are wrapped, and the placement is still the second.
        let wrapped = p.bytes(Proto::KittyPassthrough);
        assert_eq!(wrapped.len(), 2);
        assert!(wrapped[1].starts_with(b"\x1bPtmux;"));
        // A terminal that takes no picture is told nothing and draws nothing: the caller's
        // fallback is the whole of what the reader gets.
        assert!(p.lines(Proto::None).is_empty());
        assert!(p.bytes(Proto::None).is_empty());
        assert!(p.delete(Proto::None).is_empty());
    }

    /// **iTerm2's protocol has no placeholders and no id**: the frame draws the box's blank rows
    /// and the escape draws the picture into them.
    #[test]
    fn iterm2_gets_blank_rows_and_one_escape_carrying_the_whole_picture() {
        let p = Picture::of(3, b"12345678", (10, 3));
        let rows = p.lines(Proto::Iterm);
        assert_eq!(rows.len(), 3);
        assert!(rows.iter().all(|l| l.width() == 0), "{rows:?}");
        let bytes = p.bytes(Proto::Iterm);
        assert_eq!(bytes.len(), 1, "no upload and placement, one escape");
        assert_eq!(
            String::from_utf8(bytes[0].clone()).unwrap(),
            format!(
                "\x1b]1337;File=inline=1;preserveAspectRatio=1;size=8;width=10;height=3:{}\x07",
                p.png_base64
            )
        );
        // The size is the PNG's, not the base64's — a terminal told 4/3 of it refuses the file.
        assert_eq!(p.bytes, 8);
        assert_eq!(p.png_base64, base64(b"12345678"));
        // Nothing to free: the picture is the cells it was drawn in.
        assert!(p.delete(Proto::Iterm).is_empty());
        // And a `from_base64` picture counts the same size back, padding and all.
        let from = Picture::from_base64(3, &p.png_base64, (10, 3));
        assert_eq!(from.bytes, 8);
        assert_eq!(Picture::from_base64(1, "Zg==", (1, 1)).bytes, 1);
        assert_eq!(Picture::from_base64(1, "Zm8=", (1, 1)).bytes, 2);
        assert_eq!(Picture::from_base64(1, "Zm9v", (1, 1)).bytes, 3);
    }

    /// **An unchanged frame sends nothing and a moved box sends only the placement**, and a picture
    /// whose bytes another layer already sent is placed rather than sent again (`adopt`).
    #[test]
    fn an_unchanged_frame_sends_nothing_and_a_moved_box_sends_only_the_placement() {
        let mut held = Pictures::new(Proto::Kitty);
        let mut out = held.sync(&[pic(1, (4, 2))]);
        assert_eq!(out.len(), 2, "upload and placement: {out:?}");
        assert!(held.sync(&[pic(1, (4, 2))]).is_empty(), "nothing changed");
        assert_eq!(held.held(), vec![(1, (4, 2))]);
        // The window moved: the terminal has the bytes already, so only where changes.
        out = held.sync(&[pic(1, (8, 4))]);
        assert_eq!(out, vec![graphics::image_place(1, 8, 4)]);
        // A second picture is uploaded beside the first, and neither is re-sent.
        out = held.sync(&[pic(1, (8, 4)), pic(2, (3, 1))]);
        assert_eq!(out.len(), 2);
        assert!(out[0].starts_with(b"\x1b_Ga=t"));
        // Gone from the frame: deleted, and the data with it — in id order, which is the order
        // the map holds them in.
        out = held.sync(&[]);
        assert_eq!(
            out,
            vec![graphics::image_delete(1), graphics::image_delete(2)]
        );
        assert!(held.held().is_empty());
        assert!(held.sync(&[]).is_empty(), "nothing left to delete");
    }

    /// **A picture whose bytes another layer sent is placed, not sent again** — the operator's
    /// thumbnail and the enlarged view of it are one image in the terminal (`adopt`).
    #[test]
    fn an_adopted_picture_is_placed_and_never_uploaded_twice() {
        let mut held = Pictures::new(Proto::Kitty);
        // The thumbnail's uploader sent it at 4×2 and this widget is handed it from there.
        held.adopt(1, (4, 2));
        let mut out = held.sync(&[pic(1, (4, 2))]);
        assert!(out.is_empty(), "it was uploaded again: {out:?}");
        // The reader opened it larger: a placement, and only that.
        out = held.sync(&[pic(1, (12, 6))]);
        assert_eq!(out, vec![graphics::image_place(1, 12, 6)]);
        // And closing the view still frees it: one id is one image whichever layer sent it.
        assert_eq!(held.sync(&[]), vec![graphics::image_delete(1)]);
    }

    /// **Under iTerm2 a picture whose box moved is a picture to send again** — it has no
    /// placement, and the cells it was drawn in are not the cells it is drawn in now.
    #[test]
    fn iterm2_re_sends_the_bytes_when_the_box_moves_and_never_deletes() {
        let mut held = Pictures::new(Proto::Iterm);
        let one = pic(1, (4, 2));
        let mut out = held.sync(std::slice::from_ref(&one));
        assert_eq!(out.len(), 1);
        assert!(held.sync(std::slice::from_ref(&one)).is_empty());
        let wider = pic(1, (6, 3));
        out = held.sync(std::slice::from_ref(&wider));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0], Proto::Iterm.wrap(held_escape(1, (6, 3)).as_bytes()));
        // Dropping it says nothing: there is nothing in the terminal to free.
        assert!(held.sync(&[]).is_empty());
    }

    fn held_escape(id: u32, cells: (u32, u32)) -> String {
        pic(id, cells).iterm_escape()
    }

    /// **A plugin's answer is a picture**: the pixels it named, cut to the box a PNG would be
    /// cut to at the same width — and a size that disagrees with the bytes is refused by name
    /// rather than drawn.
    #[test]
    fn a_plugin_picture_is_a_picture_and_a_lying_size_is_refused() {
        // 64×48: 9216 bytes of RGB, which is three chunks of upload (see `graphics`).
        let (w, h) = (64u32, 48u32);
        let rgb = vec![0x40u8; (w * h * 3) as usize];
        let p = Picture::of_rgb(5, w, h, &rgb, 120).expect("a size that agrees");
        assert_eq!(p.intrinsic(), Some((w, h)));
        assert_eq!(
            p.cells,
            graphics::image_cells(Some(w), Some(h), graphics::image_box(120))
        );
        assert_eq!(p.rgb.as_ref().unwrap().data.len(), 9216);
        assert!(p.png_base64.is_empty(), "it is not a PNG");
        // The bytes: the raw-RGB upload, then the placement — the PNG's order, its own header.
        let bytes = p.bytes(Proto::Kitty);
        assert_eq!(bytes.len(), 2);
        assert_eq!(bytes[0], graphics::image_upload_rgb(5, w, h, &rgb));
        assert!(String::from_utf8_lossy(&bytes[0]).starts_with("\x1b_Ga=t,f=24,i=5,s=64,v=48"));
        assert_eq!(bytes[1], graphics::image_place(5, p.cells.0, p.cells.1));
        // The rows are the placeholders the terminal fills, as for any picture.
        let rows = p.lines(Proto::Kitty);
        assert_eq!(rows.len(), p.cells.1 as usize);
        assert!(rows.iter().all(|l| l.width() == p.cells.0 as usize));

        // **A size whose bytes disagree is refused**, and the sentence says both numbers.
        let e = Picture::of_rgb(5, w, h, &rgb[..rgb.len() - 1], 120).unwrap_err();
        assert!(e.contains("64×48") && e.contains("9215"), "{e}");
        // A size with no pixels in it is not a picture of nothing.
        assert!(Picture::of_rgb(5, 0, 48, &[], 120).is_err());
    }

    /// **A resize re-cuts the box, not the pixels** — the rows and the placement follow the
    /// frame, and the upload is the terminal's already.
    #[test]
    fn a_resize_moves_a_providers_picture_and_the_bytes_stay() {
        let rgb = vec![1u8; 200 * 100 * 3];
        let mut p = Picture::of_rgb(4, 200, 100, &rgb, 120).expect("a picture");
        let before = p.cells;
        assert!(p.refit(40));
        assert_ne!(p.cells, before, "a narrower frame is a smaller box");
        assert!(!p.refit(40), "the same frame changes nothing");
        // A PNG view is cut elsewhere (`diffview::cells_for` keeps its own size), so refit says
        // nothing about one.
        let mut png = pic(4, (4, 2));
        assert!(!png.refit(40));
        assert_eq!(png.cells, (4, 2));
    }

    /// **A picture is refused by name where the terminal cannot carry it** — never drawn as
    /// placeholders nothing fills, and never as an iTerm2 escape with no file in it.
    #[test]
    fn a_picture_that_is_pixels_is_refused_by_a_terminal_that_cannot_take_it() {
        let p = Picture::of_rgb(6, 2, 2, &[9u8; 12], 80).expect("a picture");
        assert!(p.refusal(Proto::Kitty).is_none());
        assert!(p.refusal(Proto::KittyPassthrough).is_none());
        let none = p.refusal(Proto::None).expect("no protocol, a refusal");
        assert!(none.contains("does not take inline pictures"), "{none}");
        let iterm = p.refusal(Proto::Iterm).expect("no raw RGB there either");
        assert!(iterm.contains("raw RGB"), "{iterm}");
        // And nothing is written for it on that terminal: an escape with an empty payload is a
        // file the terminal refuses.
        assert!(p.bytes(Proto::None).is_empty());
        assert!(p.bytes(Proto::Iterm).is_empty());
        // A PNG is still a PNG on iTerm2: the refusal is about pixels, not pictures.
        assert!(pic(3, (4, 2)).refusal(Proto::Iterm).is_none());
        assert_eq!(pic(3, (4, 2)).bytes(Proto::Iterm).len(), 1);
    }

    /// **A terminal that takes no picture is sent no bytes at all** — not "some bytes it will
    /// ignore", which in a terminal is text typed at the prompt.
    #[test]
    fn a_terminal_without_pictures_is_written_nothing() {
        let mut held = Pictures::new(Proto::None);
        assert!(held.sync(&[pic(1, (4, 2)), pic(2, (2, 2))]).is_empty());
        assert!(held.forget().is_empty());
        // And a protocol that changed drops what the old one held, because the ids cannot be
        // trusted to mean the same thing across the two.
        let mut held = Pictures::new(Proto::Kitty);
        held.sync(&[pic(1, (4, 2))]);
        assert_eq!(held.set_proto(Proto::None), vec![graphics::image_delete(1)]);
        assert!(held.held().is_empty());
        assert!(
            held.set_proto(Proto::None).is_empty(),
            "no change, no bytes"
        );
    }
}
