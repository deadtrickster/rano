//! The `rano` binary: arguments, the terminal, and the event loop. The editor
//! itself is the library's (`rano::editor`); this is one host of it.

use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

use rano::buffer::{Buffer, Pos};
use rano::editor::{Area, Editor};
use rano::render::{self, Palette, Rect};
use rano::send_ctrl::split_position;
use rano::term::{self, Event, Terminal};
use rano::{config, export, send_ctrl, syntax, ui};

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
    /// `-f`/`--follow`: open the first file at its tail, read-only (TODO.md §20).
    follow: bool,
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
            "-f" | "--follow" => out.follow = true,
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
    /// `--column`, `--follow` and `--export` act on.
    fn file(&self) -> Option<&str> {
        self.files.first().map(String::as_str)
    }
}

const USAGE: &str = "\
usage: rano [options] [file...]

  -l, --line N      put the cursor on line N (1-based) and centre it
  -c, --column N    put the cursor on column N (1-based)
  -f, --follow      open at the tail: the last few screens, read-only
                    (all three apply to the first file)
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
    // The theme the config names, and its own `color.` lines, before anything is drawn. What
    // cannot be used is said on stderr rather than dropped: a typo must not look like a
    // deliberate default.
    let themes = config::config_path()
        .parent()
        .map(|d| d.join("themes"))
        .unwrap_or_default();
    let (theme, problems) = rano::theme::resolve(&themes, cfg.theme.as_deref(), &cfg.colors);
    for p in &problems {
        eprintln!("rano: {p}");
    }
    rano::theme::set_active(theme);
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
    if let Err(e) = run(buf, load, rest, pos, args.follow, cfg) {
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
    follow: bool,
    cfg: config::Config,
) -> io::Result<()> {
    // Raw mode, the alternate screen, bracketed paste and mouse buttons (the
    // wheel scrolls, a drag selects). `Terminal::enter_with` installs the panic
    // hook that restores the terminal before the message prints, and `Drop`
    // restores it on every other way out. The cursor keeps the terminal's own
    // shape: the block cursor is letibot's choice, not the editor's.
    let terminal = Terminal::enter_with(term::Options {
        mouse: term::Mouse::Buttons,
        block_cursor: false,
    })?;
    // Until the terminal answers the background question (if it can), the dark
    // palette; the two differ only in the diff tints.
    let mut palette = Palette::Colour;
    let mut ed = Editor::new(buf, cfg);
    // **What the terminal can be asked for, told to the editor once.** A picture is drawn
    // as placeholder cells, which a terminal that cannot fill them shows as garbage, so
    // M-P on a PNG is refused — by name — where `Features::images` is off (see
    // `term::features`).
    ed.images = terminal.features().images;
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
    if let Some(path) = load {
        // `-f`/`--follow` opens at the TAIL: the last few screens read from the
        // end of the file, read-only, landing at the bottom (TODO.md §20).
        // Everything else about the open — the name, the language, the deferred
        // buffers behind it, the frame — is the same call.
        let started = if follow {
            ed.start_tail(&path)
        } else {
            ed.start_load(&path)
        };
        if let Err(e) = started {
            // The one load failure reported before a frame is drawn: there is
            // nothing on screen yet to attach a status line to.
            drop(terminal);
            eprintln!("rano: cannot read {}: {}", path.display(), e);
            std::process::exit(1);
        }
    }
    ed.add_deferred_buffers_at(&rest);
    let mut frame = render::Buffer::empty(Rect::default());
    // D6 dirty-draw: redraw only when something changed. Any handled key,
    // paste or resize dirties (coarse); `tick` reports the editor's own
    // state changes. text_w is the FULL viewport width now (draw renders
    // full-width lines); justify keeps its old wrap width via a -2 there.
    let mut dirty = true;
    loop {
        // The terminal reports no resize event; the size is asked every turn,
        // which is one ioctl, and a change re-lays the editor out.
        let (w, h) = terminal.size();
        let (w, h) = (
            w.min(u16::MAX as usize) as u16,
            h.min(u16::MAX as usize) as u16,
        );
        dirty |= ed.set_area(Area::new(0, 0, w, h));
        if frame.area() != Rect::new(0, 0, w, h) {
            frame.resize(Rect::new(0, 0, w, h));
            dirty = true;
        }
        // The pollers, the overlays' refreshes, the scroll and the frame's
        // highlight: everything between two events, the same call a host
        // embedding the editor makes.
        dirty |= ed.tick(Instant::now());
        if dirty {
            // **The pictures' own bytes first**, before the frame that draws the
            // placeholder rows naming them: the uploads, the placements and the delete of
            // a picture the previous frame was drawing. They are not part of the frame —
            // the encoder diffs rows of text — so they go out around it.
            for bytes in ed.take_graphics() {
                terminal.write_raw(&bytes);
            }
            // Only the rows that changed reach the glass; the cursor is
            // hidden while they are written and shown where the editor says,
            // or left hidden (the edit point is outside the view).
            let cursor = ui::draw(&mut frame, &ed);
            terminal.draw_buffer(
                &frame,
                palette,
                cursor.map(|(x, y)| (y as usize, x as usize)),
            );
            dirty = false;
        }
        if ed.wants_quit() {
            break;
        }
        // The idle wait is the long one, because a keystroke is what ends
        // it; `next_wakeup` shortens it while a file streams in or a
        // prefix's card is due.
        if !terminal.poll(ed.next_wakeup()) {
            continue;
        }
        for ev in terminal.events() {
            match ev {
                Event::Key(k) => {
                    ed.handle_key(k);
                    dirty = true;
                }
                Event::Paste(t) => {
                    ed.paste_text(&t);
                    dirty = true;
                }
                Event::Mouse(m) => dirty |= ed.handle_mouse(m),
                Event::Background { light } => {
                    palette = if light {
                        Palette::Light
                    } else {
                        Palette::Colour
                    };
                    dirty = true;
                }
                Event::FocusGained | Event::FocusLost => {}
            }
        }
    }
    drop(terminal);
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
    Ok(())
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
    fn follow_is_a_flag_and_not_a_file() {
        // `-f` takes no value, so the file after it is the FILE.
        for args in [&["-f", "huge.log"][..], &["--follow", "huge.log"][..]] {
            let a = parse_args(&argv(args)).expect("parse");
            assert!(a.follow, "{args:?} did not set follow");
            assert_eq!(a.file(), Some("huge.log"), "{args:?} ate the file");
        }
        // Off by default: a plain open must stay a plain open.
        assert!(!parse_args(&argv(&["huge.log"])).expect("parse").follow);
        // And in any order, like every other flag.
        let a = parse_args(&argv(&["huge.log", "-f"])).expect("parse");
        assert!(a.follow && a.file() == Some("huge.log"));
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
