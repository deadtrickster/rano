//! The `rano` binary: arguments, the terminal, and the event loop.

use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

use rano::buffer::{Buffer, Pos};
use rano::editor::{Area, Editor, Focus};
use rano::expand_tilde;
use rano::panel::Act;
use rano::render::{self, Line, Palette, Rect, Style};
use rano::send_ctrl::split_position;
use rano::style::Role;
use rano::term::{self, Event, MouseButton, MouseKind, Terminal};
use rano::{config, export, send_ctrl, syntax, ui};

/// The command line, parsed by hand: files, a few flags, no subcommands.
#[derive(Debug, Default, PartialEq, Eq)]
struct Args {
    /// In command-line order; each opens as its own buffer, the first current.
    files: Vec<String>,
    positions: Vec<Option<(usize, Option<usize>)>>,
    line: Option<usize>,
    col: Option<usize>,
    export: Option<export::Format>,
    follow: bool,
    edit: bool,
    help: bool,
    version: bool,
}

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
            "--edit" => out.edit = true,
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
    /// The first file: the one that opens current and that the flags act on.
    fn file(&self) -> Option<&str> {
        self.files.first().map(String::as_str)
    }
}

const USAGE: &str = "\
usage: rano [options] [file...]

  -l, --line N      put the cursor on line N (1-based) and centre it
  -c, --column N    put the cursor on column N (1-based)
  -f, --follow      open at the tail: the last few screens, read-only
  --edit            open a .log as an ordinary file (a .log IS one by default)
                    (all of these apply to the first file)
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
    // `--export`: read, colourise, write, exit — no terminal, so it works in a pipe.
    if let Some(fmt) = args.export {
        if let Err(e) = export_to_stdout(args.file().map(str::to_string), fmt) {
            eprintln!("rano: {e}");
            std::process::exit(1);
        }
        return;
    }
    // The file is not read here: reading before the terminal is up made a huge file
    // a dead screen. The run loop starts a loader instead.
    let file = args.file().map(str::to_string);
    let mut buf = Buffer::new();
    let mut load: Option<PathBuf> = None;
    if let Some(f) = &file {
        let p = Path::new(f);
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
    let pos = args
        .line
        .map(|l| (l, args.col))
        .or(args.positions.first().copied().flatten())
        .map(to_pos);
    let mut cfg = config::load();
    // The theme the config names, and its own `color.` lines, before anything is
    // drawn. What cannot be used is said on stderr rather than dropped.
    let themes = config::config_path()
        .parent()
        .map(|d| d.join("themes"))
        .unwrap_or_default();
    let (theme, problems) = rano::theme::resolve(&themes, cfg.theme.as_deref(), &cfg.colors);
    for p in &problems {
        eprintln!("rano: {p}");
    }
    rano::theme::set_active(theme);
    // The other files become buffers of their own, read when first visited; naming
    // several asks for several, so the session is multibuffer.
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
    if let Err(e) = run(buf, load, rest, pos, args.follow, args.edit, cfg) {
        eprintln!("rano: {}", e);
        std::process::exit(1);
    }
}

/// Read a file (or stdin), colourise it, write it out; no terminal is touched.
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
    // A name is what `detect` works from, and it supplies the fence tag and HTML title.
    let mut buf = buf;
    let title = match &path {
        Some(p) => p.clone(),
        None => String::from("<stdin>"),
    };
    buf.name = Some(std::path::PathBuf::from(&title));
    // The whole document, not a window: an export has no viewport to bound it.
    let mut hl = syntax::Highlighter::new();
    hl.refresh(&buf);
    let style_of = |p: Pos| hl.style_at(p);
    let out = export::render(&buf.rows_vec(), 8, &title, fmt, &style_of);
    // Written, not `print!`ed: a closed pipe is the normal end of `rano --export
    // ansi f.rs | head`, not a panic.
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
    edit: bool,
    cfg: config::Config,
) -> io::Result<()> {
    // Raw mode, the alternate screen, bracketed paste and mouse buttons; `Drop`
    // restores the terminal on every other way out.
    let terminal = Terminal::enter_with(term::Options {
        mouse: term::Mouse::Buttons,
        block_cursor: false,
    })?;
    // The OSC 11 answer below picks light or dark; the environment says whether the terminal draws
    // exact colours, which is where the severity table gets its orange.
    let mut palette = Palette::detected(false);
    let mut ed = Editor::new(buf, cfg);
    // Every `[keys]` refusal, on stderr where the theme's problems already go: the flash
    // names the first and counts the rest, but a record that can be read back must carry
    // them all, each with its line.
    for p in &ed.config.key_problems {
        eprintln!("rano: {p}");
    }
    // A picture is drawn as placeholder cells a terminal may fill with garbage, so
    // M-P is refused by name where the protocol is not kitty.
    ed.push_keymap(host_map());
    // The detected protocol, not a guess: inside tmux the upload must be DCS-wrapped, and only
    // `set_pictures` knows that — `images = true` alone would send it bare, as plain kitty.
    ed.set_pictures(terminal.features().proto);
    ed.tmux_passthrough_off = terminal.features().tmux_passthrough_off;
    // `--edit`: the escape from the mode a `.log` name puts it in, set before any
    // load.
    ed.edit_override = edit;
    // One request at startup, off the main thread, here rather than in
    // `Editor::new` so no test makes a network call by constructing an editor.
    ed.start_update_check();
    // The callbacks cannot reach the panel or the socket, so what a key hands them leaves through the
    // outbox and the pass after this one picks it up.
    let outbox: Outbox = std::rc::Rc::default();
    if let Some(cmd) = ed.config.send_command.clone() {
        ed.on_send = Some(send_ctrl::command_sender(cmd));
    } else {
        // No `send_command`: `M-s` attaches the place to the next prompt instead of shelling out.
        let outbox = std::rc::Rc::clone(&outbox);
        ed.on_send = Some(Box::new(move |send: &rano::send::SendEvent| {
            outbox.borrow_mut().push(Outgoing::Place(send.clone()));
            Ok("attached to the next prompt".to_string())
        }));
    }
    {
        let outbox = std::rc::Rc::clone(&outbox);
        ed.on_ask = Some(Box::new(move |ask: &rano::ask::AskEvent| {
            outbox.borrow_mut().push(Outgoing::Ask(ask.clone()));
            Ok(format!("asking: {}", ask.preset))
        }));
    }
    // `RANO_PLUGIN` beats the config file and `RANO_CAPABILITIES` the grant, so a
    // plugin can be tried without editing the config.
    if let Some(p) = std::env::var("RANO_PLUGIN")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .or_else(|| ed.config.plugin.clone())
    {
        if let Ok(caps) = std::env::var("RANO_CAPABILITIES") {
            let names: Vec<String> = caps
                .split(',')
                .map(|c| c.trim().to_string())
                .filter(|c| !c.is_empty())
                .collect();
            ed.config.capabilities = names;
        }
        ed.set_plugin(&PathBuf::from(expand_tilde(&p)));
    }
    ed.bs_mut().goto = pos;
    ed.load_builtins();
    // Where the last session per folder is remembered, beside the config: a folder reopened
    // attaches the conversation it had.
    ed.panel.state_file = config::config_path()
        .parent()
        .map(|d| d.join("panel-sessions"));
    // The viewport before the open: a plugin asked for 0 columns answers "not mine".
    let (tw, th) = terminal.size();
    ed.set_area(Area::new(
        0,
        0,
        tw.min(u16::MAX as usize) as u16,
        th.min(u16::MAX as usize) as u16,
    ));
    if let Some(path) = load {
        if !follow && path.is_dir() {
            ed.open_dir(&path);
        } else {
            // `-f` forces the tail; without it the name decides, through the one door
            // every door uses.
            let started = if follow {
                ed.start_tail(&path)
            } else {
                ed.open_named(&path)
            };
            if let Err(e) = started {
                // Reported before a frame is drawn: there is no status line yet.
                drop(terminal);
                eprintln!("rano: cannot read {}: {}", path.display(), e);
                std::process::exit(1);
            }
        }
    }
    ed.add_deferred_buffers_at(&rest);
    let mut frame = render::Buffer::empty(Rect::default());
    // The panel's connection, its columns, and whether its border is being dragged: the wire, the
    // width and the pointer are the host's, and the conversation is the editor's.
    let mut link: Option<Link> = None;
    let mut panel_width: Option<u16> = None;
    let mut dragging = false;
    // Dirty-draw: any handled key, paste or resize dirties; `tick` reports the
    // editor's own state changes.
    let mut dirty = true;
    loop {
        let (w, h) = terminal.size();
        let (w, h) = (
            w.min(u16::MAX as usize) as u16,
            h.min(u16::MAX as usize) as u16,
        );
        if frame.area() != Rect::new(0, 0, w, h) {
            frame.resize(Rect::new(0, 0, w, h));
            dirty = true;
        }
        // What the panel and the wire said to each other since the last pass, before the layout:
        // a frame may have changed the session, and an act may have opened or closed a socket.
        dirty |= pump(&mut ed, &mut link, &outbox);
        let lay = layout(w, h, &ed, panel_width);
        sync_bottom_size(&mut ed, &lay);
        dirty |= ed.set_area(lay.editor.unwrap_or(Rect::new(0, 0, w, h)).into());
        // The strip covers text rows: the editor must not scroll the cursor under it.
        let text_rows = lay.editor.unwrap_or(Rect::new(0, 0, w, h)).height as usize;
        let covered = lay.bottom.map_or(0, |r| r.height as usize);
        ed.text_h = text_rows
            .saturating_sub(ed.chrome_rows())
            .saturating_sub(covered)
            .max(1);
        // The pollers, the overlays' refreshes, the scroll and the highlight:
        // everything between two events, the same call a host makes.
        dirty |= ed.tick(Instant::now());
        // The clipboard is not the screen: drained on every pass, so a copy does not
        // wait for the next keystroke.
        if let Some(text) = ed.take_clipboard() {
            terminal.copy(&text);
        }
        if dirty {
            // The pictures' bytes before the frame that draws their placeholder
            // rows: not part of the text frame, so they go out around it.
            for bytes in ed.take_graphics() {
                terminal.write_raw(&bytes);
            }
            // Only the rows that changed reach the glass; the cursor is hidden while
            // they are written and shown where the focused surface says.
            let cursor = draw_frame(&mut frame, &mut ed, &lay);
            terminal.draw_buffer(
                &frame,
                palette,
                cursor.map(|(x, y)| (y as usize, x as usize)),
            );
            dirty = false;
            // The read mark goes out after the rows are on the glass, never on receipt: a crash
            // costs a duplicate rather than a silence.
            if let Some(open) = link.as_mut()
                && open.ack_due
            {
                open.ack_due = false;
                let _ = open.client.ack(ed.panel.rows.len() as u64, 0);
            }
        }
        if ed.wants_quit() {
            break;
        }
        // The idle wait is the long one: a keystroke ends it; `next_wakeup` shortens
        // it while a file streams in or a prefix's card is due.
        if !terminal.poll(ed.next_wakeup()) {
            continue;
        }
        for ev in terminal.events() {
            match ev {
                Event::Key(k) => {
                    // The terminal's keys come first: a focused terminal's typing must never
                    // edit the file behind it. Everything else is the editor's own maps.
                    let _ = rano::host::key(&mut ed, k);
                    dirty = true;
                }
                Event::Paste(t) => {
                    ed.paste_text(&t);
                    dirty = true;
                }
                Event::Mouse(m) => {
                    dirty |= mouse(&mut ed, &lay, &m, w, &mut panel_width, &mut dragging)
                }
                Event::Background { light } => {
                    palette = Palette::detected(light);
                    dirty = true;
                }
                Event::FocusGained | Event::FocusLost => {}
            }
        }
    }
    drop(terminal);
    // The update offer is announced on the way out: installing replaces the running
    // executable, and printing after the terminal is restored lands in scrollback.
    if let Some(u) = &ed.update.found
        && ed.update_installable_on_exit()
    {
        println!("{}", u.message());
    }
    Ok(())
}

/// The narrowest the panel is worth drawing beside the text.
const PANEL_MIN: u16 = 36;
/// The columns the text keeps; below the two minimums and a border, one surface takes the frame.
const EDITOR_MIN: u16 = 60;
/// The widest the panel opens at: more than half a wide terminal is a second editor, not a sidebar.
const PANEL_MAX: u16 = 80;

/// The panel's connection to a letibot daemon: the socket and the seat. The conversation is the
/// editor's, the wire is the host's, and this is where the two meet.
struct Link {
    client: rano::letibot::Client,
    /// Whether this connection has been through its first `Attach`: a move after that is a
    /// `Switch`.
    attached: bool,
    /// A frame arrived and has not been drawn yet, so the read mark is still owed.
    ack_due: bool,
}

impl Link {
    /// Ask the launcher where the folder's daemon is, and connect to it. A folder with no daemon is
    /// an answer rather than a socket nothing is listening on.
    fn open(folder: &Path) -> Result<Link, rano::letibot::NoDaemon> {
        let socket = rano::letibot::socket(folder)?;
        match rano::letibot::Client::connect(&socket) {
            Ok(client) => Ok(Link {
                client,
                attached: false,
                ack_due: false,
            }),
            Err(e) => Err(rano::letibot::NoDaemon::NotRunning {
                folder: folder.to_path_buf(),
                said: e.to_string(),
            }),
        }
    }

    /// A frame arrived and has not been drawn yet: the read mark is still owed.
    fn sent(&mut self) {
        self.ack_due = true;
    }
}

/// The seat the panel attaches as. `can_decide: false`: the panel draws no card, so claiming it
/// could answer one would have the daemon waiting on a head that never will.
fn seat() -> rano::letibot::Seat {
    rano::letibot::Seat {
        can_decide: false,
        ..Default::default()
    }
}

/// What the panel and the wire say to each other, once per pass: frames in, acts out. True when
/// the screen changed.
fn pump(ed: &mut Editor, link: &mut Option<Link>, outbox: &Outbox) -> bool {
    let mut dirty = false;
    // What a key handed `on_ask` or `on_send`: the panel asks, and a place becomes a chip.
    for out in std::mem::take(&mut *outbox.borrow_mut()) {
        dirty = true;
        match out {
            Outgoing::Ask(ask) => {
                ed.panel_ask(&ask);
            }
            Outgoing::Place(send) => {
                ed.panel_attach(&send);
            }
        }
    }
    // What the operator decided about earlier marks rides the next prompt, named as the model wrote
    // the range rather than by an id it has never seen.
    let taken = ed.take_mark_outcomes();
    if !taken.is_empty() {
        ed.panel.remember_outcomes(&taken);
    }
    // A reply's citations and a call's edits, on the buffers this host has open. A file that is not
    // open is kept, not dropped: it lands on the pass that opens it.
    let mut unlanded = Vec::new();
    for set in ed.panel.take_marks() {
        dirty = true;
        for source in &set.replaces {
            ed.clear_marks(Some(source));
        }
        if ed.holds_file(&set.path) {
            ed.set_marks(&set.path, &set.source, set.marks);
        } else {
            unlanded.push(set);
        }
    }
    ed.panel.keep_marks(unlanded);
    if let Some(open) = link.as_mut() {
        while let Some(frame) = open.client.try_frame() {
            if matches!(frame, rano::letibot::Frame::Hello(_)) {
                open.attached = true;
            }
            open.sent();
            dirty |= ed.panel.apply(&frame);
        }
    }
    for act in ed.panel.take_acts() {
        dirty = true;
        match act {
            Act::Connect(folder) => dirty |= connect(ed, link, &folder),
            Act::Start(folder) => match rano::letibot::start(&folder) {
                Ok(_) => dirty |= connect(ed, link, &folder),
                Err(why) => ed.panel.no_daemon(&why),
            },
            Act::Attach(session) => {
                let Some(open) = link.as_mut() else {
                    ed.panel.no_daemon(&rano::letibot::NoDaemon::NotRunning {
                        folder: ed.panel.folder.clone(),
                        said: String::new(),
                    });
                    continue;
                };
                let outcome = if open.attached {
                    open.client.switch(&session, 0)
                } else {
                    open.client.attach(&session, 0, &seat())
                };
                open.sent();
                if let Err(e) = outcome {
                    ed.panel.said = format!("cannot attach to {session}: {e}");
                }
            }
            Act::New => {
                send_frame(ed, link, |open, folder| {
                    open.client.new_session("", &folder)
                });
            }
            Act::Prompt { text, context } => {
                let context = with_open_buffers(ed, context);
                send_frame(ed, link, |open, _| {
                    open.client
                        .prompt_with_context(&text, &[], context.as_ref())
                });
            }
            Act::Interrupt => {
                send_frame(ed, link, |open, _| open.client.interrupt("esc esc"));
            }
        }
    }
    dirty
}

/// The context a prompt carries, with the editor's other open files added: the one fact only the
/// host has, and the reason the panel asks for it at send time rather than at ask time.
fn with_open_buffers(
    ed: &Editor,
    context: Option<rano::letibot::EditorContext>,
) -> Option<rano::letibot::EditorContext> {
    let open = open_buffers(ed);
    match context {
        Some(mut context) => {
            context.open = open;
            Some(context)
        }
        // Nothing pointed at, nothing decided: the open files alone are worth one line, and it is
        // still a context the daemon renders.
        None if !open.is_empty() => Some(rano::letibot::EditorContext {
            origin: "rano".to_string(),
            open,
            ..rano::letibot::EditorContext::default()
        }),
        None => None,
    }
}

/// Every file the editor holds open, names only: a view (the manual, a listing) is not a file the
/// daemon can read, so it is not one the model is told about.
fn open_buffers(ed: &Editor) -> Vec<rano::letibot::OpenBuffer> {
    ed.buffers
        .iter()
        .enumerate()
        .filter(|(_, bs)| !bs.synthetic)
        .filter_map(|(i, bs)| {
            let name = bs.buf.name.as_ref()?;
            Some(rano::letibot::OpenBuffer {
                file: name.display().to_string(),
                unsaved: bs.buf.modified,
                visible: i == ed.cur,
            })
        })
        .collect()
}

/// What the editor hands a host callback that this loop has not acted on yet: a callback holds no
/// socket and no panel, so it leaves the fact here.
type Outbox = std::rc::Rc<std::cell::RefCell<Vec<Outgoing>>>;

#[derive(Debug)]
enum Outgoing {
    /// `M-k <key>`: a quick ask, which the panel sends with the place beside it.
    Ask(rano::ask::AskEvent),
    /// `M-s`: a place to attach to the next prompt.
    Place(rano::send::SendEvent),
}

/// A frame a command sends, with the two failures every one of them has: no daemon to send it to,
/// and a socket that has gone away.
fn send_frame(
    ed: &mut Editor,
    link: &mut Option<Link>,
    send: impl FnOnce(&mut Link, String) -> std::io::Result<String>,
) {
    let folder = ed.panel.folder.display().to_string();
    let Some(open) = link.as_mut() else {
        ed.panel.no_daemon(&rano::letibot::NoDaemon::NotRunning {
            folder: ed.panel.folder.clone(),
            said: String::new(),
        });
        return;
    };
    if let Err(e) = send(open, folder) {
        ed.panel.said = format!("cannot reach the daemon: {e}");
    }
}

/// Open a connection for `folder` and ask it for its sessions.
fn connect(ed: &mut Editor, link: &mut Option<Link>, folder: &Path) -> bool {
    if let Some(old) = link.take() {
        drop(old);
    }
    match Link::open(folder) {
        Ok(mut open) => {
            // The daemon's first frame has to be ATTACH: a bare attach lands on its current session
            // (or on a control seat that carries the list, when it holds none), and only then is
            // asking for the folder's sessions legal. Listing first is refused with "the first
            // frame must be ATTACH" and the connection dropped — which is what a real daemon did
            // to this path while the scripted one in the tests accepted it.
            if let Err(e) = open.client.attach("", 0, &seat()) {
                ed.panel.said = format!("cannot attach to the daemon: {e}");
            }
            if let Err(e) = open.client.list_sessions() {
                ed.panel.said = format!("cannot ask for the sessions: {e}");
            }
            *link = Some(open);
        }
        // A folder with no daemon of its own is a normal thing to be standing in: the operator's
        // daemon usually belongs to the project ABOVE the file's folder. Walk up until one answers,
        // and only the root's refusal is the offline state — that is the case where Enter starts
        // one for this folder instead of borrowing one.
        Err(rano::letibot::NoDaemon::NotRunning { .. }) => {
            if let Some(parent) = folder.parent() {
                ed.panel.said = format!("no daemon here; asking {}", parent.display());
                return connect(ed, link, parent);
            }
            ed.panel.no_daemon(&rano::letibot::NoDaemon::NotRunning {
                folder: folder.to_path_buf(),
                said: "no daemon for this folder or any above it".to_string(),
            });
        }
        Err(why @ rano::letibot::NoDaemon::NotInstalled) => ed.panel_refuse(&why.to_string()),
        Err(why) => ed.panel.no_daemon(&why),
    }
    true
}

/// The panes of one frame. A surface that is not drawn has no rectangle: below the width limit, and
/// while zoomed, exactly one of them has the frame.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Layout {
    editor: Option<Rect>,
    border: Option<Rect>,
    panel: Option<Rect>,
    /// The bottom terminal's strip, when it is open: the rows it takes off the frame's foot.
    bottom: Option<Rect>,
    /// The status row and the key bar, full width, when the host draws them itself: the frame
    /// is shared with the panel, so the bar belongs to the frame and not to the text column.
    foot: Option<Rect>,
}

/// How a `w`×`h` frame splits between the text and the panel. `want` is the width the operator
/// dragged the border to, clamped every frame so a resize cannot leave a surface twice as small as
/// its minimum.
fn layout(w: u16, h: u16, ed: &Editor, want: Option<u16>) -> Layout {
    // The status row and the key bar are the frame's foot. With the panel open the host draws
    // them full width, the text stops above the strip and the panel runs down to the foot;
    // without the panel the editor owns the whole frame and draws its own, with the strip
    // painted over the rows above them and spanning the frame.
    let status = ed.status_rows() as u16;
    let strip = if ed.bottom.open && h > 6 {
        ed.bottom.height.min((h / 3) as usize) as u16
    } else {
        0
    };
    let strip_y = h.saturating_sub(status).saturating_sub(strip);
    let bottom = (strip > 0).then(|| Rect::new(0, strip_y, w, strip));
    let host_foot = ed.panel.open && status > 0;
    let foot = host_foot.then(|| Rect::new(0, h - status, w, status));
    let above_h = if foot.is_some() { strip_y } else { h };
    let whole = Rect::new(0, 0, w, above_h);
    if !ed.panel.open || w == 0 || h == 0 {
        return Layout {
            editor: Some(whole),
            bottom,
            foot,
            ..Layout::default()
        };
    }
    if ed.panel.zoom || w < EDITOR_MIN + 1 + PANEL_MIN {
        // One surface takes the frame: two columns that fit neither is the squeeze the design
        // refuses.
        return if ed.focus == Focus::Panel {
            // A narrow or zoomed frame still draws the strip: its rows are reserved, so the
            // terminal is drawn on them rather than leaving a blank band.
            Layout {
                panel: Some(whole),
                bottom,
                foot,
                ..Layout::default()
            }
        } else {
            Layout {
                editor: Some(whole),
                bottom,
                foot,
                ..Layout::default()
            }
        };
    }
    let default = (u32::from(w) * 2 / 5).min(u32::from(PANEL_MAX)) as u16;
    let pw = want.unwrap_or(default).clamp(PANEL_MIN, w - EDITOR_MIN - 1);
    let ew = w - pw - 1;
    // The panel is a full-height column: its side runs down to the foot, past the strip's
    // rows, while the strip sits under the text column only. The composer and the shell's
    // last drawn row therefore end on the same row, and the split rule is drawn through the
    // strip to the foot rather than stopping at it.
    let side_h = foot.map(|f| f.y).unwrap_or(whole.height);
    Layout {
        editor: Some(Rect::new(0, whole.y, ew, whole.height)),
        border: Some(Rect::new(ew, whole.y, 1, side_h)),
        panel: Some(Rect::new(ew + 1, whole.y, pw, side_h)),
        bottom: (strip > 0).then(|| Rect::new(0, strip_y, ew, strip)),
        foot,
    }
}

/// The pty is the strip's rectangle — the text column's width while the panel is open, the
/// frame's while it is not — and only the rows that are drawn: the rule and the tab strip are
/// not the shell's. Called every frame, so a frame resize, a dragged rule and M-i all reach the
/// shell as TIOCSWINSZ.
fn sync_bottom_size(ed: &mut Editor, lay: &Layout) {
    if let Some(area) = lay.bottom {
        let rows = (area.height as usize).saturating_sub(2).max(1) as u16;
        // One column narrower: the strip's last column is the scrollbar, and no shell output
        // may sit under it.
        let cols = area.width.saturating_sub(1).max(1);
        for tab in &mut ed.bottom.tabs {
            tab.resize(cols, rows);
        }
    }
}

/// Draw the frame the layout asked for, and say where the caret is.
fn draw_frame(buf: &mut render::Buffer, ed: &mut Editor, lay: &Layout) -> Option<(u16, u16)> {
    let mut cursor = None;
    // A shared frame: the status row and the key bar are the frame's, drawn full width after
    // the surfaces, and the editor does not paint its own inside its column.
    ed.foot_drawn_by_host = lay.foot.is_some();
    // The split rule carries the editor's scrollbar, so the editor's own draw leaves it out.
    ed.scrollbar_in_border = lay.border.is_some();
    if let Some(area) = lay.editor {
        cursor = ui::draw_in(buf, area, ed);
    }
    if let Some(area) = lay.border {
        // The rule between the two, the same glyph the editor's own sidebar draws — and it
        // carries the editor's scrollbar when there is one, so a bar never stands beside a
        // rule as a second rule. The thumb is the accent role on the same glyph, and it names
        // the view the text shows: a preview's place in its rendered lines when one is open,
        // the buffer's otherwise — one sum either way. The text's focus is said here too:
        // its rule carries the accent, track and thumb alike.
        let er = lay.editor.unwrap_or(area);
        let t0 = ed.chrome.top();
        let rows = (er.height as usize).saturating_sub(usize::from(t0));
        let span = match &ed.diff_view {
            Some(v) => ui::thumb_span(rano::diffview::body_rows(rows), v.top, v.lines.len()),
            None => ui::thumb_span(rows, ed.scroll_pos(), ed.scroll_total(rows)),
        };
        // The text's focus is said here, without eating the bar: focused, the track carries
        // the accent and the thumb is the accent REVERSED, so the rule is the text's marker
        // and the thumb still reads against it; unfocused, the track is faint and the thumb
        // is Strong — the scroll cue stays, and no accent sits on another surface's glass.
        let text_focus = ed.focus == Focus::Text;
        let track = Line::styled(
            "\u{2502}",
            if text_focus {
                Style::of(Role::UserAccent)
            } else {
                Style::of(Role::Faint)
            },
        );
        let thumb = if text_focus {
            Line::styled("\u{2502}", Style::of(Role::UserAccent).reverse())
        } else {
            Line::styled("\u{2502}", Style::of(Role::Strong))
        };
        for (j, y) in (area.y..area.y + area.height).enumerate() {
            let is_thumb = span
                .is_some_and(|(at, t)| j >= usize::from(t0) + at && j < usize::from(t0) + at + t);
            buf.set_line(area.x, y, if is_thumb { &thumb } else { &track }, 1);
        }
    }
    if let Some(area) = lay.panel {
        ed.panel.build(area.width as usize);
        // The frame's key bar already shows the focused surface's keys when the editor is drawn;
        // the panel repeats them only when it is the whole frame and there is no bar to say them.
        let hints = if ed.focus == Focus::Panel && lay.editor.is_none() {
            ed.key_hints()
        } else {
            Vec::new()
        };
        let chips: Vec<String> = ed
            .panel
            .chips
            .iter()
            .map(|c| c.label(&ed.panel.folder))
            .collect();
        let drawn = panel_view(ed, &hints, &chips).draw(area, buf);
        ed.panel.scroll = drawn.scroll;
        if drawn.cursor.is_some() {
            cursor = drawn.cursor;
        }
    }
    if let Some(area) = lay.bottom {
        let cwd = std::env::current_dir().unwrap_or_default();
        let content_w = (area.width as usize).saturating_sub(1);
        let lines = ed
            .bottom
            .lines_for(content_w, &cwd, ed.focus == Focus::Terminal);
        // A rule above the strip: the terminal is a pane, not more of the file's text — and
        // the terminal's focus is said here, the rule carrying the accent while the shell
        // has the keyboard.
        let term_rule = if ed.focus == Focus::Terminal {
            Style::of(Role::UserAccent)
        } else {
            Style::of(Role::Faint)
        };
        let rule = Line::styled("─".repeat(area.width as usize), term_rule.clone());
        buf.set_line(area.x, area.y, &rule, area.width as usize);
        // Where the strip's rule meets the split rule the two join: one drawn corner, not two
        // lines that happen to touch.
        if let Some(b) = lay.border {
            buf.set_line(b.x, area.y, &Line::styled("\u{2524}", term_rule), 1);
        }
        for (i, l) in lines
            .iter()
            .take((area.height as usize).saturating_sub(1))
            .enumerate()
        {
            buf.set_line(area.x, area.y + 1 + i as u16, l, content_w);
        }
        // The scrollbar, the same drawing the text pane's is: a track with a thumb, always
        // drawn, over the rows the terminal actually has.
        // The one real cursor: in the strip when the strip has the keyboard, where the shell's
        // own caret is — and only when it is on the glass and the program did not hide it.
        // A prompt is drawn over every surface and owns the cursor while it is open.
        if ed.focus == Focus::Terminal
            && ed.prompt.is_none()
            && let Some(tab) = ed.bottom.tabs.get(ed.bottom.active)
            && let Some((crow, ccol)) = tab.cursor_pos()
        {
            // `cursor_position()` answers (row, column): the frame takes (x, y), so the row is
            // the y and the column the x — used the other way round, the caret walked the strip
            // sideways and landed on the key bar with a two-line prompt.
            cursor = Some((area.x + ccol, area.y + 2 + crow));
        }
        if let Some(tab) = ed.bottom.tabs.get(ed.bottom.active) {
            let h = (area.height as usize).saturating_sub(2);
            let history = tab.history_len();
            let total = history + tab.screen_rows();
            // `pos` counts from the top of the history: offset 0 is its foot, which is the
            // thumb at the bottom of the track.
            let pos = history.saturating_sub(tab.scroll_pos());
            crate::ui::bar_always(area, buf, 2, h, pos, total, ed.focus == Focus::Terminal);
        }
    }
    if let Some(area) = lay.foot {
        // The foot, full width: the same rows `draw_in` paints when the editor owns the frame.
        if cursor.is_none() {
            cursor = ui::draw_foot(buf, area, ed);
        } else {
            ui::draw_foot(buf, area, ed);
        }
    }
    cursor
}
fn panel_view<'a>(
    ed: &'a Editor,
    hints: &'a [(String, String)],
    chips: &'a [String],
) -> rano::agent::panel::View<'a> {
    let panel = &ed.panel;
    rano::agent::panel::View {
        title: panel.title.clone(),
        state: panel.state.words(),
        state_role: panel.state.role(),
        folder: panel.folder.display().to_string(),
        moved: panel.moved.as_ref().map(|f| f.display().to_string()),
        body: panel.lines(),
        scroll: panel.scroll,
        follow: panel.follow,
        input: &panel.input,
        cursor: panel.cursor,
        chips,
        focus: ed.focus == Focus::Panel,
        hints,
    }
}

/// A mouse event in the frame: a click in the panel takes the keyboard there, a held border resizes
/// it, and anything else is the editor's.
/// The host's own keys, in the host's map: the strip's toggle and the panel's three. An
/// embedder that pushes no such map never gets them, and never forks a shell it cannot draw.
fn host_map() -> rano::keymap::Keymap {
    let mut host = rano::keymap::Keymap::new("host");
    host.bind("M-0", "toggle-terminal")
        .bind("M-i", "toggle-panel")
        .bind("C-]", "cross-panel")
        .bind("M-m", "zoom-panel");
    host
}

fn mouse(
    ed: &mut Editor,
    lay: &Layout,
    m: &rano::term::MouseEvent,
    w: u16,
    width: &mut Option<u16>,
    dragging: &mut bool,
) -> bool {
    let inside = |r: Option<Rect>| r.is_some_and(|r| r.contains(m.x, m.y));
    // A held thumb lets go wherever the release lands — over the text or the panel as
    // readily as over the rule — or the next press on the rule would scroll instead of
    // resizing.
    if ed.scroll_grab && matches!(m.kind, MouseKind::Release(_)) {
        ed.scroll_grab = false;
        return true;
    }
    // A held border is the width, whatever the pointer is over now.
    if *dragging {
        match m.kind {
            MouseKind::Drag(MouseButton::Left) => {
                *width = Some(w.saturating_sub(m.x + 1));
                return true;
            }
            MouseKind::Release(_) => {
                *dragging = false;
                return true;
            }
            _ => {}
        }
    }
    if inside(lay.border) {
        // The rule is the text's scrollbar when the text has one: a press on the THUMB — the
        // drawn affordance — grabs the view, drags in the rule follow the pointer's row until
        // the release, and a press on the rule elsewhere drags the width. Where the drawing
        // is, the hit-testing is.
        if matches!(
            m.kind,
            MouseKind::Press(MouseButton::Left) | MouseKind::Drag(MouseButton::Left)
        ) {
            let rows_of = |ed: &Editor, er: Rect| {
                (er.height as usize).saturating_sub(usize::from(ed.chrome.top()))
            };
            let on_thumb = |ed: &Editor, er: Rect| -> bool {
                if ed.diff_view.is_some() {
                    return false;
                }
                let rows = rows_of(ed, er);
                match ui::thumb_span(rows, ed.scroll_pos(), ed.scroll_total(rows)) {
                    Some((at, thumb)) => {
                        (m.y as usize) >= usize::from(ed.chrome.top()) + at
                            && (m.y as usize) < usize::from(ed.chrome.top()) + at + thumb
                    }
                    None => false,
                }
            };
            let grab_now = ed.scroll_grab
                || lay.editor.is_some_and(|er| {
                    on_thumb(ed, er) && m.kind == MouseKind::Press(MouseButton::Left)
                });
            if grab_now && let Some(er) = lay.editor {
                ed.scroll_grab = true;
                let body = rows_of(ed, er);
                // The last track row is the view's end: the whole rule is the travel. The
                // title row sits above the track, so the subtraction saturates.
                let into = (m.y.saturating_sub(ed.chrome.top())) as usize;
                let frac = (into as f64 / (body as f64 - 1.0).max(1.0)).clamp(0.0, 1.0);
                let max = ed.max_scroll(body);
                let scroll = (frac * max as f64).round() as usize;
                ed.scroll_to(scroll);
                return true;
            }
            if matches!(m.kind, MouseKind::Press(MouseButton::Left)) {
                *dragging = true;
                return true;
            }
            return false;
        }
        return false;
    }
    if inside(lay.panel) {
        // A prompt keeps the keyboard wherever the pointer goes: it is a question being answered,
        // and taking the caret out from under it would answer it by accident.
        let taking = ed.prompt.is_none();
        match m.kind {
            MouseKind::Press(MouseButton::Left) => {
                if !taking {
                    return false;
                }
                ed.focus = Focus::Panel;
                ed.panel.cursor = ed.panel.input.chars().count();
                return true;
            }
            MouseKind::WheelUp => {
                ed.panel_page_up();
                return true;
            }
            MouseKind::WheelDown => {
                ed.panel_page_down();
                return true;
            }
            _ => return false,
        }
    }
    // The terminal is a pane with a rectangle: the wheel over it scrolls its scrollback, and the
    // document above is not involved. Everywhere else is the editor's own mouse handling.
    if let Some(strip) = lay.bottom
        && (ed.bottom.dragging || inside(lay.bottom))
    {
        return ed.bottom_mouse(*m, strip.y, strip.height);
    }
    ed.focus = Focus::Text;
    ed.handle_mouse(*m)
}

#[cfg(test)]
mod panel_tests {
    use super::*;
    use rano::buffer::Buffer;
    use rano::config::Config;
    use rano::term::KeyCode;
    use rano::term::{Mods, MouseButton, MouseEvent};

    fn editor() -> Editor {
        let mut ed = Editor::new(Buffer::new(), Config::default());
        ed.push_keymap(host_map());
        ed
    }

    /// Feed the shell until one of its screen lines answers `pred`, or give up.
    fn feed_until(ed: &mut Editor, pred: impl Fn(&str) -> bool) -> bool {
        for _ in 0..3000 {
            let tab = &mut ed.bottom.tabs[ed.bottom.active];
            tab.feed();
            if tab.lines().iter().any(|l| pred(&l.plain())) {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        false
    }

    /// The pty is the strip's rectangle — full width, drawn rows only — and after `seq 1 100`
    /// the prompt is on the last drawn row, not cut off below it. A frame resize resizes the pty.
    #[test]
    fn the_pty_is_the_strips_rectangle_and_the_prompt_stays_visible() {
        use std::path::Path;
        let mut ed = editor();
        ed.bottom.open = true;
        ed.bottom.height = 12;
        ed.bottom
            .new_tab(Path::new("/tmp"), 40, 6, 200)
            .expect("a shell");
        let lay = layout(100, 40, &ed, None);
        sync_bottom_size(&mut ed, &lay);
        let strip = lay.bottom.expect("a strip");
        let rows = strip.height - 2;
        ed.bottom.tabs[0].send(b"PS1='PROMPT> '\n");
        ed.bottom.tabs[0].send(b"stty size\n");
        // The pty is one column narrower than the strip: the last column is the scrollbar.
        let want = format!("{rows} {}", strip.width - 1);
        assert!(
            feed_until(&mut ed, |l| l.trim().ends_with(&want)),
            "stty size is the drawn rectangle: {want}"
        );
        ed.bottom.tabs[0].send(b"seq 1 100\n");
        // Exactly the answer line: the echoed `seq 1 100` also contains 100.
        assert!(feed_until(&mut ed, |l| l.trim() == "100"), "seq answered");
        let mut visible = false;
        for _ in 0..3000 {
            ed.bottom.tabs[0].feed();
            if ed.bottom.tabs[0]
                .lines()
                .last()
                .is_some_and(|l| l.plain().contains("PROMPT>"))
            {
                visible = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(visible, "the prompt is on the last drawn row");
        let lines = ed.bottom.lines(strip.width as usize, Path::new("/tmp"));
        assert_eq!(
            lines.len(),
            (strip.height - 1) as usize,
            "every drawn row is a terminal row"
        );
        assert!(
            lines
                .last()
                .expect("a last row")
                .plain()
                .contains("PROMPT>"),
            "the prompt is visible after seq 1 100"
        );
        // A smaller frame is a smaller terminal, by the same rule.
        let lay2 = layout(80, 30, &ed, None);
        sync_bottom_size(&mut ed, &lay2);
        let strip2 = lay2.bottom.expect("a strip");
        let want2 = format!("{} {}", strip2.height - 2, strip2.width - 1);
        ed.bottom.tabs[0].send(b"stty size\n");
        assert!(
            feed_until(&mut ed, |l| l.trim().ends_with(&want2)),
            "after the frame resize, stty size is {want2}"
        );
    }

    /// The panel is a full-height column and the strip runs under the text only: the composer
    /// and the shell's last drawn row end on the same row above the foot, the split rule is
    /// drawn through the strip's rows with a junction, no shell cell sits right of the split,
    /// and the pty is the text column — proved against a real pty, before and after M-i.
    #[test]
    fn the_panel_is_a_full_height_column_and_the_strip_runs_under_the_text() {
        use std::path::Path;
        let mut ed = editor();
        ed.panel.open = true;
        ed.panel.input = "composer-words".to_string();
        ed.bottom.open = true;
        ed.bottom.height = 12;
        ed.focus = Focus::Terminal;
        ed.bottom
            .new_tab(Path::new("/tmp"), 40, 6, 200)
            .expect("a shell");
        for (w, h) in [(239u16, 68u16), (100, 30)] {
            let lay = layout(w, h, &ed, None);
            sync_bottom_size(&mut ed, &lay);
            let strip = lay.bottom.expect("a strip");
            let text = lay.editor.expect("the text column");
            let panel = lay.panel.expect("the panel column");
            let split = lay.border.expect("the split rule");
            let foot_y = h - ed.status_rows() as u16;
            // The geometry: the strip is the text column's width, the panel runs to the foot.
            assert_eq!(
                strip.width, text.width,
                "the strip is under the text only at {w}x{h}"
            );
            assert_eq!(split.x, text.width, "the rule is at the split");
            assert_eq!(
                panel.y + panel.height,
                foot_y,
                "the panel runs to the foot at {w}x{h}"
            );
            assert_eq!(
                text.y + text.height,
                strip.y,
                "the text ends where the strip begins at {w}x{h}"
            );
            // The shell must be told the rectangle it was given: stty says the drawn columns.
            ed.bottom.tabs[0].send(b"PS1='PROMPT> '\n");
            ed.bottom.tabs[0].send(b"stty size\n");
            let want = format!("{} {}", strip.height - 2, strip.width - 1);
            assert!(
                feed_until(&mut ed, |l| l.trim().ends_with(&want)),
                "stty is the text column at {w}x{h}: {want}"
            );
            // A run of Zs wider than the pty fills every column the shell has; the split, the
            // scrollbar and the panel's columns must all stay free of them.
            ed.bottom.tabs[0].send(b"head -c 300 /dev/zero | tr '\\0' 'Z'\n");
            let full_line = (strip.width - 1) as usize;
            assert!(
                feed_until(&mut ed, |l| l.chars().filter(|c| *c == 'Z').count()
                    >= full_line),
                "the Zs are on the glass at {w}x{h}"
            );
            let mut buf = rano::render::Buffer::empty(Rect::new(0, 0, w, h));
            draw_frame(&mut buf, &mut ed, &lay);
            let rows = buf.emit(rano::render::Palette::None);
            let split_at = split.x as usize;
            assert_eq!(
                rows[strip.y as usize].chars().nth(split_at),
                Some('\u{2524}'),
                "the strip's rule joins the split rule at {w}x{h}"
            );
            for (y, row) in rows
                .iter()
                .enumerate()
                .take(foot_y as usize)
                .skip((strip.y + 1) as usize)
            {
                let cell = row.chars().nth(split_at);
                assert_eq!(
                    cell,
                    Some('\u{2502}'),
                    "the split runs through the strip's row {y} at {w}x{h}"
                );
                let right = row.chars().skip(split_at).collect::<String>();
                assert!(
                    !right.contains('Z'),
                    "no shell cell right of the split at {w}x{h} row {y}: {:?}",
                    &right[..right.len().min(30)]
                );
            }
            // The composer and the shell's last row end together, above the foot.
            let composer_row = rows
                .iter()
                .position(|r| r.contains("composer-words"))
                .unwrap_or_else(|| panic!("the composer is drawn at {w}x{h}"));
            assert_eq!(
                composer_row as u16,
                strip.y + strip.height - 1,
                "the composer is on the strip's last row at {w}x{h}"
            );
            let at = rows[composer_row]
                .find("composer-words")
                .expect("in its row");
            assert!(
                at > split_at,
                "the composer is in the panel's columns at {w}x{h}"
            );
            // M-i: the panel away, the strip spans the frame, and the shell is told again.
            ed.panel.open = false;
            let lay = layout(w, h, &ed, None);
            sync_bottom_size(&mut ed, &lay);
            let wide = lay.bottom.expect("a strip without the panel");
            assert_eq!(wide.width, w, "M-i: the strip spans the frame at {w}x{h}");
            ed.bottom.tabs[0].send(b"clear\n");
            assert!(feed_until(&mut ed, |l| l.contains("PROMPT> ")));
            ed.bottom.tabs[0].send(b"stty size\n");
            let want_wide = format!("{} {}", wide.height - 2, wide.width - 1);
            assert!(
                feed_until(&mut ed, |l| l.trim().ends_with(&want_wide)),
                "after M-i the pty is the frame at {w}x{h}: {want_wide}"
            );
            // M-i again: back to the text column.
            ed.panel.open = true;
            let lay = layout(w, h, &ed, None);
            sync_bottom_size(&mut ed, &lay);
            ed.bottom.tabs[0].send(b"clear\n");
            assert!(feed_until(&mut ed, |l| l.contains("PROMPT> ")));
            ed.bottom.tabs[0].send(b"stty size\n");
            assert!(
                feed_until(&mut ed, |l| l.trim().ends_with(&want)),
                "M-i again: the pty is the text column at {w}x{h}: {want}"
            );
        }
    }

    /// The strip is a mouse pane with a rectangle: the wheel over it is the terminal's, and
    /// dragging its rule resizes it.
    #[test]
    fn the_strip_is_a_mouse_pane() {
        let mut ed = editor();
        ed.bottom.open = true;
        ed.bottom.height = 12;
        let lay = layout(120, 30, &ed, None);
        let strip = lay.bottom.expect("a strip");
        assert_eq!(
            strip.y + strip.height,
            30 - ed.status_rows() as u16,
            "the strip ends above the status and the key bar"
        );
        let mut width = None;
        let mut dragging = false;
        let wheel = MouseEvent {
            kind: MouseKind::WheelUp,
            x: 10,
            y: strip.y + 2,
            mods: Mods::NONE,
        };
        assert!(
            mouse(&mut ed, &lay, &wheel, 120, &mut width, &mut dragging),
            "the strip took the wheel"
        );
        // Taking hold of the rule, then dragging it to row 10, leaves 30 - 10 = 20 rows of
        // terminal — and the drag is the strip's even far above the strip's own rectangle.
        let grab = MouseEvent {
            kind: MouseKind::Press(MouseButton::Left),
            x: 10,
            y: strip.y,
            mods: Mods::NONE,
        };
        assert!(mouse(&mut ed, &lay, &grab, 120, &mut width, &mut dragging));
        let drag = MouseEvent {
            kind: MouseKind::Drag(MouseButton::Left),
            x: 10,
            y: 10,
            mods: Mods::NONE,
        };
        assert!(mouse(&mut ed, &lay, &drag, 120, &mut width, &mut dragging));
        assert_eq!(ed.bottom.height, 17, "the drag resized the strip");
    }

    /// A closed panel is not a pane: the text keeps the whole frame, as it always did.
    #[test]
    fn a_closed_panel_leaves_the_text_the_whole_frame() {
        let ed = editor();
        let lay = layout(120, 30, &ed, None);
        assert_eq!(lay.editor, Some(Rect::new(0, 0, 120, 30)));
        assert_eq!(lay.panel, None);
        assert_eq!(lay.border, None);
    }

    /// Open on a wide terminal: text, the rule, the panel; two fifths to the panel, and neither
    /// surface below its minimum whatever the operator dragged it to.
    #[test]
    fn a_wide_frame_is_two_columns_and_a_rule() {
        let mut ed = editor();
        ed.panel.open = true;
        let lay = layout(120, 30, &ed, None);
        let panel = lay.panel.expect("a panel column");
        let text = lay.editor.expect("an editor column");
        assert_eq!((panel.x, panel.width), (72, 48));
        assert_eq!(panel.width, 120 * 2 / 5);
        assert_eq!(lay.border, Some(Rect::new(71, 0, 1, 27)));
        assert_eq!(
            text.width + 1 + panel.width,
            120,
            "the frame is filled once"
        );

        // Dragged as narrow and as wide as it goes: the text keeps EDITOR_MIN and the panel keeps
        // PANEL_MIN, whatever the drag said.
        let narrow = layout(120, 30, &ed, Some(1)).panel.expect("a panel");
        assert_eq!(narrow.width, PANEL_MIN);
        assert_eq!(
            layout(120, 30, &ed, Some(1)).editor.unwrap().width,
            120 - PANEL_MIN - 1
        );
        let wide = layout(120, 30, &ed, Some(500)).panel.expect("a panel");
        assert_eq!(wide.width, 120 - EDITOR_MIN - 1);

        // A very wide terminal caps the panel where the design says, not at half.
        let capped = layout(400, 30, &ed, None).panel.expect("a panel");
        assert_eq!(capped.width, PANEL_MAX);
    }

    /// Below the two minimums and the rule, one surface takes the frame and `ctrl-]` swaps which:
    /// two columns that fit neither is the squeeze the design refuses.
    #[test]
    fn a_narrow_frame_zooms_the_focused_surface() {
        let mut ed = editor();
        ed.panel.open = true;
        ed.focus = Focus::Panel;
        let lay = layout(90, 30, &ed, None);
        assert_eq!(lay.panel, Some(Rect::new(0, 0, 90, 27)));
        assert_eq!(lay.editor, None, "the text is not drawn beside it");
        // The keyboard to the text: now it is the panel that is off the screen, still open.
        ed.focus = Focus::Text;
        let lay = layout(90, 30, &ed, None);
        assert_eq!(lay.editor, Some(Rect::new(0, 0, 90, 27)));
        assert_eq!(lay.panel, None);
        // At exactly the two minimums and the rule it splits again.
        ed.focus = Focus::Panel;
        let lay = layout(EDITOR_MIN + 1 + PANEL_MIN, 30, &ed, None);
        assert!(lay.editor.is_some() && lay.panel.is_some());
    }

    /// `alt-m` zooms the focused surface on a terminal wide enough to split, and zooms it back.
    #[test]
    fn zoom_takes_the_frame_and_gives_it_back() {
        let mut ed = editor();
        ed.panel.open = true;
        ed.focus = Focus::Panel;
        ed.panel_zoom();
        let lay = layout(120, 30, &ed, None);
        assert_eq!(lay.panel, Some(Rect::new(0, 0, 120, 27)));
        assert_eq!(lay.editor, None);
        ed.panel_cross();
        let lay = layout(120, 30, &ed, None);
        assert_eq!(lay.editor, Some(Rect::new(0, 0, 120, 27)));
        assert_eq!(lay.panel, None);
        ed.panel_zoom();
        let lay = layout(120, 30, &ed, None);
        assert!(lay.editor.is_some() && lay.panel.is_some(), "split again");
    }

    /// The three host keys are the global map's, so help, M-x and the bars see them; they reach
    /// the same places they did when the binary answered them itself.
    #[test]
    fn the_host_keys_are_the_three_the_design_shares() {
        let mut ed = editor();
        let key =
            |code: KeyCode, mods: rano::term::event::Mods| rano::term::KeyEvent { code, mods };
        let alt_i = key(KeyCode::Char('i'), rano::term::event::Mods::ALT);
        ed.handle_key(alt_i);
        assert!(
            ed.panel.open && ed.focus == Focus::Panel,
            "alt-i opens it, focused"
        );
        let cross = key(KeyCode::Char(']'), rano::term::event::Mods::CTRL);
        let _ = ed.handle_key(cross);
        assert!(ed.focus == Focus::Text, "ctrl-] crosses to the text");
        let _ = ed.handle_key(cross);
        assert!(ed.focus == Focus::Panel, "and back");
        let alt_m = key(KeyCode::Char('m'), rano::term::event::Mods::ALT);
        let _ = ed.handle_key(alt_m);
        assert!(ed.panel.zoom, "alt-m zooms");
        let _ = ed.handle_key(alt_i);
        assert!(!ed.panel.open, "alt-i hides it again");
        // A key nobody claimed: nothing happens, which is what the binary did before.
        let _ = ed.handle_key(key(KeyCode::Char('z'), rano::term::event::Mods::ALT));
        assert!(!ed.panel.open && !ed.panel.zoom);
    }

    /// The shell's caret, exactly where it is: PS1='$ ', then `abc`, puts the frame's caret at
    /// column 5 of the prompt's own row — and a second line moves it down by the lines printed,
    /// which is what a swapped (row, column) could not do.
    #[test]
    fn the_caret_is_exactly_at_the_shells_caret() {
        use std::path::Path;
        let mut ed = editor();
        ed.bottom.open = true;
        ed.bottom.height = 12;
        ed.focus = Focus::Terminal;
        ed.bottom
            .new_tab(Path::new("/tmp"), 40, 6, 200)
            .expect("a shell");
        let lay = layout(120, 40, &ed, None);
        sync_bottom_size(&mut ed, &lay);
        let strip = lay.bottom.expect("a strip");
        // The prompt is set and the per-command hook cleared: the operator's own
        // PROMPT_COMMAND rewrites PS1 on every prompt, which moves the caret off where the
        // test reads it. The waits accept only the TYPED ECHO at the new prompt — a line
        // starting `$ ` — so no earlier line, whatever the hook printed, can satisfy them.
        ed.bottom.tabs[0].send(b"PROMPT_COMMAND=\n");
        ed.bottom.tabs[0].send(b"PS1='$ '\n");
        ed.bottom.tabs[0].send(b"printf '\\033[2J\\033[H'\n");
        // The clear must FINISH before anything is typed, or the clear eats the echo: wait
        // for the new prompt to be the screen's first row.
        assert!(
            feed_until(&mut ed, |l| l.trim_end() == "$"),
            "the cleared screen shows the bare prompt"
        );
        ed.bottom.tabs[0].send(b"abc");
        assert!(
            feed_until(&mut ed, |l| l.trim_start().starts_with("$ abc")),
            "the typed echo at the new prompt"
        );
        let mut buf = rano::render::Buffer::empty(Rect::new(0, 0, 120, 40));
        let caret = draw_frame(&mut buf, &mut ed, &lay).expect("a caret");
        // The row `$ abc` — the prompt is two columns, abc three: the caret sits after the c.
        // The caret sits after the c of `abc`: the prompt's width plus three.
        let after = caret.0 - strip.x;
        assert!((3..8).contains(&after), "just after abc: {caret:?}");
        let row = caret.1;
        assert!(
            (strip.y + 2..strip.y + strip.height).contains(&row),
            "inside the shell's rows: {caret:?}"
        );
        // A printed line moves the caret down a row, not sideways.
        // The shell may be dash, where the kernel echoes the typed `z` wherever the cursor
        // is — no readline — so the wait is the ECHO ITSELF: `z` on the glass.
        ed.bottom.tabs[0].send(b"\r\nz");
        assert!(feed_until(&mut ed, |l| l.contains('z')));
        let caret = draw_frame(&mut buf, &mut ed, &lay).expect("a caret");
        assert!(caret.1 > row, "moved down, not sideways: {caret:?}");
        assert!(
            (caret.0 - strip.x) < after,
            "a fresh short prompt, left of abc's end: {caret:?}"
        );
    }

    /// One real cursor, and it is in the focused pane: the frame's caret lies inside that
    /// surface's rectangle, and nowhere else.
    #[test]
    fn the_real_cursor_is_in_the_focused_surface() {
        use std::path::Path;
        let mut ed = editor();
        let mut buf = rano::render::Buffer::empty(Rect::new(0, 0, 120, 40));
        // Text.
        let lay = layout(120, 40, &ed, None);
        let cur = draw_frame(&mut buf, &mut ed, &lay).expect("the text has a caret");
        assert!(lay.editor.expect("an editor").contains(cur.0, cur.1));
        // The panel's composer.
        ed.panel.open = true;
        ed.focus = Focus::Panel;
        ed.panel.input = "abc".to_string();
        ed.panel.cursor = 3;
        let lay = layout(120, 40, &ed, None);
        let cur = draw_frame(&mut buf, &mut ed, &lay).expect("the composer has a caret");
        assert!(
            lay.panel.expect("a panel").contains(cur.0, cur.1),
            "the caret is in the composer: {cur:?}"
        );
        // The terminal.
        ed.panel.open = false;
        ed.bottom.open = true;
        ed.bottom.height = 12;
        ed.focus = Focus::Terminal;
        ed.bottom
            .new_tab(Path::new("/tmp"), 40, 6, 200)
            .expect("a shell");
        let lay = layout(120, 40, &ed, None);
        sync_bottom_size(&mut ed, &lay);
        let strip = lay.bottom.expect("a strip");
        let cur = draw_frame(&mut buf, &mut ed, &lay).expect("the shell has a caret");
        assert!(
            strip.contains(cur.0, cur.1),
            "the shell's caret is in the strip: {cur:?}"
        );
        // An open prompt, over it all: C-f is the search.
        let _ = ed.handle_key(rano::term::KeyEvent::new(
            KeyCode::Char('f'),
            rano::term::event::Mods::CTRL,
        ));
        for c in "needle".chars() {
            let _ = ed.handle_key(rano::term::KeyEvent::new(
                KeyCode::Char(c),
                rano::term::event::Mods::NONE,
            ));
        }
        let lay = layout(120, 40, &ed, None);
        let cur = draw_frame(&mut buf, &mut ed, &lay).expect("the prompt has a caret");
        assert!(
            lay.editor.expect("an editor").contains(cur.0, cur.1),
            "the prompt's caret is in the frame: {cur:?}"
        );
        assert!(!strip.contains(cur.0, cur.1), "and not in the strip");
    }

    /// With the panel open, the strip still has its scrollbar: the panel used to paint over
    /// the strip's rows, track and all, because it was drawn after them.
    #[test]
    fn the_strip_keeps_its_scrollbar_with_the_panel_open() {
        use std::path::Path;
        let mut ed = editor();
        ed.panel.open = true;
        ed.bottom.open = true;
        ed.bottom.height = 12;
        ed.focus = Focus::Panel;
        ed.bottom
            .new_tab(Path::new("/tmp"), 40, 6, 200)
            .expect("a shell");
        // A conversation long enough that the panel's body paints every row it is given,
        // including the rows the strip sits on: the panel's lines come from its session, and a
        // long reply fills the column to its foot.
        ed.panel.session = "s-1".to_string();
        for i in 0..120 {
            ed.panel
                .rows
                .push(rano::panel::Row::Note(format!("row {i}")));
        }
        for (w, h) in [(239u16, 68u16), (100, 30)] {
            let lay = layout(w, h, &ed, None);
            sync_bottom_size(&mut ed, &lay);
            ed.bottom.tabs[0].send(b"seq 1 200\n");
            let mut ready = false;
            for _ in 0..3000 {
                ed.bottom.tabs[0].feed();
                if ed.bottom.tabs[0]
                    .lines()
                    .iter()
                    .any(|l| l.plain().trim() == "200")
                {
                    ready = true;
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            assert!(ready, "seq answered at {w}x{h}");
            let mut buf = rano::render::Buffer::empty(Rect::new(0, 0, w, h));
            draw_frame(&mut buf, &mut ed, &lay);
            let strip = lay.bottom.expect("a strip");
            let right = (strip.x + strip.width - 1) as usize;
            // One glyph the whole way down — the thumb is told from the track by role, not by
            // a fatter glyph. The panel holds the keyboard here, so the thumb is Strong (the
            // accent is the focused surface's alone) and the track faint.
            let faint = Style::of(Role::Faint);
            let col: Vec<(char, bool)> = ((strip.y as usize + 2)
                ..(strip.y + strip.height) as usize)
                .map(|y| {
                    buf.cell(right as u16, y as u16)
                        .map(|c| (c.symbol.chars().next().unwrap_or(' '), c.style != faint))
                        .unwrap_or((' ', false))
                })
                .collect();
            assert!(
                col.iter().all(|(c, _)| *c == '\u{2502}'),
                "one glyph, the track's, at {w}x{h}: {col:?}"
            );
            assert!(
                col.iter().any(|(_, t)| *t),
                "the thumb is drawn, told from the track by role, at {w}x{h}: {col:?}"
            );
            assert!(
                col.iter().any(|(_, t)| !*t),
                "and the track is faint at {w}x{h}: {col:?}"
            );
        }
    }

    /// A click in the strip takes the keyboard there; a click in the text gives it back.
    #[test]
    fn a_click_in_the_strip_focuses_the_terminal() {
        use std::path::Path;
        let mut ed = editor();
        ed.bottom.open = true;
        ed.bottom.height = 12;
        ed.focus = Focus::Text;
        ed.bottom
            .new_tab(Path::new("/tmp"), 40, 6, 200)
            .expect("a shell");
        let lay = layout(120, 40, &ed, None);
        sync_bottom_size(&mut ed, &lay);
        let strip = lay.bottom.expect("a strip");
        let click = rano::term::MouseEvent {
            kind: MouseKind::Press(MouseButton::Left),
            x: 5,
            y: strip.y + 3,
            mods: rano::term::event::Mods::NONE,
        };
        let mut width = None;
        let mut dragging = false;
        assert!(mouse(&mut ed, &lay, &click, 120, &mut width, &mut dragging));
        assert_eq!(
            ed.focus,
            Focus::Terminal,
            "the click took the keyboard there"
        );
    }

    /// M-0 and C-] each reach the open strip from the text, and its bar says so.
    #[test]
    fn m_zero_and_cross_focus_the_open_strip() {
        let mut ed = editor();
        ed.bottom.open = true;
        ed.bottom.height = 12;
        ed.focus = Focus::Text;
        rano::host::key(
            &mut ed,
            rano::term::KeyEvent::new(KeyCode::Char('0'), rano::term::event::Mods::ALT),
        );
        assert_eq!(ed.focus, Focus::Terminal, "M-0 focuses an open strip");
        assert!(
            ed.key_hints().iter().any(|(_, t)| t == "New Tab"),
            "the bar is the terminal's"
        );
        rano::host::key(
            &mut ed,
            rano::term::KeyEvent::new(KeyCode::Char(']'), rano::term::event::Mods::CTRL),
        );
        assert_eq!(ed.focus, Focus::Text, "C-] crosses back to the text");
        rano::host::key(
            &mut ed,
            rano::term::KeyEvent::new(KeyCode::Char(']'), rano::term::event::Mods::CTRL),
        );
        assert_eq!(ed.focus, Focus::Terminal, "and to the strip again");
        // With the panel open too the cycle is text -> panel -> terminal -> text.
        ed.panel.open = true;
        ed.focus = Focus::Text;
        let cross = || rano::term::KeyEvent::new(KeyCode::Char(']'), rano::term::event::Mods::CTRL);
        rano::host::key(&mut ed, cross());
        assert_eq!(ed.focus, Focus::Panel);
        rano::host::key(&mut ed, cross());
        assert_eq!(ed.focus, Focus::Terminal);
        rano::host::key(&mut ed, cross());
        assert_eq!(ed.focus, Focus::Text);
        // M-0 while focused gives the keyboard back; a second one focuses it again, never hides.
        ed.bottom.open = true;
        ed.focus = Focus::Terminal;
        rano::host::key(
            &mut ed,
            rano::term::KeyEvent::new(KeyCode::Char('0'), rano::term::event::Mods::ALT),
        );
        assert_eq!(ed.focus, Focus::Text);
        assert!(ed.bottom.open, "the strip stays open");
        rano::host::key(
            &mut ed,
            rano::term::KeyEvent::new(KeyCode::Char('0'), rano::term::event::Mods::ALT),
        );
        assert_eq!(ed.focus, Focus::Terminal, "and it comes back");
    }

    /// The strip's last column is a scrollbar — a track with a thumb, always drawn. `↑N more` is
    /// a counter; this is where in the history the view actually is.
    #[test]
    fn the_strips_last_column_is_a_scrollbar() {
        use std::path::Path;
        let mut ed = editor();
        ed.bottom.open = true;
        ed.bottom.height = 12;
        ed.focus = Focus::Terminal;
        ed.bottom
            .new_tab(Path::new("/tmp"), 40, 6, 200)
            .expect("a shell");
        let lay = layout(120, 40, &ed, None);
        sync_bottom_size(&mut ed, &lay);
        ed.bottom.tabs[0].send(b"seq 1 200\n");
        assert!(feed_until(&mut ed, |l| l.trim() == "200"), "seq answered");
        let strip = lay.bottom.expect("a strip");
        let right = (strip.x + strip.width - 1) as usize;
        let accent = Style::of(Role::UserAccent);
        let track = |buf: &rano::render::Buffer| -> Vec<(char, bool)> {
            ((strip.y as usize + 2)..(strip.y + strip.height) as usize)
                .map(|y| {
                    buf.cell(right as u16, y as u16)
                        .map(|c| (c.symbol.chars().next().unwrap_or(' '), c.style == accent))
                        .unwrap_or((' ', false))
                })
                .collect()
        };
        let mut buf = rano::render::Buffer::empty(Rect::new(0, 0, 120, 40));
        draw_frame(&mut buf, &mut ed, &lay);
        let at_bottom = track(&buf);
        assert_eq!(
            at_bottom.last(),
            Some(&('\u{2502}', true)),
            "the thumb is at the foot, in the accent: {at_bottom:?}"
        );
        assert!(
            at_bottom.iter().all(|(c, _)| *c == '\u{2502}'),
            "one glyph, the track's, never shell text: {at_bottom:?}"
        );
        // Three pages back through the host path: the thumb moves up, and the counter agrees.
        for _ in 0..3 {
            rano::host::key(
                &mut ed,
                rano::term::KeyEvent::new(KeyCode::PageUp, rano::term::event::Mods::NONE),
            );
        }
        draw_frame(&mut buf, &mut ed, &lay);
        let scrolled = track(&buf);
        assert_ne!(scrolled, at_bottom, "the thumb moved: {scrolled:?}");
        assert!(
            buf.emit(rano::render::Palette::None)
                .join("")
                .contains("↑3 more"),
            "the counter agrees with the thumb"
        );
        // A key is an answer to the newest output: back to the bottom, thumb and all.
        rano::host::key(
            &mut ed,
            rano::term::KeyEvent::new(KeyCode::Char('x'), rano::term::event::Mods::NONE),
        );
        draw_frame(&mut buf, &mut ed, &lay);
        assert_eq!(track(&buf), at_bottom, "back at the bottom after a key");
    }

    /// The split rule carries the editor's scrollbar: with the panel open there is one column
    /// between the text and the panel, and a bar beside the rule would read as a second rule.
    /// The thumb is the accent `│` in the rule's own column, and the text's last column is
    /// text. With the panel away the editor's own bar is back where it always was.
    #[test]
    fn the_split_rule_carries_the_editors_scrollbar() {
        let text = (1..=400)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let mut file_buf = rano::buffer::Buffer::new();
        file_buf.set_rows(text.lines().map(|l| l.chars().collect()).collect());
        let mut ed = Editor::new(file_buf, Config::default());
        ed.push_keymap(host_map());
        ed.panel.open = true;
        let lay = layout(120, 40, &ed, None);
        let border = lay.border.expect("a rule");
        let er = lay.editor.expect("the text column");
        let mut buf = rano::render::Buffer::empty(Rect::new(0, 0, 120, 40));
        draw_frame(&mut buf, &mut ed, &lay);
        // The text holds the keyboard here, so the thumb is the accent REVERSED on the accent
        // track — the rule is the text's marker and the thumb still reads against it.
        let thumb_style = Style::of(Role::UserAccent).reverse();
        let cell = |x: u16, y: u16| {
            buf.cell(x, y)
                .map(|c| {
                    (
                        c.symbol.chars().next().unwrap_or(' '),
                        c.style == thumb_style,
                    )
                })
                .unwrap_or((' ', false))
        };
        // The rule's column over the text's rows: one glyph, the thumb in the accent,
        // reversed against the accent track.
        let rule: Vec<(char, bool)> = (1..er.height as usize)
            .map(|y| cell(border.x, y as u16))
            .collect();
        assert!(
            rule.iter().all(|(c, _)| *c == '\u{2502}'),
            "one glyph, the rule's: {rule:?}"
        );
        assert!(
            rule.iter().any(|(_, t)| *t),
            "the thumb lives in the rule: {rule:?}"
        );
        // The text's own last column is not a second bar.
        let beside: Vec<char> = (1..er.height as usize)
            .map(|y| cell(er.width - 1, y as u16).0)
            .collect();
        assert!(
            beside.iter().all(|c| *c != '\u{2502}'),
            "no bar beside the rule: {beside:?}"
        );
        // Panel away: the editor's own scrollbar is back in its own column.
        ed.panel.open = false;
        let lay = layout(120, 40, &ed, None);
        let mut buf = rano::render::Buffer::empty(Rect::new(0, 0, 120, 40));
        draw_frame(&mut buf, &mut ed, &lay);
        let er = lay.editor.expect("the whole frame");
        let own: Vec<char> = (1..er.height as usize)
            .map(|y| {
                buf.cell(er.width - 1, y as u16)
                    .map(|c| c.symbol.chars().next().unwrap_or(' '))
                    .unwrap_or(' ')
            })
            .collect();
        assert!(
            own.contains(&'\u{2502}'),
            "the editor's own bar is back: {own:?}"
        );
    }

    /// Hit-testing follows the drawing: with the panel open the thumb lives in the split rule,
    /// so a press THERE jumps the view and a press on the text's last column does not — the
    /// column that draws nothing scrolls nothing. A press on the rule outside the thumb is
    /// still the rule's drag.
    #[test]
    fn the_bars_hit_testing_follows_the_drawing() {
        let text = (1..=400)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let mut file_buf = rano::buffer::Buffer::new();
        file_buf.set_rows(text.lines().map(|l| l.chars().collect()).collect());
        let mut ed = Editor::new(file_buf, Config::default());
        ed.push_keymap(host_map());
        ed.panel.open = true;
        let lay = layout(120, 40, &ed, None);
        let mut buf = rano::render::Buffer::empty(Rect::new(0, 0, 120, 40));
        draw_frame(&mut buf, &mut ed, &lay);
        let border = lay.border.expect("a rule");
        let er = lay.editor.expect("the text column");
        let t0 = ed.chrome.top() as usize;
        let rows = er.height as usize - t0;
        let (at, thumb) = ui::thumb_span(rows, ed.scroll_pos(), ed.scroll_total(rows))
            .expect("a long file scrolls");
        // Grab the thumb and drag it to the foot of the rule: the view follows the pointer.
        let mid = t0 + at + thumb / 2;
        let mut dragging = false;
        let mut width = None;
        let mut send =
            |ed: &mut Editor, kind: rano::term::MouseKind, y: u16, dragging: &mut bool| {
                mouse(
                    ed,
                    &lay,
                    &rano::term::MouseEvent {
                        kind,
                        x: border.x,
                        y,
                        mods: rano::term::event::Mods::NONE,
                    },
                    120,
                    &mut width,
                    dragging,
                )
            };
        // Press the thumb, then drag to the foot of the rule: the view follows.
        send(
            &mut ed,
            rano::term::MouseKind::Press(rano::term::MouseButton::Left),
            mid as u16,
            &mut dragging,
        );
        send(
            &mut ed,
            rano::term::MouseKind::Drag(rano::term::MouseButton::Left),
            (t0 + rows - 1) as u16,
            &mut dragging,
        );
        send(
            &mut ed,
            rano::term::MouseKind::Release(rano::term::MouseButton::Left),
            (t0 + rows - 1) as u16,
            &mut dragging,
        );
        assert!(!ed.scroll_grab, "the release let go");
        let max = ed.max_scroll(rows);
        assert_eq!(
            ed.bs_mut().scroll,
            max,
            "the thumb dragged to the foot is the view at its end"
        );
        assert!(!dragging, "a thumb drag is not a rule drag");
        // A drag above the track — the rule's title row — is the view's start, not a panic.
        let (at2, thumb2) =
            ui::thumb_span(rows, ed.scroll_pos(), ed.scroll_total(rows)).expect("still long");
        send(
            &mut ed,
            rano::term::MouseKind::Press(rano::term::MouseButton::Left),
            (t0 + at2 + thumb2 / 2) as u16,
            &mut dragging,
        );
        send(
            &mut ed,
            rano::term::MouseKind::Drag(rano::term::MouseButton::Left),
            0,
            &mut dragging,
        );
        assert_eq!(
            ed.bs_mut().scroll,
            0,
            "a drag to the title row is the view's start"
        );
        send(
            &mut ed,
            rano::term::MouseKind::Release(rano::term::MouseButton::Left),
            0,
            &mut dragging,
        );

        // A grab released over the TEXT lets go too: the next press on the rule resizes
        // rather than scrolling on a stale hold. (The closure above is done with by now.)
        ed.scroll_grab = true;
        mouse(
            &mut ed,
            &lay,
            &rano::term::MouseEvent {
                kind: rano::term::MouseKind::Release(rano::term::MouseButton::Left),
                x: er.width - 1,
                y: mid as u16,
                mods: rano::term::event::Mods::NONE,
            },
            120,
            &mut width,
            &mut dragging,
        );
        assert!(!ed.scroll_grab, "a release over the text lets go");
        // A press on the rule outside the thumb drags the width, and does not scroll.
        ed.scroll_to(0);
        let mut dragging = false;
        assert!(mouse(
            &mut ed,
            &lay,
            &rano::term::MouseEvent {
                kind: rano::term::MouseKind::Press(rano::term::MouseButton::Left),
                x: border.x,
                y: (t0 + at + thumb + 1).min(er.height as usize - 1) as u16,
                mods: rano::term::event::Mods::NONE,
            },
            120,
            &mut width,
            &mut dragging,
        ));
        assert!(dragging, "the rule outside the thumb still drags");
        assert_eq!(ed.bs_mut().scroll, 0, "the rule press did not scroll");
        // The text's own last column draws nothing and scrolls nothing.
        let mut dragging = false;
        mouse(
            &mut ed,
            &lay,
            &rano::term::MouseEvent {
                kind: rano::term::MouseKind::Press(rano::term::MouseButton::Left),
                x: er.width - 1,
                y: mid as u16,
                mods: rano::term::event::Mods::NONE,
            },
            120,
            &mut width,
            &mut dragging,
        );
        assert_eq!(ed.bs_mut().scroll, 0, "the text's last column is text");
    }

    /// A preview with the panel open keeps ONE bar: the rule carries the preview's place in
    /// its lines, and the text column draws no second bar beside it.
    #[test]
    fn a_preview_keeps_one_bar_in_the_rule() {
        // Blank lines between the paragraphs, or the renderer merges them into one wrapped
        // block: 200 numbered paragraphs must stay 200 blocks, more than the view holds.
        let text = (1..=200)
            .map(|i| format!("paragraph {i}\n\n"))
            .collect::<String>();
        let mut file_buf = rano::buffer::Buffer::new();
        file_buf.set_rows(text.lines().map(|l| l.chars().collect()).collect());
        file_buf.name = Some(PathBuf::from("notes.md"));
        let mut ed = Editor::new(file_buf, Config::default());
        ed.push_keymap(host_map());
        ed.panel.open = true;
        // The panel holds the keyboard, so the rule's track is faint and an accent cell can
        // only be the thumb.
        ed.focus = Focus::Panel;
        let _ = ed.handle_key(rano::term::KeyEvent::new(
            KeyCode::Char('p'),
            rano::term::event::Mods::ALT,
        ));
        assert!(ed.diff_view.is_some(), "a preview is open");
        let lay = layout(120, 40, &ed, None);
        let mut buf = rano::render::Buffer::empty(Rect::new(0, 0, 120, 40));
        draw_frame(&mut buf, &mut ed, &lay);
        let border = lay.border.expect("a rule");
        let er = lay.editor.expect("the text column");
        // The rule carries the preview's thumb: the panel holds the keyboard here, so the
        // thumb is Strong on the faint track — the cue without an accent, which is the
        // panel's marker to carry — and it sits exactly where the one sum puts it for the
        // preview's place in its rendered lines.
        let strong = Style::of(Role::Strong);
        let t0 = ed.chrome.top() as usize;
        let rows = er.height as usize - t0;
        let v = ed.diff_view.as_ref().expect("the preview");
        let (at, thumb) = ui::thumb_span(rano::diffview::body_rows(rows), v.top, v.lines.len())
            .expect("a long preview scrolls");
        for y in t0..er.height as usize {
            let cell = buf
                .cell(border.x, y as u16)
                .expect("the rule is drawn through the text's rows");
            let in_thumb = y >= t0 + at && y < t0 + at + thumb;
            assert_eq!(
                cell.style == strong,
                in_thumb,
                "row {y}: the thumb names the preview's place, in Strong"
            );
        }
        // And no second bar in the text's own last column.
        let beside: Vec<char> = (1..er.height as usize)
            .map(|y| {
                buf.cell(er.width - 1, y as u16)
                    .map(|c| c.symbol.chars().next().unwrap_or(' '))
                    .unwrap_or(' ')
            })
            .collect();
        assert!(
            beside.iter().all(|c| *c != '\u{2502}'),
            "no bar beside the rule with a preview: {beside:?}"
        );
    }

    /// The status and the key bar are the frame's last rows: the terminal's strip is above them,
    /// not the other way round.
    #[test]
    fn the_strip_sits_above_the_status_and_the_key_bar() {
        use std::path::Path;
        let mut ed = editor();
        ed.bottom.open = true;
        ed.bottom.height = 12;
        ed.bottom
            .new_tab(Path::new("/tmp"), 40, 6, 200)
            .expect("a shell");
        let lay = layout(120, 40, &ed, None);
        sync_bottom_size(&mut ed, &lay);
        let strip = lay.bottom.expect("a strip");
        assert_eq!(
            strip.y + strip.height,
            40 - ed.status_rows() as u16,
            "the strip ends above the status"
        );
        let mut buf = rano::render::Buffer::empty(Rect::new(0, 0, 120, 40));
        draw_frame(&mut buf, &mut ed, &lay);
        let rows = buf.emit(rano::render::Palette::None);
        assert!(rows[39].contains("C-x Exit"), "the key bar is the last row");
        assert!(rows[38].contains("C-g Help"), "and its second row above it");
        assert!(
            rows[strip.y as usize].contains('─'),
            "the strip's rule is above them"
        );
        // And the editor does not scroll the cursor under the strip.
        assert!(ed.text_h <= 40 - ed.chrome_rows() - strip.height as usize);
    }

    /// Each surface's own marker carries the accent while it has the keyboard — the split rule
    /// (track accent, thumb the accent reversed) for the text, the strip's rule for the
    /// terminal, the panel's header (bar and title) for the panel — and NO other marker does.
    /// The frame has a real thumb (a long file), and every accent cell on the glass is read:
    /// one accent says who types.
    #[test]
    fn the_focused_surface_is_marked() {
        let text = (1..=400)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let mut file_buf = rano::buffer::Buffer::new();
        file_buf.set_rows(text.lines().map(|l| l.chars().collect()).collect());
        let mut ed = Editor::new(file_buf, Config::default());
        ed.push_keymap(host_map());
        ed.panel.open = true;
        ed.bottom.open = true;
        ed.bottom.height = 12;
        let accent = Style::of(Role::UserAccent);
        for focus in [Focus::Text, Focus::Terminal, Focus::Panel] {
            ed.focus = focus;
            let lay = layout(120, 40, &ed, None);
            let mut buf = rano::render::Buffer::empty(Rect::new(0, 0, 120, 40));
            draw_frame(&mut buf, &mut ed, &lay);
            let border = lay.border.expect("a rule");
            let strip = lay.bottom.expect("a strip");
            let panel = lay.panel.expect("a panel");
            // Every accent cell in the three markers' rows and columns.
            let split_accent = (1..lay.editor.expect("text").height as usize).any(|y| {
                buf.cell(border.x, y as u16)
                    .is_some_and(|c| c.style == accent)
            });
            let strip_accent =
                (0..strip.width).any(|x| buf.cell(x, strip.y).is_some_and(|c| c.style == accent));
            let header_accent = (panel.x..panel.x + panel.width)
                .any(|x| buf.cell(x, 0).is_some_and(|c| c.style == accent));
            assert_eq!(
                split_accent,
                focus == Focus::Text,
                "{focus:?}: the split rule is the text's marker, thumb included"
            );
            assert_eq!(
                strip_accent,
                focus == Focus::Terminal,
                "{focus:?}: the strip's rule is the terminal's marker"
            );
            assert_eq!(
                header_accent,
                focus == Focus::Panel,
                "{focus:?}: the panel's header is the panel's marker, bar and title"
            );
        }
    }

    /// The strip is real tabs — index, title, the active one marked, the exit status when the
    /// shell is gone — and the terminal's keys are in the frame's bar while it has focus.
    #[test]
    fn the_strip_is_real_tabs_and_the_bar_has_the_terminals_keys() {
        use std::path::Path;
        let mut ed = editor();
        ed.bottom.open = true;
        ed.bottom.height = 12;
        ed.focus = Focus::Terminal;
        ed.bottom
            .new_tab(Path::new("/tmp"), 40, 6, 200)
            .expect("a first shell");
        ed.bottom
            .new_tab(Path::new("/tmp"), 40, 6, 200)
            .expect("a second shell");
        let lay = layout(120, 40, &ed, None);
        sync_bottom_size(&mut ed, &lay);
        let mut buf = rano::render::Buffer::empty(Rect::new(0, 0, 120, 40));
        draw_frame(&mut buf, &mut ed, &lay);
        let rows = buf.emit(rano::render::Palette::None);
        let frame = rows.join("\n");
        assert!(frame.contains(">2 sh"), "the active tab is marked: {frame}");
        assert!(frame.contains("1 sh"), "the other tab is there by index");
        assert!(
            !frame.contains("(M-n new"),
            "no literal key text in the strip"
        );
        assert!(frame.contains("New Tab"), "the bar has the terminal's keys");
        assert!(frame.contains("Close Tab"), "all of them");
        // A shell that exits says so in its tab.
        ed.bottom.tabs[1].send(b"exit 42\n");
        for _ in 0..3000 {
            ed.bottom.tabs[1].feed();
            if ed.bottom.tabs[1].exited.is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(ed.bottom.tabs[1].exited, Some(42), "the exit was noticed");
        draw_frame(&mut buf, &mut ed, &lay);
        let frame = buf.emit(rano::render::Palette::None).join("\n");
        assert!(
            frame.contains("exited 42"),
            "the tab says the status: {frame}"
        );
    }

    /// Scrolled back, a key sent to the shell snaps the view to the bottom: the strip's "N more"
    /// marker is gone, because the operator is answering the newest output now.
    #[test]
    fn a_key_sent_to_the_shell_snaps_the_view_to_the_bottom() {
        use std::path::Path;
        let mut ed = editor();
        ed.bottom.open = true;
        ed.bottom.height = 12;
        ed.focus = Focus::Terminal;
        ed.bottom
            .new_tab(Path::new("/tmp"), 40, 6, 200)
            .expect("a shell");
        let lay = layout(120, 40, &ed, None);
        sync_bottom_size(&mut ed, &lay);
        ed.bottom.tabs[0].send(b"seq 1 30\n");
        assert!(feed_until(&mut ed, |l| l.trim() == "30"), "seq answered");
        ed.bottom.tabs[0].scroll_up(2);
        let mut buf = rano::render::Buffer::empty(Rect::new(0, 0, 120, 40));
        draw_frame(&mut buf, &mut ed, &lay);
        assert!(
            buf.emit(rano::render::Palette::None)
                .join("")
                .contains("more"),
            "the strip says how far back the view is"
        );
        // A key through the host's path is the shell's input, and the view snaps to the bottom.
        rano::host::key(
            &mut ed,
            rano::term::KeyEvent::new(
                rano::term::KeyCode::Char('x'),
                rano::term::event::Mods::NONE,
            ),
        );
        draw_frame(&mut buf, &mut ed, &lay);
        let rows = buf.emit(rano::render::Palette::None);
        assert!(
            !rows.join("").contains("more"),
            "the marker is gone: {rows:?}"
        );
    }

    /// Scrolled back, a short history row painted over a longer live row leaves no tail: the
    /// terminal's rows are the pane's width, because `Buffer::set_line` writes only the spans.
    /// The shell's prompt is set here rather than trusted: the operator's own hook prints a
    /// timestamped line per command, which moves every row the scroll lands on. The marker is
    /// `\132` on the command line and `Z` in the output, so echo and output are distinct by
    /// construction — and the tail is a RUN of Zs, never one.
    #[test]
    fn a_short_history_row_leaves_no_tail_of_the_long_row_it_covers() {
        use std::path::Path;
        let mut ed = editor();
        ed.bottom.open = true;
        ed.bottom.height = 12;
        ed.bottom
            .new_tab(Path::new("/tmp"), 40, 6, 200)
            .expect("a shell");
        let lay = layout(120, 40, &ed, None);
        sync_bottom_size(&mut ed, &lay);
        // One prompt shape, no per-command hook: the rows the scroll lands on are then the
        // same on every machine, whatever ~/.bashrc prints.
        ed.bottom.tabs[0].send(b"PROMPT_COMMAND=\n");
        ed.bottom.tabs[0].send(b"PS1='P> '\n");
        ed.bottom.tabs[0].send(b"seq 1 30\n");
        // `\132` is a Z in the output and four characters in the echo: the echoed command
        // cannot satisfy a Z assertion, whichever rows the scroll lands on.
        ed.bottom.tabs[0].send(b"head -c 40 /dev/zero | tr '\\0' '\\132'; echo\n");
        // Both conditions, not the first: the long row is on the glass AND the shell has come
        // back to its prompt — under load the two land some time apart.
        assert!(
            feed_until(&mut ed, |l| l.contains("ZZZZZZ")),
            "the long row is on the live screen"
        );
        let settled = |ed: &mut Editor| {
            for _ in 0..3000 {
                let tab = &mut ed.bottom.tabs[ed.bottom.active];
                tab.feed();
                let prompt_back = tab
                    .lines()
                    .last()
                    .is_some_and(|l| l.plain().trim_end().ends_with("P>"));
                if prompt_back {
                    return true;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            false
        };
        assert!(settled(&mut ed), "the shell came back to its prompt");
        let mut buf = rano::render::Buffer::empty(Rect::new(0, 0, 120, 40));
        draw_frame(&mut buf, &mut ed, &lay);
        assert!(
            buf.emit(rano::render::Palette::None)
                .join("")
                .contains("ZZZZZZ"),
            "drawn before scrolling"
        );
        // Two rows back: the long row is out of the window, and short history rows take its
        // place — drawn into the SAME buffer, which is what a redraw is.
        ed.bottom.tabs[0].scroll_up(2);
        draw_frame(&mut buf, &mut ed, &lay);
        let rows = buf.emit(rano::render::Palette::None);
        let tail = rows
            .iter()
            .find(|r| r.contains("ZZZZZZ"))
            .cloned()
            .unwrap_or_default();
        assert!(
            !tail.contains("ZZZZZZ"),
            "a tail of the old row survived: {tail:?}"
        );
    }

    /// The frame's foot is full width: the composer is the panel's last row, inside the panel's
    /// columns, on the strip's last row — the two end together above the foot, not on the
    /// frame's last row beside the key bar.
    #[test]
    fn the_foot_is_full_width_and_the_composer_stays_in_the_panel() {
        use std::path::Path;
        let mut ed = editor();
        ed.panel.open = true;
        ed.panel.input = "composer-words".to_string();
        ed.focus = Focus::Panel;
        ed.bottom.open = true;
        ed.bottom.height = 12;
        ed.bottom
            .new_tab(Path::new("/tmp"), 40, 6, 200)
            .expect("a shell");
        for (w, h) in [(239u16, 68u16), (100, 30)] {
            let lay = layout(w, h, &ed, None);
            sync_bottom_size(&mut ed, &lay);
            let strip = lay.bottom.expect("a strip");
            let foot = lay.foot.expect("the frame's foot");
            assert_eq!(foot.width, w, "the foot spans the frame at {w}x{h}");
            assert_eq!(foot.y + foot.height, h, "at the very foot");
            assert!(
                strip.y + strip.height <= foot.y,
                "the strip is above the foot"
            );
            let mut buf = rano::render::Buffer::empty(Rect::new(0, 0, w, h));
            draw_frame(&mut buf, &mut ed, &lay);
            let rows = buf.emit(rano::render::Palette::None);
            let composer_row = rows
                .iter()
                .position(|r| r.contains("composer-words"))
                .unwrap_or_else(|| panic!("the composer is drawn at {w}x{h}"));
            let panel = lay.panel.expect("a panel column");
            assert_eq!(
                composer_row as u16,
                strip.y + strip.height - 1,
                "the composer is on the strip's last row at {w}x{h}: row {composer_row}"
            );
            let at = rows[composer_row]
                .find("composer-words")
                .expect("in its row");
            assert!(
                at >= panel.x as usize && at < (panel.x + panel.width) as usize,
                "inside the panel's columns at {w}x{h}"
            );
            // The status row and the key bar span the whole frame.
            let bar = rows[(h - 3) as usize..].join("");
            assert!(
                bar.contains("RET Send"),
                "the frame's bar is full width at {w}x{h}"
            );
        }
    }

    /// The foot's rows are nobody else's: the strip ends above the status row, the status row
    /// holds no shell text, the text ends where the strip begins while the panel runs beside
    /// it to the foot, and the title bar spans the frame at every open/close transition.
    #[test]
    fn the_foots_rows_are_nobody_elses() {
        use std::path::Path;
        let mut ed = editor();
        ed.bottom.open = true;
        ed.bottom.height = 12;
        ed.bottom
            .new_tab(Path::new("/tmp"), 40, 6, 200)
            .expect("a shell");
        for (w, h) in [(239u16, 68u16), (100, 30)] {
            let lay = layout(w, h, &ed, None);
            sync_bottom_size(&mut ed, &lay);
            ed.bottom.tabs[0].send(b"seq 1 50\n");
            let mut ready = false;
            for _ in 0..600 {
                ed.bottom.tabs[0].feed();
                if ed.bottom.tabs[0]
                    .lines()
                    .iter()
                    .any(|l| l.plain().trim() == "50")
                {
                    ready = true;
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            assert!(ready, "seq answered at {w}x{h}");
            let strip = lay.bottom.expect("a strip");
            let foot_y = h - ed.status_rows() as u16;
            assert!(
                strip.y + strip.height <= foot_y,
                "A: the strip ends above the status row at {w}x{h}: {} vs {foot_y}",
                strip.y + strip.height
            );
            let mut buf = rano::render::Buffer::empty(Rect::new(0, 0, w, h));
            draw_frame(&mut buf, &mut ed, &lay);
            let rows = buf.emit(rano::render::Palette::None);
            for (row, r) in rows
                .iter()
                .enumerate()
                .take(h as usize)
                .skip(foot_y as usize)
            {
                assert!(
                    !r.contains("dead@") && !r.trim_start().starts_with('['),
                    "A: no shell text on the status row {row} at {w}x{h}: {r:?}"
                );
            }
            // C: the title bar spans the frame after open and close. The reverse-video band
            // is blank to Palette::None, so the styled copy is what says its width.
            let styled = buf.emit(rano::render::Palette::Colour);
            assert!(
                styled[0].contains("  rano") && styled[0].chars().count() >= w as usize,
                "C: the title bar spans the frame at {w}x{h}: {} wide",
                styled[0].chars().count()
            );
            // Open and close the panel: the title still spans.
            ed.panel.open = true;
            let lay = layout(w, h, &ed, None);
            sync_bottom_size(&mut ed, &lay);
            let mut buf = rano::render::Buffer::empty(Rect::new(0, 0, w, h));
            draw_frame(&mut buf, &mut ed, &lay);
            let styled = buf.emit(rano::render::Palette::Colour);
            let er = lay.editor.expect("an editor");
            assert!(
                styled[0].contains("  rano") && styled[0].chars().count() >= er.width as usize,
                "C: the title spans its column with the panel open at {w}x{h}: {} wide",
                styled[0].chars().count()
            );
            // B: with the panel open, the text ends where the strip begins and the panel runs
            // on beside the strip to the foot.
            let er = lay.editor.expect("an editor");
            let pr = lay.panel.expect("a panel");
            let strip2 = lay.bottom.expect("the strip beside the panel");
            assert_eq!(
                er.y + er.height,
                strip2.y,
                "B: the text ends where the strip begins at {w}x{h}"
            );
            assert_eq!(
                pr.y + pr.height,
                h - ed.status_rows() as u16,
                "B: the panel runs to the foot at {w}x{h}"
            );
            ed.panel.open = false;
            let lay = layout(w, h, &ed, None);
            sync_bottom_size(&mut ed, &lay);
            let mut buf = rano::render::Buffer::empty(Rect::new(0, 0, w, h));
            draw_frame(&mut buf, &mut ed, &lay);
            let styled = buf.emit(rano::render::Palette::Colour);
            assert!(
                styled[0].contains("  rano") && styled[0].chars().count() >= w as usize,
                "C: the title spans after the close at {w}x{h}: {} wide",
                styled[0].chars().count()
            );
        }
    }

    /// The text fills its column down to the strip, and the title bar spans the frame: proved
    /// against a real pty, not the layout's rects — the rects were equal while the editor left
    /// three blank rows above the strip because it still subtracted the foot's rows from its
    /// text height.
    #[test]
    fn the_text_fills_its_column_and_the_title_spans_it() {
        use std::path::Path;
        // A file long enough that the editor has lines to draw past where the foot was.
        let text = (1..=120)
            .map(|i| format!("line {i} of the file"))
            .collect::<Vec<_>>()
            .join("\n");
        let mut file_buf = rano::buffer::Buffer::new();
        file_buf.set_rows(text.lines().map(|l| l.chars().collect()).collect());
        file_buf.name = Some(PathBuf::from("fill.txt"));
        // The editor is built on this buffer directly: a fresh one has no lines to fill a frame.
        let mut ed = Editor::new(file_buf, Config::default());
        ed.push_keymap(host_map());
        ed.bottom.open = true;
        ed.bottom.height = 12;
        ed.focus = Focus::Terminal;
        ed.bottom
            .new_tab(Path::new("/tmp"), 40, 6, 200)
            .expect("a shell");
        for (w, h) in [(239u16, 68u16), (100, 30)] {
            let lay = layout(w, h, &ed, None);
            sync_bottom_size(&mut ed, &lay);
            let strip = lay.bottom.expect("a strip");
            let mut buf = rano::render::Buffer::empty(Rect::new(0, 0, w, h));
            draw_frame(&mut buf, &mut ed, &lay);
            let rows = buf.emit(rano::render::Palette::None);
            // B: the row directly above the strip's rule holds a buffer line in its gutter.
            let above = (strip.y as usize).saturating_sub(1);
            assert!(
                rows[above]
                    .trim_start()
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_ascii_digit()),
                "B: the row above the strip's rule is a buffer line at {w}x{h}: {:?}",
                &rows[above][..40.min(rows[above].len())]
            );
            // C: the title bar's reverse band spans the frame — read the styled copy, which
            // keeps the video attributes the plain copy trims.
            let styled = buf.emit(rano::render::Palette::Colour);
            let visible = styled[0].replace("\u{1b}[7m", "").replace("\u{1b}[0m", "");
            assert!(
                visible.chars().count() >= w as usize,
                "C: the title spans the frame at {w}x{h}: {} wide",
                visible.chars().count()
            );
            // Close the panel (it is closed here) and reopen: still spans.
            ed.panel.open = true;
            let lay = layout(w, h, &ed, None);
            sync_bottom_size(&mut ed, &lay);
            let mut buf = rano::render::Buffer::empty(Rect::new(0, 0, w, h));
            draw_frame(&mut buf, &mut ed, &lay);
            let styled = buf.emit(rano::render::Palette::Colour);
            let er = lay.editor.expect("an editor");
            let visible = styled[0].replace("\u{1b}[7m", "").replace("\u{1b}[0m", "");
            assert!(
                visible.chars().count() >= er.width as usize,
                "C: the title spans its column with the panel at {w}x{h}: {}",
                visible.chars().count()
            );
            ed.panel.open = false;
        }
    }

    /// The terminal's bar says what the terminal answers: its own chords, and none of the
    /// editor commands that a focused shell would swallow.
    #[test]
    fn the_terminals_bar_lists_only_the_terminals_keys() {
        let mut ed = editor();
        ed.bottom.open = true;
        ed.bottom.height = 12;
        ed.focus = Focus::Terminal;
        let bar = ed.key_hints();
        let says = |t: &str| bar.iter().any(|(_, title)| title == t);
        assert!(says("New Tab"), "the terminal's own: {bar:?}");
        assert!(says("Close Tab"));
        assert!(says("Text"));
        for not in ["Write Out", "Exit", "Cut", "Where Is", "Paste"] {
            assert!(
                !says(not),
                "an editor command the shell would swallow: {bar:?}"
            );
        }
        // The host's surface keys apply while the shell has focus: the panel and the crossing.
        assert!(says("Panel"), "the host's surface keys: {bar:?}");
        assert!(says("Cross"));
    }

    /// With the panel and the terminal both open, the composer and the key hints are drawn once
    /// each: the frame's bar says the keys, the panel does not repeat them beside the strip.
    #[test]
    fn the_composer_and_the_hints_appear_once_with_the_terminal_open() {
        let mut ed = editor();
        ed.panel.open = true;
        ed.panel.input = "composer-words".to_string();
        ed.focus = Focus::Panel;
        ed.bottom.open = true;
        ed.bottom.height = 12;
        let lay = layout(120, 40, &ed, None);
        let mut buf = rano::render::Buffer::empty(Rect::new(0, 0, 120, 40));
        draw_frame(&mut buf, &mut ed, &lay);
        let rows = buf.emit(rano::render::Palette::None);
        let count = |needle: &str| rows.iter().flat_map(|r| r.match_indices(needle)).count();
        assert_eq!(count("composer-words"), 1, "the composer appears once");
        assert_eq!(count("RET Send"), 1, "the key hints appear once");
        // The one copy is the frame's bar at the foot, which is where a reader looks for keys.
        let bar = rows[rows.len() - 3..].join("");
        assert!(bar.contains("RET Send"), "the bar has the keys");
    }

    /// With the terminal focused, a click in the panel still moves the keyboard: one focus, and
    /// the pointer decides which surface it is.
    #[test]
    fn a_click_in_the_panel_moves_the_keyboard_off_the_terminal() {
        let mut ed = editor();
        ed.panel.open = true;
        ed.bottom.open = true;
        ed.focus = Focus::Terminal;
        let lay = layout(120, 30, &ed, None);
        let panel = lay.panel.expect("a panel column");
        let click = rano::term::MouseEvent {
            kind: MouseKind::Press(MouseButton::Left),
            x: panel.x + 2,
            y: 4,
            mods: rano::term::event::Mods::NONE,
        };
        let mut width = None;
        let mut dragging = false;
        assert!(mouse(&mut ed, &lay, &click, 120, &mut width, &mut dragging));
        assert_eq!(ed.focus, Focus::Panel, "the panel has the keyboard now");
    }

    /// M-i on a frame too narrow for two columns, with the terminal open: the panel takes the
    /// width above the strip, and the strip is still drawn on its reserved rows.
    #[test]
    fn a_narrow_frame_with_the_terminal_open_still_draws_the_strip() {
        let mut ed = editor();
        ed.bottom.open = true;
        let narrow = EDITOR_MIN + PANEL_MIN; // one short of the two-column split
        let _ = ed.handle_key(rano::term::KeyEvent::new(
            KeyCode::Char('i'),
            rano::term::event::Mods::ALT,
        ));
        assert!(ed.panel.open, "M-i opened the panel");
        let lay = layout(narrow, 30, &ed, None);
        let strip = lay.bottom.expect("the strip is still drawn");
        assert_eq!(
            strip.y + strip.height,
            30 - ed.status_rows() as u16,
            "above the status and the key bar at the foot"
        );
        let panel = lay.panel.expect("the panel takes the frame");
        assert_eq!(
            panel.height, 17,
            "the rows above the strip and the frame's foot"
        );
        // Zoomed is the same answer: the strip is drawn, not a blank band.
        ed.panel.zoom = true;
        let lay = layout(120, 30, &ed, None);
        assert!(lay.bottom.is_some(), "zoomed still draws the strip");
        assert_eq!(lay.panel.expect("a zoomed panel").height, 17);
    }

    /// A click in the panel takes the keyboard there, and a click in the text gives it back: the
    /// mouse and the keys must agree about which surface a person is working in.
    #[test]
    fn a_click_in_the_panel_takes_the_keyboard_and_a_click_in_the_text_gives_it_back() {
        let mut ed = editor();
        ed.panel.open = true;
        ed.focus = Focus::Text;
        let lay = layout(120, 30, &ed, None);
        let press = |x: u16| rano::term::MouseEvent {
            kind: MouseKind::Press(MouseButton::Left),
            x,
            y: 4,
            mods: rano::term::event::Mods::NONE,
        };
        let mut width = None;
        let mut dragging = false;
        assert!(mouse(
            &mut ed,
            &lay,
            &press(100),
            120,
            &mut width,
            &mut dragging
        ));
        assert!(
            ed.focus == Focus::Panel,
            "a click in the conversation is the conversation"
        );
        mouse(&mut ed, &lay, &press(10), 120, &mut width, &mut dragging);
        assert!(
            ed.focus == Focus::Text,
            "and a click in the text is the text"
        );
    }
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
        // A flag's value must not be taken for the file.
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
        // `-f` takes no value, so the word after it is the file.
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
    fn edit_is_the_escape_from_a_log_default() {
        // `--edit` takes no value either; the flag only overrides the name rule.
        let a = parse_args(&argv(&["--edit", "server.log"])).expect("parse");
        assert!(a.edit);
        assert_eq!(a.file(), Some("server.log"));
        assert!(!parse_args(&argv(&["server.log"])).expect("parse").edit);
        let a = parse_args(&argv(&["server.log", "--edit"])).expect("parse");
        assert!(a.edit && a.file() == Some("server.log"));
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
        // Each of these is a mistake the user wants told about.
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
