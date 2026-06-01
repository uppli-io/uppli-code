// cc-core: Core types, error handling, configuration, settings, and constants
// for the Uppli Code CLI Rust port.
//
// All sub-modules are defined inline below.

// Session transcript persistence (JSONL, matches TS sessionStorage.ts schema).
pub mod session_storage;

// Attachment pipeline — assembles per-turn context attachments (T1-6).
pub mod attachments;

// Git utilities (T4-3).
pub mod git_utils;

// Utility modules ported from src/utils/
pub mod auto_mode;
pub mod crypto_utils;
pub mod format_utils;
pub mod status_notices;
pub mod token_budget;
pub mod truncate;

// Remote session sync (T3-1) — cloud_session removed (Anthropic phone-home).
pub mod remote_session;

// UPPLI.md hierarchical memory loading (T4-1).
pub mod claudemd;

// Message manipulation utilities (T4-2).
pub mod message_utils;

// Per-session file modification history (T4-6).
pub mod file_history;

// Re-export commonly used types at the crate root
pub use config::{
    Config, McpServerConfig, OutputFormat, PermissionMode, ProviderSettings, ProviderType,
    Settings, Theme,
};
pub use cost::CostTracker;
pub use error::{ClaudeError, Result};
pub use history::ConversationSession;
pub use permissions::{
    format_permission_reason, AutoPermissionHandler, InteractivePermissionHandler,
    ManagedAutoPermissionHandler, ManagedInteractivePermissionHandler, PermissionAction,
    PermissionDecision, PermissionHandler, PermissionLevel, PermissionManager, PermissionRequest,
    PermissionRule, PermissionScope, SerializedPermissionRule,
};
pub use types::{
    CitationsConfig, ContentBlock, DocumentSource, ImageSource, Message, MessageContent,
    MessageCost, Role, ToolDefinition, ToolResultContent, UsageInfo,
};

// ---------------------------------------------------------------------------
// error module
// ---------------------------------------------------------------------------
pub mod error {
    use thiserror::Error;

    /// The unified error type for the Uppli Code Rust port.
    #[derive(Error, Debug)]
    pub enum ClaudeError {
        #[error("API error: {0}")]
        Api(String),

        #[error("API error {status}: {message}")]
        ApiStatus { status: u16, message: String },

        #[error("Authentication error: {0}")]
        Auth(String),

        #[error("Permission denied: {0}")]
        PermissionDenied(String),

        #[error("Tool error: {0}")]
        Tool(String),

        #[error("IO error: {0}")]
        Io(#[from] std::io::Error),

        #[error("JSON error: {0}")]
        Json(#[from] serde_json::Error),

        #[error("HTTP error: {0}")]
        Http(#[from] reqwest::Error),

        #[error("Rate limit exceeded")]
        RateLimit,

        #[error("Context window exceeded")]
        ContextWindowExceeded,

        #[error("Max tokens reached")]
        MaxTokensReached,

        #[error("Cancelled")]
        Cancelled,

        #[error("Configuration error: {0}")]
        Config(String),

        #[error("MCP error: {0}")]
        Mcp(String),

        #[error("{0}")]
        Other(String),
    }

    /// Convenience alias used throughout the project.
    pub type Result<T> = std::result::Result<T, ClaudeError>;

    impl ClaudeError {
        /// Return `true` when the caller should retry the request.
        pub fn is_retryable(&self) -> bool {
            matches!(
                self,
                ClaudeError::RateLimit
                    | ClaudeError::ApiStatus { status: 429, .. }
                    | ClaudeError::ApiStatus { status: 529, .. }
            )
        }

        /// Return `true` for errors that mean the conversation cannot continue
        /// without intervention (e.g. compaction or context-window reset).
        pub fn is_context_limit(&self) -> bool {
            matches!(
                self,
                ClaudeError::ContextWindowExceeded | ClaudeError::MaxTokensReached
            )
        }
    }
}

// ---------------------------------------------------------------------------
// types module
// ---------------------------------------------------------------------------
pub mod types {
    use serde::{Deserialize, Serialize};
    use serde_json::Value;

    // ---- Roles -----------------------------------------------------------

    #[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
    #[serde(rename_all = "lowercase")]
    pub enum Role {
        User,
        Assistant,
    }

    // ---- Content blocks --------------------------------------------------

    #[derive(Debug, Clone, Serialize, Deserialize)]
    #[serde(tag = "type", rename_all = "snake_case")]
    pub enum ContentBlock {
        Text {
            text: String,
        },
        Image {
            source: ImageSource,
        },
        ToolUse {
            id: String,
            name: String,
            input: Value,
        },
        ToolResult {
            tool_use_id: String,
            content: ToolResultContent,
            #[serde(skip_serializing_if = "Option::is_none")]
            is_error: Option<bool>,
        },
        Thinking {
            thinking: String,
            signature: String,
        },
        RedactedThinking {
            data: String,
        },
        Document {
            source: DocumentSource,
            #[serde(skip_serializing_if = "Option::is_none")]
            title: Option<String>,
            #[serde(skip_serializing_if = "Option::is_none")]
            context: Option<String>,
            #[serde(skip_serializing_if = "Option::is_none")]
            citations: Option<CitationsConfig>,
        },
        /// A `!`-prefixed shell command invoked by the user, with its captured output.
        /// Rendered as a faint gray block with a `!command` header.
        UserLocalCommandOutput {
            command: String,
            output: String,
        },
        /// A skill/slash-command invocation entered by the user.
        /// Rendered as `▸ name args` with cyan styling.
        UserCommand {
            name: String,
            args: String,
        },
        /// A memory key/value written by the user (e.g. via `/memory`).
        /// Rendered as `# key: value` in cyan with a `Got it.` footer.
        UserMemoryInput {
            key: String,
            value: String,
        },
        /// A system-level API error, rendered as a red-bordered block.
        /// Shows first 5 lines with `[expand]` hint when truncated, and an
        /// optional `Retrying in Ns...` countdown line when `retry_secs` is set.
        SystemAPIError {
            message: String,
            #[serde(skip_serializing_if = "Option::is_none")]
            retry_secs: Option<u32>,
        },
        /// A collapsed summary of multiple read/search tool calls.
        /// Rendered as `▸ Read N files (+ M more)` on a single line.
        CollapsedReadSearch {
            tool_name: String,
            paths: Vec<String>,
            n_hidden: usize,
        },
        /// A sub-task assignment in an agentic workflow.
        /// Rendered as a cyan-bordered box with Task ID, subject, and description.
        TaskAssignment {
            id: String,
            subject: String,
            description: String,
        },
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    #[serde(untagged)]
    pub enum ToolResultContent {
        Text(String),
        Blocks(Vec<ContentBlock>),
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct ImageSource {
        #[serde(rename = "type")]
        pub source_type: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub media_type: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub data: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub url: Option<String>,
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct DocumentSource {
        #[serde(rename = "type")]
        pub source_type: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub media_type: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub data: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub url: Option<String>,
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct CitationsConfig {
        pub enabled: bool,
    }

    // ---- Messages --------------------------------------------------------

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct Message {
        pub role: Role,
        pub content: MessageContent,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub uuid: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub cost: Option<MessageCost>,
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    #[serde(untagged)]
    pub enum MessageContent {
        Text(String),
        Blocks(Vec<ContentBlock>),
    }

    impl Message {
        /// Create a simple user text message.
        pub fn user(content: impl Into<String>) -> Self {
            Self {
                role: Role::User,
                content: MessageContent::Text(content.into()),
                uuid: None,
                cost: None,
            }
        }

        /// Create a user message composed of multiple content blocks.
        pub fn user_blocks(blocks: Vec<ContentBlock>) -> Self {
            Self {
                role: Role::User,
                content: MessageContent::Blocks(blocks),
                uuid: None,
                cost: None,
            }
        }

        /// Create a simple assistant text message.
        pub fn assistant(content: impl Into<String>) -> Self {
            Self {
                role: Role::Assistant,
                content: MessageContent::Text(content.into()),
                uuid: None,
                cost: None,
            }
        }

        /// Create an assistant message composed of multiple content blocks.
        pub fn assistant_blocks(blocks: Vec<ContentBlock>) -> Self {
            Self {
                role: Role::Assistant,
                content: MessageContent::Blocks(blocks),
                uuid: None,
                cost: None,
            }
        }

        /// Extract the first text content from this message.
        pub fn get_text(&self) -> Option<&str> {
            match &self.content {
                MessageContent::Text(t) => Some(t.as_str()),
                MessageContent::Blocks(blocks) => blocks.iter().find_map(|b| {
                    if let ContentBlock::Text { text } = b {
                        Some(text.as_str())
                    } else {
                        None
                    }
                }),
            }
        }

        /// Collect all text content blocks into one concatenated string.
        pub fn get_all_text(&self) -> String {
            match &self.content {
                MessageContent::Text(t) => t.clone(),
                MessageContent::Blocks(blocks) => blocks
                    .iter()
                    .filter_map(|b| {
                        if let ContentBlock::Text { text } = b {
                            Some(text.as_str())
                        } else {
                            None
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(""),
            }
        }

        /// Return references to all `ToolUse` blocks in this message.
        pub fn get_tool_use_blocks(&self) -> Vec<&ContentBlock> {
            match &self.content {
                MessageContent::Blocks(blocks) => blocks
                    .iter()
                    .filter(|b| matches!(b, ContentBlock::ToolUse { .. }))
                    .collect(),
                _ => vec![],
            }
        }

        /// Return references to all `ToolResult` blocks in this message.
        pub fn get_tool_result_blocks(&self) -> Vec<&ContentBlock> {
            match &self.content {
                MessageContent::Blocks(blocks) => blocks
                    .iter()
                    .filter(|b| matches!(b, ContentBlock::ToolResult { .. }))
                    .collect(),
                _ => vec![],
            }
        }

        /// Return references to all `Thinking` blocks in this message.
        pub fn get_thinking_blocks(&self) -> Vec<&ContentBlock> {
            match &self.content {
                MessageContent::Blocks(blocks) => blocks
                    .iter()
                    .filter(|b| matches!(b, ContentBlock::Thinking { .. }))
                    .collect(),
                _ => vec![],
            }
        }

        /// Returns all content blocks (wrapping a single text into a vec).
        pub fn content_blocks(&self) -> Vec<ContentBlock> {
            match &self.content {
                MessageContent::Text(t) => vec![ContentBlock::Text { text: t.clone() }],
                MessageContent::Blocks(b) => b.clone(),
            }
        }

        /// Check whether this message has any tool use blocks.
        pub fn has_tool_use(&self) -> bool {
            !self.get_tool_use_blocks().is_empty()
        }

        /// Create a user message representing a `!`-prefixed local shell command with output.
        pub fn user_local_command_output(
            command: impl Into<String>,
            output: impl Into<String>,
        ) -> Self {
            Self {
                role: Role::User,
                content: MessageContent::Blocks(vec![ContentBlock::UserLocalCommandOutput {
                    command: command.into(),
                    output: output.into(),
                }]),
                uuid: None,
                cost: None,
            }
        }

        /// Create a user message representing a skill/slash-command invocation.
        pub fn user_command(name: impl Into<String>, args: impl Into<String>) -> Self {
            Self {
                role: Role::User,
                content: MessageContent::Blocks(vec![ContentBlock::UserCommand {
                    name: name.into(),
                    args: args.into(),
                }]),
                uuid: None,
                cost: None,
            }
        }

        /// Create a user message representing a memory key/value entry.
        pub fn user_memory_input(key: impl Into<String>, value: impl Into<String>) -> Self {
            Self {
                role: Role::User,
                content: MessageContent::Blocks(vec![ContentBlock::UserMemoryInput {
                    key: key.into(),
                    value: value.into(),
                }]),
                uuid: None,
                cost: None,
            }
        }

        /// Create a system message representing an API error (red-bordered block).
        pub fn system_api_error(message: impl Into<String>, retry_secs: Option<u32>) -> Self {
            Self {
                role: Role::User,
                content: MessageContent::Blocks(vec![ContentBlock::SystemAPIError {
                    message: message.into(),
                    retry_secs,
                }]),
                uuid: None,
                cost: None,
            }
        }

        /// Create a system message representing a collapsed read/search summary.
        pub fn collapsed_read_search(
            tool_name: impl Into<String>,
            paths: Vec<String>,
            n_hidden: usize,
        ) -> Self {
            Self {
                role: Role::User,
                content: MessageContent::Blocks(vec![ContentBlock::CollapsedReadSearch {
                    tool_name: tool_name.into(),
                    paths,
                    n_hidden,
                }]),
                uuid: None,
                cost: None,
            }
        }

        /// Create a system message representing a sub-task assignment.
        pub fn task_assignment(
            id: impl Into<String>,
            subject: impl Into<String>,
            description: impl Into<String>,
        ) -> Self {
            Self {
                role: Role::User,
                content: MessageContent::Blocks(vec![ContentBlock::TaskAssignment {
                    id: id.into(),
                    subject: subject.into(),
                    description: description.into(),
                }]),
                uuid: None,
                cost: None,
            }
        }
    }

    // ---- Cost / usage ----------------------------------------------------

    #[derive(Debug, Clone, Serialize, Deserialize, Default)]
    pub struct MessageCost {
        pub input_tokens: u64,
        pub output_tokens: u64,
        pub cache_creation_input_tokens: u64,
        pub cache_read_input_tokens: u64,
        pub cost_usd: f64,
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct ToolDefinition {
        pub name: String,
        pub description: String,
        pub input_schema: Value,
    }

    #[derive(Debug, Clone, Serialize, Deserialize, Default)]
    pub struct UsageInfo {
        pub input_tokens: u64,
        pub output_tokens: u64,
        #[serde(default)]
        pub cache_creation_input_tokens: u64,
        #[serde(default)]
        pub cache_read_input_tokens: u64,
    }

    impl UsageInfo {
        pub fn total_input(&self) -> u64 {
            self.input_tokens + self.cache_creation_input_tokens + self.cache_read_input_tokens
        }

        pub fn total(&self) -> u64 {
            self.total_input() + self.output_tokens
        }
    }
}

// ---------------------------------------------------------------------------
// config module
// ---------------------------------------------------------------------------
pub mod config {
    use serde::{Deserialize, Serialize};
    use std::collections::HashMap;
    use std::path::PathBuf;

    // ---- Hook configuration ----------------------------------------------

    /// Events that can trigger hooks.
    #[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
    #[serde(rename_all = "PascalCase")]
    pub enum HookEvent {
        /// Fires before a tool is executed.
        PreToolUse,
        /// Fires after a tool has returned its result.
        PostToolUse,
        /// Fires when the model finishes its turn (stop).
        Stop,
        /// Fires after the model samples a response, before tool execution.
        /// Corresponds to `hooks.PostModelTurn` in settings.json.
        PostModelTurn,
        /// Fires when the user submits a prompt.
        UserPromptSubmit,
        /// General-purpose notification event.
        Notification,
    }

    /// A single hook entry: a shell command to run on a specific event.
    #[derive(Debug, Clone, Serialize, Deserialize, Default)]
    pub struct HookEntry {
        /// Shell command to execute. Receives event JSON on stdin.
        pub command: String,
        /// Optional tool name filter — only run for this tool (PreToolUse/PostToolUse).
        #[serde(skip_serializing_if = "Option::is_none")]
        pub tool_filter: Option<String>,
        /// If true, a non-zero exit code blocks the operation.
        #[serde(default)]
        pub blocking: bool,
    }

    /// Top-level configuration values, merged from CLI args + settings file + env.
    #[derive(Debug, Clone, Serialize, Deserialize, Default)]
    pub struct Config {
        pub api_key: Option<String>,
        pub model: Option<String>,
        pub max_tokens: Option<u32>,
        pub permission_mode: PermissionMode,
        pub theme: Theme,
        #[serde(default)]
        pub output_style: Option<String>,
        pub auto_compact: bool,
        pub compact_threshold: f32,
        pub verbose: bool,
        pub output_format: OutputFormat,
        pub mcp_servers: Vec<McpServerConfig>,
        #[serde(default)]
        pub lsp_servers: Vec<crate::lsp::LspServerConfig>,
        pub allowed_tools: Vec<String>,
        pub disallowed_tools: Vec<String>,
        pub env: HashMap<String, String>,
        pub enable_all_mcp_servers: bool,
        pub custom_system_prompt: Option<String>,
        pub append_system_prompt: Option<String>,
        pub disable_claude_mds: bool,
        pub project_dir: Option<PathBuf>,
        #[serde(default)]
        pub workspace_paths: Vec<PathBuf>,
        /// Additional directories granted access via --add-dir.
        #[serde(default)]
        pub additional_dirs: Vec<PathBuf>,
        /// Event hooks: map of event → list of hook commands.
        #[serde(default)]
        pub hooks: HashMap<HookEvent, Vec<HookEntry>>,
        /// Active LLM provider (default: deepseek).
        #[serde(default)]
        pub provider: ProviderType,
        /// Named provider configurations (key = arbitrary name, e.g., "local-ollama").
        #[serde(default)]
        pub providers: HashMap<String, ProviderSettings>,
    }

    // ---- Provider configuration ---------------------------------------------

    /// Which LLM provider to use.
    #[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
    #[serde(rename_all = "lowercase")]
    pub enum ProviderType {
        /// DeepSeek via Anthropic-compatible endpoint (default).
        #[default]
        Deepseek,
        /// Ollama local inference.
        Ollama,
        /// Alibaba Cloud DashScope (Qwen3).
        Alibaba,
        /// Generic OpenAI-compatible endpoint.
        #[serde(rename = "openai")]
        OpenAiCompat,
    }

    impl std::fmt::Display for ProviderType {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            match self {
                ProviderType::Deepseek => write!(f, "deepseek"),
                ProviderType::Ollama => write!(f, "ollama"),
                ProviderType::Alibaba => write!(f, "alibaba"),
                ProviderType::OpenAiCompat => write!(f, "openai"),
            }
        }
    }

    /// Per-provider configuration block in settings.json.
    ///
    /// **Security:** API keys are NEVER stored here. They are resolved
    /// exclusively from environment variables:
    ///   - DeepSeek:  `ANTHROPIC_API_KEY` (or `DEEPSEEK_API_KEY`)
    ///   - Alibaba:   `DASHSCOPE_API_KEY`
    ///   - OpenAI:    `OPENAI_API_KEY`
    ///   - Ollama:    no key needed
    #[derive(Debug, Clone, Serialize, Deserialize, Default)]
    #[serde(rename_all = "camelCase")]
    pub struct ProviderSettings {
        /// Provider type.
        #[serde(default)]
        pub provider_type: ProviderType,
        /// API base URL (overrides default for the provider).
        pub api_base: Option<String>,
        /// Default model for this provider.
        pub model: Option<String>,
        /// Fast model for hybrid mode (optional).
        pub fast_model: Option<String>,
        /// Whether this provider supports thinking/reasoning.
        #[serde(default)]
        pub supports_thinking: Option<bool>,
    }

    #[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
    #[serde(rename_all = "camelCase")]
    pub enum PermissionMode {
        #[default]
        Default,
        AcceptEdits,
        BypassPermissions,
        Plan,
    }

    #[derive(Debug, Clone, Serialize, Deserialize, Default)]
    #[serde(rename_all = "camelCase")]
    pub enum Theme {
        #[default]
        Default,
        Dark,
        Light,
        Custom(String),
    }

    #[derive(Debug, Clone, Serialize, Deserialize, Default)]
    #[serde(rename_all = "lowercase")]
    pub enum OutputFormat {
        #[default]
        Text,
        Json,
        StreamJson,
    }

    /// Transport type for MCP server connections.
    #[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq, Eq)]
    #[serde(rename_all = "lowercase")]
    pub enum McpTransportType {
        #[default]
        Stdio,
        Unix,
        Http,
        Sse,
    }

    impl McpTransportType {
        /// Returns the string representation used in match arms and logs.
        pub fn as_str(&self) -> &'static str {
            match self {
                Self::Stdio => "stdio",
                Self::Unix => "unix",
                Self::Http => "http",
                Self::Sse => "sse",
            }
        }
    }

    impl std::fmt::Display for McpTransportType {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str(self.as_str())
        }
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct McpServerConfig {
        /// Server name. Defaults to empty when deserializing from a map value
        /// (the caller sets it from the map key).
        #[serde(default)]
        pub name: String,
        pub command: Option<String>,
        #[serde(default)]
        pub args: Vec<String>,
        #[serde(default)]
        pub env: HashMap<String, String>,
        pub url: Option<String>,
        #[serde(rename = "type", default)]
        pub server_type: McpTransportType,
    }

    // ---- Settings --------------------------------------------------------

    #[derive(Debug, Clone, Serialize, Deserialize, Default)]
    pub struct Settings {
        #[serde(default)]
        pub config: Config,
        pub version: Option<u32>,
        #[serde(default)]
        pub projects: HashMap<String, ProjectSettings>,
        #[serde(default, rename = "remoteControlAtStartup")]
        pub remote_control_at_startup: bool,
        /// Persisted permission rules saved by the user across sessions.
        #[serde(default, rename = "permissionRules")]
        pub permission_rules: Vec<crate::permissions::SerializedPermissionRule>,
        /// Names of plugins that have been explicitly enabled by the user.
        #[serde(default, rename = "enabledPlugins")]
        pub enabled_plugins: std::collections::HashSet<String>,
        /// Names of plugins that have been explicitly disabled by the user.
        #[serde(default, rename = "disabledPlugins")]
        pub disabled_plugins: std::collections::HashSet<String>,
        /// Whether the user has completed the first-launch onboarding flow.
        /// Mirrors TS `hasAcknowledgedSafetyNotice` / `hasCompletedOnboarding`.
        #[serde(default, rename = "hasCompletedOnboarding")]
        pub has_completed_onboarding: bool,
        /// App version at last launch — used to detect upgrades and show release notes.
        #[serde(default, rename = "lastSeenVersion")]
        pub last_seen_version: Option<String>,
    }

    #[derive(Debug, Clone, Serialize, Deserialize, Default)]
    pub struct ProjectSettings {
        #[serde(default)]
        pub allowed_tools: Vec<String>,
        #[serde(default)]
        pub mcp_servers: Vec<McpServerConfig>,
        pub custom_system_prompt: Option<String>,
    }

    impl Config {
        /// Resolve the effective model from config.
        ///
        /// Returns the model stored in config, or a hardcoded fallback.
        /// Prefer using `provider.capabilities().default_model` instead —
        /// this method exists for backwards compatibility.
        pub fn effective_model(&self) -> &str {
            self.model
                .as_deref()
                .unwrap_or(crate::constants::DEFAULT_MODEL)
        }

        /// Resolve the effective max-tokens.
        pub fn effective_max_tokens(&self) -> u32 {
            self.max_tokens
                .unwrap_or(crate::constants::DEFAULT_MAX_TOKENS)
        }

        /// Resolve the effective compact threshold (0.0 - 1.0).
        pub fn effective_compact_threshold(&self) -> f32 {
            if self.compact_threshold > 0.0 {
                self.compact_threshold
            } else {
                crate::constants::DEFAULT_COMPACT_THRESHOLD
            }
        }

        /// Resolve the effective output style for system-prompt assembly.
        pub fn effective_output_style(&self) -> crate::system_prompt::OutputStyle {
            self.output_style
                .as_deref()
                .map(crate::system_prompt::OutputStyle::parse_style)
                .unwrap_or_default()
        }

        /// Resolve the prompt text for the selected output style, including
        /// user-defined styles loaded from `~/.uppli/output-styles/`.
        pub fn resolve_output_style_prompt(&self) -> Option<String> {
            let style_name = self.output_style.as_deref().unwrap_or("default");
            let styles = crate::output_styles::all_styles(&Settings::config_dir());
            crate::output_styles::find_style(&styles, style_name)
                .map(|style| style.prompt.clone())
                .filter(|prompt| !prompt.trim().is_empty())
        }

        /// Resolve the API key from environment variable, OS keychain, or config.
        ///
        /// Priority:
        ///   1. `DEEPSEEK_API_KEY` or `ANTHROPIC_API_KEY` env var
        ///   2. OS keychain (macOS Keychain / Linux libsecret)
        ///   3. `config.api_key` (legacy fallback)
        pub fn resolve_api_key(&self) -> Option<String> {
            // 1. Env vars (highest priority, most secure — ephemeral)
            std::env::var("DEEPSEEK_API_KEY")
                .ok()
                .or_else(|| std::env::var("ANTHROPIC_API_KEY").ok())
                .filter(|k| !k.is_empty() && k.len() > 8)
                // 2. OS keychain
                .or_else(|| crate::keychain::get_key("deepseek"))
                // 3. Config file (legacy fallback)
                .or_else(|| self.api_key.clone())
                .filter(|k| !k.is_empty() && k.len() > 8)
        }

        /// Async variant: also checks `~/.uppli/oauth_tokens.json`.
        /// Returns `(credential, use_bearer_auth)`.
        ///   - For Console OAuth flow: credential is the stored API key, bearer=false.
        ///   - For Claude.ai OAuth flow: credential is the access token, bearer=true.
        ///
        /// Silently attempts token refresh when the access token is expired.
        pub async fn resolve_auth_async(&self) -> Option<(String, bool)> {
            // Highest priority: explicit api_key or env var
            if let Some(key) = self.resolve_api_key() {
                return Some((key, false));
            }
            // Fall back to saved OAuth tokens
            let tokens = crate::oauth::OAuthTokens::load().await?;

            // If expired and we have a refresh token, attempt silent refresh.
            // Clone the refresh token up-front so we don't borrow `tokens` during the async call.
            let refresh_token_owned = tokens.refresh_token.clone();
            let tokens = if tokens.is_expired() {
                if let Some(rt) = refresh_token_owned {
                    // Inline the refresh HTTP call (cc_core can't depend on cc_cli::oauth_flow).
                    let body = serde_json::json!({
                        "grant_type": "refresh_token",
                        "refresh_token": rt,
                        "client_id": crate::oauth::CLIENT_ID,
                        "scope": crate::oauth::ALL_SCOPES.join(" "),
                    });
                    let refreshed = 'refresh: {
                        let Ok(client) = reqwest::Client::builder()
                            .timeout(std::time::Duration::from_secs(30))
                            .build()
                        else {
                            break 'refresh None;
                        };
                        let Ok(resp) = client
                            .post(crate::oauth::TOKEN_URL)
                            .header("content-type", "application/json")
                            .json(&body)
                            .send()
                            .await
                        else {
                            break 'refresh None;
                        };
                        if !resp.status().is_success() {
                            break 'refresh None;
                        }
                        let Ok(data) = resp.json::<serde_json::Value>().await else {
                            break 'refresh None;
                        };
                        let new_at = data["access_token"].as_str().unwrap_or("").to_string();
                        if new_at.is_empty() {
                            break 'refresh None;
                        }
                        let new_rt = data["refresh_token"].as_str().map(String::from);
                        let exp_in = data["expires_in"].as_u64().unwrap_or(3600);
                        let exp_ms = chrono::Utc::now().timestamp_millis() + (exp_in as i64 * 1000);
                        let scopes: Vec<String> = data["scope"]
                            .as_str()
                            .unwrap_or("")
                            .split_whitespace()
                            .map(String::from)
                            .collect();
                        let mut r = tokens.clone();
                        r.access_token = new_at;
                        if let Some(nrt) = new_rt {
                            r.refresh_token = Some(nrt);
                        }
                        r.expires_at_ms = Some(exp_ms);
                        r.scopes = scopes;
                        let _ = r.save().await;
                        Some(r)
                    };
                    refreshed.unwrap_or(tokens)
                } else {
                    tokens // expired, no refresh token → can't fix
                }
            } else {
                tokens
            };

            tokens
                .effective_credential()
                .map(|cred| (cred.to_string(), tokens.uses_bearer_auth()))
        }

        /// Resolve the API base URL, checking `ANTHROPIC_BASE_URL` first.
        pub fn resolve_api_base(&self) -> String {
            std::env::var("ANTHROPIC_BASE_URL")
                .unwrap_or_else(|_| crate::constants::ANTHROPIC_API_BASE.to_string())
        }
    }

    impl Settings {
        /// The per-user configuration directory (`~/.uppli`).
        pub fn config_dir() -> PathBuf {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".uppli")
        }

        /// Full path to the global settings JSON file.
        pub fn global_settings_path() -> PathBuf {
            Self::config_dir().join("settings.json")
        }

        /// Load settings from disk, returning defaults when the file is missing.
        pub async fn load() -> anyhow::Result<Self> {
            let path = Self::global_settings_path();
            if path.exists() {
                let content = tokio::fs::read_to_string(&path).await?;
                Ok(serde_json::from_str(&content).unwrap_or_default())
            } else {
                Ok(Self::default())
            }
        }

        /// Persist settings to disk.
        pub async fn save(&self) -> anyhow::Result<()> {
            let path = Self::global_settings_path();
            if let Some(parent) = path.parent() {
                tokio::fs::create_dir_all(parent).await?;
            }
            let content = serde_json::to_string_pretty(self)?;
            tokio::fs::write(&path, content).await?;
            Ok(())
        }

        /// Synchronous variant used by pre-session commands.
        pub fn load_sync() -> anyhow::Result<Self> {
            let path = Self::global_settings_path();
            if path.exists() {
                let content = std::fs::read_to_string(&path)?;
                Ok(serde_json::from_str(&content).unwrap_or_default())
            } else {
                Ok(Self::default())
            }
        }

        /// Synchronous variant used by pre-session commands.
        pub fn save_sync(&self) -> anyhow::Result<()> {
            let path = Self::global_settings_path();
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let content = serde_json::to_string_pretty(self)?;
            std::fs::write(&path, content)?;
            Ok(())
        }
    }
}

// ---------------------------------------------------------------------------
// constants module
// ---------------------------------------------------------------------------
pub mod constants {
    pub const APP_NAME: &str = "uppli-code";
    pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

    // Models
    pub const DEFAULT_MODEL: &str = "deepseek-v4-pro";
    pub const SONNET_MODEL: &str = "deepseek-v4-flash";
    pub const HAIKU_MODEL: &str = "deepseek-v4-flash";
    pub const OPUS_MODEL: &str = "deepseek-v4-pro";

    // Token limits — generic fallback when per-model max_output_tokens is
    // unavailable. Real per-model caps live in `ProviderCapabilities.
    // known_models[].max_output_tokens` and are resolved in
    // `QueryConfig::from_provider` via `provider.max_output_tokens(model)`.
    // Active DeepSeek output limits (per
    // https://api-docs.deepseek.com/quick_start/pricing):
    //   - deepseek-v4-pro:   384K
    //   - deepseek-v4-flash: 384K
    //   - deepseek-reasoner: 64K  (deprecating)
    //   - deepseek-chat:     8K   (deprecating)
    pub const DEFAULT_MAX_TOKENS: u32 = 64_000;
    pub const MAX_TOKENS_HARD_LIMIT: u32 = 128_000;
    pub const DEFAULT_COMPACT_THRESHOLD: f32 = 0.9;
    pub const MAX_TURNS_DEFAULT: u32 = 100;
    pub const MAX_TOOL_ERRORS: u32 = 3;
    /// Default thinking budget for deepseek-reasoner (32K tokens).
    pub const DEFAULT_THINKING_BUDGET: u32 = 32_000;
    /// Maximum cumulative size (chars) of tool results kept in conversation
    /// history before older results are replaced with a truncation notice.
    pub const DEFAULT_TOOL_RESULT_BUDGET: usize = 150_000;

    // API endpoints & headers
    pub const ANTHROPIC_API_BASE: &str = "https://api.deepseek.com/anthropic";
    pub const ANTHROPIC_API_VERSION: &str = "2023-06-01";
    pub const ANTHROPIC_BETA_HEADER: &str = "";

    // File system
    pub const CLAUDE_MD_FILENAME: &str = "UPPLI.md";
    pub const SETTINGS_FILENAME: &str = "settings.json";
    pub const HISTORY_FILENAME: &str = "conversations";
    pub const CONFIG_DIR_NAME: &str = ".uppli";

    // Tool names
    pub const TOOL_NAME_BASH: &str = "Bash";
    pub const TOOL_NAME_FILE_EDIT: &str = "Edit";
    pub const TOOL_NAME_FILE_READ: &str = "Read";
    pub const TOOL_NAME_FILE_WRITE: &str = "Write";
    pub const TOOL_NAME_GLOB: &str = "Glob";
    pub const TOOL_NAME_GREP: &str = "Grep";
    pub const TOOL_NAME_AGENT: &str = "Agent";
    pub const TOOL_NAME_WEB_FETCH: &str = "WebFetch";
    pub const TOOL_NAME_WEB_SEARCH: &str = "WebSearch";
    pub const TOOL_NAME_TODO_WRITE: &str = "TodoWrite";
    pub const TOOL_NAME_TASK_CREATE: &str = "TaskCreate";
    pub const TOOL_NAME_TASK_GET: &str = "TaskGet";
    pub const TOOL_NAME_TASK_UPDATE: &str = "TaskUpdate";
    pub const TOOL_NAME_TASK_LIST: &str = "TaskList";
    pub const TOOL_NAME_TASK_STOP: &str = "TaskStop";
    pub const TOOL_NAME_TASK_OUTPUT: &str = "TaskOutput";
    pub const TOOL_NAME_ENTER_PLAN_MODE: &str = "EnterPlanMode";
    pub const TOOL_NAME_EXIT_PLAN_MODE: &str = "ExitPlanMode";
    pub const TOOL_NAME_ASK_USER: &str = "AskUserQuestion";
    pub const TOOL_NAME_MCP: &str = "mcp";
    pub const TOOL_NAME_NOTEBOOK_EDIT: &str = "NotebookEdit";

    // Session ID prefixes
    pub const SESSION_ID_PREFIX_BASH: &str = "b";
    pub const SESSION_ID_PREFIX_AGENT: &str = "a";
    pub const SESSION_ID_PREFIX_TEAMMATE: &str = "t";

    // Retry budget
    pub const MAX_OUTPUT_TOKENS_RETRIES: u32 = 3;
    pub const MAX_COMPACT_RETRIES: u32 = 3;

    // Stop sequences
    pub const STOP_SEQUENCE_END_OF_TURN: &str = "\n\nHuman:";
}

// ---------------------------------------------------------------------------
// context module
// ---------------------------------------------------------------------------
pub mod context {
    use std::path::PathBuf;
    use tokio::process::Command;

    /// Builds the system-level and user-level context that gets prepended to
    /// every conversation with the model.
    pub struct ContextBuilder {
        cwd: PathBuf,
        disable_claude_mds: bool,
    }

    impl ContextBuilder {
        pub fn new(cwd: PathBuf) -> Self {
            Self {
                cwd,
                disable_claude_mds: false,
            }
        }

        pub fn disable_claude_mds(mut self, val: bool) -> Self {
            self.disable_claude_mds = val;
            self
        }

        /// System context (git status, platform, IDE, etc.)
        pub async fn build_system_context(&self) -> String {
            let mut parts = vec![];

            // Platform information
            parts.push(format!("Platform: {}", std::env::consts::OS));
            parts.push(format!("Working directory: {}", self.cwd.display()));

            if let Some(git_context) = self.get_git_context().await {
                parts.push(git_context);
            }

            // IDE context — injected when an IDE extension is connected.
            if let Some(ide_ctx) = crate::attachments::get_ide_context() {
                parts.push(format!("# IDE Context\n{}", ide_ctx));
            }

            parts.join("\n\n")
        }

        /// User context (date, UPPLI.md memories, etc.)
        pub async fn build_user_context(&self) -> String {
            let mut parts = vec![];

            let date = chrono::Local::now().format("%A, %B %d, %Y").to_string();
            parts.push(format!("Today's date is {}.", date));

            if !self.disable_claude_mds {
                if let Some(claude_md) = self.find_and_read_claude_md().await {
                    parts.push(claude_md);
                }
            }

            parts.join("\n\n")
        }

        /// Gather short git status + recent log.
        async fn get_git_context(&self) -> Option<String> {
            let output = Command::new("git")
                .args(["status", "--short", "--branch"])
                .current_dir(&self.cwd)
                .output()
                .await
                .ok()?;

            if !output.status.success() {
                return None;
            }

            let status = String::from_utf8_lossy(&output.stdout).to_string();

            let log_output = Command::new("git")
                .args(["log", "--oneline", "-5"])
                .current_dir(&self.cwd)
                .output()
                .await
                .ok()?;

            let log = String::from_utf8_lossy(&log_output.stdout).to_string();

            let mut result = format!("# Git Status\n{}", status.trim());
            if !log.trim().is_empty() {
                result.push_str(&format!("\n\n# Recent Commits\n{}", log.trim()));
            }

            Some(result)
        }

        /// Walk up from cwd looking for UPPLI.md files and the global one.
        async fn find_and_read_claude_md(&self) -> Option<String> {
            let mut claude_mds = vec![];

            // Global ~/.uppli/UPPLI.md
            if let Some(home) = dirs::home_dir() {
                let global_claude_md = home
                    .join(".uppli")
                    .join(crate::constants::CLAUDE_MD_FILENAME);
                if global_claude_md.exists() {
                    if let Ok(content) = tokio::fs::read_to_string(&global_claude_md).await {
                        claude_mds.push(format!(
                            "# Memory (from {})\n{}",
                            global_claude_md.display(),
                            content
                        ));
                    }
                }
            }

            // Walk from cwd up to filesystem root, collecting UPPLI.md
            let mut dir = Some(self.cwd.as_path());
            let mut project_mds: Vec<String> = vec![];
            while let Some(d) = dir {
                let candidate = d.join(crate::constants::CLAUDE_MD_FILENAME);
                if candidate.exists() {
                    if let Ok(content) = tokio::fs::read_to_string(&candidate).await {
                        project_mds.push(format!(
                            "# Project Memory (from {})\n{}",
                            candidate.display(),
                            content
                        ));
                    }
                }
                dir = d.parent();
            }
            // Reverse so outermost directory comes first
            project_mds.reverse();
            claude_mds.extend(project_mds);

            if claude_mds.is_empty() {
                None
            } else {
                Some(claude_mds.join("\n\n"))
            }
        }
    }
}

// ---------------------------------------------------------------------------
// permissions module
// ---------------------------------------------------------------------------
pub mod permissions {
    use serde::{Deserialize, Serialize};
    use std::sync::{Arc, Mutex};

    // -----------------------------------------------------------------------
    // Danger level assigned to each tool type
    // -----------------------------------------------------------------------

    /// How dangerous a tool operation is — used as the default decision when
    /// no explicit rule matches.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    pub enum PermissionLevel {
        /// Read-only operations (Glob, Grep, Read, WebSearch, etc.).
        Read,
        /// File write/edit operations (Write, Edit).
        Write,
        /// Shell command execution (Bash).
        Execute,
        /// Outbound network access (WebFetch).
        Network,
    }

    impl PermissionLevel {
        /// Derive the permission level from a well-known tool name.
        pub fn for_tool(tool_name: &str) -> Self {
            match tool_name {
                "Bash" | "bash" => Self::Execute,
                "Write" | "Edit" | "NotebookEdit" => Self::Write,
                "WebFetch" => Self::Network,
                _ => Self::Read,
            }
        }
    }

    // -----------------------------------------------------------------------
    // Rule action & scope
    // -----------------------------------------------------------------------

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    pub enum PermissionAction {
        Allow,
        Deny,
    }

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    pub enum PermissionScope {
        /// Only lasts for the current process session.
        Session,
        /// Saved to settings.json and survives restarts.
        Persistent,
    }

    // -----------------------------------------------------------------------
    // Rule definition
    // -----------------------------------------------------------------------

    /// A single permission rule.
    ///
    /// Matches requests where:
    ///   - `tool_name` is `None` (applies to every tool) OR equals the
    ///     request tool name.
    ///   - `path_pattern` is `None` OR the glob pattern matches the
    ///     request path.
    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct PermissionRule {
        /// `None` means "applies to all tools".
        pub tool_name: Option<String>,
        /// Optional glob pattern for file / command paths.
        pub path_pattern: Option<String>,
        pub action: PermissionAction,
        pub scope: PermissionScope,
    }

    impl PermissionRule {
        /// Returns `true` when this rule matches the given tool name and
        /// optional path argument.
        pub fn matches(&self, tool_name: &str, path: Option<&str>) -> bool {
            // Tool name check
            if let Some(ref rule_tool) = self.tool_name {
                if rule_tool != tool_name {
                    return false;
                }
            }
            // Path pattern check — only when a pattern is specified
            if let Some(ref pattern) = self.path_pattern {
                let Some(p) = path else {
                    // Rule requires a path but none was provided → no match
                    return false;
                };
                let pat = match glob::Pattern::new(pattern) {
                    Ok(pat) => pat,
                    Err(_) => return false,
                };
                if !pat.matches(p) {
                    return false;
                }
            }
            true
        }
    }

    // -----------------------------------------------------------------------
    // Serialised rule (stored in settings.json)
    // -----------------------------------------------------------------------

    /// Serde-friendly representation of a `PermissionRule` saved to disk.
    #[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
    pub struct SerializedPermissionRule {
        #[serde(skip_serializing_if = "Option::is_none")]
        pub tool_name: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub path_pattern: Option<String>,
        pub action: PermissionAction,
    }

    impl From<&PermissionRule> for SerializedPermissionRule {
        fn from(r: &PermissionRule) -> Self {
            Self {
                tool_name: r.tool_name.clone(),
                path_pattern: r.path_pattern.clone(),
                action: r.action.clone(),
            }
        }
    }

    impl From<&SerializedPermissionRule> for PermissionRule {
        fn from(s: &SerializedPermissionRule) -> Self {
            Self {
                tool_name: s.tool_name.clone(),
                path_pattern: s.path_pattern.clone(),
                action: s.action.clone(),
                scope: PermissionScope::Persistent,
            }
        }
    }

    // -----------------------------------------------------------------------
    // Decision type
    // -----------------------------------------------------------------------

    /// The outcome of evaluating a permission request.
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    pub enum PermissionDecision {
        /// Unconditionally allow.
        Allow,
        /// Allow and remember permanently.
        AllowPermanently,
        /// Deny.
        Deny,
        /// Deny and remember permanently.
        DenyPermanently,
        /// Ask the user (show dialog) with an explanation of why.
        Ask { reason: String },
    }

    // -----------------------------------------------------------------------
    // Format a human-readable explanation for the dialog
    // -----------------------------------------------------------------------

    /// Build the explanation paragraph shown in the permission dialog.
    ///
    /// Mirrors the TS `createPermissionRequestMessage` / `permissionExplainer`
    /// output style.
    pub fn format_permission_reason(
        tool_name: &str,
        description: &str,
        path: Option<&str>,
        level: PermissionLevel,
    ) -> String {
        match level {
            PermissionLevel::Execute => {
                let cmd = path.unwrap_or(description);
                format!(
                    "Bash wants to run: `{}`\nThis will execute a shell command.",
                    cmd
                )
            }
            PermissionLevel::Write => {
                let target = path.unwrap_or(description);
                let extra = if target.contains("/etc/") || target.contains("\\etc\\") {
                    "\nModifying system files could affect network resolution \
                     and system configuration."
                } else if target.starts_with("~/.") || target.contains("/.") {
                    "\nThis is a hidden/configuration file."
                } else {
                    "\nThis will write to the filesystem."
                };
                format!("{} wants to write to `{}`{}", tool_name, target, extra)
            }
            PermissionLevel::Network => {
                let url = path.unwrap_or(description);
                format!(
                    "WebFetch wants to fetch: `{}`\nThis will make an outbound HTTP request.",
                    url
                )
            }
            PermissionLevel::Read => {
                let target = path.unwrap_or(description);
                format!("{} wants to read: `{}`", tool_name, target)
            }
        }
    }

    // -----------------------------------------------------------------------
    // PermissionManager
    // -----------------------------------------------------------------------

    /// Pending permission request waiting for resolution (e.g. from a bridge
    /// remote peer or the interactive TUI dialog).
    pub struct PendingPermission {
        pub tool_use_id: String,
        pub created_at: std::time::Instant,
        pub resolve_tx: tokio::sync::oneshot::Sender<PermissionDecision>,
    }

    /// Central permission manager: holds mode, session rules, persistent
    /// rules, and any in-flight pending decisions.
    pub struct PermissionManager {
        pub mode: crate::config::PermissionMode,
        /// Rules added during this session only.
        pub session_rules: Vec<PermissionRule>,
        /// Rules loaded from / saved to settings.json.
        pub persistent_rules: Vec<PermissionRule>,
        /// Pending interactive decisions keyed by tool_use_id.
        pending: Vec<PendingPermission>,
    }

    impl PermissionManager {
        /// Construct from a mode and the current settings (which may contain
        /// previously-persisted rules).
        pub fn new(
            mode: crate::config::PermissionMode,
            settings: &crate::config::Settings,
        ) -> Self {
            let persistent_rules = settings
                .permission_rules
                .iter()
                .map(PermissionRule::from)
                .collect();
            Self {
                mode,
                session_rules: Vec::new(),
                persistent_rules,
                pending: Vec::new(),
            }
        }

        // ----------------------------------------------------------------
        // Evaluation (ported from TS hasPermissionsToUseTool)
        // ----------------------------------------------------------------

        /// Evaluate whether `tool_name` should be allowed to run.
        ///
        /// Evaluation order (faithful to TS behaviour):
        /// 1. BypassPermissions → always Allow.
        /// 2. Check deny rules (persistent first, then session) → if any
        ///    matched, Deny.
        /// 3. Check allow rules (persistent first, then session) → if any
        ///    matched, Allow.
        /// 4. AcceptEdits → Allow (auto-accept file edits).
        /// 5. Plan mode → Allow reads; deny everything else.
        /// 6. Default → derive from tool danger level.
        pub fn evaluate(
            &self,
            tool_name: &str,
            description: &str,
            path: Option<&str>,
        ) -> PermissionDecision {
            use crate::config::PermissionMode;

            // Step 1 — bypass everything
            if self.mode == PermissionMode::BypassPermissions {
                return PermissionDecision::Allow;
            }

            // Steps 2–3 — evaluate explicit rules (deny has priority over
            // allow; persistent rules evaluated before session rules within
            // each polarity, matching TS rule-source ordering)
            let all_rules = self
                .persistent_rules
                .iter()
                .chain(self.session_rules.iter());

            let mut deny_matched = false;
            let mut allow_matched = false;

            for rule in all_rules {
                if rule.matches(tool_name, path) {
                    match rule.action {
                        PermissionAction::Deny => {
                            deny_matched = true;
                        }
                        PermissionAction::Allow => {
                            allow_matched = true;
                        }
                    }
                }
            }

            if deny_matched {
                return PermissionDecision::Deny;
            }

            if allow_matched {
                return PermissionDecision::Allow;
            }

            // Step 4 — AcceptEdits auto-allows everything
            if self.mode == PermissionMode::AcceptEdits {
                return PermissionDecision::Allow;
            }

            // Step 5 — Plan mode: reads only
            if self.mode == PermissionMode::Plan {
                let level = PermissionLevel::for_tool(tool_name);
                return match level {
                    PermissionLevel::Read => PermissionDecision::Allow,
                    _ => PermissionDecision::Deny,
                };
            }

            // Step 6 — Default: derive from tool danger level
            let level = PermissionLevel::for_tool(tool_name);
            match level {
                PermissionLevel::Read => PermissionDecision::Allow,
                PermissionLevel::Write | PermissionLevel::Execute | PermissionLevel::Network => {
                    let reason = format_permission_reason(tool_name, description, path, level);
                    PermissionDecision::Ask { reason }
                }
            }
        }

        // ----------------------------------------------------------------
        // Rule management
        // ----------------------------------------------------------------

        /// Add an arbitrary rule to this manager.
        pub fn add_rule(&mut self, rule: PermissionRule) {
            match rule.scope {
                PermissionScope::Session => self.session_rules.push(rule),
                PermissionScope::Persistent => self.persistent_rules.push(rule),
            }
        }

        /// Allow `tool_name` for the rest of this session.
        pub fn add_session_allow(&mut self, tool_name: &str) {
            self.session_rules.push(PermissionRule {
                tool_name: Some(tool_name.to_string()),
                path_pattern: None,
                action: PermissionAction::Allow,
                scope: PermissionScope::Session,
            });
        }

        /// Allow `tool_name` on `path` (glob) for the rest of this session.
        pub fn add_session_allow_path(&mut self, tool_name: &str, path: &str) {
            self.session_rules.push(PermissionRule {
                tool_name: Some(tool_name.to_string()),
                path_pattern: Some(path.to_string()),
                action: PermissionAction::Allow,
                scope: PermissionScope::Session,
            });
        }

        /// Allow `tool_name` persistently and save to settings.
        pub fn add_persistent_allow(
            &mut self,
            tool_name: &str,
            settings: &mut crate::config::Settings,
        ) -> crate::error::Result<()> {
            let rule = PermissionRule {
                tool_name: Some(tool_name.to_string()),
                path_pattern: None,
                action: PermissionAction::Allow,
                scope: PermissionScope::Persistent,
            };
            let serialized = SerializedPermissionRule::from(&rule);
            settings.permission_rules.push(serialized);
            settings
                .save_sync()
                .map_err(|e| crate::error::ClaudeError::Config(e.to_string()))?;
            self.persistent_rules.push(rule);
            Ok(())
        }

        /// Remove a persistent rule by index and save settings.
        pub fn remove_rule(
            &mut self,
            idx: usize,
            settings: &mut crate::config::Settings,
        ) -> crate::error::Result<()> {
            if idx >= settings.permission_rules.len() {
                return Err(crate::error::ClaudeError::Config(format!(
                    "Rule index {} out of bounds",
                    idx
                )));
            }
            settings.permission_rules.remove(idx);
            settings
                .save_sync()
                .map_err(|e| crate::error::ClaudeError::Config(e.to_string()))?;
            // Rebuild persistent_rules from the updated settings
            self.persistent_rules = settings
                .permission_rules
                .iter()
                .map(PermissionRule::from)
                .collect();
            Ok(())
        }

        // ----------------------------------------------------------------
        // Bridge / async pending permissions
        // ----------------------------------------------------------------

        /// Register a pending permission and return a receiver.  The caller
        /// awaits the receiver and gets a `PermissionDecision` when the user
        /// (or a bridge peer) resolves the request.
        pub fn register_pending(
            &mut self,
            id: String,
        ) -> tokio::sync::oneshot::Receiver<PermissionDecision> {
            let (tx, rx) = tokio::sync::oneshot::channel();
            self.pending.push(PendingPermission {
                tool_use_id: id,
                created_at: std::time::Instant::now(),
                resolve_tx: tx,
            });
            rx
        }

        /// Resolve a pending permission by `tool_use_id`, delivering
        /// `decision` to the waiting receiver.  No-op if the ID is unknown.
        pub fn resolve_pending(&mut self, id: &str, decision: PermissionDecision) {
            if let Some(pos) = self.pending.iter().position(|p| p.tool_use_id == id) {
                let pending = self.pending.remove(pos);
                let _ = pending.resolve_tx.send(decision);
            }
        }
    }

    // -----------------------------------------------------------------------
    // PermissionRequest (passed to handlers & TUI)
    // -----------------------------------------------------------------------

    #[derive(Debug, Clone)]
    pub struct PermissionRequest {
        pub tool_name: String,
        pub description: String,
        pub details: Option<String>,
        pub is_read_only: bool,
    }

    // -----------------------------------------------------------------------
    // PermissionHandler trait + handlers
    // -----------------------------------------------------------------------

    /// Trait implemented by anything that can decide whether to allow a tool.
    pub trait PermissionHandler: Send + Sync {
        fn check_permission(&self, request: &PermissionRequest) -> PermissionDecision;
        fn request_permission(&self, request: &PermissionRequest) -> PermissionDecision;
    }

    /// Handler for non-interactive / headless modes.
    ///
    /// Uses simple mode-based rules.  For rule-based evaluation backed by a
    /// `PermissionManager`, use `ManagedAutoPermissionHandler` instead.
    pub struct AutoPermissionHandler {
        pub mode: crate::config::PermissionMode,
    }

    impl PermissionHandler for AutoPermissionHandler {
        fn check_permission(&self, request: &PermissionRequest) -> PermissionDecision {
            use crate::config::PermissionMode;
            match self.mode {
                PermissionMode::BypassPermissions => PermissionDecision::Allow,
                PermissionMode::AcceptEdits => PermissionDecision::Allow,
                PermissionMode::Plan => {
                    if request.is_read_only {
                        PermissionDecision::Allow
                    } else {
                        PermissionDecision::Deny
                    }
                }
                PermissionMode::Default => {
                    if request.is_read_only {
                        PermissionDecision::Allow
                    } else {
                        PermissionDecision::Deny
                    }
                }
            }
        }

        fn request_permission(&self, request: &PermissionRequest) -> PermissionDecision {
            self.check_permission(request)
        }
    }

    /// Permission handler for interactive (TUI) mode.
    ///
    /// Uses simple mode-based rules.  For rule-based evaluation backed by a
    /// `PermissionManager`, use `ManagedInteractivePermissionHandler`.
    pub struct InteractivePermissionHandler {
        pub mode: crate::config::PermissionMode,
    }

    impl PermissionHandler for InteractivePermissionHandler {
        fn check_permission(&self, request: &PermissionRequest) -> PermissionDecision {
            use crate::config::PermissionMode;
            match self.mode {
                PermissionMode::Plan => {
                    if request.is_read_only {
                        PermissionDecision::Allow
                    } else {
                        PermissionDecision::Deny
                    }
                }
                // In Default / AcceptEdits / BypassPermissions the user is
                // watching the TUI so we allow all.
                _ => PermissionDecision::Allow,
            }
        }

        fn request_permission(&self, request: &PermissionRequest) -> PermissionDecision {
            self.check_permission(request)
        }
    }

    // ---- Manager-backed handlers -----------------------------------------

    /// Non-interactive handler backed by a shared `PermissionManager`.
    ///
    /// Delegates to `PermissionManager::evaluate`; converts `Ask` decisions
    /// into `Deny` (no interactive prompt available in headless mode).
    pub struct ManagedAutoPermissionHandler {
        pub manager: Arc<Mutex<PermissionManager>>,
    }

    impl ManagedAutoPermissionHandler {
        pub fn new(manager: Arc<Mutex<PermissionManager>>) -> Self {
            Self { manager }
        }
    }

    impl PermissionHandler for ManagedAutoPermissionHandler {
        fn check_permission(&self, request: &PermissionRequest) -> PermissionDecision {
            if let Ok(m) = self.manager.lock() {
                let decision = m.evaluate(
                    &request.tool_name,
                    &request.description,
                    request.details.as_deref(),
                );
                return match decision {
                    PermissionDecision::Ask { .. } => PermissionDecision::Deny,
                    other => other,
                };
            }
            PermissionDecision::Deny
        }

        fn request_permission(&self, request: &PermissionRequest) -> PermissionDecision {
            self.check_permission(request)
        }
    }

    /// Interactive (TUI) handler backed by a shared `PermissionManager`.
    ///
    /// Delegates to `PermissionManager::evaluate`; passes `Ask` decisions
    /// through so the TUI dialog can display them.
    pub struct ManagedInteractivePermissionHandler {
        pub manager: Arc<Mutex<PermissionManager>>,
    }

    impl ManagedInteractivePermissionHandler {
        pub fn new(manager: Arc<Mutex<PermissionManager>>) -> Self {
            Self { manager }
        }
    }

    impl PermissionHandler for ManagedInteractivePermissionHandler {
        fn check_permission(&self, request: &PermissionRequest) -> PermissionDecision {
            if let Ok(m) = self.manager.lock() {
                return m.evaluate(
                    &request.tool_name,
                    &request.description,
                    request.details.as_deref(),
                );
            }
            // If the lock is poisoned fall back to allow (user is watching)
            PermissionDecision::Allow
        }

        fn request_permission(&self, request: &PermissionRequest) -> PermissionDecision {
            self.check_permission(request)
        }
    }

    // Convenience constructor aliases used by the spec
    impl InteractivePermissionHandler {
        /// Build a manager-backed interactive handler.
        pub fn with_manager(
            manager: Arc<Mutex<PermissionManager>>,
        ) -> ManagedInteractivePermissionHandler {
            ManagedInteractivePermissionHandler::new(manager)
        }
    }

    impl AutoPermissionHandler {
        /// Build a manager-backed auto handler.
        pub fn with_manager(
            manager: Arc<Mutex<PermissionManager>>,
        ) -> ManagedAutoPermissionHandler {
            ManagedAutoPermissionHandler::new(manager)
        }
    }

    // -----------------------------------------------------------------------
    // Unit tests
    // -----------------------------------------------------------------------

    #[cfg(test)]
    mod perm_tests {
        use super::*;
        use crate::config::{PermissionMode, Settings};

        fn mgr(mode: PermissionMode) -> PermissionManager {
            PermissionManager::new(mode, &Settings::default())
        }

        #[test]
        fn bypass_always_allows() {
            let m = mgr(PermissionMode::BypassPermissions);
            assert_eq!(
                m.evaluate("Bash", "rm -rf /", None),
                PermissionDecision::Allow
            );
        }

        #[test]
        fn default_read_allows() {
            let m = mgr(PermissionMode::Default);
            assert_eq!(
                m.evaluate("Read", "read file", None),
                PermissionDecision::Allow
            );
        }

        #[test]
        fn default_bash_asks() {
            let m = mgr(PermissionMode::Default);
            match m.evaluate("Bash", "echo hello", None) {
                PermissionDecision::Ask { .. } => {}
                other => panic!("Expected Ask, got {:?}", other),
            }
        }

        #[test]
        fn session_allow_overrides_default() {
            let mut m = mgr(PermissionMode::Default);
            m.add_session_allow("Bash");
            assert_eq!(
                m.evaluate("Bash", "echo hi", None),
                PermissionDecision::Allow
            );
        }

        #[test]
        fn deny_beats_allow() {
            let mut m = mgr(PermissionMode::Default);
            m.add_session_allow("Bash");
            m.add_rule(PermissionRule {
                tool_name: Some("Bash".to_string()),
                path_pattern: None,
                action: PermissionAction::Deny,
                scope: PermissionScope::Session,
            });
            assert_eq!(
                m.evaluate("Bash", "echo hi", None),
                PermissionDecision::Deny
            );
        }

        #[test]
        fn plan_denies_writes() {
            let m = mgr(PermissionMode::Plan);
            assert_eq!(
                m.evaluate("Write", "write file", Some("/tmp/foo")),
                PermissionDecision::Deny
            );
        }

        #[test]
        fn plan_allows_reads() {
            let m = mgr(PermissionMode::Plan);
            assert_eq!(
                m.evaluate("Read", "read file", Some("/tmp/foo")),
                PermissionDecision::Allow
            );
        }

        #[test]
        fn accept_edits_allows_all() {
            let m = mgr(PermissionMode::AcceptEdits);
            assert_eq!(
                m.evaluate("Bash", "rm -rf /tmp", None),
                PermissionDecision::Allow
            );
        }

        #[test]
        fn glob_path_allow_matches() {
            let mut m = mgr(PermissionMode::Default);
            m.add_rule(PermissionRule {
                tool_name: Some("Write".to_string()),
                path_pattern: Some("/tmp/**".to_string()),
                action: PermissionAction::Allow,
                scope: PermissionScope::Session,
            });
            assert_eq!(
                m.evaluate("Write", "write", Some("/tmp/foo/bar.txt")),
                PermissionDecision::Allow
            );
        }

        #[test]
        fn glob_path_no_match_asks() {
            let mut m = mgr(PermissionMode::Default);
            m.add_rule(PermissionRule {
                tool_name: Some("Write".to_string()),
                path_pattern: Some("/tmp/**".to_string()),
                action: PermissionAction::Allow,
                scope: PermissionScope::Session,
            });
            match m.evaluate("Write", "write", Some("/etc/hosts")) {
                PermissionDecision::Ask { .. } => {}
                other => panic!("Expected Ask, got {:?}", other),
            }
        }

        #[test]
        fn format_reason_bash() {
            let s = format_permission_reason("Bash", "ls -la", None, PermissionLevel::Execute);
            assert!(s.contains("Bash wants to run"));
            assert!(s.contains("ls -la"));
        }

        #[test]
        fn format_reason_write_etc() {
            let s = format_permission_reason(
                "Write",
                "write",
                Some("/etc/hosts"),
                PermissionLevel::Write,
            );
            assert!(s.contains("/etc/hosts"));
            assert!(s.contains("system files"));
        }

        #[test]
        fn format_reason_webfetch() {
            let s = format_permission_reason(
                "WebFetch",
                "fetch",
                Some("https://example.com"),
                PermissionLevel::Network,
            );
            assert!(s.contains("https://example.com"));
            assert!(s.contains("HTTP request"));
        }
    }
}

// ---------------------------------------------------------------------------
// history module
// ---------------------------------------------------------------------------
pub mod history {
    use crate::types::Message;
    use serde::{Deserialize, Serialize};

    /// A checkpoint snapshot of conversation messages at a specific point in time.
    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct SessionCheckpoint {
        /// The message index this checkpoint was taken at (exclusive upper bound).
        pub message_idx: usize,
        /// Optional human-readable label.
        pub label: Option<String>,
        /// When this checkpoint was created.
        pub created_at: chrono::DateTime<chrono::Utc>,
        /// Snapshot of all messages up to (and including) `message_idx - 1`.
        pub snapshot: Vec<Message>,
    }

    /// A single persisted conversation session.
    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct ConversationSession {
        pub id: String,
        pub created_at: chrono::DateTime<chrono::Utc>,
        pub updated_at: chrono::DateTime<chrono::Utc>,
        pub messages: Vec<Message>,
        pub model: String,
        pub title: Option<String>,
        pub working_dir: Option<String>,
        /// Tags for filtering / searching sessions.
        #[serde(default)]
        pub tags: Vec<String>,
        /// ID of the session this was branched from, if any.
        #[serde(skip_serializing_if = "Option::is_none")]
        pub branch_from: Option<String>,
        /// Message index in the parent session at which this branch was created.
        #[serde(skip_serializing_if = "Option::is_none")]
        pub branch_at_message: Option<usize>,
        /// Remote bridge URL if this session is mirrored to a remote endpoint.
        #[serde(skip_serializing_if = "Option::is_none")]
        pub remote_session_url: Option<String>,
        /// Accumulated USD cost for this session.
        #[serde(default)]
        pub total_cost: f64,
        /// Accumulated token count for this session.
        #[serde(default)]
        pub total_tokens: u64,
        /// Saved checkpoints (rewind points) within this session.
        #[serde(default)]
        pub checkpoints: Vec<SessionCheckpoint>,
    }

    impl ConversationSession {
        pub fn new(model: String) -> Self {
            let now = chrono::Utc::now();
            Self {
                id: uuid::Uuid::new_v4().to_string(),
                created_at: now,
                updated_at: now,
                messages: vec![],
                model,
                title: None,
                working_dir: None,
                tags: vec![],
                branch_from: None,
                branch_at_message: None,
                remote_session_url: None,
                total_cost: 0.0,
                total_tokens: 0,
                checkpoints: vec![],
            }
        }

        pub fn add_message(&mut self, message: Message) {
            self.messages.push(message);
            self.updated_at = chrono::Utc::now();
        }

        pub fn message_count(&self) -> usize {
            self.messages.len()
        }

        pub fn last_user_message(&self) -> Option<&Message> {
            self.messages
                .iter()
                .rev()
                .find(|m| m.role == crate::types::Role::User)
        }
    }

    // -------------------------------------------------------------------------
    // Checkpoint helpers (synchronous, operate on a mutable session in-memory)
    // -------------------------------------------------------------------------

    /// Create a checkpoint at the current end of the session's message list.
    /// The checkpoint captures all messages currently in the session.
    pub fn create_checkpoint(session: &mut ConversationSession, label: Option<&str>) {
        let idx = session.messages.len();
        let checkpoint = SessionCheckpoint {
            message_idx: idx,
            label: label.map(|s| s.to_string()),
            created_at: chrono::Utc::now(),
            snapshot: session.messages.clone(),
        };
        session.checkpoints.push(checkpoint);
        session.updated_at = chrono::Utc::now();
    }

    /// Restore the session's messages to those saved in checkpoint `idx`.
    ///
    /// Returns the messages that were replaced (i.e. the messages discarded by
    /// the rewind).  The session's `messages` field is replaced with the
    /// checkpoint snapshot; `updated_at` is refreshed.
    ///
    /// # Panics
    /// Panics if `idx` is out of bounds (i.e. >= `session.checkpoints.len()`).
    pub fn restore_checkpoint(session: &mut ConversationSession, idx: usize) -> Vec<Message> {
        let snapshot = session.checkpoints[idx].snapshot.clone();
        let replaced = std::mem::replace(&mut session.messages, snapshot);
        session.updated_at = chrono::Utc::now();
        replaced
    }

    // -------------------------------------------------------------------------
    // Persistent storage helpers
    // -------------------------------------------------------------------------

    /// The on-disk directory for conversation sessions.
    fn sessions_dir() -> std::path::PathBuf {
        crate::config::Settings::config_dir().join("sessions")
    }

    /// Save a session to `~/.uppli/sessions/<id>.json`.
    pub async fn save_session(session: &ConversationSession) -> anyhow::Result<()> {
        let dir = sessions_dir();
        tokio::fs::create_dir_all(&dir).await?;
        let path = dir.join(format!("{}.json", session.id));
        let content = serde_json::to_string_pretty(session)?;
        tokio::fs::write(&path, content).await?;
        Ok(())
    }

    /// Load a specific session by ID.
    pub async fn load_session(id: &str) -> anyhow::Result<ConversationSession> {
        let path = sessions_dir().join(format!("{}.json", id));
        let content = tokio::fs::read_to_string(&path).await?;
        Ok(serde_json::from_str(&content)?)
    }

    /// List all sessions, sorted by most-recently-updated first.
    pub async fn list_sessions() -> Vec<ConversationSession> {
        let dir = sessions_dir();
        if !dir.exists() {
            return vec![];
        }

        let mut sessions = vec![];
        if let Ok(mut entries) = tokio::fs::read_dir(&dir).await {
            while let Ok(Some(entry)) = entries.next_entry().await {
                let path = entry.path();
                if path.extension().and_then(|s| s.to_str()) == Some("json") {
                    if let Ok(content) = tokio::fs::read_to_string(&path).await {
                        if let Ok(session) = serde_json::from_str::<ConversationSession>(&content) {
                            sessions.push(session);
                        }
                    }
                }
            }
        }

        sessions.sort_by_key(|s| std::cmp::Reverse(s.updated_at));
        sessions
    }

    /// Delete a session by ID.
    pub async fn delete_session(id: &str) -> anyhow::Result<()> {
        let path = sessions_dir().join(format!("{}.json", id));
        if path.exists() {
            tokio::fs::remove_file(&path).await?;
        }
        Ok(())
    }

    /// Rename (set the title of) a session.
    pub async fn rename_session(id: &str, new_title: &str) -> anyhow::Result<()> {
        let mut session = load_session(id).await?;
        session.title = Some(new_title.to_string());
        session.updated_at = chrono::Utc::now();
        save_session(&session).await
    }

    /// Add a tag to a session (idempotent — duplicate tags are ignored).
    pub async fn tag_session(id: &str, tag: &str) -> anyhow::Result<()> {
        let mut session = load_session(id).await?;
        let tag_str = tag.to_string();
        if !session.tags.contains(&tag_str) {
            session.tags.push(tag_str);
            session.updated_at = chrono::Utc::now();
            save_session(&session).await?;
        }
        Ok(())
    }

    /// Remove a tag from a session (no-op if tag is not present).
    pub async fn untag_session(id: &str, tag: &str) -> anyhow::Result<()> {
        let mut session = load_session(id).await?;
        let before_len = session.tags.len();
        session.tags.retain(|t| t != tag);
        if session.tags.len() != before_len {
            session.updated_at = chrono::Utc::now();
            save_session(&session).await?;
        }
        Ok(())
    }

    /// Create a new session that is a branch of `source_id` at message index
    /// `at_message_idx`.  The new session starts with messages
    /// `[0, at_message_idx)` copied from the source.
    pub async fn branch_session(
        source_id: &str,
        at_message_idx: usize,
        new_title: Option<&str>,
    ) -> anyhow::Result<ConversationSession> {
        let source = load_session(source_id).await?;
        let clamped_idx = at_message_idx.min(source.messages.len());
        let now = chrono::Utc::now();
        let branched = ConversationSession {
            id: uuid::Uuid::new_v4().to_string(),
            created_at: now,
            updated_at: now,
            messages: source.messages[..clamped_idx].to_vec(),
            model: source.model.clone(),
            title: new_title
                .map(|t| t.to_string())
                .or_else(|| source.title.as_ref().map(|t| format!("{} (branch)", t))),
            working_dir: source.working_dir.clone(),
            tags: source.tags.clone(),
            branch_from: Some(source_id.to_string()),
            branch_at_message: Some(clamped_idx),
            remote_session_url: None,
            total_cost: 0.0,
            total_tokens: 0,
            checkpoints: vec![],
        };
        save_session(&branched).await?;
        Ok(branched)
    }

    /// Search sessions whose title or tags contain `query` (case-insensitive
    /// substring match).  Results are sorted by `updated_at` descending.
    pub async fn search_sessions(query: &str) -> Vec<ConversationSession> {
        let lower_query = query.to_lowercase();
        let all = list_sessions().await;
        all.into_iter()
            .filter(|s| {
                // Check title
                if let Some(ref title) = s.title {
                    if title.to_lowercase().contains(&lower_query) {
                        return true;
                    }
                }
                // Check tags
                if s.tags
                    .iter()
                    .any(|t| t.to_lowercase().contains(&lower_query))
                {
                    return true;
                }
                false
            })
            .collect()
    }
}

// ---------------------------------------------------------------------------
// cost module
// ---------------------------------------------------------------------------
pub mod cost {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;

    /// Per-model pricing tiers (USD per million tokens).
    ///
    /// PR P v2 (2026-05-30) restored pricing after the initial drop in v1 was
    /// reverted on review: exposing per-run cost is a product differentiator
    /// downstream apps rely on (e.g. catalog enrichment that refactures to a
    /// client). Pricing is BEST EFFORT — provider promos and tariff changes
    /// cause drift; the provider dashboard remains the source of truth for
    /// billing. Budget enforcement is **separate** and uses tokens
    /// (see `QueryConfig.max_total_tokens` / `--max-tokens-total`).
    #[derive(Debug, Clone, Copy)]
    pub struct ModelPricing {
        pub input_per_mtk: f64,
        pub output_per_mtk: f64,
        pub cache_creation_per_mtk: f64,
        pub cache_read_per_mtk: f64,
    }

    impl ModelPricing {
        /// Zero pricing (used when provider doesn't report pricing, e.g., Ollama).
        pub const FREE: Self = Self {
            input_per_mtk: 0.0,
            output_per_mtk: 0.0,
            cache_creation_per_mtk: 0.0,
            cache_read_per_mtk: 0.0,
        };
    }

    impl Default for ModelPricing {
        fn default() -> Self {
            Self::FREE
        }
    }

    /// Thread-safe, lock-free token + cost tracker.
    ///
    /// Tokens are objective (provider-reported) and drive budget enforcement.
    /// USD cost is derived from configured pricing and exposed for display
    /// and downstream consumption (bridge, session storage, stats).
    #[derive(Debug, Default)]
    pub struct CostTracker {
        input_tokens: AtomicU64,
        output_tokens: AtomicU64,
        cache_creation_tokens: AtomicU64,
        cache_read_tokens: AtomicU64,
        pricing: parking_lot::RwLock<ModelPricing>,
    }

    impl CostTracker {
        pub fn new() -> Arc<Self> {
            Arc::new(Self::default())
        }

        /// Create a tracker with explicit pricing (from provider.model_pricing()).
        pub fn with_pricing(pricing: ModelPricing) -> Arc<Self> {
            Arc::new(Self {
                pricing: parking_lot::RwLock::new(pricing),
                ..Default::default()
            })
        }

        /// Update pricing (e.g., when the model changes mid-session).
        pub fn set_pricing(&self, pricing: ModelPricing) {
            *self.pricing.write() = pricing;
        }

        pub fn add_usage(&self, input: u64, output: u64, cache_creation: u64, cache_read: u64) {
            self.input_tokens.fetch_add(input, Ordering::Relaxed);
            self.output_tokens.fetch_add(output, Ordering::Relaxed);
            self.cache_creation_tokens
                .fetch_add(cache_creation, Ordering::Relaxed);
            self.cache_read_tokens
                .fetch_add(cache_read, Ordering::Relaxed);
        }

        pub fn total_tokens(&self) -> u64 {
            self.input_tokens.load(Ordering::Relaxed)
                + self.output_tokens.load(Ordering::Relaxed)
                + self.cache_creation_tokens.load(Ordering::Relaxed)
                + self.cache_read_tokens.load(Ordering::Relaxed)
        }

        pub fn input_tokens(&self) -> u64 {
            self.input_tokens.load(Ordering::Relaxed)
        }

        pub fn output_tokens(&self) -> u64 {
            self.output_tokens.load(Ordering::Relaxed)
        }

        pub fn cache_creation_tokens(&self) -> u64 {
            self.cache_creation_tokens.load(Ordering::Relaxed)
        }

        pub fn cache_read_tokens(&self) -> u64 {
            self.cache_read_tokens.load(Ordering::Relaxed)
        }

        /// Computed USD cost from accumulated tokens × current pricing.
        /// Returns 0.0 when no pricing is configured (Ollama or unknown model).
        pub fn total_cost_usd(&self) -> f64 {
            let pricing = *self.pricing.read();
            let input = self.input_tokens.load(Ordering::Relaxed) as f64;
            let output = self.output_tokens.load(Ordering::Relaxed) as f64;
            let cache_creation = self.cache_creation_tokens.load(Ordering::Relaxed) as f64;
            let cache_read = self.cache_read_tokens.load(Ordering::Relaxed) as f64;

            (input * pricing.input_per_mtk
                + output * pricing.output_per_mtk
                + cache_creation * pricing.cache_creation_per_mtk
                + cache_read * pricing.cache_read_per_mtk)
                / 1_000_000.0
        }

        /// Produce a human-readable summary string, e.g. for display in the TUI.
        pub fn summary(&self) -> String {
            let total = self.total_tokens();
            let input = self.input_tokens();
            let output = self.output_tokens();
            let cost = self.total_cost_usd();
            if cost > 0.0 {
                format!(
                    "{} tokens ({} in, {} out) · ${:.4}",
                    total, input, output, cost
                )
            } else {
                format!("{} tokens ({} in, {} out)", total, input, output)
            }
        }
    }
}

// ---------------------------------------------------------------------------
// hooks module
// ---------------------------------------------------------------------------
pub mod hooks {
    use crate::config::{HookEntry, HookEvent};
    use serde_json::Value;
    use std::collections::HashMap;
    use std::path::Path;
    use tracing::{debug, warn};

    /// Context passed to hook commands via stdin as JSON.
    #[derive(Debug, serde::Serialize)]
    pub struct HookContext {
        pub event: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub tool_name: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub tool_input: Option<Value>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub tool_output: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub is_error: Option<bool>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub session_id: Option<String>,
    }

    /// Result of running a hook.
    #[derive(Debug)]
    pub enum HookOutcome {
        /// Hook ran and allowed execution to continue.
        Allowed,
        /// Hook ran and blocked execution (blocking hook with non-zero exit).
        Blocked(String),
        /// Hook produced modified output (stdout of the hook command).
        Modified(String),
    }

    /// Run all hooks registered for the given event. Returns the first blocking
    /// result if any hook blocks, otherwise `Allowed`.
    pub async fn run_hooks(
        hooks: &HashMap<HookEvent, Vec<HookEntry>>,
        event: HookEvent,
        ctx: &HookContext,
        working_dir: &Path,
    ) -> HookOutcome {
        let Some(entries) = hooks.get(&event) else {
            return HookOutcome::Allowed;
        };

        let ctx_json = match serde_json::to_string(ctx) {
            Ok(j) => j,
            Err(e) => {
                warn!("Failed to serialize hook context: {}", e);
                return HookOutcome::Allowed;
            }
        };

        for entry in entries {
            // Apply tool filter if set
            if let Some(ref filter) = entry.tool_filter {
                if let Some(ref tool) = ctx.tool_name {
                    if !filter.is_empty() && filter != tool && filter != "*" {
                        continue;
                    }
                }
            }

            debug!(command = %entry.command, event = ?event, "Running hook");

            let result = tokio::process::Command::new(if cfg!(windows) { "cmd" } else { "sh" })
                .args(if cfg!(windows) {
                    ["/C", &entry.command]
                } else {
                    ["-c", &entry.command]
                })
                .current_dir(working_dir)
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn();

            let mut child = match result {
                Ok(c) => c,
                Err(e) => {
                    warn!(command = %entry.command, error = %e, "Failed to spawn hook");
                    continue;
                }
            };

            // Write context JSON to stdin
            if let Some(mut stdin) = child.stdin.take() {
                use tokio::io::AsyncWriteExt;
                let _ = stdin.write_all(ctx_json.as_bytes()).await;
            }

            let output = match child.wait_with_output().await {
                Ok(o) => o,
                Err(e) => {
                    warn!(command = %entry.command, error = %e, "Hook wait failed");
                    continue;
                }
            };

            let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
            let exit_ok = output.status.success();

            if !exit_ok && entry.blocking {
                let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
                let reason = if !stderr.is_empty() { stderr } else { stdout };
                return HookOutcome::Blocked(format!(
                    "Hook '{}' blocked execution: {}",
                    entry.command,
                    reason.trim()
                ));
            }

            if !stdout.trim().is_empty() {
                return HookOutcome::Modified(stdout.trim().to_string());
            }
        }

        HookOutcome::Allowed
    }
}

// ---------------------------------------------------------------------------
// oauth module
// ---------------------------------------------------------------------------

/// OAuth 2.0 PKCE authentication support.
///
/// Supports two login paths mirroring the TypeScript implementation:
/// - **Console** (`org:create_api_key` scope): exchanges access token for an API key.
/// - **Claude.ai** (`user:inference` scope): uses the access token as a Bearer credential.
pub mod oauth {
    use serde::{Deserialize, Serialize};

    // ---- Production OAuth endpoints & constants ----

    // OAuth endpoints — intentionally empty (Uppli Code uses API keys).
    pub const CLIENT_ID: &str = "";
    pub const CONSOLE_AUTHORIZE_URL: &str = "";
    pub const CLAUDE_AI_AUTHORIZE_URL: &str = "";
    pub const TOKEN_URL: &str = "";
    pub const API_KEY_URL: &str = "";
    pub const MANUAL_REDIRECT_URL: &str = "";
    pub const CLAUDEAI_SUCCESS_URL: &str = "";
    pub const CONSOLE_SUCCESS_URL: &str = "";

    /// All scopes requested during login (union of Console + Claude.ai scopes).
    pub const ALL_SCOPES: &[&str] = &[
        "org:create_api_key",
        "user:profile",
        "user:inference",
        "user:sessions:claude_code",
        "user:mcp_servers",
        "user:file_upload",
    ];

    /// Scope that identifies a Claude.ai subscription token (uses Bearer auth).
    pub const CLAUDE_AI_INFERENCE_SCOPE: &str = "user:inference";

    // ---- Stored token struct ----

    /// Persisted OAuth tokens (saved to `~/.uppli/oauth_tokens.json`).
    #[derive(Debug, Clone, Serialize, Deserialize, Default)]
    pub struct OAuthTokens {
        pub access_token: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub refresh_token: Option<String>,
        /// Unix timestamp in milliseconds when the access token expires.
        #[serde(skip_serializing_if = "Option::is_none")]
        pub expires_at_ms: Option<i64>,
        pub scopes: Vec<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub account_uuid: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub email: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub organization_uuid: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub subscription_type: Option<String>,
        /// API key created for Console-flow users (exchanged from access token).
        #[serde(skip_serializing_if = "Option::is_none")]
        pub api_key: Option<String>,
    }

    impl OAuthTokens {
        /// Returns true if the token requires Bearer-style authorization
        /// (i.e. Claude.ai subscription with `user:inference` scope).
        pub fn uses_bearer_auth(&self) -> bool {
            self.scopes.iter().any(|s| s == CLAUDE_AI_INFERENCE_SCOPE)
        }

        /// The credential to present to the Anthropic API:
        /// - Console flow: the stored `api_key` (sk-ant-…)
        /// - Claude.ai flow: the `access_token` itself (Bearer)
        pub fn effective_credential(&self) -> Option<&str> {
            if self.uses_bearer_auth() {
                if self.access_token.is_empty() {
                    None
                } else {
                    Some(&self.access_token)
                }
            } else {
                self.api_key.as_deref()
            }
        }

        /// True if the access token has passed (or is within 5 minutes of) its expiry.
        pub fn is_expired(&self) -> bool {
            if let Some(exp) = self.expires_at_ms {
                let buffer_ms: i64 = 5 * 60 * 1000;
                let now_ms = chrono::Utc::now().timestamp_millis();
                (now_ms + buffer_ms) >= exp
            } else {
                false
            }
        }

        pub fn token_file_path() -> std::path::PathBuf {
            dirs::home_dir()
                .unwrap_or_else(|| std::path::PathBuf::from("."))
                .join(".uppli")
                .join("oauth_tokens.json")
        }

        pub async fn save(&self) -> anyhow::Result<()> {
            let path = Self::token_file_path();
            if let Some(parent) = path.parent() {
                tokio::fs::create_dir_all(parent).await?;
            }
            tokio::fs::write(&path, serde_json::to_string_pretty(self)?).await?;
            Ok(())
        }

        pub async fn load() -> Option<Self> {
            let path = Self::token_file_path();
            let content = tokio::fs::read_to_string(&path).await.ok()?;
            serde_json::from_str(&content).ok()
        }

        pub async fn clear() -> anyhow::Result<()> {
            let path = Self::token_file_path();
            if path.exists() {
                tokio::fs::remove_file(&path).await?;
            }
            Ok(())
        }
    }

    // ---- PKCE helpers ----

    /// Generate a 32-byte random code verifier, base64url-encoded (no padding).
    pub fn generate_code_verifier() -> String {
        use base64::Engine;
        let mut bytes = [0u8; 32];
        let u1 = uuid::Uuid::new_v4();
        let u2 = uuid::Uuid::new_v4();
        bytes[..16].copy_from_slice(u1.as_bytes());
        bytes[16..].copy_from_slice(u2.as_bytes());
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
    }

    /// Derive the PKCE code challenge from a verifier: BASE64URL(SHA256(verifier)).
    pub fn generate_code_challenge(verifier: &str) -> String {
        use base64::Engine;
        use sha2::{Digest, Sha256};
        let hash = Sha256::digest(verifier.as_bytes());
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(hash)
    }

    /// Generate a random OAuth state parameter for CSRF protection.
    pub fn generate_state() -> String {
        use base64::Engine;
        let mut bytes = [0u8; 32];
        let u1 = uuid::Uuid::new_v4();
        let u2 = uuid::Uuid::new_v4();
        bytes[..16].copy_from_slice(u1.as_bytes());
        bytes[16..].copy_from_slice(u2.as_bytes());
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
    }

    // ---- URL builder ----

    /// Build an OAuth authorization URL with all required PKCE parameters.
    pub fn build_auth_url(
        authorize_base: &str,
        code_challenge: &str,
        state: &str,
        callback_port: u16,
        is_manual: bool,
    ) -> String {
        let mut u = url::Url::parse(authorize_base).expect("valid OAuth authorize base URL");
        {
            let mut q = u.query_pairs_mut();
            q.append_pair("code", "true"); // tells the login page to show Claude Max upsell
            q.append_pair("client_id", CLIENT_ID);
            q.append_pair("response_type", "code");
            let redirect = if is_manual {
                MANUAL_REDIRECT_URL.to_string()
            } else {
                format!("http://localhost:{}/callback", callback_port)
            };
            q.append_pair("redirect_uri", &redirect);
            q.append_pair("scope", &ALL_SCOPES.join(" "));
            q.append_pair("code_challenge", code_challenge);
            q.append_pair("code_challenge_method", "S256");
            q.append_pair("state", state);
        }
        u.to_string()
    }
}

// Re-export OAuthTokens at crate root for convenience
pub use oauth::OAuthTokens;

// ---------------------------------------------------------------------------
// New modules: keybindings, analytics, lsp, system_prompt, memdir, oauth_config
// Removed: voice (OpenAI Whisper), settings_sync, team_memory_sync,
//          remote_settings (Anthropic phone-home)
// ---------------------------------------------------------------------------
pub mod analytics;
pub mod bash_classifier;

// OS keychain integration for secure API key storage.
pub mod effort;
pub mod feature_gates;
pub mod keybindings;
pub mod keychain;
pub mod lsp;
pub mod memdir;
pub mod migrations;
pub mod oauth_config;
pub mod output_styles;
pub mod prompt_history;
pub mod system_prompt;
pub mod tips;

// ---------------------------------------------------------------------------
// tasks module — background task registry
// ---------------------------------------------------------------------------
pub mod tasks {
    use chrono::{DateTime, Utc};
    use dashmap::DashMap;
    use once_cell::sync::Lazy;
    use serde::{Deserialize, Serialize};
    use std::sync::Arc;
    use uuid::Uuid;

    /// Current status of a background task.
    #[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
    pub enum TaskStatus {
        Running,
        Completed,
        Failed(String),
        Cancelled,
    }

    impl std::fmt::Display for TaskStatus {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            match self {
                TaskStatus::Running => write!(f, "running"),
                TaskStatus::Completed => write!(f, "completed"),
                TaskStatus::Failed(reason) => write!(f, "failed: {}", reason),
                TaskStatus::Cancelled => write!(f, "cancelled"),
            }
        }
    }

    /// A single background task tracked by the registry.
    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct BackgroundTask {
        /// Unique identifier for the task.
        pub id: String,
        /// Human-readable name / description.
        pub name: String,
        /// Current execution status.
        pub status: TaskStatus,
        /// When the task was registered.
        pub started_at: DateTime<Utc>,
        /// When the task finished (completed, failed, or cancelled).
        pub completed_at: Option<DateTime<Utc>>,
        /// Lines of output produced by the task.
        pub output: Vec<String>,
        /// OS process ID, if applicable.
        pub pid: Option<u32>,
    }

    impl BackgroundTask {
        /// Create a new running task with the given name.
        pub fn new(name: impl Into<String>) -> Self {
            Self {
                id: Uuid::new_v4().to_string(),
                name: name.into(),
                status: TaskStatus::Running,
                started_at: Utc::now(),
                completed_at: None,
                output: Vec::new(),
                pid: None,
            }
        }

        /// Return `true` if the task is still running.
        pub fn is_running(&self) -> bool {
            matches!(self.status, TaskStatus::Running)
        }
    }

    /// Thread-safe registry of background tasks.
    pub struct TaskRegistry {
        tasks: Arc<DashMap<String, BackgroundTask>>,
    }

    impl TaskRegistry {
        /// Create a new empty registry.
        pub fn new() -> Self {
            Self {
                tasks: Arc::new(DashMap::new()),
            }
        }

        /// Register a new task.  Returns the assigned task ID.
        pub fn register(&self, task: BackgroundTask) -> String {
            let id = task.id.clone();
            self.tasks.insert(id.clone(), task);
            id
        }

        /// Update the status of a task.  No-op if the ID is unknown.
        pub fn update_status(&self, id: &str, status: TaskStatus) {
            if let Some(mut entry) = self.tasks.get_mut(id) {
                let is_terminal = !matches!(status, TaskStatus::Running);
                entry.status = status;
                if is_terminal && entry.completed_at.is_none() {
                    entry.completed_at = Some(Utc::now());
                }
            }
        }

        /// Append a line of output to an existing task.  No-op if unknown.
        pub fn append_output(&self, id: &str, line: &str) {
            if let Some(mut entry) = self.tasks.get_mut(id) {
                entry.output.push(line.to_string());
            }
        }

        /// Look up a task by ID.
        pub fn get(&self, id: &str) -> Option<BackgroundTask> {
            self.tasks.get(id).map(|e| e.clone())
        }

        /// Return a snapshot of all tasks, ordered by `started_at` ascending.
        pub fn list(&self) -> Vec<BackgroundTask> {
            let mut tasks: Vec<BackgroundTask> =
                self.tasks.iter().map(|e| e.value().clone()).collect();
            tasks.sort_by_key(|t| t.started_at);
            tasks
        }

        /// Mark a task as `Completed`.  No-op if unknown or already terminal.
        pub fn complete(&self, id: &str) {
            self.update_status(id, TaskStatus::Completed);
        }

        /// Mark a task as `Cancelled`.  No-op if unknown or already terminal.
        pub fn cancel(&self, id: &str) {
            self.update_status(id, TaskStatus::Cancelled);
        }

        /// Set the OS process ID for a task.  No-op if unknown.
        pub fn set_pid(&self, id: &str, pid: u32) {
            if let Some(mut entry) = self.tasks.get_mut(id) {
                entry.pid = Some(pid);
            }
        }
    }

    impl Default for TaskRegistry {
        fn default() -> Self {
            Self::new()
        }
    }

    /// The process-global task registry singleton.
    static GLOBAL_REGISTRY: Lazy<TaskRegistry> = Lazy::new(TaskRegistry::new);

    /// Return a reference to the process-global `TaskRegistry`.
    pub fn global_registry() -> &'static TaskRegistry {
        &GLOBAL_REGISTRY
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_message_user() {
        let msg = Message::user("hello");
        assert_eq!(msg.role, Role::User);
        assert_eq!(msg.get_text(), Some("hello"));
    }

    #[test]
    fn test_message_assistant_blocks() {
        let msg = Message::assistant_blocks(vec![
            ContentBlock::Thinking {
                thinking: "let me think".into(),
                signature: "sig".into(),
            },
            ContentBlock::Text {
                text: "response".into(),
            },
        ]);
        assert_eq!(msg.get_text(), Some("response"));
        assert_eq!(msg.get_thinking_blocks().len(), 1);
    }

    #[test]
    fn test_hooks_config_default() {
        let cfg = crate::config::Config::default();
        assert!(cfg.hooks.is_empty());
    }

    #[test]
    fn test_cost_tracker_token_accumulation() {
        let tracker = CostTracker::new();
        tracker.add_usage(1000, 500, 200, 100);
        assert_eq!(tracker.input_tokens(), 1000);
        assert_eq!(tracker.output_tokens(), 500);
        assert_eq!(tracker.cache_creation_tokens(), 200);
        assert_eq!(tracker.cache_read_tokens(), 100);
        assert_eq!(tracker.total_tokens(), 1800);
    }

    /// PR P regression: total_tokens MUST sum input+output+cache_creation+cache_read.
    /// The previous (decorative) version asserted nothing; this one validates
    /// the actual accumulation contract.
    #[test]
    fn test_cost_tracker_total_tokens_is_sum_of_all_components() {
        let tracker = CostTracker::new();
        tracker.add_usage(11, 23, 47, 59);
        assert_eq!(tracker.input_tokens(), 11);
        assert_eq!(tracker.output_tokens(), 23);
        assert_eq!(tracker.cache_creation_tokens(), 47);
        assert_eq!(tracker.cache_read_tokens(), 59);
        assert_eq!(
            tracker.total_tokens(),
            11 + 23 + 47 + 59,
            "total_tokens must include input+output+cache_creation+cache_read"
        );
        // Add more usage and verify accumulation
        tracker.add_usage(1, 2, 3, 4);
        assert_eq!(
            tracker.total_tokens(),
            11 + 23 + 47 + 59 + 1 + 2 + 3 + 4,
            "total_tokens must accumulate across multiple add_usage calls"
        );
    }

    #[test]
    fn test_error_retryable() {
        assert!(ClaudeError::RateLimit.is_retryable());
        assert!(ClaudeError::ApiStatus {
            status: 429,
            message: "rate limited".into()
        }
        .is_retryable());
        assert!(!ClaudeError::Auth("bad key".into()).is_retryable());
    }

    // ---- Config tests -------------------------------------------------------

    #[test]
    fn test_config_effective_model_default() {
        let cfg = crate::config::Config::default();
        assert_eq!(cfg.effective_model(), crate::constants::DEFAULT_MODEL);
    }

    #[test]
    fn test_config_effective_model_override() {
        let cfg = crate::config::Config {
            model: Some("claude-haiku-4-5-20251001".to_string()),
            ..Default::default()
        };
        assert_eq!(cfg.effective_model(), "claude-haiku-4-5-20251001");
    }

    #[test]
    fn test_config_effective_max_tokens_default() {
        let cfg = crate::config::Config::default();
        assert_eq!(
            cfg.effective_max_tokens(),
            crate::constants::DEFAULT_MAX_TOKENS
        );
    }

    #[test]
    fn test_config_effective_max_tokens_override() {
        let cfg = crate::config::Config {
            max_tokens: Some(8192),
            ..Default::default()
        };
        assert_eq!(cfg.effective_max_tokens(), 8192);
    }

    #[test]
    fn test_config_resolve_api_key_from_config() {
        // resolve_api_key priority: env > keychain > config.
        // When env and keychain are empty, config.api_key is returned.
        // Remove env vars so they don't shadow the config.
        let orig_anthropic = std::env::var("ANTHROPIC_API_KEY").ok();
        let orig_deepseek = std::env::var("DEEPSEEK_API_KEY").ok();
        std::env::remove_var("ANTHROPIC_API_KEY");
        std::env::remove_var("DEEPSEEK_API_KEY");

        let cfg = crate::config::Config {
            api_key: Some("sk-ant-config-key-long-enough".to_string()),
            ..Default::default()
        };
        let resolved = cfg.resolve_api_key();
        // The result should be SOME key — either from keychain (if available)
        // or from config.  We can't control the keychain in a unit test, so
        // just verify it's not None.
        assert!(
            resolved.is_some(),
            "resolve_api_key should find the config key"
        );

        // Restore env vars
        if let Some(k) = orig_anthropic {
            std::env::set_var("ANTHROPIC_API_KEY", k);
        }
        if let Some(k) = orig_deepseek {
            std::env::set_var("DEEPSEEK_API_KEY", k);
        }
    }

    #[test]
    fn test_config_resolve_api_key_none() {
        // Temporarily ensure no env var override
        let orig = std::env::var("ANTHROPIC_API_KEY").ok();
        std::env::remove_var("ANTHROPIC_API_KEY");

        let cfg = crate::config::Config::default();
        assert!(cfg.resolve_api_key().is_none());

        // Restore
        if let Some(k) = orig {
            std::env::set_var("ANTHROPIC_API_KEY", k);
        }
    }

    #[test]
    #[ignore = "requires keychain access, fails on headless CI"]
    fn test_config_resolve_api_key_from_env() {
        // Remove DEEPSEEK_API_KEY to ensure ANTHROPIC_API_KEY is checked.
        let orig_ds = std::env::var("DEEPSEEK_API_KEY").ok();
        let orig_ant = std::env::var("ANTHROPIC_API_KEY").ok();
        std::env::remove_var("DEEPSEEK_API_KEY");
        std::env::set_var("ANTHROPIC_API_KEY", "sk-ant-env-key-long-enough");

        let cfg = crate::config::Config::default();
        let resolved = cfg.resolve_api_key();
        // Should find a key (either from env or keychain).
        assert!(resolved.is_some(), "Expected a key from env");

        // Restore
        std::env::remove_var("ANTHROPIC_API_KEY");
        if let Some(k) = orig_ant {
            std::env::set_var("ANTHROPIC_API_KEY", k);
        }
        if let Some(k) = orig_ds {
            std::env::set_var("DEEPSEEK_API_KEY", k);
        }
    }

    // ---- OAuth token tests --------------------------------------------------

    #[test]
    fn test_oauth_tokens_not_expired_no_expiry() {
        let tokens = crate::oauth::OAuthTokens {
            access_token: "at".to_string(),
            expires_at_ms: None,
            ..Default::default()
        };
        assert!(
            !tokens.is_expired(),
            "Token with no expiry should not be considered expired"
        );
    }

    #[test]
    fn test_oauth_tokens_expired_past() {
        let tokens = crate::oauth::OAuthTokens {
            access_token: "at".to_string(),
            // Expired 1 hour ago
            expires_at_ms: Some(chrono::Utc::now().timestamp_millis() - 3_600_000),
            ..Default::default()
        };
        assert!(tokens.is_expired());
    }

    #[test]
    fn test_oauth_tokens_not_expired_future() {
        let tokens = crate::oauth::OAuthTokens {
            access_token: "at".to_string(),
            // Expires in 1 hour
            expires_at_ms: Some(chrono::Utc::now().timestamp_millis() + 3_600_000),
            ..Default::default()
        };
        assert!(!tokens.is_expired());
    }

    #[test]
    fn test_oauth_tokens_expired_within_buffer() {
        let tokens = crate::oauth::OAuthTokens {
            access_token: "at".to_string(),
            // Expires in 3 minutes — within the 5-minute buffer, so treated as expired
            expires_at_ms: Some(chrono::Utc::now().timestamp_millis() + 3 * 60 * 1000),
            ..Default::default()
        };
        assert!(
            tokens.is_expired(),
            "Token within 5-min buffer should be considered expired"
        );
    }

    #[test]
    fn test_oauth_uses_bearer_auth_with_inference_scope() {
        let tokens = crate::oauth::OAuthTokens {
            scopes: vec![crate::oauth::CLAUDE_AI_INFERENCE_SCOPE.to_string()],
            ..Default::default()
        };
        assert!(tokens.uses_bearer_auth());
    }

    #[test]
    fn test_oauth_uses_bearer_auth_without_inference_scope() {
        let tokens = crate::oauth::OAuthTokens {
            scopes: vec!["org:create_api_key".to_string()],
            ..Default::default()
        };
        assert!(!tokens.uses_bearer_auth());
    }

    #[test]
    fn test_oauth_effective_credential_bearer() {
        let tokens = crate::oauth::OAuthTokens {
            access_token: "bearer_token_xyz".to_string(),
            scopes: vec![crate::oauth::CLAUDE_AI_INFERENCE_SCOPE.to_string()],
            api_key: Some("sk-ant-ignored".to_string()),
            ..Default::default()
        };
        assert_eq!(tokens.effective_credential(), Some("bearer_token_xyz"));
    }

    #[test]
    fn test_oauth_effective_credential_api_key() {
        let tokens = crate::oauth::OAuthTokens {
            access_token: "at".to_string(),
            scopes: vec!["org:create_api_key".to_string()],
            api_key: Some("sk-ant-real-key".to_string()),
            ..Default::default()
        };
        assert_eq!(tokens.effective_credential(), Some("sk-ant-real-key"));
    }

    #[test]
    fn test_oauth_effective_credential_bearer_empty_access_token() {
        let tokens = crate::oauth::OAuthTokens {
            access_token: String::new(),
            scopes: vec![crate::oauth::CLAUDE_AI_INFERENCE_SCOPE.to_string()],
            ..Default::default()
        };
        assert_eq!(tokens.effective_credential(), None);
    }

    #[test]
    fn test_oauth_effective_credential_no_api_key() {
        let tokens = crate::oauth::OAuthTokens {
            access_token: "at".to_string(),
            scopes: vec!["org:create_api_key".to_string()],
            api_key: None,
            ..Default::default()
        };
        assert_eq!(tokens.effective_credential(), None);
    }

    // ---- PKCE tests ---------------------------------------------------------

    #[test]
    fn test_pkce_code_verifier_length() {
        let verifier = crate::oauth::generate_code_verifier();
        // 32 bytes base64url-encoded (no padding) = ceil(32 * 4/3) = 43 chars
        assert_eq!(
            verifier.len(),
            43,
            "Code verifier should be 43 base64url chars (32 bytes)"
        );
        // Must only contain URL-safe base64 chars
        assert!(verifier
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
    }

    #[test]
    fn test_pkce_code_challenge_format() {
        let verifier = crate::oauth::generate_code_verifier();
        let challenge = crate::oauth::generate_code_challenge(&verifier);
        // SHA256 = 32 bytes → 43 base64url chars
        assert_eq!(
            challenge.len(),
            43,
            "Code challenge should be 43 base64url chars (SHA256 = 32 bytes)"
        );
        assert!(challenge
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
    }

    #[test]
    fn test_pkce_challenge_deterministic() {
        // Same verifier must produce same challenge
        let verifier = "test_verifier_fixed_input";
        let c1 = crate::oauth::generate_code_challenge(verifier);
        let c2 = crate::oauth::generate_code_challenge(verifier);
        assert_eq!(c1, c2);
    }

    #[test]
    fn test_pkce_verifier_unique() {
        let v1 = crate::oauth::generate_code_verifier();
        let v2 = crate::oauth::generate_code_verifier();
        assert_ne!(v1, v2, "Code verifiers should be unique");
    }

    #[test]
    fn test_pkce_state_length_and_format() {
        let state = crate::oauth::generate_state();
        assert_eq!(state.len(), 43);
        assert!(state
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
    }

    // ---- Auth URL building tests --------------------------------------------

    #[test]
    fn test_oauth_urls_are_disabled() {
        // Uppli Code uses API keys — OAuth URLs must be empty.
        assert!(crate::oauth::CONSOLE_AUTHORIZE_URL.is_empty());
        assert!(crate::oauth::CLAUDE_AI_AUTHORIZE_URL.is_empty());
        assert!(crate::oauth::TOKEN_URL.is_empty());
        assert!(crate::oauth::CLIENT_ID.is_empty());
    }

    // ---- Permission handler tests -------------------------------------------

    fn make_req(tool_name: &str, is_read_only: bool) -> crate::permissions::PermissionRequest {
        crate::permissions::PermissionRequest {
            tool_name: tool_name.to_string(),
            description: format!("{} operation", tool_name),
            details: None,
            is_read_only,
        }
    }

    #[test]
    fn test_auto_handler_bypass_allows_all() {
        let handler = crate::permissions::AutoPermissionHandler {
            mode: crate::config::PermissionMode::BypassPermissions,
        };
        assert_eq!(
            handler.check_permission(&make_req("FileWrite", false)),
            crate::permissions::PermissionDecision::Allow
        );
    }

    #[test]
    fn test_auto_handler_default_allows_reads() {
        let handler = crate::permissions::AutoPermissionHandler {
            mode: crate::config::PermissionMode::Default,
        };
        assert_eq!(
            handler.check_permission(&make_req("FileRead", true)),
            crate::permissions::PermissionDecision::Allow
        );
    }

    #[test]
    fn test_auto_handler_default_denies_writes() {
        let handler = crate::permissions::AutoPermissionHandler {
            mode: crate::config::PermissionMode::Default,
        };
        assert_eq!(
            handler.check_permission(&make_req("FileWrite", false)),
            crate::permissions::PermissionDecision::Deny
        );
    }

    #[test]
    fn test_auto_handler_accept_edits_allows_writes() {
        let handler = crate::permissions::AutoPermissionHandler {
            mode: crate::config::PermissionMode::AcceptEdits,
        };
        assert_eq!(
            handler.check_permission(&make_req("FileWrite", false)),
            crate::permissions::PermissionDecision::Allow
        );
    }

    #[test]
    fn test_auto_handler_plan_denies_writes() {
        let handler = crate::permissions::AutoPermissionHandler {
            mode: crate::config::PermissionMode::Plan,
        };
        assert_eq!(
            handler.check_permission(&make_req("Bash", false)),
            crate::permissions::PermissionDecision::Deny
        );
        assert_eq!(
            handler.check_permission(&make_req("FileRead", true)),
            crate::permissions::PermissionDecision::Allow
        );
    }

    #[test]
    fn test_interactive_handler_default_allows_writes() {
        // InteractivePermissionHandler allows writes in Default mode
        // (user is watching the TUI)
        let handler = crate::permissions::InteractivePermissionHandler {
            mode: crate::config::PermissionMode::Default,
        };
        assert_eq!(
            handler.check_permission(&make_req("FileWrite", false)),
            crate::permissions::PermissionDecision::Allow
        );
    }

    #[test]
    fn test_interactive_handler_plan_allows_reads_denies_writes() {
        let handler = crate::permissions::InteractivePermissionHandler {
            mode: crate::config::PermissionMode::Plan,
        };
        assert_eq!(
            handler.check_permission(&make_req("FileRead", true)),
            crate::permissions::PermissionDecision::Allow
        );
        assert_eq!(
            handler.check_permission(&make_req("FileWrite", false)),
            crate::permissions::PermissionDecision::Deny
        );
    }

    // ---- Message content tests ----------------------------------------------

    #[test]
    fn test_message_get_all_text_multiple_blocks() {
        let msg = Message::assistant_blocks(vec![
            ContentBlock::Text {
                text: "First ".into(),
            },
            ContentBlock::Text {
                text: "Second".into(),
            },
        ]);
        assert_eq!(msg.get_all_text(), "First Second");
    }

    #[test]
    fn test_message_get_text_returns_first_text_block() {
        let msg = Message::assistant_blocks(vec![
            ContentBlock::Thinking {
                thinking: "reasoning".into(),
                signature: "sig".into(),
            },
            ContentBlock::Text {
                text: "answer".into(),
            },
        ]);
        assert_eq!(msg.get_text(), Some("answer"));
    }

    #[test]
    fn test_message_has_tool_use_false() {
        let msg = Message::user("just text");
        assert!(!msg.has_tool_use());
    }

    #[test]
    fn test_cost_tracker_cumulative() {
        let tracker = CostTracker::new();
        tracker.add_usage(1000, 500, 100, 50);
        tracker.add_usage(200, 100, 0, 0);
        assert_eq!(tracker.input_tokens(), 1200);
        assert_eq!(tracker.output_tokens(), 600);
    }

    #[test]
    fn test_cost_tracker_initial_zero() {
        let tracker = CostTracker::new();
        assert_eq!(tracker.input_tokens(), 0);
        assert_eq!(tracker.output_tokens(), 0);
        assert_eq!(tracker.total_tokens(), 0);
    }
}
