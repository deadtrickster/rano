//! The panel without the `agent` feature: the same vocabulary (re-exported from
//! [`crate::conversation`]) and a surface that says by name that this build has no letibot
//! panel. Nothing here opens a socket, holds a conversation or draws a pane; the C-] cross
//! still trades the text and the terminal, because that cycle is not the panel's to take away.

use std::path::PathBuf;

use crate::letibot::{Frame, MarkOutcome, NoDaemon, Session};
use crate::marks::Outcome;
use crate::render::Line;
use crate::send::SendEvent;
use crate::{ask::AskEvent, editor::Editor};

pub use crate::conversation::{
    Act, Chip, Markset, PanelState, Reply, Row, folder_for, remember_in, remembered_in,
};

/// The shell of the panel: the fields a host reads, all of them inert. `open` stays false,
/// so no frame is ever laid out for a conversation this build does not have.
#[derive(Debug, Default)]
pub struct Panel {
    pub open: bool,
    pub zoom: bool,
    pub folder: PathBuf,
    pub moved: Option<PathBuf>,
    pub session: String,
    pub title: String,
    pub state: PanelState,
    pub said: String,
    pub rows: Vec<Row>,
    pub input: String,
    pub cursor: usize,
    pub scroll: usize,
    pub follow: bool,
    pub sessions: Vec<Session>,
    pub pick: usize,
    pub state_file: Option<PathBuf>,
    pub chips: Vec<Chip>,
    pub outcomes: Vec<MarkOutcome>,
    pub tools_open: bool,
}

impl Panel {
    pub fn attached(&self) -> bool {
        false
    }

    pub fn open(&mut self, _folder: PathBuf) {
        self.said = "this build has no letibot panel".to_string();
    }

    pub fn close(&mut self) {
        self.open = false;
    }

    pub fn folder_moved(&mut self, _folder: PathBuf) {}

    pub fn follow(&mut self) {}

    pub fn start(&mut self) {
        self.said = "this build has no letibot panel".to_string();
    }

    pub fn no_daemon(&mut self, why: &NoDaemon) {
        self.said = why.to_string();
    }

    /// No acts, ever: the host never opens a connection for a surface that is not there.
    pub fn take_acts(&mut self) -> Vec<Act> {
        Vec::new()
    }

    pub fn apply(&mut self, _frame: &Frame) -> bool {
        false
    }

    /// Typing goes nowhere: there is no composer.
    pub fn insert(&mut self, _c: char) {}

    pub fn send(&mut self) {
        self.said = "this build has no letibot panel".to_string();
    }

    /// The place is not attached anywhere: the caller's own route (the embedding's
    /// `send_command`, say) is the one that still works.
    pub fn attach_place(&mut self, _send: &SendEvent, _lang: &str) -> bool {
        false
    }

    pub fn ask(&mut self, _ask: &AskEvent, _lang: &str) -> bool {
        false
    }

    pub fn remember_outcomes(&mut self, _taken: &[(String, Outcome)]) {}

    pub fn take_marks(&mut self) -> Vec<Markset> {
        Vec::new()
    }

    pub fn keep_marks(&mut self, _kept: Vec<Markset>) {}

    pub fn turn_line(&self, _w: usize) -> Option<Line> {
        None
    }

    pub fn tick_turn(&mut self) -> bool {
        false
    }

    pub fn build(&mut self, _width: usize) {}

    pub fn rebuilt(&mut self) {}

    pub fn lines(&self) -> &[Line] {
        &[]
    }
}

impl Editor {
    /// M-i names what this build does not have rather than opening an empty column.
    pub fn panel_toggle(&mut self) {
        self.flash("this build has no letibot panel");
    }

    pub fn panel_open(&mut self) {
        self.flash("this build has no letibot panel");
    }

    /// C-] with no panel in the build: the text and the terminal trade the keyboard, which is
    /// the whole cycle that is left. With no strip open either, the key says why nothing moved.
    pub fn panel_cross(&mut self) {
        if self.bottom.open {
            self.focus = match self.focus {
                crate::editor::Focus::Terminal => crate::editor::Focus::Text,
                _ => crate::editor::Focus::Terminal,
            };
        } else {
            self.flash("this build has no letibot panel");
        }
    }

    pub fn panel_zoom(&mut self) {}

    pub fn panel_refuse(&mut self, said: &str) {
        self.flash(said);
    }

    /// The folder a file belongs to is not the panel's secret: the ask flow uses it too.
    pub fn panel_folder(&self) -> PathBuf {
        let name = self.bs().buf.name.clone();
        let cwd = std::env::current_dir().unwrap_or_default();
        folder_for(name.as_deref(), &cwd)
    }

    /// No conversation to answer into: the ask takes the caller's other route.
    pub fn panel_ask(&mut self, _ask: &AskEvent) -> bool {
        false
    }

    pub fn panel_attach(&mut self, _send: &SendEvent) -> bool {
        false
    }

    pub(crate) fn panel_watch_folder(&mut self) -> bool {
        false
    }

    pub fn panel_send(&mut self) {
        self.flash("this build has no letibot panel");
    }

    pub fn panel_new(&mut self) {}

    pub fn panel_leave(&mut self) {}

    pub fn panel_interrupt(&mut self) {}

    pub fn panel_follow(&mut self) {}

    pub fn panel_up(&mut self) {}

    pub fn panel_down(&mut self) {}

    pub fn panel_page_up(&mut self) {}

    pub fn panel_page_down(&mut self) {}

    pub fn panel_key(&mut self, _key: &crate::term::KeyEvent) -> bool {
        false
    }
}
