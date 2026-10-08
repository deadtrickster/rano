//! Every command, by name, and every key that reaches one.
//!
//! **One table of commands** ([`commands`]): a name (`write-out`), a title for
//! people (`Write Out`), a line of documentation, the group it is shown under,
//! and what it does. **Keymaps** ([`Keymaps`]) say which keys reach which
//! command, in the global layer and in each mode (a diff view, the conflict
//! view, a list, a help page, the palette). Dispatch, the bottom bar, the help
//! pages, `M-x` and the which-key cards all read these two — nothing else says
//! what a key does, so nothing else can drift from it.
//!
//! The shape is emacs's: commands are named and documented, keymaps are layered
//! (a mode's map over the global one) and can nest (`M-t t`), `M-x` runs any
//! command by name, and a prefix key held still for a moment shows what can
//! follow it. The keys themselves are nano's where nano has one.

use std::sync::OnceLock;
use std::time::{Duration, Instant};

use crate::conflict::Take;
use crate::keymap::{Entry, Key, Keymap, Lookup, lookup, seq_emacs, where_is};
use crate::keys::KeyOutcome;

use crate::diffview::{ConflictAct, DiffAct, DiffKind};
use crate::editor::Editor;
use crate::picker::PickAct;

/// Where a command is shown: a heading on the help cards and in `M-x`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Group {
    File,
    Edit,
    Mark,
    Search,
    Move,
    Code,
    Buffers,
    Todo,
    View,
    Host,
    Help,
    // The modes' own commands: shown on their cards and bars, not in `M-x`.
    Diff,
    Conflict,
    List,
    Page,
    Palette,
}

impl Group {
    pub fn title(self) -> &'static str {
        match self {
            Group::File => "Files",
            Group::Edit => "Editing",
            Group::Mark => "Mark & clipboard",
            Group::Search => "Search & replace",
            Group::Move => "Moving",
            Group::Code => "Code",
            Group::Buffers => "Buffers",
            Group::Todo => "Todo lists",
            Group::View => "View",
            Group::Host => "Host & updates",
            Group::Help => "Help",
            Group::Diff => "Diff view",
            Group::Conflict => "Conflict view",
            Group::List => "Lists",
            Group::Page => "Help pages",
            Group::Palette => "M-x",
        }
    }

    /// Whether `M-x` offers it: the global commands, not a mode's own.
    pub fn global(self) -> bool {
        !matches!(
            self,
            Group::Diff | Group::Conflict | Group::List | Group::Page | Group::Palette
        )
    }
}

pub struct Command {
    pub name: &'static str,
    pub title: &'static str,
    pub group: Group,
    pub doc: &'static str,
    pub run: fn(&mut Editor),
}

const fn cmd(
    name: &'static str,
    title: &'static str,
    group: Group,
    doc: &'static str,
    run: fn(&mut Editor),
) -> Command {
    Command {
        name,
        title,
        group,
        doc,
        run,
    }
}

/// Every command.
pub fn commands() -> &'static [Command] {
    static ALL: OnceLock<Vec<Command>> = OnceLock::new();
    ALL.get_or_init(|| {
        use Group::*;
        vec![
            // ---- files ----
            cmd(
                "write-out",
                "Write Out",
                File,
                "Save the buffer, asking for the file name.",
                |e| e.start_write(),
            ),
            cmd(
                "insert-file",
                "Read File",
                File,
                "Insert a file's contents at the cursor.",
                |e| e.start_read(),
            ),
            cmd(
                "open-file",
                "Open File",
                File,
                "Open a file (name:line[:col] jumps there).",
                |e| e.start_open(),
            ),
            cmd(
                "make-backup",
                "Make Backup",
                File,
                "Copy the file on disk to a backup name.",
                |e| e.start_backup(),
            ),
            cmd(
                "exit",
                "Exit",
                File,
                "Quit, offering every modified buffer for saving first.",
                |e| e.try_quit(),
            ),
            // ---- editing ----
            cmd(
                "newline",
                "Newline",
                Edit,
                "Break the line, keeping its indent.",
                |e| e.newline(),
            ),
            cmd(
                "indent",
                "Indent",
                Edit,
                "Indent the line (or the marked lines).",
                |e| e.indent_line(),
            ),
            cmd(
                "backspace",
                "Backspace",
                Edit,
                "Delete the character before the cursor.",
                |e| e.backspace(),
            ),
            cmd(
                "delete-char",
                "Delete",
                Edit,
                "Delete the character under the cursor.",
                |e| e.delete_at(),
            ),
            cmd(
                "delete-char-cut",
                "Cut Character",
                Edit,
                "Delete the character under the cursor into the cutbuffer.",
                |e| e.delete_char_cut(),
            ),
            cmd("undo", "Undo", Edit, "Undo the last change.", |e| e.undo()),
            cmd("redo", "Redo", Edit, "Redo the last undone change.", |e| {
                e.redo()
            }),
            cmd(
                "justify",
                "Justify",
                Edit,
                "Re-wrap the paragraph to the window width.",
                |e| e.justify(),
            ),
            cmd(
                "sort-lines",
                "Sort",
                Edit,
                "Sort the marked lines (or the whole buffer).",
                |e| e.sort_lines(),
            ),
            cmd(
                "execute",
                "Execute",
                Edit,
                "Run a shell command and insert its output.",
                |e| e.start_exec(),
            ),
            cmd(
                "filter",
                "Filter",
                Edit,
                "Filter the marked lines through a shell command.",
                |e| e.start_filter(),
            ),
            // ---- mark & clipboard ----
            cmd(
                "set-mark",
                "Set Mark",
                Mark,
                "Set or clear the mark; the region runs from it to the cursor.",
                |e| e.toggle_mark(),
            ),
            cmd(
                "deactivate-mark",
                "Clear Mark",
                Mark,
                "Clear the mark.",
                |e| e.bs_mut().mark = None,
            ),
            cmd(
                "cut",
                "Cut",
                Mark,
                "Cut the line, or the marked region.",
                |e| e.cut(),
            ),
            cmd(
                "copy",
                "Copy",
                Mark,
                "Copy the line, or the marked region.",
                |e| e.copy(),
            ),
            cmd(
                "paste",
                "Paste",
                Mark,
                "Paste the cutbuffer at the cursor.",
                |e| e.paste(),
            ),
            // ---- search ----
            cmd(
                "search-forward",
                "Where Is",
                Search,
                "Search forward.",
                |e| e.start_search(),
            ),
            cmd(
                "search-backward",
                "Where Was",
                Search,
                "Search backward.",
                |e| e.start_search_backward(),
            ),
            cmd(
                "next-match",
                "Next",
                Search,
                "Jump to the next match of the last search.",
                |e| e.next_match(),
            ),
            cmd(
                "prev-match",
                "Previous",
                Search,
                "Jump to the previous match of the last search.",
                |e| e.prev_match(),
            ),
            cmd("replace", "Replace", Search, "Search and replace.", |e| {
                e.start_replace()
            }),
            // ---- moving ----
            cmd("forward-char", "Forward", Move, "Move right.", |e| {
                e.move_right()
            }),
            cmd("backward-char", "Back", Move, "Move left.", |e| {
                e.move_left()
            }),
            cmd("next-line", "Next Line", Move, "Move down a line.", |e| {
                e.move_down()
            }),
            cmd("prev-line", "Prev Line", Move, "Move up a line.", |e| {
                e.move_up()
            }),
            cmd(
                "forward-word",
                "Next Word",
                Move,
                "Move to the next word.",
                |e| e.next_word(),
            ),
            cmd(
                "backward-word",
                "Prev Word",
                Move,
                "Move to the previous word.",
                |e| e.prev_word(),
            ),
            cmd(
                "line-start",
                "Home",
                Move,
                "Move to the start of the line (then its first non-blank).",
                |e| e.move_home(),
            ),
            cmd(
                "line-end",
                "End",
                Move,
                "Move to the end of the line.",
                |e| e.move_end(),
            ),
            cmd("page-up", "Prev Page", Move, "Scroll up a page.", |e| {
                e.page_up(e.text_h)
            }),
            cmd("page-down", "Next Page", Move, "Scroll down a page.", |e| {
                e.page_down(e.text_h)
            }),
            cmd(
                "goto-line",
                "Go To Line",
                Move,
                "Go to a line (and column).",
                |e| e.start_goto(),
            ),
            cmd(
                "match-bracket",
                "To Bracket",
                Move,
                "Jump to the bracket matching the one at the cursor.",
                |e| e.match_bracket(),
            ),
            cmd(
                "show-location",
                "Location",
                Move,
                "Say where the cursor is.",
                |e| e.loc_until = Some(Instant::now() + Duration::from_secs(2)),
            ),
            // ---- code ----
            cmd(
                "goto-definition",
                "Definition",
                Code,
                "Jump to the definition of the symbol at the cursor (LSP).",
                |e| e.jump_definition(),
            ),
            cmd(
                "jump-back",
                "Jump Back",
                Code,
                "Return from the last jump.",
                |e| e.jump_back(),
            ),
            cmd(
                "find-usages",
                "Usages",
                Code,
                "List every usage of the symbol at the cursor (LSP).",
                |e| e.find_usages(),
            ),
            cmd(
                "next-diagnostic",
                "Next Diagnostic",
                Code,
                "Jump to the next error or warning.",
                |e| e.jump_next_diag(),
            ),
            cmd(
                "diff-preview",
                "Diff Preview",
                Code,
                "Show a diff/patch buffer rendered, or a file's merge conflicts side by side.",
                |e| e.toggle_rendered_view(),
            ),
            // ---- buffers ----
            cmd(
                "next-buffer",
                "Next Buffer",
                Buffers,
                "Switch to the next buffer.",
                |e| e.switch_buffer(1),
            ),
            cmd(
                "prev-buffer",
                "Prev Buffer",
                Buffers,
                "Switch to the previous buffer.",
                |e| e.switch_buffer(-1),
            ),
            cmd(
                "buffer-list",
                "Buffer List",
                Buffers,
                "List the open buffers.",
                |e| e.open_buffer_list(),
            ),
            cmd(
                "close-buffer",
                "Close Buffer",
                Buffers,
                "Close this buffer (asks to save a modified one).",
                |e| e.close_buffer(),
            ),
            // ---- todo lists ----
            cmd(
                "todo-tick",
                "Tick",
                Todo,
                "Tick the task on this line; a parent whose children are all done follows.",
                |e| e.todo_toggle(),
            ),
            cmd(
                "todo-section",
                "Tick Section",
                Todo,
                "Give the whole subtree the task's new state.",
                |e| e.todo_cascade(),
            ),
            cmd(
                "todo-decline",
                "Decline",
                Todo,
                "Mark the task declined (the third state).",
                |e| e.todo_decline(),
            ),
            cmd(
                "next-heading",
                "Next Heading",
                Todo,
                "Jump to the next markdown heading.",
                |e| e.todo_next_heading(),
            ),
            cmd(
                "prev-heading",
                "Prev Heading",
                Todo,
                "Jump to the previous markdown heading.",
                |e| e.todo_prev_heading(),
            ),
            // ---- view ----
            cmd(
                "toggle-line-numbers",
                "Line Numbers",
                View,
                "Show or hide the line-number gutter.",
                |e| e.show_line_numbers = !e.show_line_numbers,
            ),
            cmd(
                "toggle-wrap",
                "Wrap",
                View,
                "Turn soft line wrap on or off.",
                |e| e.wrap = !e.wrap,
            ),
            // ---- host & updates ----
            cmd(
                "send-position",
                "Send Position",
                Host,
                "Send the file, cursor and selection to the host (send_command).",
                |e| e.send_position(),
            ),
            cmd(
                "install-update",
                "Install Update",
                Host,
                "Install the newer release the startup check found.",
                |e| e.update_install(),
            ),
            // ---- help ----
            cmd(
                "execute-extended-command",
                "Command",
                Help,
                "Run any command by name (M-x).",
                |e| e.open_palette(),
            ),
            cmd(
                "help",
                "Help",
                Help,
                "The help page: how rano works and its keys.",
                |e| e.open_help_overview(),
            ),
            cmd(
                "describe-bindings",
                "All Keys",
                Help,
                "Every key in effect, grouped.",
                |e| e.open_describe_bindings(),
            ),
            cmd(
                "describe-key",
                "Describe Key",
                Help,
                "Press a key to see what it does.",
                |e| e.start_describe_key(),
            ),
            // ---- diff views ----
            cmd(
                "diff-toggle-split",
                "Split/Unified",
                Diff,
                "Switch between two panels and one.",
                |e| e.diff_act(DiffAct::ToggleSplit),
            ),
            cmd("diff-scroll-up", "Up", Diff, "Scroll up a line.", |e| {
                e.diff_act(DiffAct::Scroll(-1))
            }),
            cmd(
                "diff-scroll-down",
                "Down",
                Diff,
                "Scroll down a line.",
                |e| e.diff_act(DiffAct::Scroll(1)),
            ),
            cmd(
                "diff-page-up",
                "Prev Page",
                Diff,
                "Scroll up a page.",
                |e| e.diff_act(DiffAct::Page(-1)),
            ),
            cmd(
                "diff-page-down",
                "Next Page",
                Diff,
                "Scroll down a page.",
                |e| e.diff_act(DiffAct::Page(1)),
            ),
            cmd("diff-top", "Top", Diff, "Go to the top.", |e| {
                e.diff_act(DiffAct::Top)
            }),
            cmd("diff-bottom", "Bottom", Diff, "Go to the bottom.", |e| {
                e.diff_act(DiffAct::Bottom)
            }),
            cmd(
                "diff-close",
                "Back",
                Diff,
                "Leave the view: back to the text, or to the save question.",
                |e| e.diff_act(DiffAct::Close),
            ),
            cmd(
                "diff-save-anyway",
                "Save Anyway",
                Diff,
                "Overwrite the file on disk with the buffer.",
                |e| e.diff_act(DiffAct::Answer('y')),
            ),
            cmd(
                "diff-dont-save",
                "Don't Save",
                Diff,
                "Leave the file on disk as it is.",
                |e| e.diff_act(DiffAct::Answer('n')),
            ),
            // ---- conflict view ----
            cmd(
                "conflict-next",
                "Next Conflict",
                Conflict,
                "Go to the next conflict.",
                |e| e.diff_act(DiffAct::Conflict(ConflictAct::Move(1))),
            ),
            cmd(
                "conflict-prev",
                "Prev Conflict",
                Conflict,
                "Go to the previous conflict.",
                |e| e.diff_act(DiffAct::Conflict(ConflictAct::Move(-1))),
            ),
            cmd(
                "conflict-compare",
                "Compare",
                Conflict,
                "Cycle ours/theirs, base/ours, base/theirs.",
                |e| e.diff_act(DiffAct::Conflict(ConflictAct::Compare)),
            ),
            cmd(
                "conflict-take-ours",
                "Take Ours",
                Conflict,
                "Resolve this conflict with our side.",
                |e| e.diff_act(DiffAct::Conflict(ConflictAct::Take(Take::Ours))),
            ),
            cmd(
                "conflict-take-theirs",
                "Take Theirs",
                Conflict,
                "Resolve this conflict with their side.",
                |e| e.diff_act(DiffAct::Conflict(ConflictAct::Take(Take::Theirs))),
            ),
            cmd(
                "conflict-take-both",
                "Take Both",
                Conflict,
                "Resolve with both sides, ours first.",
                |e| e.diff_act(DiffAct::Conflict(ConflictAct::Take(Take::OursThenTheirs))),
            ),
            cmd(
                "conflict-take-both-theirs-first",
                "Both, Theirs First",
                Conflict,
                "Resolve with both sides, theirs first.",
                |e| e.diff_act(DiffAct::Conflict(ConflictAct::Take(Take::TheirsThenOurs))),
            ),
            // ---- lists ----
            cmd("list-up", "Up", List, "Select the previous item.", |e| {
                e.picker_act(PickAct::Move(-1))
            }),
            cmd("list-down", "Down", List, "Select the next item.", |e| {
                e.picker_act(PickAct::Move(1))
            }),
            cmd("list-page-up", "Prev Page", List, "Up a page.", |e| {
                e.picker_act(PickAct::Page(-1))
            }),
            cmd("list-page-down", "Next Page", List, "Down a page.", |e| {
                e.picker_act(PickAct::Page(1))
            }),
            cmd("list-top", "First", List, "Select the first item.", |e| {
                e.picker_act(PickAct::Top)
            }),
            cmd("list-bottom", "Last", List, "Select the last item.", |e| {
                e.picker_act(PickAct::Bottom)
            }),
            cmd("list-accept", "Go", List, "Go to the selected item.", |e| {
                e.picker_act(PickAct::Accept)
            }),
            cmd("list-close", "Close", List, "Close the list.", |e| {
                e.picker_act(PickAct::Close)
            }),
            cmd(
                "list-delete",
                "Close Buffer",
                List,
                "Close the selected buffer.",
                |e| e.picker_act(PickAct::Delete),
            ),
            // ---- help pages ----
            cmd("page-scroll-up", "Up", Page, "Scroll up a line.", |e| {
                e.info_scroll(-1)
            }),
            cmd(
                "page-scroll-down",
                "Down",
                Page,
                "Scroll down a line.",
                |e| e.info_scroll(1),
            ),
            cmd("page-prev", "Prev Page", Page, "Scroll up a page.", |e| {
                e.info_page(-1)
            }),
            cmd("page-next", "Next Page", Page, "Scroll down a page.", |e| {
                e.info_page(1)
            }),
            cmd("page-top", "Top", Page, "Go to the top.", |e| {
                e.info_scroll(isize::MIN / 2)
            }),
            cmd("page-bottom", "Bottom", Page, "Go to the bottom.", |e| {
                e.info_scroll(isize::MAX / 2)
            }),
            cmd("page-close", "Close", Page, "Close the page.", |e| {
                e.info = None
            }),
            // ---- the palette ----
            cmd(
                "palette-run",
                "Run",
                Palette,
                "Run the selected command.",
                |e| e.palette_run(),
            ),
            cmd(
                "palette-next",
                "Next",
                Palette,
                "Select the next command.",
                |e| e.palette_move(1),
            ),
            cmd(
                "palette-prev",
                "Previous",
                Palette,
                "Select the previous command.",
                |e| e.palette_move(-1),
            ),
            cmd(
                "palette-backspace",
                "Backspace",
                Palette,
                "Delete the last character typed.",
                |e| e.palette_backspace(),
            ),
            cmd(
                "palette-cancel",
                "Cancel",
                Palette,
                "Close without running anything.",
                |e| e.palette = None,
            ),
        ]
    })
}

/// The command called `name`.
pub fn command(name: &str) -> Option<&'static Command> {
    commands().iter().find(|c| c.name == name)
}

/// The keymaps: the global layer, and one per mode. A mode's maps replace the
/// global one while it is open (a view is modal), most specific first.
#[derive(Debug, Clone)]
pub struct Keymaps {
    pub global: Keymap,
    pub diff: Keymap,
    pub save: Keymap,
    pub patch: Keymap,
    pub conflict: Keymap,
    pub list: Keymap,
    pub buffers: Keymap,
    pub page: Keymap,
    pub palette: Keymap,
}

/// Prefix maps whose card shows at once rather than after a pause: a help key
/// is pressed to be shown something.
pub const SHOW_AT_ONCE: &[&str] = &["help"];

/// How long a prefix key waits before its card shows (emacs's which-key
/// default is a second; a terminal editor is quicker).
pub const CARD_DELAY: Duration = Duration::from_millis(400);

impl Keymaps {
    pub fn standard() -> Keymaps {
        let mut g = Keymap::new("global");
        // Help is a prefix, as emacs's C-h: ^G and F1 open its card.
        for h in ["C-g", "<f1>"] {
            g.bind(&format!("{h} ?"), "help")
                .bind(&format!("{h} {h}"), "help")
                .bind(&format!("{h} k"), "describe-key")
                .bind(&format!("{h} b"), "describe-bindings")
                .bind(&format!("{h} x"), "execute-extended-command");
            g.name_prefix(h, "help");
        }
        g.bind("C-x", "exit")
            .bind("C-o", "write-out")
            .bind("C-r", "insert-file")
            .bind("C-f", "search-forward")
            .bind("C-b", "search-backward")
            .bind("C-\\", "replace")
            .bind("C-k", "cut")
            .bind("C-u", "paste")
            .bind("C-t", "execute")
            .bind("C-j", "justify")
            .bind("C-c", "show-location")
            .bind("C-/", "goto-line")
            .bind("C-a", "set-mark")
            .bind("C-d", "delete-char-cut")
            .bind("C-e", "line-end")
            .bind("C-h", "backspace")
            .bind("C-n", "next-line")
            .bind("C-p", "prev-line")
            .bind("C-<left>", "backward-word")
            .bind("C-<right>", "forward-word")
            .bind("M-x", "execute-extended-command")
            .bind("M-u", "undo")
            .bind("M-e", "redo")
            .bind("M-a", "set-mark")
            .bind("M-6", "copy")
            .bind("M-b", "prev-match")
            .bind("M-f", "next-match")
            .bind("M-]", "match-bracket")
            .bind("M-d", "next-diagnostic")
            .bind("M-.", "goto-definition")
            .bind("M-,", "jump-back")
            .bind("M-?", "find-usages")
            .bind("M-|", "filter")
            .bind("M-p", "diff-preview")
            .bind("M-n", "toggle-line-numbers")
            .bind("M-\\", "toggle-wrap")
            .bind("M-<", "prev-buffer")
            .bind("M->", "next-buffer")
            .bind("M-l", "buffer-list")
            .bind("M-w", "close-buffer")
            .bind("M-s", "send-position")
            .bind("M-v", "install-update")
            // Todo lists are a prefix: the first real submode.
            .bind("M-t t", "todo-tick")
            .bind("M-t c", "todo-section")
            .bind("M-t x", "todo-decline")
            .bind("M-t ]", "next-heading")
            .bind("M-t [", "prev-heading")
            .bind("M-}", "next-heading")
            .bind("M-{", "prev-heading")
            .bind("M-<left>", "backward-word")
            .bind("M-<right>", "forward-word")
            .bind("DEL", "backspace")
            .bind("<delete>", "delete-char")
            .bind("RET", "newline")
            .bind("TAB", "indent")
            .bind("ESC", "deactivate-mark")
            .bind("<left>", "backward-char")
            .bind("<right>", "forward-char")
            .bind("<up>", "prev-line")
            .bind("<down>", "next-line")
            .bind("<home>", "line-start")
            .bind("<end>", "line-end")
            .bind("<pgup>", "page-up")
            .bind("<pgdn>", "page-down")
            .bind("<f2>", "write-out")
            .bind("<f3>", "search-forward")
            .bind("<f4>", "replace")
            .bind("<f5>", "insert-file")
            .bind("<f6>", "execute")
            .bind("<f7>", "make-backup")
            .bind("<f8>", "open-file")
            .bind("<f9>", "sort-lines")
            .bind("<f10>", "justify")
            .bind("<f11>", "goto-line");
        g.name_prefix("M-t", "todo");

        let mut diff = Keymap::new("diff");
        diff.bind("s", "diff-toggle-split")
            .bind("TAB", "diff-toggle-split")
            .bind("<up>", "diff-scroll-up")
            .bind("C-p", "diff-scroll-up")
            .bind("<down>", "diff-scroll-down")
            .bind("C-n", "diff-scroll-down")
            .bind("<pgup>", "diff-page-up")
            .bind("<pgdn>", "diff-page-down")
            .bind("SPC", "diff-page-down")
            .bind("<home>", "diff-top")
            .bind("<end>", "diff-bottom")
            .bind("ESC", "diff-close")
            .bind("RET", "diff-close")
            .bind("q", "diff-close")
            .bind("C-g", "diff-close")
            .bind("C-c", "diff-close");
        let mut save = Keymap::new("save diff");
        save.bind("y", "diff-save-anyway")
            .bind("Y", "diff-save-anyway")
            .bind("n", "diff-dont-save")
            .bind("N", "diff-dont-save")
            .bind("d", "diff-close");
        let mut patch = Keymap::new("patch");
        patch.bind("M-p", "diff-close");
        let mut conflict = Keymap::new("conflicts");
        conflict
            .bind("n", "conflict-next")
            .bind("]", "conflict-next")
            .bind("p", "conflict-prev")
            .bind("[", "conflict-prev")
            .bind("o", "conflict-take-ours")
            .bind("t", "conflict-take-theirs")
            .bind("b", "conflict-take-both")
            .bind("B", "conflict-take-both-theirs-first")
            .bind("c", "conflict-compare")
            .bind("M-p", "diff-close");

        let mut list = Keymap::new("list");
        list.bind("RET", "list-accept")
            .bind("<up>", "list-up")
            .bind("C-p", "list-up")
            .bind("<down>", "list-down")
            .bind("C-n", "list-down")
            .bind("<pgup>", "list-page-up")
            .bind("<pgdn>", "list-page-down")
            .bind("<home>", "list-top")
            .bind("<end>", "list-bottom")
            .bind("ESC", "list-close")
            .bind("C-g", "list-close")
            .bind("C-c", "list-close")
            .bind("C-x", "list-close")
            .bind("M-l", "list-close")
            .bind("M-?", "list-close");
        let mut buffers = Keymap::new("buffers");
        buffers.bind("<delete>", "list-delete");

        let mut page = Keymap::new("help page");
        page.bind("q", "page-close")
            .bind("ESC", "page-close")
            .bind("RET", "page-close")
            .bind("C-g", "page-close")
            .bind("<up>", "page-scroll-up")
            .bind("C-p", "page-scroll-up")
            .bind("<down>", "page-scroll-down")
            .bind("C-n", "page-scroll-down")
            .bind("<pgup>", "page-prev")
            .bind("<pgdn>", "page-next")
            .bind("SPC", "page-next")
            .bind("<home>", "page-top")
            .bind("<end>", "page-bottom");

        let mut palette = Keymap::new("M-x");
        palette
            .bind("RET", "palette-run")
            .bind("TAB", "palette-run")
            .bind("<down>", "palette-next")
            .bind("C-n", "palette-next")
            .bind("<up>", "palette-prev")
            .bind("C-p", "palette-prev")
            .bind("DEL", "palette-backspace")
            .bind("C-h", "palette-backspace")
            .bind("ESC", "palette-cancel")
            .bind("C-g", "palette-cancel")
            .bind("M-x", "palette-cancel");

        Keymaps {
            global: g,
            diff,
            save,
            patch,
            conflict,
            list,
            buffers,
            page,
            palette,
        }
    }
}

/// The global bar, in nano's order: the commands worth a slot, as many as the
/// width fits. Their keys come from the global keymap; `@KEY` is a prefix,
/// shown as its key and its map's name.
pub const BAR: &[&str] = &[
    "@C-g",
    "exit",
    "write-out",
    "insert-file",
    "search-forward",
    "replace",
    "cut",
    "paste",
    "execute",
    "justify",
    "show-location",
    "goto-line",
    "undo",
    "redo",
    "set-mark",
    "copy",
    "execute-extended-command",
    "match-bracket",
    "search-backward",
    "prev-match",
    "next-match",
    "backward-word",
    "forward-word",
    "next-diagnostic",
    "goto-definition",
    "jump-back",
    "find-usages",
    "toggle-line-numbers",
    "toggle-wrap",
    "filter",
    "prev-buffer",
    "next-buffer",
    "buffer-list",
    "close-buffer",
    "diff-preview",
    "send-position",
];

/// A key sequence that has started but not finished: a prefix pressed, or a
/// `describe-key` waiting for its key.
#[derive(Debug, Default)]
pub struct Pending {
    pub keys: Vec<Key>,
    pub since: Option<Instant>,
    /// `describe-key`: the next whole sequence is described, not run.
    pub describe: bool,
    /// The loop drew the card for this prefix (so it redraws once, when the
    /// delay passes).
    pub card_shown: bool,
}

/// What the dispatcher decided, owned so the keymaps are not borrowed while a
/// command runs.
enum Decision {
    Run(String),
    Prefix,
    Unbound,
}

impl Editor {
    /// The keymaps in effect, most specific first.
    ///
    /// A host's maps ([`Self::push_keymap`]) come first, last pushed first,
    /// over whatever the editor's own stack is at the moment — so a host's
    /// chord works in a list or a diff as well as in the text.
    pub(crate) fn active_keymaps(&self) -> Vec<&Keymap> {
        let mut stack: Vec<&Keymap> = self.host_keymaps.iter().rev().collect();
        stack.extend(self.editor_keymaps());
        stack
    }

    /// The editor's own maps in effect, without a host's: the global layer or
    /// the open mode's. What decides whether an unbound character is typed,
    /// which a host map stacked on top must not change.
    pub(crate) fn editor_keymaps(&self) -> Vec<&Keymap> {
        let k = &self.keymaps;
        if self.palette.is_some() {
            return vec![&k.palette];
        }
        if self.info.is_some() {
            return vec![&k.page];
        }
        if let Some(kind) = self.diff_kind() {
            let own = match kind {
                DiffKind::Save => &k.save,
                DiffKind::Patch => &k.patch,
                DiffKind::Conflict => &k.conflict,
            };
            return vec![own, &k.diff];
        }
        if self.picker.is_some() {
            return if self.picker_is_buffers() {
                vec![&k.buffers, &k.list]
            } else {
                vec![&k.list]
            };
        }
        vec![&k.global]
    }

    /// One key through the keymaps: a command runs, a prefix waits for the next
    /// key (its card shows after [`CARD_DELAY`]), and an unbound printable
    /// character is typed — into the text, or into the `M-x` query.
    ///
    /// A command name no editor command has can only have come from a host's
    /// map; it is returned for the host to run rather than reported as
    /// missing.
    pub(crate) fn dispatch_key(&mut self, key: Key) -> KeyOutcome {
        let mut seq = std::mem::take(&mut self.pending.keys);
        let describing = std::mem::take(&mut self.pending.describe);
        self.pending.since = None;
        seq.push(key);
        let decision = match lookup(&self.active_keymaps(), &seq) {
            Lookup::Command(c) => Decision::Run(c.to_string()),
            Lookup::Prefix(_) => Decision::Prefix,
            Lookup::Unbound => Decision::Unbound,
        };
        match decision {
            Decision::Prefix => {
                self.pending.keys = seq;
                self.pending.since = Some(Instant::now());
                self.pending.describe = describing;
            }
            Decision::Run(name) if describing => self.show_description(&seq, Some(&name)),
            Decision::Run(name) if command(&name).is_none() => return KeyOutcome::Host(name),
            Decision::Run(name) => self.run_command(&name),
            Decision::Unbound if describing => self.show_description(&seq, None),
            Decision::Unbound => {
                if seq.len() == 1
                    && let Some(c) = key.printable()
                {
                    if self.palette.is_some() {
                        self.palette_type(c);
                    } else if self
                        .editor_keymaps()
                        .first()
                        .is_some_and(|m| m.name == "global")
                    {
                        self.insert_char(c);
                    } else {
                        // A mode (a list, a diff, help) types nothing.
                        return KeyOutcome::Unhandled;
                    }
                } else if seq.len() > 1 {
                    // C-g and ESC are how a prefix is abandoned, as in emacs.
                    if key == Key::ctrl('g') || key == Key::plain(crossterm::event::KeyCode::Esc) {
                        self.flash("Quit");
                    } else {
                        self.flash(&format!("{} is undefined", seq_emacs(&seq)));
                    }
                } else {
                    return KeyOutcome::Unhandled;
                }
            }
        }
        KeyOutcome::Handled
    }

    /// Run a command by name, and remember it for `M-x`'s ordering.
    pub(crate) fn run_command(&mut self, name: &str) {
        let Some(c) = command(name) else {
            self.flash(&format!("No command {name}"));
            return;
        };
        // M-x itself is how commands are reached, not one to be offered.
        if c.group.global() && c.name != "execute-extended-command" {
            self.command_history.retain(|n| *n != c.name);
            self.command_history.insert(0, c.name);
        }
        (c.run)(self);
    }

    /// The prefix card to show now, if any: the map a pending prefix leads to,
    /// once it has waited [`CARD_DELAY`] (or at once for help).
    pub(crate) fn pending_card(&self) -> Option<(&Keymap, Vec<Key>)> {
        let since = self.pending.since?;
        match lookup(&self.active_keymaps(), &self.pending.keys) {
            Lookup::Prefix(m)
                if SHOW_AT_ONCE.contains(&m.name.as_str()) || since.elapsed() >= CARD_DELAY =>
            {
                Some((m, self.pending.keys.clone()))
            }
            _ => None,
        }
    }

    /// Whether a prefix is waiting for its card's delay to pass, so the loop
    /// knows to come back and draw it.
    pub(crate) fn card_due(&self) -> Option<Duration> {
        let since = self.pending.since?;
        CARD_DELAY.checked_sub(since.elapsed())
    }

    /// The bar's entries for what is in effect: `(key, title)`, in the
    /// configured notation.
    /// A pending prefix shows what can follow it; the global layer shows
    /// [`BAR`]; a mode shows its own maps.
    pub(crate) fn bar_items(&self) -> Vec<(String, String)> {
        let nano = self.config.nano_keys;
        let note = |seq: &[Key]| crate::help::notation(nano, seq);
        let stack = self.active_keymaps();
        if !self.pending.keys.is_empty()
            && let Lookup::Prefix(m) = lookup(&stack, &self.pending.keys)
        {
            return entries_of(m, nano);
        }
        if self
            .editor_keymaps()
            .first()
            .is_some_and(|m| m.name == "global")
        {
            return BAR
                .iter()
                .filter_map(|name| {
                    if let Some(prefix) = name.strip_prefix('@') {
                        let seq = crate::keymap::parse_seq(prefix)?;
                        let Lookup::Prefix(m) = lookup(&stack, &seq) else {
                            return None;
                        };
                        return Some((note(&seq), prefix_title(m)));
                    }
                    let seq = where_is(&stack, name).into_iter().next()?;
                    Some((note(&seq), command(name)?.title.to_string()))
                })
                .collect();
        }
        let mut out: Vec<(String, String)> = Vec::new();
        let mut seen: Vec<&str> = Vec::new();
        for m in &stack {
            for (k, e) in m.entries() {
                if let Entry::Command(c) = e
                    && !seen.contains(&c.as_str())
                    && lookup(&stack, &[*k]) == Lookup::Command(c)
                {
                    seen.push(c);
                    if let Some(cmd) = command(c) {
                        out.push((note(std::slice::from_ref(k)), cmd.title.to_string()));
                    }
                }
            }
        }
        out
    }
}

/// A prefix as an entry's title: its map's name, capitalised, and `…` for
/// "more keys follow".
pub(crate) fn prefix_title(m: &Keymap) -> String {
    let mut c = m.name.chars();
    let first: String = c
        .next()
        .map(|f| f.to_uppercase().collect())
        .unwrap_or_default();
    format!("{first}{}…", c.as_str())
}

/// A map's entries as `(key, title)`, one per command (its first key), a
/// prefix shown as its name with `…`.
pub(crate) fn entries_of(m: &Keymap, nano: bool) -> Vec<(String, String)> {
    let key = |k: &Key| crate::help::notation(nano, std::slice::from_ref(k));
    let mut out = Vec::new();
    let mut seen: Vec<&str> = Vec::new();
    for (k, e) in m.entries() {
        match e {
            Entry::Command(c) if !seen.contains(&c.as_str()) => {
                seen.push(c);
                if let Some(cmd) = command(c) {
                    out.push((key(k), cmd.title.to_string()));
                }
            }
            Entry::Prefix(p) => out.push((key(k), prefix_title(p))),
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all_maps(k: &Keymaps) -> Vec<&Keymap> {
        vec![
            &k.global,
            &k.diff,
            &k.save,
            &k.patch,
            &k.conflict,
            &k.list,
            &k.buffers,
            &k.page,
            &k.palette,
        ]
    }

    #[test]
    fn every_bound_command_exists_and_every_name_is_unique() {
        let mut names: Vec<&str> = commands().iter().map(|c| c.name).collect();
        let n = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), n, "a command name is used twice");
        let k = Keymaps::standard();
        for m in all_maps(&k) {
            for (seq, c) in m.flatten() {
                assert!(
                    command(&c).is_some(),
                    "{} in {} names no command {c}",
                    seq_emacs(&seq),
                    m.name
                );
            }
        }
    }

    /// Every global command is reachable by a key or by `M-x`, and the bar's
    /// entries all have a key.
    #[test]
    fn the_bar_names_bound_global_commands() {
        let k = Keymaps::standard();
        for name in BAR {
            if let Some(p) = name.strip_prefix('@') {
                let seq = crate::keymap::parse_seq(p).unwrap();
                assert!(
                    matches!(lookup(&[&k.global], &seq), Lookup::Prefix(_)),
                    "{p}"
                );
                continue;
            }
            let c = command(name).unwrap_or_else(|| panic!("{name}"));
            assert!(c.group.global(), "{name}");
            assert!(
                !where_is(&[&k.global], name).is_empty(),
                "{name} has no key"
            );
        }
    }

    #[test]
    fn every_mode_command_has_a_key_in_its_mode() {
        let k = Keymaps::standard();
        let maps = all_maps(&k);
        for c in commands().iter().filter(|c| !c.group.global()) {
            assert!(
                maps.iter()
                    .any(|m| m.flatten().iter().any(|(_, n)| n == c.name)),
                "{} is bound nowhere",
                c.name
            );
        }
    }
}
