//! **Docker's `json-file` log driver**: one JSON record a line, and the container's own text inside
//! it. `rano <id>-json.log` should show what the container said, not the envelope around it.
//!
//! Two facts about the format that are not in its name, both measured on a real file:
//!
//! * **A record is not a line.** The driver splits long output at 16 KiB, so one line of a
//!   container's text arrives as several records and only the last of them ends in `\n`. A reader
//!   that treats a record as a row turns a 40 KB line into three rows of nothing.
//! * **The file is not in time order.** Records are written as the writer emits them, and stderr
//!   can land before stdout with a later `time`. The order is the file's, and that is what a
//!   reader shows.
//!
//! Nothing here decides anything about a *log*: a docker log is a log like any other, so this is
//! the unwrap the loader applies while a record becomes a row.

/// Which stream a record carried.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stream {
    Stdout,
    Stderr,
}

/// One record: the chunk of text it carries, and where it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    pub text: String,
    pub stream: Stream,
}

/// **Whether a file is a docker log**, decided from its first bytes: the driver writes
/// `{"log":"…","stream":"…","time":"…"}` with no space after the colon, and nothing else that
/// rano reads opens with that.
pub fn looks_like(prefix: &[u8]) -> bool {
    let at = prefix
        .iter()
        .position(|b| !b.is_ascii_whitespace())
        .unwrap_or(prefix.len());
    prefix[at..].starts_with(br#"{"log":"#)
}

/// **One record, or `None`** for a line that is not one — a file with a stray line in it is not a
/// reason to throw the file away, so the caller passes those through untouched.
pub fn record(line: &[u8]) -> Option<Record> {
    let value: serde_json::Value = serde_json::from_slice(line).ok()?;
    let text = value.get("log")?.as_str()?.to_string();
    let stream = match value.get("stream").and_then(|s| s.as_str()) {
        Some("stderr") => Stream::Stderr,
        _ => Stream::Stdout,
    };
    Some(Record { text, stream })
}

/// **Records into lines.** Feed it each record's text in file order and it hands back the lines
/// that are complete; a chunk that does not end in `\n` is held until the rest of its line arrives.
#[derive(Debug, Default)]
pub struct Unwrap {
    pending: String,
}

impl Unwrap {
    /// Feed one record's text, collecting every line it completes.
    pub fn feed(&mut self, chunk: &str, out: &mut Vec<String>) {
        for part in chunk.split_inclusive('\n') {
            self.pending.push_str(part);
            if part.ends_with('\n') {
                let line = std::mem::take(&mut self.pending);
                out.push(line.trim_end_matches(['\r', '\n']).to_string());
            }
        }
    }

    /// The last line of a file that does not end in a newline, if there is one.
    pub fn flush(&mut self) -> Option<String> {
        if self.pending.is_empty() {
            None
        } else {
            let line = std::mem::take(&mut self.pending);
            Some(line.trim_end_matches(['\r', '\n']).to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The real first record of a real file: one `echo` to stdout.
    const SAMPLE: &[u8] =
        br#"{"log":"one\n","stream":"stdout","time":"2026-10-30T09:20:27.642864303Z"}"#;

    #[test]
    fn a_docker_log_is_recognised_by_its_first_record() {
        assert!(looks_like(SAMPLE));
        assert!(looks_like(b"\n  {\"log\":\"x\"}"));
        // Not a docker log: an ordinary line, a JSON file of rano's own, and an empty file.
        assert!(!looks_like(b"2026-10-30 INFO started\n"));
        assert!(!looks_like(b"{\"name\": \"rano\"}\n"));
        assert!(!looks_like(b""));
    }

    #[test]
    fn a_record_carries_its_text_and_its_stream() {
        let r = record(SAMPLE).expect("a record");
        assert_eq!(r.text, "one\n");
        assert_eq!(r.stream, Stream::Stdout);
        let e = record(br#"{"log":"three\n","stream":"stderr","time":"t"}"#).expect("a record");
        assert_eq!(e.stream, Stream::Stderr);
        // A JSON object that is not a record, and a line that is not JSON at all.
        assert!(record(br#"{"name":"rano"}"#).is_none());
        assert!(record(b"plain text").is_none());
        // Escapes are the JSON string's, not the file's: a quoted tab arrives as a tab.
        let q = record(br#"{"log":"a\tb","stream":"stdout"}"#).expect("a record");
        assert_eq!(q.text, "a\tb");
    }

    /// **The measured hazard**: a long line is several records, none of them a line until the last.
    #[test]
    fn chunks_join_until_a_newline() {
        let mut u = Unwrap::default();
        let mut out = Vec::new();
        u.feed(&"x".repeat(16384), &mut out);
        u.feed(&"x".repeat(16384), &mut out);
        assert!(out.is_empty(), "no newline yet, so no line");
        u.feed(&"x".repeat(7233), &mut out);
        assert!(
            out.is_empty(),
            "still no newline: 7233 bytes of the same line"
        );
        // The measured file ends the long line with its own record, and `echo done` is the next
        // one: sizes 16384, 16384, 7233, then 5.
        u.feed("\n", &mut out);
        u.feed("done\n", &mut out);
        assert_eq!(out.len(), 2, "the long line, then done");
        assert_eq!(out[0].len(), 16384 * 2 + 7233);
        assert_eq!(out[1], "done");
    }

    #[test]
    fn a_chunk_can_complete_several_lines() {
        let mut u = Unwrap::default();
        let mut out = Vec::new();
        u.feed("a\nb\nc", &mut out);
        assert_eq!(out, vec!["a".to_string(), "b".to_string()]);
        assert_eq!(u.flush().as_deref(), Some("c"));
        assert_eq!(u.flush(), None, "and nothing twice");
    }

    /// **The file's order is the reader's order**: stderr before stdout is what was written, so
    /// it is what is shown. Sorting by `time` would rewrite the file.
    #[test]
    fn the_order_is_the_files_and_not_the_clock() {
        let lines: [&[u8]; 2] = [
            br#"{"log":"three\n","stream":"stderr","time":"2026-10-30T09:20:27.64286801Z"}"#,
            br#"{"log":"one\n","stream":"stdout","time":"2026-10-30T09:20:27.642864303Z"}"#,
        ];
        let mut u = Unwrap::default();
        let mut out = Vec::new();
        for line in lines {
            u.feed(&record(line).expect("a record").text, &mut out);
        }
        assert_eq!(out, vec!["three".to_string(), "one".to_string()]);
    }
}
