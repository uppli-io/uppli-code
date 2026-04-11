// MCP Server mode — exposes uppli-code as an MCP tool via stdio.
//
// When launched with `uppli-code --mcp-server`, the CLI speaks JSON-RPC 2.0
// on stdin/stdout instead of launching the TUI or headless mode. Any MCP
// client (Claude Code, another uppli-code instance, CI pipelines) can
// connect to it and orchestrate coding tasks.
//
// This enables the "SuperAgent" pattern: a master agent piloting multiple
// uppli-code workers in parallel, each with its own provider/model/workdir.
//
// The protocol is bidirectional during a query:
//   - Server → Client: notifications/progress (tool_start, tool_end, text_delta, permission_request)
//   - Client → Server: notifications during query (permission_response, inject_prompt, cancel)

use cc_mcp::types::{JsonRpcError, JsonRpcRequest, JsonRpcResponse};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

/// MCP server state — holds the provider and configuration needed to
/// execute queries on behalf of the remote MCP client.
pub struct McpServerState {
    pub provider: Arc<dyn cc_api::LlmProvider>,
    pub working_dir: PathBuf,
    pub config: cc_core::config::Config,
    pub cost_tracker: Arc<cc_core::cost::CostTracker>,
    pub permission_mode: cc_core::config::PermissionMode,
    pub max_turns: u32,
    /// CLI --model override (None = use provider default).
    pub model_override: Option<String>,
    /// Shared stdout for responses AND streaming notifications.
    pub stdout: Arc<Mutex<tokio::io::Stdout>>,
    /// True while a query is running.
    pub busy: Arc<std::sync::atomic::AtomicBool>,
}

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Run the MCP server loop on stdin/stdout. Blocks until stdin closes.
pub async fn run_mcp_server(state: McpServerState) -> anyhow::Result<()> {
    let state = Arc::new(state);
    let stdin = BufReader::new(tokio::io::stdin());
    let stdout = state.stdout.clone();
    let mut lines = stdin.lines();

    info!("MCP server started (stdio), waiting for requests");

    while let Ok(Some(line)) = lines.next_line().await {
        let line = line.trim().to_string();
        if line.is_empty() {
            continue;
        }

        let request: JsonRpcRequest = match serde_json::from_str(&line) {
            Ok(r) => r,
            Err(e) => {
                write_response(
                    &stdout,
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

        let response = handle_request(&state, &request).await;
        write_response(&stdout, &response).await;
    }

    info!("MCP server stdin closed, shutting down");
    Ok(())
}

// ---------------------------------------------------------------------------
// Dispatcher
// ---------------------------------------------------------------------------

async fn handle_request(state: &Arc<McpServerState>, req: &JsonRpcRequest) -> JsonRpcResponse {
    let result = match req.method.as_str() {
        "initialize" => handle_initialize(state),
        "initialized" => return ok(req, json!({})),
        "tools/list" => Ok(json!({ "tools": tool_definitions() })),
        "tools/call" => handle_tool_call(state, req.params.as_ref()).await,
        "resources/list" => Ok(json!({ "resources": resource_definitions() })),
        "resources/read" => handle_resource_read(state, req.params.as_ref()).await,
        "ping" => Ok(json!({})),
        _ => Err(rpc_err(-32601, format!("Method not found: {}", req.method))),
    };

    match result {
        Ok(v) => ok(req, v),
        Err(e) => JsonRpcResponse {
            jsonrpc: "2.0".to_string(),
            id: Some(req.id.clone()),
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

async fn handle_tool_call(
    state: &Arc<McpServerState>,
    params: Option<&Value>,
) -> Result<Value, JsonRpcError> {
    let params = params.ok_or_else(|| rpc_err(-32602, "Missing params"))?;
    let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let args = params.get("arguments").cloned().unwrap_or(json!({}));

    match name {
        "uppli_query" => tool_query(state, &args).await,
        "uppli_status" => tool_status(state),
        "uppli_cancel" => tool_cancel(state),
        "uppli_sessions" => tool_sessions(state).await,
        _ => Err(rpc_err(-32602, format!("Unknown tool: {}", name))),
    }
}

// ---------------------------------------------------------------------------
// uppli_query — the main tool
// ---------------------------------------------------------------------------

async fn tool_query(
    state: &Arc<McpServerState>,
    args: &Value,
) -> Result<Value, JsonRpcError> {
    let prompt = args
        .get("prompt")
        .and_then(|v| v.as_str())
        .ok_or_else(|| rpc_err(-32602, "Missing required: prompt"))?;

    if state.busy.swap(true, std::sync::atomic::Ordering::SeqCst) {
        return Err(rpc_err(-32000, "Agent is busy with another query"));
    }

    let max_turns = args.get("max_turns").and_then(|v| v.as_u64()).unwrap_or(250) as u32;
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
        // Max output: 32K (2x Claude Code's 16K)
        max_tokens: 32_768,
        max_turns,
        system_prompt: None,
        append_system_prompt: None,
        output_style: cc_core::system_prompt::OutputStyle::Default,
        output_style_prompt: None,
        working_directory: Some(tool_ctx.working_dir.display().to_string()),
        // Thinking: 64K — sweet spot. 131K available but causes timeouts
        // (model thinks for 5+ minutes without acting).
        thinking_budget: Some(64_000),
        temperature: None,
        // Never truncate tool results — 1M context window handles it.
        // Compaction kicks in only if we approach 80% (800K tokens).
        tool_result_budget: 0,
        effort_level: Some(cc_core::effort::EffortLevel::Max),
        command_queue: None,
        skill_index: None,
        max_budget_usd: None,
        // Full reasoning on all turns — no fast model fallback.
        fallback_model: None,
    };

    let mut messages = vec![cc_core::types::Message::user(prompt.to_string())];
    let cancel = tokio_util::sync::CancellationToken::new();

    // Event channel for streaming progress notifications to MCP client.
    let (event_tx, mut event_rx) =
        tokio::sync::mpsc::unbounded_channel::<cc_query::QueryEvent>();
    let stdout_clone = state.stdout.clone();

    // Forward events as MCP notifications in a background task.
    let notif_handle = tokio::spawn(async move {
        use cc_api::streaming::{ContentDelta, StreamEvent};

        while let Some(event) = event_rx.recv().await {
            let params = match &event {
                cc_query::QueryEvent::ToolStart { tool_name, tool_id, input_json } => {
                    let preview: String = input_json.chars().take(200).collect();
                    Some(json!({"event":"tool_start","tool":tool_name,"tool_id":tool_id,"input_preview":preview}))
                }
                cc_query::QueryEvent::ToolEnd { tool_name, tool_id, result, is_error } => {
                    let preview: String = result.chars().take(500).collect();
                    Some(json!({"event":"tool_end","tool":tool_name,"tool_id":tool_id,"result_preview":preview,"is_error":is_error}))
                }
                cc_query::QueryEvent::TurnComplete { turn, stop_reason, .. } => {
                    Some(json!({"event":"turn_complete","turn":turn,"stop_reason":stop_reason}))
                }
                cc_query::QueryEvent::Status(msg) => {
                    Some(json!({"event":"status","message":msg}))
                }
                cc_query::QueryEvent::Error(msg) => {
                    Some(json!({"event":"error","message":msg}))
                }
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
                send_notification(&stdout_clone, "notifications/progress", p).await;
            }
        }
    });

    // Run the agentic loop.
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

    // Wait for notifications to drain before building the response.
    let _ = notif_handle.await;

    state.busy.store(false, std::sync::atomic::Ordering::SeqCst);

    // Extract final assistant text.
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
    Ok(json!({ "content": [{ "type": "text", "text": serde_json::to_string_pretty(&status).unwrap_or_default() }] }))
}

// ---------------------------------------------------------------------------
// uppli_cancel
// ---------------------------------------------------------------------------

fn tool_cancel(state: &Arc<McpServerState>) -> Result<Value, JsonRpcError> {
    // TODO: wire to a shared CancellationToken
    let was_busy = state.busy.swap(false, std::sync::atomic::Ordering::SeqCst);
    let msg = if was_busy { "Query cancelled" } else { "No active query" };
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
    Ok(json!({ "content": [{ "type": "text", "text": serde_json::to_string_pretty(&list).unwrap_or_default() }] }))
}

// ---------------------------------------------------------------------------
// Resource read
// ---------------------------------------------------------------------------

async fn handle_resource_read(
    state: &Arc<McpServerState>,
    params: Option<&Value>,
) -> Result<Value, JsonRpcError> {
    let uri = params.and_then(|p| p.get("uri")).and_then(|v| v.as_str()).unwrap_or("");
    match uri {
        "uppli://status" => {
            let status = tool_status(state)?;
            Ok(json!({ "contents": [{ "uri": "uppli://status", "mimeType": "application/json", "text": status["content"][0]["text"] }] }))
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
        id: Some(req.id.clone()),
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

async fn write_response(stdout: &Arc<Mutex<tokio::io::Stdout>>, resp: &JsonRpcResponse) {
    if let Ok(mut line) = serde_json::to_string(resp) {
        line.push('\n');
        let mut out = stdout.lock().await;
        let _ = out.write_all(line.as_bytes()).await;
        let _ = out.flush().await;
    }
}

async fn send_notification(stdout: &Arc<Mutex<tokio::io::Stdout>>, method: &str, params: Value) {
    let notif = JsonRpcRequest::notification(method, Some(params));
    if let Ok(mut line) = serde_json::to_string(&notif) {
        line.push('\n');
        let mut out = stdout.lock().await;
        let _ = out.write_all(line.as_bytes()).await;
        let _ = out.flush().await;
    }
}
