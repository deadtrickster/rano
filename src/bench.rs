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
use std::path::{Path, PathBuf};
use std::time::Instant;

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
    let longest = buf.lines.iter().map(Vec::len).max().unwrap_or(0);
    Some(Sample {
        label: label.to_string(),
        path: p.to_path_buf(),
        bytes,
        lines: buf.lines.len(),
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
        ("minified.js (1 line)", "/tmp/ranoperf/minified.js"),
        ("oneline.txt (1 line)", "/tmp/ranoperf/oneline.txt"),
        ("big.log (30 MB)", "/tmp/ranoperf/big.log"),
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
        let mid = e.bs().buf.lines.len() / 2;
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
        ("minified.js (1 line)", "/tmp/ranoperf/minified.js"),
        ("oneline.txt (1 line)", "/tmp/ranoperf/oneline.txt"),
        ("big.log (30 MB)", "/tmp/ranoperf/big.log"),
        ("big.rs (7 MB)", "/tmp/ranoperf/big.rs"),
    ];
    println!("\nkeystroke breakdown — median of 3, µs\n");
    println!(
        "{:<22} {:>9} {:>11} {:>12} {:>12} {:>11} {:>11} {:>11}",
        "file",
        "size",
        "reparse",
        "hl.refresh",
        "highlight_now",
        "wrap tbl",
        "buf edit",
        "keypress"
    );
    println!("{}", "-".repeat(112));
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

        let first = buf.lines.first().map(|l| l.iter().collect::<String>());
        let lang = crate::syntax::detect(buf.name.as_deref(), first.as_deref());
        let src = buf.text();
        let (t_parse, _) = match lang {
            Some(lang) => {
                let mut parser = tree_sitter::Parser::new();
                parser.set_language(&lang.language()).expect("language");
                time(3, || {
                    std::hint::black_box(parser.parse(src.as_bytes(), None).expect("parse"));
                })
            }
            None => (0.0, 0.0),
        };

        let (t_editbuf, _) = time(3, || {
            e.bs_mut().buf.insert_char(0, 0, 'x');
            e.bs_mut().buf.backspace(0, 1);
        });

        // The whole highlight (parse + capture walk + grid), and the walk on
        // its own, so a slow keystroke can be attributed inside `refresh`.
        let (t_refresh, _) = time(3, || {
            e.bs_mut().hl.refresh(&buf);
        });
        let (t_hlnow, _) = time(3, || {
            e.highlight_now();
        });
        let (t_wrap, _) = time(3, || {
            e.bs_mut().edit_gen = e.bs().edit_gen.wrapping_add(1);
            e.ensure_wrap_prefix();
        });
        // ONE insertion: a key+backspace pair is two edits and doubled every
        // number in this table before 2026-09-22.
        let (t_key, _) = time(3, || {
            e.handle_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
        });

        println!(
            "{:<22} {:>9} {:>11} {:>12} {:>12} {:>11} {:>11} {:>11}",
            label,
            human(size),
            ms(t_parse),
            ms(t_refresh),
            ms(t_hlnow),
            ms(t_wrap),
            ms(t_editbuf),
            ms(t_key),
        );
    }
    println!();
}
