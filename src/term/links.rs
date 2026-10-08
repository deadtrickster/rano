//! **Hyperlinks** (OSC 8): a file named on a row, and every URL on a frame. Zero columns wide,
//! so nothing that measures a row has to know they are there.
//!
//! Ported from letibot's `crates/tui/src/backend/links.rs`. [`file_link`] and [`link_urls`] are
//! letibot's string forms; [`file_url`] and [`link_urls_in`] are the same decisions for a
//! [`Line`], where a link is a [`crate::render::Style::link`] and the emitter writes the bytes.

use crate::render::{Line, Span};

/// `text` as a link to the file `target` names — absolute, or relative to `root` — when
/// `target` is one path and nothing else. Anything else (a quoted command, a pattern with a
/// path after it, a call id) comes back as it was.
///
/// The URL carries this machine's name, so a link drawn by a head on the far end of `ssh`
/// does not open the same-named file on the near one.
pub fn file_link(root: &str, target: &str, text: &str) -> String {
    match file_url(root, target) {
        Some(url) => format!("\x1b]8;;{url}\x1b\\{text}\x1b]8;;\x1b\\"),
        None => text.to_string(),
    }
}

/// The `file://` URL [`file_link`] would link to, or `None` when `target` is not one path.
/// For a [`crate::render::Style::link`].
pub fn file_url(root: &str, target: &str) -> Option<String> {
    let t = target.trim();
    if t.is_empty()
        || t.starts_with('"')
        || t.starts_with('(')
        || t.chars().any(char::is_whitespace)
    {
        return None;
    }
    let abs = if t.starts_with('/') {
        t.to_string()
    } else {
        format!("{}/{t}", root.trim_end_matches('/'))
    };
    Some(format!("file://{}{}", host_name(), uri_path(&abs)))
}

/// **Every `http://` and `https://` in a rendered row, made a link.** Only inside a plain text
/// run: an escape sequence is copied through untouched, and a URL broken by one is left as
/// text rather than half-linked. Trailing sentence punctuation is not part of the URL.
pub fn link_urls(row: &str) -> String {
    let b = row.as_bytes();
    let mut out = String::with_capacity(row.len() + 64);
    let mut i = 0;
    // Inside a link somebody already drew, the text is that link's and is left alone.
    let mut in_link = false;
    while i < b.len() {
        if b[i] == 0x1b {
            let end = escape_end(b, i);
            let seq = &row[i..end];
            if let Some(body) = seq.strip_prefix("\x1b]8;") {
                let uri = body.split_once(';').map(|(_, u)| u).unwrap_or("");
                in_link = !(uri.is_empty() || uri.starts_with('\x1b') || uri.starts_with('\x07'));
            }
            out.push_str(seq);
            i = end;
            continue;
        }
        let rest = &row[i..];
        if !in_link && let Some(end) = url_at(rest) {
            let url = &rest[..end];
            out.push_str(&format!("\x1b]8;;{url}\x1b\\{url}\x1b]8;;\x1b\\"));
            i += end;
            continue;
        }
        let c = rest.chars().next().unwrap_or(' ');
        out.push(c);
        i += c.len_utf8();
    }
    out
}

/// The length of the URL that `rest` starts with, or `None` when it does not start with one.
/// Trailing sentence punctuation is not part of the URL.
fn url_at(rest: &str) -> Option<usize> {
    if !(rest.starts_with("https://") || rest.starts_with("http://")) {
        return None;
    }
    let mut end = rest
        .find(|c: char| {
            c.is_whitespace() || c == '\x1b' || matches!(c, '"' | '\'' | '<' | '>' | '`')
        })
        .unwrap_or(rest.len());
    while end > 0 && rest[..end].ends_with(['.', ',', ';', ':', '!', '?', ')', ']']) {
        // A closing bracket is the URL's own only while the URL has opened more of
        // them than it has closed — `…/c_(d)` keeps its `)`, `(…/c_(d))` gives one back.
        let url = &rest[..end];
        let last = url.chars().last().unwrap_or(' ');
        let opens = match last {
            ')' => Some('('),
            ']' => Some('['),
            _ => None,
        };
        if let Some(o) = opens
            && url.matches(o).count() >= url.matches(last).count()
        {
            break;
        }
        end -= 1;
    }
    (end > "https://".len()).then_some(end)
}

/// [`link_urls`] for a [`Line`]: every URL in a span that is not already a link is split out
/// into its own span carrying the link, in the span's own style. A span that is a link
/// already is left alone, as letibot leaves a run inside an OSC 8 it did not draw.
pub fn link_urls_in(line: &Line) -> Line {
    let mut out = Line {
        spans: Vec::with_capacity(line.spans.len()),
        style: line.style.clone(),
    };
    for sp in &line.spans {
        if sp.style.link.is_some() || !sp.content.contains("http") {
            out.spans.push(sp.clone());
            continue;
        }
        let s = &sp.content;
        let mut plain_from = 0;
        let mut i = 0;
        while i < s.len() {
            if let Some(end) = url_at(&s[i..]) {
                if plain_from < i {
                    out.spans
                        .push(Span::styled(&s[plain_from..i], sp.style.clone()));
                }
                let url = &s[i..i + end];
                out.spans
                    .push(Span::styled(url, sp.style.clone().link(url)));
                i += end;
                plain_from = i;
                continue;
            }
            i += s[i..].chars().next().map_or(1, char::len_utf8);
        }
        if plain_from < s.len() {
            out.spans
                .push(Span::styled(&s[plain_from..], sp.style.clone()));
        }
    }
    out
}

/// Where the escape sequence at `i` ends: CSI to its final byte, OSC/APC to BEL or ST.
fn escape_end(b: &[u8], i: usize) -> usize {
    let mut j = i + 1;
    match b.get(j) {
        Some(b'[') => {
            j += 1;
            while j < b.len() && !(0x40..=0x7e).contains(&b[j]) {
                j += 1;
            }
            (j + 1).min(b.len())
        }
        Some(b']') | Some(b'_') => {
            while j < b.len() {
                if b[j] == 0x07 {
                    return j + 1;
                }
                if b[j] == 0x1b && b.get(j + 1) == Some(&b'\\') {
                    return j + 2;
                }
                j += 1;
            }
            b.len()
        }
        Some(_) => j + 1,
        None => b.len(),
    }
}

/// A path as a URI path: the bytes that would end or confuse it percent-encoded.
fn uri_path(p: &str) -> String {
    let mut out = String::with_capacity(p.len());
    for &c in p.as_bytes() {
        if c.is_ascii_alphanumeric() || b"/-._~".contains(&c) {
            out.push(c as char);
        } else {
            out.push_str(&format!("%{c:02X}"));
        }
    }
    out
}

/// This machine's name, once.
fn host_name() -> &'static str {
    static NAME: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    NAME.get_or_init(|| {
        let mut buf = [0u8; 256];
        let ok = unsafe { libc::gethostname(buf.as_mut_ptr() as *mut libc::c_char, buf.len()) };
        if ok != 0 {
            return String::new();
        }
        let end = buf.iter().position(|b| *b == 0).unwrap_or(buf.len());
        String::from_utf8_lossy(&buf[..end]).into_owned()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opened(url: &str) -> String {
        format!("\x1b]8;;{url}\x1b\\")
    }

    #[test]
    fn a_url_in_a_row_is_a_link_and_takes_no_columns() {
        let row = "see https://github.com/x/y/pull/192. then (https://a.b/c_(d)) ok";
        let out = link_urls(row);
        assert!(
            out.contains(&opened("https://github.com/x/y/pull/192")),
            "{out:?}"
        );
        assert!(out.contains(&opened("https://a.b/c_(d)")), "{out:?}");
        assert_eq!(
            crate::width::text::width(&out),
            crate::width::text::width(row),
            "a link is zero columns"
        );
        // Inside a coloured run, the colour stays and the URL is linked.
        let coloured = "\x1b[33mhttps://x.org\x1b[0m";
        assert_eq!(
            link_urls(coloured),
            format!(
                "\x1b[33m{}https://x.org\x1b]8;;\x1b\\\x1b[0m",
                opened("https://x.org")
            )
        );
        // A bare scheme is not a link.
        assert_eq!(link_urls("https:// nothing"), "https:// nothing");
        // An existing OSC sequence is copied through, not re-linked.
        let already = format!("{}https://x.org\x1b]8;;\x1b\\", opened("https://x.org"));
        assert_eq!(link_urls(&already), already);
    }

    #[test]
    fn a_file_target_links_to_its_absolute_path_on_this_host() {
        let out = file_link("/w/proj", "src/a b.rs", "a");
        assert_eq!(out, "a", "a target with a space is not one path");
        let out = file_link("/w/proj", "src/main.rs", "…/main.rs");
        assert!(
            out.starts_with("\x1b]8;;file://")
                && out.contains("/w/proj/src/main.rs\x1b\\…/main.rs"),
            "{out:?}"
        );
        let abs = file_link("/w", "/tmp/x.md", "x");
        assert!(abs.contains("/tmp/x.md\x1b\\x"), "{abs:?}");
        assert_eq!(file_link("/w", "\"cargo test\"", "c"), "c");
        assert_eq!(file_link("/w", "(call_0)", "c"), "c");
    }

    #[test]
    fn urls_in_a_line_become_linked_spans_and_keep_their_style() {
        use crate::render::{Buffer, Palette, Rect, Role, Style};
        let line = Line::new(vec![
            Span::role("see https://x.org/a. and ", Role::Faint),
            Span::styled("https://kept", Style::new().link("https://other")),
        ]);
        let out = link_urls_in(&line);
        let texts: Vec<&str> = out.spans.iter().map(|s| s.content.as_str()).collect();
        assert_eq!(
            texts,
            vec!["see ", "https://x.org/a", ". and ", "https://kept"]
        );
        assert_eq!(out.spans[1].style.link.as_deref(), Some("https://x.org/a"));
        assert_eq!(out.spans[1].style.top(), Role::Faint);
        assert_eq!(out.spans[3].style.link.as_deref(), Some("https://other"));
        assert_eq!(out.width(), line.width(), "a link is zero columns");
        // And through the emitter it is the same link the string form draws.
        let mut b = Buffer::empty(Rect::new(0, 0, 40, 1));
        b.set_line(0, 0, &link_urls_in(&Line::raw("go https://x.org now")), 40);
        assert_eq!(
            b.emit(Palette::Colour)[0],
            link_urls("go https://x.org now")
        );
    }

    #[test]
    fn a_file_url_is_the_url_file_link_draws() {
        let url = file_url("/w/proj", "src/main.rs").unwrap();
        assert_eq!(
            file_link("/w/proj", "src/main.rs", "m"),
            format!("\x1b]8;;{url}\x1b\\m\x1b]8;;\x1b\\")
        );
        assert_eq!(file_url("/w", "a b"), None);
    }
}
