//! The `rano` binary: arguments, the terminal, and the event loop. The editor
//! itself is the library's (`rano::editor`); this is one host of it.

use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crossterm::event::{
    self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    Event, KeyEventKind,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use rano::buffer::{Buffer, Pos};
use rano::editor::{Area, Editor};
use rano::send_ctrl::split_position;
use rano::{config, export, send_ctrl, syntax, ui};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

/// The command line, parsed.
///
/// Hand-rolled rather than a parser crate: files, a handful of flags, and no
/// subcommands. What it must get right is that a flag's VALUE is
/// not mistaken for the file — `rano --line 42 main.rs` — which the positional
/// `args.first()` it replaced did get wrong.
#[derive(Debug, Default, PartialEq, Eq)]
struct Args {
    /// In command-line order; each opens as its own buffer, the first one
    /// current. `--line`/`--column` apply to the first.
    files: Vec<String>,
    /// Each file's own position, 1-based (line, column), from `file:line:col`
    /// or a preceding `+line[,col]`; parallel to `files`.
    positions: Vec<Option<(usize, Option<usize>)>>,
    /// 1-based, as a person reads them and as `Ln` in the status bar counts.
    /// Converted to 0-based before the editor sees them.
    line: Option<usize>,
    col: Option<usize>,
    export: Option<export::Format>,
    help: bool,
    version: bool,
}

/// Split `--opt=value` into `("--opt", Some("value"))`.
fn split_eq(a: &str) -> (&str, Option<&str>) {
    match a.split_once('=') {
        Some((k, v)) => (k, Some(v)),
        None => (a, None),
    }
}

fn parse_num(flag: &str, v: &str) -> Result<usize, String> {
    v.parse::<usize>()
        .map_err(|_| format!("{flag} needs a number, got {v:?}"))
}

/// `+line` or `+line,col` (nano's and vi's form), for the file after it.
fn plus_position(arg: &str) -> Option<(usize, Option<usize>)> {
    let rest = arg.strip_prefix('+')?;
    let (line, col) = match rest.split_once(',') {
        Some((l, c)) => (l, Some(c.parse().ok()?)),
        None => (rest, None),
    };
    Some((line.parse().ok()?, col))
}

fn parse_args(args: &[String]) -> Result<Args, String> {
    let mut out = Args::default();
    let mut plus: Option<(usize, Option<usize>)> = None;
    let mut i = 0usize;
    while i < args.len() {
        let (key, inline) = split_eq(&args[i]);
        // A flag that takes a value reads it from `=` or the next word.
        let mut value = |name: &str| -> Result<String, String> {
            if let Some(v) = inline {
                return Ok(v.to_string());
            }
            i += 1;
            args.get(i)
                .cloned()
                .ok_or_else(|| format!("{name} needs a value"))
        };
        match key {
            "-h" | "--help" => out.help = true,
            "-V" | "--version" => out.version = true,
            "-l" | "--line" => out.line = Some(parse_num("--line", &value("--line")?)?),
            "-c" | "--col" | "--column" => {
                out.col = Some(parse_num("--column", &value("--column")?)?)
            }
            "--export" => {
                let v = value("--export")?;
                out.export = Some(export::Format::parse(&v).ok_or_else(|| {
                    format!(
                        "unknown export format {v:?} (expected {})",
                        export::Format::NAMES
                    )
                })?);
            }
            other if other.len() > 1 && other.starts_with('-') => {
                return Err(format!("unknown option {other}"));
            }
            other if plus_position(other).is_some() => plus = plus_position(other),
            other => match split_position(other) {
                Some((name, line, col)) => {
                    out.files.push(name);
                    out.positions.push(Some((line, col)));
                    plus = None;
                }
                None => {
                    out.files.push(other.to_string());
                    out.positions.push(plus.take());
                }
            },
        }
        i += 1;
    }
    if out.export.is_some() && out.files.len() > 1 {
        return Err(format!(
            "--export takes one file, got {} ({})",
            out.files.len(),
            out.files.join(", ")
        ));
    }
    Ok(out)
}

impl Args {
    /// The first file: the one that opens current, and the one `--line`,
    /// `--column` and `--export` act on.
    fn file(&self) -> Option<&str> {
        self.files.first().map(String::as_str)
    }
}

const USAGE: &str = "\
usage: rano [options] [file...]

  -l, --line N      put the cursor on line N (1-based) and centre it
  -c, --column N    put the cursor on column N (1-based)
                    (both apply to the first file)
  file:LINE[:COL]   open that file at that position
  +LINE[,COL] file  the same, for the file after it
  -h, --help        this
  -V, --version     the version

exporting (no terminal needed):
  --export FORMAT [file]   write the file to stdout and exit
                           formats: html, ansi, markdown, text";

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let args = match parse_args(&argv) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("rano: {e}");
            eprintln!("{USAGE}");
            std::process::exit(2);
        }
    };
    if args.help {
        println!("{USAGE}");
        return;
    }
    if args.version {
        println!("rano {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    // `--export`: read, colourise, write, exit. No terminal is touched, which is
    // what makes it usable in a pipe and as a way to measure the highlighter
    // without the UI in the way.
    if let Some(fmt) = args.export {
        if let Err(e) = export_to_stdout(args.file().map(str::to_string), fmt) {
            eprintln!("rano: {e}");
            std::process::exit(1);
        }
        return;
    }
    // The file is NOT read here. Reading it before the terminal is set up is
    // what made `rano huge.log` a dead screen — no frame, no event loop, so not
    // even ^C — for 575 ms at 184 MB and about six seconds at 2 GB. The run
    // loop starts a loader instead and adopts rows as they arrive; see
    // `load_ctrl.rs` and `loader.rs`.
    let file = args.file().map(str::to_string);
    let mut buf = Buffer::new();
    let mut load: Option<PathBuf> = None;
    if let Some(f) = &file {
        let p = Path::new(f);
        // The name is set either way, so language detection, the title bar and
        // the gutter are right from the first frame.
        buf.name = Some(p.to_path_buf());
        if p.exists() {
            load = Some(p.to_path_buf());
        }
    }

    // 1-based on the command line, 0-based internally; a missing column is 0.
    let to_pos = |(l, c): (usize, Option<usize>)| Pos {
        row: l.saturating_sub(1),
        col: c.unwrap_or(1).saturating_sub(1),
    };
    // `--line`/`--column` win over the first file's own `file:line`.
    let pos = args
        .line
        .map(|l| (l, args.col))
        .or(args.positions.first().copied().flatten())
        .map(to_pos);
    let mut cfg = config::load();
    // The other files become buffers of their own, read when first visited.
    // Naming several files is asking for several buffers, so the session is
    // multibuffer whatever the config says: F8 and jumps then add buffers
    // rather than replacing the one in view.
    let rest: Vec<(PathBuf, Option<Pos>)> = args
        .files
        .iter()
        .zip(args.positions.iter())
        .skip(1)
        .map(|(f, p)| (PathBuf::from(f), p.map(to_pos)))
        .collect();
    if !rest.is_empty() {
        cfg.multibuffer = true;
    }
    if let Err(e) = run(buf, load, rest, pos, cfg) {
        eprintln!("rano: {}", e);
        std::process::exit(1);
    }
}

/// Read a file (or stdin), colourise it, and write it out. No terminal is
/// touched: this is the path that makes the highlighter measurable on its own,
/// and what `rano --export html big.rs > big.html` uses.
fn export_to_stdout(path: Option<String>, fmt: export::Format) -> io::Result<()> {
    let buf = match &path {
        Some(p) => match Buffer::from_file(Path::new(p)) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("rano: cannot read {p}: {e}");
                std::process::exit(1);
            }
        },
        None => {
            // No file: read stdin, which is what a pipe wants.
            let mut text = String::new();
            io::Read::read_to_string(&mut io::stdin(), &mut text)?;
            let mut b = Buffer::new();
            b.set_rows(text.lines().map(|l| l.chars().collect()).collect());
            if b.rows_is_empty() {
                b.push_row(Vec::new());
            }
            b
        }
    };
    // A name is what `detect` works from, and it also supplies the markdown
    // fence tag and the HTML title, so it is set before highlighting.
    let mut buf = buf;
    let title = match &path {
        Some(p) => p.clone(),
        None => String::from("<stdin>"),
    };
    buf.name = Some(std::path::PathBuf::from(&title));
    // The whole document, not a window: colouring everything is the point of
    // an export, and there is no viewport to bound it by.
    let mut hl = syntax::Highlighter::new();
    hl.refresh(&buf);
    let style_of = |p: Pos| hl.style_at(p);
    let out = export::render(&buf.rows_vec(), 8, &title, fmt, &style_of);
    // Written, not `print!`ed: a closed pipe is the normal end of
    // `rano --export ansi f.rs | head`, not a panic. `println!` aborts the
    // process with a broken-pipe message when the reader goes away.
    let mut stdout = io::stdout().lock();
    if let Err(e) = stdout.write_all(out.as_bytes()) {
        if e.kind() == io::ErrorKind::BrokenPipe {
            return Ok(());
        }
        return Err(e);
    }
    stdout.flush()?;
    Ok(())
}

fn run(
    buf: Buffer,
    load: Option<PathBuf>,
    rest: Vec<(PathBuf, Option<Pos>)>,
    pos: Option<Pos>,
    cfg: config::Config,
) -> io::Result<()> {
    // Panic guard: restore the terminal, then report the panic normally.
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen, DisableMouseCapture);
        prev(info);
    }));
    enable_raw_mode()?;
    execute!(io::stdout(), EnterAlternateScreen, EnableMouseCapture)?;
    execute!(io::stdout(), EnableBracketedPaste)?;
    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::new(backend)?;
    let mut ed = Editor::new(buf, cfg);
    // One request at startup, off the main thread. `run` rather than
    // `Editor::new` because it is a property of RUNNING, not of being — no test
    // makes a network call by constructing an editor.
    ed.start_update_check();
    if let Some(cmd) = ed.config.send_command.clone() {
        ed.on_send = Some(send_ctrl::command_sender(cmd));
    }
    // 0-based, once, here: `--line 1` is the first row, and the editor's own
    // coordinates are 0-based throughout. `--column` alone leaves the row at 0.
    ed.bs_mut().goto = pos;
    if let Some(path) = load
        && let Err(e) = ed.start_load(&path)
    {
        // The one load failure reported before a frame is drawn: there is
        // nothing on screen yet to attach a status line to.
        disable_raw_mode()?;
        execute!(io::stdout(), LeaveAlternateScreen, DisableMouseCapture)?;
        eprintln!("rano: cannot read {}: {}", path.display(), e);
        std::process::exit(1);
    }
    ed.add_deferred_buffers_at(&rest);
    // D6 dirty-draw: redraw only when something changed. Any handled key,
    // paste or resize dirties (coarse); the pollers below report their own
    // state changes. text_w is the FULL viewport width now (draw renders
    // full-width lines); justify keeps its old wrap width via a -2 there.
    let mut dirty = true;
    let result = loop {
        let size = terminal.size()?;
        dirty |= ed.set_area(Area::new(0, 0, size.width, size.height));
        // Before the scroll: `--line` CENTRES the target, and centring sets
        // the scroll. Running `adjust_scroll` first would then pull the view
        // back to the nearest edge, which is the opposite of centring. It is
        // also why this waits for the row — see `apply_startup_pos`.
        // A file from the command line whose buffer just became current.
        dirty |= ed.start_pending_load();
        dirty |= ed.refresh_prompt_hints();
        dirty |= ed.refresh_diff_view();
        dirty |= ed.refresh_info_view();
        // A prefix's card appears after a pause: redraw when it becomes due.
        let card = ed.pending_card().is_some();
        if card != ed.pending.card_shown {
            ed.pending.card_shown = card;
            dirty = true;
        }
        ed.apply_startup_pos();
        ed.adjust_scroll(ed.text_h);
        ed.adjust_scroll_x();
        if dirty {
            // The frame's highlight, after the scroll the window is measured
            // against. Lazy on purpose: a burst of keystrokes between two
            // frames costs one highlight here rather than one per key, and a
            // huge file's first highlight is this window rather than the whole
            // document.
            ed.ensure_highlight();
            // Hide the physical cursor while the frame paints: the backend
            // moves it across the cells it writes, and on a fast scroll that
            // sweep shows as a ghost cursor blinking at painted cells (often
            // a line start, column 0, mid-screen). draw() re-shows it at the
            // positioned spot, or leaves it hidden when ui::draw skips
            // positioning (edit point outside the viewport).
            terminal.hide_cursor()?;
            terminal.draw(|f| ui::draw_in(f, f.area(), &ed))?;
            dirty = false;
        }
        if ed.quit {
            break Ok(());
        }
        // While a file is arriving the loop must come back promptly to adopt
        // it, or the text appears in 200 ms lumps and the load looks like a
        // stutter rather than a stream. 8 ms is a frame at 120 Hz; otherwise the
        // idle wait is the long one, because a keystroke is what ends it.
        //
        // And when the loader is SATURATED — it filled the adoption budget, so
        // more was ready — wait not at all: the budget is what keeps one
        // iteration short, not what paces the load. Without this the loop slept
        // 8 ms per 4096 rows and a 2.6M-line file took ten seconds instead of
        // one, with the disk idle in between.
        let wait = if ed.load_saturated {
            0
        } else if ed.loading() {
            8
        } else {
            200
        };
        // Come back when a prefix's card is due, not at the next keystroke.
        let wait = match ed.card_due() {
            Some(d) => wait.min(d.as_millis() as u64 + 1),
            None => wait,
        };
        if event::poll(Duration::from_millis(wait))? {
            match event::read()? {
                Event::Key(k) if matches!(k.kind, KeyEventKind::Press | KeyEventKind::Repeat) => {
                    ed.handle_key(k);
                    dirty = true;
                }
                Event::Paste(t) => {
                    ed.paste_text(&t);
                    dirty = true;
                }
                Event::Resize(..) => dirty = true,
                Event::Mouse(m) => dirty |= ed.handle_mouse(m),
                _ => {}
            }
        }
        // The load, beside the other pollers: bounded per iteration, never
        // waiting, and it reports its own state changes.
        dirty |= ed.load_poll();
        dirty |= ed.diag_flush(Instant::now());
        dirty |= ed.tick_status();
        dirty |= ed.lsp_poll();
        dirty |= ed.lsp_flush(Instant::now());
        ed.completion_retry_poll();
        dirty |= ed.exec_poll();
        dirty |= ed.update_poll();
    };
    disable_raw_mode()?;
    execute!(
        io::stdout(),
        DisableBracketedPaste,
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    // The update offer is announced HERE, on the way out, and that is deliberate.
    // Installing replaces the running executable, so it takes effect on the next
    // start — and a notice that had to be caught before the screen was torn down
    // would be a notice nobody reads. Printed after the terminal is restored, so
    // it lands in the shell's scrollback and survives.
    if let Some(u) = &ed.update.found
        && ed.update_installable_on_exit()
    {
        println!("{}", u.message());
    }
    result
}

#[cfg(test)]
mod args_tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    struct TempDir(PathBuf);
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn temp_dir(tag: &str) -> TempDir {
        let d = std::env::temp_dir().join(format!(
            "rano_args_{}_{}_{:?}",
            tag,
            std::process::id(),
            std::time::Instant::now()
        ));
        fs::create_dir_all(&d).unwrap();
        TempDir(d)
    }

    fn argv(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_flag_value_is_not_mistaken_for_the_file() {
        // The bug the positional `args.first()` had: `--line 42 main.rs` made
        // "42" the file.
        let a = parse_args(&argv(&["--line", "42", "main.rs"])).expect("parse");
        assert_eq!(a.file(), Some("main.rs"));
        assert_eq!(a.line, Some(42));
        assert_eq!(a.col, None);
        // And in the other order.
        let a = parse_args(&argv(&["main.rs", "--line", "42"])).expect("parse");
        assert_eq!(a.file(), Some("main.rs"));
        assert_eq!(a.line, Some(42));
    }

    #[test]
    fn the_value_forms_are_accepted() {
        let a = parse_args(&argv(&["--line=42", "--col=7", "f.rs"])).expect("equals form");
        assert_eq!((a.line, a.col, a.file()), (Some(42), Some(7), Some("f.rs")));
        let a = parse_args(&argv(&["-l", "42", "-c", "7", "f.rs"])).expect("short form");
        assert_eq!((a.line, a.col), (Some(42), Some(7)));
        let a = parse_args(&argv(&["--column", "3", "f.rs"])).expect("long column");
        assert_eq!(
            (a.line, a.col),
            (None, Some(3)),
            "a column alone leaves the row at 0"
        );
    }

    #[test]
    fn bad_arguments_are_refused_rather_than_guessed() {
        // Each of these is a mistake the user wants told about, not silently
        // reinterpreted.
        for (args, why) in [
            (argv(&["--line"]), "a flag with no value"),
            (argv(&["--line", "abc"]), "a non-number"),
            (argv(&["--line", "-1"]), "a negative line"),
            (argv(&["--nope", "f.rs"]), "an unknown option"),
            (
                argv(&["--export", "html", "a.rs", "b.rs"]),
                "two files to export",
            ),
            (argv(&["--export", "pdf", "f.rs"]), "an unknown format"),
        ] {
            assert!(parse_args(&args).is_err(), "{why}: {args:?} was accepted");
        }
    }

    #[test]
    fn help_and_version_win_over_a_file() {
        // `rano --help file.rs` prints usage rather than opening the file.
        let a = parse_args(&argv(&["--help", "file.rs"])).expect("parse");
        assert!(a.help && a.file().is_some());
        let a = parse_args(&argv(&["-V"])).expect("parse");
        assert!(a.version);
    }

    #[test]
    fn a_file_can_carry_its_own_position_on_the_command_line() {
        let a = parse_args(&argv(&[
            "no_such_a.rs:12:3",
            "no_such_b.rs:7",
            "+5",
            "c.rs",
            "+9,2",
            "d.rs",
            "e.rs",
        ]))
        .expect("parse");
        assert_eq!(
            a.files,
            vec!["no_such_a.rs", "no_such_b.rs", "c.rs", "d.rs", "e.rs"]
        );
        assert_eq!(
            a.positions,
            vec![
                Some((12, Some(3))),
                Some((7, None)),
                Some((5, None)),
                Some((9, Some(2))),
                None
            ]
        );
        // A name that exists as written is never split, colon or not.
        let d = temp_dir("colon_name");
        let odd = d.0.join("x:3");
        fs::write(&odd, "").unwrap();
        let s = odd.display().to_string();
        let a = parse_args(&argv(&[&s])).expect("parse");
        assert_eq!(a.files, vec![s]);
        assert_eq!(a.positions, vec![None]);
        // Not a position: no number, or a zero line.
        assert_eq!(split_position("a.rs:"), None);
        assert_eq!(split_position("a.rs:0"), None);
        assert_eq!(split_position(":5"), None);
        assert_eq!(
            split_position("dir:x/a.rs:4"),
            Some(("dir:x/a.rs".into(), 4, None))
        );
    }

    #[test]
    fn several_files_parse_in_order() {
        let a = parse_args(&argv(&["a.rs", "-l", "3", "b.rs", "c.rs"])).expect("parse");
        assert_eq!(a.files, vec!["a.rs", "b.rs", "c.rs"]);
        assert_eq!(a.file(), Some("a.rs"));
        assert_eq!(a.line, Some(3));
    }
}
