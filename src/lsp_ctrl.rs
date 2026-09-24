//! LSP session control (D4/D5): start/restart the server to match the
//! current buffer, adopt async handshakes, debounce didChange flushes, and
//! surface diagnostics (status line summary + M-D jump).

use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::buffer::Pos;
use crate::editor::Editor;
use crate::editor::widen_zero_width;
use crate::lsp;
use crate::syntax;

impl Editor {
    /// Send the pending didChange once the 300 ms debounce window has
    /// elapsed. The flag is cleared even when no client is attached, so a
    /// later handshake never sends a stale extra change (the adopt path in
    /// lsp_poll re-flags dirty to catch up with the current text). Returns
    /// whether a change actually went out (dirty-draw, D6).
    pub(crate) fn lsp_flush(&mut self, now: Instant) -> bool {
        let bs = self.bs_mut();
        if !bs.lsp_dirty || now < bs.lsp_last_send + Duration::from_millis(300) {
            return false;
        }
        let mut sent = false;
        if let Some(l) = bs.lsp.as_mut() {
            l.change(&bs.buf.text());
            sent = true;
        }
        bs.lsp_dirty = false;
        bs.lsp_last_send = now;
        sent
    }

    // ---------- LSP ----------

    /// Start (or restart) the language server to match the current buffer's
    /// file name and language. Stops it for scratch/unsupported files.
    pub(crate) fn lsp_sync(&mut self) {
        let bs = self.bs_mut();
        let Some(name) = bs.buf.name.clone() else {
            if bs.lsp.take().is_some() {
                bs.lsp_diags.clear();
            }
            return;
        };
        let first_line = bs.buf.row_opt(0).map(|l| l.iter().collect::<String>());
        let Some(lang) = syntax::detect(Some(&name), first_line.as_deref()) else {
            if bs.lsp.take().is_some() {
                bs.lsp_diags.clear();
            }
            return;
        };
        let need = match &bs.lsp {
            Some(c) => c.lang != lang || c.doc_path.as_ref() != Some(&name),
            None => true,
        };
        if !need {
            return;
        }
        if let Some(mut old) = bs.lsp.take() {
            old.shutdown();
        }
        bs.lsp_diags.clear();
        // Languages rano has no server for run highlighting-only: shut any
        // old client down (above) but don't spawn, and don't flash an error
        // for a server that was never supposed to exist.
        if lsp::command_for(lang).is_none() {
            return;
        }
        let dir = name
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."));
        let root = match lang {
            syntax::Lang::Rust => lsp::find_project_root(&dir, "Cargo.toml"),
            syntax::Lang::Go => lsp::find_project_root(&dir, "go.mod"),
            syntax::Lang::Python => lsp::find_project_root(&dir, "pyproject.toml"),
            syntax::Lang::C => lsp::find_project_root(&dir, "compile_commands.json"),
            // JS/TS projects are rooted by package.json, PHP by composer.json,
            // Ruby by the Gemfile.
            syntax::Lang::JavaScript | syntax::Lang::TypeScript | syntax::Lang::Tsx => {
                lsp::find_project_root(&dir, "package.json")
            }
            syntax::Lang::Php => lsp::find_project_root(&dir, "composer.json"),
            syntax::Lang::Ruby => lsp::find_project_root(&dir, "Gemfile"),
            // CL projects are rooted by their .asd files, whose names vary
            // with the system, so there is no marker to walk up for.
            syntax::Lang::Bash | syntax::Lang::Json | syntax::Lang::CommonLisp => dir,
            // The rest have no reliable single marker (or no server at all);
            // the file's own directory is the root.
            _ => dir,
        };
        let text = bs.buf.text();
        // Async handshake: spawn_async returns instantly; lsp_poll adopts
        // the client when the background initialize finishes. Dropping a
        // stale receiver drops its in-flight LspClient (→ Drop::shutdown).
        bs.lsp_starting = Some((
            name.display().to_string(),
            lsp::LspClient::spawn_async(lang, &root, &name, &text),
        ));
    }

    /// Adopt a finished async handshake, then drain LSP notifications into
    /// `lsp_diags`. Rano has a single open document, so the latest report
    /// for it is simply the latest one. Returns whether diagnostics were
    /// replaced (dirty-draw, D6).
    pub(crate) fn lsp_poll(&mut self) -> bool {
        let mut dirty = false;
        let pending = self.bs_mut().lsp_starting.take();
        if let Some((tag, rx)) = pending {
            let started_for = self
                .bs()
                .buf
                .name
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_default();
            match rx.try_recv() {
                Ok(Ok(client)) => {
                    if tag == started_for {
                        // Catch-up: edits made during the handshake were only
                        // flagged; lsp_flush sends the current text.
                        let bs = self.bs_mut();
                        bs.lsp_dirty = true;
                        bs.lsp = Some(client);
                    }
                    // Stale: dropping `client` here runs shutdown.
                }
                Ok(Err(e)) => {
                    if tag == started_for {
                        self.flash(&e);
                    }
                    // Stale failures are silent.
                }
                // Still waiting: keep the handshake in flight.
                Err(mpsc::TryRecvError::Empty) => self.bs_mut().lsp_starting = Some((tag, rx)),
                // Handshake thread died without a result: give up silently.
                Err(mpsc::TryRecvError::Disconnected) => {}
            }
        }
        let events = match self.bs_mut().lsp.as_mut() {
            Some(l) => l.poll(),
            None => Vec::new(),
        };
        for e in events {
            match e {
                lsp::LspEvent::Diagnostics { mut diags, .. } => {
                    let bs = self.bs_mut();
                    widen_zero_width(&mut diags, bs.buf.lines_slice());
                    bs.lsp_diags = diags;
                    dirty = true;
                }
                lsp::LspEvent::Completion(items) => {
                    self.complete_response(items);
                    dirty = true;
                }
                // Server requests are answered inside client.poll().
                lsp::LspEvent::ServerRequest { .. } => {}
            }
        }
        dirty
    }

    /// A status-line summary of the latest diagnostics.
    pub(crate) fn lsp_status(&self) -> Option<String> {
        if self.bs().lsp_diags.is_empty() {
            return None;
        }
        let on_line: Vec<&lsp::Diagnostic> = self
            .bs()
            .lsp_diags
            .iter()
            .filter(|d| d.line == self.bs().cursor.row)
            .collect();
        if !on_line.is_empty() {
            let tag = match on_line[0].severity {
                1 => "E",
                2 => "W",
                3 => "I",
                _ => "H",
            };
            let mut s = format!("[{}] {}", tag, on_line[0].message);
            if on_line.len() > 1 {
                s.push_str(&format!(" (+{} more)", on_line.len() - 1));
            }
            return Some(s);
        }
        let errs = self
            .bs()
            .lsp_diags
            .iter()
            .filter(|d| d.severity == 1)
            .count();
        let warns = self
            .bs()
            .lsp_diags
            .iter()
            .filter(|d| d.severity == 2)
            .count();
        let mut parts = Vec::new();
        if errs > 0 {
            parts.push(format!(
                "{} error{}",
                errs,
                if errs == 1 { "" } else { "s" }
            ));
        }
        if warns > 0 {
            parts.push(format!(
                "{} warning{}",
                warns,
                if warns == 1 { "" } else { "s" }
            ));
        }
        if parts.is_empty() {
            return None;
        }
        Some(format!("LSP: {}", parts.join(", ")))
    }

    /// M-D: move to the next diagnostic on a line strictly below the
    /// cursor, wrapping to the first one. Diags are sorted by (line, col)
    /// defensively; cols are UTF-16 units and may exceed the char count.
    pub(crate) fn jump_next_diag(&mut self) {
        let ds_all = self.all_diags();
        if ds_all.is_empty() {
            self.flash("No diagnostics");
            return;
        }
        let next = ds_all
            .iter()
            .find(|d| d.line > self.bs().cursor.row)
            .unwrap_or(&ds_all[0]);
        let target = Pos {
            row: next.line,
            col: next.col,
        };
        let bs = self.bs_mut();
        bs.cursor = target;
        bs.mark = None;
    }
}
