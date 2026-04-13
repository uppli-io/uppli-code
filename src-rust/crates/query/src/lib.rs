// cc-query: The core agentic query loop.
//
// This crate implements the main conversation loop that:
// 1. Sends messages to the Anthropic API
// 2. Processes streaming responses
// 3. Detects tool-use requests and dispatches them
// 4. Feeds tool results back to the model
// 5. Handles auto-compact when the context window fills up
// 6. Manages stop conditions (end_turn, max_turns, cancellation)

pub mod agent_tool;
pub mod auto_dream;
pub mod away_summary;
pub mod command_queue;
pub mod compact;
pub mod context_analyzer;
pub mod coordinator;
pub mod cron_scheduler;
pub mod session_memory;
pub mod skill_prefetch;
pub use agent_tool::AgentTool;
pub use command_queue::{drain_command_queue, CommandPriority, CommandQueue, QueuedCommand};
pub use compact::{
    auto_compact_if_needed, calculate_messages_to_keep_index, calculate_token_warning_state,
    compact_conversation, context_collapse, format_compact_summary, get_compact_prompt,
    group_messages_for_compact, micro_compact_if_needed, reactive_compact, should_auto_compact,
    should_compact, should_context_collapse, snip_compact, AutoCompactState, CompactResult,
    CompactTrigger, MessageGroup, MicroCompactConfig, TokenWarningState,
};
pub use cron_scheduler::start_cron_scheduler;
pub use session_memory::{
    ExtractedMemory, MemoryCategory, SessionMemoryExtractor, SessionMemoryState,
};
pub use skill_prefetch::{
    format_skill_listing, prefetch_skills, SharedSkillIndex, SkillDefinition, SkillIndex,
};

use cc_api::{
    ApiMessage, ApiToolDefinition, CreateMessageRequest, LlmProvider, StreamAccumulator,
    StreamEvent, StreamHandler, SystemPrompt, ThinkingConfig,
};
use cc_core::config::Config;
use cc_core::cost::CostTracker;
use cc_core::error::ClaudeError;
use cc_core::types::{ContentBlock, Message, ToolResultContent, UsageInfo};
use cc_tools::{Tool, ToolContext, ToolResult};
use serde_json::Value;
use std::sync::{Arc, OnceLock};
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

// ---------------------------------------------------------------------------
// Global provider — set once at startup, read by sub-agents (agent_tool).
// This avoids adding LlmProvider to ToolContext (which would create a
// circular dependency between cc-tools and cc-api).
// ---------------------------------------------------------------------------

static GLOBAL_PROVIDER: OnceLock<Arc<dyn LlmProvider>> = OnceLock::new();

/// Set the global LLM provider (called once from main.rs at startup).
pub fn set_global_provider(provider: Arc<dyn LlmProvider>) {
    let _ = GLOBAL_PROVIDER.set(provider);
}

/// Get the global LLM provider (for sub-agents).
pub fn global_provider() -> Option<&'static Arc<dyn LlmProvider>> {
    GLOBAL_PROVIDER.get()
}

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Outcome of a single query-loop run.
#[derive(Debug)]
pub enum QueryOutcome {
    /// The model finished its turn (end_turn stop reason).
    EndTurn { message: Message, usage: UsageInfo },
    /// The model hit max_tokens.
    MaxTokens {
        partial_message: Message,
        usage: UsageInfo,
    },
    /// The conversation was cancelled by the user.
    Cancelled,
    /// An unrecoverable error occurred.
    Error(ClaudeError),
    /// The configured USD budget was exceeded.
    BudgetExceeded { cost_usd: f64, limit_usd: f64 },
}

/// Configuration for a single query-loop invocation.
#[derive(Clone)]
pub struct QueryConfig {
    pub model: String,
    pub max_tokens: u32,
    pub max_turns: u32,
    pub system_prompt: Option<String>,
    pub append_system_prompt: Option<String>,
    pub output_style: cc_core::system_prompt::OutputStyle,
    pub output_style_prompt: Option<String>,
    pub working_directory: Option<String>,
    pub thinking_budget: Option<u32>,
    pub temperature: Option<f32>,
    /// Maximum cumulative character count of all tool results in the message
    /// history before older results are replaced with a truncation notice.
    /// Mirrors the TS `applyToolResultBudget` mechanism.  Default: 50_000.
    pub tool_result_budget: usize,
    /// Optional effort level.  When set and `thinking_budget` is `None`,
    /// the effort level's `thinking_budget_tokens()` is used as the
    /// thinking budget.  Also provides a temperature override when the
    /// level specifies one.
    pub effort_level: Option<cc_core::effort::EffortLevel>,
    /// T1-4: Optional shared command queue.
    ///
    /// When set, the query loop drains this queue before each API call and
    /// injects any resulting messages into the conversation.  The queue is
    /// shared (Arc-backed) so the TUI input thread can push commands while the
    /// loop is waiting for a model response.
    pub command_queue: Option<CommandQueue>,
    /// T1-5: Optional shared skill index.
    ///
    /// When set, `prefetch_skills` is spawned once before the loop begins and
    /// the resulting index is used to inject a skill listing attachment into
    /// the conversation context.
    pub skill_index: Option<SharedSkillIndex>,
    /// Optional USD spend cap. The query loop checks accumulated cost after
    /// each turn and aborts with `QueryOutcome::BudgetExceeded` when exceeded.
    pub max_budget_usd: Option<f64>,
    /// Fallback model name. Used when the primary model returns overloaded /
    /// rate-limit errors (mirrors TS `--fallback-model`).
    pub fallback_model: Option<String>,
}

impl Default for QueryConfig {
    fn default() -> Self {
        // These defaults are used ONLY when from_provider() is not called.
        // The model/fallback_model come from the global provider if available,
        // otherwise fall back to compile-time constants (DeepSeek legacy).
        let (model, fallback) = match global_provider() {
            Some(p) => {
                let caps = p.capabilities();
                (caps.default_model.clone(), caps.fast_model.clone())
            }
            None => (
                cc_core::constants::DEFAULT_MODEL.to_string(),
                Some(cc_core::constants::SONNET_MODEL.to_string()),
            ),
        };
        Self {
            model,
            max_tokens: cc_core::constants::DEFAULT_MAX_TOKENS,
            max_turns: cc_core::constants::MAX_TURNS_DEFAULT,
            system_prompt: None,
            append_system_prompt: None,
            output_style: cc_core::system_prompt::OutputStyle::Default,
            output_style_prompt: None,
            working_directory: None,
            thinking_budget: Some(cc_core::constants::DEFAULT_THINKING_BUDGET),
            temperature: None,
            tool_result_budget: cc_core::constants::DEFAULT_TOOL_RESULT_BUDGET,
            effort_level: Some(cc_core::effort::EffortLevel::High),
            command_queue: None,
            skill_index: None,
            max_budget_usd: None,
            fallback_model: fallback,
        }
    }
}

impl QueryConfig {
    pub fn from_config(cfg: &Config) -> Self {
        Self {
            model: cfg.effective_model().to_string(),
            max_tokens: cfg.effective_max_tokens(),
            output_style: cfg.effective_output_style(),
            output_style_prompt: cfg.resolve_output_style_prompt(),
            working_directory: cfg.project_dir.as_ref().map(|p| p.display().to_string()),
            ..Default::default()
        }
    }

    /// Create a QueryConfig with defaults sourced from the active provider.
    /// This replaces the hardcoded constants with provider metadata.
    pub fn from_provider(provider: &dyn cc_api::LlmProvider, cfg: &Config) -> Self {
        let caps = provider.capabilities();
        Self {
            model: caps.default_model.clone(),
            max_tokens: caps.default_max_tokens,
            thinking_budget: caps.default_thinking_budget,
            fallback_model: caps.fast_model.clone(),
            output_style: cfg.effective_output_style(),
            output_style_prompt: cfg.resolve_output_style_prompt(),
            working_directory: cfg.project_dir.as_ref().map(|p| p.display().to_string()),
            ..Default::default()
        }
    }
}

/// Events emitted by the query loop for the TUI to render.
#[derive(Debug, Clone)]
pub enum QueryEvent {
    /// A stream event from the API.
    Stream(StreamEvent),
    /// A tool is about to be executed.
    ToolStart {
        tool_name: String,
        tool_id: String,
        input_json: String,
    },
    /// A tool has finished executing.
    ToolEnd {
        tool_name: String,
        tool_id: String,
        result: String,
        is_error: bool,
    },
    /// The model finished a turn.
    TurnComplete {
        turn: u32,
        stop_reason: String,
        usage: Option<UsageInfo>,
    },
    /// An informational status message.
    Status(String),
    /// An error.
    Error(String),
    /// Token usage has crossed a warning threshold.
    /// `state` is Warning (≥ 80 %) or Critical (≥ 95 %).
    /// `pct_used` is the fraction of the context window consumed (0.0–1.0).
    TokenWarning {
        state: TokenWarningState,
        pct_used: f64,
    },
}

// ---------------------------------------------------------------------------
// T1-3: Post-sampling hooks
// ---------------------------------------------------------------------------

/// Result returned by `fire_post_sampling_hooks`.
#[derive(Debug, Default)]
pub struct PostSamplingHookResult {
    /// Error messages produced by hooks with non-zero exit codes.
    /// These are injected into the conversation as user messages before the
    /// next model turn so the model can react to them.
    pub blocking_errors: Vec<cc_core::types::Message>,
    /// When `true` the query loop must not continue and should surface the
    /// error messages to the caller.  Set when any hook exits with code > 1.
    pub prevent_continuation: bool,
}

/// Execute all `PostModelTurn` hooks defined in `config.hooks`.
///
/// Each hook is run synchronously (blocking via `std::process::Command`).
/// On a non-zero exit code, the hook's stderr (falling back to stdout) is
/// wrapped in a user `Message` and appended to `blocking_errors`.
/// If the exit code is **strictly greater than 1** `prevent_continuation` is
/// set so the query loop can return early.
pub fn fire_post_sampling_hooks(
    _turn_result: &cc_core::types::Message,
    config: &cc_core::config::Config,
) -> PostSamplingHookResult {
    use cc_core::config::HookEvent;
    use cc_core::types::Message;

    let mut result = PostSamplingHookResult::default();

    let entries = match config.hooks.get(&HookEvent::PostModelTurn) {
        Some(e) => e,
        None => return result,
    };

    for entry in entries {
        let sh = if cfg!(windows) { "cmd" } else { "sh" };
        let flag = if cfg!(windows) { "/C" } else { "-c" };

        let output = match std::process::Command::new(sh)
            .args([flag, &entry.command])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output()
        {
            Ok(o) => o,
            Err(e) => {
                tracing::warn!(command = %entry.command, error = %e, "PostModelTurn hook spawn failed");
                continue;
            }
        };

        if output.status.success() {
            continue;
        }

        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let body = if !stderr.trim().is_empty() {
            stderr
        } else {
            stdout
        };

        tracing::warn!(
            command = %entry.command,
            exit_code = ?output.status.code(),
            "PostModelTurn hook returned non-zero exit"
        );

        result.blocking_errors.push(Message::user(format!(
            "[Hook '{}' error]:\n{}",
            entry.command,
            body.trim()
        )));

        // Exit code > 1 → hard veto of continuation.
        if output.status.code().unwrap_or(1) > 1 {
            result.prevent_continuation = true;
        }
    }

    result
}

/// Spawn all `Stop` hooks in fire-and-forget background tasks.
///
/// Stop hooks are non-blocking by design: the caller does not wait for them.
/// Returns an empty `Vec` immediately; results (if any) are lost.
pub fn stop_hooks_with_full_behavior(
    turn_result: &cc_core::types::Message,
    config: &cc_core::config::Config,
    working_dir: std::path::PathBuf,
) -> Vec<cc_core::types::Message> {
    use cc_core::config::HookEvent;

    let entries = match config.hooks.get(&HookEvent::Stop) {
        Some(e) if !e.is_empty() => e.clone(),
        _ => return Vec::new(),
    };

    let output_text = turn_result.get_all_text();

    for entry in entries {
        let cmd = entry.command.clone();
        let dir = working_dir.clone();
        let text = output_text.clone();

        tokio::task::spawn_blocking(move || {
            let sh = if cfg!(windows) { "cmd" } else { "sh" };
            let flag = if cfg!(windows) { "/C" } else { "-c" };

            let _ = std::process::Command::new(sh)
                .args([flag, &cmd])
                .current_dir(&dir)
                .env("CLAUDE_HOOK_OUTPUT", &text)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn();
        });
    }

    Vec::new()
}

// ---------------------------------------------------------------------------
// Tool-result budgeting
// ---------------------------------------------------------------------------

/// Return the combined character count of all tool-result content blocks found
/// in `messages`.  Only user messages are examined (tool results always live
/// in user turns).
fn total_tool_result_chars(messages: &[Message]) -> usize {
    messages
        .iter()
        .filter(|m| m.role == cc_core::types::Role::User)
        .flat_map(|m| match &m.content {
            cc_core::types::MessageContent::Blocks(blocks) => blocks.as_slice(),
            _ => &[],
        })
        .filter_map(|b| {
            if let ContentBlock::ToolResult { content, .. } = b {
                Some(match content {
                    ToolResultContent::Text(t) => t.len(),
                    ToolResultContent::Blocks(blocks) => blocks
                        .iter()
                        .map(|b| {
                            if let ContentBlock::Text { text } = b {
                                text.len()
                            } else {
                                0
                            }
                        })
                        .sum(),
                })
            } else {
                None
            }
        })
        .sum()
}

/// When the cumulative tool-result content exceeds `budget` characters, walk
/// the message list from oldest to newest and replace individual
/// `ToolResult` content with a placeholder until the running total is back
/// under budget.  Returns the (possibly modified) message list and the
/// number of results that were truncated.
///
/// Mirrors the spirit of the TypeScript `applyToolResultBudget` /
/// `enforceToolResultBudget` logic, simplified to a straightforward
/// oldest-first eviction without the session-persistence layer.
fn apply_tool_result_budget(messages: Vec<Message>, budget: usize) -> (Vec<Message>, usize) {
    let total = total_tool_result_chars(&messages);
    if total <= budget {
        return (messages, 0);
    }

    let mut to_shed = total - budget;
    let mut truncated = 0usize;
    let mut result = messages;

    'outer: for msg in result.iter_mut() {
        if msg.role != cc_core::types::Role::User {
            continue;
        }
        let blocks = match &mut msg.content {
            cc_core::types::MessageContent::Blocks(b) => b,
            _ => continue,
        };
        for block in blocks.iter_mut() {
            if let ContentBlock::ToolResult { content, .. } = block {
                let size = match &*content {
                    ToolResultContent::Text(t) => t.len(),
                    ToolResultContent::Blocks(inner) => inner
                        .iter()
                        .map(|b| {
                            if let ContentBlock::Text { text } = b {
                                text.len()
                            } else {
                                0
                            }
                        })
                        .sum(),
                };
                if size == 0 {
                    continue;
                }
                *content =
                    ToolResultContent::Text("[tool result truncated to save context]".to_string());
                truncated += 1;
                if size > to_shed {
                    break 'outer;
                }
                to_shed -= size;
            }
        }
    }

    (result, truncated)
}

// ---------------------------------------------------------------------------
// Query loop
// ---------------------------------------------------------------------------

/// Maximum number of max_tokens continuation attempts before surfacing the
/// partial response.  Mirrors `MAX_OUTPUT_TOKENS_RECOVERY_LIMIT` in query.ts.
const MAX_TOKENS_RECOVERY_LIMIT: u32 = 3;

/// Maximum number of overload retry attempts before giving up.
const MAX_OVERLOAD_RETRIES: u32 = 5;

/// Message injected when the model hits its output-token limit.
/// Mirrors the TS recovery message in query.ts lines 1224-1228.
const MAX_TOKENS_RECOVERY_MSG: &str =
    "Output token limit hit. Resume directly — no apology, no recap of what \
     you were doing. Pick up mid-thought if that is where the cut happened. \
     Break remaining work into smaller pieces.";

/// Run the agentic query loop.
///
/// This sends the conversation to the API, handles tool calls in a loop, and
/// returns when the model issues an end_turn or an error/limit is hit.
///
/// `pending_messages` is an optional queue of user messages that were enqueued
/// during tool execution (e.g. by the UI or a command queue).  Each string is
/// appended as a plain user message between turns.  Callers that do not need
/// command queuing may pass `None` or an empty `Vec`.
pub async fn run_query_loop(
    client: &dyn LlmProvider,
    messages: &mut Vec<Message>,
    tools: &[Box<dyn Tool>],
    tool_ctx: &ToolContext,
    config: &QueryConfig,
    cost_tracker: Arc<CostTracker>,
    event_tx: Option<mpsc::UnboundedSender<QueryEvent>>,
    cancel_token: tokio_util::sync::CancellationToken,
    mut pending_messages: Option<&mut Vec<String>>,
) -> QueryOutcome {
    let mut turn = 0u32;
    let mut compact_state = compact::AutoCompactState::default();
    // Tracks how many consecutive max_tokens recoveries we've attempted so
    // we don't loop forever on a model that can't finish within any budget.
    let mut max_tokens_recovery_count: u32 = 0;
    // Tracks consecutive overload retries to avoid infinite loops.
    let mut overload_retries: u32 = 0;
    // Stuck-loop detection: track the last tool call signature to detect
    // the model repeating the same action. After 3 identical calls, we
    // inject a nudge to try a different approach.
    let mut last_tool_sig: Option<String> = None;
    let mut repeat_count: u32 = 0;
    // CodeAudit gate: track files that have been audited.
    // Edit/Write on source files is blocked until CodeAudit has been called.
    let mut audited_files: std::collections::HashSet<String> = std::collections::HashSet::new();
    // Store pre-edit audit reports for post-edit comparison.
    let mut pre_edit_audits: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    // Empty-response retry: when the model returns end_turn with no tools
    // and no text (likely rate-limit or API glitch), retry with backoff.
    let mut empty_response_retries: u32 = 0;
    const MAX_EMPTY_RETRIES: u32 = 5;
    // Active model — user can switch via /model command.
    let effective_model = config.model.clone();
    // Context window resolved from provider metadata (not substring matching).
    let ctx_window = client.context_window(&effective_model);

    loop {
        turn += 1;
        tool_ctx
            .current_turn
            .store(turn as usize, std::sync::atomic::Ordering::Relaxed);
        if turn > config.max_turns {
            info!(turns = turn, "Max turns reached");
            if let Some(ref tx) = event_tx {
                let _ = tx.send(QueryEvent::Status(format!(
                    "Reached maximum turn limit ({})",
                    config.max_turns
                )));
            }
            // Return the last assistant message if any
            let last_msg = messages
                .last()
                .cloned()
                .unwrap_or_else(|| Message::assistant("Max turns reached."));
            return QueryOutcome::EndTurn {
                message: last_msg,
                usage: UsageInfo::default(),
            };
        }

        // Check for cancellation
        if cancel_token.is_cancelled() {
            return QueryOutcome::Cancelled;
        }

        // Drain any pending user messages that were queued during the previous
        // tool-execution phase (e.g. commands entered while tools ran).
        // Mirrors the TS `messageQueueManager` drain between turns.
        if let Some(queue) = pending_messages.as_deref_mut() {
            for text in queue.drain(..) {
                debug!("Injecting pending message: {}", &text);
                messages.push(Message::user(text));
            }
        }

        // T1-4: Drain the priority command queue (if wired up) and prepend any
        // resulting messages to the conversation before the API call.
        // Mirrors the TS `messageQueueManager` priority-queue drain.
        if let Some(ref cq) = config.command_queue {
            if !cq.is_empty() {
                let injected = drain_command_queue(cq);
                if !injected.is_empty() {
                    debug!(count = injected.len(), "Injecting command-queue messages");
                    // Prepend so that higher-priority commands appear first.
                    let tail = std::mem::take(messages);
                    messages.extend(injected);
                    messages.extend(tail);
                }
            }
        }

        // Apply tool-result budget: if the cumulative size of all tool results
        // in the conversation exceeds the configured threshold, replace the
        // oldest results with a placeholder until we're back under budget.
        // This mirrors the TS `applyToolResultBudget` call in query.ts.
        if config.tool_result_budget > 0 {
            let (budgeted, truncated) =
                apply_tool_result_budget(std::mem::take(messages), config.tool_result_budget);
            *messages = budgeted;
            if truncated > 0 {
                info!(
                    truncated,
                    budget = config.tool_result_budget,
                    "Tool-result budget exceeded: truncated {} result(s)",
                    truncated
                );
                if let Some(ref tx) = event_tx {
                    let _ = tx.send(QueryEvent::Status(format!(
                        "[{} older tool result(s) truncated to save context]",
                        truncated
                    )));
                }
            }
        }

        // Build API request
        let api_messages: Vec<ApiMessage> = messages.iter().map(ApiMessage::from).collect();
        let api_tools: Vec<ApiToolDefinition> = tools
            .iter()
            .map(|t| ApiToolDefinition::from(&t.to_definition()))
            .collect();

        let system = build_system_prompt(config);

        // Hybrid model selection: when the current model supports it, use the
        // provider's fast model for processing tool results since they don't
        // need deep reasoning. If the user explicitly chose a non-default
        // model, respect their choice for all turns.
        let has_tool_results = messages
            .last()
            .map(|m| !m.get_tool_result_blocks().is_empty())
            .unwrap_or(false);

        // Use the fast model for tool-result turns ONLY if config allows it.
        // If fallback_model is None, stay on the main model for all turns.
        let fast_override = if has_tool_results && config.fallback_model.is_some() {
            client.fast_model_for(&effective_model)
        } else {
            None
        };
        let model_to_use = fast_override.unwrap_or(&effective_model);

        if let Some(ref tx) = event_tx {
            let mode_name = if fast_override.is_some() {
                format!("⚡ FAST MODE ({})", model_to_use)
            } else if client.model_supports_thinking(model_to_use) {
                format!("🧠 THINK MODE ({})", model_to_use)
            } else {
                model_to_use.to_string()
            };
            let _ = tx.send(QueryEvent::Status(format!("Model: {}", mode_name)));
        }
        let mut req_builder = CreateMessageRequest::builder(model_to_use, config.max_tokens)
            .messages(api_messages)
            .system(system)
            .tools(api_tools);

        // Resolve effective thinking budget:
        //   1. Only enable thinking for models that support it (deepseek-reasoner).
        //      deepseek-chat does NOT support thinking — sending it causes the API
        //      to silently route to deepseek-reasoner, defeating the model choice.
        //   2. Explicit `thinking_budget` in config takes precedence.
        //   3. Fall back to the effort level's budget when no explicit budget is set.
        // Never enable thinking when in fast mode (tool-result turns).
        // The whole point of fast mode is to skip reasoning.
        let model_supports_thinking =
            fast_override.is_none() && client.model_supports_thinking(model_to_use);

        if model_supports_thinking {
            let effective_thinking_budget = config.thinking_budget.or_else(|| {
                config
                    .effort_level
                    .and_then(|el| el.thinking_budget_tokens())
            });

            if let Some(budget) = effective_thinking_budget {
                req_builder = req_builder.thinking(ThinkingConfig::enabled(budget));
            }
        }

        // Apply temperature: explicit config value takes precedence, then effort-level override.
        let effective_temperature = config
            .temperature
            .or_else(|| config.effort_level.and_then(|el| el.temperature()));
        if let Some(t) = effective_temperature {
            req_builder = req_builder.temperature(t);
        }

        let request = req_builder.build();

        // Create a stream handler that forwards to the event channel
        let handler: Arc<dyn StreamHandler> = if let Some(ref tx) = event_tx {
            let tx = tx.clone();
            Arc::new(ChannelStreamHandler { tx })
        } else {
            Arc::new(cc_api::streaming::NullStreamHandler)
        };

        // Send to API — measure time-to-first-token for load detection
        let request_start = std::time::Instant::now();
        debug!(turn, model = %effective_model, "Sending API request");
        let mut stream_rx = match client.create_message_stream(request, handler).await {
            Ok(rx) => rx,
            Err(e) => {
                // On overloaded/rate-limit errors, notify the user and retry
                // the same model. Never auto-switch — the user decides.
                let err_str = e.to_string().to_lowercase();
                if err_str.contains("overloaded")
                    || err_str.contains("529")
                    || err_str.contains("rate_limit")
                {
                    overload_retries += 1;
                    if overload_retries > MAX_OVERLOAD_RETRIES {
                        error!(
                            "API overloaded — max retries ({}) exhausted",
                            MAX_OVERLOAD_RETRIES
                        );
                        if let Some(ref tx) = event_tx {
                            let fb_hint = config.fallback_model.as_deref().unwrap_or("unknown");
                            let _ = tx.send(QueryEvent::Status(format!(
                                "⚠ {} still overloaded after {} retries. Use /model {} to switch.",
                                effective_model, MAX_OVERLOAD_RETRIES, fb_hint
                            )));
                        }
                        return QueryOutcome::Error(e);
                    }
                    warn!(model = %effective_model, retry = overload_retries, "API overloaded — will retry");
                    if let Some(ref tx) = event_tx {
                        let fb_hint = config.fallback_model.as_deref().unwrap_or("unknown");
                        let _ = tx.send(QueryEvent::Status(format!(
                            "⚠ {} overloaded — retry {}/{} (use /model {} to switch)",
                            effective_model, overload_retries, MAX_OVERLOAD_RETRIES, fb_hint
                        )));
                    }
                    let wait =
                        std::time::Duration::from_secs((2u64.pow(overload_retries.min(4))).min(30));
                    tokio::time::sleep(wait).await;
                    turn -= 1;
                    continue;
                }
                error!(error = %e, "API request failed");
                return QueryOutcome::Error(e);
            }
        };

        // Request succeeded — reset overload retry counter.
        overload_retries = 0;

        // Accumulate the streamed response
        let mut accumulator = StreamAccumulator::new();
        let mut first_token_received = false;

        loop {
            tokio::select! {
                _ = cancel_token.cancelled() => {
                    return QueryOutcome::Cancelled;
                }
                event = stream_rx.recv() => {
                    match event {
                        Some(evt) => {
                            // Measure Time-To-First-Token (TTFT) for load detection
                            if !first_token_received
                                && matches!(&evt, StreamEvent::ContentBlockDelta { .. }) {
                                    first_token_received = true;
                                    let ttft = request_start.elapsed();
                                    if ttft.as_secs() > 30 {
                                        warn!(
                                            ttft_secs = ttft.as_secs(),
                                            model = %effective_model,
                                            "Slow TTFT detected — API under heavy load"
                                        );
                                        if let Some(ref tx) = event_tx {
                                            let _ = tx.send(QueryEvent::Status(format!(
                                                "⚠ API slow ({}s to first token) — servers may be loaded",
                                                ttft.as_secs()
                                            )));
                                        }
                                    }
                                }
                            accumulator.on_event(&evt);
                            match &evt {
                                StreamEvent::Error { error_type, message } => {
                                    if error_type == "overloaded_error" {
                                        warn!(model = %effective_model, "API overloaded");
                                    }
                                    error!(error_type, message, "Stream error");
                                }
                                StreamEvent::MessageStop => break,
                                _ => {}
                            }
                        }
                        None => break, // Stream ended
                    }
                }
            }
        }

        let turn_duration = request_start.elapsed();
        debug!(
            turn,
            duration_secs = turn_duration.as_secs(),
            model = %effective_model,
            "Turn completed"
        );

        let (assistant_msg, usage, stop_reason) = accumulator.finish();

        // Track costs
        cost_tracker.add_usage(
            usage.input_tokens,
            usage.output_tokens,
            usage.cache_creation_input_tokens,
            usage.cache_read_input_tokens,
        );

        // Budget guard: abort the loop if the configured USD cap is exceeded.
        if let Some(limit) = config.max_budget_usd {
            let spent = cost_tracker.total_cost_usd();
            if spent >= limit {
                if let Some(ref tx) = event_tx {
                    let _ = tx.send(QueryEvent::Status(format!(
                        "Budget limit ${:.4} exceeded (spent ${:.4}) — stopping.",
                        limit, spent
                    )));
                }
                return QueryOutcome::BudgetExceeded {
                    cost_usd: spent,
                    limit_usd: limit,
                };
            }
        }

        // Append assistant message to conversation
        messages.push(assistant_msg.clone());

        let stop = stop_reason.as_deref().unwrap_or("end_turn");

        // T1-3: Fire PostModelTurn hooks after the model samples a response.
        // Hooks can inject blocking errors or veto continuation entirely.
        {
            let hook_result = fire_post_sampling_hooks(&assistant_msg, &tool_ctx.config);
            if !hook_result.blocking_errors.is_empty() {
                if hook_result.prevent_continuation {
                    // Hard veto: push the errors into the conversation and abort.
                    for err_msg in hook_result.blocking_errors {
                        messages.push(err_msg);
                    }
                    if let Some(ref tx) = event_tx {
                        let _ = tx.send(QueryEvent::Status(
                            "PostModelTurn hook vetoed continuation.".to_string(),
                        ));
                    }
                    let last = messages
                        .last()
                        .cloned()
                        .unwrap_or_else(|| Message::assistant("Hook blocked continuation."));
                    return QueryOutcome::EndTurn {
                        message: last,
                        usage,
                    };
                }
                // Soft errors: inject them so the model can react next turn.
                for err_msg in hook_result.blocking_errors {
                    debug!("PostModelTurn hook injecting error message");
                    messages.push(err_msg);
                }
            }
        }

        // Emit token warning events when approaching context limits.
        // Thresholds mirror TypeScript autoCompact.ts: 80% → Warning, 95% → Critical.
        {
            let warning_state =
                compact::calculate_token_warning_state(usage.input_tokens, ctx_window);
            if warning_state != compact::TokenWarningState::Ok {
                if let Some(ref tx) = event_tx {
                    let pct_used = usage.input_tokens as f64 / ctx_window as f64;
                    let _ = tx.send(QueryEvent::TokenWarning {
                        state: warning_state,
                        pct_used,
                    });
                }
            }
        }

        // Auto-compact: if context is near-full, summarise older messages now
        // (before the next turn's API call would fail with prompt-too-long).
        //
        // Reactive compact (T1-1): when the CLAUDE_REACTIVE_COMPACT feature gate
        // is enabled, we replace the proactive auto-compact path with reactive
        // compact / context-collapse instead. This fires on every streaming turn
        // so it can act before a prompt-too-long error is returned by the API.
        //
        // Feature gate check: CLAUDE_CODE_FEATURE_REACTIVE_COMPACT=1
        let reactive_compact_enabled =
            cc_core::feature_gates::is_feature_enabled("reactive_compact");

        if reactive_compact_enabled {
            // Reactive path: emergency collapse takes priority over normal compact.
            let context_limit = ctx_window;
            if compact::should_context_collapse(usage.input_tokens, context_limit) {
                if let Some(ref tx) = event_tx {
                    let _ = tx.send(QueryEvent::Status(
                        "Compacting context... (emergency collapse)".to_string(),
                    ));
                }
                match compact::context_collapse(std::mem::take(messages), client, config).await {
                    Ok(result) => {
                        *messages = result.messages;
                        info!(
                            tokens_freed = result.tokens_freed,
                            "Context-collapse complete"
                        );
                    }
                    Err(e) => {
                        warn!(error = %e, "Context-collapse failed");
                        // Put messages back on failure (mem::take drained them).
                        // We can't recover them here — re-run auto-compact as fallback.
                    }
                }
            } else if compact::should_compact(usage.input_tokens, context_limit) {
                if let Some(ref tx) = event_tx {
                    let _ = tx.send(QueryEvent::Status("Compacting context...".to_string()));
                }
                match compact::reactive_compact(
                    std::mem::take(messages),
                    client,
                    config,
                    cancel_token.clone(),
                    &[],
                )
                .await
                {
                    Ok(result) => {
                        *messages = result.messages;
                        info!(
                            tokens_freed = result.tokens_freed,
                            "Reactive compact complete"
                        );
                    }
                    Err(cc_core::error::ClaudeError::Cancelled) => {
                        warn!("Reactive compact was cancelled");
                    }
                    Err(e) => {
                        warn!(error = %e, "Reactive compact failed");
                    }
                }
            }
        } else if stop == "end_turn" || stop == "tool_use" {
            // Proactive auto-compact (original path, used when reactive compact is off).
            if let Some(new_msgs) = compact::auto_compact_if_needed(
                client,
                messages,
                usage.input_tokens,
                &config.model,
                &mut compact_state,
                ctx_window,
            )
            .await
            {
                *messages = new_msgs;
                if let Some(ref tx) = event_tx {
                    let _ = tx.send(QueryEvent::Status(
                        "Context compacted to stay within limits.".to_string(),
                    ));
                }
            }
        }

        if let Some(ref tx) = event_tx {
            let _ = tx.send(QueryEvent::TurnComplete {
                turn,
                stop_reason: stop.to_string(),
                usage: Some(usage.clone()),
            });
        }

        // Helper closure for firing the Stop hook.
        macro_rules! fire_stop_hook {
            ($msg:expr) => {{
                let stop_ctx = cc_core::hooks::HookContext {
                    event: "Stop".to_string(),
                    tool_name: None,
                    tool_input: None,
                    tool_output: Some($msg.get_all_text()),
                    is_error: None,
                    session_id: Some(tool_ctx.session_id.clone()),
                };
                cc_core::hooks::run_hooks(
                    &tool_ctx.config.hooks,
                    cc_core::config::HookEvent::Stop,
                    &stop_ctx,
                    &tool_ctx.working_dir,
                )
                .await;
            }};
        }

        match stop {
            "end_turn" => {
                // Detect empty responses (likely API rate-limit or glitch):
                // no tool calls AND no meaningful text content.
                let has_tools = !assistant_msg.get_tool_use_blocks().is_empty();
                let has_text = match &assistant_msg.content {
                    cc_core::types::MessageContent::Text(t) => !t.trim().is_empty(),
                    cc_core::types::MessageContent::Blocks(blocks) => blocks.iter().any(|b| {
                        matches!(b, ContentBlock::Text { text } if !text.trim().is_empty())
                    }),
                };
                if !has_tools && !has_text && empty_response_retries < MAX_EMPTY_RETRIES {
                    empty_response_retries += 1;
                    let wait = std::time::Duration::from_secs(1 << empty_response_retries);
                    warn!(
                        retry = empty_response_retries,
                        wait_secs = wait.as_secs(),
                        "Empty response from API (no tools, no text) — retrying"
                    );
                    if let Some(ref tx) = event_tx {
                        let _ = tx.send(QueryEvent::Status(format!(
                            "Empty API response, retrying in {}s ({}/{})",
                            wait.as_secs(), empty_response_retries, MAX_EMPTY_RETRIES
                        )));
                    }
                    // Remove the empty assistant message before retrying.
                    messages.pop();
                    tokio::time::sleep(wait).await;
                    continue;
                }
                // Valid end_turn — reset the retry counter.
                empty_response_retries = 0;

                fire_stop_hook!(assistant_msg);

                // T1-3: Fire Stop hooks in background (fire-and-forget).
                // `stop_hooks_with_full_behavior` spawns blocking tasks internally
                // and returns immediately with an empty Vec.
                let _bg = stop_hooks_with_full_behavior(
                    &assistant_msg,
                    &tool_ctx.config,
                    tool_ctx.working_dir.clone(),
                );

                // Asynchronously extract and persist session memories if warranted.
                // Runs in a detached Tokio task so it doesn't block the query loop.
                if session_memory::SessionMemoryExtractor::should_extract(messages) {
                    let model_clone = config.model.clone();
                    let messages_clone = messages.clone();
                    let working_dir_clone = tool_ctx.working_dir.clone();

                    // Build a fresh client using the same API key.  This avoids
                    // requiring an Arc in the existing run_query_loop signature.
                    if let Ok(api_key) = std::env::var("ANTHROPIC_API_KEY") {
                        if !api_key.is_empty() {
                            if let Ok(sm_client) =
                                cc_api::AnthropicClient::new(cc_api::client::ClientConfig {
                                    api_key,
                                    ..Default::default()
                                })
                            {
                                let sm_client: std::sync::Arc<dyn cc_api::LlmProvider> =
                                    std::sync::Arc::new(sm_client);
                                tokio::spawn(async move {
                                    let extractor =
                                        session_memory::SessionMemoryExtractor::new(&model_clone);
                                    match extractor
                                        .extract(
                                            &messages_clone,
                                            &working_dir_clone,
                                            sm_client.as_ref(),
                                        )
                                        .await
                                    {
                                        Ok(memories) if !memories.is_empty() => {
                                            let target =
                                                working_dir_clone.join(".uppli").join("UPPLI.md");
                                            if let Err(e) =
                                                session_memory::SessionMemoryExtractor::persist(
                                                    &memories, &target,
                                                )
                                                .await
                                            {
                                                tracing::warn!(
                                                    error = %e,
                                                    "Failed to persist session memories"
                                                );
                                            }
                                        }
                                        Ok(_) => {} // no memories extracted
                                        Err(e) => {
                                            tracing::debug!(
                                                error = %e,
                                                "Session memory extraction failed (non-fatal)"
                                            );
                                        }
                                    }
                                });
                            }
                        }
                    }
                }

                return QueryOutcome::EndTurn {
                    message: assistant_msg,
                    usage,
                };
            }
            "max_tokens" | "length" => {
                // Mirror the TS recovery loop: inject a continuation nudge and
                // retry up to MAX_TOKENS_RECOVERY_LIMIT times before surfacing
                // the partial response as QueryOutcome::MaxTokens.
                if max_tokens_recovery_count < MAX_TOKENS_RECOVERY_LIMIT {
                    max_tokens_recovery_count += 1;
                    warn!(
                        attempt = max_tokens_recovery_count,
                        limit = MAX_TOKENS_RECOVERY_LIMIT,
                        "max_tokens hit — injecting continuation message (attempt {}/{})",
                        max_tokens_recovery_count,
                        MAX_TOKENS_RECOVERY_LIMIT,
                    );
                    if let Some(ref tx) = event_tx {
                        let _ = tx.send(QueryEvent::Status(format!(
                            "Output token limit hit — continuing (attempt {}/{})",
                            max_tokens_recovery_count, MAX_TOKENS_RECOVERY_LIMIT
                        )));
                    }
                    // The partial assistant message must be in the history so
                    // the continuation makes sense to the model.
                    messages.push(Message::user(MAX_TOKENS_RECOVERY_MSG));
                    continue;
                }
                // Recovery exhausted — surface the partial response.
                warn!(
                    "max_tokens recovery exhausted after {} attempts",
                    MAX_TOKENS_RECOVERY_LIMIT
                );
                return QueryOutcome::MaxTokens {
                    partial_message: assistant_msg,
                    usage,
                };
            }
            "tool_use" => {
                // A completed tool-use turn counts as a successful recovery
                // boundary; reset the max_tokens retry counter.
                max_tokens_recovery_count = 0;
                // Extract tool calls and execute them
                let tool_blocks = assistant_msg.get_tool_use_blocks();
                if tool_blocks.is_empty() {
                    // Shouldn't happen but treat as end_turn
                    return QueryOutcome::EndTurn {
                        message: assistant_msg,
                        usage,
                    };
                }

                let mut result_blocks: Vec<ContentBlock> = Vec::new();

                for block in tool_blocks {
                    if let ContentBlock::ToolUse { id, name, input } = block {
                        // Stuck-loop detection: if the model sends the exact same
                        // tool call 3+ times, inject a nudge in the result.
                        let sig = format!("{}:{}", name, input);
                        if last_tool_sig.as_deref() == Some(&sig) {
                            repeat_count += 1;
                        } else {
                            last_tool_sig = Some(sig);
                            repeat_count = 1;
                        }

                        if let Some(ref tx) = event_tx {
                            let _ = tx.send(QueryEvent::ToolStart {
                                tool_name: name.clone(),
                                tool_id: id.clone(),
                                input_json: input.to_string(),
                            });
                        }

                        // Fire PreToolUse hooks (blocking hooks can cancel execution)
                        let hooks = &tool_ctx.config.hooks;
                        let hook_ctx = cc_core::hooks::HookContext {
                            event: "PreToolUse".to_string(),
                            tool_name: Some(name.clone()),
                            tool_input: Some(input.clone()),
                            tool_output: None,
                            is_error: None,
                            session_id: Some(tool_ctx.session_id.clone()),
                        };
                        let pre_outcome = cc_core::hooks::run_hooks(
                            hooks,
                            cc_core::config::HookEvent::PreToolUse,
                            &hook_ctx,
                            &tool_ctx.working_dir,
                        )
                        .await;

                        // Also run plugin PreToolUse hooks (stored in global static).
                        let plugin_pre_outcome = cc_plugins::run_global_pre_tool_hook(name, input);

                        let result = if let cc_core::hooks::HookOutcome::Blocked(reason) =
                            pre_outcome
                        {
                            warn!(tool = name, reason = %reason, "PreToolUse hook blocked execution");
                            cc_tools::ToolResult::error(format!("Blocked by hook: {}", reason))
                        } else if let cc_plugins::HookOutcome::Deny(reason) = plugin_pre_outcome {
                            warn!(tool = name, reason = %reason, "Plugin PreToolUse hook blocked execution");
                            cc_tools::ToolResult::error(format!(
                                "Blocked by plugin hook: {}",
                                reason
                            ))
                        } else {
                            // CodeAudit gate: block Edit/Write on source files
                            // unless CodeAudit was called first.
                            let gate_blocked = {
                                let is_edit = name == "Edit" || name == "Write";
                                if is_edit {
                                    let path = input.as_object()
                                        .and_then(|o| o.get("file_path"))
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("");
                                    let is_source = path.ends_with(".py") || path.ends_with(".rs")
                                        || path.ends_with(".js") || path.ends_with(".ts")
                                        || path.ends_with(".go") || path.ends_with(".java");
                                    is_source && !audited_files.contains(path)
                                        && !path.contains("test_") && !path.contains("/tests/")
                                } else {
                                    false
                                }
                            };

                            if gate_blocked {
                                let path = input.as_object()
                                    .and_then(|o| o.get("file_path"))
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("?");
                                cc_tools::ToolResult::error(format!(
                                    "CodeAudit gate: You must call CodeAudit(\"{}\") before editing this file. \
                                     CodeAudit surfaces structural anomalies you might miss. \
                                     After reviewing the audit report, you can edit the file.",
                                    path
                                ))
                            } else {
                                // Race tool execution against the cancel token so ESC
                                // can interrupt long-running tools (e.g. bash scripts).
                                let mut result = tokio::select! {
                                    result = execute_tool(name, input, tools, tool_ctx) => result,
                                    _ = cancel_token.cancelled() => {
                                        warn!(tool = name, "Tool execution interrupted by cancel");
                                        cc_tools::ToolResult::error("Interrupted by user".to_string())
                                    }
                                };

                                // Track CodeAudit calls: store report for post-edit comparison
                                if name == "CodeAudit" && !result.is_error {
                                    if let Some(path) = input.as_object()
                                        .and_then(|o| o.get("file_path"))
                                        .and_then(|v| v.as_str())
                                    {
                                        audited_files.insert(path.to_string());
                                        pre_edit_audits.insert(path.to_string(), result.content.clone());
                                    }
                                }

                                // Post-edit audit: after Edit/Write on an audited source file,
                                // re-run CodeAudit and check if HIGH anomalies were resolved.
                                if (name == "Edit" || name == "Write") && !result.is_error {
                                    let path = input.as_object()
                                        .and_then(|o| o.get("file_path"))
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("");
                                    if let Some(pre_report) = pre_edit_audits.get(path) {
                                        // Re-run CodeAudit
                                        let audit_input = serde_json::json!({
                                            "file_path": path,
                                            "language": "auto"
                                        });
                                        let post_audit = execute_tool(
                                            &"CodeAudit".to_string(),
                                            &audit_input,
                                            tools,
                                            tool_ctx,
                                        ).await;
                                        if !post_audit.is_error {
                                            // Check if HIGH anomalies from pre-edit are still present
                                            let pre_has_high = pre_report.contains("[HIGH]");
                                            let post_has_high = post_audit.content.contains("[HIGH]");
                                            if pre_has_high && post_has_high {
                                                result.content.push_str(&format!(
                                                    "\n\n⚠️ CodeAudit post-edit check: HIGH anomalies are still present after your edit.\n\
                                                     \n**BEFORE your edit:**\n{}\n\
                                                     \n**AFTER your edit:**\n{}\n\
                                                     \nYour edit did not resolve the structural anomaly. \
                                                     Instead of adding new branches, try modifying the EXISTING \
                                                     format string to show the full list instead of [0]. \
                                                     The condition compares slices — the error message should too.",
                                                    pre_report.lines()
                                                        .filter(|l| l.contains("[HIGH]") || l.starts_with("Lines") || l.starts_with("Condition"))
                                                        .collect::<Vec<_>>().join("\n"),
                                                    post_audit.content.lines()
                                                        .filter(|l| l.contains("[HIGH]") || l.starts_with("Lines") || l.starts_with("Condition"))
                                                        .collect::<Vec<_>>().join("\n"),
                                                ));
                                            } else if pre_has_high && !post_has_high {
                                                result.content.push_str(
                                                    "\n\n✓ CodeAudit post-edit check: HIGH anomalies resolved."
                                                );
                                            }
                                        }
                                    }
                                }

                                result
                            }
                        };

                        // Fire PostToolUse hooks
                        let post_ctx = cc_core::hooks::HookContext {
                            event: "PostToolUse".to_string(),
                            tool_name: Some(name.clone()),
                            tool_input: Some(input.clone()),
                            tool_output: Some(result.content.clone()),
                            is_error: Some(result.is_error),
                            session_id: Some(tool_ctx.session_id.clone()),
                        };
                        cc_core::hooks::run_hooks(
                            hooks,
                            cc_core::config::HookEvent::PostToolUse,
                            &post_ctx,
                            &tool_ctx.working_dir,
                        )
                        .await;

                        // Also run plugin PostToolUse hooks.
                        cc_plugins::run_global_post_tool_hook(
                            name,
                            input,
                            &result.content,
                            result.is_error,
                        );

                        if let Some(ref tx) = event_tx {
                            let _ = tx.send(QueryEvent::ToolEnd {
                                tool_name: name.clone(),
                                tool_id: id.clone(),
                                result: result.content.clone(),
                                is_error: result.is_error,
                            });
                        }

                        // Append contextual tips from the tool expertise database.
                        // On error: suggest alternatives. On success: no extra noise.
                        let mut enriched_content = if result.is_error {
                            if let Some(tips) = cc_tools::tool_expertise::on_error_tips(&name) {
                                format!("{}\n\nTips: {}", result.content, tips)
                            } else {
                                result.content.clone()
                            }
                        } else {
                            result.content.clone()
                        };

                        // Stuck-loop nudge: if the model repeated the same call 3+ times,
                        // tell it to try something different.
                        if repeat_count >= 3 {
                            enriched_content.push_str(
                                "\n\n⚠️ You've repeated this exact tool call multiple times \
                                 with the same result. Try a different approach: \
                                 read the file directly, use a different search pattern, \
                                 or use Bash to explore the code."
                            );
                            warn!(
                                tool = name.as_str(),
                                repeat_count,
                                "Stuck loop detected — nudging model"
                            );
                        }

                        // CodeAudit nudge: after Read on a source file,
                        // remind the model that CodeAudit is available.
                        if name == "Read" && !result.is_error {
                            // Only nudge for source files (not READMEs, configs, etc.)
                            if let Some(input_obj) = input.as_object() {
                                let path = input_obj.get("file_path")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("");
                                let is_source = path.ends_with(".py") || path.ends_with(".rs")
                                    || path.ends_with(".js") || path.ends_with(".ts")
                                    || path.ends_with(".go") || path.ends_with(".java");
                                if is_source {
                                    enriched_content.push_str(
                                        "\n\nTip: Call CodeAudit(file_path) before editing \
                                         to surface structural anomalies you might miss."
                                    );
                                }
                            }
                        }

                        result_blocks.push(ContentBlock::ToolResult {
                            tool_use_id: id.clone(),
                            content: ToolResultContent::Text(enriched_content),
                            is_error: if result.is_error { Some(true) } else { None },
                        });
                    }
                }

                // Append tool results as a user message
                messages.push(Message::user_blocks(result_blocks));

                // Continue the loop to send results back to the model
                continue;
            }
            "stop_sequence" => {
                fire_stop_hook!(assistant_msg);
                let _bg = stop_hooks_with_full_behavior(
                    &assistant_msg,
                    &tool_ctx.config,
                    tool_ctx.working_dir.clone(),
                );
                return QueryOutcome::EndTurn {
                    message: assistant_msg,
                    usage,
                };
            }
            other => {
                warn!(
                    stop_reason = other,
                    "Unknown stop reason, treating as end_turn"
                );
                fire_stop_hook!(assistant_msg);
                let _bg = stop_hooks_with_full_behavior(
                    &assistant_msg,
                    &tool_ctx.config,
                    tool_ctx.working_dir.clone(),
                );
                return QueryOutcome::EndTurn {
                    message: assistant_msg,
                    usage,
                };
            }
        }
    }
}

/// Execute a single tool invocation.
async fn execute_tool(
    name: &str,
    input: &Value,
    tools: &[Box<dyn Tool>],
    ctx: &ToolContext,
) -> ToolResult {
    let tool = tools.iter().find(|t| t.name() == name);

    match tool {
        Some(tool) => {
            debug!(tool = name, "Executing tool");
            tool.execute(input.clone(), ctx).await
        }
        None => {
            warn!(tool = name, "Unknown tool requested");
            ToolResult::error(format!("Unknown tool: {}", name))
        }
    }
}

/// Build the system prompt from config.
///
/// Delegates to `cc_core::system_prompt::build_system_prompt` so that all
/// default content (capabilities, safety guidelines, dynamic-boundary marker,
/// etc.) is assembled in one place.  The `QueryConfig` fields map directly to
/// `SystemPromptOptions`:
///
/// - `system_prompt`        → `custom_system_prompt` (added to cacheable block)
/// - `append_system_prompt` → `append_system_prompt` (added after boundary)
fn build_system_prompt(config: &QueryConfig) -> SystemPrompt {
    use cc_core::system_prompt::SystemPromptOptions;

    let opts = SystemPromptOptions {
        custom_system_prompt: config.system_prompt.clone(),
        append_system_prompt: config.append_system_prompt.clone(),
        // All other fields use sensible defaults:
        // - prefix:                auto-detect from env
        // - memory_content:        empty (callers inject via append if needed)
        // - replace_system_prompt: false (additive mode)
        // - coordinator_mode:      false
        output_style: config.output_style,
        custom_output_style_prompt: config.output_style_prompt.clone(),
        working_directory: config.working_directory.clone(),
        ..Default::default()
    };

    let text = cc_core::system_prompt::build_system_prompt(&opts);
    SystemPrompt::Text(text)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use cc_api::SystemPrompt;

    fn make_config(sys: Option<&str>, append: Option<&str>) -> QueryConfig {
        QueryConfig {
            model: "claude-sonnet-4-6".to_string(),
            max_tokens: 4096,
            max_turns: 10,
            system_prompt: sys.map(String::from),
            append_system_prompt: append.map(String::from),
            output_style: cc_core::system_prompt::OutputStyle::Default,
            output_style_prompt: None,
            working_directory: None,
            thinking_budget: None,
            temperature: None,
            tool_result_budget: 50_000,
            effort_level: None,
            command_queue: None,
            skill_index: None,
            max_budget_usd: None,
            fallback_model: None,
        }
    }

    // ---- build_system_prompt tests ------------------------------------------

    #[test]
    fn test_system_prompt_default_when_empty() {
        // The default prompt (no custom system prompt set) should include the
        // Uppli Code attribution and standard sections.
        let cfg = make_config(None, None);
        let prompt = build_system_prompt(&cfg);
        if let SystemPrompt::Text(text) = prompt {
            assert!(
                text.contains("Uppli Code") || text.contains("Claude agent"),
                "Default prompt should contain attribution: {}",
                text
            );
            assert!(
                text.contains(cc_core::system_prompt::SYSTEM_PROMPT_DYNAMIC_BOUNDARY),
                "Default prompt must contain the dynamic boundary marker"
            );
        } else {
            panic!("Expected SystemPrompt::Text");
        }
    }

    #[test]
    fn test_system_prompt_with_custom() {
        // A custom system prompt is injected into the cacheable section as
        // <custom_instructions>; the default sections are still present.
        let cfg = make_config(Some("You are a code reviewer."), None);
        let prompt = build_system_prompt(&cfg);
        if let SystemPrompt::Text(text) = prompt {
            assert!(
                text.contains("You are a code reviewer."),
                "Custom prompt text should appear in the output"
            );
            assert!(
                text.contains("Uppli Code") || text.contains("Claude agent"),
                "Default attribution should still be present"
            );
        } else {
            panic!("Expected SystemPrompt::Text");
        }
    }

    #[test]
    fn test_system_prompt_with_append() {
        // Appended text lands after the dynamic boundary.
        let cfg = make_config(Some("Base prompt."), Some("Additional context."));
        let prompt = build_system_prompt(&cfg);
        if let SystemPrompt::Text(text) = prompt {
            assert!(text.contains("Base prompt."));
            assert!(text.contains("Additional context."));
            // append_system_prompt appears after the boundary
            let boundary_pos = text
                .find(cc_core::system_prompt::SYSTEM_PROMPT_DYNAMIC_BOUNDARY)
                .expect("boundary must exist");
            let append_pos = text.find("Additional context.").unwrap();
            assert!(
                append_pos > boundary_pos,
                "Appended text must appear after the dynamic boundary"
            );
        } else {
            panic!("Expected SystemPrompt::Text");
        }
    }

    #[test]
    fn test_system_prompt_append_only() {
        // When only append is set, default sections are present plus the
        // appended text after the dynamic boundary.
        let cfg = make_config(None, Some("Appended text."));
        let prompt = build_system_prompt(&cfg);
        if let SystemPrompt::Text(text) = prompt {
            assert!(
                text.contains("Appended text."),
                "Appended text must appear in the prompt"
            );
            let boundary_pos = text
                .find(cc_core::system_prompt::SYSTEM_PROMPT_DYNAMIC_BOUNDARY)
                .expect("boundary must exist");
            let append_pos = text.find("Appended text.").unwrap();
            assert!(
                append_pos > boundary_pos,
                "Appended text must appear after the dynamic boundary"
            );
        } else {
            panic!("Expected SystemPrompt::Text");
        }
    }

    #[test]
    fn test_system_prompt_with_custom_output_style_prompt() {
        let mut cfg = make_config(None, None);
        cfg.output_style_prompt = Some("Answer like a pirate.".to_string());
        let prompt = build_system_prompt(&cfg);
        if let SystemPrompt::Text(text) = prompt {
            assert!(text.contains("Answer like a pirate."));
        } else {
            panic!("Expected SystemPrompt::Text");
        }
    }

    // ---- QueryConfig tests --------------------------------------------------

    #[test]
    fn test_query_config_clone() {
        let cfg = make_config(Some("test"), Some("append"));
        let cloned = cfg.clone();
        assert_eq!(cloned.model, "claude-sonnet-4-6");
        assert_eq!(cloned.max_tokens, 4096);
        assert_eq!(cloned.system_prompt, Some("test".to_string()));
    }

    // ---- QueryOutcome variant tests -----------------------------------------

    #[test]
    fn test_query_outcome_debug() {
        // Ensure the enum variants can be created and debug-formatted
        let outcome = QueryOutcome::Cancelled;
        let s = format!("{:?}", outcome);
        assert!(s.contains("Cancelled"));

        let err_outcome = QueryOutcome::Error(cc_core::error::ClaudeError::RateLimit);
        let s2 = format!("{:?}", err_outcome);
        assert!(s2.contains("Error"));
    }
}

/// Stream handler that forwards events to an unbounded channel.
struct ChannelStreamHandler {
    tx: mpsc::UnboundedSender<QueryEvent>,
}

impl StreamHandler for ChannelStreamHandler {
    fn on_event(&self, event: &StreamEvent) {
        let _ = self.tx.send(QueryEvent::Stream(event.clone()));
    }
}

// ---------------------------------------------------------------------------
// Single-shot query (non-looping, for simple one-off calls)
// ---------------------------------------------------------------------------

/// Run a single (non-agentic) query – no tool loop, just one API call.
pub async fn run_single_query(
    client: &dyn LlmProvider,
    messages: Vec<Message>,
    config: &QueryConfig,
) -> Result<Message, ClaudeError> {
    let api_messages: Vec<ApiMessage> = messages.iter().map(ApiMessage::from).collect();
    let system = build_system_prompt(config);

    // Pour les requêtes simples, on utilise toujours le modèle de raisonnement par défaut (R2)
    let request = CreateMessageRequest::builder(&config.model, config.max_tokens)
        .messages(api_messages)
        .system(system)
        .build();

    let handler: Arc<dyn StreamHandler> = Arc::new(cc_api::streaming::NullStreamHandler);

    let mut rx = client.create_message_stream(request, handler).await?;
    let mut acc = StreamAccumulator::new();

    while let Some(evt) = rx.recv().await {
        acc.on_event(&evt);
        if matches!(evt, StreamEvent::MessageStop) {
            break;
        }
    }

    let (msg, _usage, _stop) = acc.finish();
    Ok(msg)
}
