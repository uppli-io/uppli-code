//! Language Server Protocol client.
//!
//! Implements the client side of the LSP JSON-RPC protocol over the LSP
//! server's stdin/stdout.  Each [`LspClient`] manages one server process;
//! [`LspManager`] tracks a collection of clients keyed by server name.
//!
//! # Protocol overview
//! Messages are framed with a `Content-Length` HTTP-style header:
//! ```text
//! Content-Length: <N>\r\n
//! \r\n
//! <N bytes of UTF-8 JSON>
//! ```
//! The server sends the same framing back on its stdout.

use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;
use std::path::Path;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader, BufWriter};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::{oneshot, Mutex};

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// Configuration for a single LSP server process.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LspServerConfig {
    /// Display name, e.g. "rust-analyzer"
    pub name: String,
    /// Path or name of the server binary, e.g. "rust-analyzer"
    pub command: String,
    /// Command-line arguments passed to the server binary
    pub args: Vec<String>,
    /// Glob patterns that activate this server, e.g. `["*.rs", "*.toml"]`
    pub file_patterns: Vec<String>,
    /// Optional server-specific initialization options (passed in LSP `initialize`)
    pub initialization_options: Option<serde_json::Value>,
    /// Map of file extension (e.g. `.rs`) to LSP language identifier (e.g.
    /// `rust`).  Used to supply `textDocument/didOpen::languageId` and to
    /// route files to the right server.
    #[serde(default)]
    pub extension_to_language: HashMap<String, String>,
    /// Optional extra environment variables for the server process.
    #[serde(default)]
    pub env: HashMap<String, String>,
}

impl LspServerConfig {
    /// Look up the LSP language identifier for `file_path`, falling back to
    /// `"plaintext"` when the extension is not mapped.
    pub fn language_for_file(&self, file_path: &str) -> String {
        let ext = Path::new(file_path)
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| format!(".{}", e.to_lowercase()))
            .unwrap_or_default();
        self.extension_to_language
            .get(&ext)
            .cloned()
            .unwrap_or_else(|| "plaintext".to_string())
    }
}

// ---------------------------------------------------------------------------
// Diagnostics
// ---------------------------------------------------------------------------

/// A single diagnostic emitted by an LSP server.
#[derive(Debug, Clone)]
pub struct LspDiagnostic {
    /// Workspace-relative or absolute file path
    pub file: String,
    /// 1-based line number
    pub line: u32,
    /// 1-based column number
    pub column: u32,
    pub severity: DiagnosticSeverity,
    pub message: String,
    /// The LSP server that produced this diagnostic (e.g. "rust-analyzer")
    pub source: Option<String>,
    /// Diagnostic code (e.g. "E0308"), if provided by the server
    pub code: Option<String>,
}

/// Severity level of a diagnostic, matching the LSP spec.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum DiagnosticSeverity {
    Error = 1,
    Warning = 2,
    Information = 3,
    Hint = 4,
}

impl DiagnosticSeverity {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warning => "warning",
            Self::Information => "info",
            Self::Hint => "hint",
        }
    }

    fn from_lsp_int(n: u64) -> Self {
        match n {
            1 => Self::Error,
            2 => Self::Warning,
            3 => Self::Information,
            _ => Self::Hint,
        }
    }
}

// ---------------------------------------------------------------------------
// JSON-RPC framing helpers
// ---------------------------------------------------------------------------

async fn send_message(writer: &mut BufWriter<ChildStdin>, body: &str) -> anyhow::Result<()> {
    let header = format!("Content-Length: {}\r\n\r\n", body.len());
    writer.write_all(header.as_bytes()).await?;
    writer.write_all(body.as_bytes()).await?;
    writer.flush().await?;
    Ok(())
}

async fn read_message(reader: &mut BufReader<ChildStdout>) -> anyhow::Result<serde_json::Value> {
    let mut content_length: usize = 0;
    loop {
        let mut line = String::new();
        let n = reader.read_line(&mut line).await?;
        if n == 0 {
            return Err(anyhow::anyhow!("LSP server closed stdout"));
        }
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            break;
        }
        if let Some(val) = trimmed.strip_prefix("Content-Length: ") {
            content_length = val.trim().parse()?;
        }
    }
    if content_length == 0 {
        return Err(anyhow::anyhow!("LSP message missing Content-Length header"));
    }
    let mut buf = vec![0u8; content_length];
    reader.read_exact(&mut buf).await?;
    Ok(serde_json::from_slice(&buf)?)
}

// ---------------------------------------------------------------------------
// LspClient
// ---------------------------------------------------------------------------

type PendingMap = Arc<DashMap<u64, oneshot::Sender<serde_json::Value>>>;

/// A running LSP client connected to a single server process.
pub struct LspClient {
    pub server_name: String,
    pub server_config: LspServerConfig,
    /// The child process handle; `None` after shutdown.
    process: Option<Child>,
    request_id: Arc<AtomicU64>,
    pending: PendingMap,
    /// Diagnostics indexed by URI.
    pub diagnostics: Arc<DashMap<String, Vec<LspDiagnostic>>>,
    is_initialized: bool,
    /// Shared writer — wrapped in a Mutex so `start_receiver_task` and the
    /// public `send_*` methods can both hold it.
    writer: Option<Arc<Mutex<BufWriter<ChildStdin>>>>,
}

impl LspClient {
    /// Spawn the server process and return a connected client.  The I/O pump
    /// task is started in the background.
    pub async fn start(config: LspServerConfig) -> anyhow::Result<Self> {
        let mut cmd = Command::new(&config.command);
        cmd.args(&config.args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);

        // Inject environment variables
        for (k, v) in &config.env {
            cmd.env(k, v);
        }

        // On Windows, suppress the console window (CREATE_NO_WINDOW = 0x0800_0000).
        // tokio::process::Command exposes creation_flags() directly on Windows.
        #[cfg(target_os = "windows")]
        {
            cmd.creation_flags(0x0800_0000u32);
        }

        let mut child = cmd.spawn().map_err(|e| {
            anyhow::anyhow!("Failed to start LSP server '{}': {}", config.command, e)
        })?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow::anyhow!("LSP server stdin not available"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow::anyhow!("LSP server stdout not available"))?;

        let pending: PendingMap = Arc::new(DashMap::new());
        let diagnostics: Arc<DashMap<String, Vec<LspDiagnostic>>> = Arc::new(DashMap::new());

        let writer = Arc::new(Mutex::new(BufWriter::new(stdin)));
        let pending_clone = pending.clone();
        let diagnostics_clone = diagnostics.clone();
        let server_name = config.name.clone();

        // Consume stderr in the background so the OS pipe buffer never fills up
        if let Some(stderr) = child.stderr.take() {
            let name = server_name.clone();
            tokio::spawn(async move {
                let mut lines = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    tracing::debug!("[LSP SERVER {}] {}", name, line);
                }
            });
        }

        // I/O pump: reads messages from stdout and resolves pending requests
        // or stores incoming diagnostics.
        tokio::spawn(async move {
            let mut reader = BufReader::new(stdout);
            loop {
                match read_message(&mut reader).await {
                    Ok(msg) => {
                        dispatch_incoming(msg, &pending_clone, &diagnostics_clone, &server_name);
                    }
                    Err(e) => {
                        tracing::debug!("LSP server {} reader exited: {}", server_name, e);
                        break;
                    }
                }
            }
        });

        Ok(Self {
            server_name: config.name.clone(),
            server_config: config,
            process: Some(child),
            request_id: Arc::new(AtomicU64::new(1)),
            pending,
            diagnostics,
            is_initialized: false,
            writer: Some(writer),
        })
    }

    fn next_id(&self) -> u64 {
        self.request_id.fetch_add(1, Ordering::SeqCst)
    }

    /// Send a JSON-RPC request and wait for the matching response.
    async fn send_request_inner(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> anyhow::Result<serde_json::Value> {
        let id = self.next_id();
        let msg = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        let body = serde_json::to_string(&msg)?;

        let (tx, rx) = oneshot::channel();
        self.pending.insert(id, tx);

        {
            let writer = self
                .writer
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("LSP client already shut down"))?;
            let mut w = writer.lock().await;
            send_message(&mut w, &body).await?;
        }

        let response = tokio::time::timeout(std::time::Duration::from_secs(30), rx)
            .await
            .map_err(|_| {
                anyhow::anyhow!(
                    "LSP request '{}' timed out (server: {})",
                    method,
                    self.server_name
                )
            })?
            .map_err(|_| {
                anyhow::anyhow!(
                    "LSP request '{}' channel closed (server: {})",
                    method,
                    self.server_name
                )
            })?;

        if let Some(err) = response.get("error") {
            return Err(anyhow::anyhow!(
                "LSP error from {}: {}",
                self.server_name,
                err
            ));
        }
        Ok(response["result"].clone())
    }

    /// Send a JSON-RPC notification (fire-and-forget, no response expected).
    async fn send_notification_inner(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> anyhow::Result<()> {
        let msg = json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        });
        let body = serde_json::to_string(&msg)?;
        let writer = self
            .writer
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("LSP client already shut down"))?;
        let mut w = writer.lock().await;
        send_message(&mut w, &body).await
    }

    /// Perform the LSP `initialize` / `initialized` handshake.
    pub async fn initialize(&mut self, root_uri: &str) -> anyhow::Result<()> {
        let params = json!({
            "processId": std::process::id(),
            "clientInfo": { "name": "claude-code", "version": "1.0" },
            "rootUri": root_uri,
            "capabilities": {
                "textDocument": {
                    "publishDiagnostics": {
                        "relatedInformation": true,
                        "versionSupport": false,
                        "codeDescriptionSupport": false
                    },
                    "synchronization": {
                        "dynamicRegistration": false,
                        "willSave": false,
                        "willSaveWaitUntil": false,
                        "didSave": true
                    }
                },
                "workspace": {
                    "configuration": false,
                    "didChangeConfiguration": { "dynamicRegistration": false }
                }
            },
            "initializationOptions": self.server_config.initialization_options,
        });

        self.send_request_inner("initialize", params).await?;

        // Send the `initialized` notification to complete the handshake
        self.send_notification_inner("initialized", json!({}))
            .await?;

        self.is_initialized = true;
        tracing::debug!("LSP server '{}' initialized", self.server_name);
        Ok(())
    }

    /// Notify the server that a document has been opened.
    pub async fn open_document(
        &mut self,
        uri: &str,
        language_id: &str,
        content: &str,
    ) -> anyhow::Result<()> {
        self.send_notification_inner(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": uri,
                    "languageId": language_id,
                    "version": 1,
                    "text": content,
                }
            }),
        )
        .await
    }

    /// Notify the server that a document has been changed.
    pub async fn change_document(
        &mut self,
        uri: &str,
        content: &str,
        version: i64,
    ) -> anyhow::Result<()> {
        self.send_notification_inner(
            "textDocument/didChange",
            json!({
                "textDocument": { "uri": uri, "version": version },
                "contentChanges": [{ "text": content }],
            }),
        )
        .await
    }

    /// Notify the server that a document has been saved.
    pub async fn save_document(&mut self, uri: &str) -> anyhow::Result<()> {
        self.send_notification_inner(
            "textDocument/didSave",
            json!({ "textDocument": { "uri": uri } }),
        )
        .await
    }

    /// Notify the server that a document has been closed.
    pub async fn close_document(&mut self, uri: &str) -> anyhow::Result<()> {
        self.send_notification_inner(
            "textDocument/didClose",
            json!({ "textDocument": { "uri": uri } }),
        )
        .await
    }

    /// Get cached diagnostics for `file_path`.
    pub fn get_diagnostics(&self, file_path: &str) -> Vec<LspDiagnostic> {
        let uri = path_to_uri(file_path);
        self.diagnostics
            .get(&uri)
            .map(|v| v.clone())
            .unwrap_or_default()
    }

    /// Get all cached diagnostics across every file.
    pub fn all_diagnostics(&self) -> Vec<LspDiagnostic> {
        self.diagnostics
            .iter()
            .flat_map(|entry| entry.value().clone())
            .collect()
    }

    /// Returns `true` if `initialize` has completed successfully.
    pub fn is_initialized(&self) -> bool {
        self.is_initialized
    }

    /// Gracefully shut down the server.
    pub async fn shutdown(&mut self) -> anyhow::Result<()> {
        if !self.is_initialized {
            return Ok(());
        }
        // Attempt graceful shutdown; ignore errors since we kill anyway.
        let _ = self.send_request_inner("shutdown", json!(null)).await;
        let _ = self.send_notification_inner("exit", json!(null)).await;

        // Drop the writer so the pipe closes cleanly before we wait.
        self.writer.take();

        if let Some(mut child) = self.process.take() {
            // Give the process a moment to exit cleanly.
            let _ = tokio::time::timeout(std::time::Duration::from_secs(5), child.wait()).await;
            let _ = child.kill().await;
        }
        self.is_initialized = false;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Incoming message dispatch
// ---------------------------------------------------------------------------

fn dispatch_incoming(
    msg: serde_json::Value,
    pending: &PendingMap,
    diagnostics: &Arc<DashMap<String, Vec<LspDiagnostic>>>,
    server_name: &str,
) {
    // Response to a request we sent
    if let Some(id) = msg.get("id").and_then(|v| v.as_u64()) {
        if let Some((_, tx)) = pending.remove(&id) {
            let _ = tx.send(msg);
        }
        return;
    }

    // Notification or request from the server
    if let Some(method) = msg.get("method").and_then(|v| v.as_str()) {
        match method {
            "textDocument/publishDiagnostics" => {
                handle_publish_diagnostics(&msg["params"], diagnostics, server_name);
            }
            _ => {
                tracing::trace!(
                    "LSP server {}: unhandled notification '{}'",
                    server_name,
                    method
                );
            }
        }
    }
}

fn handle_publish_diagnostics(
    params: &serde_json::Value,
    diagnostics: &Arc<DashMap<String, Vec<LspDiagnostic>>>,
    server_name: &str,
) {
    let uri = match params.get("uri").and_then(|v| v.as_str()) {
        Some(u) => u.to_string(),
        None => return,
    };

    let raw_diags = match params.get("diagnostics").and_then(|v| v.as_array()) {
        Some(d) => d,
        None => {
            diagnostics.insert(uri, Vec::new());
            return;
        }
    };

    // Convert the URI back to a file path for storage
    let file_path = uri_to_path(&uri);

    let parsed: Vec<LspDiagnostic> = raw_diags
        .iter()
        .filter_map(|d| parse_diagnostic(d, &file_path, server_name))
        .collect();

    tracing::debug!(
        "LSP server {}: {} diagnostics for {}",
        server_name,
        parsed.len(),
        file_path
    );

    diagnostics.insert(uri, parsed);
}

fn parse_diagnostic(
    d: &serde_json::Value,
    file_path: &str,
    server_name: &str,
) -> Option<LspDiagnostic> {
    let range = d.get("range")?;
    let start = range.get("start")?;
    let line = start.get("line")?.as_u64()? as u32 + 1; // LSP is 0-based
    let column = start.get("character")?.as_u64()? as u32 + 1;
    let message = d.get("message")?.as_str()?.to_string();

    let severity = d
        .get("severity")
        .and_then(|v| v.as_u64())
        .map(DiagnosticSeverity::from_lsp_int)
        .unwrap_or(DiagnosticSeverity::Error);

    let source = d
        .get("source")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .or_else(|| Some(server_name.to_string()));

    let code = d.get("code").map(|v| match v {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Number(n) => n.to_string(),
        other => other.to_string(),
    });

    Some(LspDiagnostic {
        file: file_path.to_string(),
        line,
        column,
        severity,
        message,
        source,
        code,
    })
}

// ---------------------------------------------------------------------------
// URI helpers
// ---------------------------------------------------------------------------

fn path_to_uri(path: &str) -> String {
    // Simple heuristic; for full correctness callers should pass pre-formed URIs
    if path.starts_with("file://") {
        return path.to_string();
    }
    let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| std::path::PathBuf::from(path));
    let s = canonical.to_string_lossy();
    if cfg!(target_os = "windows") {
        // Drive letters need a leading slash: file:///C:/...
        format!("file:///{}", s.replace('\\', "/"))
    } else {
        format!("file://{}", s)
    }
}

fn uri_to_path(uri: &str) -> String {
    let stripped = uri
        .strip_prefix("file:///")
        .or_else(|| uri.strip_prefix("file://"))
        .unwrap_or(uri);
    if cfg!(target_os = "windows") {
        stripped.replace('/', "\\")
    } else {
        stripped.to_string()
    }
}

// ---------------------------------------------------------------------------
// Diagnostic formatting (shared utility)
// ---------------------------------------------------------------------------

impl LspManager {
    /// Format a slice of diagnostics into a human-readable multi-line string
    /// suitable for inclusion in tool output or TUI display.
    pub fn format_diagnostics(diagnostics: &[LspDiagnostic]) -> String {
        if diagnostics.is_empty() {
            return "No diagnostics.".to_string();
        }
        diagnostics
            .iter()
            .map(|d| {
                format!(
                    "[{}] {}:{}:{} - {}{}{}",
                    d.severity.as_str().to_uppercase(),
                    d.file,
                    d.line,
                    d.column,
                    d.message,
                    d.source
                        .as_deref()
                        .map(|s| format!(" ({})", s))
                        .unwrap_or_default(),
                    d.code
                        .as_deref()
                        .map(|c| format!(" [{}]", c))
                        .unwrap_or_default(),
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

// ---------------------------------------------------------------------------
// LspManager — registry and multi-server coordination
// ---------------------------------------------------------------------------

/// Manages a collection of [`LspClient`] instances, routing file operations
/// to the correct server based on extension mappings.
pub struct LspManager {
    /// Registered configs (used for lookup before a client is started)
    configs: Vec<LspServerConfig>,
    /// Running clients keyed by server name
    clients: HashMap<String, LspClient>,
    /// Map of file extension → list of server names that handle it
    extension_map: HashMap<String, Vec<String>>,
    /// Set of file URIs that have been opened on a specific server (URI → server name)
    opened_files: HashMap<String, String>,
}

impl LspManager {
    pub fn new() -> Self {
        Self {
            configs: Vec::new(),
            clients: HashMap::new(),
            extension_map: HashMap::new(),
            opened_files: HashMap::new(),
        }
    }

    /// Register an LSP server configuration.
    pub fn register_server(&mut self, config: LspServerConfig) {
        // Build extension → server mapping
        for ext in config.extension_to_language.keys() {
            let normalized = ext.to_lowercase();
            self.extension_map
                .entry(normalized)
                .or_default()
                .push(config.name.clone());
        }
        // Also handle glob patterns like "*.rs" → ".rs"
        for pattern in &config.file_patterns {
            if let Some(ext) = pattern.strip_prefix("*.") {
                let normalized = format!(".{}", ext.to_lowercase());
                let entry = self.extension_map.entry(normalized).or_default();
                if !entry.contains(&config.name) {
                    entry.push(config.name.clone());
                }
            }
        }
        self.configs.push(config);
    }

    /// Return all registered server configurations.
    pub fn servers(&self) -> &[LspServerConfig] {
        &self.configs
    }

    /// Look up a server configuration by name.
    pub fn server_by_name(&self, name: &str) -> Option<&LspServerConfig> {
        self.configs.iter().find(|s| s.name == name)
    }

    /// Find the first server name that handles `file_path` based on extension.
    fn server_name_for_file(&self, file_path: &str) -> Option<&str> {
        let ext = Path::new(file_path)
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| format!(".{}", e.to_lowercase()))
            .unwrap_or_default();
        self.extension_map
            .get(&ext)
            .and_then(|names| names.first())
            .map(|s| s.as_str())
    }

    /// Spawn and initialize the server for `file_path` if it is not already
    /// running.  Returns `None` when no server is configured for this file type.
    async fn ensure_started(
        &mut self,
        file_path: &str,
        root_dir: &Path,
    ) -> anyhow::Result<Option<&mut LspClient>> {
        let server_name = match self.server_name_for_file(file_path) {
            Some(n) => n.to_string(),
            None => return Ok(None),
        };

        if !self.clients.contains_key(&server_name) {
            let config = match self.configs.iter().find(|c| c.name == server_name) {
                Some(c) => c.clone(),
                None => return Ok(None),
            };
            match LspClient::start(config).await {
                Ok(mut client) => {
                    let root_uri = path_to_uri(&root_dir.to_string_lossy());
                    if let Err(e) = client.initialize(&root_uri).await {
                        tracing::warn!("Failed to initialize LSP server '{}': {}", server_name, e);
                        // Don't insert — allow retry on next call
                        return Ok(None);
                    }
                    self.clients.insert(server_name.clone(), client);
                }
                Err(e) => {
                    tracing::warn!("Failed to start LSP server '{}': {}", server_name, e);
                    return Ok(None);
                }
            }
        }

        Ok(self.clients.get_mut(&server_name))
    }

    /// Spawn and initialize servers whose file patterns match files in the project.
    /// Skips servers that have no matching files (e.g., rust-analyzer in a Go project).
    pub async fn start_servers(&mut self, root_dir: &Path) {
        let configs: Vec<LspServerConfig> = self.configs.clone();
        for config in configs {
            let name = config.name.clone();
            if self.clients.contains_key(&name) {
                continue;
            }

            // Only start this server if the project has matching files.
            if !project_has_matching_files(root_dir, &config.file_patterns) {
                tracing::debug!(
                    server = %name,
                    patterns = ?config.file_patterns,
                    "Skipping LSP server — no matching files in project"
                );
                continue;
            }

            match LspClient::start(config).await {
                Ok(mut client) => {
                    let root_uri = path_to_uri(&root_dir.to_string_lossy());
                    if let Err(e) = client.initialize(&root_uri).await {
                        tracing::warn!("Failed to initialize LSP server '{}': {}", name, e);
                        continue;
                    }
                    self.clients.insert(name.clone(), client);
                    tracing::info!("LSP server '{}' started", name);
                }
                Err(e) => {
                    tracing::warn!("Failed to start LSP server '{}': {}", name, e);
                }
            }
        }
    }

    /// Open a file on the appropriate LSP server.
    pub async fn open_file(&mut self, file_path: &str, root_dir: &Path) -> anyhow::Result<()> {
        let uri = path_to_uri(file_path);
        let server_name = match self.server_name_for_file(file_path) {
            Some(n) => n.to_string(),
            None => return Ok(()),
        };

        // Skip if already opened on this server
        if self.opened_files.get(&uri).map(|s| s.as_str()) == Some(server_name.as_str()) {
            return Ok(());
        }

        let content = match tokio::fs::read_to_string(file_path).await {
            Ok(c) => c,
            Err(e) => {
                return Err(anyhow::anyhow!(
                    "Cannot read '{}' for LSP: {}",
                    file_path,
                    e
                ))
            }
        };

        // Ensure the server is running first (borrows self mutably, so must
        // finish before we borrow opened_files).
        self.ensure_started(file_path, root_dir).await?;

        if let Some(client) = self.clients.get_mut(&server_name) {
            let lang = client.server_config.language_for_file(file_path);
            client.open_document(&uri, &lang, &content).await?;
            self.opened_files.insert(uri, server_name);
        }
        Ok(())
    }

    /// Notify the appropriate server that a file's content has changed.
    pub async fn notify_file_changed(
        &mut self,
        file_path: &str,
        new_content: &str,
    ) -> anyhow::Result<()> {
        let server_name = match self.opened_files.get(file_path) {
            Some(n) => n.clone(),
            None => return Ok(()),
        };
        if let Some(client) = self.clients.get_mut(&server_name) {
            let uri = path_to_uri(file_path);
            client.change_document(&uri, new_content, 2).await?;
        }
        Ok(())
    }

    /// Notify the appropriate server that a file has been saved.
    pub async fn notify_file_saved(&mut self, file_path: &str) -> anyhow::Result<()> {
        let server_name = match self.opened_files.get(file_path) {
            Some(n) => n.clone(),
            None => return Ok(()),
        };
        if let Some(client) = self.clients.get_mut(&server_name) {
            let uri = path_to_uri(file_path);
            client.save_document(&uri).await?;
        }
        Ok(())
    }

    /// Get cached diagnostics for `file_path` across all running servers.
    pub fn get_diagnostics_for_file(&self, file_path: &str) -> Vec<LspDiagnostic> {
        self.clients
            .values()
            .flat_map(|c| c.get_diagnostics(file_path))
            .collect()
    }

    /// Get all cached diagnostics from all running servers.
    pub fn all_diagnostics(&self) -> Vec<LspDiagnostic> {
        self.clients
            .values()
            .flat_map(|c| c.all_diagnostics())
            .collect()
    }

    /// Shut down all running servers.
    pub async fn shutdown_all(&mut self) {
        let names: Vec<String> = self.clients.keys().cloned().collect();
        for name in names {
            if let Some(mut client) = self.clients.remove(&name) {
                if let Err(e) = client.shutdown().await {
                    tracing::warn!("Error shutting down LSP server '{}': {}", name, e);
                }
            }
        }
        self.opened_files.clear();
    }

    /// Get a legacy-compatible async diagnostic query (returns cached results).
    pub async fn get_diagnostics(&self, file: &str) -> Vec<LspDiagnostic> {
        self.get_diagnostics_for_file(file)
    }
}

impl Default for LspManager {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Global singleton
// ---------------------------------------------------------------------------

use once_cell::sync::Lazy;

static GLOBAL_LSP_MANAGER: Lazy<Arc<tokio::sync::Mutex<LspManager>>> =
    Lazy::new(|| Arc::new(tokio::sync::Mutex::new(LspManager::new())));

/// Access the global [`LspManager`] instance.
pub fn global_lsp_manager() -> Arc<tokio::sync::Mutex<LspManager>> {
    GLOBAL_LSP_MANAGER.clone()
}

/// Auto-detect installed language servers by checking PATH.
pub fn detect_installed_servers() -> Vec<LspServerConfig> {
    // (name, command_candidates, args, patterns, ext→lang mappings)
    let candidates: &[(&str, &[&str], &[&str], &[&str], &[(&str, &str)])] = &[
        // Python
        ("pyright", &["pyright-langserver"], &["--stdio"], &["*.py"], &[(".py", "python")]),
        ("pylsp", &["pylsp"], &[], &["*.py"], &[(".py", "python")]),
        // TypeScript / JavaScript
        ("typescript", &["typescript-language-server"], &["--stdio"],
            &["*.ts", "*.tsx", "*.js", "*.jsx"],
            &[(".ts","typescript"), (".tsx","typescriptreact"), (".js","javascript"), (".jsx","javascriptreact")]),
        // Rust
        ("rust-analyzer", &["rust-analyzer"], &[], &["*.rs"], &[(".rs", "rust")]),
        // Go
        ("gopls", &["gopls"], &[], &["*.go"], &[(".go", "go")]),
        // C / C++
        ("clangd", &["clangd"], &[], &["*.c", "*.cpp", "*.h", "*.hpp", "*.cc"],
            &[(".c","c"), (".cpp","cpp"), (".h","c"), (".hpp","cpp"), (".cc","cpp")]),
        // Java
        ("jdtls", &["jdtls", "java-language-server"], &[], &["*.java"], &[(".java", "java")]),
        // C#
        ("omnisharp", &["OmniSharp", "omnisharp"], &["--languageserver"], &["*.cs"], &[(".cs", "csharp")]),
        // Ruby
        ("solargraph", &["solargraph"], &["stdio"], &["*.rb"], &[(".rb", "ruby")]),
        // PHP
        ("phpactor", &["phpactor"], &["language-server"], &["*.php"], &[(".php", "php")]),
        ("intelephense", &["intelephense"], &["--stdio"], &["*.php"], &[(".php", "php")]),
        // Swift
        ("sourcekit-lsp", &["sourcekit-lsp"], &[], &["*.swift"], &[(".swift", "swift")]),
        // Kotlin
        ("kotlin-ls", &["kotlin-language-server"], &[], &["*.kt", "*.kts"], &[(".kt","kotlin"), (".kts","kotlin")]),
        // Lua
        ("lua-ls", &["lua-language-server"], &[], &["*.lua"], &[(".lua", "lua")]),
        // Elixir
        ("elixir-ls", &["elixir-ls", "language_server.sh"], &[], &["*.ex", "*.exs"], &[(".ex","elixir"), (".exs","elixir")]),
        // Zig
        ("zls", &["zls"], &[], &["*.zig"], &[(".zig", "zig")]),
        // YAML
        ("yaml-ls", &["yaml-language-server"], &["--stdio"], &["*.yaml", "*.yml"], &[(".yaml","yaml"), (".yml","yaml")]),
        // HTML / CSS
        ("html-ls", &["vscode-html-language-server"], &["--stdio"], &["*.html"], &[(".html", "html")]),
        ("css-ls", &["vscode-css-language-server"], &["--stdio"], &["*.css", "*.scss"], &[(".css","css"), (".scss","scss")]),
        // Bash
        ("bash-ls", &["bash-language-server"], &["start"], &["*.sh", "*.bash"], &[(".sh","shellscript"), (".bash","shellscript")]),
        // Terraform
        ("terraform-ls", &["terraform-ls"], &["serve"], &["*.tf"], &[(".tf", "terraform")]),
        // Docker
        ("dockerfile-ls", &["docker-langserver"], &["--stdio"], &["Dockerfile"], &[]),
        // SQL
        ("sqls", &["sqls"], &[], &["*.sql"], &[(".sql", "sql")]),
    ];

    let mut servers = Vec::new();
    let mut seen_patterns: std::collections::HashSet<String> = std::collections::HashSet::new();

    for (name, cmds, args, patterns, ext_map) in candidates {
        // Skip if we already have a server for these patterns (e.g., pyright beats pylsp)
        if patterns.iter().any(|p| seen_patterns.contains(*p)) {
            continue;
        }

        // Find the first command that exists in PATH
        let cmd = match cmds.iter().find(|c| which(c)) {
            Some(c) => *c,
            None => continue,
        };

        servers.push(LspServerConfig {
            name: name.to_string(),
            command: cmd.to_string(),
            args: args.iter().map(|s| s.to_string()).collect(),
            file_patterns: patterns.iter().map(|s| s.to_string()).collect(),
            initialization_options: None,
            extension_to_language: ext_map
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            env: Default::default(),
        });

        for p in *patterns {
            seen_patterns.insert(p.to_string());
        }
    }

    if !servers.is_empty() {
        tracing::info!(
            count = servers.len(),
            names = servers.iter().map(|s| s.name.as_str()).collect::<Vec<_>>().join(", "),
            "Auto-detected LSP servers"
        );
    }

    servers
}

/// Check if the project root contains any files matching the given glob patterns.
/// Does a shallow scan (top-level + one level deep) to avoid scanning huge repos.
fn project_has_matching_files(root: &Path, patterns: &[String]) -> bool {
    for pattern in patterns {
        // Extract extension from pattern like "*.rs" → ".rs"
        let ext = if let Some(e) = pattern.strip_prefix("*.") {
            format!(".{}", e)
        } else {
            continue;
        };

        // Check top-level files
        if let Ok(entries) = std::fs::read_dir(root) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_file() {
                    if let Some(file_ext) = path.extension().and_then(|e| e.to_str()) {
                        if format!(".{}", file_ext) == ext {
                            return true;
                        }
                    }
                }
                // Check one level deep (src/, lib/, etc.)
                if path.is_dir() {
                    if let Ok(sub_entries) = std::fs::read_dir(&path) {
                        for sub in sub_entries.flatten().take(50) {
                            let sub_path = sub.path();
                            if sub_path.is_file() {
                                if let Some(file_ext) =
                                    sub_path.extension().and_then(|e| e.to_str())
                                {
                                    if format!(".{}", file_ext) == ext {
                                        return true;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    false
}

fn which(cmd: &str) -> bool {
    std::process::Command::new("which")
        .arg(cmd)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn make_config(name: &str) -> LspServerConfig {
        LspServerConfig {
            name: name.to_string(),
            command: name.to_string(),
            args: vec![],
            file_patterns: vec!["*.rs".to_string()],
            initialization_options: None,
            extension_to_language: {
                let mut m = HashMap::new();
                m.insert(".rs".to_string(), "rust".to_string());
                m
            },
            env: HashMap::new(),
        }
    }

    fn make_diagnostic(
        file: &str,
        line: u32,
        col: u32,
        severity: DiagnosticSeverity,
        message: &str,
    ) -> LspDiagnostic {
        LspDiagnostic {
            file: file.to_string(),
            line,
            column: col,
            severity,
            message: message.to_string(),
            source: None,
            code: None,
        }
    }

    #[test]
    fn test_new_manager_empty() {
        let mgr = LspManager::new();
        assert!(mgr.servers().is_empty());
    }

    #[test]
    fn test_register_server() {
        let mut mgr = LspManager::new();
        mgr.register_server(make_config("rust-analyzer"));
        assert_eq!(mgr.servers().len(), 1);
        assert_eq!(mgr.servers()[0].name, "rust-analyzer");
    }

    #[test]
    fn test_register_multiple_servers() {
        let mut mgr = LspManager::new();
        mgr.register_server(make_config("rust-analyzer"));
        mgr.register_server(make_config("pyright"));
        assert_eq!(mgr.servers().len(), 2);
    }

    #[test]
    fn test_server_by_name_found() {
        let mut mgr = LspManager::new();
        mgr.register_server(make_config("rust-analyzer"));
        mgr.register_server(make_config("pyright"));
        let s = mgr.server_by_name("pyright");
        assert!(s.is_some());
        assert_eq!(s.unwrap().name, "pyright");
    }

    #[test]
    fn test_server_by_name_not_found() {
        let mgr = LspManager::new();
        assert!(mgr.server_by_name("missing").is_none());
    }

    #[tokio::test]
    async fn test_get_diagnostics_empty_when_no_servers() {
        let mgr = LspManager::new();
        let diags = mgr.get_diagnostics("src/main.rs").await;
        assert!(diags.is_empty());
    }

    #[test]
    fn test_format_diagnostics_empty() {
        let result = LspManager::format_diagnostics(&[]);
        assert_eq!(result, "No diagnostics.");
    }

    #[test]
    fn test_format_diagnostics_single_error() {
        let diags = vec![make_diagnostic(
            "src/lib.rs",
            10,
            5,
            DiagnosticSeverity::Error,
            "type mismatch",
        )];
        let result = LspManager::format_diagnostics(&diags);
        assert!(result.contains("[ERROR]"));
        assert!(result.contains("src/lib.rs"));
        assert!(result.contains("10:5"));
        assert!(result.contains("type mismatch"));
    }

    #[test]
    fn test_format_diagnostics_multiple() {
        let diags = vec![
            make_diagnostic("a.rs", 1, 1, DiagnosticSeverity::Error, "err1"),
            make_diagnostic("b.rs", 2, 3, DiagnosticSeverity::Warning, "warn1"),
        ];
        let result = LspManager::format_diagnostics(&diags);
        let lines: Vec<&str> = result.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains("[ERROR]"));
        assert!(lines[1].contains("[WARNING]"));
    }

    #[test]
    fn test_format_diagnostics_with_source_and_code() {
        let mut d = make_diagnostic(
            "main.rs",
            5,
            1,
            DiagnosticSeverity::Error,
            "mismatched types",
        );
        d.source = Some("rust-analyzer".to_string());
        d.code = Some("E0308".to_string());
        let result = LspManager::format_diagnostics(&[d]);
        assert!(result.contains("(rust-analyzer)"), "result = {}", result);
        assert!(result.contains("[E0308]"), "result = {}", result);
    }

    #[test]
    fn test_diagnostic_severity_ordering() {
        assert!(DiagnosticSeverity::Error < DiagnosticSeverity::Warning);
        assert!(DiagnosticSeverity::Warning < DiagnosticSeverity::Information);
        assert!(DiagnosticSeverity::Information < DiagnosticSeverity::Hint);
    }

    #[test]
    fn test_diagnostic_severity_as_str() {
        assert_eq!(DiagnosticSeverity::Error.as_str(), "error");
        assert_eq!(DiagnosticSeverity::Warning.as_str(), "warning");
        assert_eq!(DiagnosticSeverity::Information.as_str(), "info");
        assert_eq!(DiagnosticSeverity::Hint.as_str(), "hint");
    }

    #[test]
    fn test_lsp_server_config_serialization() {
        let cfg = make_config("rust-analyzer");
        let json = serde_json::to_string(&cfg).unwrap();
        let back: LspServerConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(back.name, "rust-analyzer");
    }

    #[test]
    fn test_default_trait() {
        let mgr = LspManager::default();
        assert!(mgr.servers().is_empty());
    }

    #[test]
    fn test_extension_routing() {
        let mut mgr = LspManager::new();
        mgr.register_server(make_config("rust-analyzer"));
        // .rs maps to rust-analyzer
        assert_eq!(
            mgr.server_name_for_file("src/main.rs"),
            Some("rust-analyzer")
        );
        // .py has no mapping
        assert_eq!(mgr.server_name_for_file("app.py"), None);
    }

    #[test]
    fn test_path_to_uri_roundtrip() {
        // On the current platform, converting a relative path to URI and back
        // should not panic.
        let uri = path_to_uri("src/main.rs");
        assert!(
            uri.starts_with("file://"),
            "expected file:// URI, got {}",
            uri
        );
        let _back = uri_to_path(&uri);
    }

    #[test]
    fn test_language_for_file() {
        let cfg = make_config("rust-analyzer");
        assert_eq!(cfg.language_for_file("src/main.rs"), "rust");
        assert_eq!(cfg.language_for_file("README.md"), "plaintext");
    }

    #[test]
    fn test_severity_from_lsp_int() {
        assert_eq!(
            DiagnosticSeverity::from_lsp_int(1),
            DiagnosticSeverity::Error
        );
        assert_eq!(
            DiagnosticSeverity::from_lsp_int(2),
            DiagnosticSeverity::Warning
        );
        assert_eq!(
            DiagnosticSeverity::from_lsp_int(3),
            DiagnosticSeverity::Information
        );
        assert_eq!(
            DiagnosticSeverity::from_lsp_int(4),
            DiagnosticSeverity::Hint
        );
        assert_eq!(
            DiagnosticSeverity::from_lsp_int(99),
            DiagnosticSeverity::Hint
        );
    }

    #[test]
    fn test_global_lsp_manager_consistent() {
        let m1 = global_lsp_manager();
        let m2 = global_lsp_manager();
        assert!(Arc::ptr_eq(&m1, &m2));
    }

    #[test]
    fn test_parse_diagnostic_valid() {
        let raw = serde_json::json!({
            "range": {
                "start": { "line": 4, "character": 2 },
                "end":   { "line": 4, "character": 10 }
            },
            "severity": 1,
            "message": "type mismatch",
            "source": "rust-analyzer",
            "code": "E0308"
        });
        let d = parse_diagnostic(&raw, "src/main.rs", "rust-analyzer").unwrap();
        assert_eq!(d.line, 5); // 0-based → 1-based
        assert_eq!(d.column, 3);
        assert_eq!(d.message, "type mismatch");
        assert_eq!(d.severity, DiagnosticSeverity::Error);
        assert_eq!(d.code.as_deref(), Some("E0308"));
    }

    #[test]
    fn test_parse_diagnostic_missing_range_returns_none() {
        let raw = serde_json::json!({ "message": "oops" });
        assert!(parse_diagnostic(&raw, "f.rs", "lsp").is_none());
    }
}
