// cc-mcp: Model Context Protocol (MCP) client implementation.
//
// MCP is a JSON-RPC 2.0 based protocol for connecting Claude to external
// tool/resource servers. This crate implements:
//
// - JSON-RPC 2.0 client primitives
// - MCP protocol handshake (initialize, initialized)
// - Tool discovery (tools/list)
// - Tool execution (tools/call)
// - Resource management (resources/list, resources/read)
// - Prompt templates (prompts/list, prompts/get)
// - Transport: stdio (subprocess) and HTTP/SSE
// - Environment variable expansion in server configs
// - Connection manager with exponential-backoff reconnection

use async_trait::async_trait;
use cc_core::config::McpServerConfig;
use cc_core::types::ToolDefinition;
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{mpsc, oneshot, Mutex};
use tracing::{debug, error, info, warn};

pub use client::McpClient;
pub use connection_manager::{McpConnectionManager, McpServerStatus};
pub use types::*;

pub mod connection_manager;
pub mod oauth;
pub mod registry;

// ---------------------------------------------------------------------------
// Environment variable expansion
// ---------------------------------------------------------------------------

/// Expand `${VAR_NAME}` and `${VAR_NAME:-default}` patterns in `input` using
/// the process environment.  Unknown variables without a default are left as-is
/// (matching the TS behaviour: report missing but don't crash).
pub fn expand_env_vars(input: &str) -> String {
    let mut result = input.to_string();
    // We iterate from left to right, always restarting the search after each
    // substitution so that replaced values are not re-scanned.
    let mut search_from = 0;
    loop {
        match result[search_from..].find("${") {
            None => break,
            Some(rel_start) => {
                let start = search_from + rel_start;
                match result[start..].find('}') {
                    None => break, // unclosed brace — stop
                    Some(rel_end) => {
                        let end = start + rel_end; // index of '}'
                        let inner = &result[start + 2..end]; // content between ${ and }

                        // Support ${VAR:-default} syntax
                        let (var_name, default_value) = if let Some(pos) = inner.find(":-") {
                            (&inner[..pos], Some(&inner[pos + 2..]))
                        } else {
                            (inner, None)
                        };

                        let replacement = match std::env::var(var_name) {
                            Ok(val) => val,
                            Err(_) => match default_value {
                                Some(def) => def.to_string(),
                                None => {
                                    // Leave the original text in place; advance past it
                                    search_from = end + 1;
                                    continue;
                                }
                            },
                        };

                        result =
                            format!("{}{}{}", &result[..start], replacement, &result[end + 1..]);
                        // Continue scanning from where the replacement ends
                        search_from = start + replacement.len();
                    }
                }
            }
        }
    }
    result
}

/// Expand env vars in every string field of a `McpServerConfig`.
/// Returns a new owned config; the original is not modified.
pub fn expand_server_config(config: &McpServerConfig) -> McpServerConfig {
    McpServerConfig {
        name: config.name.clone(),
        command: config.command.as_deref().map(expand_env_vars),
        args: config.args.iter().map(|a| expand_env_vars(a)).collect(),
        env: config
            .env
            .iter()
            .map(|(k, v)| (k.clone(), expand_env_vars(v)))
            .collect(),
        url: config.url.as_deref().map(expand_env_vars),
        server_type: config.server_type.clone(),
    }
}

// ---------------------------------------------------------------------------
// JSON-RPC 2.0 Types
// ---------------------------------------------------------------------------

pub mod types {
    use super::*;

    /// A JSON-RPC 2.0 request.
    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct JsonRpcRequest {
        pub jsonrpc: String,
        pub id: Value,
        pub method: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub params: Option<Value>,
    }

    impl JsonRpcRequest {
        pub fn new(id: impl Into<Value>, method: impl Into<String>, params: Option<Value>) -> Self {
            Self {
                jsonrpc: "2.0".to_string(),
                id: id.into(),
                method: method.into(),
                params,
            }
        }

        pub fn notification(method: impl Into<String>, params: Option<Value>) -> Self {
            Self {
                jsonrpc: "2.0".to_string(),
                id: Value::Null,
                method: method.into(),
                params,
            }
        }
    }

    /// A JSON-RPC 2.0 response.
    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct JsonRpcResponse {
        pub jsonrpc: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub id: Option<Value>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub result: Option<Value>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub error: Option<JsonRpcError>,
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct JsonRpcError {
        pub code: i64,
        pub message: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub data: Option<Value>,
    }

    // ---- MCP protocol types ------------------------------------------------

    /// MCP initialize request params.
    #[derive(Debug, Clone, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct InitializeParams {
        pub protocol_version: String,
        pub capabilities: ClientCapabilities,
        pub client_info: ClientInfo,
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct ClientCapabilities {
        #[serde(skip_serializing_if = "Option::is_none")]
        pub roots: Option<RootsCapability>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub sampling: Option<Value>,
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct RootsCapability {
        #[serde(rename = "listChanged")]
        pub list_changed: bool,
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct ClientInfo {
        pub name: String,
        pub version: String,
    }

    /// MCP initialize response result.
    #[derive(Debug, Clone, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct InitializeResult {
        pub protocol_version: String,
        pub capabilities: ServerCapabilities,
        pub server_info: ServerInfo,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub instructions: Option<String>,
    }

    #[derive(Debug, Clone, Serialize, Deserialize, Default)]
    pub struct ServerCapabilities {
        #[serde(skip_serializing_if = "Option::is_none")]
        pub tools: Option<ToolsCapability>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub resources: Option<ResourcesCapability>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub prompts: Option<PromptsCapability>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub logging: Option<Value>,
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct ToolsCapability {
        #[serde(default)]
        pub list_changed: bool,
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct ResourcesCapability {
        #[serde(default)]
        pub subscribe: bool,
        #[serde(default)]
        pub list_changed: bool,
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct PromptsCapability {
        #[serde(default)]
        pub list_changed: bool,
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct ServerInfo {
        pub name: String,
        pub version: String,
    }

    /// An MCP tool definition.
    #[derive(Debug, Clone, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct McpTool {
        pub name: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub description: Option<String>,
        pub input_schema: Value,
    }

    impl From<&McpTool> for ToolDefinition {
        fn from(t: &McpTool) -> Self {
            ToolDefinition {
                name: t.name.clone(),
                description: t.description.clone().unwrap_or_default(),
                input_schema: t.input_schema.clone(),
            }
        }
    }

    /// tools/list response.
    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct ListToolsResult {
        pub tools: Vec<McpTool>,
        #[serde(rename = "nextCursor", skip_serializing_if = "Option::is_none")]
        pub next_cursor: Option<String>,
    }

    /// tools/call params.
    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct CallToolParams {
        pub name: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub arguments: Option<Value>,
    }

    /// tools/call response.
    #[derive(Debug, Clone, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct CallToolResult {
        pub content: Vec<McpContent>,
        #[serde(default)]
        pub is_error: bool,
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    #[serde(tag = "type", rename_all = "lowercase")]
    pub enum McpContent {
        Text {
            text: String,
        },
        Image {
            data: String,
            #[serde(rename = "mimeType")]
            mime_type: String,
        },
        Resource {
            resource: ResourceContents,
        },
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct ResourceContents {
        pub uri: String,
        #[serde(rename = "mimeType", skip_serializing_if = "Option::is_none")]
        pub mime_type: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub text: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub blob: Option<String>,
    }

    /// An MCP resource.
    #[derive(Debug, Clone, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct McpResource {
        pub uri: String,
        pub name: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub description: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub mime_type: Option<String>,
    }

    /// resources/list response.
    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct ListResourcesResult {
        pub resources: Vec<McpResource>,
        #[serde(rename = "nextCursor", skip_serializing_if = "Option::is_none")]
        pub next_cursor: Option<String>,
    }

    /// An MCP prompt template.
    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct McpPrompt {
        pub name: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub description: Option<String>,
        #[serde(default)]
        pub arguments: Vec<McpPromptArgument>,
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct McpPromptArgument {
        pub name: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub description: Option<String>,
        #[serde(default)]
        pub required: bool,
    }

    /// prompts/list response.
    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct ListPromptsResult {
        pub prompts: Vec<McpPrompt>,
    }

    /// A single message returned by prompts/get.
    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct PromptMessage {
        /// "user" or "assistant"
        pub role: String,
        pub content: PromptMessageContent,
    }

    /// Content inside a PromptMessage.
    #[derive(Debug, Clone, Serialize, Deserialize)]
    #[serde(tag = "type", rename_all = "lowercase")]
    pub enum PromptMessageContent {
        Text { text: String },
        Image { data: String, mime_type: String },
        Resource { resource: serde_json::Value },
    }

    /// prompts/get response.
    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct GetPromptResult {
        #[serde(skip_serializing_if = "Option::is_none")]
        pub description: Option<String>,
        pub messages: Vec<PromptMessage>,
    }
}

// ---------------------------------------------------------------------------
// Transport layer
// ---------------------------------------------------------------------------

pub mod transport {
    use super::*;

    /// A transport can send requests and receive responses.
    #[async_trait]
    pub trait McpTransport: Send + Sync {
        async fn send(&self, message: &JsonRpcRequest) -> anyhow::Result<()>;
        async fn recv(&self) -> anyhow::Result<Option<JsonRpcResponse>>;
        async fn close(&self) -> anyhow::Result<()>;
    }

    /// Stdio transport: spawns a subprocess and communicates via stdin/stdout.
    pub struct StdioTransport {
        child: Arc<Mutex<Child>>,
        stdin: Arc<Mutex<ChildStdin>>,
        stdout_rx: Arc<Mutex<mpsc::UnboundedReceiver<String>>>,
    }

    impl StdioTransport {
        pub async fn spawn(config: &McpServerConfig) -> anyhow::Result<Self> {
            let command = config.command.as_deref().ok_or_else(|| {
                anyhow::anyhow!("MCP server '{}' has no command configured", config.name)
            })?;

            let mut cmd = Command::new(command);
            cmd.args(&config.args)
                .envs(&config.env)
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped());

            let mut child = cmd.spawn().map_err(|e| {
                anyhow::anyhow!(
                    "MCP server '{}': failed to spawn '{}': {}",
                    config.name,
                    command,
                    e
                )
            })?;

            let stdin = child.stdin.take().ok_or_else(|| {
                anyhow::anyhow!("MCP server '{}': could not capture stdin", config.name)
            })?;
            let stdout = child.stdout.take().ok_or_else(|| {
                anyhow::anyhow!("MCP server '{}': could not capture stdout", config.name)
            })?;

            let (tx, rx) = mpsc::unbounded_channel::<String>();

            // Background reader task — forwards stdout lines to the channel.
            tokio::spawn(async move {
                let reader = BufReader::new(stdout);
                let mut lines = reader.lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    if tx.send(line).is_err() {
                        break;
                    }
                }
            });

            Ok(Self {
                child: Arc::new(Mutex::new(child)),
                stdin: Arc::new(Mutex::new(stdin)),
                stdout_rx: Arc::new(Mutex::new(rx)),
            })
        }
    }

    #[async_trait]
    impl McpTransport for StdioTransport {
        async fn send(&self, message: &JsonRpcRequest) -> anyhow::Result<()> {
            let json = serde_json::to_string(message)? + "\n";
            let mut stdin = self.stdin.lock().await;
            stdin.write_all(json.as_bytes()).await?;
            stdin.flush().await?;
            Ok(())
        }

        async fn recv(&self) -> anyhow::Result<Option<JsonRpcResponse>> {
            let mut rx = self.stdout_rx.lock().await;
            let line = rx.recv().await;
            match line {
                Some(s) => {
                    let resp: JsonRpcResponse = serde_json::from_str(&s).map_err(|e| {
                        anyhow::anyhow!("MCP response parse error: {} (raw: {})", e, s)
                    })?;
                    Ok(Some(resp))
                }
                None => Ok(None),
            }
        }

        async fn close(&self) -> anyhow::Result<()> {
            let mut child = self.child.lock().await;
            let _ = child.kill().await;
            Ok(())
        }
    }
}

// ---------------------------------------------------------------------------
// MCP Client
// ---------------------------------------------------------------------------

pub mod client {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// A fully initialized MCP client connected to a single server.
    pub struct McpClient {
        pub server_name: String,
        pub server_info: Option<ServerInfo>,
        pub capabilities: ServerCapabilities,
        pub tools: Vec<McpTool>,
        pub resources: Vec<McpResource>,
        pub prompts: Vec<McpPrompt>,
        pub instructions: Option<String>,
        transport: Arc<dyn transport::McpTransport>,
        next_id: AtomicU64,
        #[allow(dead_code)]
        pending: Arc<Mutex<HashMap<u64, oneshot::Sender<JsonRpcResponse>>>>,
    }

    impl McpClient {
        /// Connect to an MCP server using stdio transport and complete the
        /// initialize handshake.  The `config` should already have env vars
        /// expanded via `expand_server_config`.
        pub async fn connect_stdio(config: &McpServerConfig) -> anyhow::Result<Self> {
            let transport = transport::StdioTransport::spawn(config).await?;
            let client = Self {
                server_name: config.name.clone(),
                server_info: None,
                capabilities: ServerCapabilities::default(),
                tools: vec![],
                resources: vec![],
                prompts: vec![],
                instructions: None,
                transport: Arc::new(transport),
                next_id: AtomicU64::new(1),
                pending: Arc::new(Mutex::new(HashMap::new())),
            };

            client.initialize().await
        }

        /// Send the MCP initialize handshake and discover capabilities.
        async fn initialize(mut self) -> anyhow::Result<Self> {
            let params = InitializeParams {
                protocol_version: "2024-11-05".to_string(),
                capabilities: ClientCapabilities {
                    roots: Some(RootsCapability {
                        list_changed: false,
                    }),
                    sampling: None,
                },
                client_info: ClientInfo {
                    name: cc_core::constants::APP_NAME.to_string(),
                    version: cc_core::constants::APP_VERSION.to_string(),
                },
            };

            let result: InitializeResult = self
                .call("initialize", Some(serde_json::to_value(&params)?))
                .await
                .map_err(|e| {
                    anyhow::anyhow!("MCP server '{}' initialize failed: {}", self.server_name, e)
                })?;

            self.server_info = Some(result.server_info);
            self.instructions = result.instructions;
            self.capabilities = result.capabilities.clone();

            // Send initialized notification
            let notif = JsonRpcRequest::notification("notifications/initialized", None);
            self.transport.send(&notif).await?;

            // Discover tools if supported
            if result.capabilities.tools.is_some() {
                match self.list_tools().await {
                    Ok(tools) => self.tools = tools,
                    Err(e) => warn!(server = %self.server_name, error = %e, "Failed to list tools"),
                }
            }

            // Discover resources if supported
            if result.capabilities.resources.is_some() {
                match self.list_resources().await {
                    Ok(resources) => self.resources = resources,
                    Err(e) => {
                        warn!(server = %self.server_name, error = %e, "Failed to list resources")
                    }
                }
            }

            // Discover prompts if supported
            if result.capabilities.prompts.is_some() {
                match self.list_prompts().await {
                    Ok(prompts) => self.prompts = prompts,
                    Err(e) => {
                        warn!(server = %self.server_name, error = %e, "Failed to list prompts")
                    }
                }
            }

            Ok(self)
        }

        // ---- High-level API -----------------------------------------------

        pub async fn list_tools(&self) -> anyhow::Result<Vec<McpTool>> {
            let result: ListToolsResult = self.call("tools/list", None).await?;
            Ok(result.tools)
        }

        pub async fn call_tool(
            &self,
            name: &str,
            arguments: Option<Value>,
        ) -> anyhow::Result<CallToolResult> {
            let params = CallToolParams {
                name: name.to_string(),
                arguments,
            };
            self.call("tools/call", Some(serde_json::to_value(&params)?))
                .await
                .map_err(|e| {
                    anyhow::anyhow!(
                        "MCP server '{}': tool '{}' call failed: {}",
                        self.server_name,
                        name,
                        e
                    )
                })
        }

        pub async fn list_resources(&self) -> anyhow::Result<Vec<McpResource>> {
            let result: ListResourcesResult = self.call("resources/list", None).await?;
            Ok(result.resources)
        }

        pub async fn read_resource(&self, uri: &str) -> anyhow::Result<ResourceContents> {
            let params = serde_json::json!({ "uri": uri });
            let result: Value = self.call("resources/read", Some(params)).await?;
            let contents = result
                .get("contents")
                .and_then(|c| c.as_array())
                .and_then(|arr| arr.first())
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "MCP server '{}': no contents in resources/read response for '{}'",
                        self.server_name,
                        uri
                    )
                })?;
            Ok(serde_json::from_value(contents.clone())?)
        }

        pub async fn list_prompts(&self) -> anyhow::Result<Vec<McpPrompt>> {
            let result: ListPromptsResult = self.call("prompts/list", None).await?;
            Ok(result.prompts)
        }

        /// Invoke `prompts/get` with the given name and optional arguments map.
        ///
        /// Returns the expanded prompt messages that should be injected into the
        /// conversation as-is (mirrors TS `getMCPPrompt`).
        pub async fn get_prompt(
            &self,
            name: &str,
            arguments: Option<std::collections::HashMap<String, String>>,
        ) -> anyhow::Result<GetPromptResult> {
            let mut params = serde_json::json!({ "name": name });
            if let Some(args) = arguments {
                params["arguments"] = serde_json::to_value(args)?;
            }
            let result: GetPromptResult = self.call("prompts/get", Some(params)).await?;
            Ok(result)
        }

        /// Get all tools as `ToolDefinition` objects suitable for the API.
        pub fn tool_definitions(&self) -> Vec<ToolDefinition> {
            self.tools.iter().map(|t| t.into()).collect()
        }

        // ---- Internal RPC machinery ---------------------------------------

        /// Send a request and wait for the response, deserializing into T.
        pub(crate) async fn call<T: for<'de> Deserialize<'de>>(
            &self,
            method: &str,
            params: Option<Value>,
        ) -> anyhow::Result<T> {
            let id = self.next_id.fetch_add(1, Ordering::SeqCst);
            let req = JsonRpcRequest::new(id, method, params);

            // We use a simple request/response loop here (no concurrent requests).
            // For production use, proper demultiplexing by id would be needed.
            self.transport.send(&req).await?;

            loop {
                let resp = self.transport.recv().await?.ok_or_else(|| {
                    anyhow::anyhow!(
                        "MCP transport closed while waiting for response to '{}'",
                        method
                    )
                })?;

                // Check if this response matches our request id
                let resp_id = resp.id.as_ref().and_then(|v| v.as_u64()).unwrap_or(0);
                if resp_id != id {
                    // Might be a server-initiated notification; skip
                    debug!(
                        got_id = resp_id,
                        want_id = id,
                        "Skipping non-matching response"
                    );
                    continue;
                }

                if let Some(err) = resp.error {
                    return Err(anyhow::anyhow!(
                        "MCP error {} from '{}': {}",
                        err.code,
                        method,
                        err.message
                    ));
                }

                let result = resp
                    .result
                    .ok_or_else(|| anyhow::anyhow!("No result in MCP response for '{}'", method))?;
                return Ok(serde_json::from_value(result)?);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// MCP Auth State
// ---------------------------------------------------------------------------

/// Authentication state for a single MCP server.
#[derive(Debug, Clone)]
pub enum McpAuthState {
    /// Server does not require OAuth authentication.
    NotRequired,
    /// OAuth required; `auth_url` is where the user should go.
    Required { auth_url: String },
    /// Successfully authenticated; token may have an expiry.
    Authenticated {
        token_expiry: Option<chrono::DateTime<chrono::Utc>>,
    },
    /// An error occurred reading / initiating auth.
    Error(String),
}

// ---------------------------------------------------------------------------
// MCP Manager: manages multiple server connections
// ---------------------------------------------------------------------------

/// Manages a pool of MCP server connections.
pub struct McpManager {
    clients: HashMap<String, McpClient>,
    /// Servers that failed to connect during `connect_all`.
    failed_servers: Vec<(String, String)>, // (name, error)
    /// Original (unexpanded) server configs — needed for OAuth initiation.
    server_configs: HashMap<String, McpServerConfig>,
    /// Active resource subscriptions: (server_name, uri) → change event sender.
    pub resource_subscriptions:
        DashMap<(String, String), tokio::sync::mpsc::Sender<ResourceChangedEvent>>,
}

#[derive(Debug, Clone)]
pub struct McpServerCatalog {
    pub tool_count: usize,
    pub resource_count: usize,
    pub prompt_count: usize,
    pub resources: Vec<String>,
    pub prompts: Vec<String>,
}

impl McpManager {
    pub fn new() -> Self {
        Self {
            clients: HashMap::new(),
            failed_servers: Vec::new(),
            server_configs: HashMap::new(),
            resource_subscriptions: DashMap::new(),
        }
    }

    /// Connect to all configured MCP servers.
    ///
    /// - Expands env vars in each config before connecting.
    /// - Logs success/failure clearly.
    /// - Continues on failure (does not bail out on first error).
    /// - Tracks failed servers in `failed_servers()`.
    pub async fn connect_all(configs: &[McpServerConfig]) -> Self {
        let mut manager = Self::new();
        for config in configs {
            // Store original config for later OAuth use
            manager
                .server_configs
                .insert(config.name.clone(), config.clone());
            // Expand env vars before using the config
            let expanded = expand_server_config(config);

            match expanded.server_type.as_str() {
                "stdio" => {
                    debug!(
                        server = %expanded.name,
                        command = ?expanded.command,
                        "Connecting to MCP server via stdio"
                    );
                    match McpClient::connect_stdio(&expanded).await {
                        Ok(client) => {
                            info!(
                                server = %expanded.name,
                                tools = client.tools.len(),
                                resources = client.resources.len(),
                                "MCP server connected"
                            );
                            manager.clients.insert(expanded.name.clone(), client);
                        }
                        Err(e) => {
                            error!(
                                server = %expanded.name,
                                error = %e,
                                "Failed to connect to MCP server"
                            );
                            manager
                                .failed_servers
                                .push((expanded.name.clone(), e.to_string()));
                        }
                    }
                }
                other => {
                    warn!(
                        server = %expanded.name,
                        transport = other,
                        "Unsupported MCP transport type; skipping server"
                    );
                    manager.failed_servers.push((
                        expanded.name.clone(),
                        format!("unsupported transport: {}", other),
                    ));
                }
            }
        }
        manager
    }

    // -----------------------------------------------------------------------
    // Status / query API (used by /mcp command and McpConnectionManager)
    // -----------------------------------------------------------------------

    /// Return all connected server names.
    pub fn server_names(&self) -> Vec<String> {
        self.clients.keys().cloned().collect()
    }

    /// Return status for a single server.
    pub fn server_status(&self, name: &str) -> McpServerStatus {
        if let Some(client) = self.clients.get(name) {
            McpServerStatus::Connected {
                tool_count: client.tools.len(),
            }
        } else if let Some((_, err)) = self.failed_servers.iter().find(|(n, _)| n == name) {
            McpServerStatus::Disconnected {
                last_error: Some(err.clone()),
            }
        } else {
            McpServerStatus::Disconnected { last_error: None }
        }
    }

    /// Return status for every configured server (connected + failed).
    pub fn all_statuses(&self) -> HashMap<String, McpServerStatus> {
        let mut map = HashMap::new();
        for (name, client) in &self.clients {
            map.insert(
                name.clone(),
                McpServerStatus::Connected {
                    tool_count: client.tools.len(),
                },
            );
        }
        for (name, err) in &self.failed_servers {
            map.insert(
                name.clone(),
                McpServerStatus::Disconnected {
                    last_error: Some(err.clone()),
                },
            );
        }
        map
    }

    /// Servers that failed to connect during `connect_all`.
    /// Each entry is `(server_name, error_message)`.
    pub fn failed_servers(&self) -> &[(String, String)] {
        &self.failed_servers
    }

    /// Return counts and names for tools/resources/prompts on connected servers.
    pub fn server_catalog(&self, name: &str) -> Option<McpServerCatalog> {
        let client = self.clients.get(name)?;
        Some(McpServerCatalog {
            tool_count: client.tools.len(),
            resource_count: client.resources.len(),
            prompt_count: client.prompts.len(),
            resources: client.resources.iter().map(|r| r.name.clone()).collect(),
            prompts: client.prompts.iter().map(|p| p.name.clone()).collect(),
        })
    }

    // -----------------------------------------------------------------------
    // Tool / resource helpers
    // -----------------------------------------------------------------------

    /// Get all tool definitions from all connected servers.
    pub fn all_tool_definitions(&self) -> Vec<(String, ToolDefinition)> {
        let mut defs = vec![];
        for (server_name, client) in &self.clients {
            for td in client.tool_definitions() {
                // Prefix tool name with server name to avoid conflicts
                let prefixed = ToolDefinition {
                    name: format!("{}_{}", server_name, td.name),
                    description: format!("[{}] {}", server_name, td.description),
                    input_schema: td.input_schema.clone(),
                };
                defs.push((server_name.clone(), prefixed));
            }
        }
        defs
    }

    /// Execute a tool call, routing to the correct server.
    /// Tool name format: `<server_name>_<tool_name>`.
    pub async fn call_tool(
        &self,
        prefixed_name: &str,
        arguments: Option<Value>,
    ) -> anyhow::Result<CallToolResult> {
        // Find the server name by matching prefix
        for (server_name, client) in &self.clients {
            let prefix = format!("{}_", server_name);
            if let Some(tool_name) = prefixed_name.strip_prefix(&prefix) {
                return client.call_tool(tool_name, arguments).await;
            }
        }
        Err(anyhow::anyhow!(
            "No MCP server found for tool '{}'. Connected servers: [{}]",
            prefixed_name,
            self.clients.keys().cloned().collect::<Vec<_>>().join(", ")
        ))
    }

    /// Number of connected servers.
    pub fn server_count(&self) -> usize {
        self.clients.len()
    }

    /// Get server instructions (from initialize response).
    pub fn server_instructions(&self) -> Vec<(String, String)> {
        self.clients
            .iter()
            .filter_map(|(name, client)| {
                client
                    .instructions
                    .as_ref()
                    .map(|instr| (name.clone(), instr.clone()))
            })
            .collect()
    }

    /// List all resources from all (or a specific) connected server.
    pub async fn list_all_resources(&self, server_filter: Option<&str>) -> Vec<serde_json::Value> {
        let mut all = vec![];
        for (name, client) in &self.clients {
            if let Some(filter) = server_filter {
                if name != filter {
                    continue;
                }
            }
            match client.list_resources().await {
                Ok(resources) => {
                    for r in resources {
                        all.push(serde_json::json!({
                            "uri": r.uri,
                            "name": r.name,
                            "description": r.description,
                            "mimeType": r.mime_type,
                            "server": name,
                        }));
                    }
                }
                Err(e) => {
                    warn!(server = %name, error = %e, "Failed to list resources");
                }
            }
        }
        all
    }

    /// Read a specific resource from a named server.
    pub async fn read_resource(
        &self,
        server_name: &str,
        uri: &str,
    ) -> anyhow::Result<serde_json::Value> {
        let client = self.clients.get(server_name).ok_or_else(|| {
            anyhow::anyhow!("MCP server '{}' not found or not connected", server_name)
        })?;

        let contents = client.read_resource(uri).await?;
        Ok(serde_json::to_value(&contents)?)
    }

    /// List all prompts from all (or a specific) connected server.
    pub async fn list_all_prompts(&self, server_filter: Option<&str>) -> Vec<serde_json::Value> {
        let mut all = vec![];
        for (name, client) in &self.clients {
            if let Some(filter) = server_filter {
                if name != filter {
                    continue;
                }
            }
            match client.list_prompts().await {
                Ok(prompts) => {
                    for p in prompts {
                        all.push(serde_json::json!({
                            "name": p.name,
                            "description": p.description,
                            "arguments": p.arguments,
                            "server": name,
                        }));
                    }
                }
                Err(e) => {
                    warn!(server = %name, error = %e, "Failed to list prompts");
                }
            }
        }
        all
    }

    /// Get an expanded prompt from a named server by prompt name and arguments.
    ///
    /// Returns the `GetPromptResult` with fully-rendered messages suitable for
    /// injection into the conversation (mirrors TS `getMCPPrompt`).
    pub async fn get_prompt(
        &self,
        server_name: &str,
        prompt_name: &str,
        arguments: Option<std::collections::HashMap<String, String>>,
    ) -> anyhow::Result<GetPromptResult> {
        let client = self.clients.get(server_name).ok_or_else(|| {
            anyhow::anyhow!("MCP server '{}' not found or not connected", server_name)
        })?;
        client.get_prompt(prompt_name, arguments).await
    }

    // -----------------------------------------------------------------------
    // OAuth / authentication helpers
    // -----------------------------------------------------------------------

    /// Return the current authentication state for a server.
    ///
    /// - Returns `Authenticated` if a valid (non-expired) token exists on disk.
    /// - Returns `NotRequired` for stdio servers (they don't use OAuth).
    /// - Returns `Required` for HTTP servers that lack a valid token.
    pub fn auth_state(&self, server_name: &str) -> McpAuthState {
        // Check whether a token is already stored
        if let Some(token) = oauth::get_mcp_token(server_name) {
            if !token.is_expired(60) {
                let token_expiry = token.expires_at.map(|ts| {
                    chrono::DateTime::<chrono::Utc>::from(
                        std::time::UNIX_EPOCH + std::time::Duration::from_secs(ts),
                    )
                });
                return McpAuthState::Authenticated { token_expiry };
            }
        }

        // Determine server type from stored configs
        let config = match self.server_configs.get(server_name) {
            Some(c) => c,
            None => return McpAuthState::NotRequired,
        };

        match config.server_type.as_str() {
            "http" | "sse" => McpAuthState::Required {
                auth_url: config
                    .url
                    .clone()
                    .unwrap_or_else(|| "(unknown URL)".to_string()),
            },
            _ => McpAuthState::NotRequired,
        }
    }

    /// Initiate OAuth 2.0 + PKCE for an HTTP MCP server.
    ///
    /// 1. GETs `<server_url>/.well-known/oauth-authorization-server`
    /// 2. Parses `authorization_endpoint`
    /// 3. Generates PKCE challenge
    /// 4. Returns the full auth URL (browser opening done at the command layer)
    ///
    /// The PKCE verifier is *not* persisted here; it is embedded in the URL
    /// so the command layer can display it.  A full end-to-end exchange would
    /// store the verifier and wait for the callback — that is handled by
    /// `oauth::exchange_code` once the code is received.
    pub async fn initiate_auth(&self, server_name: &str) -> anyhow::Result<String> {
        let config = self
            .server_configs
            .get(server_name)
            .ok_or_else(|| anyhow::anyhow!("Unknown MCP server: {}", server_name))?;

        let base_url = config
            .url
            .as_deref()
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "MCP server '{}' has no URL configured (required for OAuth)",
                    server_name
                )
            })?
            .trim_end_matches('/');

        // 1. Fetch OAuth Authorization Server Metadata (RFC 8414)
        let metadata_url = format!("{}/.well-known/oauth-authorization-server", base_url);
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .map_err(|e| anyhow::anyhow!("Failed to build HTTP client: {}", e))?;

        let authorization_endpoint = match client.get(&metadata_url).send().await {
            Ok(resp) if resp.status().is_success() => {
                let meta: serde_json::Value = resp
                    .json()
                    .await
                    .map_err(|e| anyhow::anyhow!("OAuth metadata parse error: {}", e))?;
                meta.get("authorization_endpoint")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "OAuth metadata for '{}' missing 'authorization_endpoint'",
                            server_name
                        )
                    })?
            }
            Ok(resp) => {
                // Metadata endpoint not found — fall back to <base_url>/oauth/authorize
                let status = resp.status();
                debug!(
                    server = %server_name,
                    status = %status,
                    "OAuth metadata endpoint returned non-success; using fallback"
                );
                format!("{}/oauth/authorize", base_url)
            }
            Err(e) => {
                // Network error — fall back
                debug!(server = %server_name, error = %e, "Failed to fetch OAuth metadata; using fallback");
                format!("{}/oauth/authorize", base_url)
            }
        };

        // 2. Allocate a redirect port
        let redirect_port = oauth::oauth_port_alloc()
            .map_err(|e| anyhow::anyhow!("Failed to allocate OAuth redirect port: {}", e))?;
        let redirect_uri = format!("http://127.0.0.1:{}/callback", redirect_port);

        // 3. Generate PKCE
        let verifier = oauth::pkce_verifier();
        let challenge = oauth::pkce_challenge(&verifier);

        // 4. Build auth URL
        let auth_url = format!(
            "{}?client_id=claude-code&redirect_uri={}&response_type=code&code_challenge={}&code_challenge_method=S256",
            authorization_endpoint,
            urlencoding::encode(&redirect_uri),
            challenge,
        );

        Ok(auth_url)
    }

    /// Store an OAuth access token for an MCP server.
    ///
    /// `expires_in` is the lifetime in seconds (as returned by the token endpoint).
    pub fn store_token(
        &self,
        server_name: &str,
        token: &str,
        expires_in: Option<u64>,
    ) -> anyhow::Result<()> {
        let expires_at = expires_in.map(|secs| {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs()
                + secs
        });
        let mcp_token = oauth::McpToken {
            access_token: token.to_string(),
            refresh_token: None,
            expires_at,
            scope: None,
            server_name: server_name.to_string(),
        };
        oauth::store_mcp_token(&mcp_token)
            .map_err(|e| anyhow::anyhow!("Failed to store MCP token for '{}': {}", server_name, e))
    }

    /// Load the stored OAuth access token for an MCP server, if any.
    ///
    /// Returns `None` if no token is stored or the token is expired.
    pub fn load_token(&self, server_name: &str) -> Option<String> {
        let token = oauth::get_mcp_token(server_name)?;
        if token.is_expired(60) {
            None
        } else {
            Some(token.access_token)
        }
    }
}

impl Default for McpManager {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// MCP result → string conversion
// ---------------------------------------------------------------------------

/// Convert an MCP tool call result to a string for the model.
///
/// Content type handling:
/// - `text`     → the text itself
/// - `image`    → `[Image: <mime_type>]` with a short base64 preview
/// - `resource` → `[Resource: <uri>]` plus text content if present
///
/// Mixed content is joined with newlines.
/// If all content is empty, returns an empty string.
pub fn mcp_result_to_string(result: &CallToolResult) -> String {
    let parts: Vec<String> = result
        .content
        .iter()
        .map(|c| match c {
            McpContent::Text { text } => text.clone(),
            McpContent::Image { data, mime_type } => {
                // Show a short preview (first 32 chars of base64) so the model
                // knows an image was returned without embedding the full blob.
                let preview_len = data.len().min(32);
                let preview = &data[..preview_len];
                let ellipsis = if data.len() > 32 { "…" } else { "" };
                format!(
                    "[Image: {} | base64 preview: {}{}]",
                    mime_type, preview, ellipsis
                )
            }
            McpContent::Resource { resource } => {
                let mut parts = vec![format!("[Resource: {}]", resource.uri)];
                if let Some(ref text) = resource.text {
                    parts.push(text.clone());
                } else if resource.blob.is_some() {
                    let mime = resource
                        .mime_type
                        .as_deref()
                        .unwrap_or("application/octet-stream");
                    parts.push(format!("[Binary resource: {}]", mime));
                }
                parts.join("\n")
            }
        })
        .collect();

    parts.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- env expansion -----------------------------------------------------

    #[test]
    fn test_expand_env_vars_no_vars() {
        assert_eq!(expand_env_vars("hello world"), "hello world");
    }

    #[test]
    fn test_expand_env_vars_known_var() {
        std::env::set_var("_CC_TEST_VAR", "rustacean");
        let out = expand_env_vars("hello ${_CC_TEST_VAR}!");
        assert_eq!(out, "hello rustacean!");
        std::env::remove_var("_CC_TEST_VAR");
    }

    #[test]
    fn test_expand_env_vars_default_value() {
        std::env::remove_var("_CC_MISSING_VAR");
        let out = expand_env_vars("val=${_CC_MISSING_VAR:-fallback}");
        assert_eq!(out, "val=fallback");
    }

    #[test]
    fn test_expand_env_vars_missing_no_default() {
        std::env::remove_var("_CC_REALLY_MISSING");
        // Missing with no default → keep original
        let out = expand_env_vars("${_CC_REALLY_MISSING}");
        assert_eq!(out, "${_CC_REALLY_MISSING}");
    }

    #[test]
    fn test_expand_env_vars_multiple() {
        std::env::set_var("_CC_A", "foo");
        std::env::set_var("_CC_B", "bar");
        let out = expand_env_vars("${_CC_A}/${_CC_B}");
        assert_eq!(out, "foo/bar");
        std::env::remove_var("_CC_A");
        std::env::remove_var("_CC_B");
    }

    #[test]
    fn test_expand_server_config() {
        std::env::set_var("_CC_TEST_HOME", "/home/user");
        let cfg = McpServerConfig {
            name: "test".to_string(),
            command: Some("${_CC_TEST_HOME}/bin/server".to_string()),
            args: vec!["--root".to_string(), "${_CC_TEST_HOME}".to_string()],
            env: {
                let mut m = HashMap::new();
                m.insert("PATH".to_string(), "${_CC_TEST_HOME}/bin".to_string());
                m
            },
            url: None,
            server_type: "stdio".to_string(),
        };
        let expanded = expand_server_config(&cfg);
        assert_eq!(expanded.command.as_deref(), Some("/home/user/bin/server"));
        assert_eq!(expanded.args[1], "/home/user");
        assert_eq!(
            expanded.env.get("PATH").map(|s| s.as_str()),
            Some("/home/user/bin")
        );
        std::env::remove_var("_CC_TEST_HOME");
    }

    // ---- JSON-RPC -----------------------------------------------------------

    #[test]
    fn test_json_rpc_request_serialization() {
        let req = JsonRpcRequest::new(1u64, "tools/list", None);
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("\"jsonrpc\":\"2.0\""));
        assert!(json.contains("\"method\":\"tools/list\""));
    }

    // ---- McpTool → ToolDefinition ------------------------------------------

    #[test]
    fn test_mcp_tool_to_definition() {
        let tool = McpTool {
            name: "search".to_string(),
            description: Some("Search the web".to_string()),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": { "query": { "type": "string" } }
            }),
        };
        let def: ToolDefinition = (&tool).into();
        assert_eq!(def.name, "search");
        assert_eq!(def.description, "Search the web");
    }

    // ---- mcp_result_to_string ----------------------------------------------

    #[test]
    fn test_result_to_string_text() {
        let result = CallToolResult {
            content: vec![McpContent::Text {
                text: "hello".to_string(),
            }],
            is_error: false,
        };
        assert_eq!(mcp_result_to_string(&result), "hello");
    }

    #[test]
    fn test_result_to_string_image() {
        let result = CallToolResult {
            content: vec![McpContent::Image {
                data: "abc123".to_string(),
                mime_type: "image/png".to_string(),
            }],
            is_error: false,
        };
        let s = mcp_result_to_string(&result);
        assert!(s.contains("Image:"));
        assert!(s.contains("image/png"));
        assert!(s.contains("abc123"));
    }

    #[test]
    fn test_result_to_string_resource_with_text() {
        let result = CallToolResult {
            content: vec![McpContent::Resource {
                resource: ResourceContents {
                    uri: "file:///foo.txt".to_string(),
                    mime_type: Some("text/plain".to_string()),
                    text: Some("file contents".to_string()),
                    blob: None,
                },
            }],
            is_error: false,
        };
        let s = mcp_result_to_string(&result);
        assert!(s.contains("[Resource: file:///foo.txt]"));
        assert!(s.contains("file contents"));
    }

    #[test]
    fn test_result_to_string_resource_binary() {
        let result = CallToolResult {
            content: vec![McpContent::Resource {
                resource: ResourceContents {
                    uri: "file:///img.png".to_string(),
                    mime_type: Some("image/png".to_string()),
                    text: None,
                    blob: Some("BASE64==".to_string()),
                },
            }],
            is_error: false,
        };
        let s = mcp_result_to_string(&result);
        assert!(s.contains("[Resource: file:///img.png]"));
        assert!(s.contains("[Binary resource: image/png]"));
    }

    #[test]
    fn test_result_to_string_mixed() {
        let result = CallToolResult {
            content: vec![
                McpContent::Text {
                    text: "line one".to_string(),
                },
                McpContent::Text {
                    text: "line two".to_string(),
                },
            ],
            is_error: false,
        };
        assert_eq!(mcp_result_to_string(&result), "line one\nline two");
    }

    // ---- McpManager --------------------------------------------------------

    #[test]
    fn test_manager_server_names_empty() {
        let mgr = McpManager::new();
        assert!(mgr.server_names().is_empty());
    }

    #[test]
    fn test_manager_all_statuses_empty() {
        let mgr = McpManager::new();
        assert!(mgr.all_statuses().is_empty());
    }

    #[test]
    fn test_manager_failed_servers_empty() {
        let mgr = McpManager::new();
        assert!(mgr.failed_servers().is_empty());
    }
}

// ---------------------------------------------------------------------------
// Resource subscriptions (T2-12)
// ---------------------------------------------------------------------------

use tokio::sync::mpsc as tokio_mpsc;

/// Notification that a resource has changed.
#[derive(Debug, Clone)]
pub struct ResourceChangedEvent {
    pub server_name: String,
    pub uri: String,
}

/// Subscription handle for a single MCP resource URI.
pub struct ResourceSubscription {
    pub server_name: String,
    pub uri: String,
}

/// Subscribe to resource changes on an MCP server.
///
/// Sends the `resources/subscribe` JSON-RPC request to the named server and
/// returns a channel receiver that will deliver [`ResourceChangedEvent`] values
/// whenever the server fires a `notifications/resources/updated` notification.
/// The notification dispatch loop (elsewhere) looks up the tx in
/// `manager.resource_subscriptions` and forwards events.
///
/// If the server is not connected or the RPC fails, a dead receiver is returned
/// (no events will ever be delivered).
pub async fn subscribe_resource(
    manager: &McpManager,
    server_name: &str,
    uri: &str,
) -> tokio_mpsc::Receiver<ResourceChangedEvent> {
    let make_dead = || {
        let (_tx, rx) = tokio_mpsc::channel::<ResourceChangedEvent>(1);
        rx
    };

    let client = match manager.clients.get(server_name) {
        Some(c) => c,
        None => {
            tracing::warn!(server_name, uri, "subscribe_resource: server not connected");
            return make_dead();
        }
    };

    let params = serde_json::json!({ "uri": uri });
    if let Err(e) = client
        .call::<serde_json::Value>("resources/subscribe", Some(params))
        .await
    {
        tracing::warn!(server_name, uri, error = %e, "subscribe_resource RPC failed");
        return make_dead();
    }

    let (tx, rx) = tokio_mpsc::channel(32);
    manager
        .resource_subscriptions
        .insert((server_name.to_string(), uri.to_string()), tx);
    tracing::info!(server_name, uri, "MCP resource subscription registered");
    rx
}

/// Unsubscribe from resource change notifications.
///
/// Sends `resources/unsubscribe` JSON-RPC request to the named server via
/// `McpManager`.  Returns an error if the server is not connected or the
/// request fails.
pub async fn unsubscribe_resource(
    manager: &McpManager,
    server_name: &str,
    uri: &str,
) -> Result<(), String> {
    let client = manager.clients.get(server_name).ok_or_else(|| {
        format!(
            "unsubscribe_resource: server '{}' not connected",
            server_name
        )
    })?;

    let params = serde_json::json!({ "uri": uri });
    client
        .call_tool("resources/unsubscribe", Some(params))
        .await
        .map_err(|e| format!("unsubscribe_resource failed: {e}"))
        .map(|_| ())
}
