//! File encodings: what the bytes are, and how to keep them that way.
//!
//! Cheapest evidence first: a BOM, else valid UTF-8, else windows-1252, which maps every
//! byte so a legacy file opens rather than being refused; a save then re-encodes it.

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

    /// Whether the file's rows can be split on `0x0A` BYTES, which is what makes
    /// the lazy loader possible: in UTF-8 a `0x0A` is always a newline, while
    /// UTF-16 stores `U+000A` as two bytes and has to be decoded to find rows.
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
        // UTF-32 is not supported and saying so beats misreading it as UTF-16:
        // the caller refuses the file by name instead of opening nonsense.
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

/// How many bytes [`is_binary`] looks at: 8000, git's number, which checks for a
/// zero byte in exactly this many.
pub const BINARY_PROBE: usize = 8000;

/// Whether these bytes are a binary file rather than text: a NUL byte in the
/// first [`BINARY_PROBE`] bytes. UTF-16 has NULs too and is decoded before this
/// matters; without it a `.pyc` would open as mojibake and could be saved back.
pub fn is_binary(bytes: &[u8]) -> bool {
    bytes[..bytes.len().min(BINARY_PROBE)].contains(&0)
}

/// Whether the bytes handed to [`detect`] are the whole file or a prefix: a
/// sequence cut off at the end is the reader's doing in one case and the file's
/// in the other, and only the caller knows which it read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    /// More bytes follow; a truncated final sequence is the reader's doing.
    Prefix,
    /// This is the entire file; a truncated final sequence is the file's.
    Whole,
}

/// Decide the encoding: the BOM if there is one, else UTF-8 if the bytes validate,
/// else windows-1252. A truncated final sequence is the reader's cut in a prefix and
/// the file's own in a whole file, so a prefix must hold a whole offending sequence.
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

/// The scalar a byte maps to in windows-1252: latin-1 everywhere but `0x80`–`0x9F`,
/// where the standard's printable characters (curly quotes, dashes, the euro sign)
/// replace the control codes. The five undefined bytes stay controls, keeping it total.
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
/// The inverse of [`cp1252_char`], asked of it so the two cannot drift.
fn cp1252_byte(c: char) -> Option<u8> {
    let u = c as u32;
    if u < 0x80 || (0xA0..=0xFF).contains(&u) {
        return Some(u as u8);
    }
    // The C1 range is the only part that is not latin-1.
    (0x80..=0x9Fu8).find(|b| cp1252_char(*b) == c)
}

/// Decode `bytes` as `enc`, with the BOM stripped. `Err` for the two cases that
/// are genuinely unreadable: a UTF-16 byte length that cannot hold whole code
/// units, or a lone surrogate.
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
    if !body.len().is_multiple_of(2) {
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

/// Encode `text` as `enc`, BOM included. The inverse of [`decode`], so a file
/// read and written back unchanged produces the same bytes.
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

    /// Every shape a file arrives in, as bytes. Two texts, because cp1252 cannot
    /// hold CJK: the western shape is what such a file really contains, latin-1
    /// letters and the C1 range's curly quotes.
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
        // Detection is a PREFIX operation: any prefix past the BOM, or past the
        // first offending byte, gives the whole file's answer. A legacy file
        // before that byte IS valid UTF-8 — see the note on `detect`.
        for (label, want, bytes) in shapes() {
            let from = if want == Encoding::Cp1252 {
                // One byte past the first invalid one: that byte alone is an
                // incomplete sequence and needs the next to show it is not a
                // continuation.
                bytes
                    .iter()
                    .position(|b| *b >= 0x80)
                    .map(|i| (i + 2).min(bytes.len()))
                    .unwrap_or(bytes.len())
            } else {
                // A BOM is enough on its own.
                want.bom().len().max(1)
            };
            for cut in from..=bytes.len() {
                assert_eq!(
                    detect(&bytes[..cut], Scope::Prefix),
                    want,
                    "{label} cut at {cut}"
                );
            }
            // Shorter than that, only a BOM case is knowable: a legacy file has
            // no BOM, so a short prefix of one is evidence of nothing.
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
        // Pins the limitation `detect` documents: the first bytes of a cp1252
        // file are ASCII, and ASCII is valid UTF-8.
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
        // What `Scope` exists for: the same bytes, and the answer differs.
        let bytes = b"hello caf\xe9"; // cp1252 'é', and an incomplete UTF-8 lead
        assert_eq!(detect(bytes, Scope::Whole), Encoding::Cp1252);
        assert_eq!(detect(bytes, Scope::Prefix), Encoding::Utf8);
    }

    #[test]
    fn a_prefix_cut_mid_sequence_is_still_utf8() {
        // A 3-byte character split across the 64 KiB a caller happens to read.
        let text = "a".repeat(64) + "\u{4e2d}";
        let bytes = text.as_bytes();
        let cut = bytes.len() - 1; // one byte into the character
        assert_eq!(detect(&bytes[..cut], Scope::Prefix), Encoding::Utf8);
    }

    #[test]
    fn a_utf8_bom_is_stripped_and_not_a_column() {
        // A BOM validates, so without stripping it the file opens with U+FEFF as
        // its first character: invisible, but Home and `^`-anchored regexes see
        // it.
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

/// The byte mark: git's NUL rule, and the boundaries it is defined at.
#[cfg(test)]
mod binary_probe {
    use super::*;

    #[test]
    fn a_nul_in_the_first_bytes_is_a_binary() {
        assert!(is_binary(b"\x7fELF\x00\x00\x00\x00"));
        assert!(is_binary(b"\x00"));
        assert!(is_binary(b"plain text, then a NUL: \x00"));
        // A real shape: a Python bytecode header is `\r\r\n` + a NUL and a version word.
        assert!(is_binary(b"\x0d\x0d\x0a\x00"));
    }

    /// Text of every kind rano decodes has no NUL in it, so the rule never fires
    /// on a text file — the cp1252 fallback included, which exists for bytes that
    /// are not UTF-8 but are still text.
    #[test]
    fn text_is_not_a_binary() {
        assert!(!is_binary(b""));
        assert!(!is_binary("fn main() {}\n".as_bytes()));
        assert!(!is_binary("héllo wörld 日本語 🙂\n".as_bytes()));
        // cp1252: 0xE9 is `é` there and not `\0`, so this is text, not binary.
        assert!(!is_binary(b"caf\xe9 au lait\n"));
        assert!(!is_binary(&[0xff; 64]));
    }

    /// Only the first [`BINARY_PROBE`] bytes are looked at, which keeps the rule
    /// cheap and stops one stray byte 2 GB into a log from reclassifying it.
    #[test]
    fn only_the_first_probe_is_read() {
        let mut far = vec![b'x'; BINARY_PROBE + 100];
        far[BINARY_PROBE + 50] = 0;
        assert!(!is_binary(&far), "a NUL past the probe is not evidence");
        let mut near = vec![b'x'; BINARY_PROBE + 100];
        near[BINARY_PROBE - 1] = 0;
        assert!(is_binary(&near), "a NUL inside the probe is");
    }
}
