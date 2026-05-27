// claude-code CLI entry point
//
// This is the main binary for the Uppli Code Rust port. It:
// 1. Parses CLI arguments with clap (mirrors cli.tsx + main.tsx flags)
// 2. Loads configuration from settings.json + env vars
// 3. Builds system/user context (git status, UPPLI.md)
// 4. Runs in either:
//    - Headless (--print / -p) mode: single query, output to stdout
//    - Interactive REPL mode: full TUI with ratatui

mod binome;
mod mcp_server;
mod oauth_flow;

use async_trait::async_trait;
use cc_core::types::ToolDefinition;
use cc_core::{
    config::{Config, PermissionMode, Settings},
    constants::APP_VERSION,
    context::ContextBuilder,
    cost::CostTracker,
    permissions::{AutoPermissionHandler, InteractivePermissionHandler},
};
use cc_tools::{PermissionLevel, Tool, ToolContext, ToolResult};
use clap::{ArgAction, Parser, ValueEnum};
use parking_lot::Mutex as ParkingMutex;
use std::{path::PathBuf, sync::Arc};
use tracing::{debug, info, warn};
use tracing_subscriber::EnvFilter;

// ---------------------------------------------------------------------------
// MCP tool wrapper: makes MCP server tools look like native cc-tools.
// ---------------------------------------------------------------------------

struct McpToolWrapper {
    tool_def: ToolDefinition,
    server_name: String,
    manager: Arc<cc_mcp::McpManager>,
}

#[async_trait]
impl Tool for McpToolWrapper {
    fn name(&self) -> &str {
        &self.tool_def.name
    }

    fn description(&self) -> &str {
        &self.tool_def.description
    }

    fn permission_level(&self) -> PermissionLevel {
        // MCP tools run external processes – treat as Execute.
        PermissionLevel::Execute
    }

    fn input_schema(&self) -> serde_json::Value {
        self.tool_def.input_schema.clone()
    }

    async fn execute(&self, input: serde_json::Value, _ctx: &ToolContext) -> ToolResult {
        // Strip the server-name prefix to get the bare tool name.
        let prefix = format!("{}_", self.server_name);
        let bare_name = self
            .tool_def
            .name
            .strip_prefix(&prefix)
            .unwrap_or(&self.tool_def.name);

        let args = if input.is_null() { None } else { Some(input) };

        match self.manager.call_tool(&self.tool_def.name, args).await {
            Ok(result) => {
                let text = cc_mcp::mcp_result_to_string(&result);
                if result.is_error {
                    ToolResult::error(text)
                } else {
                    ToolResult::success(text)
                }
            }
            Err(e) => ToolResult::error(format!("MCP tool '{}' failed: {}", bare_name, e)),
        }
    }
}

// ---------------------------------------------------------------------------
// CLI argument definition (matches TypeScript main.tsx flags)
// ---------------------------------------------------------------------------

#[derive(Parser, Debug)]
#[command(
    name = "uppli-code",
    version = APP_VERSION,
    about = "Uppli Code - AI-powered multi-provider coding agent",
    long_about = None,
)]
struct Cli {
    /// Initial prompt to send (enables headless/print mode)
    prompt: Option<String>,

    /// Print mode: send prompt and exit (non-interactive)
    #[arg(short = 'p', long = "print", action = ArgAction::SetTrue)]
    print: bool,

    /// MCP server mode: expose uppli-code as an MCP tool on stdio.
    /// Enables orchestration by Claude Code, another uppli-code, or any MCP client.
    #[arg(long = "mcp-server", action = ArgAction::SetTrue)]
    mcp_server: bool,

    /// Groom (binome) mode: spawn a read-only peer in the same working
    /// directory and expose it over MCP as `uppli_query`. The peer is
    /// available for brainstorming, review, and sanity-checking decisions
    /// before edits.
    #[arg(long = "groom", action = ArgAction::SetTrue)]
    groom: bool,

    /// Peer mode (internal — set by `--groom`): run as the read-only half
    /// of a binome, serving MCP on the given Unix socket. Forces
    /// `--permission-mode plan`. Not intended for direct invocation by the
    /// user; we document it for transparency.
    #[arg(long = "peer", value_name = "SOCKET")]
    peer_socket: Option<PathBuf>,

    /// Model to use (overrides provider default)
    #[arg(short = 'm', long = "model")]
    model: Option<String>,

    /// Permission mode
    #[arg(long = "permission-mode", value_enum, default_value_t = CliPermissionMode::Default)]
    permission_mode: CliPermissionMode,

    /// Resume a previous session by ID
    #[arg(long = "resume")]
    resume: Option<String>,

    /// Maximum number of agentic turns
    #[arg(long = "max-turns", default_value_t = 100)]
    max_turns: u32,

    /// Custom system prompt
    #[arg(long = "system-prompt", short = 's')]
    system_prompt: Option<String>,

    /// Append to system prompt
    #[arg(long = "append-system-prompt")]
    append_system_prompt: Option<String>,

    /// Disable UPPLI.md memory files
    #[arg(long = "no-claude-md", action = ArgAction::SetTrue)]
    no_claude_md: bool,

    /// Output format
    #[arg(long = "output-format", value_enum, default_value_t = CliOutputFormat::Text)]
    output_format: CliOutputFormat,

    /// Enable verbose logging
    #[arg(long = "verbose", short = 'v', action = ArgAction::SetTrue)]
    verbose: bool,

    /// API key (overrides ANTHROPIC_API_KEY env var)
    #[arg(long = "api-key")]
    api_key: Option<String>,

    /// API base URL (overrides provider default, e.g. https://openrouter.ai/api)
    #[arg(long = "api-base")]
    api_base: Option<String>,

    /// Maximum tokens per response
    #[arg(long = "max-tokens")]
    max_tokens: Option<u32>,

    /// Working directory
    #[arg(long = "cwd")]
    cwd: Option<PathBuf>,

    /// Bypass all permission checks (danger!)
    #[arg(long = "dangerously-skip-permissions", action = ArgAction::SetTrue)]
    dangerously_skip_permissions: bool,

    /// Dump the system prompt to stdout and exit
    #[arg(long = "dump-system-prompt", action = ArgAction::SetTrue, hide = true)]
    dump_system_prompt: bool,

    /// MCP config JSON string (inline server definitions)
    #[arg(long = "mcp-config")]
    mcp_config: Option<String>,

    /// Disable auto-compaction
    #[arg(long = "no-auto-compact", action = ArgAction::SetTrue)]
    no_auto_compact: bool,

    /// Grant Claude access to an additional directory (can be repeated)
    #[arg(long = "add-dir", value_name = "DIR", action = ArgAction::Append)]
    add_dir: Vec<PathBuf>,

    /// Input format for --print mode (text or stream-json)
    #[arg(long = "input-format", value_enum, default_value_t = CliInputFormat::Text)]
    input_format: CliInputFormat,

    /// Session ID to tag this headless run (for tracking in logs/hooks)
    #[arg(long = "session-id")]
    session_id_flag: Option<String>,

    /// Prefill the first assistant turn with this text
    #[arg(long = "prefill")]
    prefill: Option<String>,

    /// Effort level for extended thinking (low, medium, high, max)
    #[arg(long = "effort", value_name = "LEVEL")]
    effort: Option<String>,

    /// Extended thinking budget in tokens (enables extended thinking)
    #[arg(long = "thinking", value_name = "TOKENS_OR_MODE")]
    thinking: Option<String>,

    /// Max thinking tokens (SDK alias for --thinking)
    #[arg(long = "max-thinking-tokens", value_name = "TOKENS")]
    max_thinking_tokens: Option<u32>,

    /// Include partial streaming messages (SDK compatibility — always active)
    #[arg(long = "include-partial-messages", action = ArgAction::SetTrue)]
    include_partial_messages: bool,

    /// Permission prompt tool (SDK compatibility — "stdio" for interactive permissions)
    #[arg(long = "permission-prompt-tool", value_name = "MODE")]
    permission_prompt_tool: Option<String>,

    /// Continue the most recent conversation
    #[arg(short = 'c', long = "continue", action = ArgAction::SetTrue)]
    continue_session: bool,

    /// Override system prompt from a file
    #[arg(long = "system-prompt-file")]
    system_prompt_file: Option<PathBuf>,

    /// Tools to allow (comma-separated, default: all)
    #[arg(long = "allowed-tools", value_name = "TOOLS")]
    allowed_tools: Option<String>,

    /// Tools to disallow (comma-separated)
    #[arg(long = "disallowed-tools", value_name = "TOOLS")]
    disallowed_tools: Option<String>,

    /// Extra beta feature headers to send (comma-separated)
    #[arg(long = "betas", value_name = "HEADERS")]
    betas: Option<String>,

    /// Disable all slash commands
    #[arg(long = "disable-slash-commands", action = ArgAction::SetTrue)]
    disable_slash_commands: bool,

    /// Run in bare mode (no hooks, no plugins, no UPPLI.md)
    #[arg(long = "bare", action = ArgAction::SetTrue)]
    bare: bool,

    /// Billing workload tag
    #[arg(long = "workload", value_name = "TAG")]
    workload: Option<String>,

    /// Maximum spend in USD before aborting the query loop
    #[arg(long = "max-budget-usd", value_name = "USD")]
    max_budget_usd: Option<f64>,

    /// Fallback model to use if the primary model is overloaded or unavailable
    #[arg(long = "fallback-model")]
    fallback_model: Option<String>,

    /// LLM provider to use (deepseek, ollama, alibaba, openrouter, openai)
    #[arg(long = "provider")]
    provider: Option<String>,

    // --- SDK compatibility flags (accepted but ignored) ---
    /// Setting sources to load (SDK compat)
    #[arg(long = "setting-sources", value_name = "SOURCES")]
    setting_sources: Option<String>,

    /// Allow bypassing permissions (SDK compat)
    #[arg(long = "allow-dangerously-skip-permissions", action = ArgAction::SetTrue)]
    allow_dangerously_skip_permissions: bool,

    /// Debug output to stderr (SDK compat)
    #[arg(long = "debug-to-stderr", action = ArgAction::SetTrue)]
    debug_to_stderr: bool,

    /// Debug mode (SDK compat)
    #[arg(long = "debug", action = ArgAction::SetTrue)]
    debug_mode: bool,

    /// Debug to file (SDK compat)
    #[arg(long = "debug-file", value_name = "PATH")]
    debug_file: Option<String>,

    /// Agent name (SDK compat)
    #[arg(long = "agent", value_name = "NAME")]
    agent: Option<String>,

    /// Assistant mode (SDK compat)
    #[arg(long = "assistant", action = ArgAction::SetTrue)]
    assistant: bool,

    /// Channels (SDK compat)
    #[arg(long = "channels", value_name = "CHANNELS", action = ArgAction::Append)]
    channels: Vec<String>,

    /// Tools configuration (SDK compat)
    #[arg(long = "tools", value_name = "TOOLS")]
    tools_config: Option<String>,

    /// Allowed tools (SDK compat)
    #[arg(long = "allowedTools", value_name = "TOOLS")]
    sdk_allowed_tools: Option<String>,

    /// Disallowed tools (SDK compat)
    #[arg(long = "disallowedTools", value_name = "TOOLS")]
    sdk_disallowed_tools: Option<String>,

    /// JSON schema output (SDK compat)
    #[arg(long = "json-schema", value_name = "SCHEMA")]
    json_schema: Option<String>,

    /// Fork session (SDK compat)
    #[arg(long = "fork-session", action = ArgAction::SetTrue)]
    fork_session: bool,

    /// Resume session at message (SDK compat)
    #[arg(long = "resume-session-at", value_name = "UUID")]
    resume_session_at: Option<String>,

    /// Disable session persistence (SDK compat)
    #[arg(long = "no-session-persistence", action = ArgAction::SetTrue)]
    no_session_persistence: bool,

    /// Task budget (SDK compat)
    #[arg(long = "task-budget", value_name = "BUDGET")]
    task_budget: Option<String>,

    /// Plugin directory (SDK compat)
    #[arg(long = "plugin-dir", value_name = "DIR", action = ArgAction::Append)]
    plugin_dir: Vec<String>,

    /// Proactive mode (SDK compat)
    #[arg(long = "proactive", action = ArgAction::SetTrue)]
    proactive: bool,

    /// Porcelain output (SDK compat)
    #[arg(long = "porcelain", action = ArgAction::SetTrue)]
    porcelain: bool,

    /// Include hook events in output (SDK compat)
    #[arg(long = "include-hook-events", action = ArgAction::SetTrue)]
    include_hook_events: bool,

    /// Strict MCP config (SDK compat)
    #[arg(long = "strict-mcp-config", action = ArgAction::SetTrue)]
    strict_mcp_config: bool,

    /// Settings file path (SDK compat, via extraArgs)
    #[arg(long = "settings", value_name = "PATH")]
    settings_path: Option<String>,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
enum CliPermissionMode {
    Default,
    #[value(alias = "acceptEdits")]
    AcceptEdits,
    #[value(alias = "bypassPermissions")]
    BypassPermissions,
    #[value(alias = "dontAsk")]
    Plan,
}

impl From<CliPermissionMode> for PermissionMode {
    fn from(m: CliPermissionMode) -> Self {
        match m {
            CliPermissionMode::Default => PermissionMode::Default,
            CliPermissionMode::AcceptEdits => PermissionMode::AcceptEdits,
            CliPermissionMode::BypassPermissions => PermissionMode::BypassPermissions,
            CliPermissionMode::Plan => PermissionMode::Plan,
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
enum CliOutputFormat {
    Text,
    Json,
    #[value(name = "stream-json")]
    StreamJson,
}

impl From<CliOutputFormat> for cc_core::config::OutputFormat {
    fn from(f: CliOutputFormat) -> Self {
        match f {
            CliOutputFormat::Text => cc_core::config::OutputFormat::Text,
            CliOutputFormat::Json => cc_core::config::OutputFormat::Json,
            CliOutputFormat::StreamJson => cc_core::config::OutputFormat::StreamJson,
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
enum CliInputFormat {
    /// Plain text prompt (default)
    Text,
    /// Newline-delimited JSON messages — each line is {"role":"user"|"assistant","content":"..."}
    #[value(name = "stream-json")]
    StreamJson,
}

fn resolve_bridge_config(
    settings: &Settings,
    auth_credential: &str,
    use_bearer_auth: bool,
    is_headless: bool,
) -> Option<cc_bridge::BridgeConfig> {
    if is_headless {
        return None;
    }

    let mut bridge_config = cc_bridge::BridgeConfig::from_env();

    if settings.remote_control_at_startup {
        bridge_config.enabled = true;
    }

    if bridge_config.session_token.is_none() && use_bearer_auth && !auth_credential.is_empty() {
        bridge_config.session_token = Some(auth_credential.to_string());
    }

    bridge_config.is_active().then_some(bridge_config)
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Fast-path: handle --version before parsing everything
    let raw_args: Vec<String> = std::env::args().collect();
    if raw_args.iter().any(|a| a == "--version" || a == "-V") {
        println!("{} {}", cc_core::constants::APP_NAME, APP_VERSION);
        return Ok(());
    }

    // Fast-path: `claude auth <login|logout|status>` — mirrors TypeScript cli.tsx pattern
    if raw_args.get(1).map(|s| s.as_str()) == Some("auth") {
        return handle_auth_command(&raw_args[2..]).await;
    }

    // Fast-path: named commands (`claude agents`, `claude ide`, `claude branch`, …)
    // Check before Cli::parse() so these names don't conflict with positional prompt arg.
    if let Some(cmd_name) = raw_args.get(1).map(|s| s.as_str()) {
        // Only intercept if it looks like a subcommand (no leading `-` or `/`)
        if !cmd_name.starts_with('-') && !cmd_name.starts_with('/') {
            if let Some(named_cmd) = cc_commands::named_commands::find_named_command(cmd_name) {
                // Build a minimal CommandContext (named commands are pre-session)
                let settings = Settings::load().await.unwrap_or_default();
                let config = settings.config.clone();
                let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
                let cmd_ctx = cc_commands::CommandContext {
                    config,
                    cost_tracker: CostTracker::new(),
                    messages: vec![],
                    working_dir: cwd,
                    session_id: "pre-session".to_string(),
                    session_title: None,
                    remote_session_url: None,
                    mcp_manager: None,
                };
                // Collect remaining args after the command name
                let rest: Vec<&str> = raw_args[2..].iter().map(|s| s.as_str()).collect();
                let result = named_cmd.execute_named(&rest, &cmd_ctx);
                match result {
                    cc_commands::CommandResult::Message(msg)
                    | cc_commands::CommandResult::UserMessage(msg) => {
                        println!("{}", msg);
                        std::process::exit(0);
                    }
                    cc_commands::CommandResult::Error(e) => {
                        eprintln!("Error: {}", e);
                        eprintln!("Usage: {}", named_cmd.usage());
                        std::process::exit(1);
                    }
                    _ => {
                        // For any other result variant, fall through to normal startup
                    }
                }
                return Ok(());
            }
        }
    }

    let cli = Cli::parse();

    // Setup logging
    let log_level = if cli.verbose { "debug" } else { "warn" };
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(log_level)),
        )
        .with_target(false)
        .without_time()
        .with_writer(std::io::stderr)
        .init();

    // Determine working directory
    let cwd = cli
        .cwd
        .clone()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

    debug!(cwd = %cwd.display(), "Starting Uppli Code");

    // Load settings from disk
    let settings = Settings::load().await.unwrap_or_default();

    // Build effective config (CLI args override settings)
    let mut config = settings.config.clone();
    if let Some(ref key) = cli.api_key {
        config.api_key = Some(key.clone());
    }
    if let Some(ref base) = cli.api_base {
        std::env::set_var("UPPLI_API_BASE", base);
    }
    if cli.model.is_some() {
        config.model = cli.model.clone();
    }
    if let Some(mt) = cli.max_tokens {
        config.max_tokens = Some(mt);
    }
    config.verbose = cli.verbose;
    config.output_format = cli.output_format.into();
    config.disable_claude_mds = cli.no_claude_md;
    if let Some(sp) = cli.system_prompt.clone() {
        config.custom_system_prompt = Some(sp);
    }
    if let Some(asp) = cli.append_system_prompt.clone() {
        config.append_system_prompt = Some(asp);
    }
    if cli.dangerously_skip_permissions || cli.allow_dangerously_skip_permissions {
        // Mirror TS setup.ts: block bypass mode when running as root/sudo.
        #[cfg(unix)]
        if nix::unistd::Uid::effective().is_root() {
            anyhow::bail!(
                "--dangerously-skip-permissions cannot be used with root/sudo privileges for security reasons"
            );
        }
        config.permission_mode = PermissionMode::BypassPermissions;
    } else {
        config.permission_mode = cli.permission_mode.into();
    }
    // --peer forces Plan (read-only) regardless of what the user passed —
    // the peer half of the binome must not be able to edit, ever. Override
    // comes last so no prior flag (including --dangerously-skip-permissions)
    // can accidentally unlock writes.
    if cli.peer_socket.is_some() {
        config.permission_mode = PermissionMode::Plan;
        // Append the peer role addendum to whatever system prompt override
        // the user may already have set, so custom prompts still compose.
        let addendum = cc_core::system_prompt::groom_peer_addendum();
        config.append_system_prompt = Some(match config.append_system_prompt.take() {
            Some(prev) => format!("{prev}\n{addendum}"),
            None => addendum,
        });
    }
    // --groom appends the worker addendum and injects a synthetic "peer"
    // MCP server pointing at the socket we will spawn the child on. The
    // child itself is spawned just below, before we connect the manager.
    if cli.groom {
        let addendum = cc_core::system_prompt::groom_worker_addendum();
        config.append_system_prompt = Some(match config.append_system_prompt.take() {
            Some(prev) => format!("{prev}\n{addendum}"),
            None => addendum,
        });
    }
    config.additional_dirs = cli.add_dir.clone();
    if cli.no_auto_compact {
        config.auto_compact = false;
    }
    config.project_dir = Some(cwd.clone());

    // --mcp-config: merge MCP servers from a JSON file or inline JSON string.
    // Format matches .mcp.json: {"mcpServers": {"name": {"command": "...", ...}}}
    if let Some(ref mcp_config_value) = cli.mcp_config {
        let json_str = if std::path::Path::new(mcp_config_value).is_file() {
            std::fs::read_to_string(mcp_config_value).unwrap_or_else(|e| {
                warn!(path = %mcp_config_value, error = %e, "Failed to read --mcp-config file");
                String::new()
            })
        } else {
            mcp_config_value.clone()
        };

        if !json_str.is_empty() {
            match serde_json::from_str::<serde_json::Value>(&json_str) {
                Ok(data) => {
                    if let Some(servers_obj) = data
                        .get("mcpServers")
                        .and_then(|s| s.as_object())
                    {
                        let existing_names: std::collections::HashSet<String> =
                            config.mcp_servers.iter().map(|s| s.name.clone()).collect();
                        let mut added = 0usize;
                        let mut overridden = 0usize;
                        for (name, server_val) in servers_obj {
                            let command = server_val
                                .get("command")
                                .and_then(|v| v.as_str())
                                .map(|s| s.to_string());
                            let args: Vec<String> = server_val
                                .get("args")
                                .and_then(|v| v.as_array())
                                .map(|arr| {
                                    arr.iter()
                                        .filter_map(|v| v.as_str().map(String::from))
                                        .collect()
                                })
                                .unwrap_or_default();
                            let env: std::collections::HashMap<String, String> = server_val
                                .get("env")
                                .and_then(|v| v.as_object())
                                .map(|obj| {
                                    obj.iter()
                                        .filter_map(|(k, v)| {
                                            v.as_str().map(|s| (k.clone(), s.to_string()))
                                        })
                                        .collect()
                                })
                                .unwrap_or_default();
                            let url = server_val
                                .get("url")
                                .and_then(|v| v.as_str())
                                .map(|s| s.to_string());
                            let server_type = server_val
                                .get("type")
                                .and_then(|v| v.as_str())
                                .unwrap_or("stdio")
                                .to_string();

                            let mcp_cfg = cc_core::McpServerConfig {
                                name: name.clone(),
                                command,
                                args,
                                env,
                                url,
                                server_type,
                            };

                            if existing_names.contains(name) {
                                // Override: remove existing entry, then push new one.
                                config.mcp_servers.retain(|s| s.name != *name);
                                overridden += 1;
                            } else {
                                added += 1;
                            }
                            config.mcp_servers.push(mcp_cfg);
                        }
                        info!(
                            added,
                            overridden,
                            total = config.mcp_servers.len(),
                            "MCP servers loaded from --mcp-config"
                        );
                    } else {
                        warn!("--mcp-config JSON does not contain a \"mcpServers\" key");
                    }
                }
                Err(e) => {
                    warn!(error = %e, "Failed to parse --mcp-config as JSON");
                }
            }
        }
    }

    // --dump-system-prompt fast path
    if cli.dump_system_prompt {
        let ctx = ContextBuilder::new(cwd.clone()).disable_claude_mds(config.disable_claude_mds);
        let sys = ctx.build_system_context().await;
        let user = ctx.build_user_context().await;
        println!("{}\n\n{}", sys, user);
        return Ok(());
    }

    // Build context
    let ctx_builder =
        ContextBuilder::new(cwd.clone()).disable_claude_mds(config.disable_claude_mds);
    let system_ctx = ctx_builder.build_system_context().await;
    let user_ctx = ctx_builder.build_user_context().await;

    // System prompt is built after the provider is created (needs attribution).
    // Context parts are gathered here, prompt assembled later.

    // Determine mode early (needed for auth error handling and permission handler selection).
    // Headless mode: explicit -p flag, prompt arg, OR stdout is piped (SDK spawns us without -p)
    let stdout_is_piped = !std::io::IsTerminal::is_terminal(&std::io::stdout());
    let is_headless = cli.print || cli.prompt.is_some() || stdout_is_piped;
    let is_sdk_piped = stdout_is_piped
        && cli.input_format == CliInputFormat::StreamJson
        && !cli.print
        && cli.prompt.is_none();

    // Onboarding is now handled in the TUI (onboarding_dialog.rs).
    // In headless mode, if no key is found, we bail with a clear error.
    let provider_name = cli.provider.as_deref();

    // ---- Resolve API key ─────────────────────────────────────────────────
    // The provider factory handles key resolution per-provider (env var →
    // keychain → config fallback).  Here we only need to handle two cases:
    //   1. DeepSeek: use resolve_auth_async (backwards compat with OAuth)
    //   2. Others: let the factory handle it (it knows which keychain/env var)
    // If the factory fails because of a missing key, we prompt interactively.
    // Determine effective provider: --provider flag overrides config.provider.
    let effective_provider = provider_name
        .map(|s| s.to_string())
        .unwrap_or_else(|| config.provider.to_string());
    let preset = cc_api::find_preset(&effective_provider);
    // DeepSeek uses a special auth flow (backwards compat with OAuth).
    // Detect via ProviderType enum rather than hardcoded string comparison.
    let is_deepseek = preset
        .map(|p| p.provider_type == cc_core::config::ProviderType::Deepseek)
        .unwrap_or(false);
    let (api_key, use_bearer_auth) = if is_deepseek {
        match config.resolve_auth_async().await {
            Some(auth) => auth,
            None if is_headless => {
                let env_hint = preset
                    .and_then(|p| p.auth.env_vars.first())
                    .copied()
                    .unwrap_or("API_KEY");
                anyhow::bail!("No API key found. Set {} or use --api-key.", env_hint);
            }
            None => {
                // No key found — in TUI mode the onboarding dialog handles it.
                // Pass empty key; the provider factory will be called after
                // onboarding completes with the key from the keychain.
                (String::new(), false)
            }
        }
    } else {
        // Non-DeepSeek: the factory resolves the key itself.
        // Pass empty here — create_provider() will find it via keychain/env.
        (String::new(), false)
    };

    // Store resolved API key in config for the factory to pick up.
    if !api_key.is_empty() {
        config.api_key = Some(api_key.clone());
    }

    let client: Arc<dyn cc_api::LlmProvider> = match cc_api::create_provider(&config, provider_name)
    {
        Ok(c) => Arc::from(c),
        Err(e) => {
            if is_headless {
                return Err(e.context("Failed to create LLM provider"));
            }
            // In TUI mode, no provider yet is OK — the onboarding dialog will set it up.
            // Create a placeholder Ollama provider (needs no key) that will be
            // replaced after onboarding completes.
            info!("No provider configured — TUI onboarding will handle setup");
            Arc::from(
                cc_api::create_provider(
                    &config,
                    Some("ollama"), // Ollama needs no key, safest placeholder
                )
                .unwrap_or_else(|_| {
                    // Last resort: create Ollama provider with defaults.
                    // Use the Ollama preset's default model, not a hardcoded name.
                    let default_model = cc_api::find_preset("ollama")
                        .map(|p| p.default_model.to_string())
                        .unwrap_or_else(|| "llama3".to_string());
                    match cc_api::OpenAiProvider::new(cc_api::OpenAiProviderConfig::ollama(
                        &default_model,
                    )) {
                        Ok(p) => Box::new(p),
                        Err(e) => {
                            warn!(error = %e, "Failed to create fallback Ollama provider");
                            // Absolute last resort — will fail on first API call
                            // but at least won't panic at startup.
                            Box::new(
                                cc_api::OpenAiProvider::new(cc_api::OpenAiProviderConfig::ollama(
                                    "llama3",
                                ))
                                .unwrap_or_else(|_| unreachable!("static config")),
                            )
                        }
                    }
                }),
            )
        }
    };

    // Store provider globally so sub-agents (AgentTool) can reuse it.
    cc_query::set_global_provider(Arc::clone(&client));

    // Initialize the RAG embedding model (22MB, ~1s load).
    cc_rag::Embedder::init();

    // Log active provider.
    info!(
        provider = client.capabilities().name.as_str(),
        model = client.capabilities().default_model.as_str(),
        format = %client.capabilities().api_format,
        "LLM provider initialized"
    );

    // Build system prompt now that provider attribution is available.
    let base_prompt = if let Some(ref custom) = config.custom_system_prompt {
        custom.clone()
    } else {
        format!(
            "You are Uppli Code, an AI coding assistant {}. \
             When asked what model you use, say {} (not Claude, not GPT, not any other model).\n\n{}",
            client.capabilities().attribution,
            client.capabilities().display_name,
            include_str!("system_prompt.txt"),
        )
    };
    let mut system_parts = vec![base_prompt, system_ctx, user_ctx];
    if let Some(ref append) = config.append_system_prompt {
        system_parts.push(append.clone());
    }
    let system_prompt = system_parts.join("\n\n");

    let bridge_config = resolve_bridge_config(&settings, &api_key, use_bearer_auth, is_headless);
    if let Some(cfg) = bridge_config.as_ref() {
        info!(
            server_url = %cfg.server_url,
            startup_enabled = settings.remote_control_at_startup,
            "Remote control bridge configured for interactive startup"
        );
    }

    // Build tools
    // Interactive mode uses InteractivePermissionHandler which allows writes in Default mode
    // (the user is watching the TUI so they can intervene). Headless/print mode uses
    // AutoPermissionHandler which denies writes in Default mode for safety.
    // In SDK-piped mode, permissions are managed by the SDK/extension (not the CLI),
    // so we bypass all permission checks. In one-shot headless (-p), we keep the
    // AutoPermissionHandler for safety. In interactive TUI, the user watches.
    let permission_handler: Arc<dyn cc_core::PermissionHandler> = if is_sdk_piped {
        Arc::new(AutoPermissionHandler {
            mode: PermissionMode::BypassPermissions,
        })
    } else if is_headless {
        Arc::new(AutoPermissionHandler {
            mode: config.permission_mode.clone(),
        })
    } else {
        Arc::new(InteractivePermissionHandler {
            mode: config.permission_mode.clone(),
        })
    };
    let cost_tracker = CostTracker::new();
    // Use --session-id if provided, otherwise generate a fresh UUID.
    let session_id = cli
        .session_id_flag
        .clone()
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let file_history = Arc::new(ParkingMutex::new(cc_core::file_history::FileHistory::new()));
    let current_turn = Arc::new(std::sync::atomic::AtomicUsize::new(0));

    // Initialize MCP servers first (needed for ToolContext.mcp_manager).
    let mcp_manager_arc = connect_mcp_manager_arc(&config).await;

    let tool_ctx = ToolContext {
        working_dir: cwd.clone(),
        permission_mode: config.permission_mode.clone(),
        permission_handler: permission_handler.clone(),
        cost_tracker: cost_tracker.clone(),
        session_id: session_id.clone(),
        file_history: file_history.clone(),
        current_turn: current_turn.clone(),
        non_interactive: cli.print || cli.prompt.is_some(),
        mcp_manager: mcp_manager_arc.clone(),
        config: config.clone(),
    };

    // Build the full tool list: built-ins from cc-tools plus AgentTool from cc-query
    // (AgentTool lives in cc-query to avoid a circular cc-tools ↔ cc-query dependency).
    // Wrap in Arc so the list can be shared by the main loop AND the cron scheduler.
    let tools = build_tools_with_mcp(mcp_manager_arc.clone());

    // Load plugins and register any plugin-provided MCP servers into the
    // in-memory config (does not modify the settings file on disk).
    let plugin_registry = cc_plugins::load_plugins(&cwd, &[]).await;
    {
        let plugin_cmd_count = plugin_registry.all_command_defs().len();
        let plugin_hook_count = plugin_registry
            .build_hook_registry()
            .values()
            .map(|v| v.len())
            .sum::<usize>();
        info!(
            plugins = plugin_registry.enabled_count(),
            commands = plugin_cmd_count,
            hooks = plugin_hook_count,
            "Plugins loaded"
        );

        // Register plugin MCP servers into the in-memory config so they are
        // picked up by any subsequent MCP manager construction.
        let existing_names: std::collections::HashSet<String> =
            config.mcp_servers.iter().map(|s| s.name.clone()).collect();
        for mcp_server in plugin_registry.all_mcp_servers() {
            if !existing_names.contains(&mcp_server.name) {
                config.mcp_servers.push(mcp_server);
            }
        }
    }

    // --groom is now handled in-process by `binome::run_binome`. The
    // cross-process peer (spawn_peer_child + Unix socket MCP) remains in
    // the codebase as dormant infrastructure (UnixSocketTransport,
    // run_mcp_server_unix, --peer flag) but is no longer wired into
    // --groom — the serial orchestrator avoids the IPC complexity and
    // gives deterministic routing between worker and peer.

    // Build query config from the provider's metadata (model, max_tokens, thinking, etc.).
    let mut query_config = cc_query::QueryConfig::from_provider(client.as_ref(), &config);
    // If the user explicitly set --model, override the provider default.
    if let Some(ref explicit_model) = cli.model {
        query_config.model = explicit_model.clone();
    }
    // Set cost tracker pricing from provider metadata (same type, no conversion).
    if let Some(pricing) = client.model_pricing(&query_config.model) {
        cost_tracker.set_pricing(pricing);
    }

    // Start LSP servers in background (non-blocking).
    // If lsp_servers is empty, try auto-detecting installed language servers.
    {
        let mut lsp_configs = config.lsp_servers.clone();
        if lsp_configs.is_empty() {
            lsp_configs = cc_core::lsp::detect_installed_servers();
        }
        if !lsp_configs.is_empty() {
            let lsp_mgr = cc_core::lsp::global_lsp_manager();
            let cwd_clone = cwd.clone();
            let n_servers = lsp_configs.len();
            info!(count = n_servers, "Starting LSP servers in background");
            tokio::spawn(async move {
                let mut mgr = lsp_mgr.lock().await;
                for cfg in lsp_configs {
                    mgr.register_server(cfg);
                }
                mgr.start_servers(&cwd_clone).await;
                debug!("LSP background startup finished");
            });
        }
    }
    query_config.max_turns = cli.max_turns;
    query_config.system_prompt = Some(system_prompt);
    query_config.append_system_prompt = None;
    query_config.working_directory = Some(tool_ctx.working_dir.display().to_string());
    // --thinking accepts: a number (tokens), "adaptive", or "disabled"
    // --max-thinking-tokens is an SDK alias that always provides a number
    if let Some(ref val) = cli.thinking {
        match val.as_str() {
            "disabled" => {
                query_config.thinking_budget = None;
            }
            "adaptive" => { /* use default from effort level */ }
            other => {
                if let Ok(tokens) = other.parse::<u32>() {
                    query_config.thinking_budget = Some(tokens);
                }
            }
        }
    }
    if let Some(tokens) = cli.max_thinking_tokens {
        query_config.thinking_budget = Some(tokens);
    }
    if let Some(ref level_str) = cli.effort {
        if let Some(level) = cc_core::effort::EffortLevel::from_str(level_str) {
            query_config.effort_level = Some(level);
        } else {
            eprintln!(
                "Warning: unknown effort level '{}' — expected low/medium/high/max",
                level_str
            );
        }
    }
    if let Some(usd) = cli.max_budget_usd {
        query_config.max_budget_usd = Some(usd);
    }
    if let Some(ref fb) = cli.fallback_model {
        query_config.fallback_model = Some(fb.clone());
    }

    // Spawn the background cron scheduler (fires cron tasks at scheduled times).
    // Cancelled automatically when the process exits since we use a shared token.
    let cron_cancel = tokio_util::sync::CancellationToken::new();
    cc_query::start_cron_scheduler(
        client.clone(),
        tools.clone(),
        tool_ctx.clone(),
        query_config.clone(),
        cron_cancel.clone(),
    );

    // MCP server mode: expose uppli-code as an MCP tool on stdio.
    // Launched with `uppli-code --mcp-server`, used by Claude Code, another
    // uppli-code master, CI pipelines, or any MCP client.
    let result = if cli.mcp_server {
        let mcp_state = mcp_server::McpServerState {
            provider: client,
            working_dir: cwd.clone(),
            config: config.clone(),
            cost_tracker: cost_tracker.clone(),
            permission_mode: config.permission_mode.clone(),
            max_turns: cli.max_turns,
            model_override: cli.model.clone(),
            busy: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        };
        mcp_server::run_mcp_server(mcp_state).await
    } else if let Some(sock) = cli.peer_socket.clone() {
        // --peer: MCP server on a Unix socket. The parent --groom process
        // is our sole client. `config.permission_mode` has already been
        // forced to Plan above.
        #[cfg(unix)]
        {
            let mcp_state = mcp_server::McpServerState {
                provider: client,
                working_dir: cwd.clone(),
                config: config.clone(),
                cost_tracker: cost_tracker.clone(),
                permission_mode: config.permission_mode.clone(),
                max_turns: cli.max_turns,
                model_override: cli.model.clone(),
                busy: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            };
            mcp_server::run_mcp_server_unix(mcp_state, sock).await
        }
        #[cfg(not(unix))]
        {
            let _ = sock;
            anyhow::bail!("--peer is only supported on Unix platforms");
        }
    } else if cli.groom {
        // In-process binome: one worker (Edit perms), one peer (Plan).
        // We already baked the worker addendum into `system_prompt` above
        // (see the `if cli.groom` block ~line 569). For the peer we build
        // a second system prompt with the peer addendum and a Plan-mode
        // ToolContext so its tool calls can't mutate state.
        let user_prompt = cli
            .prompt
            .clone()
            .ok_or_else(|| anyhow::anyhow!("--groom requires a prompt (-p \"...\")"))?;

        // Peer system prompt: swap the worker addendum for the peer
        // addendum in the already-assembled system prompt.
        let worker_addendum = cc_core::system_prompt::groom_worker_addendum();
        let peer_addendum = cc_core::system_prompt::groom_peer_addendum();
        let peer_system_prompt = query_config
            .system_prompt
            .as_ref()
            .map(|s| s.replace(&worker_addendum, &peer_addendum))
            .unwrap_or_else(|| peer_addendum.clone());

        let mut peer_query_config = query_config.clone();
        peer_query_config.system_prompt = Some(peer_system_prompt);

        let mut peer_tool_ctx = tool_ctx.clone();
        peer_tool_ctx.permission_mode = cc_core::config::PermissionMode::Plan;

        binome::run_binome(
            client,
            tools,
            tool_ctx,
            peer_tool_ctx,
            query_config,
            peer_query_config,
            cost_tracker,
            user_prompt,
        )
        .await
    } else if is_sdk_piped {
        run_sdk_headless(&cli, client, tools, tool_ctx, query_config, cost_tracker).await
    } else if is_headless {
        run_headless(&cli, client, tools, tool_ctx, query_config, cost_tracker).await
    } else {
        run_interactive(
            config,
            settings,
            client,
            tools,
            tool_ctx,
            query_config,
            cost_tracker,
            cli.resume,
            bridge_config,
        )
        .await
    };

    cron_cancel.cancel();
    result
}

/// Spawn the read-only peer child process for `--groom` mode.
///
/// Design notes:
/// - The child listens on a Unix socket at `/tmp/uppli-peer-<pid>.sock`.
///   Including the parent pid scopes the path to this invocation and
///   avoids collisions when multiple groomed sessions run concurrently.
/// - We wait for the socket file to appear (bounded poll) before
///   returning so the MCP manager's subsequent `connect_all` finds a
///   listener. Without this, connect_unix races the child's bind.
/// - `kill_on_drop(true)` ensures the child dies with the parent on
///   normal exit or unwind. On unexpected kills (SIGKILL on parent),
///   the orphan remains — acceptable for MVP, revisit with
///   `PR_SET_PDEATHSIG` on Linux.
/// - Provider credentials / api-base / model are inherited via env;
///   the child re-reads them in its own startup. We pass `--cwd`
///   explicitly so the peer sees the same working directory even if
///   env-driven cwd differs.
#[cfg(unix)]
async fn spawn_peer_child(
    cli: &Cli,
    cwd: &std::path::Path,
) -> anyhow::Result<(tokio::process::Child, cc_core::config::McpServerConfig)> {
    let pid = std::process::id();
    let socket = std::env::temp_dir().join(format!("uppli-peer-{pid}.sock"));
    // Clean stale socket if one lingered from a prior crash with the same pid.
    let _ = std::fs::remove_file(&socket);

    let exe = std::env::current_exe().map_err(|e| {
        anyhow::anyhow!("failed to locate current uppli-code executable to spawn peer: {e}")
    })?;

    let mut cmd = tokio::process::Command::new(&exe);
    cmd.arg("--peer")
        .arg(&socket)
        .arg("--cwd")
        .arg(cwd)
        .kill_on_drop(true);
    if let Some(m) = cli.model.as_deref() {
        cmd.arg("--model").arg(m);
    }
    // Inherit stdio — the peer's logs join the parent terminal, which is
    // exactly what a user wants for a "binome at my side" experience.

    info!(socket = %socket.display(), "Spawning groom peer child");
    let child = cmd.spawn().map_err(|e| {
        anyhow::anyhow!("failed to spawn peer child ({}): {e}", exe.display())
    })?;

    // Wait for the listener to be ready. The peer binds immediately after
    // startup so ~200ms is typically enough; we give 5s to tolerate cold
    // starts on slow machines.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !socket.exists() {
        if std::time::Instant::now() > deadline {
            anyhow::bail!(
                "peer child did not create socket at {} within 5s",
                socket.display()
            );
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }

    let peer_cfg = cc_core::config::McpServerConfig {
        name: "peer".to_string(),
        command: Some(socket.display().to_string()),
        args: Vec::new(),
        env: std::collections::HashMap::new(),
        url: None,
        server_type: "unix".to_string(),
    };

    Ok((child, peer_cfg))
}

async fn connect_mcp_manager_arc(config: &Config) -> Option<Arc<cc_mcp::McpManager>> {
    if config.mcp_servers.is_empty() {
        return None;
    }

    info!(
        count = config.mcp_servers.len(),
        "Connecting to MCP servers"
    );
    let mcp_manager = cc_mcp::McpManager::connect_all(&config.mcp_servers).await;
    Some(Arc::new(mcp_manager))
}

fn build_tools_with_mcp(
    mcp_manager: Option<Arc<cc_mcp::McpManager>>,
) -> Arc<Vec<Box<dyn cc_tools::Tool>>> {
    let mut v: Vec<Box<dyn cc_tools::Tool>> = cc_tools::all_tools();
    v.push(Box::new(cc_query::AgentTool));

    if let Some(ref manager_arc) = mcp_manager {
        for (server_name, tool_def) in manager_arc.all_tool_definitions() {
            let wrapper = McpToolWrapper {
                tool_def,
                server_name,
                manager: manager_arc.clone(),
            };
            v.push(Box::new(wrapper));
        }
        debug!(total_tools = v.len(), "MCP tools registered");
    }

    Arc::new(v)
}

// ---------------------------------------------------------------------------
// SDK headless mode: interactive conversational loop over stdin/stdout.
// Stays alive, reads JSON Lines from stdin, runs query loop per message.
// This is the mode the Claude Agent SDK (npm) expects.
// ---------------------------------------------------------------------------

/// Extract text from content field (string, array of blocks, or nested message).
fn extract_text_content(v: &serde_json::Value) -> String {
    if let Some(c) = v.get("content") {
        if c.is_string() {
            return c.as_str().unwrap_or("").to_string();
        }
        if c.is_array() {
            return c
                .as_array()
                .map(|arr| {
                    arr.iter()
                        .filter_map(|block| block.get("text").and_then(|t| t.as_str()))
                        .collect::<Vec<_>>()
                        .join("\n")
                })
                .unwrap_or_default();
        }
    }
    String::new()
}

/// Build the merged list of MCP servers from config + project .mcp.json.
fn build_mcp_list(tool_ctx: &ToolContext) -> Vec<serde_json::Value> {
    let mut servers: Vec<serde_json::Value> = tool_ctx
        .config
        .mcp_servers
        .iter()
        .map(|s| serde_json::json!({"name": s.name, "status": "configured"}))
        .collect();
    let known: std::collections::HashSet<String> = servers
        .iter()
        .filter_map(|s| s.get("name").and_then(|n| n.as_str()).map(String::from))
        .collect();
    for s in scan_mcp_servers(&tool_ctx.working_dir) {
        if let Some(name) = s.get("name").and_then(|n| n.as_str()) {
            if !known.contains(name) {
                servers.push(s);
            }
        }
    }
    servers
}

/// Scan .mcp.json in the project directory for configured MCP servers.
/// Returns a list of {name, status} objects for the system/init message.
fn scan_mcp_servers(project_dir: &std::path::Path) -> Vec<serde_json::Value> {
    let mut servers = Vec::new();
    let mcp_path = project_dir.join(".mcp.json");
    if mcp_path.exists() {
        if let Ok(content) = std::fs::read_to_string(&mcp_path) {
            if let Ok(data) = serde_json::from_str::<serde_json::Value>(&content) {
                if let Some(obj) = data.get("mcpServers").and_then(|s| s.as_object()) {
                    for name in obj.keys() {
                        servers.push(serde_json::json!({"name": name, "status": "configured"}));
                    }
                }
            }
        }
    }
    servers
}

/// Scan ~/.uppli/commands/ and .uppli/commands/ for custom skill files (.md).
/// Returns a list of skill names for the system/init message.
fn scan_skills(project_dir: &std::path::Path) -> Vec<String> {
    let mut skills = Vec::new();
    let dirs = [
        dirs::home_dir().map(|h| h.join(cc_core::constants::CONFIG_DIR_NAME).join("commands")),
        Some(
            project_dir
                .join(cc_core::constants::CONFIG_DIR_NAME)
                .join("commands"),
        ),
    ];
    for dir in dirs.into_iter().flatten() {
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().map(|e| e == "md").unwrap_or(false) {
                    if let Some(name) = path.file_stem().and_then(|s| s.to_str()) {
                        skills.push(name.to_string());
                    }
                }
            }
        }
    }
    skills.sort();
    skills.dedup();
    skills
}

async fn run_sdk_headless(
    _cli: &Cli,
    client: Arc<dyn cc_api::LlmProvider>,
    tools: Arc<Vec<Box<dyn cc_tools::Tool>>>,
    tool_ctx: ToolContext,
    query_config: cc_query::QueryConfig,
    cost_tracker: Arc<CostTracker>,
) -> anyhow::Result<()> {
    use cc_query::{QueryEvent, QueryOutcome};
    use tokio::io::{AsyncBufReadExt, BufReader};
    use tokio::sync::mpsc;
    use tokio_util::sync::CancellationToken;

    let mut messages: Vec<cc_core::types::Message> = Vec::new();
    let session_id = uuid::Uuid::new_v4().to_string();

    debug!("SDK headless mode: emitting system/init");

    // Emit system/init as first message (SDK expects this)
    let tool_names: Vec<String> = tools.iter().map(|t| t.name().to_string()).collect();
    let cwd = query_config.working_directory.as_deref().unwrap_or(".");
    let init_msg = serde_json::json!({
        "type": "system",
        "subtype": "init",
        "uuid": uuid::Uuid::new_v4().to_string(),
        "session_id": &session_id,
        "claude_code_version": cc_core::constants::APP_VERSION,
        "cwd": cwd,
        "tools": tool_names,
        "model": &query_config.model,
        "permissionMode": "bypassPermissions",
        "mcp_servers": build_mcp_list(&tool_ctx),
        "slash_commands": [],
        "output_style": "default",
        "skills": scan_skills(&tool_ctx.working_dir),
        "plugins": []
    });
    println!("{}", init_msg);
    {
        use std::io::Write;
        std::io::stdout().flush().ok();
    }

    debug!("SDK headless mode: waiting for messages on stdin");

    // Helper: write a JSON line to stdout and flush immediately.
    macro_rules! emit {
        ($json:expr) => {{
            println!("{}", $json);
            use std::io::Write;
            std::io::stdout().flush().ok();
        }};
    }

    // --- Concurrent stdin reader ---
    // Runs in a dedicated task so we can read control messages (interrupt, set_model)
    // while the query loop is executing. Messages are dispatched via channels.
    enum StdinMessage {
        UserMessage(cc_core::types::Message),
        ControlRequest {
            request_id: String,
            subtype: String,
            #[allow(dead_code)]
            payload: serde_json::Value,
        },
        Eof,
    }

    let (stdin_tx, mut stdin_rx) = mpsc::unbounded_channel::<StdinMessage>();
    tokio::spawn(async move {
        let stdin = tokio::io::stdin();
        let mut reader = BufReader::new(stdin);
        let mut line = String::new();

        loop {
            line.clear();
            match reader.read_line(&mut line).await {
                Ok(0) => {
                    let _ = stdin_tx.send(StdinMessage::Eof);
                    break;
                }
                Ok(_) => {}
                Err(_) => break,
            }

            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }

            let v: serde_json::Value = match serde_json::from_str(trimmed) {
                Ok(v) => v,
                Err(e) => {
                    debug!(error = %e, "stdin: malformed JSON");
                    continue;
                }
            };

            let msg_type = v.get("type").and_then(|t| t.as_str()).unwrap_or("");

            match msg_type {
                // Control requests from the SDK (interrupt, set_model, etc.)
                "control_request" => {
                    let request_id = v
                        .get("request_id")
                        .and_then(|r| r.as_str())
                        .unwrap_or("")
                        .to_string();
                    let subtype = v
                        .get("request")
                        .and_then(|r| r.get("subtype"))
                        .and_then(|s| s.as_str())
                        .unwrap_or("")
                        .to_string();
                    let payload = v.get("request").cloned().unwrap_or_default();
                    let _ = stdin_tx.send(StdinMessage::ControlRequest {
                        request_id,
                        subtype,
                        payload,
                    });
                }
                // Control responses (from permission callbacks — ignore for now)
                "control_response" | "control_cancel_request" => {
                    debug!("stdin: received {}, ignoring", msg_type);
                }
                // User or assistant messages
                "user" | "assistant" => {
                    let inner = v.get("message").unwrap_or(&v);
                    let role = inner
                        .get("role")
                        .and_then(|r| r.as_str())
                        .unwrap_or(msg_type);
                    let content = extract_text_content(inner);
                    if !content.is_empty() {
                        let msg = if role == "assistant" {
                            cc_core::types::Message::assistant(content)
                        } else {
                            cc_core::types::Message::user(content)
                        };
                        let _ = stdin_tx.send(StdinMessage::UserMessage(msg));
                    }
                }
                // Simple format fallback
                _ => {
                    if v.get("role").is_some() {
                        let role = v.get("role").and_then(|r| r.as_str()).unwrap_or("user");
                        let content = extract_text_content(&v);
                        if !content.is_empty() {
                            let msg = if role == "assistant" {
                                cc_core::types::Message::assistant(content)
                            } else {
                                cc_core::types::Message::user(content)
                            };
                            let _ = stdin_tx.send(StdinMessage::UserMessage(msg));
                        }
                    } else {
                        debug!("stdin: unknown message type '{}'", msg_type);
                    }
                }
            }
        }
    });

    // Active cancel token for the current query (if any).
    let mut active_cancel: Option<CancellationToken> = None;

    // Runtime-mutable model — can be changed via control_request set_model.
    let mut active_model = query_config.model.clone();

    // --- Main loop: wait for user messages from stdin channel ---
    loop {
        let user_msg = match stdin_rx.recv().await {
            Some(StdinMessage::UserMessage(msg)) => msg,
            Some(StdinMessage::ControlRequest {
                request_id,
                subtype,
                payload,
            }) => {
                // Handle control request immediately
                let response = match subtype.as_str() {
                    "interrupt" => {
                        if let Some(ref ct) = active_cancel {
                            ct.cancel();
                            debug!("control: interrupted active query");
                        }
                        serde_json::json!({"subtype": "success", "request_id": request_id})
                    }
                    "set_model" => {
                        if let Some(model) = payload.get("model").and_then(|m| m.as_str()) {
                            info!(model, "control: switching model");
                            active_model = model.to_string();
                        }
                        serde_json::json!({"subtype": "success", "request_id": request_id})
                    }
                    "set_max_thinking_tokens" => {
                        if let Some(tokens) =
                            payload.get("max_thinking_tokens").and_then(|t| t.as_u64())
                        {
                            info!(tokens, "control: setting thinking tokens");
                            // Thinking toggle: switch to fast model (no thinking)
                            // or back to default model (with thinking).
                            let caps = client.capabilities();
                            if tokens == 0 {
                                if let Some(ref fast) = caps.fast_model {
                                    active_model = fast.clone();
                                    info!(model = %active_model, "control: thinking disabled");
                                }
                            } else {
                                active_model = caps.default_model.clone();
                                info!(model = %active_model, "control: thinking enabled");
                            }
                        }
                        serde_json::json!({"subtype": "success", "request_id": request_id})
                    }
                    _ => {
                        debug!(subtype = %subtype, "control: unhandled subtype");
                        serde_json::json!({"subtype": "success", "request_id": request_id})
                    }
                };
                emit!(serde_json::json!({
                    "type": "control_response",
                    "response": response,
                }));
                continue;
            }
            Some(StdinMessage::Eof) | None => {
                debug!("SDK headless: stdin closed, exiting");
                break;
            }
        };

        // Intercept slash commands before sending to the LLM.
        // Simple commands are handled locally without consuming API tokens.
        let user_text = user_msg.get_all_text();
        if user_text.starts_with('/') {
            let cmd = user_text.trim_start_matches('/');
            let (cmd_name, _cmd_args) = cmd.split_once(' ').unwrap_or((cmd, ""));

            let handled = match cmd_name {
                "clear" => {
                    messages.clear();
                    Some("Conversation cleared.")
                }
                "cost" => {
                    let cost = cost_tracker.total_cost_usd();
                    let input = cost_tracker.input_tokens();
                    let output = cost_tracker.output_tokens();
                    let msg = format!(
                        "Session cost: ${:.4}\nTokens: {} input, {} output",
                        cost, input, output
                    );
                    emit!(serde_json::json!({
                        "type": "assistant",
                        "uuid": uuid::Uuid::new_v4().to_string(),
                        "session_id": &session_id,
                        "message": {
                            "id": format!("msg_{}", uuid::Uuid::new_v4().to_string().get(..8).unwrap_or("0")),
                            "model": &active_model,
                            "role": "assistant",
                            "content": [{"type": "text", "text": msg}],
                            "stop_reason": "end_turn",
                            "usage": {"input_tokens": 0, "output_tokens": 0},
                        },
                        "parent_tool_use_id": null,
                    }));
                    emit!(serde_json::json!({
                        "type": "result",
                        "subtype": "success",
                        "uuid": uuid::Uuid::new_v4().to_string(),
                        "session_id": &session_id,
                        "duration_ms": 0, "duration_api_ms": 0,
                        "is_error": false, "num_turns": 0,
                        "result": "", "stop_reason": "end_turn",
                        "total_cost_usd": cost_tracker.total_cost_usd(),
                        "usage": {"input_tokens": 0, "output_tokens": 0},
                        "modelUsage": {}, "permission_denials": [],
                    }));
                    continue; // Skip the query loop entirely
                }
                "compact" => {
                    Some("Context compacted. (Note: auto-compact is active at 95% capacity)")
                }
                "help" => {
                    Some("Available commands:\n\
                        /help — Show this help\n\
                        /clear — Clear conversation\n\
                        /cost — Show session cost\n\
                        /compact — Compact context\n\
                        /model — Current model info\n\
                        /fast — Toggle fast mode\n\
                        /thinking — Toggle extended thinking\n\
                        /effort — Set effort level\n\
                        /memory — View UPPLI.md files\n\
                        /init — Create UPPLI.md\n\
                        /mcp — Manage MCP servers\n\
                        /config — View settings\n\
                        /diff — View file changes\n\
                        /review — Code review\n\
                        /commit — Git commit\n\
                        /doctor — Run diagnostics\n\
                        /provider — Show/switch LLM provider\n\
                        /status — System status")
                }
                "model" => {
                    Some(&*format!("Current model: {}", &active_model))
                }
                "mcp" => {
                    Some("MCP Servers:\n\
                        No MCP servers currently connected.\n\n\
                        To configure MCP servers, add them to:\n\
                        - Project: .mcp.json (in project root)\n\
                        - Global: ~/.uppli/settings.json\n\n\
                        Example .mcp.json:\n\
                        ```json\n\
                        {\n  \"mcpServers\": {\n    \"filesystem\": {\n      \"command\": \"npx\",\n      \"args\": [\"@modelcontextprotocol/server-filesystem\", \"/path\"]\n    }\n  }\n}\n\
                        ```\n\n\
                        Available MCP servers: filesystem, github, postgres, sqlite, slack, memory")
                }
                "provider" => {
                    let caps = client.capabilities();
                    Some(&*format!(
                        "Provider: {} ({})\n\
                        Default model: {}\n\
                        Fast model: {}\n\
                        Thinking: {}\n\n\
                        Available providers: deepseek, ollama, alibaba, openrouter, openai\n\
                        Switch: uppli-code --provider <name>",
                        caps.name,
                        caps.api_format,
                        caps.default_model,
                        caps.fast_model.as_deref().unwrap_or("none"),
                        if caps.default_thinking_budget.is_some() { "supported" } else { "not supported" },
                    ))
                }
                "status" => {
                    let caps = client.capabilities();
                    let cost = cost_tracker.total_cost_usd();
                    Some(&*format!(
                        "Uppli Code Status:\n\
                        Provider: {}\n\
                        Model: {}\n\
                        Session cost: ${:.4}\n\
                        Messages: {}\n\
                        Config: ~/.uppli/\n\
                        Memory: UPPLI.md",
                        caps.name, &active_model, cost, messages.len()
                    ))
                }
                "version" => {
                    Some(&*format!("Uppli Code v{}", cc_core::constants::APP_VERSION))
                }
                _ => None, // Unknown command → send to LLM
            };

            if let Some(response_text) = handled {
                let response_owned = response_text.to_string();
                emit!(serde_json::json!({
                    "type": "assistant",
                    "uuid": uuid::Uuid::new_v4().to_string(),
                    "session_id": &session_id,
                    "message": {
                        "id": format!("msg_{}", uuid::Uuid::new_v4().to_string().get(..8).unwrap_or("0")),
                        "model": &active_model,
                        "role": "assistant",
                        "content": [{"type": "text", "text": response_owned}],
                        "stop_reason": "end_turn",
                        "usage": {"input_tokens": 0, "output_tokens": 0},
                    },
                    "parent_tool_use_id": null,
                }));
                emit!(serde_json::json!({
                    "type": "result",
                    "subtype": "success",
                    "uuid": uuid::Uuid::new_v4().to_string(),
                    "session_id": &session_id,
                    "duration_ms": 0, "duration_api_ms": 0,
                    "is_error": false, "num_turns": 0,
                    "result": "", "stop_reason": "end_turn",
                    "total_cost_usd": cost_tracker.total_cost_usd(),
                    "usage": {"input_tokens": 0, "output_tokens": 0},
                    "modelUsage": {}, "permission_denials": [],
                }));
                continue; // Don't send to LLM
            }
            // Unknown slash command → falls through to LLM
        }

        messages.push(user_msg);

        // Run the query loop for this turn
        let (event_tx, mut event_rx) = mpsc::unbounded_channel::<QueryEvent>();
        let cancel = CancellationToken::new();
        active_cancel = Some(cancel.clone());

        let client_clone = client.clone();
        let tool_ctx_clone = tool_ctx.clone();
        let mut qcfg = query_config.clone();
        // Apply runtime model override (from set_model control_request)
        qcfg.model = active_model.clone();
        let tracker_clone = cost_tracker.clone();
        let event_tx_clone = event_tx.clone();
        let cancel_clone = cancel.clone();
        let tools_clone = tools.clone();
        let mut msgs_for_query = messages.clone();

        let query_handle = tokio::spawn(async move {
            let outcome = cc_query::run_query_loop(
                client_clone.as_ref(),
                &mut msgs_for_query,
                tools_clone.as_slice(),
                &tool_ctx_clone,
                &qcfg,
                tracker_clone,
                Some(event_tx_clone),
                cancel_clone,
                None,
            )
            .await;
            (outcome, msgs_for_query)
        });

        drop(event_tx);

        // Stream events to stdout as JSON Lines (SDKMessage format).
        //
        // The SDK consumer expects:
        //   1. Real-time partial messages (text/thinking deltas) for live UI updates
        //   2. Complete assistant messages at the end of each turn
        //
        // We emit both: partial content as it arrives (for streaming UX) and
        // a full assistant message when the turn completes (for state management).
        let mut full_text = String::new();
        let mut thinking_text = String::new();
        let msg_uuid = uuid::Uuid::new_v4().to_string();
        let mut last_model = query_config.model.clone();
        let mut last_usage = serde_json::json!({"input_tokens":0,"output_tokens":0});
        let mut stop_reason = "end_turn".to_string();
        let mut tool_uses: Vec<serde_json::Value> = Vec::new();

        // emit! macro is defined at the function level above

        let mut stdin_eof = false;
        loop {
            // Select between query events and control requests from stdin.
            // This allows interrupt to work while the query is running.
            // Note: stdin EOF does NOT break this loop — we must finish
            // processing query events even after the pipe closes.
            let event = tokio::select! {
                ev = event_rx.recv() => match ev {
                    Some(ev) => ev,
                    None => break, // query loop finished — all events processed
                },
                msg = stdin_rx.recv(), if !stdin_eof => {
                    match msg {
                        Some(StdinMessage::ControlRequest { request_id, subtype, .. }) => {
                            if subtype == "interrupt" {
                                if let Some(ref ct) = active_cancel {
                                    ct.cancel();
                                    debug!("control: interrupted query during execution");
                                }
                            }
                            emit!(serde_json::json!({
                                "type": "control_response",
                                "response": {"subtype": "success", "request_id": request_id},
                            }));
                            continue;
                        }
                        Some(StdinMessage::Eof) | None => {
                            // stdin closed but query is still running — stop
                            // listening to stdin but keep draining query events.
                            stdin_eof = true;
                            continue;
                        }
                        _ => continue,
                    }
                },
            };
            match &event {
                // --- Text delta → accumulate and emit partial for streaming UX ---
                QueryEvent::Stream(cc_api::StreamEvent::ContentBlockDelta {
                    delta: cc_api::streaming::ContentDelta::TextDelta { text },
                    ..
                }) => {
                    full_text.push_str(text);
                    // Emit partial assistant with the SAME uuid so Claudix
                    // replaces the existing message instead of adding a new one.
                    emit!(serde_json::json!({
                        "type": "assistant",
                        "uuid": &msg_uuid,
                        "session_id": &session_id,
                        "message": {
                            "id": format!("msg_{}", &msg_uuid[..8]),
                            "model": &last_model,
                            "role": "assistant",
                            "content": [{"type": "text", "text": &full_text}],
                            "stop_reason": serde_json::Value::Null,
                            "usage": &last_usage,
                        },
                        "parent_tool_use_id": null,
                    }));
                }
                // --- Thinking delta → accumulate (not shown in real-time) ---
                QueryEvent::Stream(cc_api::StreamEvent::ContentBlockDelta {
                    delta: cc_api::streaming::ContentDelta::ThinkingDelta { thinking },
                    ..
                }) => {
                    thinking_text.push_str(thinking);
                }
                // --- Message start → record model and initial usage ---
                QueryEvent::Stream(cc_api::StreamEvent::MessageStart { model, usage, .. }) => {
                    last_model = model.clone();
                    last_usage = serde_json::json!({
                        "input_tokens": usage.input_tokens,
                        "output_tokens": usage.output_tokens,
                        "cache_creation_input_tokens": usage.cache_creation_input_tokens,
                        "cache_read_input_tokens": usage.cache_read_input_tokens,
                    });
                }
                // --- Message delta → update stop reason and usage ---
                QueryEvent::Stream(cc_api::StreamEvent::MessageDelta {
                    stop_reason: sr,
                    usage,
                }) => {
                    if let Some(sr) = sr {
                        stop_reason = sr.clone();
                    }
                    if let Some(u) = usage {
                        last_usage = serde_json::json!({
                            "input_tokens": u.input_tokens,
                            "output_tokens": u.output_tokens,
                            "cache_creation_input_tokens": u.cache_creation_input_tokens,
                            "cache_read_input_tokens": u.cache_read_input_tokens,
                        });
                    }
                }
                // --- Tool start → track for the final assistant message ---
                QueryEvent::ToolStart {
                    tool_name,
                    tool_id,
                    input_json,
                } => {
                    let input: serde_json::Value = serde_json::from_str(input_json)
                        .unwrap_or(serde_json::Value::Object(Default::default()));
                    tool_uses.push(serde_json::json!({
                        "type": "tool_use",
                        "id": tool_id,
                        "name": tool_name,
                        "input": input,
                    }));
                }
                // --- Turn complete → emit final assistant message with all content ---
                QueryEvent::TurnComplete {
                    stop_reason: sr,
                    usage,
                    ..
                } => {
                    stop_reason = sr.clone();
                    if let Some(u) = usage {
                        last_usage = serde_json::json!({
                            "input_tokens": u.input_tokens,
                            "output_tokens": u.output_tokens,
                            "cache_creation_input_tokens": u.cache_creation_input_tokens,
                            "cache_read_input_tokens": u.cache_read_input_tokens,
                        });
                    }

                    // Build complete content array
                    let mut content = Vec::new();
                    if !thinking_text.is_empty() {
                        content.push(
                            serde_json::json!({"type":"thinking","thinking": &thinking_text}),
                        );
                    }
                    if !full_text.is_empty() {
                        content.push(serde_json::json!({"type":"text","text": &full_text}));
                    }
                    for tu in &tool_uses {
                        content.push(tu.clone());
                    }

                    emit!(serde_json::json!({
                        "type": "assistant",
                        "uuid": &msg_uuid,
                        "session_id": &session_id,
                        "message": {
                            "id": format!("msg_{}", &msg_uuid[..8]),
                            "model": &last_model,
                            "role": "assistant",
                            "content": content,
                            "stop_reason": &stop_reason,
                            "usage": &last_usage,
                        },
                        "parent_tool_use_id": null,
                    }));

                    // Reset for next turn (multi-tool-use within same query loop)
                    thinking_text.clear();
                    tool_uses.clear();
                }
                _ => {}
            }
        }

        // Query finished — clear the active cancel token
        active_cancel = None;

        // Get the outcome and updated messages
        let (outcome, updated_msgs) = query_handle.await.unwrap_or_else(|_| {
            (
                QueryOutcome::Error(cc_core::error::ClaudeError::Other(
                    "Query task panicked".to_string(),
                )),
                messages.clone(),
            )
        });

        // Sync messages with what the query loop produced (includes tool calls/results)
        messages = updated_msgs;

        // Persist the latest messages to the session JSONL transcript.
        // This enables --resume and /resume to reload the conversation.
        {
            let transcript_path =
                cc_core::session_storage::transcript_path(&tool_ctx.working_dir, &session_id);
            // Write the last user message + all new assistant/tool messages.
            // We write the user message we pushed earlier and the assistant response.
            if let Some(last_user) = messages
                .iter()
                .rev()
                .find(|m| m.role == cc_core::types::Role::User)
            {
                let entry = cc_core::session_storage::TranscriptEntry::User(
                    cc_core::session_storage::TranscriptMessage {
                        uuid: Some(uuid::Uuid::new_v4().to_string()),
                        parent_uuid: None,
                        timestamp: chrono::Utc::now().to_rfc3339(),
                        session_id: session_id.clone(),
                        cwd: tool_ctx.working_dir.display().to_string(),
                        message: last_user.clone(),
                        is_sidechain: false,
                        user_type: "external".to_string(),
                        version: cc_core::constants::APP_VERSION.to_string(),
                        git_branch: None,
                        extra: Default::default(),
                    },
                );
                let _ = cc_core::session_storage::write_transcript_entry(&transcript_path, &entry)
                    .await;
            }
            if let Some(last_asst) = messages
                .iter()
                .rev()
                .find(|m| m.role == cc_core::types::Role::Assistant)
            {
                let entry = cc_core::session_storage::TranscriptEntry::Assistant(
                    cc_core::session_storage::TranscriptMessage {
                        uuid: Some(msg_uuid.clone()),
                        parent_uuid: None,
                        timestamp: chrono::Utc::now().to_rfc3339(),
                        session_id: session_id.clone(),
                        cwd: tool_ctx.working_dir.display().to_string(),
                        message: last_asst.clone(),
                        is_sidechain: false,
                        user_type: "external".to_string(),
                        version: cc_core::constants::APP_VERSION.to_string(),
                        git_branch: None,
                        extra: Default::default(),
                    },
                );
                let _ = cc_core::session_storage::write_transcript_entry(&transcript_path, &entry)
                    .await;
            }
        }

        // Emit SDKResultMessage
        let result_json = match &outcome {
            QueryOutcome::EndTurn { usage, .. } => {
                serde_json::json!({
                    "type": "result",
                    "subtype": "success",
                    "uuid": uuid::Uuid::new_v4().to_string(),
                    "session_id": &session_id,
                    "duration_ms": 0,
                    "duration_api_ms": 0,
                    "is_error": false,
                    "num_turns": 1,
                    "result": &full_text,
                    "stop_reason": &stop_reason,
                    "total_cost_usd": cost_tracker.total_cost_usd(),
                    "usage": {
                        "input_tokens": usage.input_tokens,
                        "output_tokens": usage.output_tokens,
                        "cache_creation_input_tokens": usage.cache_creation_input_tokens,
                        "cache_read_input_tokens": usage.cache_read_input_tokens,
                    },
                    "modelUsage": {},
                    "permission_denials": [],
                })
            }
            QueryOutcome::Error(e) => {
                serde_json::json!({
                    "type": "result",
                    "subtype": "error_during_execution",
                    "uuid": uuid::Uuid::new_v4().to_string(),
                    "session_id": &session_id,
                    "duration_ms": 0,
                    "duration_api_ms": 0,
                    "is_error": true,
                    "num_turns": 0,
                    "stop_reason": null,
                    "total_cost_usd": cost_tracker.total_cost_usd(),
                    "usage": {"input_tokens":0,"output_tokens":0},
                    "modelUsage": {},
                    "permission_denials": [],
                    "errors": [e.to_string()],
                })
            }
            _ => {
                serde_json::json!({
                    "type": "result",
                    "subtype": "error_during_execution",
                    "uuid": uuid::Uuid::new_v4().to_string(),
                    "session_id": &session_id,
                    "duration_ms": 0,
                    "duration_api_ms": 0,
                    "is_error": true,
                    "num_turns": 0,
                    "stop_reason": null,
                    "total_cost_usd": cost_tracker.total_cost_usd(),
                    "usage": {"input_tokens":0,"output_tokens":0},
                    "modelUsage": {},
                    "permission_denials": [],
                    "errors": ["cancelled_or_budget_exceeded"],
                })
            }
        };
        println!("{}", result_json);
        {
            use std::io::Write;
            std::io::stdout().flush().ok();
        }

        // Continue the loop — wait for next message on stdin
        debug!(
            msg_count = messages.len(),
            "SDK headless: turn complete, waiting for next message"
        );
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Headless mode: read prompt from arg/stdin, run, print response (one-shot)
// ---------------------------------------------------------------------------

async fn run_headless(
    cli: &Cli,
    client: Arc<dyn cc_api::LlmProvider>,
    tools: Arc<Vec<Box<dyn cc_tools::Tool>>>,
    tool_ctx: ToolContext,
    query_config: cc_query::QueryConfig,
    cost_tracker: Arc<CostTracker>,
) -> anyhow::Result<()> {
    use cc_query::{QueryEvent, QueryOutcome};
    use tokio::sync::mpsc;
    use tokio_util::sync::CancellationToken;

    // If --resume is set, load prior messages from the session JSONL.
    //
    // Messages are loaded in their original format and prepended to the
    // conversation. The provider's translate_message() handles format
    // conversion (thinking blocks, tool_use/tool_result) for the API.
    // In TUI mode these messages also appear in the scrollable history.
    let mut prior_messages: Vec<cc_core::types::Message> = Vec::new();
    if let Some(ref session_id) = cli.resume {
        let tp = cc_core::session_storage::transcript_path(&tool_ctx.working_dir, session_id);
        if let Ok(entries) = cc_core::session_storage::load_transcript(&tp).await {
            let loaded =
                cc_core::session_storage::messages_from_transcript(&entries);
            if !loaded.is_empty() {
                eprintln!(
                    "Resumed session '{}' ({} messages)",
                    session_id,
                    loaded.len()
                );
                prior_messages = loaded;
            }
        }
    }

    // Build initial messages list from input.
    let mut messages: Vec<cc_core::types::Message> =
        if cli.input_format == CliInputFormat::StreamJson {
            use tokio::io::{self, AsyncBufReadExt, BufReader};
            let stdin = io::stdin();
            let mut reader = BufReader::new(stdin);
            let mut line = String::new();
            let mut parsed: Vec<cc_core::types::Message> = Vec::new();
            loop {
                line.clear();
                let n = reader.read_line(&mut line).await?;
                if n == 0 {
                    break;
                }
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                match serde_json::from_str::<serde_json::Value>(trimmed) {
                    Ok(v) => {
                        let role = v.get("role").and_then(|r| r.as_str()).unwrap_or("user");
                        let content = v
                            .get("content")
                            .and_then(|c| c.as_str())
                            .unwrap_or("")
                            .to_string();
                        if role == "assistant" {
                            parsed.push(cc_core::types::Message::assistant(content));
                        } else {
                            parsed.push(cc_core::types::Message::user(content));
                        }
                    }
                    Err(e) => {
                        eprintln!(
                            "Warning: skipping malformed JSON line: {} ({:?})",
                            trimmed, e
                        );
                    }
                }
            }
            if parsed.is_empty() {
                // Also check positional arg as fallback
                if let Some(ref p) = cli.prompt {
                    parsed.push(cc_core::types::Message::user(p.clone()));
                }
            }
            parsed
        } else {
            // Plain text mode
            let prompt = if let Some(ref p) = cli.prompt {
                p.clone()
            } else {
                use tokio::io::{self, AsyncReadExt};
                let mut stdin = io::stdin();
                let mut buf = String::new();
                stdin.read_to_string(&mut buf).await?;
                buf.trim().to_string()
            };

            if prompt.is_empty() {
                eprintln!("Error: No prompt provided. Use --print <prompt> or pipe text to stdin.");
                std::process::exit(1);
            }

            vec![cc_core::types::Message::user(prompt)]
        };

    // Prepend resumed messages before the new prompt
    if !prior_messages.is_empty() {
        let mut full = prior_messages;
        full.extend(messages);
        messages = full;
    }

    // --prefill: inject a partial assistant turn before the query so the model
    // continues from that text (mirrors TS --prefill flag).
    if let Some(ref prefill_text) = cli.prefill {
        messages.push(cc_core::types::Message::assistant(prefill_text.clone()));
    }

    if messages.is_empty() {
        eprintln!("Error: No messages provided.");
        std::process::exit(1);
    }

    let is_json_output = matches!(
        cli.output_format,
        CliOutputFormat::Json | CliOutputFormat::StreamJson
    );
    let is_stream_json = matches!(cli.output_format, CliOutputFormat::StreamJson);

    let (event_tx, mut event_rx) = mpsc::unbounded_channel::<QueryEvent>();
    let cancel = CancellationToken::new();

    let client_clone = client.clone();
    let tool_ctx_clone = tool_ctx.clone();
    let qcfg = query_config.clone();
    let tracker_clone = cost_tracker.clone();
    let event_tx_clone = event_tx.clone();
    let cancel_clone = cancel.clone();
    // Save info for session persistence (before messages is moved into spawn)
    let headless_working_dir = tool_ctx.working_dir.clone();
    let headless_user_msg = messages.first().cloned();

    let query_handle = tokio::spawn(async move {
        cc_query::run_query_loop(
            client_clone.as_ref(),
            &mut messages,
            tools.as_slice(),
            &tool_ctx_clone,
            &qcfg,
            tracker_clone,
            Some(event_tx_clone),
            cancel_clone,
            None,
        )
        .await
    });

    // Drop the original tx so the channel closes when the task drops its clone
    drop(event_tx);

    // Drain events and print streaming text
    let mut full_text = String::new();

    while let Some(event) = event_rx.recv().await {
        match &event {
            QueryEvent::Stream(cc_api::StreamEvent::ContentBlockDelta {
                delta: cc_api::streaming::ContentDelta::TextDelta { text },
                ..
            }) => {
                full_text.push_str(text);
                if !is_json_output {
                    print!("{}", text);
                    use std::io::Write;
                    std::io::stdout().flush().ok();
                } else if is_stream_json {
                    let chunk = serde_json::json!({ "type": "text_delta", "text": text });
                    println!("{}", chunk);
                }
            }
            QueryEvent::ToolStart { tool_name, .. } => {
                if !is_json_output {
                    eprintln!("\n[{}...]", tool_name);
                } else {
                    let ev = serde_json::json!({ "type": "tool_start", "tool": tool_name });
                    println!("{}", ev);
                }
            }
            QueryEvent::Error(msg) => {
                if is_json_output {
                    let ev = serde_json::json!({ "type": "error", "error": msg });
                    eprintln!("{}", ev);
                } else {
                    eprintln!("\nError: {}", msg);
                }
            }
            _ => {}
        }
    }

    // Wait for the query task to finish and get the final outcome
    let outcome =
        query_handle
            .await
            .unwrap_or(QueryOutcome::Error(cc_core::error::ClaudeError::Other(
                "Query task panicked".to_string(),
            )));

    // Persist session transcript (headless mode)
    if let QueryOutcome::EndTurn { ref message, .. } = outcome {
        let sid = cli.session_id_flag.as_deref().unwrap_or("headless");
        let tp = cc_core::session_storage::transcript_path(&headless_working_dir, sid);
        let cwd_str = headless_working_dir.display().to_string();
        if let Some(ref user_msg) = headless_user_msg {
            let entry = cc_core::session_storage::TranscriptEntry::User(
                cc_core::session_storage::TranscriptMessage {
                    uuid: Some(uuid::Uuid::new_v4().to_string()),
                    parent_uuid: None,
                    timestamp: chrono::Utc::now().to_rfc3339(),
                    session_id: sid.to_string(),
                    cwd: cwd_str.clone(),
                    message: user_msg.clone(),
                    is_sidechain: false,
                    user_type: "external".to_string(),
                    version: cc_core::constants::APP_VERSION.to_string(),
                    git_branch: None,
                    extra: Default::default(),
                },
            );
            let _ = cc_core::session_storage::write_transcript_entry(&tp, &entry).await;
        }
        let entry = cc_core::session_storage::TranscriptEntry::Assistant(
            cc_core::session_storage::TranscriptMessage {
                uuid: Some(uuid::Uuid::new_v4().to_string()),
                parent_uuid: None,
                timestamp: chrono::Utc::now().to_rfc3339(),
                session_id: sid.to_string(),
                cwd: cwd_str,
                message: message.clone(),
                is_sidechain: false,
                user_type: "external".to_string(),
                version: cc_core::constants::APP_VERSION.to_string(),
                git_branch: None,
                extra: Default::default(),
            },
        );
        let _ = cc_core::session_storage::write_transcript_entry(&tp, &entry).await;
    }

    // Final output
    match cli.output_format {
        CliOutputFormat::Json => match outcome {
            QueryOutcome::EndTurn { message, usage } => {
                let result_text = if full_text.is_empty() {
                    message.get_all_text()
                } else {
                    full_text
                };
                let out = serde_json::json!({
                    "type": "result",
                    "result": result_text,
                    "usage": {
                        "input_tokens": usage.input_tokens,
                        "output_tokens": usage.output_tokens,
                        "cache_creation_input_tokens": usage.cache_creation_input_tokens,
                        "cache_read_input_tokens": usage.cache_read_input_tokens,
                    },
                    "cost_usd": cost_tracker.total_cost_usd(),
                });
                println!("{}", out);
            }
            QueryOutcome::Error(e) => {
                let out = serde_json::json!({ "type": "error", "error": e.to_string() });
                eprintln!("{}", out);
                std::process::exit(1);
            }
            _ => {}
        },
        CliOutputFormat::StreamJson => {
            // Already streamed above; emit final result event
            match outcome {
                QueryOutcome::EndTurn { usage, .. } => {
                    let out = serde_json::json!({
                        "type": "result",
                        "usage": {
                            "input_tokens": usage.input_tokens,
                            "output_tokens": usage.output_tokens,
                        },
                        "cost_usd": cost_tracker.total_cost_usd(),
                    });
                    println!("{}", out);
                }
                QueryOutcome::Error(e) => {
                    let out = serde_json::json!({ "type": "error", "error": e.to_string() });
                    eprintln!("{}", out);
                    std::process::exit(1);
                }
                _ => {}
            }
        }
        CliOutputFormat::Text => {
            // Streaming text was already printed; add newline
            println!();
            if cli.verbose {
                eprintln!(
                    "\nTokens: {} in / {} out | Cost: ${:.4}",
                    cost_tracker.input_tokens(),
                    cost_tracker.output_tokens(),
                    cost_tracker.total_cost_usd(),
                );
            }
            match outcome {
                QueryOutcome::Error(e) => {
                    eprintln!("Error: {}", e);
                    std::process::exit(1);
                }
                QueryOutcome::BudgetExceeded {
                    cost_usd,
                    limit_usd,
                } => {
                    eprintln!(
                        "Budget limit ${:.4} reached (spent ${:.4}). Stopping.",
                        limit_usd, cost_usd
                    );
                    std::process::exit(2);
                }
                _ => {}
            }
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Interactive REPL mode
// ---------------------------------------------------------------------------

async fn run_interactive(
    config: Config,
    settings: cc_core::config::Settings,
    client: Arc<dyn cc_api::LlmProvider>,
    tools: Arc<Vec<Box<dyn cc_tools::Tool>>>,
    tool_ctx: ToolContext,
    query_config: cc_query::QueryConfig,
    cost_tracker: Arc<CostTracker>,
    resume_id: Option<String>,
    bridge_config: Option<cc_bridge::BridgeConfig>,
) -> anyhow::Result<()> {
    use cc_bridge::{BridgeOutbound, TuiBridgeEvent};
    use cc_commands::{execute_command, CommandContext, CommandResult};
    use cc_query::{QueryEvent, QueryOutcome};
    use cc_tui::{
        bridge_state::BridgeConnectionState, notifications::NotificationKind, render::render_app,
        restore_terminal, setup_terminal, App,
    };
    use crossterm::event::{self, Event, KeyCode};
    use std::time::Duration;
    use tokio::sync::mpsc;
    use tokio_util::sync::CancellationToken;

    let mut tool_ctx = tool_ctx;
    let mut session = if let Some(ref id) = resume_id {
        match cc_core::history::load_session(id).await {
            Ok(session) => {
                println!("Resumed session: {}", id);
                if let Some(saved_dir) = session.working_dir.as_ref() {
                    let saved_path = std::path::PathBuf::from(saved_dir);
                    if saved_path.exists() {
                        tool_ctx.working_dir = saved_path;
                    }
                }
                tool_ctx.session_id = session.id.clone();
                session
            }
            Err(e) => {
                eprintln!("Warning: could not load session {}: {}", id, e);
                let mut session =
                    cc_core::history::ConversationSession::new(query_config.model.clone());
                session.id = tool_ctx.session_id.clone();
                session.working_dir = Some(tool_ctx.working_dir.display().to_string());
                session
            }
        }
    } else {
        let mut session = cc_core::history::ConversationSession::new(query_config.model.clone());
        session.id = tool_ctx.session_id.clone();
        session.working_dir = Some(tool_ctx.working_dir.display().to_string());
        session
    };
    let initial_messages = session.messages.clone();
    let base_query_config = query_config;
    let mut live_config = config.clone();
    if !session.model.is_empty() {
        live_config.model = Some(session.model.clone());
    }

    // Set up terminal
    let mut terminal = setup_terminal()?;

    // Handle SIGCONT (resume after suspend/sleep) by reinitializing the terminal.
    // Without this, the TUI freezes after laptop sleep/wake.
    #[cfg(unix)]
    {
        let _sigcont_handle = tokio::spawn(async {
            use tokio::signal::unix::{signal, SignalKind};
            // nix provides the platform-correct signal number
            let sigcont = nix::sys::signal::Signal::SIGCONT as i32;
            if let Ok(mut stream) = signal(SignalKind::from_raw(sigcont)) {
                while stream.recv().await.is_some() {
                    // Re-enable raw mode and alternate screen after resume
                    let _ = crossterm::terminal::enable_raw_mode();
                    let _ = crossterm::execute!(
                        std::io::stdout(),
                        crossterm::terminal::EnterAlternateScreen,
                        crossterm::event::EnableMouseCapture
                    );
                }
            }
        });
    }
    let mut app = App::new(live_config.clone(), cost_tracker.clone());
    // Initialize the model picker from the provider's self-description so the
    // picker shows the correct models, descriptions, and effort support for
    // whichever provider is active (DeepSeek, Alibaba, Ollama, etc.).
    app.set_provider_capabilities(client.capabilities());
    // Set the model name from the query config (provider-driven, not hardcoded).
    app.model_name = base_query_config.model.clone();
    // Sync initial effort level (from --effort flag or /effort command) to TUI indicator.
    if let Some(level) = base_query_config.effort_level {
        use cc_tui::EffortLevel as TuiEL;
        app.effort_level = match level {
            cc_core::effort::EffortLevel::Low => TuiEL::Low,
            cc_core::effort::EffortLevel::Medium => TuiEL::Normal,
            cc_core::effort::EffortLevel::High => TuiEL::High,
            cc_core::effort::EffortLevel::Max => TuiEL::Max,
        };
    }
    app.config.project_dir = Some(tool_ctx.working_dir.clone());
    app.attach_turn_diff_state(tool_ctx.file_history.clone(), tool_ctx.current_turn.clone());
    if let Some(manager) = tool_ctx.mcp_manager.clone() {
        app.attach_mcp_manager(manager);
    }
    app.replace_messages(initial_messages.clone());

    // Show onboarding if no provider configured (first launch or /provider).
    if !settings.has_completed_onboarding {
        app.onboarding_dialog.show();
    }

    // Home directory warning: mirror TS feedConfigs.tsx warningText
    let home_dir = dirs::home_dir();
    if home_dir.as_deref() == Some(tool_ctx.working_dir.as_path()) {
        app.home_dir_warning = true;
    }

    // Bypass permissions confirmation dialog: must be accepted before any work
    // Mirror TS BypassPermissionsModeDialog.tsx startup gate
    use cc_core::config::PermissionMode;
    if live_config.permission_mode == PermissionMode::BypassPermissions {
        app.bypass_permissions_dialog.show();
    }

    // Version-upgrade notice: record the current version for future comparisons.
    // (Actual upgrade notice UI is handled by the release-notes slash command.)
    {
        let current_version = cc_core::constants::APP_VERSION.to_string();
        if settings.last_seen_version.as_deref() != Some(&current_version)
            && settings.has_completed_onboarding
        {
            // Only persist version if onboarding is done — otherwise the async
            // save races with the onboarding dialog and overwrites user config.
            let version_clone = current_version.clone();
            tokio::spawn(async move {
                if let Ok(mut s) = cc_core::config::Settings::load().await {
                    s.last_seen_version = Some(version_clone);
                    let _ = s.save().await;
                }
            });
        }
    }

    // CLAUDE_STATUS_COMMAND: optional external command whose stdout replaces the
    // left-side status bar text. Polled every 500ms (debounced) in the main loop.
    // The command is run in a background task; results flow through a channel.
    let status_cmd_str = std::env::var("CLAUDE_STATUS_COMMAND").ok();
    let (status_cmd_tx, mut status_cmd_rx) = mpsc::channel::<String>(4);
    if let Some(ref cmd_str) = status_cmd_str {
        // Security: split the command into program + args using shell-word
        // parsing rules (respects quotes and escapes) instead of passing the
        // raw string through `sh -c` which is an injection vector.
        // Pipes/redirects are NOT supported — use a wrapper script for those.
        match shell_words::split(cmd_str) {
            Ok(parts) if !parts.is_empty() => {
                let program = parts[0].clone();
                let args: Vec<String> = parts[1..].to_vec();
                let tx = status_cmd_tx.clone();
                tokio::spawn(async move {
                    loop {
                        tokio::time::sleep(Duration::from_millis(500)).await;
                        let output = tokio::process::Command::new(&program)
                            .args(&args)
                            .output()
                            .await;
                        if let Ok(out) = output {
                            let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
                            let _ = tx.try_send(text);
                        }
                    }
                });
            }
            Ok(_) => {} // empty command string — ignore
            Err(e) => {
                warn!(
                    error = %e,
                    cmd = cmd_str,
                    "CLAUDE_STATUS_COMMAND has invalid quoting — ignoring"
                );
            }
        }
    }

    // Bridge runtime channels — Some when bridge is configured and started.
    //
    // tui_rx:       TUI-facing events from the bridge worker (connect/disconnect/prompts)
    // outbound_tx:  Forward query events to the bridge worker for upload to server
    // bridge_cancel: CancellationToken to stop the bridge worker task
    struct BridgeRuntime {
        tui_rx: mpsc::Receiver<TuiBridgeEvent>,
        outbound_tx: mpsc::Sender<BridgeOutbound>,
        cancel: CancellationToken,
    }

    let mut bridge_runtime: Option<BridgeRuntime> = if let Some(cfg) = bridge_config {
        let bridge_cancel = CancellationToken::new();
        let (tui_tx, tui_rx) = mpsc::channel::<TuiBridgeEvent>(64);
        let (outbound_tx, outbound_rx) = mpsc::channel::<BridgeOutbound>(256);

        // Update TUI state to "connecting" before the task starts.
        app.bridge_state = BridgeConnectionState::Connecting;

        let cancel_clone = bridge_cancel.clone();
        tokio::spawn(async move {
            if let Err(e) = cc_bridge::run_bridge_loop(cfg, tui_tx, outbound_rx, cancel_clone).await
            {
                warn!("Bridge loop exited with error: {}", e);
            }
        });

        Some(BridgeRuntime {
            tui_rx,
            outbound_tx,
            cancel: bridge_cancel,
        })
    } else {
        None
    };

    let mut messages = initial_messages;
    let mut cmd_ctx = CommandContext {
        config: live_config,
        cost_tracker: cost_tracker.clone(),
        messages: messages.clone(),
        working_dir: tool_ctx.working_dir.clone(),
        session_id: session.id.clone(),
        session_title: session.title.clone(),
        remote_session_url: session.remote_session_url.clone(),
        mcp_manager: tool_ctx.mcp_manager.clone(),
    };

    // tools is already Arc<Vec<...>> — share it across spawned tasks without copying.
    let mut tools_arc = tools;

    // Current cancel token (replaced each turn)
    let mut cancel: Option<CancellationToken> = None;
    let (event_tx, mut event_rx) = mpsc::unbounded_channel::<QueryEvent>();
    type MessagesArc = Arc<tokio::sync::Mutex<Vec<cc_core::types::Message>>>;
    let mut current_query: Option<(tokio::task::JoinHandle<QueryOutcome>, MessagesArc)> = None;
    // Active effort level (None = use model default / High).
    // Tracks the user's /effort selection; flows into qcfg each turn.
    let mut current_effort: Option<cc_core::effort::EffortLevel> = None;

    'main: loop {
        app.frame_count = app.frame_count.wrapping_add(1);

        // Draw the UI
        terminal.draw(|f| render_app(f, &app))?;

        // Poll for crossterm events (keyboard/mouse) with short timeout
        if crossterm::event::poll(Duration::from_millis(16))? {
            let evt = event::read()?;
            match evt {
                Event::Key(key) => {
                    // On Windows crossterm emits Press + Release for a single key.
                    // Only process Press to avoid double-registering input.
                    if key.kind != crossterm::event::KeyEventKind::Press {
                        continue;
                    }

                    // When a modal dialog is open, skip all main-loop key
                    // handling and let handle_key_event() deal with it.
                    if app.onboarding_dialog.visible {
                        app.handle_key_event(key);
                        continue;
                    }

                    // Ctrl+C while streaming => cancel
                    if key.code == KeyCode::Char('c')
                        && key
                            .modifiers
                            .contains(crossterm::event::KeyModifiers::CONTROL)
                    {
                        if app.is_streaming {
                            if let Some(ref ct) = cancel {
                                ct.cancel();
                            }
                            app.is_streaming = false;
                            app.status_message = Some("Cancelled.".to_string());
                            continue;
                        } else {
                            break 'main;
                        }
                    }

                    // ESC while streaming => cancel (same as Ctrl+C)
                    if key.code == KeyCode::Esc && app.is_streaming {
                        if let Some(ref ct) = cancel {
                            ct.cancel();
                        }
                        app.is_streaming = false;
                        app.status_message = Some("Cancelled.".to_string());
                        continue;
                    }

                    // Ctrl+D on empty input => quit
                    if key.code == KeyCode::Char('d')
                        && key
                            .modifiers
                            .contains(crossterm::event::KeyModifiers::CONTROL)
                        && app.prompt_input.is_empty()
                    {
                        break 'main;
                    }

                    // Enter => submit input
                    if key.code == KeyCode::Enter && !app.is_streaming {
                        // If a slash-command suggestion is active, accept it
                        // and wait for the next Enter to actually submit.
                        if !app.prompt_input.suggestions.is_empty()
                            && app.prompt_input.suggestion_index.is_some()
                            && app.prompt_input.text.starts_with('/')
                        {
                            app.prompt_input.accept_suggestion();
                            continue;
                        }

                        let input = app.take_input();
                        if input.is_empty() {
                            continue;
                        }

                        // Check for slash command
                        if input.starts_with('/') {
                            let (cmd_name, cmd_args) = cc_tui::input::parse_slash_command(&input);
                            let cmd_name = cmd_name.to_string();
                            let cmd_args = cmd_args.to_string();

                            // ── Step 1: TUI-layer intercept (overlays, toggles) ────────
                            // Run first so we know whether a UI overlay opened, which
                            // lets us suppress redundant CLI text output below.
                            //
                            // Skip TUI overlay for arg-bearing commands where the user
                            // wants to SET state, not browse a picker:
                            //   /model claude-haiku  → set model, don't open picker
                            //   /theme dark          → set theme, don't open picker
                            //   /resume <id>         → load session, don't open browser
                            // Also skip TUI for /vim, /voice, /fast with explicit
                            // on|off args so the blind-toggle doesn't misfire.
                            let skip_tui_for_args = !cmd_args.is_empty()
                                && matches!(
                                    cmd_name.as_str(),
                                    "model"
                                        | "theme"
                                        | "resume"
                                        | "session"
                                        | "vim"
                                        | "vi"
                                        | "voice"
                                        | "fast"
                                        | "speed"
                                );
                            let handled_by_tui = if skip_tui_for_args {
                                false
                            } else {
                                app.intercept_slash_command(&cmd_name)
                            };

                            // Sync effort level when TUI cycled the visual indicator
                            // (no-args /effort → cycle Low→Med→High→Max→Low).
                            if handled_by_tui && cmd_name == "effort" && cmd_args.is_empty() {
                                current_effort = Some(match app.effort_level {
                                    cc_tui::EffortLevel::Low => cc_core::effort::EffortLevel::Low,
                                    cc_tui::EffortLevel::Normal => {
                                        cc_core::effort::EffortLevel::Medium
                                    }
                                    cc_tui::EffortLevel::High => cc_core::effort::EffortLevel::High,
                                    cc_tui::EffortLevel::Max => cc_core::effort::EffortLevel::Max,
                                });
                            }

                            // Honour exit/quit triggered by TUI intercept immediately.
                            if app.should_quit {
                                break 'main;
                            }

                            // ── Step 2: CLI-layer (real side effects) ──────────────────
                            // Handles: config changes, session ops, file I/O, OAuth, etc.
                            // Always runs — some commands need BOTH (e.g. /clear clears
                            // app state via TUI AND the messages vec via CLI).
                            cmd_ctx.messages = messages.clone();
                            let cli_result = execute_command(&input, &mut cmd_ctx).await;
                            // Start optimistically true; set false for Silent/None below.
                            let mut handled_by_cli = cli_result.is_some();

                            // Whether we need to fall through and submit a user message.
                            let mut submit_user_msg: Option<String> = None;

                            match cli_result {
                                Some(CommandResult::Exit) => break 'main,
                                Some(CommandResult::ClearConversation) => {
                                    messages.clear();
                                    app.replace_messages(Vec::new());
                                    session.messages.clear();
                                    session.updated_at = chrono::Utc::now();
                                    app.status_message = Some("Conversation cleared.".to_string());
                                }
                                Some(CommandResult::SetMessages(new_msgs)) => {
                                    let removed = messages.len().saturating_sub(new_msgs.len());
                                    messages = new_msgs.clone();
                                    app.replace_messages(new_msgs);
                                    session.messages = messages.clone();
                                    session.updated_at = chrono::Utc::now();
                                    app.status_message = Some(format!(
                                        "Rewound {} message{}.",
                                        removed,
                                        if removed == 1 { "" } else { "s" }
                                    ));
                                }
                                Some(CommandResult::OpenRewindOverlay) => {
                                    app.replace_messages(messages.clone());
                                    app.open_rewind_flow();
                                    app.status_message =
                                        Some("Select a message to rewind to.".to_string());
                                }
                                Some(CommandResult::ResumeSession(resumed_session)) => {
                                    session = resumed_session;
                                    messages = session.messages.clone();
                                    app.replace_messages(messages.clone());
                                    cmd_ctx.config.model = Some(session.model.clone());
                                    app.config.model = Some(session.model.clone());
                                    tool_ctx.session_id = session.id.clone();
                                    tool_ctx.file_history = Arc::new(ParkingMutex::new(
                                        cc_core::file_history::FileHistory::new(),
                                    ));
                                    tool_ctx.current_turn =
                                        Arc::new(std::sync::atomic::AtomicUsize::new(0));
                                    cmd_ctx.session_id = session.id.clone();
                                    cmd_ctx.session_title = session.title.clone();
                                    if let Some(saved_dir) = session.working_dir.as_ref() {
                                        let saved_path = std::path::PathBuf::from(saved_dir);
                                        if saved_path.exists() {
                                            tool_ctx.working_dir = saved_path.clone();
                                            cmd_ctx.working_dir = saved_path;
                                        }
                                    }
                                    app.config.project_dir = Some(tool_ctx.working_dir.clone());
                                    app.attach_turn_diff_state(
                                        tool_ctx.file_history.clone(),
                                        tool_ctx.current_turn.clone(),
                                    );
                                    app.status_message =
                                        Some(format!("Resumed session {}.", &session.id[..8]));
                                }
                                Some(CommandResult::RenameSession(title)) => {
                                    session.title = Some(title.clone());
                                    session.updated_at = chrono::Utc::now();
                                    cmd_ctx.session_title = session.title.clone();
                                    let _ = cc_core::history::save_session(&session).await;
                                    app.status_message =
                                        Some(format!("Session renamed to \"{}\".", title));
                                }
                                Some(CommandResult::Message(msg)) => {
                                    // Suppress text output when TUI already opened an
                                    // overlay for this command (e.g. /stats opens dialog
                                    // AND would push a text message — drop the text).
                                    if !handled_by_tui {
                                        app.push_message(cc_core::types::Message::assistant(msg));
                                    }
                                }
                                Some(CommandResult::ConfigChange(new_cfg)) => {
                                    cmd_ctx.config = new_cfg.clone();
                                    app.config = new_cfg.clone();
                                    // Sync model name shown in the TUI header.
                                    if let Some(ref model) = new_cfg.model {
                                        app.model_name = model.clone();
                                    }
                                    // Sync fast_mode visual indicator.
                                    app.fast_mode = new_cfg
                                        .model
                                        .as_deref()
                                        .map(|m| m.contains("haiku"))
                                        .unwrap_or(false);
                                    // Sync plan_mode visual indicator.
                                    app.plan_mode = matches!(
                                        new_cfg.permission_mode,
                                        cc_core::config::PermissionMode::Plan
                                    );
                                    app.status_message = Some("Configuration updated.".to_string());
                                }
                                Some(CommandResult::ConfigChangeMessage(new_cfg, msg)) => {
                                    cmd_ctx.config = new_cfg.clone();
                                    // Sync model name + fast_mode visual indicator.
                                    if let Some(ref model) = new_cfg.model {
                                        app.model_name = model.clone();
                                        app.fast_mode = model.contains("haiku");
                                    } else {
                                        // model reset to None means fast mode off.
                                        app.fast_mode = false;
                                    }
                                    app.config = new_cfg;
                                    app.status_message = Some(msg);
                                }
                                Some(CommandResult::UserMessage(msg)) => {
                                    // Queue a user-visible turn for the model.
                                    submit_user_msg = Some(msg);
                                }
                                Some(CommandResult::StartOAuthFlow(with_claude_ai)) => {
                                    cc_tui::restore_terminal(&mut terminal).ok();
                                    match oauth_flow::run_oauth_login_flow(with_claude_ai).await {
                                        Ok(_) => {
                                            app.status_message =
                                                Some("Login successful!".to_string());
                                            eprintln!(
                                                "\nLogin successful! Please restart \
                                                 claude to use the new credentials."
                                            );
                                            break 'main;
                                        }
                                        Err(e) => {
                                            eprintln!("\nLogin failed: {}", e);
                                        }
                                    }
                                    terminal = cc_tui::setup_terminal()?;
                                }
                                Some(CommandResult::Error(e)) => {
                                    app.status_message = Some(format!("Error: {}", e));
                                }
                                Some(CommandResult::Silent) | None => {
                                    handled_by_cli = false;
                                }
                            }

                            // Sync effort visual + API level when CLI handled
                            // /effort with explicit args (/effort high).
                            if handled_by_cli && cmd_name == "effort" && !cmd_args.is_empty() {
                                if let Some(level) =
                                    cc_core::effort::EffortLevel::from_str(&cmd_args)
                                {
                                    current_effort = Some(level);
                                    app.effort_level = match level {
                                        cc_core::effort::EffortLevel::Low => {
                                            cc_tui::EffortLevel::Low
                                        }
                                        cc_core::effort::EffortLevel::Medium => {
                                            cc_tui::EffortLevel::Normal
                                        }
                                        cc_core::effort::EffortLevel::High => {
                                            cc_tui::EffortLevel::High
                                        }
                                        cc_core::effort::EffortLevel::Max => {
                                            cc_tui::EffortLevel::Max
                                        }
                                    };
                                    app.status_message = Some(format!(
                                        "Effort: {} {}",
                                        app.effort_level.symbol(),
                                        app.effort_level.label(),
                                    ));
                                }
                            }

                            // Sync vim mode when CLI handled /vim with explicit args.
                            if handled_by_cli
                                && matches!(cmd_name.as_str(), "vim" | "vi")
                                && !cmd_args.is_empty()
                            {
                                app.prompt_input.vim_enabled =
                                    matches!(cmd_args.trim(), "on" | "vim");
                            }

                            if !handled_by_cli && !handled_by_tui {
                                app.status_message =
                                    Some(format!("Unknown command: /{}", cmd_name));
                            }

                            // If a UserMessage was queued (e.g. /compact), submit it.
                            if let Some(msg) = submit_user_msg {
                                messages.push(cc_core::types::Message::user(msg.clone()));
                                app.push_message(cc_core::types::Message::user(msg));
                                // Fall through to the send path below.
                            } else {
                                continue;
                            }
                        }

                        // Fire UserPromptSubmit hook (non-blocking)
                        if !config.hooks.is_empty() {
                            let hook_ctx = cc_core::hooks::HookContext {
                                event: "UserPromptSubmit".to_string(),
                                tool_name: None,
                                tool_input: None,
                                tool_output: Some(input.clone()),
                                is_error: None,
                                session_id: Some(tool_ctx.session_id.clone()),
                            };
                            cc_core::hooks::run_hooks(
                                &config.hooks,
                                cc_core::config::HookEvent::UserPromptSubmit,
                                &hook_ctx,
                                &tool_ctx.working_dir,
                            )
                            .await;
                        }

                        // Regular user message (with optional image attachments)
                        let pending_imgs = app.prompt_input.clear_images();
                        let user_msg = if pending_imgs.is_empty() {
                            cc_core::types::Message::user(input.clone())
                        } else {
                            let mut blocks: Vec<cc_core::types::ContentBlock> = pending_imgs
                                .iter()
                                .filter_map(|img| {
                                    cc_tui::image_paste::encode_image_base64(&img.path).map(|b64| {
                                        cc_core::types::ContentBlock::Image {
                                            source: cc_core::types::ImageSource {
                                                source_type: "base64".to_string(),
                                                media_type: Some("image/png".to_string()),
                                                data: Some(b64),
                                                url: None,
                                            },
                                        }
                                    })
                                })
                                .collect();
                            blocks.push(cc_core::types::ContentBlock::Text {
                                text: input.clone(),
                            });
                            cc_core::types::Message::user_blocks(blocks)
                        };
                        messages.push(user_msg.clone());
                        app.push_message(user_msg);
                        session.messages = messages.clone();
                        session.updated_at = chrono::Utc::now();

                        // Start async query
                        app.is_streaming = true;
                        app.streaming_text.clear();

                        let ct = CancellationToken::new();
                        cancel = Some(ct.clone());

                        // Use Arc<Mutex> so the task can write updated messages back
                        let msgs_arc = Arc::new(tokio::sync::Mutex::new(messages.clone()));
                        let msgs_arc_clone = msgs_arc.clone();

                        // Share the Arc so the spawned task can access all tools (incl. MCP).
                        let tools_arc_clone = tools_arc.clone();
                        let ctx_clone = tool_ctx.clone();
                        let mut qcfg = base_query_config.clone();
                        qcfg.model = base_query_config.model.clone();
                        qcfg.max_tokens = cmd_ctx.config.effective_max_tokens();
                        qcfg.append_system_prompt = cmd_ctx.config.append_system_prompt.clone();
                        qcfg.system_prompt = base_query_config.system_prompt.clone();
                        qcfg.output_style = cmd_ctx.config.effective_output_style();
                        qcfg.output_style_prompt = cmd_ctx.config.resolve_output_style_prompt();
                        qcfg.working_directory = Some(tool_ctx.working_dir.display().to_string());
                        // Apply active effort level (set via /effort command).
                        if let Some(level) = current_effort {
                            qcfg.effort_level = Some(level);
                        }
                        let tracker = cost_tracker.clone();
                        let tx = event_tx.clone();
                        let client_clone = client.clone();

                        let handle = tokio::spawn(async move {
                            let mut msgs = msgs_arc_clone.lock().await.clone();
                            let outcome = cc_query::run_query_loop(
                                client_clone.as_ref(),
                                &mut msgs,
                                tools_arc_clone.as_slice(),
                                &ctx_clone,
                                &qcfg,
                                tracker,
                                Some(tx),
                                ct,
                                None,
                            )
                            .await;
                            // Write updated messages (with tool calls + assistant response) back
                            *msgs_arc_clone.lock().await = msgs;
                            outcome
                        });

                        // Store the Arc so we can read messages after task completes
                        current_query = Some((handle, msgs_arc));
                        continue;
                    }

                    app.handle_key_event(key);
                    if !app.is_streaming && app.messages.len() < messages.len() {
                        messages = app.messages.clone();
                        session.messages = messages.clone();
                        session.updated_at = chrono::Utc::now();
                    }
                }
                Event::Mouse(mouse_event) => {
                    app.handle_mouse_event(mouse_event);
                }
                Event::Paste(data) => {
                    if !app.is_streaming {
                        app.prompt_input.paste(&data);
                    }
                }
                Event::Resize(_, _) => {
                    // Terminal resize - will be handled on next draw
                }
                _ => {}
            }
        }

        // Drain query events — also forward relevant ones to the bridge as outbound.
        while let Ok(evt) = event_rx.try_recv() {
            // Forward to bridge before consuming (clone only what we need).
            if let Some(ref runtime) = bridge_runtime {
                let outbound: Option<BridgeOutbound> = match &evt {
                    QueryEvent::Stream(cc_api::StreamEvent::ContentBlockDelta {
                        delta: cc_api::streaming::ContentDelta::TextDelta { text },
                        index,
                        ..
                    }) => Some(BridgeOutbound::TextDelta {
                        delta: text.clone(),
                        message_id: format!("msg-{}", index),
                    }),
                    QueryEvent::ToolStart {
                        tool_name,
                        tool_id,
                        input_json,
                    } => Some(BridgeOutbound::ToolStart {
                        id: tool_id.clone(),
                        name: tool_name.clone(),
                        input_preview: Some(input_json.clone()),
                    }),
                    QueryEvent::ToolEnd {
                        tool_id,
                        result,
                        is_error,
                        ..
                    } => Some(BridgeOutbound::ToolEnd {
                        id: tool_id.clone(),
                        output: result.clone(),
                        is_error: *is_error,
                    }),
                    QueryEvent::TurnComplete {
                        stop_reason, turn, ..
                    } => Some(BridgeOutbound::TurnComplete {
                        message_id: format!("turn-{}", turn),
                        stop_reason: stop_reason.clone(),
                    }),
                    QueryEvent::Error(msg) => Some(BridgeOutbound::Error {
                        message: msg.clone(),
                    }),
                    _ => None,
                };
                if let Some(ob) = outbound {
                    let _ = runtime.outbound_tx.try_send(ob);
                }
            }
            app.handle_query_event(evt);
        }

        // Drain TUI-facing bridge events.
        let mut disconnect_bridge = false;
        if let Some(runtime) = bridge_runtime.as_mut() {
            loop {
                match runtime.tui_rx.try_recv() {
                    Ok(TuiBridgeEvent::Connected {
                        session_url,
                        session_id: _,
                    }) => {
                        let short = if session_url.len() > 60 {
                            format!("{}…", &session_url[..60])
                        } else {
                            session_url.clone()
                        };
                        app.bridge_state = BridgeConnectionState::Connected {
                            session_url: session_url.clone(),
                            peer_count: 0,
                        };
                        app.remote_session_url = Some(session_url.clone());
                        cmd_ctx.remote_session_url = Some(session_url.clone());
                        app.notifications.push(
                            NotificationKind::Success,
                            format!("Remote control active: {}", short),
                            Some(5),
                        );
                        // Persist the session URL into the saved session record.
                        session.remote_session_url = Some(session_url.clone());
                        session.updated_at = chrono::Utc::now();
                        let _ = cc_core::history::save_session(&session).await;
                    }
                    Ok(TuiBridgeEvent::Disconnected { reason }) => {
                        app.bridge_state = BridgeConnectionState::Disconnected;
                        app.remote_session_url = None;
                        cmd_ctx.remote_session_url = None;
                        if let Some(r) = reason {
                            app.notifications.push(
                                NotificationKind::Warning,
                                format!("Bridge disconnected: {}", r),
                                Some(5),
                            );
                        }
                        disconnect_bridge = true;
                        break;
                    }
                    Ok(TuiBridgeEvent::Reconnecting { attempt }) => {
                        app.bridge_state = BridgeConnectionState::Reconnecting { attempt };
                    }
                    Ok(TuiBridgeEvent::InboundPrompt { content, .. }) => {
                        // Inject the remote prompt as if the user typed it, then
                        // trigger submission automatically.
                        app.set_prompt_text(content.clone());
                        // Push as a user message and fire a query immediately.
                        messages.push(cc_core::types::Message::user(content.clone()));
                        app.push_message(cc_core::types::Message::user(content.clone()));
                        session.messages = messages.clone();
                        session.updated_at = chrono::Utc::now();
                        app.is_streaming = true;
                        app.streaming_text.clear();
                        let ct = CancellationToken::new();
                        cancel = Some(ct.clone());
                        let msgs_arc = Arc::new(tokio::sync::Mutex::new(messages.clone()));
                        let msgs_arc_clone = msgs_arc.clone();
                        let tools_arc_clone = tools_arc.clone();
                        let ctx_clone = tool_ctx.clone();
                        let mut qcfg = base_query_config.clone();
                        qcfg.model = base_query_config.model.clone();
                        qcfg.max_tokens = cmd_ctx.config.effective_max_tokens();
                        let tracker = cost_tracker.clone();
                        let tx = event_tx.clone();
                        let client_clone = client.clone();
                        let handle = tokio::spawn(async move {
                            let mut msgs = msgs_arc_clone.lock().await.clone();
                            let outcome = cc_query::run_query_loop(
                                client_clone.as_ref(),
                                &mut msgs,
                                tools_arc_clone.as_slice(),
                                &ctx_clone,
                                &qcfg,
                                tracker,
                                Some(tx),
                                ct,
                                None,
                            )
                            .await;
                            *msgs_arc_clone.lock().await = msgs;
                            outcome
                        });
                        current_query = Some((handle, msgs_arc));
                    }
                    Ok(TuiBridgeEvent::Cancelled) => {
                        if app.is_streaming {
                            if let Some(ref ct) = cancel {
                                ct.cancel();
                            }
                            app.is_streaming = false;
                            app.status_message = Some("Cancelled by remote control.".to_string());
                        }
                    }
                    Ok(TuiBridgeEvent::PermissionResponse {
                        tool_use_id,
                        response,
                    }) => {
                        // Resolve a pending permission dialog if IDs match.
                        if let Some(ref pr) = app.permission_request {
                            if pr.tool_use_id == tool_use_id {
                                use cc_bridge::PermissionResponseKind;
                                let _allow = matches!(
                                    response,
                                    PermissionResponseKind::Allow
                                        | PermissionResponseKind::AllowSession
                                );
                                app.permission_request = None;
                            }
                        }
                    }
                    Ok(TuiBridgeEvent::SessionNameUpdate { title }) => {
                        session.title = Some(title.clone());
                        session.updated_at = chrono::Utc::now();
                        cmd_ctx.session_title = Some(title.clone());
                        app.session_title = Some(title);
                        let _ = cc_core::history::save_session(&session).await;
                    }
                    Ok(TuiBridgeEvent::Error(msg)) => {
                        app.bridge_state = BridgeConnectionState::Failed {
                            reason: msg.clone(),
                        };
                        app.notifications.push(
                            NotificationKind::Warning,
                            format!("Bridge error: {}", msg),
                            Some(5),
                        );
                        disconnect_bridge = true;
                        break;
                    }
                    Ok(TuiBridgeEvent::Ping) => {
                        // No TUI action needed; pong is handled inside run_bridge_loop.
                    }
                    Err(tokio::sync::mpsc::error::TryRecvError::Empty) => break,
                    Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {
                        app.bridge_state = BridgeConnectionState::Disconnected;
                        app.remote_session_url = None;
                        cmd_ctx.remote_session_url = None;
                        app.notifications.push(
                            NotificationKind::Warning,
                            "Remote control connection lost.".to_string(),
                            Some(5),
                        );
                        disconnect_bridge = true;
                        break;
                    }
                }
            }
        }
        if disconnect_bridge {
            bridge_runtime = None;
        }

        // Drain CLAUDE_STATUS_COMMAND results (most recent wins)
        if status_cmd_str.is_some() {
            loop {
                match status_cmd_rx.try_recv() {
                    Ok(text) => {
                        app.status_line_override = if text.is_empty() { None } else { Some(text) };
                    }
                    Err(_) => break,
                }
            }
        }

        // Check if query task is done; sync messages from the task
        let task_finished = current_query
            .as_ref()
            .map(|(h, _)| h.is_finished())
            .unwrap_or(false);

        if task_finished {
            if let Some((handle, msgs_arc)) = current_query.take() {
                // Get the outcome (ignore errors for now)
                let _ = handle.await;
                // Sync the updated conversation back to our local vector
                messages = msgs_arc.lock().await.clone();
                session.messages = messages.clone();
                session.updated_at = chrono::Utc::now();
                session.model = base_query_config.model.clone();
                session.working_dir = Some(tool_ctx.working_dir.display().to_string());

                // Persist session to JSONL (enables /resume)
                {
                    let tp = cc_core::session_storage::transcript_path(
                        &tool_ctx.working_dir,
                        &session.id,
                    );
                    let cwd_str = tool_ctx.working_dir.display().to_string();
                    // Write last user + assistant messages
                    for msg in messages.iter().rev().take(2) {
                        let entry_type = match msg.role {
                            cc_core::types::Role::User => {
                                cc_core::session_storage::TranscriptEntry::User
                            }
                            cc_core::types::Role::Assistant => {
                                cc_core::session_storage::TranscriptEntry::Assistant
                            }
                        };
                        let entry = entry_type(cc_core::session_storage::TranscriptMessage {
                            uuid: Some(uuid::Uuid::new_v4().to_string()),
                            parent_uuid: None,
                            timestamp: chrono::Utc::now().to_rfc3339(),
                            session_id: session.id.clone(),
                            cwd: cwd_str.clone(),
                            message: msg.clone(),
                            is_sidechain: false,
                            user_type: "external".to_string(),
                            version: cc_core::constants::APP_VERSION.to_string(),
                            git_branch: None,
                            extra: Default::default(),
                        });
                        let _ = cc_core::session_storage::write_transcript_entry(&tp, &entry).await;
                    }
                }

                app.is_streaming = false;
                app.status_message = None;
                // Sync TUI messages with the authoritative list from the query task
                // to prevent stale/duplicate messages from showing.
                app.messages = messages.clone();
                app.streaming_text.clear();
                app.tool_use_blocks.clear();
                app.invalidate_transcript();

                // Save session
                let _ = cc_core::history::save_session(&session).await;
            }
        }

        if !app.is_streaming && current_query.is_none() && app.take_pending_mcp_reconnect() {
            let new_mcp_manager = connect_mcp_manager_arc(&cmd_ctx.config).await;
            tool_ctx.mcp_manager = new_mcp_manager.clone();
            app.mcp_manager = new_mcp_manager.clone();
            tools_arc = build_tools_with_mcp(new_mcp_manager.clone());
            if app.mcp_view.open {
                app.refresh_mcp_view();
            }

            let connected = new_mcp_manager
                .as_ref()
                .map(|manager| manager.server_count())
                .unwrap_or(0);
            app.status_message = Some(if cmd_ctx.config.mcp_servers.is_empty() {
                "No MCP servers configured.".to_string()
            } else {
                format!(
                    "Reconnected MCP runtime ({} connected server{}).",
                    connected,
                    if connected == 1 { "" } else { "s" }
                )
            });
        }

        if app.should_quit {
            break 'main;
        }
    }

    if let Some(runtime) = bridge_runtime.take() {
        runtime.cancel.cancel();
    }
    restore_terminal(&mut terminal)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// `claude auth` subcommand handler
// ---------------------------------------------------------------------------
// Mirrors TypeScript cli.tsx `if (args[0] === 'auth') { ... }` fast-path.
// Called before Cli::parse() so it doesn't conflict with positional `prompt`.
//
// Usage:
//   claude auth login [--console]   — OAuth PKCE login (uppli.dev by default)
//   claude auth logout              — Clear stored credentials
//   claude auth status [--json]     — Show authentication status

async fn handle_auth_command(args: &[String]) -> anyhow::Result<()> {
    match args.first().map(|s| s.as_str()) {
        Some("login") => {
            // --console flag selects the Console OAuth flow (creates an API key)
            // Default (no flag) uses the Claude.ai flow (Bearer token)
            let login_with_claude_ai = !args.iter().any(|a| a == "--console");
            println!("Starting authentication...");
            match oauth_flow::run_oauth_login_flow(login_with_claude_ai).await {
                Ok(result) => {
                    println!("Successfully logged in!");
                    if let Some(email) = &result.tokens.email {
                        println!("  Account: {}", email);
                    }
                    if result.use_bearer_auth {
                        println!("  Auth method: uppli.dev");
                    } else {
                        println!("  Auth method: console (API key)");
                    }
                    std::process::exit(0);
                }
                Err(e) => {
                    eprintln!("Login failed: {}", e);
                    std::process::exit(1);
                }
            }
        }

        Some("logout") => {
            auth_logout().await;
        }

        Some("status") => {
            let json_output = args.iter().any(|a| a == "--json");
            auth_status(json_output).await;
        }

        Some(unknown) => {
            eprintln!("Unknown auth subcommand: '{}'", unknown);
            eprintln!();
            eprintln!("Usage: claude auth <subcommand>");
            eprintln!(
                "  login [--console]   Authenticate (uppli.dev by default; --console for API key)"
            );
            eprintln!("  logout              Remove stored credentials");
            eprintln!("  status [--json]     Show authentication status");
            std::process::exit(1);
        }

        None => {
            eprintln!("Usage: claude auth <login|logout|status>");
            eprintln!("  login [--console]   Authenticate");
            eprintln!("  logout              Remove stored credentials");
            eprintln!("  status [--json]     Show authentication status");
            std::process::exit(1);
        }
    }

    Ok(())
}

/// Print current auth status, then exit with code 0 (logged in) or 1 (not logged in).
async fn auth_status(json_output: bool) {
    // Gather auth state
    let env_api_key = std::env::var("ANTHROPIC_API_KEY")
        .ok()
        .filter(|k| !k.is_empty());
    let settings = Settings::load().await.unwrap_or_default();
    let settings_api_key = settings.config.api_key.clone().filter(|k| !k.is_empty());
    let oauth_tokens = cc_core::oauth::OAuthTokens::load().await;
    let api_provider = "Uppli";
    let api_key_source = if env_api_key.is_some() {
        Some("ANTHROPIC_API_KEY".to_string())
    } else if settings_api_key.is_some() {
        Some("settings".to_string())
    } else if oauth_tokens
        .as_ref()
        .is_some_and(|tokens| !tokens.uses_bearer_auth() && tokens.api_key.is_some())
    {
        Some("/login managed key".to_string())
    } else {
        None
    };
    let token_source = oauth_tokens.as_ref().map(|tokens| {
        if tokens.uses_bearer_auth() {
            "uppli.dev".to_string()
        } else {
            "console_oauth".to_string()
        }
    });
    let login_method = oauth_tokens
        .as_ref()
        .and_then(|tokens| subscription_label(tokens.subscription_type.as_deref()))
        .or_else(|| {
            oauth_tokens.as_ref().map(|tokens| {
                if tokens.uses_bearer_auth() {
                    "Cloud Account".to_string()
                } else {
                    "Console Account".to_string()
                }
            })
        })
        .or_else(|| api_key_source.as_ref().map(|_| "API Key".to_string()));
    let billing_mode = oauth_tokens.as_ref().map_or_else(
        || {
            if api_key_source.is_some() {
                "API".to_string()
            } else {
                "None".to_string()
            }
        },
        |tokens| {
            if tokens.uses_bearer_auth() {
                "Subscription".to_string()
            } else {
                "API".to_string()
            }
        },
    );

    // Determine auth method (mirrors TypeScript authStatus())
    let (auth_method, logged_in) = if let Some(ref tokens) = oauth_tokens {
        let uses_bearer = tokens.uses_bearer_auth();
        let method = if uses_bearer {
            "uppli.dev"
        } else {
            "oauth_token"
        };
        (method.to_string(), true)
    } else if env_api_key.is_some() {
        ("api_key".to_string(), true)
    } else if settings_api_key.is_some() {
        ("api_key".to_string(), true)
    } else {
        ("none".to_string(), false)
    };

    if json_output {
        // JSON output (used by SDK + scripts)
        let mut obj = serde_json::json!({
            "loggedIn": logged_in,
            "authMethod": auth_method,
            "apiProvider": api_provider,
            "billing": billing_mode,
        });

        // Include API key source if known
        if let Some(ref source) = api_key_source {
            obj["apiKeySource"] = serde_json::Value::String(source.clone());
        }
        if let Some(ref source) = token_source {
            obj["tokenSource"] = serde_json::Value::String(source.clone());
        }
        if let Some(ref method) = login_method {
            obj["loginMethod"] = serde_json::Value::String(method.clone());
        }

        if let Some(ref tokens) = oauth_tokens {
            obj["email"] = json_null_or_string(&tokens.email);
            obj["orgId"] = json_null_or_string(&tokens.organization_uuid);
            obj["subscriptionType"] = json_null_or_string(&tokens.subscription_type);
        }

        println!("{}", serde_json::to_string_pretty(&obj).unwrap_or_default());
    } else {
        // Human-readable text output
        if !logged_in {
            println!("Not logged in. Run `claude auth login` to authenticate.");
        } else {
            println!("Logged in.");
            println!("  API provider: {}", api_provider);
            println!("  Billing: {}", billing_mode);
            if let Some(ref method) = login_method {
                println!("  Login method: {}", method);
            }
            if let Some(ref source) = token_source {
                println!("  Auth token: {}", source);
            }
            if let Some(ref source) = api_key_source {
                println!("  API key: {}", source);
            }
            match auth_method.as_str() {
                "uppli.dev" | "oauth_token" => {
                    if let Some(ref tokens) = oauth_tokens {
                        if let Some(ref email) = tokens.email {
                            println!("  Email: {}", email);
                        }
                        if let Some(ref org) = tokens.organization_uuid {
                            println!("  Organization ID: {}", org);
                        } else {
                            println!("  Organization ID: unavailable");
                        }
                        if let Some(ref sub) = tokens.subscription_type {
                            println!("  Subscription: {}", sub);
                        }
                    }
                }
                "api_key" => {
                    println!("  Organization ID: unavailable for direct API key auth");
                }
                _ => {}
            }
        }
    }

    std::process::exit(if logged_in { 0 } else { 1 });
}

/// Clear all stored credentials and exit.
async fn auth_logout() {
    let mut had_error = false;

    // Clear OAuth tokens
    if let Err(e) = cc_core::oauth::OAuthTokens::clear().await {
        eprintln!("Warning: failed to clear OAuth tokens: {}", e);
        had_error = true;
    }

    // Also clear any API key stored in settings.json
    match Settings::load().await {
        Ok(mut settings) => {
            if settings.config.api_key.is_some() {
                settings.config.api_key = None;
                if let Err(e) = settings.save().await {
                    eprintln!("Warning: failed to update settings.json: {}", e);
                    had_error = true;
                }
            }
        }
        Err(e) => {
            eprintln!("Warning: failed to load settings.json: {}", e);
        }
    }

    if had_error {
        eprintln!("Logout completed with warnings.");
        std::process::exit(1);
    } else {
        println!("Successfully logged out.");
        std::process::exit(0);
    }
}

/// Helper: convert `Option<String>` to a JSON string or null.
fn subscription_label(subscription_type: Option<&str>) -> Option<String> {
    match subscription_type? {
        "enterprise" => Some("Enterprise Account".to_string()),
        "team" => Some("Team Account".to_string()),
        "max" => Some("Max Account".to_string()),
        "pro" => Some("Pro Account".to_string()),
        other if !other.is_empty() => Some(format!("{} Account", other)),
        _ => None,
    }
}

/// Helper: convert `Option<String>` to a JSON string or null.
fn json_null_or_string(opt: &Option<String>) -> serde_json::Value {
    match opt {
        Some(s) => serde_json::Value::String(s.clone()),
        None => serde_json::Value::Null,
    }
}
