//! A minimal LSP (Language Server Protocol) client: JSON-RPC 2.0 over the
//! server process's stdio. A background reader thread parses the byte stream
//! and routes responses to per-request channels and notifications to a
//! channel the editor drains each event-loop tick.
//!
//! Only the subset rano needs is implemented: initialize/initialized,
//! didOpen, didChange (full-document sync), publishDiagnostics, shutdown/exit.

use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};

use crate::syntax::Lang;

/// A single diagnostic reported by the server (0-based positions).
#[derive(Clone, Debug, PartialEq)]
pub struct Diagnostic {
    pub line: usize,
    pub col: usize,
    /// End of the range (exclusive), in UTF-16 units on `line`.
    pub end_col: usize,
    pub message: String,
    /// 1 = error, 2 = warning, 3 = info, 4 = hint.
    pub severity: u64,
}

/// One completion choice from `textDocument/completion`. `insert` is the
/// text to place in the buffer (label, insertText or textEdit.newText, with
/// snippet placeholders stripped); `kind` is the LSP CompletionItemKind
/// (0 = unknown), used for the popup's type tag.
#[derive(Clone, Debug)]
pub struct CompletionItem {
    pub label: String,
    pub kind: u64,
    pub insert: String,
    /// Server-defined ranking key (sortText): the popup sorts by it, the
    /// server's JSON order is not its ranked order.
    pub sort: String,
    /// Text the server matched against (filterText): usually the label, but
    /// can differ (e.g. `self::` items).
    pub filter: String,
}

/// Events the reader thread delivers to the editor.
pub enum LspEvent {
    Diagnostics {
        uri: String,
        diags: Vec<Diagnostic>,
    },
    /// The result of a `textDocument/completion` request.
    Completion(Vec<CompletionItem>),
    /// A request the *server* sent us (has a method and an id). The client
    /// must answer it; rano answers with a null result, which is sufficient
    /// for the requests rust-analyzer/gopls/bash-language-server send.
    ServerRequest {
        id: u64,
        method: String,
    },
}

pub struct LspClient {
    child: Child,
    stdin: Box<dyn Write + Send>,
    next_id: u64,
    pending: Arc<Mutex<HashMap<u64, mpsc::Sender<Value>>>>,
    /// ids of in-flight completion requests. Completion is fire-and-forget,
    /// so the reader needs this set to tell a completion response (or an
    /// error response) apart from a late answer to a timed-out synchronous
    /// request — both look "unclaimed", and conflating them desyncs the
    /// editor's completion queue.
    completion_ids: Arc<Mutex<HashSet<u64>>>,
    rx: mpsc::Receiver<LspEvent>,
    root_uri: String,
    pub doc_uri: Option<String>,
    pub doc_path: Option<PathBuf>,
    doc_version: i32,
    initialized: bool,
    pub lang: Lang,
}

/// Resolve an LSP server binary: search `PATH`, then `~/.local/bin` and
/// `~/.cargo/bin` (where rustup/gopls/bash-language-server typically live).
/// Falls back to the bare name so a "not found" error is still meaningful.
fn find_bin(name: &str) -> String {
    use std::path::Path;
    if let Ok(path) = std::env::var("PATH") {
        for dir in std::env::split_paths(&path) {
            let cand = dir.join(name);
            if cand.is_file() {
                return cand.to_string_lossy().into_owned();
            }
        }
    }
    if let Some(home) = std::env::var_os("HOME") {
        for sub in ["local/bin", "cargo/bin"] {
            let cand = Path::new(&home).join(sub).join(name);
            if cand.is_file() {
                return cand.to_string_lossy().into_owned();
            }
        }
    }
    name.to_string()
}

/// The server command for `lang`, or `None` when rano knows of no language
/// server for it — those buffers run highlighting-only, with no spawn
/// attempt and no error flash.
pub(crate) fn command_for(lang: Lang) -> Option<(String, Vec<String>)> {
    let (name, args): (&str, &[&str]) = match lang {
        // The rustup `rust-analyzer` component speaks LSP over stdio by
        // default (no --stdio flag; that's the standalone release binary).
        Lang::Rust => ("rust-analyzer", &[]),
        Lang::Go => ("gopls", &[]),
        // v5.x CLI: `bash-language-server start` listens on stdin/stdout.
        Lang::Bash => ("bash-language-server", &["start"]),
        Lang::Python => ("pylsp", &[]),
        Lang::C => ("clangd", &[]),
        Lang::Json => ("vscode-json-language-server", &["--stdio"]),
        // cl-lsp (roswell-installable) is the closest thing to a standard
        // Common Lisp server; when it's absent the spawn fails and the
        // buffer simply runs without LSP.
        Lang::CommonLisp => ("cl-lsp", &[]),
        Lang::JavaScript | Lang::TypeScript | Lang::Tsx => {
            ("typescript-language-server", &["--stdio"])
        }
        Lang::Markdown => ("marksman", &[]),
        Lang::Toml => ("taplo", &[]),
        Lang::Yaml => ("yaml-language-server", &["--stdio"]),
        Lang::Html => ("vscode-html-language-server", &["--stdio"]),
        Lang::Css => ("vscode-css-language-server", &["--stdio"]),
        Lang::Lua => ("lua-language-server", &[]),
        Lang::Ruby => ("ruby-lsp", &[]),
        Lang::Php => ("intelephense", &["--stdio"]),
        Lang::Java => ("jdtls", &[]),
        Lang::Sql => ("sqls", &[]),
        Lang::Clojure => ("clojure-lsp", &[]),
        // No standard server worth spawning: make, dockerfiles, ini-style
        // configs, diffs, and the emacs/scheme lisps all run without LSP.
        Lang::Make | Lang::Dockerfile | Lang::Ini | Lang::Diff | Lang::Elisp | Lang::Scheme => {
            return None;
        }
    };
    Some((find_bin(name), args.iter().map(|s| s.to_string()).collect()))
}

fn language_id(lang: Lang) -> &'static str {
    match lang {
        Lang::Rust => "rust",
        Lang::Go => "go",
        Lang::Bash => "shellscript",
        Lang::Python => "python",
        Lang::C => "c",
        Lang::Json => "json",
        Lang::CommonLisp => "lisp",
        Lang::JavaScript => "javascript",
        Lang::TypeScript => "typescript",
        Lang::Tsx => "typescriptreact",
        Lang::Markdown => "markdown",
        Lang::Toml => "toml",
        Lang::Yaml => "yaml",
        Lang::Html => "html",
        Lang::Css => "css",
        Lang::Lua => "lua",
        Lang::Ruby => "ruby",
        Lang::Php => "php",
        Lang::Java => "java",
        Lang::Make => "make",
        Lang::Dockerfile => "dockerfile",
        Lang::Ini => "ini",
        Lang::Diff => "diff",
        Lang::Elisp => "elisp",
        Lang::Scheme => "scheme",
        Lang::Sql => "sql",
        Lang::Clojure => "clojure",
    }
}

/// Walk up from `start` looking for a directory containing `marker`; fall
/// back to `start` itself when none is found.
pub fn find_project_root(start: &Path, marker: &str) -> PathBuf {
    let mut dir = start.to_path_buf();
    loop {
        if dir.join(marker).exists() {
            return dir;
        }
        if !dir.pop() {
            return start.to_path_buf();
        }
    }
}

pub(crate) fn path_to_uri(path: &Path) -> String {
    let s = path.to_string_lossy();
    if cfg!(windows) {
        format!("file:///{}", s.replace('\\', "/"))
    } else {
        format!("file://{}", s)
    }
}

/// If `RANO_LSP_RAW` names a file, append every LSP message there for
/// debugging (one JSON object per line, prefixed with direction).
fn raw_log(dir: &str, value: &Value) {
    use std::io::Write as _;
    if let Ok(path) = std::env::var("RANO_LSP_RAW")
        && let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
    {
        let _ = writeln!(f, "[{dir}] {value}");
    }
}

fn write_message(w: &mut dyn Write, value: &Value) -> std::io::Result<()> {
    raw_log("->", value);
    let bytes = serde_json::to_vec(value)?;
    write!(w, "Content-Length: {}\r\n\r\n", bytes.len())?;
    w.write_all(&bytes)?;
    w.flush()
}

fn read_message(r: &mut dyn BufRead) -> Option<Value> {
    let mut content_length: Option<usize> = None;
    loop {
        let mut line = String::new();
        r.read_line(&mut line).ok()?;
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some(v) = line.strip_prefix("Content-Length:") {
            content_length = v.trim().parse().ok();
        }
    }
    let len = content_length?;
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf).ok()?;
    let v = serde_json::from_slice(&buf).ok()?;
    raw_log("<-", &v);
    Some(v)
}

fn parse_diagnostics(v: &Value) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    if let Some(arr) = v.as_array() {
        for d in arr {
            let range = &d["range"];
            let line = range["start"]["line"].as_u64().unwrap_or(0) as usize;
            let col = range["start"]["character"].as_u64().unwrap_or(0) as usize;
            let end_col = range["end"]["character"].as_u64().unwrap_or(0) as usize;
            let message = d["message"].as_str().unwrap_or("").to_string();
            let severity = d["severity"].as_u64().unwrap_or(1);
            out.push(Diagnostic {
                line,
                col,
                end_col,
                message,
                severity,
            });
        }
    }
    out
}

/// Convert a char column on `line` to LSP's UTF-16 code-unit column.
pub fn utf16_col(line: &[char], col_chars: usize) -> usize {
    line.iter().take(col_chars).map(|c| c.len_utf16()).sum()
}

/// Inverse of `utf16_col`: a UTF-16 code-unit column back to a char column,
/// clamped to the line length.
pub fn utf16_to_char(line: &[char], col_utf16: usize) -> usize {
    let mut u = 0usize;
    for (i, c) in line.iter().enumerate() {
        if u >= col_utf16 {
            return i;
        }
        u += c.len_utf16();
    }
    line.len()
}

/// One `textDocument/definition` target: a document URI plus the 0-based
/// line and UTF-16 column of the definition's start.
#[derive(Debug, Clone, PartialEq)]
pub struct DefLocation {
    pub uri: String,
    pub line: u64,
    pub character: u64,
}

/// Parse a definition response: a Location, a Location array, a
/// LocationLink array, or null. Returns the first target.
fn parse_definition(v: &Value) -> Option<DefLocation> {
    let candidates: Vec<&Value> = match v {
        Value::Null => return None,
        Value::Array(a) => a.iter().collect(),
        one => vec![one],
    };
    for loc in candidates {
        if let Some(uri) = loc.get("uri").and_then(Value::as_str) {
            let s = &loc["range"]["start"];
            return Some(DefLocation {
                uri: uri.to_string(),
                line: s["line"].as_u64().unwrap_or(0),
                character: s["character"].as_u64().unwrap_or(0),
            });
        }
        if let Some(uri) = loc.get("targetUri").and_then(Value::as_str) {
            let mut s = &loc["targetSelectionRange"]["start"];
            if s.is_null() {
                s = &loc["targetRange"]["start"];
            }
            return Some(DefLocation {
                uri: uri.to_string(),
                line: s["line"].as_u64().unwrap_or(0),
                character: s["character"].as_u64().unwrap_or(0),
            });
        }
    }
    None
}

/// Inverse of `path_to_uri`, which does not percent-encode: a `file://` URI
/// is just the path with the scheme stripped.
pub fn uri_to_path(uri: &str) -> PathBuf {
    let rest = uri.strip_prefix("file://").unwrap_or(uri);
    let rest = if cfg!(windows) {
        rest.strip_prefix('/').unwrap_or(rest)
    } else {
        rest
    };
    PathBuf::from(rest)
}

/// Flatten an LSP snippet to plain text: `${n:body}` keeps the body, bare
/// `$n` placeholders vanish, and `\}` / `\$` unescape.
fn strip_snippet(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars();
    while let Some(c) = it.next() {
        match c {
            '\\' => match it.next() {
                Some(e @ ('}' | '$')) => out.push(e),
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
                None => out.push('\\'),
            },
            '$' => {
                let rest: String = it.clone().collect();
                if let Some(rest) = rest.strip_prefix('{') {
                    if let Some(end) = rest.find('}') {
                        if let Some((_, inner)) = rest[..end].split_once(':') {
                            out.push_str(inner);
                        }
                        // consume '{' + body + '}'
                        for _ in 0..end + 2 {
                            it.next();
                        }
                    } else {
                        out.push('$');
                    }
                } else if rest.starts_with(|ch: char| ch.is_ascii_digit()) {
                    while it.clone().next().is_some_and(|ch| ch.is_ascii_digit()) {
                        it.next();
                    }
                } else {
                    out.push('$');
                }
            }
            _ => out.push(c),
        }
    }
    out
}

/// Parse the `result` of a completion request: null, an item array, or a
/// CompletionList object with an `items` array. `insert` prefers
/// textEdit.newText, then insertText, then the label; snippets are
/// flattened to plain text.
fn parse_completion(v: &Value) -> Vec<CompletionItem> {
    let arr = match v {
        Value::Null => return Vec::new(),
        Value::Array(a) => a.clone(),
        _ => v["items"].as_array().cloned().unwrap_or_default(),
    };
    let mut out = Vec::with_capacity(arr.len());
    for it in arr {
        let label = it["label"].as_str().unwrap_or("").to_string();
        if label.is_empty() {
            continue;
        }
        let insert = it["textEdit"]["newText"]
            .as_str()
            .or_else(|| it["insertText"].as_str())
            .unwrap_or(&label)
            .to_string();
        let insert = if it["insertTextFormat"].as_u64() == Some(2) {
            strip_snippet(&insert)
        } else {
            insert
        };
        out.push(CompletionItem {
            filter: it["filterText"].as_str().unwrap_or(&label).to_string(),
            sort: it["sortText"].as_str().unwrap_or(&label).to_string(),
            label,
            kind: it["kind"].as_u64().unwrap_or(0),
            insert,
        });
    }
    out
}

fn spawn_reader(
    stdout: ChildStdout,
    pending: Arc<Mutex<HashMap<u64, mpsc::Sender<Value>>>>,
    completion_ids: Arc<Mutex<HashSet<u64>>>,
    tx: mpsc::Sender<LspEvent>,
) {
    std::thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        while let Some(v) = read_message(&mut reader) {
            let method = v.get("method").and_then(|m| m.as_str());
            let id = v.get("id").and_then(|i| i.as_u64());
            match (method, id) {
                // A request from the server (has both a method and an id).
                (Some(m), Some(i)) => {
                    let _ = tx.send(LspEvent::ServerRequest {
                        id: i,
                        method: m.to_string(),
                    });
                }
                // A notification from the server (method, no id).
                (Some(m), None) => {
                    if m == "textDocument/publishDiagnostics" {
                        let uri = v["params"]["uri"].as_str().unwrap_or("").to_string();
                        let diags = parse_diagnostics(&v["params"]["diagnostics"]);
                        let _ = tx.send(LspEvent::Diagnostics { uri, diags });
                    }
                    // Other notifications (logMessage, showMessage, progress)
                    // are not surfaced yet.
                }
                // A response to one of our requests (id, no method). Requests
                // with a pending waiter are synchronous (handshake, shutdown);
                // everything else rano sends fire-and-forget is completion.
                (None, Some(i)) => {
                    let waiter = pending.lock().ok().and_then(|m| m.get(&i).cloned());
                    if let Some(tx) = waiter {
                        let _ = tx.send(v.get("result").cloned().unwrap_or(Value::Null));
                    } else if completion_ids.lock().ok().is_some_and(|mut s| s.remove(&i)) {
                        // A completion answer: a result, null, or an error —
                        // each consumes exactly one queued request.
                        let items = v.get("result").map(parse_completion).unwrap_or_default();
                        let _ = tx.send(LspEvent::Completion(items));
                    }
                    // Anything else is a late answer to a timed-out
                    // synchronous request: ignore.
                }
                (None, None) => {}
            }
        }
    });
}

/// Build the `textDocument/didChange` params for a full-document sync: one
/// range edit spanning the entire text, ending at the last line's length in
/// UTF-16 code units (a trailing `\n` yields an empty final line).
fn did_change_params(uri: &str, version: i32, text: &str) -> Value {
    let lines = text.split('\n').collect::<Vec<_>>();
    let last = lines.len().saturating_sub(1);
    let last_len = lines[last]
        .chars()
        .map(|c| c.len_utf16() as u64)
        .sum::<u64>();
    json!({
        "textDocument": { "uri": uri, "version": version },
        "contentChanges": [{
            "range": {
                "start": { "line": 0, "character": 0 },
                "end": { "line": last, "character": last_len }
            },
            "text": text
        }]
    })
}

impl LspClient {
    /// Spawn the server for `lang` rooted at `root`, open `doc` with `text`,
    /// and complete the initialize handshake.
    pub fn spawn(lang: Lang, root: &Path, doc: &Path, text: &str) -> Result<Self, String> {
        let Some((bin, args)) = command_for(lang) else {
            return Err(format!("LSP: no language server for {lang:?}"));
        };
        let mut child = Command::new(&bin)
            .args(&args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("LSP: cannot start {bin}: {e}"))?;

        let stdin: ChildStdin = child.stdin.take().unwrap();
        let stdout: ChildStdout = child.stdout.take().unwrap();
        let pending: Arc<Mutex<HashMap<u64, mpsc::Sender<Value>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let completion_ids: Arc<Mutex<HashSet<u64>>> = Arc::new(Mutex::new(HashSet::new()));
        let (tx, rx) = mpsc::channel();
        spawn_reader(stdout, pending.clone(), completion_ids.clone(), tx);

        let mut client = Self {
            child,
            stdin: Box::new(BufWriter::new(stdin)),
            next_id: 1,
            pending,
            completion_ids,
            rx,
            root_uri: path_to_uri(root),
            doc_uri: Some(path_to_uri(doc)),
            doc_path: Some(doc.to_path_buf()),
            doc_version: 1,
            initialized: false,
            lang,
        };
        client.handshake(text)?;
        Ok(client)
    }

    /// Spawn on a background thread so the initialize handshake (up to 15 s)
    /// never blocks the UI. The receiver yields exactly one result; dropping
    /// it before that drops the in-flight client (→ `Drop::shutdown`).
    pub fn spawn_async(
        lang: Lang,
        root: &Path,
        doc: &Path,
        text: &str,
    ) -> mpsc::Receiver<Result<LspClient, String>> {
        let (tx, rx) = mpsc::channel();
        let (root, doc) = (root.to_path_buf(), doc.to_path_buf());
        let text = text.to_string();
        std::thread::spawn(move || {
            let _ = tx.send(LspClient::spawn(lang, &root, &doc, &text));
        });
        rx
    }

    fn request(&mut self, method: &str, params: Value) -> (u64, mpsc::Receiver<Value>) {
        let id = self.next_id;
        self.next_id += 1;
        let (tx, rx) = mpsc::channel();
        if let Ok(mut m) = self.pending.lock() {
            m.insert(id, tx);
        }
        let msg = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        let _ = write_message(self.stdin.as_mut(), &msg);
        (id, rx)
    }

    fn notify(&mut self, method: &str, params: Value) {
        let msg = json!({ "jsonrpc": "2.0", "method": method, "params": params });
        let _ = write_message(self.stdin.as_mut(), &msg);
    }

    fn wait(
        &mut self,
        id: u64,
        rx: mpsc::Receiver<Value>,
        timeout: Duration,
    ) -> Result<Value, String> {
        let r = match rx.recv_timeout(timeout) {
            Ok(v) => Ok(v),
            Err(_) => Err("LSP: server did not respond in time".into()),
        };
        if let Ok(mut m) = self.pending.lock() {
            m.remove(&id);
        }
        r
    }

    fn handshake(&mut self, text: &str) -> Result<(), String> {
        let root_name = Path::new(&self.root_uri)
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let params = json!({
            "processId": std::process::id(),
            "rootUri": self.root_uri,
            "workspaceFolders": [{ "uri": self.root_uri, "name": root_name }],
            "capabilities": {
                "textDocument": {
                    "publishDiagnostics": { "relatedInformation": true },
                    "synchronization": {}
                },
                "workspace": {}
            }
        });
        let (id, rx) = self.request("initialize", params);
        let result = self.wait(id, rx, Duration::from_secs(15))?;
        if result.get("capabilities").is_none() {
            return Err("LSP: malformed initialize response".into());
        }
        self.notify("initialized", json!({}));

        let uri = self.doc_uri.clone().unwrap();
        self.notify(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": uri,
                    "languageId": language_id(self.lang),
                    "version": self.doc_version,
                    "text": text
                }
            }),
        );
        self.initialized = true;
        Ok(())
    }

    /// Send a change. Servers may request full or incremental sync; a single
    /// range edit covering the whole document is valid for both, so we always
    /// use that (LSP character offsets are UTF-16 code units).
    pub fn change(&mut self, text: &str) {
        if !self.initialized {
            return;
        }
        let Some(uri) = self.doc_uri.clone() else {
            return;
        };
        self.doc_version += 1;
        let params = did_change_params(&uri, self.doc_version, text);
        self.notify("textDocument/didChange", params);
    }

    /// Drain any notifications the reader thread has delivered. Also answers
    /// any requests the server sent us (with a null result) so the server is
    /// never left waiting on us.
    pub fn poll(&mut self) -> Vec<LspEvent> {
        let mut out = Vec::new();
        while let Ok(e) = self.rx.try_recv() {
            match e {
                LspEvent::ServerRequest { id, method: _ } => {
                    self.respond(id, Value::Null);
                }
                other => out.push(other),
            }
        }
        out
    }

    /// Fire-and-forget `textDocument/completion`. The response arrives as
    /// `LspEvent::Completion` (the reader routes responses without a pending
    /// waiter there); completion is rano's only async request kind.
    pub fn request_completion(&mut self, uri: &str, line: u64, character: u64) {
        if !self.initialized {
            return;
        }
        let id = self.next_id;
        self.next_id += 1;
        if let Ok(mut s) = self.completion_ids.lock() {
            s.insert(id);
        }
        let msg = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "textDocument/completion",
            "params": {
                "textDocument": { "uri": uri },
                "position": { "line": line, "character": character }
            }
        });
        let _ = write_message(self.stdin.as_mut(), &msg);
    }

    /// Synchronous `textDocument/definition`. Returns the first location,
    /// or None when the server answers null / an empty list.
    pub fn definition(
        &mut self,
        uri: &str,
        line: u64,
        character: u64,
        timeout: Duration,
    ) -> Result<Option<DefLocation>, String> {
        if !self.initialized {
            return Ok(None);
        }
        let params = json!({
            "textDocument": { "uri": uri },
            "position": { "line": line, "character": character }
        });
        let (id, rx) = self.request("textDocument/definition", params);
        let result = self.wait(id, rx, timeout)?;
        Ok(parse_definition(&result))
    }

    /// Send a JSON-RPC response to a server-initiated request.
    fn respond(&mut self, id: u64, result: Value) {
        let msg = json!({ "jsonrpc": "2.0", "id": id, "result": result });
        let _ = write_message(self.stdin.as_mut(), &msg);
    }

    pub fn shutdown(&mut self) {
        if self.initialized {
            let (id, rx) = self.request("shutdown", Value::Null);
            let _ = self.wait(id, rx, Duration::from_secs(2));
            self.notify("exit", Value::Null);
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for LspClient {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_roundtrip() {
        let v = json!({ "jsonrpc": "2.0", "id": 7, "method": "x", "n": 3 });
        let bytes = serde_json::to_vec(&v).unwrap();
        let mut wire = format!("Content-Length: {}\r\n\r\n", bytes.len()).into_bytes();
        wire.extend_from_slice(&bytes);

        let mut reader = std::io::BufReader::new(&wire[..]);
        let parsed = read_message(&mut reader).unwrap();
        assert_eq!(parsed["id"], 7);
        assert_eq!(parsed["method"], "x");
        assert_eq!(parsed["n"], 3);
    }

    #[test]
    fn finds_project_root() {
        let base = std::env::temp_dir().join(format!("rano_lsp_{}", std::process::id()));
        let deep = base.join("a").join("b");
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::write(base.join("Cargo.toml"), "").unwrap();
        let root = find_project_root(&deep, "Cargo.toml");
        assert_eq!(root, base);
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn parses_completion_variants() {
        // Plain item array: snippet insertText, bare label, textEdit.
        let v = json!([
            { "label": "println!", "kind": 3, "insertTextFormat": 2, "insertText": "println!($0)" },
            { "label": "x", "kind": 6 },
            { "label": "y", "textEdit": { "range": {}, "newText": "y()" } }
        ]);
        let items = parse_completion(&v);
        assert_eq!(items.len(), 3);
        assert_eq!(items[0].insert, "println!()");
        assert_eq!(items[0].kind, 3);
        assert_eq!(items[1].insert, "x");
        assert_eq!(items[2].insert, "y()");

        // CompletionList object.
        let v = json!({ "isIncomplete": true, "items": [ { "label": "z" } ] });
        let items = parse_completion(&v);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].label, "z");

        assert!(parse_completion(&Value::Null).is_empty());
    }

    #[test]
    fn strips_snippets() {
        assert_eq!(strip_snippet("foo($0)"), "foo()");
        assert_eq!(strip_snippet("foo(${1:x}, ${2:y})"), "foo(x, y)");
        assert_eq!(strip_snippet("f($1)"), "f()");
        assert_eq!(strip_snippet("a\\}b\\$c"), "a}b$c");
        assert_eq!(
            strip_snippet("$HOME"),
            "$HOME",
            "non-placeholder $ survives"
        );
    }

    #[test]
    fn utf16_cols() {
        assert_eq!(utf16_col(&['a', 'b'], 2), 2);
        assert_eq!(utf16_col(&['a', '𝕏', 'b'], 3), 4, "𝕏 is 2 UTF-16 units");
        assert_eq!(utf16_col(&['a'], 5), 1);
    }

    #[test]
    fn utf16_to_char_roundtrip() {
        let line: Vec<char> = "a𝕏b".chars().collect();
        assert_eq!(utf16_to_char(&line, 0), 0);
        assert_eq!(utf16_to_char(&line, 1), 1);
        assert_eq!(
            utf16_to_char(&line, 2),
            2,
            "utf16 offsets never land mid-char; clamp forward"
        );
        assert_eq!(utf16_to_char(&line, 3), 2);
        assert_eq!(utf16_to_char(&line, 4), 3);
        assert_eq!(utf16_to_char(&line, 99), 3, "past EOL clamps");
        assert_eq!(utf16_to_char(&line, utf16_col(&line, 2)), 2);
    }

    #[test]
    fn parses_definition_shapes() {
        // A single Location.
        let loc = json!({
            "uri": "file:///x/y.rs",
            "range": { "start": { "line": 3, "character": 7 }, "end": {} }
        });
        assert_eq!(
            parse_definition(&loc),
            Some(DefLocation {
                uri: "file:///x/y.rs".into(),
                line: 3,
                character: 7
            })
        );
        // An array picks the first.
        let arr = json!([loc, {
            "uri": "file:///other.rs",
            "range": { "start": { "line": 9, "character": 0 } }
        }]);
        assert_eq!(parse_definition(&arr).unwrap().line, 3);
        // LocationLink uses targetUri + targetSelectionRange.
        let link = json!([{
            "targetUri": "file:///z.rs",
            "targetSelectionRange": { "start": { "line": 1, "character": 4 } }
        }]);
        assert_eq!(
            parse_definition(&link),
            Some(DefLocation {
                uri: "file:///z.rs".into(),
                line: 1,
                character: 4
            })
        );
        // null and junk.
        assert_eq!(parse_definition(&json!(null)), None);
        assert_eq!(parse_definition(&json!("nope")), None);
    }

    #[test]
    fn uri_path_roundtrip() {
        let p = std::path::Path::new("/home/u/my file.rs");
        assert_eq!(uri_to_path(&path_to_uri(p)), p);
    }

    #[test]
    fn parses_diagnostics() {
        let v = json!([{
            "range": { "start": { "line": 2, "character": 4 }, "end": { "line": 2, "character": 9 } },
            "message": "boom",
            "severity": 1
        }, {
            "range": { "start": { "line": 0, "character": 0 } },
            "message": "no end",
            "severity": 2
        }]);
        let d = parse_diagnostics(&v);
        assert_eq!(d.len(), 2);
        assert_eq!(d[0].line, 2);
        assert_eq!(d[0].col, 4);
        assert_eq!(d[0].end_col, 9);
        assert_eq!(d[0].message, "boom");
        assert_eq!(d[0].severity, 1);
        assert_eq!(d[1].line, 0);
        assert_eq!(d[1].col, 0);
        assert_eq!(d[1].end_col, 0);
    }

    #[test]
    fn did_change_params_single_line() {
        let v = did_change_params("file:///t", 3, "abc");
        assert_eq!(v["textDocument"]["uri"], "file:///t");
        assert_eq!(v["textDocument"]["version"], 3);
        assert_eq!(
            v["contentChanges"][0]["range"]["end"],
            json!({ "line": 0, "character": 3 })
        );
        assert_eq!(v["contentChanges"][0]["text"], "abc");
    }

    #[test]
    fn did_change_params_trailing_newline() {
        // "a𝕏" is 3 chars / 4 UTF-16 units; the trailing \n makes an empty
        // final line, so the range ends at {line:2, character:0}.
        let v = did_change_params("file:///t", 1, "a𝕏\nb\n");
        assert_eq!(
            v["contentChanges"][0]["range"]["end"],
            json!({ "line": 2, "character": 0 })
        );
    }

    #[test]
    fn did_change_params_astral_last_line() {
        // 𝕏 is one char but 2 UTF-16 code units.
        let v = did_change_params("file:///t", 1, "a𝕏");
        assert_eq!(
            v["contentChanges"][0]["range"]["end"],
            json!({ "line": 0, "character": 3 })
        );
    }

    // Test-only client whose writes land in `buf` instead of a server's stdin;
    // `cat` never reads its stdin pipe, and `initialized:false` keeps Drop
    // from sending a shutdown request (it just kills the process).
    struct SharedBuf(Arc<Mutex<Vec<u8>>>);

    impl Write for SharedBuf {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn test_client(rx: mpsc::Receiver<LspEvent>, buf: Arc<Mutex<Vec<u8>>>) -> LspClient {
        let child = Command::new("cat")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        LspClient {
            child,
            stdin: Box::new(SharedBuf(buf)),
            next_id: 1,
            pending: Arc::new(Mutex::new(HashMap::new())),
            completion_ids: Arc::new(Mutex::new(HashSet::new())),
            rx,
            root_uri: "file:///tmp".into(),
            doc_uri: None,
            doc_path: None,
            doc_version: 1,
            initialized: false,
            lang: Lang::Rust,
        }
    }

    #[test]
    fn poll_responds_to_server_request() {
        let buf = Arc::new(Mutex::new(Vec::new()));
        let (tx, rx) = mpsc::channel();
        tx.send(LspEvent::ServerRequest {
            id: 5,
            method: "x".into(),
        })
        .unwrap();
        let mut client = test_client(rx, buf.clone());

        let out = client.poll();
        assert!(out.is_empty(), "server requests are answered, not surfaced");
        let written = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
        assert!(written.contains("\"id\":5"), "got: {written}");
        assert!(written.contains("\"result\":null"), "got: {written}");

        // Empty rx afterwards: poll drains nothing and writes nothing.
        let len = buf.lock().unwrap().len();
        assert!(client.poll().is_empty());
        assert_eq!(buf.lock().unwrap().len(), len);
    }

    #[test]
    fn path_uri() {
        assert_eq!(path_to_uri(Path::new("/tmp/x.rs")), "file:///tmp/x.rs");
    }

    // End-to-end: spawn rust-analyzer and confirm the initial analysis yields
    // a type-error diagnostic. (This rust-analyzer build only pushes once at
    // open and does not push after `didChange`, so we assert the initial
    // report rather than a live update.) Skips when the server is absent.
    #[test]
    #[ignore] // spawns rust-analyzer: slow + env-dependent (run with -- --ignored)
    fn rust_diagnostics_flow() {
        let bin = find_bin("rust-analyzer");
        if bin == "rust-analyzer" {
            eprintln!("skip: rust-analyzer not found");
            return;
        }
        let base = std::env::temp_dir().join(format!("rano_lspflow_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let src = base.join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(
            base.join("Cargo.toml"),
            "[package]\nname=\"d\"\nversion=\"0.1.0\"\nedition=\"2021\"\n",
        )
        .unwrap();
        let doc = src.join("main.rs");
        let broken = "fn main() {\n    let x: String = 1;\n}\n";
        std::fs::write(&doc, broken).unwrap();

        let mut client =
            LspClient::spawn(Lang::Rust, &base, &doc, broken).expect("spawn rust-analyzer");

        // The initial diagnostics should include a type error.
        let mut saw_error = false;
        for _ in 0..200 {
            for e in client.poll() {
                if let LspEvent::Diagnostics { diags, .. } = e
                    && diags.iter().any(|d| d.severity == 1)
                {
                    saw_error = true;
                }
            }
            if saw_error {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        assert!(
            saw_error,
            "expected a type-error diagnostic from rust-analyzer"
        );

        client.shutdown();
        std::fs::remove_dir_all(&base).ok();
    }
}
