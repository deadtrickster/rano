//! The update check, in the editor: one request at startup, and the install
//! behind a confirmation.
//!
//! Shape follows the other pollers (`lsp_poll`, `exec_poll`): a `start_*` that
//! spawns a thread and hands back a receiver, and a `*_poll` that is called every
//! loop iteration and reports whether anything visible changed. The check never
//! blocks a frame — the request is made off-thread and collected later.
//!
//! # Why the notice appears when you quit
//!
//! Installing replaces the running executable, and the running executable is
//! still mapped. Replacing the file is fine on Unix (the rename swaps the
//! directory entry; the old inode stays alive until this process exits), so the
//! current session keeps working — but a notice that vanishes before it can be
//! read is worse than no notice. So the offer is announced when the editor is
//! about to exit, where it is the last thing on the status line and cannot be
//! overwritten by a redraw. `RANO_UPDATE_ON_EXIT=0` turns that off for a run.

use crate::update::{self, Update};
use std::sync::mpsc::{Receiver, TryRecvError};

/// A check is in flight, or one has answered.
#[derive(Debug, Default)]
pub struct UpdateCheck {
    rx: Option<Receiver<Option<Update>>>,
    /// The offer, once a check found a newer release.
    pub found: Option<Update>,
    /// What went wrong, for the one place that wants to say so. A failed check
    /// is not an editor problem, so nothing shows this unless asked.
    pub error: Option<String>,
}

impl UpdateCheck {
    /// Start the check on a thread. No-op when the check is disabled, so the
    /// decision to make no request is visible at the call site.
    pub fn start(enabled: bool) -> Self {
        if !enabled {
            return Self::default();
        }
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            // One request, on a thread that does nothing else. The result is the
            // whole payload of the thread.
            let got = update::check_with(true, &update::Curl);
            let _ = tx.send(got);
        });
        Self {
            rx: Some(rx),
            found: None,
            error: None,
        }
    }

    /// Collect the answer if it has arrived. Returns whether anything changed,
    /// so a caller can decide to redraw.
    pub fn poll(&mut self) -> bool {
        let Some(rx) = &self.rx else {
            return false;
        };
        match rx.try_recv() {
            Ok(Some(u)) => {
                self.found = Some(u);
                self.rx = None;
                true
            }
            Ok(None) => {
                self.rx = None;
                false
            }
            Err(TryRecvError::Empty) => false,
            Err(TryRecvError::Disconnected) => {
                self.error = Some("the update check thread died".to_string());
                self.rx = None;
                false
            }
        }
    }

    /// Is a check still running?
    pub fn checking(&self) -> bool {
        self.rx.is_some()
    }
}

/// Install the offered update, replacing this executable.
///
/// Called only from an accepted confirmation — nothing here asks. On success the
/// new binary is in place and takes effect on the next start; this process keeps
/// running the old image, which is what lets the message say "restart".
pub fn install(u: &Update) -> Result<String, String> {
    update::install(u)?;
    Ok(format!("Updated to {}. Restart rano to use it.", u.latest))
}

/// Whether to announce on exit. On by default; `RANO_UPDATE_ON_EXIT=0` off.
pub fn announce_on_exit() -> bool {
    match std::env::var("RANO_UPDATE_ON_EXIT") {
        Ok(v) => !matches!(v.trim(), "0" | "false" | "no" | "off"),
        Err(_) => true,
    }
}

impl crate::editor::Editor {
    /// Start the startup check, if it is enabled. Called from `run()` rather
    /// than `Editor::new` so no test makes a network request by constructing an
    /// editor — the check is a property of RUNNING, not of being.
    pub(crate) fn start_update_check(&mut self) {
        let on = crate::update::enabled(
            self.config.autoupdate,
            std::env::var("RANO_AUTOUPDATE").ok().as_deref(),
        );
        self.update = UpdateCheck::start(on);
    }

    /// Collect the check's answer, and SAY SO. Returns whether anything visible
    /// changed.
    ///
    /// Flashing here is the whole point of the poller: without it the notice was
    /// only printed on exit, so the session said nothing and `M-V` was
    /// undiscoverable — found by running the binary and watching the status line
    /// stay empty, not by reading the code.
    pub(crate) fn update_poll(&mut self) -> bool {
        if !self.update.poll() {
            return false;
        }
        if let Some(u) = &self.update.found {
            let msg = u.message();
            self.flash(&msg);
        }
        true
    }

    /// Whether the exit notice should be printed. Separate from `found` so the
    /// environment can silence it for a run without disabling the check.
    pub(crate) fn update_installable_on_exit(&self) -> bool {
        announce_on_exit()
    }

    /// M-V: install the offered update, or say why not.
    pub(crate) fn update_install(&mut self) {
        let Some(u) = self.update.found.clone() else {
            let msg = if self.update.checking() {
                "Checking for updates...".to_string()
            } else {
                "rano is up to date".to_string()
            };
            self.flash(&msg);
            return;
        };
        match install(&u) {
            Ok(msg) => {
                self.update.found = None;
                self.flash(&msg);
            }
            Err(e) => self.flash(&format!("Update failed: {e}")),
        }
    }
}
