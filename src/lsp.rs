//! A minimal LSP (Language Server Protocol) client: JSON-RPC 2.0 over the
//! server process's stdio. A background reader thread parses the byte stream
//! and routes responses to per-request channels and notifications to a
//! channel the editor drains each event-loop tick.
//!
//! Only the subset rano needs is implemented: initialize/initialized,
//! didOpen, didChange (full-document sync), publishDiagnostics, shutdown/exit.

use std::collections::HashMap;
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

/// Events the reader thread delivers to the editor.
pub enum LspEvent {
    Diagnostics {
        uri: String,
        diags: Vec<Diagnostic>,
    },
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

fn command_for(lang: Lang) -> (String, Vec<String>) {
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
    };
    (find_bin(name), args.iter().map(|s| s.to_string()).collect())
}

fn language_id(lang: Lang) -> &'static str {
    match lang {
        Lang::Rust => "rust",
        Lang::Go => "go",
        Lang::Bash => "shellscript",
        Lang::Python => "python",
        Lang::C => "c",
        Lang::Json => "json",
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

fn path_to_uri(path: &Path) -> String {
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

fn spawn_reader(
    stdout: ChildStdout,
    pending: Arc<Mutex<HashMap<u64, mpsc::Sender<Value>>>>,
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
                // A response to one of our requests (id, no method).
                (None, Some(i)) => {
                    if let Some(tx) = pending.lock().ok().and_then(|m| m.get(&i).cloned()) {
                        let _ = tx.send(v.get("result").cloned().unwrap_or(Value::Null));
                    }
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
        let (bin, args) = command_for(lang);
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
        let (tx, rx) = mpsc::channel();
        spawn_reader(stdout, pending.clone(), tx);

        let mut client = Self {
            child,
            stdin: Box::new(BufWriter::new(stdin)),
            next_id: 1,
            pending,
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
