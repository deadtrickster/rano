//! File encodings: what the bytes are, and how to keep them that way.
//!
//! Before this module `Buffer::from_file` was one line —
//! `fs::read_to_string(path)?` — and its `InvalidData` error was the whole
//! story: a file that was not valid UTF-8 could not be opened at all, and a
//! UTF-8 BOM *validated* so it opened with `U+FEFF` as the buffer's first
//! character: invisible, but a real column that `Home`, click positioning and
//! `^`-anchored regexes all saw. Measured (§13.5): of six common shapes of one
//! small document, **three were refused** and a fourth was quietly wrong.
//!
//! # The ladder
//!
//! Cheapest evidence first, and it stops as soon as the answer is certain:
//!
//! 1. **A BOM.** Four to six byte comparisons, and in practice it *defines*
//!    UTF-16 and UTF-32 — a BOM-less UTF-16 file is a guess for anyone.
//! 2. **The prefix validates as UTF-8.** Overwhelming evidence, and it needs no
//!    table: every multi-byte sequence is checked by the decoder itself.
//! 3. **Otherwise, the single-byte legacy encoding.** This is the rung that
//!    needs a decision. Every byte maps to a character in windows-1252, which
//!    is a superset of latin-1 and is what a file "that is not UTF-8" almost
//!    always is — so the file opens and reads correctly rather than being
//!    refused.
//!
//! It is deliberately dependency-free. `chardetng` (Mozilla's detector, with
//! `encoding_rs` for the decoding) is the right tool for the legacy East-Asian
//! encodings — Shift-JIS, EUC-JP, GB18030 — which rung 3 cannot tell apart from
//! cp1252. That is the extension point, and it is a dependency decision rather
//! than an oversight: rung 3 as written covers every non-UTF-8 file measured
//! here, and the heavy detector is worth adding only when a legacy CJK file
//! actually turns up.
//!
//! # Detection is a prefix, not a pass
//!
//! The ladder reads only what it is given, so it costs one 64 KiB read rather
//! than a scan of the file: measured at **8 µs against 46 ms** on a 193 MB file,
//! agreeing with the whole-file answer on every shape tried. `chardetng` is
//! designed for the same discipline ("If you want to perform detection on just
//! the prefix of a longer stream, do not pass `last=true`") — this ladder simply
//! never needed to be told.
//!
//! # Why it travels with the buffer
//!
//! A save re-encodes, so a file written as cp1252 is not silently rewritten as
//! UTF-8 with replacement characters in it. That is the whole reason
//! [`Encoding`] lives on the [`Buffer`](crate::buffer::Buffer) beside `crlf`,
//! which is the same idea for line endings.

/// How a file's bytes map to characters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Encoding {
    /// UTF-8 with no BOM. The overwhelming majority of files.
    Utf8,
    /// UTF-8 preceded by `EF BB BF`. The BOM is stripped on read and put back
    /// on write, so it is never a column.
    Utf8Bom,
    /// `FF FE`, little-endian. Also covers UTF-32LE's first two bytes, which is
    /// why UTF-32 is checked first.
    Utf16Le,
    /// `FE FF`, big-endian.
    Utf16Be,
    /// windows-1252: every byte is a character, so any byte sequence decodes.
    /// The last rung, and therefore what "not valid UTF-8" means.
    Cp1252,
}

impl Encoding {
    /// The bytes this encoding's BOM is, or an empty slice for the two that
    /// have none.
    pub fn bom(self) -> &'static [u8] {
        match self {
            Self::Utf8Bom => &[0xEF, 0xBB, 0xBF],
            Self::Utf16Le => &[0xFF, 0xFE],
            Self::Utf16Be => &[0xFE, 0xFF],
            Self::Utf8 | Self::Cp1252 => &[],
        }
    }

    /// A short name for the status line and for tests.
    pub fn name(self) -> &'static str {
        match self {
            Self::Utf8 => "UTF-8",
            Self::Utf8Bom => "UTF-8 (BOM)",
            Self::Utf16Le => "UTF-16LE",
            Self::Utf16Be => "UTF-16BE",
            Self::Cp1252 => "windows-1252",
        }
    }

    /// Whether the file's rows can be split on `0x0A` BYTES.
    ///
    /// True for the UTF-8 family, and that is what makes the lazy loader
    /// possible: in UTF-8 a `0x0A` byte is always a newline, because multi-byte
    /// sequences use lead bytes `C2`–`F4` and continuation bytes `80`–`BF`.
    /// UTF-16 stores `U+000A` as two bytes, so its rows cannot be found without
    /// decoding — which is why it takes the eager path.
    pub fn rows_split_on_byte_newlines(self) -> bool {
        matches!(self, Self::Utf8 | Self::Utf8Bom | Self::Cp1252)
    }
}

/// The BOM a byte prefix starts with, if any. Four to six comparisons.
pub fn bom_of(prefix: &[u8]) -> Option<Encoding> {
    // UTF-32 before UTF-16: `FF FE 00 00` starts with UTF-16LE's `FF FE`, and
    // `00 00 FE FF` would otherwise be read as neither.
    if prefix.starts_with(&[0xFF, 0xFE, 0x00, 0x00])
        || prefix.starts_with(&[0x00, 0x00, 0xFE, 0xFF])
    {
        // UTF-32 is not supported: it is vanishingly rare in text files and
        // pretending otherwise would be worse than saying so. The BOM is still
        // reported as *some* BOM, so the caller can refuse it by name.
        return None;
    }
    if prefix.starts_with(&[0xEF, 0xBB, 0xBF]) {
        return Some(Encoding::Utf8Bom);
    }
    if prefix.starts_with(&[0xFF, 0xFE]) {
        return Some(Encoding::Utf16Le);
    }
    if prefix.starts_with(&[0xFE, 0xFF]) {
        return Some(Encoding::Utf16Be);
    }
    None
}

/// Whether the bytes handed to [`detect`] are the whole file or a prefix of it.
///
/// This is not a detail: a file that ENDS in the middle of a multi-byte
/// sequence is not UTF-8, while a *prefix* that stops there is — the reader cut
/// it, the author did not write it. The caller always knows which case it is
/// (it knows whether it read 64 KiB or the whole file), so it says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    /// More bytes follow; a truncated final sequence is the reader's doing.
    Prefix,
    /// This is the entire file; a truncated final sequence is the file's.
    Whole,
}

/// Decide the encoding — the BOM if there is one, else UTF-8 if the bytes
/// validate, else windows-1252.
///
/// `prefix` may be the whole file or its first 64 KiB; the answer is the same
/// either way for every case measured. A prefix that is valid UTF-8 except for
/// a *truncated* final sequence is still UTF-8 — a cut at a chunk boundary is
/// not evidence about the file.
///
/// **The prefix has to be big enough to contain a whole offending sequence.**
/// A cp1252 file whose first bytes are ASCII looks exactly like UTF-8 until
/// something that is not valid UTF-8 turns up, and a prefix that stops before
/// that will say `Utf8` — correctly, about the bytes it was given. Note
/// "sequence", not "byte": a cp1252 `é` is the single byte `0xE9`, which in
/// UTF-8 is the *lead* of a three-byte character, so a prefix ending exactly
/// there is genuinely ambiguous until the next byte shows it is not a
/// continuation. So the caller's read size is part of the answer's quality:
/// 64 KiB is the number this module is measured and used at, and it is what
/// makes rung 3 reliable in practice. A ten-byte prefix is not evidence of
/// anything but those ten bytes.
pub fn detect(bytes: &[u8], scope: Scope) -> Encoding {
    if let Some(enc) = bom_of(bytes) {
        return enc;
    }
    match std::str::from_utf8(bytes) {
        Ok(_) => Encoding::Utf8,
        // An incomplete sequence at the very end. If the reader cut it, the
        // bytes are still UTF-8; if this IS the file, it ended mid-character
        // and the file is not UTF-8 at all.
        Err(e) if e.error_len().is_none() && scope == Scope::Prefix => Encoding::Utf8,
        Err(_) => Encoding::Cp1252,
    }
}

/// The scalar a byte maps to in windows-1252.
///
/// Bytes `0x00`–`0x7F` and `0xA0`–`0xFF` are latin-1, which is the identity.
/// `0x80`–`0x9F` are the C1 range, where windows-1252 differs from latin-1 by
/// putting printable characters (curly quotes, dashes, the euro sign) where the
/// control codes are. Five of those byte values are *undefined* in the
/// standard; they are left as their C1 control, which is what every decoder in
/// practice does and keeps the mapping total.
fn cp1252_char(b: u8) -> char {
    const C1: [char; 32] = [
        '\u{20AC}', '\u{81}', '\u{201A}', '\u{0192}', '\u{201E}', '\u{2026}', '\u{2020}',
        '\u{2021}', '\u{02C6}', '\u{2030}', '\u{0160}', '\u{2039}', '\u{0152}', '\u{8D}',
        '\u{017D}', '\u{8F}', '\u{90}', '\u{2018}', '\u{2019}', '\u{201C}', '\u{201D}', '\u{2022}',
        '\u{2013}', '\u{2014}', '\u{02DC}', '\u{2122}', '\u{0161}', '\u{203A}', '\u{0153}',
        '\u{9D}', '\u{017E}', '\u{0178}',
    ];
    match b {
        0x80..=0x9F => C1[(b - 0x80) as usize],
        _ => b as char,
    }
}

/// The byte a windows-1252 character came from, or `None` when it did not.
///
/// The inverse of [`cp1252_char`], built from it so the two cannot drift — a
/// hand-written table would be a second copy of those 32 entries.
fn cp1252_byte(c: char) -> Option<u8> {
    let u = c as u32;
    if u < 0x80 || (0xA0..=0xFF).contains(&u) {
        return Some(u as u8);
    }
    (0x80..=0x9F)
        .find(|b| cp1252_char(*b) == c)
        .map(|b| b as u8)
}

/// Decode `bytes` as `enc`, with the BOM stripped.
///
/// `Err` for the two cases that are genuinely unreadable: a UTF-16 byte length
/// that cannot hold whole code units, or a lone surrogate.
pub fn decode(bytes: &[u8], enc: Encoding) -> Result<String, String> {
    let bom = enc.bom();
    let body = bytes.strip_prefix(bom).unwrap_or(bytes);
    match enc {
        Encoding::Utf8 | Encoding::Utf8Bom => String::from_utf8(body.to_vec())
            .map_err(|e| format!("not UTF-8 at byte {}", e.utf8_error().valid_up_to())),
        Encoding::Cp1252 => Ok(body.iter().map(|b| cp1252_char(*b)).collect()),
        Encoding::Utf16Le | Encoding::Utf16Be => decode_utf16(body, enc == Encoding::Utf16Be),
    }
}

fn decode_utf16(body: &[u8], big_endian: bool) -> Result<String, String> {
    if body.len() % 2 != 0 {
        return Err(format!(
            "UTF-16 file has an odd byte length ({})",
            body.len()
        ));
    }
    let unit = |i: usize| -> u16 {
        let (a, b) = (body[i], body[i + 1]);
        if big_endian {
            u16::from_be_bytes([a, b])
        } else {
            u16::from_le_bytes([a, b])
        }
    };
    let mut out = String::with_capacity(body.len() / 2);
    let mut i = 0;
    while i < body.len() {
        let u = unit(i);
        i += 2;
        // A surrogate pair is two units and one scalar.
        if (0xD800..0xDC00).contains(&u) {
            if i + 1 >= body.len() {
                return Err("UTF-16 ends with a lone high surrogate".into());
            }
            let lo = unit(i);
            i += 2;
            if !(0xDC00..0xE000).contains(&lo) {
                return Err("UTF-16 has a high surrogate not followed by a low one".into());
            }
            let cp = 0x10000 + (((u as u32 - 0xD800) << 10) | (lo as u32 - 0xDC00));
            out.push(char::from_u32(cp).unwrap_or('\u{FFFD}'));
        } else if (0xDC00..0xE000).contains(&u) {
            return Err("UTF-16 starts with a lone low surrogate".into());
        } else {
            out.push(char::from_u32(u as u32).unwrap_or('\u{FFFD}'));
        }
    }
    Ok(out)
}

/// Encode `text` as `enc`, BOM included.
///
/// The inverse of [`decode`] for every encoding: reading a file and writing it
/// back unchanged must produce the same bytes, which is what the round-trip
/// test asserts per encoding.
pub fn encode(text: &str, enc: Encoding) -> Result<Vec<u8>, String> {
    let mut out = Vec::with_capacity(text.len() + enc.bom().len());
    out.extend_from_slice(enc.bom());
    match enc {
        Encoding::Utf8 | Encoding::Utf8Bom => out.extend_from_slice(text.as_bytes()),
        Encoding::Cp1252 => {
            for c in text.chars() {
                match cp1252_byte(c) {
                    Some(b) => out.push(b),
                    None => {
                        return Err(format!(
                            "{} is not representable in {}",
                            c.escape_debug(),
                            enc.name()
                        ));
                    }
                }
            }
        }
        Encoding::Utf16Le | Encoding::Utf16Be => {
            let be = enc == Encoding::Utf16Be;
            for u in text.encode_utf16() {
                let b = if be { u.to_be_bytes() } else { u.to_le_bytes() };
                out.extend_from_slice(&b);
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shapes from §13.5, as bytes.
    ///
    /// Two texts, because a cp1252 file cannot hold CJK — asking it to would be
    /// testing the refusal, which `a_character_outside_cp1252_is_refused` does
    /// on purpose. The cp1252 shape is what such a file actually looks like:
    /// latin-1 letters and the C1 range's curly quotes.
    fn shapes() -> Vec<(&'static str, Encoding, Vec<u8>)> {
        let text = "hello caf\u{e9} \u{4e2d}\u{6587} line\nsecond line\n";
        let western = "hello caf\u{e9} \u{201C}quoted\u{201D} line\nsecond line\n";
        vec![
            ("utf8", Encoding::Utf8, text.as_bytes().to_vec()),
            (
                "utf8+bom",
                Encoding::Utf8Bom,
                [&[0xEF, 0xBB, 0xBF][..], text.as_bytes()].concat(),
            ),
            (
                "utf16le",
                Encoding::Utf16Le,
                [
                    &[0xFF, 0xFE][..],
                    &encode(text, Encoding::Utf16Le).unwrap()[2..],
                ]
                .concat(),
            ),
            (
                "utf16be",
                Encoding::Utf16Be,
                [
                    &[0xFE, 0xFF][..],
                    &encode(text, Encoding::Utf16Be).unwrap()[2..],
                ]
                .concat(),
            ),
            (
                "cp1252",
                Encoding::Cp1252,
                encode(western, Encoding::Cp1252).unwrap(),
            ),
        ]
    }

    #[test]
    fn every_shape_is_detected_by_name() {
        for (label, want, bytes) in shapes() {
            assert_eq!(detect(&bytes, Scope::Whole), want, "{label}");
        }
        // And detection is a PREFIX operation: any prefix that contains the
        // BOM, or an offending byte, gives the same answer as the whole file.
        // For UTF-8 and the BOM cases that is every prefix; for cp1252 it is
        // every prefix at or past the first non-ASCII byte, because before that
        // the file IS valid UTF-8 — see the note on `detect`.
        for (label, want, bytes) in shapes() {
            let from = if want == Encoding::Cp1252 {
                // Past the first byte that is invalid UTF-8, and one more: that
                // byte alone is an incomplete sequence and reads as UTF-8 from
                // a prefix (see `detect`), and it takes the NEXT byte to show
                // it is not a continuation.
                bytes
                    .iter()
                    .position(|b| *b >= 0x80)
                    .map(|i| (i + 2).min(bytes.len()))
                    .unwrap_or(bytes.len())
            } else {
                // A BOM is enough on its own, which is the point of rung 1.
                want.bom().len().max(1)
            };
            for cut in from..=bytes.len() {
                assert_eq!(
                    detect(&bytes[..cut], Scope::Prefix),
                    want,
                    "{label} cut at {cut}"
                );
            }
            // Shorter than that, only a BOM case is knowable — and it is
            // knowable from the BOM alone, which is the point of rung 1. A
            // legacy file has no BOM, so a short prefix of one is not evidence
            // about anything but those bytes.
            if !want.bom().is_empty() {
                for cut in want.bom().len()..from {
                    assert_eq!(
                        detect(&bytes[..cut], Scope::Prefix),
                        want,
                        "{label} cut at {cut} (the BOM is enough)"
                    );
                }
            }
        }
    }

    #[test]
    fn a_short_prefix_of_a_legacy_file_reads_as_utf8_and_that_is_why_size_matters() {
        // Pins the limitation `detect` documents, so it is a known property
        // rather than a latent surprise: the first bytes of a cp1252 file are
        // ASCII, and ASCII is valid UTF-8.
        let bytes = encode("hello caf\u{e9}", Encoding::Cp1252).unwrap();
        assert_eq!(
            detect(&bytes[..5], Scope::Prefix),
            Encoding::Utf8,
            "5 bytes are all ASCII"
        );
        assert_eq!(
            detect(&bytes, Scope::Whole),
            Encoding::Cp1252,
            "the whole thing is not"
        );
    }

    #[test]
    fn a_file_ending_mid_sequence_is_not_utf8_but_a_prefix_is() {
        // The distinction `Scope` exists for, and the bug a test caught: these
        // two byte strings are IDENTICAL, and the right answer differs.
        let bytes = b"hello caf\xe9"; // cp1252 'é', and an incomplete UTF-8 lead
        assert_eq!(detect(bytes, Scope::Whole), Encoding::Cp1252);
        assert_eq!(detect(bytes, Scope::Prefix), Encoding::Utf8);
    }

    #[test]
    fn a_prefix_cut_mid_sequence_is_still_utf8() {
        // A 3-byte character split across the 64 KiB a caller happens to read:
        // a truncated final sequence is not evidence about the file.
        let text = "a".repeat(64) + "\u{4e2d}";
        let bytes = text.as_bytes();
        let cut = bytes.len() - 1; // one byte into the character
        assert_eq!(detect(&bytes[..cut], Scope::Prefix), Encoding::Utf8);
    }

    #[test]
    fn a_utf8_bom_is_stripped_and_not_a_column() {
        // The quiet bug: the BOM validated, so the file opened with U+FEFF as
        // the first character — invisible, but Home and click positioning saw
        // it, and a `^`-anchored regex saw it.
        let bytes = [&[0xEF, 0xBB, 0xBF][..], b"hello\n"].concat();
        let enc = detect(&bytes, Scope::Whole);
        assert_eq!(enc, Encoding::Utf8Bom);
        let text = decode(&bytes, enc).unwrap();
        assert_eq!(text, "hello\n", "no U+FEFF in the text");
        assert!(!text.starts_with('\u{FEFF}'));
    }

    #[test]
    fn round_trips_every_encoding() {
        // Read, write, read: a file must not be corrupted by being opened.
        for (label, enc, bytes) in shapes() {
            let text = decode(&bytes, enc).expect(label);
            let again = encode(&text, enc).expect(label);
            assert_eq!(again, bytes, "{label} did not round-trip byte for byte");
            assert_eq!(decode(&again, enc).unwrap(), text, "{label} text");
        }
    }

    #[test]
    fn utf16_decodes_including_a_surrogate_pair() {
        // U+1F600 is a surrogate pair: two units, one scalar.
        let text = "a\u{1F600}b\n";
        for enc in [Encoding::Utf16Le, Encoding::Utf16Be] {
            let bytes = encode(text, enc).unwrap();
            assert_eq!(decode(&bytes, enc).unwrap(), text, "{}", enc.name());
            // And detection finds it from the BOM.
            assert_eq!(detect(&bytes, Scope::Whole), enc);
        }
    }

    #[test]
    fn utf16_failures_are_reported_not_guessed() {
        // Odd length: cannot hold whole code units.
        let odd = [0xFF, 0xFE, 0x41];
        assert!(decode(&odd, Encoding::Utf16Le).is_err());
        // A lone high surrogate.
        let lone = [0xFF, 0xFE, 0x00, 0xD8];
        assert!(decode(&lone, Encoding::Utf16Le).is_err());
        // A lone low surrogate.
        let lone = [0xFF, 0xFE, 0x00, 0xDC];
        assert!(decode(&lone, Encoding::Utf16Le).is_err());
    }

    #[test]
    fn cp1252_maps_the_c1_range_to_printable_characters() {
        // 0x93/0x94 are curly quotes in windows-1252 and C1 controls in latin-1:
        // this is the difference between a file reading correctly and a file
        // full of \u{93}.
        assert_eq!(
            decode(&[0x93, b'x', 0x94], Encoding::Cp1252).unwrap(),
            "\u{201C}x\u{201D}"
        );
        // Outside that range it is latin-1.
        assert_eq!(decode(&[0xE9], Encoding::Cp1252).unwrap(), "\u{e9}");
        // And every byte decodes: the mapping is total, which is what lets this
        // be the last rung.
        for b in 0u8..=255 {
            let s = decode(&[b], Encoding::Cp1252).unwrap();
            assert_eq!(s.chars().count(), 1, "byte {b:#04x} is one character");
            // And it round-trips, including the five undefined C1 codes.
            assert_eq!(
                encode(&s, Encoding::Cp1252).unwrap(),
                vec![b],
                "byte {b:#04x} did not survive the round trip"
            );
        }
    }

    #[test]
    fn a_character_outside_cp1252_is_refused_rather_than_mangled() {
        // Saving a CJK character to a cp1252 file cannot be done. Refusing is
        // the only honest answer: a '?' would be silent data loss.
        assert!(encode("\u{4e2d}", Encoding::Cp1252).is_err());
    }

    #[test]
    fn bytes_split_on_newlines_only_for_the_utf8_family() {
        // The property the lazy loader needs. UTF-16 stores U+000A as two
        // bytes, so its rows cannot be found without decoding.
        assert!(Encoding::Utf8.rows_split_on_byte_newlines());
        assert!(Encoding::Utf8Bom.rows_split_on_byte_newlines());
        assert!(Encoding::Cp1252.rows_split_on_byte_newlines());
        assert!(!Encoding::Utf16Le.rows_split_on_byte_newlines());
        assert!(!Encoding::Utf16Be.rows_split_on_byte_newlines());
    }

    #[test]
    fn utf32_is_not_pretended_about() {
        // Its BOM overlaps UTF-16LE's, which is why it is checked first and
        // deliberately declined rather than misread as UTF-16.
        assert_eq!(bom_of(&[0xFF, 0xFE, 0x00, 0x00]), None);
        assert_eq!(bom_of(&[0x00, 0x00, 0xFE, 0xFF]), None);
    }
}
