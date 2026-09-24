//! Performance measurement, kept in the tree because the numbers matter and
//! the tool to reproduce them should not be rebuilt from scratch each time.
//!
//! Run: `cargo test --release --bin rano bench -- --ignored --nocapture`
//!
//! Three numbers per file, in the order a person notices them:
//!
//! - **open** — read the file and do the first highlight (what you wait for
//!   after `rano big.rs`).
//! - **keystroke** — `handle_key` for one inserted character. This is the one
//!   that matters: it is the latency of typing, and it runs on EVERY key.
//! - **frame** — one `ui::draw` into a headless backend, which is what a
//!   scroll costs per notch.
//!
//! Deliberately does not touch a terminal: `TestBackend` counts the same
//! cells without a pty, so the numbers are the editor's own work.

use crate::buffer::Buffer;
use crate::config;
use crate::editor::Editor;
use crate::ui;
use crate::width;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// One measured file.
struct Sample {
    label: String,
    path: PathBuf,
    bytes: u64,
    lines: usize,
    longest: usize,
}

fn sample(label: &str, path: &str) -> Option<Sample> {
    let p = Path::new(path);
    if !p.exists() {
        return None;
    }
    let bytes = std::fs::metadata(p).ok()?.len();
    let buf = Buffer::from_file(p).ok()?;
    let longest = buf.rows().map(Vec::len).max().unwrap_or(0);
    Some(Sample {
        label: label.to_string(),
        path: p.to_path_buf(),
        bytes,
        lines: buf.row_count(),
        longest,
    })
}

/// `n` iterations of `f`, reporting the median and p95 in microseconds.
fn time<F: FnMut()>(n: usize, mut f: F) -> (f64, f64) {
    // One warm run, untimed: the first call allocates the caches.
    f();
    let mut ns: Vec<u128> = Vec::with_capacity(n);
    for _ in 0..n {
        let t = Instant::now();
        f();
        ns.push(t.elapsed().as_nanos());
    }
    ns.sort_unstable();
    let med = ns[ns.len() / 2] as f64 / 1e3;
    let p95 = ns[(ns.len() * 95 / 100).min(ns.len() - 1)] as f64 / 1e3;
    (med, p95)
}

fn ms(us: f64) -> String {
    if us >= 10_000.0 {
        format!("{:>8.2} ms", us / 1000.0)
    } else if us >= 1000.0 {
        format!("{:>8.1} ms", us / 1000.0)
    } else {
        format!("{:>8.1} µs", us)
    }
}

fn human(n: u64) -> String {
    if n >= 1 << 30 {
        format!("{:.1} GiB", n as f64 / (1u64 << 30) as f64)
    } else if n >= 1 << 20 {
        format!("{:.1} MiB", n as f64 / (1u64 << 20) as f64)
    } else if n >= 1 << 10 {
        format!("{:.0} KiB", n as f64 / 1024.0)
    } else {
        format!("{n} B")
    }
}

/// The measured files: big in different ways. Synthetic ones live in
/// `/tmp/ranoperf` (`--bench` is where they come from); the real ones are the
/// largest text files on this machine at the time of writing.
fn samples() -> Vec<Sample> {
    let mut v = Vec::new();
    // One enormous line: the minified-JS shape, and the wrap/cursor stress.
    for (label, path) in [
        ("minified20.js (20MB 1 line)", "/tmp/ranoperf/minified20.js"),
        ("minified.js (1 line)", "/tmp/ranoperf/minified.js"),
        ("oneline.txt (1 line)", "/tmp/ranoperf/oneline.txt"),
        ("big.log (30 MB)", "/tmp/ranoperf/big.log"),
        ("huge200.log (193 MB)", "/tmp/ranoperf/huge200.log"),
        ("big.rs (7 MB)", "/tmp/ranoperf/big.rs"),
        ("big.md (3 MB)", "/tmp/ranoperf/big.md"),
        // Real files, if this machine still has them.
        (
            "leticl app.rs",
            "/home/dead/Projects/letibot/letibot/crates/tui/src/app.rs",
        ),
        (
            "ci README.md",
            "/home/dead/Projects/ci-tmp/results-rocm/README.md",
        ),
        (
            "suite-base.log",
            "/home/dead/Projects/llama.cpp-t23/suite-base.log",
        ),
    ] {
        if let Some(s) = sample(label, path) {
            v.push(s);
        }
    }
    v
}

#[test]
#[ignore = "performance measurement; run explicitly with --ignored --nocapture"]
fn bench() {
    let width = 120u16;
    let height = 40u16;
    let text_h = (height as usize).saturating_sub(4);
    println!(
        "\nrano perf — {}x{} viewport, {} text rows\n",
        width, height, text_h
    );
    println!(
        "{:<22} {:>10} {:>9} {:>9} | {:>13} {:>13} | {:>13}",
        "file", "size", "lines", "longest", "open+first hl", "keystroke", "frame (draw)"
    );
    println!("{}", "-".repeat(110));

    for s in samples() {
        let t = Instant::now();
        let buf = Buffer::from_file(&s.path).expect("read");
        let mut e = Editor::new(buf, config::Config::default());
        let open = t.elapsed();

        e.text_w = width as usize;
        e.text_h = text_h;
        e.show_line_numbers = true;
        e.ensure_wrap_prefix();
        e.adjust_scroll(text_h);
        // Put the cursor somewhere real, mid-document.
        let mid = e.bs().buf.row_count() / 2;
        e.bs_mut().cursor = crate::buffer::Pos { row: mid, col: 0 };
        e.adjust_scroll(text_h);

        // A keystroke: insert one character. Times the full path, which is
        // the re-highlight plus the syntax-error walk plus the LSP dirtying.
        // ONE insertion per iteration — a key+backspace pair measured two
        // edits and doubled every number here before 2026-09-22. The document
        // grows by one character per iteration, which is what typing does.
        let keys = 3;
        let (kmed, kp95) = time(keys, || {
            e.handle_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
        });

        let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("backend");
        let frames = 10;
        let (fmed, _) = time(frames, || {
            let _ = terminal.draw(|f| ui::draw(f, &e));
        });

        println!(
            "{:<22} {:>10} {:>9} {:>9} | {:>13} {:>13} | {:>13}",
            s.label,
            human(s.bytes),
            s.lines,
            s.longest,
            format!("{}  ({})", ms(open.as_micros() as f64), "once"),
            format!("{} / p95 {}", ms(kmed), ms(kp95)),
            ms(fmed),
        );
    }
    println!();
    let _ = width::is_simple(&[]);
}

/// Attribute one keystroke's cost to the steps that make it up.
///
/// The point is to know WHICH step is O(document) before touching anything:
/// a keystroke on a 30 MB file should not cost 30 MB of work, and if it does,
/// saying so precisely is what makes the fix obvious.
#[test]
#[ignore = "performance measurement; run explicitly with --ignored --nocapture"]
fn bench_breakdown() {
    let paths: Vec<(&str, &str)> = vec![
        ("minified20 (20MB 1 line)", "/tmp/ranoperf/minified20.js"),
        ("minified.js (1 line)", "/tmp/ranoperf/minified.js"),
        ("oneline.txt (1 line)", "/tmp/ranoperf/oneline.txt"),
        ("big.log (30 MB)", "/tmp/ranoperf/big.log"),
        ("huge200.log (193 MB)", "/tmp/ranoperf/huge200.log"),
        ("big.rs (7 MB)", "/tmp/ranoperf/big.rs"),
    ];
    // One labelled line per metric rather than a table: the columns here have
    // been renamed often enough that a mislabelled number is a real risk, and
    // a measurement nobody can read is worse than no measurement.
    println!();
    for (label, path) in paths {
        let p = Path::new(path);
        if !p.exists() {
            continue;
        }
        let size = std::fs::metadata(p).map(|m| m.len()).unwrap_or(0);
        let buf = Buffer::from_file(p).expect("read");
        let mut e = Editor::new(buf.clone(), config::Config::default());
        e.text_w = 120;
        e.text_h = 36;
        // Put the edit point somewhere real: mid-document, so the undo
        // snapshot and the wrap re-sum have rows below them as they would when
        // someone is actually typing.
        let mid = e.bs().buf.row_count() / 2;
        e.bs_mut().cursor = crate::buffer::Pos { row: mid, col: 0 };

        println!(
            "{} — {} bytes, {} rows, longest row {}",
            label,
            size,
            e.bs().buf.row_count(),
            e.bs().buf.rows().map(Vec::len).max().unwrap_or(0)
        );
        // The whole document, for scale: what a keystroke used to pay.
        let (t, _) = time(3, || {
            e.bs_mut().hl.refresh(&buf);
        });
        println!("    {:>22}  {}", "hl.refresh (whole doc)", ms(t));
        // The edit alone: the buffer mutation and the undo snapshot.
        let (t, _) = time(3, || {
            e.handle_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
        });
        println!("    {:>22}  {}", "edit only", ms(t));
        // The frame's highlight, which is where the remaining cost lives.
        let (t, _) = time(3, || {
            e.handle_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
            e.ensure_highlight();
        });
        println!("    {:>22}  {}", "edit + highlight", ms(t));
        // The wrap table's incremental update: one row re-measured, then the
        // prefix re-summed from that row to the end. Arithmetic, but over every
        // remaining row — O(rows).
        let (t, _) = time(3, || {
            let bs = e.bs_mut();
            bs.edit_gen = bs.edit_gen.wrapping_add(1);
            bs.wrap_dirty_row = Some(bs.cursor.row);
            e.ensure_wrap_prefix();
        });
        println!("    {:>22}  {}", "wrap table (re-sum)", ms(t));
        // The undo snapshot: `begin_action` copies the affected rows, so a
        // one-character insertion on a single enormous row copies the row.
        let (t, _) = time(3, || {
            let bs = e.bs_mut();
            let r = bs.cursor.row;
            std::hint::black_box(bs.buf.lines_slice()[r..r + 1].to_vec());
        });
        println!("    {:>22}  {}", "undo row clone", ms(t));
        // The buffer's own insert: a Vec<char> splice, O(row).
        let (t, _) = time(3, || {
            let bs = e.bs_mut();
            let (r, c) = (bs.cursor.row, bs.cursor.col);
            bs.buf.insert_char(r, c, 'z');
            bs.buf.backspace(r, c + 1);
        });
        println!("    {:>22}  {}", "buffer splice", ms(t));
        println!();
    }
}

/// What a person waits for after `rano FILE`: read the file, then the first
/// highlight. Both are in here because both are before the first frame.
#[test]
#[ignore = "performance measurement; run explicitly with --ignored --nocapture"]
fn bench_open() {
    let paths: Vec<(&str, &str)> = vec![
        ("big.rs (7 MB)", "/tmp/ranoperf/big.rs"),
        ("big.md (3 MB)", "/tmp/ranoperf/big.md"),
        ("big.log (30 MB)", "/tmp/ranoperf/big.log"),
        ("huge200.log (193 MB)", "/tmp/ranoperf/huge200.log"),
    ];
    println!("\nopen — read the file, then the first highlight\n");
    println!(
        "{:<28} {:>10} {:>14} {:>14} {:>14}",
        "file", "size", "read", "first hl", "total"
    );
    println!("{}", "-".repeat(86));
    for (label, path) in paths {
        let p = Path::new(path);
        if !p.exists() {
            continue;
        }
        let size = std::fs::metadata(p).map(|m| m.len()).unwrap_or(0);
        let t = Instant::now();
        let buf = Buffer::from_file(p).expect("read");
        let read = t.elapsed();
        let mut e = Editor::new(buf, config::Config::default());
        e.text_w = 120;
        e.text_h = 36;
        let t = Instant::now();
        e.ensure_wrap_prefix();
        e.ensure_highlight();
        let first = t.elapsed();
        println!(
            "{:<28} {:>10} {:>14} {:>14} {:>14}",
            label,
            human(size),
            ms(read.as_micros() as f64),
            ms(first.as_micros() as f64),
            ms((read + first).as_micros() as f64),
        );
    }
    println!();
}

/// How the read path behaves: time AND memory.
///
/// The read is the one part of the open cost that was never opened up — every
/// other measurement in this file is about highlighting. It matters because
/// `Buffer`'s text model is `Vec<Vec<char>>`, and a `char` is four bytes.
#[test]
#[ignore = "performance measurement; run explicitly with --ignored --nocapture"]
fn bench_read() {
    /// Peak resident set size in KiB, from the kernel. Monotonic: it is the
    /// high-water mark, so it must be read once at the end of the process's
    /// heaviest point, not per file.
    fn peak_rss_kib() -> u64 {
        let s = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
        for line in s.lines() {
            if let Some(rest) = line.strip_prefix("VmHWM:") {
                return rest
                    .trim()
                    .trim_end_matches(" kB")
                    .trim()
                    .parse()
                    .unwrap_or(0);
            }
        }
        0
    }
    fn rss_kib() -> u64 {
        let s = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
        for line in s.lines() {
            if let Some(rest) = line.strip_prefix("VmRSS:") {
                return rest
                    .trim()
                    .trim_end_matches(" kB")
                    .trim()
                    .parse()
                    .unwrap_or(0);
            }
        }
        0
    }

    let paths: Vec<(&str, &str)> = vec![
        ("big.rs (7 MB)", "/tmp/ranoperf/big.rs"),
        ("big.log (30 MB)", "/tmp/ranoperf/big.log"),
        ("huge200.log (193 MB)", "/tmp/ranoperf/huge200.log"),
    ];
    println!("\nread — time and memory, one file per process\n");
    println!(
        "{:<26} {:>10} {:>10} {:>12} {:>14} {:>10}",
        "file", "size", "chars", "read", "RSS delta", "bytes/char"
    );
    println!("{}", "-".repeat(90));
    for (label, path) in paths {
        let p = Path::new(path);
        if !p.exists() {
            continue;
        }
        let size = std::fs::metadata(p).map(|m| m.len()).unwrap_or(0);
        let before = rss_kib();
        let t = Instant::now();
        let buf = Buffer::from_file(p).expect("read");
        let read = t.elapsed();
        let after = rss_kib();
        let chars: usize = buf.rows().map(Vec::len).sum();
        let delta = after.saturating_sub(before);
        println!(
            "{:<26} {:>10} {:>10} {:>12} {:>14} {:>10.2}",
            label,
            human(size),
            chars,
            ms(read.as_micros() as f64),
            human(delta * 1024),
            if chars > 0 {
                delta as f64 * 1024.0 / chars as f64
            } else {
                0.0
            }
        );
        std::hint::black_box(&buf);
        drop(buf);
    }
    // The arithmetic behind the RSS column, stated so it needs no trust.
    let c = std::mem::size_of::<char>();
    let v = std::mem::size_of::<Vec<char>>();
    println!("\nsize_of::<char>() = {c} bytes; size_of::<Vec<char>>() = {v} bytes (per LINE)");
    println!(
        "so a row of n characters costs 4n + {v} + allocator overhead, and a 1-byte-per-character"
    );
    println!("file becomes at least 4 bytes per character before it is editable.");
    println!("\npeak RSS for the process: {} KiB", peak_rss_kib());
    println!();
}

/// The read path, phase by phase: what `Buffer::from_file` actually spends.
///
/// `from_file` is four steps — read the bytes, validate them as UTF-8, look
/// for CRLF, convert every line to `Vec<char>` — and the conversion is the one
/// that costs both time and memory, because a `char` is four bytes.
#[test]
#[ignore = "performance measurement; run explicitly with --ignored --nocapture"]
fn bench_read_phases() {
    let paths: Vec<(&str, &str)> = vec![
        ("big.rs (7 MB)", "/tmp/ranoperf/big.rs"),
        ("big.log (30 MB)", "/tmp/ranoperf/big.log"),
        ("huge200.log (193 MB)", "/tmp/ranoperf/huge200.log"),
    ];
    println!("\nread phases — median of 3, ms\n");
    println!(
        "{:<26} {:>10} {:>12} {:>12} {:>14} {:>14}",
        "file", "size", "read+utf8", "crlf scan", "chars convert", "total"
    );
    println!("{}", "-".repeat(94));
    for (label, path) in paths {
        let p = Path::new(path);
        if !p.exists() {
            continue;
        }
        let size = std::fs::metadata(p).map(|m| m.len()).unwrap_or(0);
        let (t_read, _) = time(3, || {
            let text = fs::read_to_string(p).expect("read");
            std::hint::black_box(text.len());
        });
        let text = fs::read_to_string(p).expect("read");
        let (t_crlf, _) = time(3, || {
            std::hint::black_box(text.contains("\r\n"));
        });
        let (t_chars, _) = time(3, || {
            let lines: Vec<Vec<char>> = text.lines().map(|l| l.chars().collect()).collect();
            std::hint::black_box(lines.len());
        });
        let (t_all, _) = time(3, || {
            let b = Buffer::from_file(p).expect("read");
            std::hint::black_box(b.row_count());
        });
        println!(
            "{:<26} {:>10} {:>12} {:>12} {:>14} {:>14}",
            label,
            human(size),
            ms(t_read),
            ms(t_crlf),
            ms(t_chars),
            ms(t_all),
        );
    }
    println!();
}

/// What a line index costs when it is built from BYTES instead of characters.
///
/// In UTF-8 a 0x0A byte is always a newline — multi-byte sequences use only
/// lead bytes C2-F4 and continuation bytes 80-BF, so no 0x0A can appear inside
/// one (verified over every codepoint in the 0.1-0.2 s this test's sibling
/// takes). That means the row index can be built by scanning the file's bytes,
/// with no decoding at all, which is what makes "load only the viewport"
/// possible: you need to know where row N starts before you can decode it, and
/// this answers that without touching a `char`.
#[test]
#[ignore = "performance measurement; run explicitly with --ignored --nocapture"]
fn bench_line_index() {
    let paths: Vec<(&str, &str)> = vec![
        ("big.rs (7 MB)", "/tmp/ranoperf/big.rs"),
        ("big.log (30 MB)", "/tmp/ranoperf/big.log"),
        ("huge200.log (193 MB)", "/tmp/ranoperf/huge200.log"),
    ];
    println!("\nline index from bytes — median of 3\n");
    println!(
        "{:<26} {:>10} {:>10} {:>14} {:>16} {:>14}",
        "file", "size", "lines", "byte scan", "index memory", "vs decode"
    );
    println!("{}", "-".repeat(96));
    for (label, path) in paths {
        let p = Path::new(path);
        if !p.exists() {
            continue;
        }
        let bytes = fs::read(p).expect("read");
        let size = bytes.len();
        let (t_scan, _) = time(3, || {
            let mut starts: Vec<u64> = Vec::new();
            for (i, b) in bytes.iter().enumerate() {
                if *b == b'\n' {
                    starts.push(i as u64);
                }
            }
            std::hint::black_box(starts.len());
        });
        // What that index costs to keep, against decoding the whole file.
        let mut starts: Vec<u64> = Vec::new();
        for (i, b) in bytes.iter().enumerate() {
            if *b == b'\n' {
                starts.push(i as u64);
            }
        }
        let (t_decode, _) = time(3, || {
            let text = String::from_utf8(bytes.clone()).expect("utf8");
            let lines: Vec<Vec<char>> = text.lines().map(|l| l.chars().collect()).collect();
            std::hint::black_box(lines.len());
        });
        println!(
            "{:<26} {:>10} {:>10} {:>14} {:>16} {:>14}",
            label,
            human(size as u64),
            starts.len() + 1,
            ms(t_scan),
            human(starts.len() as u64 * 8),
            ms(t_decode),
        );
    }
    println!();
}

/// Time to the first frame — the freeze a person actually experiences.
///
/// Every other measurement here is in-process. This one is the wall clock from
/// "the process started" to "something is on screen", which is what `rano
/// huge.log` makes you wait through: the read, the first highlight, and the
/// first draw, all before a single row is painted.
///
/// The second column is the same work with the READ hoisted onto a worker
/// thread — hand-rolled, `std::thread` + a channel, the pattern `exec.rs` and
/// `lsp.rs` already use. It is the bound a loading change can reach: the frame
/// is drawn immediately from an empty buffer, so the wait is one frame instead
/// of the whole file.
#[test]
#[ignore = "performance measurement; run explicitly with --ignored --nocapture"]
fn bench_cold_open() {
    let paths: Vec<(&str, &str)> = vec![
        ("big.rs (7 MB)", "/tmp/ranoperf/big.rs"),
        ("big.log (30 MB)", "/tmp/ranoperf/big.log"),
        ("huge200.log (193 MB)", "/tmp/ranoperf/huge200.log"),
    ];
    println!("\ntime to first frame — the freeze, and the floor under it\n");
    println!(
        "{:<26} {:>10} {:>16} {:>16} {:>14}",
        "file", "size", "today (blocking)", "read off-thread", "first frame"
    );
    println!("{}", "-".repeat(88));
    for (label, path) in paths {
        let p = Path::new(path);
        if !p.exists() {
            continue;
        }
        let size = std::fs::metadata(p).map(|m| m.len()).unwrap_or(0);

        // Today: nothing is on screen until the whole file has been read,
        // decoded, highlighted and drawn.
        let t = Instant::now();
        let buf = Buffer::from_file(p).expect("read");
        let mut e = Editor::new(buf, config::Config::default());
        e.text_w = 120;
        e.text_h = 36;
        e.ensure_wrap_prefix();
        e.ensure_highlight();
        let mut terminal = Terminal::new(TestBackend::new(120, 40)).expect("backend");
        terminal.draw(|f| ui::draw(f, &e)).expect("draw");
        let today = t.elapsed();

        // Hand-rolled: the read happens on a worker; the main thread draws a
        // frame it can draw now. Measured as "start the worker, then do the
        // frame the loader owes the user" — the wait is the frame, not the
        // read, because the read is no longer on this thread.
        let t = Instant::now();
        let (tx, rx) = std::sync::mpsc::channel::<Buffer>();
        let path_buf = p.to_path_buf();
        std::thread::spawn(move || {
            let _ = tx.send(Buffer::from_file(&path_buf).expect("worker read"));
        });
        // The frame the user sees first: an empty buffer, immediately.
        let mut e = Editor::new(Buffer::new(), config::Config::default());
        e.text_w = 120;
        e.text_h = 36;
        let mut terminal = Terminal::new(TestBackend::new(120, 40)).expect("backend");
        terminal.draw(|f| ui::draw(f, &e)).expect("draw");
        let off_thread = t.elapsed();
        let read = rx.recv().expect("worker sent the buffer");
        std::hint::black_box(read.row_count());

        println!(
            "{:<26} {:>10} {:>16} {:>16} {:>14}",
            label,
            human(size),
            ms(today.as_micros() as f64),
            ms(off_thread.as_micros() as f64),
            ms(off_thread.as_micros() as f64),
        );
    }
    println!("\n(the read still has to finish before the text appears; the point is that the");
    println!("window and its chrome are up, and the event loop is running, while it does)");
    println!();
}

/// Scheduling, measured: time to the first SCREENFUL of text rather than the
/// first frame.
///
/// A frame with nothing in it is not the goal — the goal is the top of the
/// file, and that does not need the whole file. This reads in chunks and stops
/// as soon as it has enough rows to fill a viewport, which is the "minimum
/// that makes the user happy" from §13.6 stated as a number.
///
/// It also measures cancellation latency: a chunked reader that checks a flag
/// between chunks can abandon a 184 MB read promptly, which a single
/// `read_to_string` cannot.
#[test]
#[ignore = "performance measurement; run explicitly with --ignored --nocapture"]
fn bench_first_screen() {
    /// A viewport's worth of rows.
    const ROWS: usize = 40;
    /// Read granularity. 64 KiB is one page-cache-friendly read and ~800 rows
    /// of a log line, so the first chunk is usually enough on its own.
    const CHUNK: usize = 64 * 1024;

    let paths: Vec<(&str, &str)> = vec![
        ("big.rs (7 MB)", "/tmp/ranoperf/big.rs"),
        ("big.log (30 MB)", "/tmp/ranoperf/big.log"),
        ("huge200.log (193 MB)", "/tmp/ranoperf/huge200.log"),
    ];
    println!("\ntime to the first screenful of text — {ROWS} rows, {CHUNK}-byte chunks\n");
    println!(
        "{:<26} {:>10} {:>14} {:>12} {:>16} {:>16}",
        "file", "size", "first screen", "bytes read", "whole file", "cancel seen"
    );
    println!("{}", "-".repeat(100));
    for (label, path) in paths {
        let p = Path::new(path);
        if !p.exists() {
            continue;
        }
        let size = std::fs::metadata(p).map(|m| m.len()).unwrap_or(0);

        // Time to the first screen: read chunk by chunk, decode, stop at ROWS.
        let (t_screen, bytes_read) = {
            use std::io::Read as _;
            let mut f = fs::File::open(p).expect("open");
            let t = Instant::now();
            let mut buf = vec![0u8; CHUNK];
            let mut text = String::new();
            let mut total = 0usize;
            loop {
                let n = f.read(&mut buf).expect("read");
                if n == 0 {
                    break;
                }
                total += n;
                text.push_str(&String::from_utf8_lossy(&buf[..n]));
                if text.matches('\n').count() >= ROWS {
                    break;
                }
            }
            let lines: Vec<Vec<char>> = text
                .lines()
                .take(ROWS)
                .map(|l| l.chars().collect())
                .collect();
            std::hint::black_box(lines.len());
            (t.elapsed(), total)
        };

        // The whole file, for contrast (the number in §14.1).
        let t = Instant::now();
        let whole = Buffer::from_file(p).expect("read");
        let whole_t = t.elapsed();
        std::hint::black_box(whole.row_count());

        // Cancellation: how soon does a chunked read notice the flag? The flag
        // is checked per chunk, so the bound is one chunk, not one file.
        let flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let f2 = flag.clone();
        let t = Instant::now();
        let handle = std::thread::spawn(move || {
            use std::io::Read as _;
            let mut f = fs::File::open(p).expect("open");
            let mut buf = vec![0u8; CHUNK];
            let mut n_reads = 0usize;
            loop {
                if f2.load(std::sync::atomic::Ordering::Relaxed) {
                    break;
                }
                match f.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => n_reads += 1,
                }
            }
            n_reads
        });
        // Let it get going, then cancel.
        std::thread::sleep(Duration::from_millis(5));
        flag.store(true, std::sync::atomic::Ordering::Relaxed);
        let cancel_latency = t.elapsed();
        let _ = handle.join();

        println!(
            "{:<26} {:>10} {:>14} {:>12} {:>16} {:>16}",
            label,
            human(size),
            ms(t_screen.as_micros() as f64),
            human(bytes_read as u64),
            ms(whole_t.as_micros() as f64),
            ms(cancel_latency.as_micros() as f64),
        );
    }
    println!(
        "\n(first screen = read until {ROWS} newlines; cancel seen = ~one chunk after the flag)"
    );
    println!();
}

/// A prototype of lazy loading: the file stays on disk, an index is built from
/// its BYTES, and rows are decoded only when something asks for them.
///
/// This exists to answer the questions a design would otherwise assume:
/// what does the index cost, what does a window cost on demand, does the
/// cheap path agree with the eager one, and what does it all weigh.
mod lazy {
    use std::fs::File;
    use std::io::{self, Read};
    use std::os::unix::fs::FileExt;

    /// One bit per row. NOT "is the row pure ASCII" — see `narrow_here`: the
    /// property that matters is "every character is exactly one column and
    /// there is no tab", which Latin-1, Greek, Cyrillic and most of the world's
    /// text satisfy while being non-ASCII. The first version of this prototype
    /// used `is_ascii` and was wrong: it excluded text that wraps exactly like
    /// ASCII does.
    #[derive(Default)]
    pub struct Bits(Vec<u64>);

    impl Bits {
        fn push(&mut self, r: usize, v: bool) {
            if r / 64 == self.0.len() {
                self.0.push(0);
            }
            if v {
                self.0[r / 64] |= 1 << (r % 64);
            }
        }
        pub fn get(&self, r: usize) -> bool {
            self.0.get(r / 64).is_some_and(|w| w & (1 << (r % 64)) != 0)
        }
        pub fn bytes(&self) -> usize {
            self.0.len() * 8
        }
    }

    /// Does the UTF-8 sequence starting at `b[i]` occupy exactly one column,
    /// and is it not a tab? Answered from bytes; the 3- and 4-byte cases need
    /// the codepoint, which is arithmetic on the sequence — no `char`, no
    /// allocation, and nothing at all for the 99% of bytes that are ASCII.
    ///
    /// Returns `(width_is_one, sequence_len)`.
    fn narrow_here(b: &[u8], i: usize) -> (bool, usize) {
        let x = b[i];
        match x {
            0x09 => (false, 1),       // tab: expands, so not one column
            0x00..=0x7f => (true, 1), // ASCII
            0xc2..=0xdf => (true, 2), // U+0080..U+07FF: Latin-1, Greek,
            //                                Cyrillic, Hebrew, Arabic — all narrow
            0xe0..=0xef => {
                // U+0800..U+FFFF: CJK and kana are two columns; Indic, Thai and
                // the rest are one. The codepoint decides.
                if i + 2 >= b.len() {
                    return (false, 1);
                }
                let cp = ((x as u32 & 0x0f) << 12)
                    | ((b[i + 1] as u32 & 0x3f) << 6)
                    | (b[i + 2] as u32 & 0x3f);
                (!crate::width::is_wide_cp(cp), 3)
            }
            0xf0..=0xf4 => {
                if i + 3 >= b.len() {
                    return (false, 1);
                }
                let cp = ((x as u32 & 0x07) << 18)
                    | ((b[i + 1] as u32 & 0x3f) << 12)
                    | ((b[i + 2] as u32 & 0x3f) << 6)
                    | (b[i + 3] as u32 & 0x3f);
                (!crate::width::is_wide_cp(cp), 4)
            }
            // A continuation byte where a lead was expected, a surrogate, or an
            // overlong: not text we can reason about. Treat as wide so the row
            // takes the decoding path.
            _ => (false, 1),
        }
    }

    /// The file, indexed but not decoded.
    pub struct Lazy {
        file: File,
        /// Byte offset of each row's first byte; `starts.len()` is the row
        /// count. The end of a row is the next start minus one (the newline),
        /// except for the last row, whose end is `last_end`.
        starts: Vec<u64>,
        /// End of the final row's CONTENT — which is not the file size when
        /// the file ends in a newline. Getting this wrong was the one bug the
        /// prototype had: it produced a phantom empty final row on every file.
        last_end: u64,
        ascii: Bits,
        /// One bit per row: every BYTE is ASCII, so `char_count == byte_len`.
        pure_ascii: Bits,
        /// Char counts for rows that are not pure ASCII, `(row, chars)`,
        /// ascending. EMPTY for an all-ASCII file — which is every source file
        /// and almost every log, and the reason the index stays 19.8 MB rather
        /// than doubling: counting chars costs nothing for a row whose count
        /// equals its byte length.
        non_ascii_chars: Vec<(u32, u32)>,
        size: u64,
    }

    impl Lazy {
        /// Stream the file once, recording where each row starts and whether it
        /// is ASCII. No decoding, no `char` is constructed.
        pub fn open(path: &std::path::Path) -> io::Result<Self> {
            let mut file = File::open(path)?;
            let size = file.metadata()?.len();
            let mut starts = vec![0u64];
            let mut ascii = Bits::default();
            let mut pure_ascii = Bits::default();
            let mut buf = vec![0u8; 1 << 16];
            let mut row_narrow = true;
            let mut row_ascii = true;
            let mut row_chars = 0usize;
            let mut non_ascii_chars: Vec<(u32, u32)> = Vec::new();
            let mut offset = 0u64;
            loop {
                let n = file.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                let chunk = &buf[..n];
                // Whole-chunk fast path: if the chunk is ASCII there are no
                // multi-byte sequences to reason about, so the scan is a
                // newline loop plus one tab test. This is the path every source
                // file and log takes, and it is what keeps the index near the
                // cost of the bare newline scan.
                if chunk.is_ascii() {
                    // No char counting here: a pure-ASCII row's char count IS
                    // its byte count, so there is nothing to store and nothing
                    // to count. This is why the index stays near the cost of
                    // the bare newline scan.
                    if chunk.contains(&b'\t') {
                        row_narrow = false;
                    }
                    for (i, b) in chunk.iter().enumerate() {
                        if *b == b'\n' {
                            let r = starts.len() - 1;
                            ascii.push(r, row_narrow);
                            pure_ascii.push(r, true);
                            starts.push(offset + i as u64 + 1);
                            row_narrow = true;
                        }
                    }
                    offset += n as u64;
                    continue;
                }
                // Mixed or non-ASCII: per-character, deciding per sequence.
                let mut i = 0usize;
                while i < chunk.len() {
                    let x = chunk[i];
                    if x == b'\n' {
                        let r = starts.len() - 1;
                        ascii.push(r, row_narrow);
                        pure_ascii.push(r, row_ascii);
                        if !row_ascii {
                            non_ascii_chars.push((r as u32, row_chars as u32));
                        }
                        starts.push(offset + i as u64 + 1);
                        row_narrow = true;
                        row_ascii = true;
                        row_chars = 0;
                        i += 1;
                        continue;
                    }
                    if x < 0x80 {
                        if x == 0x09 {
                            row_narrow = false;
                        }
                        row_chars += 1;
                        i += 1;
                        continue;
                    }
                    row_ascii = false;
                    let (narrow, len) = narrow_here(chunk, i);
                    if !narrow {
                        row_narrow = false;
                    }
                    row_chars += 1;
                    i += len;
                }
                offset += n as u64;
            }
            // `starts` now holds one entry per row plus a phantom entry for the
            // row a trailing newline would begin. Resolve which case this is,
            // and make sure every real row has its ASCII flag — the final row
            // is the one the scan could not push, because no newline ended it.
            let last_end;
            if size == 0 {
                last_end = 0;
            } else if starts.last().copied() == Some(size) {
                starts.pop(); // the file ended in a newline: no phantom row
                last_end = size - 1;
            } else {
                last_end = size;
                let r = starts.len() - 1;
                ascii.push(r, row_narrow);
                pure_ascii.push(r, row_ascii);
                if !row_ascii {
                    non_ascii_chars.push((r as u32, row_chars as u32));
                }
            }
            if starts.is_empty() {
                starts.push(0);
                ascii.push(0, true); // an empty file is one empty ASCII row
                pure_ascii.push(0, true);
            }
            Ok(Self {
                file,
                starts,
                last_end,
                ascii,
                pure_ascii,
                non_ascii_chars,
                size,
            })
        }

        pub fn rows(&self) -> usize {
            self.starts.len()
        }

        /// Byte range of row `r`'s CONTENT, the newline excluded.
        pub fn byte_range(&self, r: usize) -> (u64, u64) {
            let start = self.starts[r];
            let end = match self.starts.get(r + 1) {
                Some(next) => next.saturating_sub(1), // one back over the '\n'
                None => self.last_end,
            };
            (start, end.max(start).min(self.size))
        }

        /// Byte length of row `r` — known without decoding anything.
        pub fn byte_len(&self, r: usize) -> u64 {
            let (a, b) = self.byte_range(r);
            b - a
        }

        /// Is row `r` pure ASCII? Then `byte_len == char_count`, and its wrap
        /// segments are `ceil(byte_len / view_w)` — exactly, with no decode.
        pub fn is_narrow(&self, r: usize) -> bool {
            self.ascii.get(r)
        }

        /// Characters in row `r`. Free when the row is pure ASCII (its char
        /// count IS its byte count); otherwise one binary search into a list
        /// that is empty for an ASCII file.
        pub fn char_count(&self, r: usize) -> Option<u64> {
            if self.pure_ascii.get(r) {
                return Some(self.byte_len(r));
            }
            self.non_ascii_chars
                .binary_search_by_key(&(r as u32), |(row, _)| *row)
                .ok()
                .map(|i| self.non_ascii_chars[i].1 as u64)
        }

        /// Decode rows `[first, last]`. The only place bytes become chars, and
        /// the only place that touches the disk after `open`.
        pub fn decode(&self, first: usize, last: usize) -> io::Result<Vec<Vec<char>>> {
            let last = last.min(self.rows().saturating_sub(1));
            let lo = self.starts[first];
            // Up to the END OF CONTENT of the last row, so the text has one
            // newline between rows and none after the last — `split('\n')`
            // then yields exactly the rows asked for, with no trailing "".
            let hi = self.byte_range(last).1;
            let mut buf = vec![0u8; (hi - lo) as usize];
            self.file.read_exact_at(&mut buf, lo)?;
            let text = String::from_utf8(buf)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
            Ok(text
                .split('\n')
                .map(|l| l.strip_suffix('\r').unwrap_or(l).chars().collect())
                .collect())
        }

        pub fn index_bytes(&self) -> usize {
            self.starts.capacity() * 8
                + self.non_ascii_chars.capacity() * 8
                + self.ascii.bytes()
                + self.pure_ascii.bytes()
        }
    }
}

/// Does the lazy path agree with the eager one, on every row, and what does it
/// cost? The first question is the one that decides whether the design is
/// sound; the second whether it is worth having.
#[test]
#[ignore = "performance measurement; run explicitly with --ignored --nocapture"]
fn bench_lazy_is_correct_and_cheap() {
    let paths: Vec<(&str, &str)> = vec![
        ("big.rs (7 MB)", "/tmp/ranoperf/big.rs"),
        ("big.log (30 MB)", "/tmp/ranoperf/big.log"),
        ("huge200.log (193 MB)", "/tmp/ranoperf/huge200.log"),
        // Multibyte, so the byte scan must not mistake a continuation byte for
        // a newline and the ASCII flags must be honest about it.
        ("cjk-check.md", "/tmp/cjk-check.md"),
    ];
    println!("\nlazy loading — index cost, on-demand windows, and agreement with eager decode\n");
    println!(
        "{:<24} {:>10} {:>8} {:>12} {:>12} {:>14} {:>10}",
        "file", "size", "rows", "index", "index mem", "first window", "all match"
    );
    println!("{}", "-".repeat(96));
    for (label, path) in paths {
        let p = Path::new(path);
        if !p.exists() {
            continue;
        }
        let size = std::fs::metadata(p).map(|m| m.len()).unwrap_or(0);

        // What the index costs to build, and to keep.
        let rss_before = std::fs::read_to_string("/proc/self/status")
            .unwrap_or_default()
            .lines()
            .find_map(|l| l.strip_prefix("VmRSS:"))
            .and_then(|v| v.trim().trim_end_matches(" kB").trim().parse::<u64>().ok())
            .unwrap_or(0);
        let t = Instant::now();
        let lazy = lazy::Lazy::open(p).expect("index");
        let index_t = t.elapsed();
        let rss_after = std::fs::read_to_string("/proc/self/status")
            .unwrap_or_default()
            .lines()
            .find_map(|l| l.strip_prefix("VmRSS:"))
            .and_then(|v| v.trim().trim_end_matches(" kB").trim().parse::<u64>().ok())
            .unwrap_or(0);

        // A window on demand: the first screen, and one deep in the file.
        let t = Instant::now();
        let first = lazy.decode(0, 39).expect("decode first window");
        let first_t = t.elapsed();
        let deep_row = lazy.rows() / 2;
        let t = Instant::now();
        let deep = lazy
            .decode(deep_row, deep_row + 39)
            .expect("decode deep window");
        let deep_t = t.elapsed();

        // THE correctness question: does every row decoded lazily equal the
        // row the eager path produced? Checked on every row, not a sample.
        let eager = Buffer::from_file(p).expect("eager read");
        let mut cols: Vec<(usize, usize)> = (0..lazy.rows()).map(|r| (r, r)).collect();
        cols.extend((0..lazy.rows()).map(|r| (r, r)));
        let mut mismatch = 0usize;
        let mut checked = 0usize;
        // Whole-file decode in one call, then compare row for row.
        if lazy.rows() > 0 {
            let all = lazy
                .decode(0, lazy.rows().saturating_sub(1))
                .expect("decode all");
            if all.len() != eager.row_count() {
                mismatch += 1;
            }
            for (r, (got, want)) in all.iter().zip(eager.rows()).enumerate() {
                checked += 1;
                if got != want {
                    if mismatch < 3 {
                        println!(
                            "    MISMATCH row {r}: lazy {:?} vs eager {:?}",
                            got.iter().take(30).collect::<String>(),
                            want.iter().take(30).collect::<String>()
                        );
                    }
                    mismatch += 1;
                }
            }
        }
        // The claim that makes wrapping possible without decoding: for an
        // ASCII row, segment count is ceil(byte_len / view_w), exactly.
        // Verified against the decoded rows for every row of the file.
        let vw = 100u64;
        let mut seg_mismatch = 0usize;
        let mut non_ascii_rows = 0usize;
        let mut chars_mismatch = 0usize;
        for r in 0..lazy.rows() {
            if lazy.is_narrow(r) {
                // Segment count from bytes alone: ceil(chars / view_w), where
                // `chars` was counted from lead bytes and never decoded.
                let from_index = lazy
                    .char_count(r)
                    .expect("narrow rows have a count")
                    .div_ceil(vw)
                    .max(1) as usize;
                let from_chars = eager.row(r).len().div_ceil(vw as usize).max(1);
                if from_index != from_chars {
                    seg_mismatch += 1;
                }
                // And the char count itself, for every row, not just narrow ones.
                if lazy.char_count(r) != Some(eager.row(r).len() as u64) {
                    chars_mismatch += 1;
                }
            } else {
                non_ascii_rows += 1;
            }
            let _ = lazy.byte_range(r);
        }
        let _ = cols;

        println!(
            "{:<24} {:>10} {:>8} {:>12} {:>12} {:>14} {:>10}",
            label,
            human(size),
            lazy.rows(),
            ms(index_t.as_micros() as f64),
            human((rss_after.saturating_sub(rss_before)) * 1024),
            format!(
                "{} / deep {}",
                ms(first_t.as_micros() as f64),
                ms(deep_t.as_micros() as f64)
            ),
            if mismatch == 0 && seg_mismatch == 0 && chars_mismatch == 0 {
                format!("yes {checked}/{checked}")
            } else {
                format!(
                    "NO {} rows, {} seg, {} chars",
                    mismatch, seg_mismatch, chars_mismatch
                )
            },
        );
        println!(
            "{:<24}   from bytes alone: chars exact on {} rows, segment counts exact on \
             {} NARROW rows ({} wide/tab rows decoded to measure)",
            "",
            checked,
            lazy.rows() - non_ascii_rows,
            non_ascii_rows
        );
        std::hint::black_box((first.len(), deep.len(), lazy.index_bytes()));
    }
    println!("\n(first window = rows 0..40; deep = 40 rows from the middle of the file)");
    println!();
}

/// Encoding detection does not need the whole file.
///
/// The design in §13.5 implied a full-file pass to validate UTF-8, and that
/// implication is what made encoding look like it forces an eager read. It does
/// not: a BOM is four bytes, and a valid UTF-8 PREFIX is the evidence the
/// decision is made on — which is also how `chardetng` is designed (its own
/// docs: "If you want to perform detection on just the prefix of a longer
/// stream, do not pass `last=true`"). Combined with lazy loading, no step of
/// opening a file needs the whole file.
///
/// What this measures: the cost of deciding from a prefix, and whether that
/// decision agrees with the whole-file answer.
#[test]
#[ignore = "performance measurement; run explicitly with --ignored --nocapture"]
fn bench_detect_from_prefix() {
    /// The prefix the ladder looks at. One page-cache read; big enough for a
    /// BOM, a shebang and many kilobytes of text.
    const PREFIX: usize = 64 * 1024;

    /// The std-only rungs: BOM sniffing, then "does this prefix validate as
    /// UTF-8". The legacy rung (chardetng) is the one that needs a crate; it is
    /// fed the same prefix and is documented for exactly that.
    fn sniff(prefix: &[u8]) -> &'static str {
        if prefix.starts_with(b"\xef\xbb\xbf") {
            return "utf-8 (BOM)";
        }
        if prefix.starts_with(b"\xff\xfe\x00\x00") || prefix.starts_with(b"\x00\x00\xfe\xff") {
            return "utf-32 (BOM)";
        }
        if prefix.starts_with(b"\xff\xfe") {
            return "utf-16le (BOM)";
        }
        if prefix.starts_with(b"\xfe\xff") {
            return "utf-16be (BOM)";
        }
        match std::str::from_utf8(prefix) {
            Ok(_) => "utf-8 (prefix validates)",
            // Not valid: either the file is legacy, or the prefix ended
            // mid-sequence. The rung below has to look, and for a prefix the
            // trailing partial sequence is expected rather than evidence.
            Err(e) if e.error_len().is_none() => "utf-8 (prefix cut mid-sequence)",
            Err(_) => "legacy -> detector",
        }
    }

    let paths: Vec<(&str, &str)> = vec![
        ("big.rs (7 MB)", "/tmp/ranoperf/big.rs"),
        ("big.log (30 MB)", "/tmp/ranoperf/big.log"),
        ("huge200.log (193 MB)", "/tmp/ranoperf/huge200.log"),
        ("utf8-bom.txt", "/tmp/ranoperf/utf8-bom.txt"),
        ("utf16le-bom.txt", "/tmp/ranoperf/utf16le-bom.txt"),
        ("latin1.txt", "/tmp/ranoperf/latin1.txt"),
        ("cjk-check.md", "/tmp/cjk-check.md"),
    ];
    println!("\nencoding from a {PREFIX}-byte prefix — cost, and agreement with the whole file\n");
    println!(
        "{:<24} {:>10} {:>16} {:>14} {:>18} {:>18}",
        "file", "size", "prefix read", "whole-file scan", "prefix says", "whole file says"
    );
    println!("{}", "-".repeat(106));
    for (label, path) in paths {
        let p = Path::new(path);
        if !p.exists() {
            continue;
        }
        let size = std::fs::metadata(p).map(|m| m.len()).unwrap_or(0);

        let (t_prefix, prefix_says) = {
            use std::io::Read as _;
            let t = Instant::now();
            let mut f = fs::File::open(p).expect("open");
            let mut buf = vec![0u8; PREFIX];
            let n = f.read(&mut buf).expect("read");
            buf.truncate(n);
            let says = sniff(&buf);
            (t.elapsed(), says)
        };

        // The whole file, for contrast: the decode that lazy loading avoids.
        let (t_all, all_says) = {
            let t = Instant::now();
            let bytes = fs::read(p).expect("read");
            let says = sniff(&bytes);
            (t.elapsed(), says)
        };

        println!(
            "{:<24} {:>10} {:>16} {:>14} {:>18} {:>18}",
            label,
            human(size),
            ms(t_prefix.as_micros() as f64),
            ms(t_all.as_micros() as f64),
            prefix_says,
            all_says
        );
    }
    println!(
        "\n(a prefix that validates as UTF-8 is the evidence; a prefix cut mid-sequence is not,"
    );
    println!("and a prefix that is not valid UTF-8 at all is what sends the read to the detector)");
    println!();
}

/// Which EDIT operations scale with the document? "Size does not influence
/// editability" is the principle, so the question is which edit paths violate
/// it — measured on a real 2.6M-row file, not reasoned about.
///
/// Each row of the table is one thing a keystroke or a key can do, run on the
/// same buffer at two sizes. A cost that scales with the file is a violation;
/// a cost that tracks the ROW is not (that is what "the row you are typing in"
/// costs, and it is the same at every file size).
#[test]
#[ignore = "performance measurement; run explicitly with --ignored --nocapture"]
fn bench_edit_scaling() {
    let paths: Vec<(&str, &str)> = vec![
        ("big.log (30 MB)", "/tmp/ranoperf/big.log"),
        ("huge200.log (193 MB)", "/tmp/ranoperf/huge200.log"),
    ];
    println!("\nedit operations vs document size — median of 5\n");
    println!(
        "{:<24} {:>10} {:>14} {:>14} {:>14} {:>14} {:>14}",
        "file", "rows", "type a char", "insert a line", "delete a line", "undo one", "join lines"
    );
    println!("{}", "-".repeat(112));
    for (label, path) in paths {
        let p = Path::new(path);
        if !p.exists() {
            continue;
        }
        let mut buf = Buffer::from_file(p).expect("read");
        let rows = buf.row_count();
        // Edit at the START, which is the worst case for anything that shifts
        // the lines after the edit point.
        let (row, col) = (0usize, 0usize);

        let (t_char, _) = time(5, || {
            buf.insert_char(row, col, 'x');
            buf.backspace(row, col + 1);
        });
        let (t_ins, _) = time(5, || {
            let at = buf.insert_lines_at(row, vec![vec!['x']]);
            let _ = at;
            buf.remove_row(row);
        });
        let (t_del, _) = time(5, || {
            let mut second = buf.row(row + 1).clone();
            buf.insert_row(row + 1, second.clone());
            second.clear();
            buf.remove_row(row + 1);
        });
        // Undo: what `begin_action` + `finish_step` snapshot for one insertion.
        let (t_undo, _) = time(5, || {
            let r = buf.row(row).clone();
            std::hint::black_box(r);
        });
        let (t_join, _) = time(5, || {
            let l = buf.remove_row(row);
            buf.insert_row(row, l);
        });

        println!(
            "{:<24} {:>10} {:>14} {:>14} {:>14} {:>14} {:>14}",
            label,
            rows,
            ms(t_char),
            ms(t_ins),
            ms(t_del),
            ms(t_undo),
            ms(t_join),
        );
    }
    println!("\n(scaled with the file = a violation; tracking the row = the floor)");
    println!();
}

/// Does chunking the row store fix the line-insert violation?
///
/// Measured above: `Vec<Vec<char>>` costs 1.5 ms to insert a row at the top of a
/// 2.6M-row file, because that is a memmove of 2.6M `Vec` headers. "Size does
/// not influence editability" says that must go. A chunked row store makes the
/// insert a memmove of one CHUNK instead — and, crucially, keeps line lookup
/// O(1) (chunk index + offset), which is the property §13.6a's survey says the
/// piece table gives up.
mod chunked {
    /// Rows per chunk. 1,024 rows is 8 KiB of pointers to shift on an insert —
    /// one cache line's worth of work beyond the copy, at any file size.
    const CHUNK: usize = 1024;

    /// A row store with O(1) lookup and O(chunk) structural edits.
    pub struct Chunked {
        chunks: Vec<Vec<Vec<char>>>,
        rows: usize,
    }

    impl Chunked {
        pub fn from_rows(rows: Vec<Vec<char>>) -> Self {
            let mut chunks: Vec<Vec<Vec<char>>> = Vec::new();
            let mut it = rows.into_iter().peekable();
            while it.peek().is_some() {
                chunks.push(it.by_ref().take(CHUNK).collect());
            }
            let n = chunks.iter().map(Vec::len).sum();
            Self { chunks, rows: n }
        }

        pub fn rows(&self) -> usize {
            self.rows
        }

        /// O(1): which chunk, and where in it.
        fn locate(&self, r: usize) -> (usize, usize) {
            let c = r / CHUNK;
            (c.min(self.chunks.len().saturating_sub(1)), r % CHUNK)
        }

        pub fn row(&self, r: usize) -> &[char] {
            let (c, o) = self.locate(r);
            self.chunks
                .get(c)
                .and_then(|ch| ch.get(o))
                .map(Vec::as_slice)
                .unwrap_or(&[])
        }

        /// Insert a row: O(CHUNK), not O(rows).
        pub fn insert(&mut self, r: usize, row: Vec<char>) {
            if self.chunks.is_empty() {
                self.chunks.push(vec![row]);
                self.rows = 1;
                return;
            }
            let (c, o) = self.locate(r);
            let chunk = &mut self.chunks[c];
            let at = o.min(chunk.len());
            chunk.insert(at, row);
            self.rows += 1;
            if chunk.len() > CHUNK * 2 {
                let tail = chunk.split_off(CHUNK);
                self.chunks.insert(c + 1, tail);
            }
        }

        pub fn remove(&mut self, r: usize) -> Vec<char> {
            let (c, o) = self.locate(r);
            let out = if let Some(chunk) = self.chunks.get_mut(c) {
                if o < chunk.len() {
                    chunk.remove(o)
                } else {
                    Vec::new()
                }
            } else {
                Vec::new()
            };
            self.rows = self.rows.saturating_sub(1);
            if self.chunks.get(c).is_some_and(|ch| ch.is_empty()) && self.chunks.len() > 1 {
                self.chunks.remove(c);
            }
            out
        }
    }
}

/// The chunked store against the plain one, on the operations that scaled.
#[test]
#[ignore = "performance measurement; run explicitly with --ignored --nocapture"]
fn bench_chunked_fixes_the_scaling() {
    let paths: Vec<(&str, &str)> = vec![
        ("big.rs (360k rows)", "/tmp/ranoperf/big.rs"),
        ("big.log (400k rows)", "/tmp/ranoperf/big.log"),
        ("huge200.log (2.6M rows)", "/tmp/ranoperf/huge200.log"),
    ];
    println!("\nchunked row store vs Vec<Vec<char>> — median of 5, µs\n");
    println!(
        "{:<26} {:>10} {:>16} {:>16} {:>16} {:>16} {:>16}",
        "file",
        "rows",
        "plain insert",
        "chunked insert",
        "plain lookup",
        "chunked lookup",
        "chunked del"
    );
    println!("{}", "-".repeat(120));
    for (label, path) in paths {
        let p = Path::new(path);
        if !p.exists() {
            continue;
        }
        let buf = Buffer::from_file(p).expect("read");
        let rows = buf.row_count();

        let mut plain = buf.to_lines();
        let (t_plain_ins, _) = time(5, || {
            plain.insert(0, vec!['x']);
            plain.remove(0);
        });
        let (t_plain_get, _) = time(5, || {
            let r = plain.len() / 2;
            std::hint::black_box(&plain[r]);
        });

        let mut ch = chunked::Chunked::from_rows(buf.to_lines());
        assert_eq!(ch.rows(), rows, "the chunked store must hold every row");
        // Correctness, not just speed: every row must read back identically,
        // and structural edits must keep the numbering right. This is the check
        // that earned its keep on the lazy prototype (it found a phantom row).
        for r in 0..rows {
            assert_eq!(ch.row(r), buf.row(r).as_slice(), "row {r} differs");
        }
        // Insert in the middle and confirm the shift is exact.
        let mid = rows / 2;
        ch.insert(mid, vec!['Z']);
        assert_eq!(ch.rows(), rows + 1);
        assert_eq!(ch.row(mid), &['Z']);
        assert_eq!(ch.row(mid + 1), buf.row(mid).as_slice(), "shifted row");
        assert_eq!(ch.row(mid - 1), buf.row(mid - 1).as_slice(), "row before");
        assert_eq!(ch.remove(mid), vec!['Z']);
        assert_eq!(ch.rows(), rows);
        for r in [0usize, mid - 1, mid, rows - 1] {
            assert_eq!(ch.row(r), buf.row(r).as_slice(), "row {r} after the edit");
        }
        let (t_ch_ins, _) = time(5, || {
            ch.insert(0, vec!['x']);
            ch.remove(0);
        });
        let (t_ch_get, _) = time(5, || {
            let r = ch.rows() / 2;
            std::hint::black_box(ch.row(r));
        });
        let (t_ch_del, _) = time(5, || {
            ch.insert(0, vec!['x']);
            let _ = ch.remove(0);
        });

        println!(
            "{:<26} {:>10} {:>16} {:>16} {:>16} {:>16} {:>16}",
            label,
            rows,
            ms(t_plain_ins),
            ms(t_ch_ins),
            ms(t_plain_get),
            ms(t_ch_get),
            ms(t_ch_del),
        );
    }
    println!("\n(insert at row 0: the worst case for anything that shifts what follows)");
    println!();
}

/// Scrolling, profiled. Not attributed by hand — this test exists to be run
/// under `perf record`, so the split comes from samples rather than from my
/// guesses about which phase is expensive.
///
/// It scrolls the whole file, one wheel notch at a time, doing exactly what the
/// run loop does per frame: adjust the scroll, highlight the window, rebuild the
/// wrap table if it is stale, draw. Then it scrolls back up. Repeat, so there is
/// enough work to sample.
#[test]
#[ignore = "performance measurement; run explicitly with --ignored --nocapture"]
fn bench_scroll_profile() {
    let path = std::env::var("RANO_PROFILE_FILE")
        .unwrap_or_else(|_| "/home/dead/Projects/letibot/letibot/crates/tui/src/app.rs".into());
    let p = Path::new(&path);
    if !p.exists() {
        eprintln!("{path} is not here; set RANO_PROFILE_FILE");
        return;
    }
    let size = std::fs::metadata(p).map(|m| m.len()).unwrap_or(0);
    let t = Instant::now();
    let buf = Buffer::from_file(p).expect("read");
    let open = t.elapsed();
    let mut e = Editor::new(buf, config::Config::default());
    e.text_w = 120;
    e.text_h = 36;
    let mut terminal = Terminal::new(TestBackend::new(120, 40)).expect("backend");

    // The first frame (open + first window highlight).
    let t = Instant::now();
    e.ensure_wrap_prefix();
    e.ensure_highlight();
    terminal.draw(|f| ui::draw(f, &e)).expect("draw");
    let first = t.elapsed();

    let use_wheel = |e: &mut Editor| {
        let me = |kind| crossterm::event::MouseEvent {
            kind,
            column: 0,
            row: 0,
            modifiers: crossterm::event::KeyModifiers::NONE,
        };
        e.handle_mouse(me(crossterm::event::MouseEventKind::ScrollDown))
    };

    // One pass: scroll to the bottom, then back to the top, timing every frame.
    let rows = e.bs().buf.row_count();
    let notches = (rows / 3) + 1;
    println!(
        "\nscrolling {} — {} bytes, {} rows, {notches} notches each way\n",
        path, size, rows
    );
    println!("  open (read+decode)   {}", ms(open.as_micros() as f64));
    println!("  first frame          {}", ms(first.as_micros() as f64));

    let mut down: Vec<u128> = Vec::new();
    let mut up: Vec<u128> = Vec::new();
    for pass in 0..3 {
        for _ in 0..notches {
            let t = Instant::now();
            use_wheel(&mut e);
            e.adjust_scroll(e.text_h);
            e.adjust_scroll_x();
            e.ensure_highlight();
            terminal.draw(|f| ui::draw(f, &e)).expect("draw");
            down.push(t.elapsed().as_nanos());
        }
        for _ in 0..notches {
            let t = Instant::now();
            e.handle_mouse(crossterm::event::MouseEvent {
                kind: crossterm::event::MouseEventKind::ScrollUp,
                column: 0,
                row: 0,
                modifiers: crossterm::event::KeyModifiers::NONE,
            });
            e.adjust_scroll(e.text_h);
            e.adjust_scroll_x();
            e.ensure_highlight();
            terminal.draw(|f| ui::draw(f, &e)).expect("draw");
            up.push(t.elapsed().as_nanos());
        }
        eprintln!(
            "pass {pass}: down {} frames, up {} frames",
            down.len(),
            up.len()
        );
    }

    let report = |label: &str, v: &[u128]| {
        if v.is_empty() {
            return;
        }
        let mut s = v.to_vec();
        s.sort_unstable();
        let med = s[s.len() / 2] as f64 / 1e3;
        let p99 = s[(s.len() * 99 / 100).min(s.len() - 1)] as f64 / 1e3;
        let worst = s[s.len() - 1] as f64 / 1e3;
        println!(
            "  {label:<20} {:<12} p99 {:<12} worst {:<12} (n={})",
            ms(med),
            ms(p99),
            ms(worst),
            v.len()
        );
    };
    report("scroll down", &down);
    report("scroll up", &up);

    // The same scroll without the highlight, to separate "the frame" from "the
    // window highlight the frame asked for".
    let mut no_hl: Vec<u128> = Vec::new();
    for _ in 0..notches.min(2_000) {
        let t = Instant::now();
        use_wheel(&mut e);
        e.adjust_scroll(e.text_h);
        e.adjust_scroll_x();
        terminal.draw(|f| ui::draw(f, &e)).expect("draw");
        no_hl.push(t.elapsed().as_nanos());
    }
    report("down, no highlight", &no_hl);

    // And the draw alone, on a settled screen: the floor.
    let mut draw_only: Vec<u128> = Vec::new();
    e.ensure_highlight();
    for _ in 0..notches.min(2_000) {
        let t = Instant::now();
        terminal.draw(|f| ui::draw(f, &e)).expect("draw");
        draw_only.push(t.elapsed().as_nanos());
    }
    report("draw alone", &draw_only);
    println!();
}

/// Either side of where `LARGE_BUFFER` used to be (2 MiB).
///
/// Kept after the threshold was removed, because it is the regression test for
/// the cliff it caused: `Editor::highlight_now` used to pick a whole-document
/// highlight below that size and a window above it, which cost **402 ms per
/// keystroke on a 2.09 MB file against 2.5 ms on the same file one byte over**.
/// Both columns must now be the window, at every size.
#[test]
#[ignore = "performance measurement; run explicitly with --ignored --nocapture"]
fn bench_threshold_cliff() {
    println!("\nhighlight cost either side of LARGE_BUFFER (2 MiB = 2,097,152)\n");
    println!(
        "{:<28} {:>10} {:>14} {:>16} {:>16}",
        "file", "size", "whole document", "per keystroke", "windowed"
    );
    println!("{}", "-".repeat(88));
    for (label, path) in [
        ("just under (2.09 MB)", "/tmp/ranoperf/under2mb.rs"),
        ("just over  (2.11 MB)", "/tmp/ranoperf/over2mb.rs"),
        ("big.rs (7 MB)", "/tmp/ranoperf/big.rs"),
    ] {
        let p = Path::new(path);
        if !p.exists() {
            continue;
        }
        let size = std::fs::metadata(p).map(|m| m.len()).unwrap_or(0);
        let buf = Buffer::from_file(p).expect("read");
        let rows = buf.row_count();
        let mut e = Editor::new(buf.clone(), config::Config::default());
        e.text_w = 120;
        e.text_h = 36;

        // The whole document, directly.
        let (t_whole, _) = time(3, || {
            e.bs_mut().hl.refresh(&buf);
        });
        // A keypress: the edit plus the frame's highlight, which is where the
        // threshold decides which path runs.
        let (t_key, _) = time(5, || {
            e.handle_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
            e.ensure_highlight();
        });
        // And a viewport window, for comparison.
        let (t_win, _) = time(3, || {
            let win = crate::syntax::Window::rows(rows / 2, rows / 2 + 80);
            e.bs_mut().hl.refresh_window(&buf, win);
        });
        println!(
            "{:<28} {:>10} {:>14} {:>16} {:>16}",
            label,
            human(size),
            ms(t_whole),
            ms(t_key),
            ms(t_win)
        );
    }
    println!("\n(no threshold: a keystroke takes the window at every size. The whole-document");
    println!(" column is `hl.refresh`, which only the export path and the tests call now.)");
    println!();
}

/// Load throughput: how fast rows arrive, and where the time goes.
///
/// This exists because a load was quadratic in disguise and nothing measured
/// it. Each adopted batch bumped `edit_gen`, invalidating the wrap table, and
/// the next frame re-measured EVERY row so far — ~800M row measurements across
/// a 2.6M-line file, which showed up as 315k rows/s against a page-cached read
/// of 17 GB/s. Extending the table from the join instead of rebuilding it is
/// what the number below is checking: it should now be a small multiple of the
/// raw read, not thousands of times it.
#[test]
#[ignore = "performance measurement; run explicitly with --ignored --nocapture"]
fn bench_load_throughput() {
    let path =
        std::env::var("RANO_PROFILE_FILE").unwrap_or_else(|_| "/tmp/ranoperf/huge200.log".into());
    let p = Path::new(&path);
    if !p.exists() {
        eprintln!("{path} is not here; set RANO_PROFILE_FILE");
        return;
    }
    let size = std::fs::metadata(p).map(|m| m.len()).unwrap_or(0);

    // A raw read, for the ceiling.
    let t = Instant::now();
    let bytes = fs::read(p).expect("read");
    let raw = t.elapsed();
    let raw_bytes = bytes.len();
    drop(bytes);

    // The whole editor loop: adopt, highlight the window, extend the wrap
    // table, draw — which is what the run loop does while a file arrives.
    let t = Instant::now();
    let mut ed = Editor::new(Buffer::new(), config::Config::default());
    ed.text_w = 120;
    ed.text_h = 36;
    ed.start_load(p).expect("start load");
    let mut terminal = Terminal::new(TestBackend::new(120, 40)).expect("backend");
    let mut frames = 0usize;
    let deadline = Instant::now() + Duration::from_secs(120);
    while ed.loading() {
        assert!(Instant::now() < deadline, "load did not finish");
        ed.load_poll();
        ed.apply_startup_pos();
        ed.adjust_scroll(ed.text_h);
        ed.adjust_scroll_x();
        ed.ensure_highlight();
        terminal.draw(|f| ui::draw(f, &ed)).expect("draw");
        frames += 1;
    }
    let loaded = t.elapsed();
    let rows = ed.bs().buf.row_count();
    println!("\nload throughput — {path}\n");
    println!("  {:<28} {}", "file", human(size));
    println!("  {:<28} {}", "rows", rows);
    println!("  {:<28} {}", "raw fs::read", ms(raw.as_micros() as f64));
    println!(
        "  {:<28} {}  ({}k rows/s, {} frames)",
        "load + highlight + draw",
        ms(loaded.as_micros() as f64),
        rows as f64 / loaded.as_secs_f64() / 1000.0,
        frames
    );
    println!(
        "  {:<28} {:.1}x the read, {:.1} MB/s through the loop",
        "",
        loaded.as_secs_f64() / raw.as_secs_f64(),
        raw_bytes as f64 / 1e6 / loaded.as_secs_f64()
    );
    assert!(rows > 0, "the load produced nothing");
    println!();
}
