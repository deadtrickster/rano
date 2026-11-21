//! Prompt line: the Prompt/PromptKind model, its key handling, word motion,
//! history, tilde expansion, path completion and the labels.

use std::fs;

use crate::term::{KeyCode, KeyEvent};

use crate::editor::Editor;
use crate::search_ctrl::ReplaceState;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptKind {
    WriteName,
    ReadName,
    OpenName,
    /// M-g: a folder to list (the dired).
    DirName,
    Search,
    ReplaceFind,
    ReplaceWith,
    GoTo,
    /// `M-x Mode`: the buffer's language, chosen rather than detected.
    SetMode,
    Exec,
    FilterCmd,
    BackupName,
    ConfirmSave,
    ConfirmClose,
    ConfirmOverwrite,
    /// The file changed on disk since it was read; `pending_write` holds it.
    ConfirmExternal,
    /// The save's directory does not exist; `pending_write` and `pending_dir` hold
    /// the write and the directory to make.
    ConfirmMkdir,
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
        PromptKind::DirName => "Directory: ",
        PromptKind::Search => "Search: ",
        PromptKind::ReplaceFind => "Find: ",
        PromptKind::ReplaceWith => "Replace With: ",
        PromptKind::GoTo => "Go To Line: ",
        PromptKind::SetMode => "Mode: ",
        PromptKind::Exec => "Execute: ",
        PromptKind::FilterCmd => "Filter: ",
        PromptKind::BackupName => "Backup Name: ",
        PromptKind::ConfirmSave => "Save modified buffer? (y, n, or ^G to cancel) ",
        PromptKind::ConfirmClose => "Save before closing? (y, n, or ^G to cancel) ",
        PromptKind::ConfirmOverwrite => "File exists, overwrite? (y or n) ",
        PromptKind::ConfirmExternal => {
            "File changed on disk since it was read. Overwrite? (y, n, d for diff) "
        }
        PromptKind::ConfirmMkdir => "Directory does not exist; create it? (y or n) ",
        PromptKind::ReplaceAsk => "Replace? (y, n, a, q) ",
    }
}

impl Editor {
    // ---------- prompt history ----------

    /// The history list recorded for a prompt kind: search queries, exec commands,
    /// file names.
    fn history_for(&self, kind: PromptKind) -> Option<&Vec<String>> {
        match kind {
            PromptKind::Search => Some(&self.search_hist),
            PromptKind::Exec | PromptKind::FilterCmd => Some(&self.exec_hist),
            PromptKind::WriteName
            | PromptKind::ReadName
            | PromptKind::OpenName
            | PromptKind::BackupName
            | PromptKind::DirName => Some(&self.file_hist),
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
            | PromptKind::BackupName
            | PromptKind::DirName => Some(&mut self.file_hist),
            _ => None,
        }
    }

    /// Record a submitted prompt line: skip empty input, dedupe consecutive repeats.
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
        let ctrl = key.mods.ctrl();
        let alt = key.mods.alt();
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
            PromptKind::ConfirmClose => {
                if cancel {
                    return;
                }
                if let KeyCode::Char(c) = key.code {
                    self.answer_close(c);
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
            PromptKind::ConfirmExternal => {
                if cancel {
                    self.cancel_external_save();
                    return;
                }
                if let KeyCode::Char(c) = key.code {
                    self.answer_external(c);
                    return;
                }
                self.prompt = Some(p);
                return;
            }
            PromptKind::ConfirmMkdir => {
                if cancel {
                    self.answer_mkdir('n');
                    return;
                }
                if let KeyCode::Char(c) = key.code {
                    self.answer_mkdir(c);
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
                    PromptKind::DirName => self.do_dir(p.text),
                    PromptKind::OpenName => {
                        // **The picker's Enter.** An empty filter opens the folder itself as a
                        // listing ("open ." without typing it); a Tab-selected entry opens that
                        // entry, built from the filter's directory part; anything else is the
                        // typed path, `name:line[:col]` jumping as on the command line. A failed
                        // read keeps the prompt open for editing.
                        let picked = self.path_hints.as_ref().and_then(|(k, t, e, sel)| {
                            (k == &p.kind && t == &p.text).then(|| e.get(*sel).cloned())?
                        });
                        let text = p.text.trim().to_string();
                        let opened = if text.is_empty() {
                            let dir = self
                                .bs()
                                .buf
                                .name
                                .as_deref()
                                .and_then(std::path::Path::parent)
                                .map(|p| p.to_path_buf())
                                .unwrap_or_else(|| std::path::PathBuf::from("."));
                            self.open_dir(&dir)
                        } else if let Some(hint) = picked {
                            // The filter's directory part, or "" when none was typed: the picker
                            // lists "." then, so the entry's own name is the path.
                            let d = text.rfind('/').map(|i| &text[..=i]).unwrap_or("");
                            self.open_file(&format!("{d}{}", hint.name))
                        } else {
                            match crate::send_ctrl::split_position(&expand_tilde(&text)) {
                                Some((name, line, col)) => {
                                    self.open_at(std::path::Path::new(&name), line, col).is_ok()
                                }
                                None => self.open_file(&text),
                            }
                        };
                        if !opened {
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
                    PromptKind::SetMode => self.do_setmode(&p.text),
                    PromptKind::Exec => self.do_exec(&p.text),
                    PromptKind::FilterCmd => self.do_filter(&p.text),
                    PromptKind::BackupName => self.do_backup(p.text),
                    _ => {}
                }
            }
            // Modifier-specific arms sit above the modifier-blind Char/Left/Right
            // arms, or M-b/M-f type letters instead of moving by words.
            KeyCode::Char('b') if alt => {
                prompt_prev_word(&mut p);
                self.prompt = Some(p);
            }
            KeyCode::Char('f') if alt => {
                prompt_next_word(&mut p);
                self.prompt = Some(p);
            }
            // Search-modifier toggles, Search prompt only, above the modifier-blind
            // arms so M-c/M-r toggle instead of typing.
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
            // Up cycles into the history, newest first; Down walks forward, and past
            // the newest restores the text being edited when cycling began.
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
            // Tab completes file paths, in the path-ish prompts only.
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
                // **Tab completes, then selects.** The first press takes the common prefix the
                // entries share; when there is nothing left to complete, Tab walks the picker's
                // list, so `*.md` + Tab + Tab + Enter is the whole gesture.
                let completes = complete_path(&p.text)
                    .filter(|(common, _)| common.chars().count() > p.text.chars().count());
                let completed = completes.is_some();
                if let Some((common, _)) = completes {
                    p.text = common;
                    p.cursor = p.text.chars().count();
                }
                self.prompt = Some(p);
                // With the prompt back in place (the refresh reads it), keep the picker's list
                // fresh, and walk it: Tab completes first, and when there is nothing left to
                // complete it selects, so `*.md` + Tab + Tab + Enter is the whole gesture.
                self.refresh_path_hints();
                if !completed && let Some((_, _, e, sel)) = &mut self.path_hints {
                    if e.len() > 1 {
                        *sel = (*sel + 1) % e.len();
                    } else if e.is_empty() {
                        self.flash("No match");
                    }
                }
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
            // A chord is not text: C-g cancels (see `cancel` above), and a control chord the
            // prompt does not bind must not arrive as the letter it was pressed with — that is how
            // C-g typed a `g` into a search prompt behind the reader's back.
            KeyCode::Char(c) if !c.is_control() && !ctrl => {
                prompt_insert(&mut p, c);
                self.prompt = Some(p);
            }
            _ => {
                if !cancel {
                    self.prompt = Some(p);
                } else {
                    self.hist_idx = None;
                    self.hist_draft.clear();
                    // A file name asked for by a quit or a close: cancelling it abandons
                    // that too, or the next ^O would quit behind the user's back.
                    if matches!(p.kind, PromptKind::WriteName) {
                        self.quit_after_save = false;
                        self.close_after_save = false;
                    }
                }
            }
        }
    }
}

// `Prompt::cursor` is a CHAR index while String takes BYTE indices, so editing
// goes through a Vec<char> to stay multibyte-safe.
pub(crate) fn prompt_insert(p: &mut Prompt, c: char) {
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

// Word motion inside prompts, over the same word chars as the buffer's: skip
// whitespace, then the word (or one non-word char), then trailing whitespace.
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

/// Expand a leading `~` or `~/` to $HOME. `~user` is deliberately not expanded.
pub fn expand_tilde(p: &str) -> String {
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

/// Complete a file-path prefix against the filesystem: split at the last '/', read
/// that directory, collect matching entries (hidden ones only when the typed part
/// starts with '.'), and return the common prefix with the candidates.
/// One entry of the file picker's list: a name, and whether it is a folder to descend into.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathHint {
    pub name: String,
    pub dir: bool,
}

/// Does `name` match a fuzzy filter: every character of `filter`, in order, case-insensitively,
/// somewhere in `name`? A prefix match is the same walk from the start, so it ranks first by
/// construction (the caller sorts prefix matches before the rest).
fn fuzzy(name: &str, filter: &str) -> bool {
    let lower = name.to_lowercase();
    let mut it = lower.chars();
    'f: for want in filter.to_lowercase().chars() {
        for got in it.by_ref() {
            if got == want {
                continue 'f;
            }
        }
        return false;
    }
    true
}

/// Does `name` match a glob filter (`*.md`, `?.txt`)? `*` is any run, `?` one character,
/// case-insensitively; everything else matches itself.
fn glob(name: &str, filter: &str) -> bool {
    let n: Vec<char> = name.to_lowercase().chars().collect();
    let f: Vec<char> = filter.to_lowercase().chars().collect();
    // The classic two-pointer walk: `*` may eat nothing, and on failure the last star retries
    // with one more character eaten.
    let (mut ni, mut fi) = (0usize, 0usize);
    let (mut star, mut mark) = (None::<usize>, 0usize);
    while ni < n.len() {
        if fi < f.len() && (f[fi] == '?' || f[fi] == n[ni]) {
            ni += 1;
            fi += 1;
        } else if fi < f.len() && f[fi] == '*' {
            star = Some(fi);
            mark = ni;
            fi += 1;
        } else if let Some(s) = star {
            fi = s + 1;
            mark += 1;
            ni = mark;
        } else {
            return false;
        }
    }
    while fi < f.len() && f[fi] == '*' {
        fi += 1;
    }
    fi == f.len()
}

/// **The file picker's list** for the filter as typed: the entries of the directory the filter
/// names (the current one when it does not), matched by glob when it has a wildcard and fuzzily
/// otherwise. Folders always match - they are how the picker moves - and sort first, the way the
/// dired lists them.
pub(crate) fn pick_path(filter: &str) -> Option<(String, Vec<PathHint>)> {
    let (dir, file) = match filter.rfind('/') {
        Some(i) => (&filter[..=i], &filter[i + 1..]),
        None => ("", filter),
    };
    let rd = fs::read_dir(if dir.is_empty() {
        ".".to_string()
    } else {
        expand_tilde(dir)
    })
    .ok()?;
    let mut out: Vec<PathHint> = Vec::new();
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if !file.is_empty() || !name.starts_with('.') {
            let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
            let wildcard = file.contains('*') || file.contains('?');
            // **Folders obey the filter too**: the picker is a match list, not matches mixed
            // with furniture - `*.md` shows markdown files and folders whose own names match,
            // and descending is what an empty filter is for.
            let hit = if wildcard {
                glob(&name, file)
            } else {
                fuzzy(&name, file)
            };
            if hit {
                out.push(PathHint {
                    name: if is_dir { format!("{name}/") } else { name },
                    dir: is_dir,
                });
            }
        }
    }
    // Folders first, then names: the dired's order, so a picker and a listing read the same.
    out.sort_by(|a, b| {
        b.dir
            .cmp(&a.dir)
            .then_with(|| fuzzy_rank(&b.name, file).cmp(&fuzzy_rank(&a.name, file)))
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    if out.is_empty() {
        return None;
    }
    let mut common = out[0].name.clone();
    for n in &out[1..] {
        let len = common
            .chars()
            .zip(n.name.chars())
            .take_while(|(a, b)| a == b)
            .count();
        common = common.chars().take(len).collect();
    }
    Some((format!("{dir}{common}"), out))
}

/// A prefix match ranks before a scattered one: 0 for "the filter starts the name", 1 otherwise.
fn fuzzy_rank(name: &str, filter: &str) -> u8 {
    u8::from(!name.to_lowercase().starts_with(&filter.to_lowercase()))
}

pub(crate) fn complete_path(prefix: &str) -> Option<(String, Vec<String>)> {
    let (dir, file) = match prefix.rfind('/') {
        Some(i) => (&prefix[..=i], &prefix[i + 1..]),
        None => ("", prefix),
    };
    let rd = fs::read_dir(if dir.is_empty() {
        ".".to_string()
    } else {
        expand_tilde(dir)
    })
    .ok()?;
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
