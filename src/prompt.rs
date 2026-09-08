//! Prompt line (E4): the Prompt/PromptKind model, its key handling, word
//! motion, history, tilde expansion, path completion and the status-bar
//! prompt labels.

use std::fs;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::editor::Editor;
use crate::search_ctrl::ReplaceState;

#[derive(Debug, Clone, Copy)]
pub enum PromptKind {
    WriteName,
    ReadName,
    OpenName,
    Search,
    ReplaceFind,
    ReplaceWith,
    GoTo,
    Exec,
    FilterCmd,
    BackupName,
    ConfirmSave,
    ConfirmOverwrite,
    ReplaceAsk,
}

#[derive(Debug)]
pub struct Prompt {
    pub kind: PromptKind,
    pub text: String,
    pub cursor: usize,
}

pub(crate) fn prompt_label(kind: PromptKind) -> &'static str {
    match kind {
        PromptKind::WriteName => "File Name: ",
        PromptKind::ReadName => "Read File: ",
        PromptKind::OpenName => "Open: ",
        PromptKind::Search => "Search: ",
        PromptKind::ReplaceFind => "Find: ",
        PromptKind::ReplaceWith => "Replace With: ",
        PromptKind::GoTo => "Go To Line: ",
        PromptKind::Exec => "Execute: ",
        PromptKind::FilterCmd => "Filter: ",
        PromptKind::BackupName => "Backup Name: ",
        PromptKind::ConfirmSave => "Save modified buffer? (y, n, or ^G to cancel) ",
        PromptKind::ConfirmOverwrite => "File exists, overwrite? (y or n) ",
        PromptKind::ReplaceAsk => "Replace? (y, n, a, q) ",
    }
}

impl Editor {
    // ---------- prompt history ----------

    /// The history list recorded for a prompt kind, if any (E4b): Search
    /// queries, Exec/Filter commands, and file names.
    fn history_for(&self, kind: PromptKind) -> Option<&Vec<String>> {
        match kind {
            PromptKind::Search => Some(&self.search_hist),
            PromptKind::Exec | PromptKind::FilterCmd => Some(&self.exec_hist),
            PromptKind::WriteName
            | PromptKind::ReadName
            | PromptKind::OpenName
            | PromptKind::BackupName => Some(&self.file_hist),
            _ => None,
        }
    }

    fn history_for_mut(&mut self, kind: PromptKind) -> Option<&mut Vec<String>> {
        match kind {
            PromptKind::Search => Some(&mut self.search_hist),
            PromptKind::Exec | PromptKind::FilterCmd => Some(&mut self.exec_hist),
            PromptKind::WriteName
            | PromptKind::ReadName
            | PromptKind::OpenName
            | PromptKind::BackupName => Some(&mut self.file_hist),
            _ => None,
        }
    }

    /// Record a submitted prompt line: skip empty input, dedupe consecutive
    /// repeats. Called from the Enter arm before dispatch.
    fn record_history(&mut self, kind: PromptKind, text: &str) {
        if text.is_empty() {
            return;
        }
        if let Some(h) = self.history_for_mut(kind)
            && h.last().map(|s| s.as_str()) != Some(text)
        {
            h.push(text.to_string());
        }
    }

    pub(crate) fn handle_prompt_key(&mut self, mut p: Prompt, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let cancel = key.code == KeyCode::Esc || (ctrl && matches!(key.code, KeyCode::Char('g')));

        match p.kind {
            PromptKind::ConfirmSave => {
                if cancel {
                    return;
                }
                if let KeyCode::Char(c) = key.code {
                    self.answer_save(c);
                    return;
                }
                self.prompt = Some(p);
                return;
            }
            PromptKind::ConfirmOverwrite => {
                if cancel {
                    self.pending_write = None;
                    return;
                }
                if let KeyCode::Char(c) = key.code {
                    match c {
                        'y' | 'Y' => {
                            if let Some(path) = self.pending_write.take() {
                                self.save_to(path);
                            }
                        }
                        'n' | 'N' => {
                            self.pending_write = None;
                        }
                        _ => {}
                    }
                    return;
                }
                self.prompt = Some(p);
                return;
            }
            PromptKind::ReplaceAsk => {
                if cancel {
                    self.replace = None;
                    self.flash("Replace cancelled");
                    return;
                }
                if let KeyCode::Char(c) = key.code {
                    self.answer_replace_ask(c);
                    return;
                }
                return;
            }
            _ => {}
        }

        match key.code {
            KeyCode::Enter => {
                self.record_history(p.kind, &p.text);
                self.hist_idx = None;
                self.hist_draft.clear();
                match p.kind {
                    PromptKind::WriteName => self.do_write(p.text),
                    PromptKind::ReadName => self.do_read(p.text),
                    PromptKind::OpenName => {
                        // Read failed: keep the prompt open for editing.
                        if !self.open_file(&p.text) {
                            self.prompt = Some(p);
                        }
                    }
                    PromptKind::Search => self.do_search(p.text),
                    PromptKind::ReplaceFind => {
                        if !p.text.is_empty() {
                            self.replace = Some(ReplaceState {
                                find: p.text,
                                with: String::new(),
                            });
                            self.prompt = Some(Prompt {
                                kind: PromptKind::ReplaceWith,
                                text: String::new(),
                                cursor: 0,
                            });
                        }
                    }
                    PromptKind::ReplaceWith => {
                        if let Some(st) = self.replace.as_mut() {
                            st.with = p.text.clone();
                        }
                        self.replace_count = 0;
                        self.next_replace_ask();
                    }
                    PromptKind::GoTo => self.do_goto(&p.text),
                    PromptKind::Exec => self.do_exec(&p.text),
                    PromptKind::FilterCmd => self.do_filter(&p.text),
                    PromptKind::BackupName => self.do_backup(p.text),
                    _ => {}
                }
            }
            // E4d: modifier-specific arms MUST sit above the modifier-blind
            // Char/Left/Right arms below, or M-b/M-f type letters instead of
            // moving by words.
            KeyCode::Char('b') if alt => {
                prompt_prev_word(&mut p);
                self.prompt = Some(p);
            }
            KeyCode::Char('f') if alt => {
                prompt_next_word(&mut p);
                self.prompt = Some(p);
            }
            // F5: search-modifier toggles, Search prompt ONLY, and above the
            // modifier-blind arms so M-c/M-r toggle instead of typing.
            KeyCode::Char('c') if alt && matches!(p.kind, PromptKind::Search) => {
                self.search_case_sensitive = !self.search_case_sensitive;
                self.flash(&format!(
                    "Case sensitive: {}",
                    if self.search_case_sensitive {
                        "on"
                    } else {
                        "off"
                    }
                ));
                self.prompt = Some(p);
            }
            KeyCode::Char('r') if alt && matches!(p.kind, PromptKind::Search) => {
                self.search_regex = !self.search_regex;
                self.flash(&format!(
                    "Regex: {}",
                    if self.search_regex { "on" } else { "off" }
                ));
                self.prompt = Some(p);
            }
            KeyCode::Left if ctrl => {
                prompt_prev_word(&mut p);
                self.prompt = Some(p);
            }
            KeyCode::Right if ctrl => {
                prompt_next_word(&mut p);
                self.prompt = Some(p);
            }
            // E4b: Up cycles into the history (newest first); Down walks
            // back forward, and past the newest entry restores the text that
            // was being edited when cycling began.
            KeyCode::Up => {
                let entry = self.history_for(p.kind).and_then(|h| {
                    if h.is_empty() {
                        return None;
                    }
                    let i = match self.hist_idx {
                        None => h.len() - 1,
                        Some(i) => i.saturating_sub(1),
                    };
                    Some((i, h[i].clone()))
                });
                if let Some((i, entry)) = entry {
                    if self.hist_idx.is_none() {
                        self.hist_draft = p.text.clone();
                    }
                    self.hist_idx = Some(i);
                    p.text = entry;
                    p.cursor = p.text.chars().count();
                }
                self.prompt = Some(p);
            }
            KeyCode::Down => {
                if let Some(i) = self.hist_idx {
                    let next = self.history_for(p.kind).and_then(|h| h.get(i + 1).cloned());
                    if let Some(entry) = next {
                        self.hist_idx = Some(i + 1);
                        p.text = entry;
                    } else {
                        self.hist_idx = None;
                        p.text = std::mem::take(&mut self.hist_draft);
                    }
                    p.cursor = p.text.chars().count();
                }
                self.prompt = Some(p);
            }
            // E4c: Tab completes file paths (only in the path-ish prompts;
            // other kinds fall through to the re-store arm below).
            KeyCode::Tab
                if matches!(
                    p.kind,
                    PromptKind::WriteName
                        | PromptKind::ReadName
                        | PromptKind::OpenName
                        | PromptKind::BackupName
                        | PromptKind::FilterCmd
                ) =>
            {
                match complete_path(&p.text) {
                    Some((common, options)) => {
                        if common.chars().count() > p.text.chars().count() {
                            p.text = common;
                        }
                        p.cursor = p.text.chars().count();
                        if options.len() > 1 {
                            let shown: Vec<String> = options.iter().take(5).cloned().collect();
                            self.flash(&shown.join("  "));
                        }
                    }
                    None => self.flash("No match"),
                }
                self.prompt = Some(p);
            }
            KeyCode::Backspace if p.cursor > 0 => {
                prompt_backspace(&mut p);
                self.prompt = Some(p);
            }
            KeyCode::Left if p.cursor > 0 => {
                p.cursor -= 1;
                self.prompt = Some(p);
            }
            KeyCode::Right if p.cursor < p.text.chars().count() => {
                p.cursor += 1;
                self.prompt = Some(p);
            }
            KeyCode::Home => {
                p.cursor = 0;
                self.prompt = Some(p);
            }
            KeyCode::End => {
                p.cursor = p.text.chars().count();
                self.prompt = Some(p);
            }
            KeyCode::Char(c) if !c.is_control() => {
                prompt_insert(&mut p, c);
                self.prompt = Some(p);
            }
            _ => {
                if !cancel {
                    self.prompt = Some(p);
                } else {
                    self.hist_idx = None;
                    self.hist_draft.clear();
                }
            }
        }
    }
}

// Prompt text editing helpers. `Prompt::cursor` is a CHAR index while
// String::{insert, remove} take BYTE indices, so editing goes through a
// Vec<char> to stay multibyte-safe.
fn prompt_insert(p: &mut Prompt, c: char) {
    let idx = p.cursor.min(p.text.chars().count());
    let mut v: Vec<char> = p.text.chars().collect();
    v.insert(idx, c);
    p.cursor = idx + 1;
    p.text = v.into_iter().collect();
}

fn prompt_backspace(p: &mut Prompt) {
    if p.cursor == 0 {
        return;
    }
    let mut v: Vec<char> = p.text.chars().collect();
    v.remove(p.cursor - 1);
    p.cursor -= 1;
    p.text = v.into_iter().collect();
}

// E4d: word motion inside prompts, over the same word chars (alphanumeric
// or '_') as the buffer's prev_word/next_word. Forward skips whitespace,
// then the word (or a single non-word char), then any trailing whitespace;
// backward is symmetric without the trailing skip.
fn prompt_next_word(p: &mut Prompt) {
    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    let v: Vec<char> = p.text.chars().collect();
    let mut c = p.cursor.min(v.len());
    while c < v.len() && v[c].is_whitespace() {
        c += 1;
    }
    if c < v.len() && is_word(v[c]) {
        while c < v.len() && is_word(v[c]) {
            c += 1;
        }
    } else if c < v.len() {
        c += 1;
    }
    while c < v.len() && v[c].is_whitespace() {
        c += 1;
    }
    p.cursor = c;
}

fn prompt_prev_word(p: &mut Prompt) {
    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    let v: Vec<char> = p.text.chars().collect();
    let mut c = p.cursor.min(v.len());
    while c > 0 && v[c - 1].is_whitespace() {
        c -= 1;
    }
    if c > 0 && is_word(v[c - 1]) {
        while c > 0 && is_word(v[c - 1]) {
            c -= 1;
        }
    } else {
        c = c.saturating_sub(1);
    }
    p.cursor = c;
}

/// E4a: expand a leading `~` or `~/` to $HOME. `~user` is intentionally not
/// expanded; without $HOME the input is returned unchanged.
pub(crate) fn expand_tilde(p: &str) -> String {
    if p == "~" {
        return std::env::var("HOME").unwrap_or_else(|_| p.to_string());
    }
    if let Some(rest) = p.strip_prefix("~/")
        && let Ok(home) = std::env::var("HOME")
    {
        return format!("{home}/{rest}");
    }
    p.to_string()
}

/// E4c: complete a file-path prefix against the filesystem. Splits at the
/// last '/', reads that directory ("." when there is none), and collects
/// matching entries — hidden ones only when the typed part starts with '.',
/// directories suffixed with '/', sorted. Returns the longest common prefix
/// (directory part re-attached) plus the candidates.
pub(crate) fn complete_path(prefix: &str) -> Option<(String, Vec<String>)> {
    let (dir, file) = match prefix.rfind('/') {
        Some(i) => (&prefix[..=i], &prefix[i + 1..]),
        None => ("", prefix),
    };
    let rd = fs::read_dir(if dir.is_empty() { "." } else { dir }).ok()?;
    let mut names: Vec<String> = rd
        .flatten()
        .filter(|e| {
            let n = e.file_name().to_string_lossy().into_owned();
            n.starts_with(file) && (file.starts_with('.') || !n.starts_with('.'))
        })
        .map(|e| {
            let n = e.file_name().to_string_lossy().into_owned();
            if e.path().is_dir() {
                format!("{n}/")
            } else {
                n
            }
        })
        .collect();
    names.sort();
    if names.is_empty() {
        return None;
    }
    let mut common: String = names[0].clone();
    for n in &names[1..] {
        let len = common
            .chars()
            .zip(n.chars())
            .take_while(|(a, b)| a == b)
            .count();
        common = common.chars().take(len).collect();
    }
    Some((format!("{dir}{common}"), names))
}
