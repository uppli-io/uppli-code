// MCP Server mode — exposes uppli-code as an MCP tool via stdio or Unix socket.
//
// When launched with `uppli-code --mcp-server`, the CLI speaks JSON-RPC 2.0
// on stdin/stdout. When launched with `--peer` inside a `--groom` parent, it
// listens on a Unix domain socket instead. Either way, the same dispatch
// logic ([`handle_request`] + tool handlers) serves the connection.
//
// The per-connection state (writer channel) is passed separately from the
// long-lived [`McpServerState`] so a single state can back multiple
// connection handlers over the lifetime of the process. In practice the
// groom binome is 1:1, but this shape keeps the stdio and Unix paths
// symmetric and leaves room for future multi-client use.

use cc_mcp::types::{JsonRpcError, JsonRpcRequest, JsonRpcResponse};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

/// Capacity of the bounded per-connection write channel. 1024 messages is
/// generous for any realistic MCP session; if the writer falls this far
/// behind, notifications are dropped with a warning rather than OOMing.
const CONN_CHANNEL_CAPACITY: usize = 1024;

// ---------------------------------------------------------------------------
// NotificationSender — typed wrapper around the bounded channel
// ---------------------------------------------------------------------------

/// Mediates all writes to the per-connection channel. Handlers receive a
/// `&NotificationSender` instead of the raw `mpsc::Sender`, which
/// restricts them to the two intended send patterns:
///
/// * **Notifications** — best-effort via `try_send`; dropped with a warning
///   if the channel is full (a slow reader must not block the handler).
/// * **Responses** — use the blocking `.send().await` path (a response
///   *must* be delivered; back-pressure from a slow reader is acceptable).
#[derive(Clone)]
struct NotificationSender {
    tx: mpsc::Sender<String>,
}

impl NotificationSender {
    /// Send a notification (best-effort). If the channel is full, the
    /// message is dropped and a warning is logged.
    fn send_notification(&self, line: String) {
        if let Err(mpsc::error::TrySendError::Full(_)) = self.tx.try_send(line) {
            warn!("MCP notification dropped (channel full)");
        }
        // Closed channel → peer is gone, silently ignore.
    }

    /// Send a response. Blocks if the channel is full (responses must not
    /// be silently dropped). If the channel is closed, the error is
    /// ignored — the peer is already gone.
    async fn send_response(&self, line: String) {
        let _ = self.tx.send(line).await;
    }
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

/// MCP server state — holds the provider and configuration needed to
/// execute queries on behalf of the remote MCP client. No I/O handles live
/// here; see [`Conn`] for per-connection state.
pub struct McpServerState {
    pub provider: Arc<dyn cc_api::LlmProvider>,
    pub working_dir: PathBuf,
    pub config: cc_core::config::Config,
    pub cost_tracker: Arc<cc_core::cost::CostTracker>,
    pub permission_mode: cc_core::config::PermissionMode,
    #[allow(dead_code)]
    pub max_turns: u32,
    /// CLI --model override (None = use provider default).
    pub model_override: Option<String>,
    /// True while a query is running on this state. Rejects concurrent
    /// `uppli_query` calls with a busy error.
    pub busy: Arc<std::sync::atomic::AtomicBool>,
}

/// Per-connection context. Bundles the shared state with a
/// [`NotificationSender`] so responses and streaming notifications can be
/// emitted from anywhere in the call chain without threading the raw
/// writer through every signature.
#[derive(Clone)]
struct Conn {
    state: Arc<McpServerState>,
    /// Typed wrapper around the bounded channel. Handlers use
    /// `sender.send_notification()` for best-effort notifications and
    /// `sender.send_response()` for must-deliver responses.
    sender: NotificationSender,
}

// ---------------------------------------------------------------------------
// Public entry points
// ---------------------------------------------------------------------------

/// Run the MCP server on stdin/stdout. Blocks until stdin closes.
pub async fn run_mcp_server(state: McpServerState) -> anyhow::Result<()> {
    let state = Arc::new(state);
    info!("MCP server started (stdio), waiting for requests");
    serve_connection(state, tokio::io::stdin(), tokio::io::stdout()).await?;
    info!("MCP server stdin closed, shutting down");
    Ok(())
}

/// Run the MCP server on a Unix domain socket. Binds the socket, accepts a
/// single connection (the binome peer), serves it to completion, and then
/// returns. The socket file is removed on exit.
///
/// Rationale for single-accept: the `--groom` feature is a 1:1 binome. Any
/// second connection attempt would share [`McpServerState.busy`] with the
/// first and race on `uppli_query`, which is precisely what we want to
/// avoid. If multi-client ever becomes a requirement, loop over `accept`
/// and spawn a task per connection — `serve_connection` is already written
/// to support that.
#[cfg(unix)]
pub async fn run_mcp_server_unix(
    state: McpServerState,
    socket_path: PathBuf,
) -> anyhow::Result<()> {
    let state = Arc::new(state);

    // Clean any stale socket file from a prior crash. Safe because the
    // caller chose the path (we prefix it with our pid upstream).
    let _ = std::fs::remove_file(&socket_path);

    let listener = tokio::net::UnixListener::bind(&socket_path).map_err(|e| {
        anyhow::anyhow!(
            "failed to bind MCP Unix socket at {}: {e}",
            socket_path.display()
        )
    })?;

    info!(path = %socket_path.display(), "MCP server listening (unix socket)");

    // Guard that removes the socket file on drop so we don't leak it if
    // the caller bails early.
    struct SocketGuard(PathBuf);
    impl Drop for SocketGuard {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }
    let _guard = SocketGuard(socket_path.clone());

    let (stream, _addr) = listener.accept().await?;
    info!("MCP peer connected");
    let (read_half, write_half) = stream.into_split();
    serve_connection(state, read_half, write_half).await?;
    info!("MCP peer disconnected, shutting down");
    Ok(())
}

// ---------------------------------------------------------------------------
// Generic per-connection driver
// ---------------------------------------------------------------------------

/// Drive one JSON-RPC connection: spawn a writer task that drains pre-framed
/// lines to `writer`, then loop on `reader`, dispatching requests. Returns
/// when the reader hits EOF.
async fn serve_connection<R, W>(
    state: Arc<McpServerState>,
    reader: R,
    writer: W,
) -> anyhow::Result<()>
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let (tx, mut rx) = mpsc::channel::<String>(CONN_CHANNEL_CAPACITY);

    // Writer task — owns the AsyncWrite for the lifetime of the connection.
    // Exits when `tx` is dropped (reader loop completes) or when a write
    // fails (peer closed).
    let writer_task = tokio::spawn(async move {
        let mut w = writer;
        while let Some(line) = rx.recv().await {
            if w.write_all(line.as_bytes()).await.is_err() {
                break;
            }
            if w.flush().await.is_err() {
                break;
            }
        }
    });

    // Guard that aborts the writer task on early return (e.g. `?`
    // propagation from handle_request). Without this, an early `drop(conn)`
    // closes the sender but the `.await` on writer_task is skipped, so a
    // panicked writer disappears silently.
    //
    // Uses `Option<JoinHandle>` — on Drop the handle is aborted (early
    // return / panic). On the happy path, `.take()` extracts it for a clean
    // `.await`.
    struct WriterGuard(Option<tokio::task::JoinHandle<()>>);
    impl Drop for WriterGuard {
        fn drop(&mut self) {
            if let Some(h) = self.0.take() {
                h.abort();
            }
        }
    }
    let mut guard = WriterGuard(Some(writer_task));

    let conn = Conn {
        state,
        sender: NotificationSender { tx },
    };
    let mut lines = BufReader::new(reader).lines();

    while let Ok(Some(line)) = lines.next_line().await {
        let line = line.trim().to_string();
        if line.is_empty() {
            continue;
        }

        let request: JsonRpcRequest = match serde_json::from_str(&line) {
            Ok(r) => r,
            Err(e) => {
                send_response(
                    &conn,
                    &JsonRpcResponse {
                        jsonrpc: "2.0".to_string(),
                        id: None,
                        result: None,
                        error: Some(JsonRpcError {
                            code: -32700,
                            message: format!("Parse error: {}", e),
                            data: None,
                        }),
                    },
                )
                .await;
                continue;
            }
        };

        debug!(method = %request.method, "MCP request");
        let response = handle_request(&conn, &request).await;
        send_response(&conn, &response).await;
    }

    // Drop the sender so the writer task exits cleanly, then await it.
    drop(conn);

    // Take the handle out of the guard so Drop does NOT abort it.
    let writer_handle = guard.0.take().expect("writer handle already taken");

    match writer_handle.await {
        Ok(()) => {}
        Err(e) if e.is_cancelled() => {
            debug!("MCP writer task cancelled");
        }
        Err(e) => {
            warn!(error = %e, "MCP writer task panicked");
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Dispatcher
// ---------------------------------------------------------------------------

async fn handle_request(conn: &Conn, req: &JsonRpcRequest) -> JsonRpcResponse {
    let result = match req.method.as_str() {
        "initialize" => handle_initialize(&conn.state),
        "initialized" => return ok(req, json!({})),
        "tools/list" => Ok(json!({ "tools": tool_definitions() })),
        "tools/call" => handle_tool_call(conn, req.params.as_ref()).await,
        "resources/list" => Ok(json!({ "resources": resource_definitions() })),
        "resources/read" => handle_resource_read(&conn.state, req.params.as_ref()).await,
        "ping" => Ok(json!({})),
        _ => Err(rpc_err(-32601, format!("Method not found: {}", req.method))),
    };

    match result {
        Ok(v) => ok(req, v),
        Err(e) => JsonRpcResponse {
            jsonrpc: "2.0".to_string(),
            id: req.id.clone(),
            result: None,
            error: Some(e),
        },
    }
}

// ---------------------------------------------------------------------------
// Protocol: initialize
// ---------------------------------------------------------------------------

fn handle_initialize(state: &Arc<McpServerState>) -> Result<Value, JsonRpcError> {
    let caps = state.provider.capabilities();
    let model = state
        .model_override
        .as_deref()
        .unwrap_or(&caps.default_model);
    Ok(json!({
        "protocolVersion": "2024-11-05",
        "capabilities": {
            "tools": { "listChanged": false },
            "resources": { "subscribe": false, "listChanged": false },
        },
        "serverInfo": {
            "name": "uppli-code",
            "version": env!("CARGO_PKG_VERSION"),
            "description": format!(
                "Uppli Code agent — {} ({})",
                caps.display_name, model
            ),
        }
    }))
}

// ---------------------------------------------------------------------------
// Protocol: tool + resource definitions
// ---------------------------------------------------------------------------

fn tool_definitions() -> Value {
    json!([
        {
            "name": "uppli_query",
            "description": "Send a prompt to the uppli-code agent and run the full agentic loop \
                (read, edit, bash, etc.). Progress notifications stream in real time. \
                Returns the final assistant response.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "prompt": { "type": "string", "description": "Task or question" },
                    "max_turns": { "type": "integer", "description": "Max agentic turns (default 250)", "default": 250 },
                    "working_dir": { "type": "string", "description": "Working directory override" }
                },
                "required": ["prompt"]
            }
        },
        {
            "name": "uppli_status",
            "description": "Get agent status: provider, model, working dir, busy state, cost",
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "uppli_cancel",
            "description": "Cancel the currently running query",
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "uppli_sessions",
            "description": "List sessions for the current project",
            "inputSchema": { "type": "object", "properties": {} }
        }
    ])
}

fn resource_definitions() -> Value {
    json!([{
        "uri": "uppli://status",
        "name": "Agent Status",
        "description": "Current agent status as JSON",
        "mimeType": "application/json"
    }])
}

// ---------------------------------------------------------------------------
// Tool dispatch
// ---------------------------------------------------------------------------

async fn handle_tool_call(conn: &Conn, params: Option<&Value>) -> Result<Value, JsonRpcError> {
    let params = params.ok_or_else(|| rpc_err(-32602, "Missing params"))?;
    let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let args = params.get("arguments").cloned().unwrap_or(json!({}));

    match name {
        "uppli_query" => tool_query(conn, &args).await,
        "uppli_status" => tool_status(&conn.state),
        "uppli_cancel" => tool_cancel(&conn.state),
        "uppli_sessions" => tool_sessions(&conn.state).await,
        _ => Err(rpc_err(-32602, format!("Unknown tool: {}", name))),
    }
}

// ---------------------------------------------------------------------------
// uppli_query — the main tool
// ---------------------------------------------------------------------------

/// RAII guard that clears the busy flag on drop — prevents the flag from
/// staying stuck `true` if the function returns early via `?` or panics.
struct BusyGuard<'a>(&'a std::sync::atomic::AtomicBool);

impl Drop for BusyGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, std::sync::atomic::Ordering::SeqCst);
    }
}

async fn tool_query(conn: &Conn, args: &Value) -> Result<Value, JsonRpcError> {
    let state = &conn.state;
    let prompt = args
        .get("prompt")
        .and_then(|v| v.as_str())
        .ok_or_else(|| rpc_err(-32602, "Missing required: prompt"))?;

    if state.busy.swap(true, std::sync::atomic::Ordering::SeqCst) {
        return Err(rpc_err(-32000, "Agent is busy with another query"));
    }
    let _busy_guard = BusyGuard(&state.busy);

    let max_turns = args
        .get("max_turns")
        .and_then(|v| v.as_u64())
        .unwrap_or(250) as u32;
    let work_dir = args
        .get("working_dir")
        .and_then(|v| v.as_str())
        .map(PathBuf::from)
        .unwrap_or_else(|| state.working_dir.clone());

    info!(prompt_len = prompt.len(), max_turns, "MCP uppli_query");

    // Build tool context — auto-permissions, non-interactive
    let file_history = Arc::new(parking_lot::Mutex::new(
        cc_core::file_history::FileHistory::new(),
    ));
    let tool_ctx = cc_tools::ToolContext {
        working_dir: work_dir,
        permission_mode: state.permission_mode.clone(),
        permission_handler: Arc::new(cc_core::permissions::AutoPermissionHandler {
            mode: state.permission_mode.clone(),
        }),
        cost_tracker: state.cost_tracker.clone(),
        session_id: uuid::Uuid::new_v4().to_string(),
        file_history,
        current_turn: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        non_interactive: true,
        mcp_manager: None,
        config: state.config.clone(),
    };

    let tools: Vec<Box<dyn cc_tools::Tool>> = cc_tools::all_tools()
        .into_iter()
        .filter(|t| t.name() != cc_core::constants::TOOL_NAME_AGENT)
        .collect();

    let caps = state.provider.capabilities();
    let effective_model = state
        .model_override
        .as_deref()
        .unwrap_or(&caps.default_model)
        .to_string();
    let query_config = cc_query::QueryConfig {
        model: effective_model,
        max_tokens: 32_768,
        max_turns,
        system_prompt: None,
        append_system_prompt: None,
        output_style: cc_core::system_prompt::OutputStyle::Default,
        output_style_prompt: None,
        working_directory: Some(tool_ctx.working_dir.display().to_string()),
        thinking_budget: state
            .provider
            .capabilities()
            .default_thinking_budget
            .is_some()
            .then_some(64_000),
        temperature: None,
        tool_result_budget: 0,
        effort_level: Some(cc_core::effort::EffortLevel::Max),
        command_queue: None,
        skill_index: None,
        max_total_tokens: None,
        max_budget_usd: None,
        fallback_model: None,
    };

    let mut messages = vec![cc_core::types::Message::user(prompt.to_string())];
    let cancel = tokio_util::sync::CancellationToken::new();

    let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel::<cc_query::QueryEvent>();
    let notif_sender = conn.sender.clone();

    let notif_handle = tokio::spawn(async move {
        use cc_api::streaming::{ContentDelta, StreamEvent};

        while let Some(event) = event_rx.recv().await {
            let params = match &event {
                cc_query::QueryEvent::ToolStart {
                    tool_name,
                    tool_id,
                    input_json,
                } => {
                    let preview: String = input_json.chars().take(200).collect();
                    Some(
                        json!({"event":"tool_start","tool":tool_name,"tool_id":tool_id,"input_preview":preview}),
                    )
                }
                cc_query::QueryEvent::ToolEnd {
                    tool_name,
                    tool_id,
                    result,
                    is_error,
                } => {
                    let preview: String = result.chars().take(500).collect();
                    Some(
                        json!({"event":"tool_end","tool":tool_name,"tool_id":tool_id,"result_preview":preview,"is_error":is_error}),
                    )
                }
                cc_query::QueryEvent::TurnComplete {
                    turn, stop_reason, ..
                } => Some(json!({"event":"turn_complete","turn":turn,"stop_reason":stop_reason})),
                cc_query::QueryEvent::Status(msg) => Some(json!({"event":"status","message":msg})),
                cc_query::QueryEvent::Error(msg) => Some(json!({"event":"error","message":msg})),
                cc_query::QueryEvent::Stream(StreamEvent::ContentBlockDelta {
                    delta: ContentDelta::TextDelta { text },
                    ..
                }) => Some(json!({"event":"text_delta","text":text})),
                cc_query::QueryEvent::Stream(StreamEvent::ContentBlockDelta {
                    delta: ContentDelta::ThinkingDelta { thinking },
                    ..
                }) => Some(json!({"event":"thinking_delta","text":thinking})),
                _ => None,
            };
            if let Some(p) = params {
                let notif = JsonRpcRequest::notification("notifications/progress", Some(p));
                if let Ok(mut line) = serde_json::to_string(&notif) {
                    line.push('\n');
                    notif_sender.send_notification(line);
                }
            }
        }
    });

    let outcome = cc_query::run_query_loop(
        state.provider.as_ref(),
        &mut messages,
        &tools,
        &tool_ctx,
        &query_config,
        state.cost_tracker.clone(),
        Some(event_tx),
        cancel,
        None,
    )
    .await;

    let _ = notif_handle.await;

    // busy flag is cleared automatically when `_busy_guard` drops.

    let response_text = messages
        .iter()
        .rev()
        .find(|m| m.role == cc_core::types::Role::Assistant)
        .map(|m| {
            m.content_blocks()
                .iter()
                .filter_map(|b| match b {
                    cc_core::types::ContentBlock::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default();

    let stop_reason = match &outcome {
        cc_query::QueryOutcome::EndTurn { .. } => "end_turn",
        cc_query::QueryOutcome::MaxTokens { .. } => "max_tokens",
        cc_query::QueryOutcome::Cancelled => "cancelled",
        cc_query::QueryOutcome::Error(e) => {
            warn!(error = %e, "MCP query error");
            "error"
        }
        cc_query::QueryOutcome::BudgetExceeded { .. } => "budget_exceeded",
    };

    let cost_summary = state.cost_tracker.summary();

    info!(stop_reason, "MCP uppli_query complete");

    Ok(json!({
        "content": [{ "type": "text", "text": response_text }],
        "metadata": {
            "stop_reason": stop_reason,
            "message_count": messages.len(),
            "cost_summary": cost_summary,
        }
    }))
}

// ---------------------------------------------------------------------------
// uppli_status
// ---------------------------------------------------------------------------

fn tool_status(state: &Arc<McpServerState>) -> Result<Value, JsonRpcError> {
    let caps = state.provider.capabilities();
    let busy = state.busy.load(std::sync::atomic::Ordering::Relaxed);
    let status = json!({
        "provider": caps.name,
        "model": caps.default_model,
        "fast_model": caps.fast_model,
        "working_dir": state.working_dir.display().to_string(),
        "busy": busy,
        "cost_summary": state.cost_tracker.summary(),
    });
    Ok(
        json!({ "content": [{ "type": "text", "text": serde_json::to_string_pretty(&status).unwrap_or_default() }] }),
    )
}

// ---------------------------------------------------------------------------
// uppli_cancel
// ---------------------------------------------------------------------------

fn tool_cancel(state: &Arc<McpServerState>) -> Result<Value, JsonRpcError> {
    // TODO: wire to a shared CancellationToken
    let was_busy = state.busy.swap(false, std::sync::atomic::Ordering::SeqCst);
    let msg = if was_busy {
        "Query cancelled"
    } else {
        "No active query"
    };
    Ok(json!({ "content": [{ "type": "text", "text": msg }] }))
}

// ---------------------------------------------------------------------------
// uppli_sessions
// ---------------------------------------------------------------------------

async fn tool_sessions(state: &Arc<McpServerState>) -> Result<Value, JsonRpcError> {
    let sessions = cc_core::session_storage::list_sessions(&state.working_dir)
        .await
        .unwrap_or_default();
    let list: Vec<Value> = sessions
        .iter()
        .map(|s| json!({ "session_id": s.session_id, "title": s.title }))
        .collect();
    Ok(
        json!({ "content": [{ "type": "text", "text": serde_json::to_string_pretty(&list).unwrap_or_default() }] }),
    )
}

// ---------------------------------------------------------------------------
// Resource read
// ---------------------------------------------------------------------------

async fn handle_resource_read(
    state: &Arc<McpServerState>,
    params: Option<&Value>,
) -> Result<Value, JsonRpcError> {
    let uri = params
        .and_then(|p| p.get("uri"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    match uri {
        "uppli://status" => {
            let status = tool_status(state)?;
            Ok(
                json!({ "contents": [{ "uri": "uppli://status", "mimeType": "application/json", "text": status["content"][0]["text"] }] }),
            )
        }
        _ => Err(rpc_err(-32602, format!("Unknown resource: {}", uri))),
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn ok(req: &JsonRpcRequest, result: Value) -> JsonRpcResponse {
    JsonRpcResponse {
        jsonrpc: "2.0".to_string(),
        id: req.id.clone(),
        result: Some(result),
        error: None,
    }
}

fn rpc_err(code: i64, msg: impl Into<String>) -> JsonRpcError {
    JsonRpcError {
        code,
        message: msg.into(),
        data: None,
    }
}

async fn send_response(conn: &Conn, resp: &JsonRpcResponse) {
    if let Ok(mut line) = serde_json::to_string(resp) {
        line.push('\n');
        conn.sender.send_response(line).await;
    }
}
