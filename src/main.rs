//! The `rano` binary: arguments, the terminal, and the event loop.

use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

use rano::buffer::{Buffer, Pos};
use rano::editor::KeyOutcome;
use rano::editor::{Area, Editor};
use rano::expand_tilde;
use rano::panel::Act;
use rano::render::{self, Line, Palette, Rect, Style};
use rano::send_ctrl::split_position;
use rano::style::Role;
use rano::term::{self, Event, KeyCode, MouseButton, MouseKind, Terminal};
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
    // A picture is drawn as placeholder cells a terminal may fill with garbage, so
    // M-P is refused by name where the protocol is not kitty.
    ed.images = matches!(
        terminal.features().proto,
        rano::term::Proto::Kitty | rano::term::Proto::KittyPassthrough
    );
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
        dirty |= ed.set_area(lay.editor.unwrap_or(Rect::new(0, 0, w, h)).into());
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
                    // A key the editor has no use for is the host's: the three the design puts
                    // above both programs, and nothing else.
                    if ed.handle_key(k) == KeyOutcome::Unhandled {
                        host_key(&mut ed, &k);
                    }
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
        // Nothing at all could open a panel here: the line goes where the reader is already
        // looking, on the status row, and the panel stays shut.
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
}

/// How a `w`×`h` frame splits between the text and the panel. `want` is the width the operator
/// dragged the border to, clamped every frame so a resize cannot leave a surface twice as small as
/// its minimum.
fn layout(w: u16, h: u16, ed: &Editor, want: Option<u16>) -> Layout {
    let whole = Rect::new(0, 0, w, h);
    if !ed.panel.open || w == 0 || h == 0 {
        return Layout {
            editor: Some(whole),
            ..Layout::default()
        };
    }
    if ed.panel.zoom || w < EDITOR_MIN + 1 + PANEL_MIN {
        // One surface takes the frame: two columns that fit neither is the squeeze the design
        // refuses.
        return if ed.panel.focus {
            Layout {
                panel: Some(whole),
                ..Layout::default()
            }
        } else {
            Layout {
                editor: Some(whole),
                ..Layout::default()
            }
        };
    }
    let default = (u32::from(w) * 2 / 5).min(u32::from(PANEL_MAX)) as u16;
    let pw = want.unwrap_or(default).clamp(PANEL_MIN, w - EDITOR_MIN - 1);
    let ew = w - pw - 1;
    Layout {
        editor: Some(Rect::new(0, 0, ew, h)),
        border: Some(Rect::new(ew, 0, 1, h)),
        panel: Some(Rect::new(ew + 1, 0, pw, h)),
    }
}

/// Draw the frame the layout asked for, and say where the caret is.
fn draw_frame(buf: &mut render::Buffer, ed: &mut Editor, lay: &Layout) -> Option<(u16, u16)> {
    let mut cursor = None;
    if let Some(area) = lay.editor {
        cursor = ui::draw_in(buf, area, ed);
    }
    if let Some(area) = lay.border {
        // The rule between the two, the same glyph the editor's own sidebar draws.
        let rule = Line::styled("\u{2502}", Style::of(Role::Faint));
        for y in area.y..area.y + area.height {
            buf.set_line(area.x, y, &rule, 1);
        }
    }
    if let Some(area) = lay.panel {
        ed.panel.build(area.width as usize);
        let hints = if ed.panel.focus {
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
    cursor
}

/// The panel as its widget sees it: the facts it draws, and nothing it would have to know.
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
        focus: panel.focus,
        hints,
    }
}

/// The three keys the design puts above both programs, pressed where rano binds none of them: the
/// crossing, the side surface and the zoom. A key the editor did not want and this does not know is
/// dropped, as it always was.
fn host_key(ed: &mut Editor, key: &rano::term::KeyEvent) {
    let alt = key.mods.alt();
    let ctrl = key.mods.ctrl();
    match key.code {
        KeyCode::Char(']') if ctrl && !alt => ed.panel_cross(),
        KeyCode::Char('i') if alt && !ctrl => ed.panel_toggle(),
        KeyCode::Char('m') if alt && !ctrl => ed.panel_zoom(),
        _ => {}
    }
}

/// A mouse event in the frame: a click in the panel takes the keyboard there, a held border resizes
/// it, and anything else is the editor's.
fn mouse(
    ed: &mut Editor,
    lay: &Layout,
    m: &rano::term::MouseEvent,
    w: u16,
    width: &mut Option<u16>,
    dragging: &mut bool,
) -> bool {
    let inside = |r: Option<Rect>| r.is_some_and(|r| r.contains(m.x, m.y));
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
        if matches!(m.kind, MouseKind::Press(MouseButton::Left)) {
            *dragging = true;
            return true;
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
                ed.panel.focus = true;
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
    ed.panel.focus = false;
    ed.handle_mouse(*m)
}

#[cfg(test)]
mod panel_tests {
    use super::*;
    use rano::buffer::Buffer;
    use rano::config::Config;

    fn editor() -> Editor {
        Editor::new(Buffer::new(), Config::default())
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
        assert_eq!(lay.border, Some(Rect::new(71, 0, 1, 30)));
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
        ed.panel.focus = true;
        let lay = layout(90, 30, &ed, None);
        assert_eq!(lay.panel, Some(Rect::new(0, 0, 90, 30)));
        assert_eq!(lay.editor, None, "the text is not drawn beside it");
        // The keyboard to the text: now it is the panel that is off the screen, still open.
        ed.panel.focus = false;
        let lay = layout(90, 30, &ed, None);
        assert_eq!(lay.editor, Some(Rect::new(0, 0, 90, 30)));
        assert_eq!(lay.panel, None);
        // At exactly the two minimums and the rule it splits again.
        ed.panel.focus = true;
        let lay = layout(EDITOR_MIN + 1 + PANEL_MIN, 30, &ed, None);
        assert!(lay.editor.is_some() && lay.panel.is_some());
    }

    /// `alt-m` zooms the focused surface on a terminal wide enough to split, and zooms it back.
    #[test]
    fn zoom_takes_the_frame_and_gives_it_back() {
        let mut ed = editor();
        ed.panel.open = true;
        ed.panel.focus = true;
        ed.panel_zoom();
        let lay = layout(120, 30, &ed, None);
        assert_eq!(lay.panel, Some(Rect::new(0, 0, 120, 30)));
        assert_eq!(lay.editor, None);
        ed.panel_cross();
        let lay = layout(120, 30, &ed, None);
        assert_eq!(lay.editor, Some(Rect::new(0, 0, 120, 30)));
        assert_eq!(lay.panel, None);
        ed.panel_zoom();
        let lay = layout(120, 30, &ed, None);
        assert!(lay.editor.is_some() && lay.panel.is_some(), "split again");
    }

    /// The three host keys: the crossing, the side surface and the zoom. Every other key the editor
    /// did not want changes nothing.
    #[test]
    fn the_host_keys_are_the_three_the_design_shares() {
        let mut ed = editor();
        let key =
            |code: KeyCode, mods: rano::term::event::Mods| rano::term::KeyEvent { code, mods };
        let alt_i = key(KeyCode::Char('i'), rano::term::event::Mods::ALT);
        host_key(&mut ed, &alt_i);
        assert!(ed.panel.open && ed.panel.focus, "alt-i opens it, focused");
        let cross = key(KeyCode::Char(']'), rano::term::event::Mods::CTRL);
        host_key(&mut ed, &cross);
        assert!(!ed.panel.focus, "ctrl-] crosses to the text");
        host_key(&mut ed, &cross);
        assert!(ed.panel.focus, "and back");
        let alt_m = key(KeyCode::Char('m'), rano::term::event::Mods::ALT);
        host_key(&mut ed, &alt_m);
        assert!(ed.panel.zoom, "alt-m zooms");
        host_key(&mut ed, &alt_i);
        assert!(!ed.panel.open, "alt-i hides it again");
        // A key nobody claimed: nothing happens, which is what the binary did before.
        host_key(
            &mut ed,
            &key(KeyCode::Char('z'), rano::term::event::Mods::ALT),
        );
        assert!(!ed.panel.open && !ed.panel.zoom);
    }

    /// A click in the panel takes the keyboard there, and a click in the text gives it back: the
    /// mouse and the keys must agree about which surface a person is working in.
    #[test]
    fn a_click_in_the_panel_takes_the_keyboard_and_a_click_in_the_text_gives_it_back() {
        let mut ed = editor();
        ed.panel.open = true;
        ed.panel.focus = false;
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
            ed.panel.focus,
            "a click in the conversation is the conversation"
        );
        mouse(&mut ed, &lay, &press(10), 120, &mut width, &mut dragging);
        assert!(!ed.panel.focus, "and a click in the text is the text");
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
