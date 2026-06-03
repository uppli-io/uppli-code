// Auto-compact service for cc-query.
//
// When the conversation context window fills up (~90%+), we automatically
// summarise older messages to free space. This mirrors the TypeScript
// autoCompact / compact service behaviour.
//
// Strategy:
//   1. Keep the last KEEP_RECENT_MESSAGES messages verbatim.
//   2. Group messages by API round (same assistant message ID) and summarise
//      all groups except the most recent KEEP_RECENT_MESSAGES worth.
//   3. Replace the head of the conversation with a single synthetic
//      <compact-summary> user message, followed by the recent tail.
//
// The summary is generated in a single non-agentic API call so it doesn't
// trigger another compaction recursively.
//
// MicroCompact strategy (partial compaction):
//   When context is above `trigger_threshold` but not yet at the full
//   auto-compact level, we summarise only the oldest messages while keeping
//   the most recent `keep_recent_messages` intact.  This is lighter than a
//   full compaction and can fire proactively at 75 % capacity.

use cc_api::{
    ApiMessage, CreateMessageRequest, StreamAccumulator, StreamEvent, StreamHandler, SystemPrompt,
};
use cc_core::error::ClaudeError;
use cc_core::types::{ContentBlock, Message, MessageContent, Role};
use serde_json::Value;
use std::sync::Arc;
use tracing::{debug, info, warn};

// ---------------------------------------------------------------------------
// Constants (mirrors TypeScript autoCompact.ts)
// ---------------------------------------------------------------------------

/// Start warning when this many tokens remain in the context window.
const WARNING_THRESHOLD_BUFFER_TOKENS: u64 = 20_000;

/// Fraction of the context window at which auto-compact triggers.
const AUTOCOMPACT_TRIGGER_FRACTION: f64 = 0.95;

/// Default for how many recent messages to preserve verbatim after compaction.
/// Effective value is `Config::compact_keep_recent_messages`
/// (CLI: --compact-keep-recent-messages).
const KEEP_RECENT_MESSAGES: usize = cc_core::constants::DEFAULT_COMPACT_KEEP_RECENT_MESSAGES;

/// Default max consecutive auto-compact failures before giving up
/// (circuit breaker). The effective value at runtime is
/// `Config::effective_max_compact_retries()` (CLI: --max-compact-retries),
/// resolved through `cc_core::constants::MAX_COMPACT_RETRIES`.
const MAX_CONSECUTIVE_FAILURES: u32 = cc_core::constants::MAX_COMPACT_RETRIES;

// Percentage thresholds for token warning states (mirrors TS autoCompact.ts).
// Effective values are sourced from QueryConfig / Config (see
// --compact-warning-pct / --compact-critical-pct). These constants are kept
// only as compile-time defaults.
const WARNING_PCT: f64 = cc_core::constants::DEFAULT_COMPACT_WARNING_PCT; // 90 % full → yellow warning
const CRITICAL_PCT: f64 = cc_core::constants::DEFAULT_COMPACT_CRITICAL_PCT; // 98 % full → red critical

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Tracks auto-compact state across turns.
#[derive(Debug, Default, Clone)]
pub struct AutoCompactState {
    /// Total compactions performed this session.
    pub compaction_count: u32,
    /// Consecutive failures (reset on success).
    pub consecutive_failures: u32,
    /// Whether the circuit breaker is open (too many failures).
    pub disabled: bool,
}

impl AutoCompactState {
    /// Record a successful compaction.
    pub fn on_success(&mut self) {
        self.compaction_count += 1;
        self.consecutive_failures = 0;
    }

    /// Record a failed compaction; open circuit breaker if too many.
    /// Uses the compile-time default `MAX_CONSECUTIVE_FAILURES` — for
    /// runtime-configured limits, use `on_failure_with_limit`.
    pub fn on_failure(&mut self) {
        self.on_failure_with_limit(MAX_CONSECUTIVE_FAILURES);
    }

    /// Record a failed compaction; open circuit breaker after
    /// `max_retries` consecutive failures. Wired by the query loop from
    /// `Config::effective_max_compact_retries()` (CLI:
    /// --max-compact-retries) so the user can raise the threshold when
    /// the model is paying the cost of transient compaction failures.
    pub fn on_failure_with_limit(&mut self, max_retries: u32) {
        self.consecutive_failures += 1;
        if self.consecutive_failures >= max_retries {
            warn!(
                failures = self.consecutive_failures,
                limit = max_retries,
                "Auto-compact circuit breaker opened – disabling for this session"
            );
            self.disabled = true;
        }
    }
}

/// Token-usage state relative to the context window.
/// Matches the TypeScript TokenWarningState semantics:
///   Ok      = below 80 % of context window
///   Warning = 80–95 % ("yellow" in TUI)
///   Critical= above 95 % ("red" in TUI)
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenWarningState {
    /// Plenty of space left.
    Ok,
    /// Getting close – warn the user (≥ 80 %).
    Warning,
    /// Critical – compact now (≥ 95 %).
    Critical,
}

// ---------------------------------------------------------------------------
// Message grouping (from TypeScript grouping.ts)
// ---------------------------------------------------------------------------

/// A semantically coherent chunk of messages suitable for individual
/// summarisation.  Groups are formed at API-round boundaries: one group per
/// assistant response, which naturally pairs every tool_use with its result.
#[derive(Debug, Clone)]
pub struct MessageGroup {
    pub messages: Vec<Message>,
    /// First file path or tool name mentioned in this group, if any.
    pub topic_hint: Option<String>,
    /// Rough token estimate for the group (chars / 4, padded by 4/3).
    pub token_estimate: usize,
}

impl MessageGroup {
    fn from_messages(messages: Vec<Message>) -> Self {
        let topic_hint = extract_topic_hint(&messages);
        let token_estimate = estimate_tokens_for_messages(&messages);
        Self {
            messages,
            topic_hint,
            token_estimate,
        }
    }
}

/// Extract a short "topic hint" from a group: first file path or tool name
/// mentioned in any tool_use or tool_result block.
fn extract_topic_hint(messages: &[Message]) -> Option<String> {
    for msg in messages {
        let blocks = match &msg.content {
            MessageContent::Blocks(b) => b,
            _ => continue,
        };
        for block in blocks {
            if let ContentBlock::ToolUse { name, input, .. } = block {
                // Try to get a file_path from input, else use tool name
                if let Some(fp) = input.get("file_path").and_then(|v| v.as_str()) {
                    return Some(fp.to_string());
                }
                if let Some(cmd) = input.get("command").and_then(|v| v.as_str()) {
                    // Use first word of command as hint
                    let first_word = cmd.split_whitespace().next().unwrap_or(cmd);
                    return Some(first_word.to_string());
                }
                return Some(name.clone());
            }
        }
    }
    None
}

/// Rough token estimate: sum of character lengths divided by 4, padded by 4/3.
fn estimate_tokens_for_messages(messages: &[Message]) -> usize {
    let chars: usize = messages
        .iter()
        .map(|m| match &m.content {
            MessageContent::Text(t) => t.len(),
            MessageContent::Blocks(blocks) => blocks.iter().map(estimate_block_chars).sum(),
        })
        .sum();
    // chars / 4 = rough tokens, then * 4/3 padding
    (chars / 4) * 4 / 3
}

fn estimate_block_chars(block: &ContentBlock) -> usize {
    match block {
        ContentBlock::Text { text } => text.len(),
        ContentBlock::ToolUse { name, input, .. } => name.len() + input.to_string().len(),
        ContentBlock::ToolResult { content, .. } => match content {
            cc_core::types::ToolResultContent::Text(t) => t.len(),
            cc_core::types::ToolResultContent::Blocks(blocks) => {
                blocks.iter().map(estimate_block_chars).sum()
            }
        },
        ContentBlock::Thinking { thinking, .. } => thinking.len(),
        ContentBlock::RedactedThinking { data } => data.len(),
        _ => 200, // default for images/documents
    }
}

/// Group messages at API-round boundaries: one group per assistant response.
/// This mirrors `groupMessagesByApiRound` from TypeScript grouping.ts.
///
/// Each group represents one complete API round:
///   [user_messages..., assistant_response]
///
/// Boundary detection:
/// - When messages have UUIDs, a new group fires at the START of each new
///   assistant message whose UUID differs from the previous one.
/// - When messages lack UUIDs (local / test messages), boundaries fire
///   when an assistant message follows a PREVIOUS assistant in the current
///   group — i.e. each assistant turn closes its own group.
///
/// The result is that user messages are grouped with the SUBSEQUENT assistant
/// response that replies to them (matching TypeScript round semantics).
pub fn group_messages_for_compact(messages: &[Message]) -> Vec<MessageGroup> {
    let mut groups: Vec<MessageGroup> = Vec::new();
    let mut current: Vec<Message> = Vec::new();

    for msg in messages {
        if msg.role == Role::Assistant {
            // Add this assistant message to the current group (with any
            // accumulated user messages from this round).
            current.push(msg.clone());

            // Close the group: the next user message(s) belong to the next round.
            groups.push(MessageGroup::from_messages(current.clone()));
            current.clear();
        } else {
            current.push(msg.clone());
        }
    }

    // Any trailing non-assistant messages (shouldn't happen in practice)
    // form their own group.
    if !current.is_empty() {
        groups.push(MessageGroup::from_messages(current));
    }

    groups
}

// ---------------------------------------------------------------------------
// MicroCompact configuration & logic
// ---------------------------------------------------------------------------

/// Configuration for micro-compaction (partial, proactive summarisation).
#[derive(Debug, Clone)]
pub struct MicroCompactConfig {
    /// Compact when context is this fraction full (e.g. 0.75 = 75 %).
    pub trigger_threshold: f32,
    /// Always keep this many recent messages verbatim.
    pub keep_recent_messages: usize,
    /// Target token count for the generated summary.
    pub summary_target_tokens: usize,
}

impl Default for MicroCompactConfig {
    fn default() -> Self {
        Self {
            trigger_threshold: 0.90,
            keep_recent_messages: 10,
            summary_target_tokens: 2048,
        }
    }
}

/// Attempt a micro-compact if the context is above `config.trigger_threshold`.
///
/// Returns `Some(new_messages)` when compaction occurred, `None` otherwise.
pub async fn micro_compact_if_needed(
    client: &dyn cc_api::LlmProvider,
    messages: &[Message],
    input_tokens: u64,
    model: &str,
    config: &MicroCompactConfig,
    context_window: u64,
) -> Option<Vec<Message>> {
    let window = context_window;
    let pct_used = input_tokens as f64 / window as f64;

    if pct_used < config.trigger_threshold as f64 {
        return None;
    }

    let total = messages.len();
    if total <= config.keep_recent_messages + 1 {
        return None;
    }

    let split_at = total.saturating_sub(config.keep_recent_messages);

    info!(
        input_tokens,
        pct_used = format!("{:.1}%", pct_used * 100.0),
        split_at,
        keep = config.keep_recent_messages,
        "MicroCompact triggered"
    );

    let target_tokens = config.summary_target_tokens as u32;
    match summarise_head(client, messages, split_at, model, target_tokens).await {
        Ok(new_msgs) => {
            info!(
                original = total,
                compacted = new_msgs.len(),
                "MicroCompact complete"
            );
            Some(new_msgs)
        }
        Err(e) => {
            warn!(error = %e, "MicroCompact failed");
            None
        }
    }
}

// ---------------------------------------------------------------------------
// Compaction prompt (matches TypeScript prompt.ts)
// ---------------------------------------------------------------------------

/// The critical preamble that prevents the summariser from making tool calls.
const NO_TOOLS_PREAMBLE: &str = "CRITICAL: Respond with TEXT ONLY. Do NOT call any tools.\n\
\n\
- Do NOT use Read, Bash, Grep, Glob, Edit, Write, or ANY other tool.\n\
- You already have all the context you need in the conversation above.\n\
- Tool calls will be REJECTED and will waste your only turn — you will fail the task.\n\
- Your entire response must be plain text: an <analysis> block followed by a <summary> block.\n\
\n";

/// The trailing reminder that reinforces the no-tools instruction.
const NO_TOOLS_TRAILER: &str =
    "\n\nREMINDER: Do NOT call any tools. Respond with plain text only — \
an <analysis> block followed by a <summary> block. \
Tool calls will be rejected and you will fail the task.";

/// The base compaction prompt (mirrors BASE_COMPACT_PROMPT from TypeScript prompt.ts).
const BASE_COMPACT_PROMPT: &str = "Your task is to create a detailed summary of the conversation \
so far, paying close attention to the user's explicit requests and your previous actions.\n\
This summary should be thorough in capturing technical details, code patterns, and architectural \
decisions that would be essential for continuing development work without losing context.\n\
\n\
Before providing your final summary, wrap your analysis in <analysis> tags to organize your \
thoughts and ensure you've covered all necessary points. In your analysis process:\n\
\n\
1. Chronologically analyze each message and section of the conversation. For each section \
thoroughly identify:\n\
   - The user's explicit requests and intents\n\
   - Your approach to addressing the user's requests\n\
   - Key decisions, technical concepts and code patterns\n\
   - Specific details like:\n\
     - file names\n\
     - full code snippets\n\
     - function signatures\n\
     - file edits\n\
   - Errors that you ran into and how you fixed them\n\
   - Pay special attention to specific user feedback that you received, especially if the user \
told you to do something differently.\n\
2. Double-check for technical accuracy and completeness, addressing each required element \
thoroughly.\n\
\n\
Your summary should include the following sections:\n\
\n\
1. Primary Request and Intent: Capture all of the user's explicit requests and intents in detail\n\
2. Key Technical Concepts: List all important technical concepts, technologies, and frameworks \
discussed.\n\
3. Files and Code Sections: Enumerate specific files and code sections examined, modified, or \
created. Pay special attention to the most recent messages and include full code snippets where \
applicable and include a summary of why this file read or edit is important.\n\
4. Errors and fixes: List all errors that you ran into, and how you fixed them. Pay special \
attention to specific user feedback that you received, especially if the user told you to do \
something differently.\n\
5. Problem Solving: Document problems solved and any ongoing troubleshooting efforts.\n\
6. All user messages: List ALL user messages that are not tool results. These are critical for \
understanding the users' feedback and changing intent.\n\
7. Pending Tasks: Outline any pending tasks that you have explicitly been asked to work on.\n\
8. Current Work: Describe in detail precisely what was being worked on immediately before this \
summary request, paying special attention to the most recent messages from both user and \
assistant. Include file names and code snippets where applicable.\n\
9. Optional Next Step: List the next step that you will take that is related to the most recent \
work you were doing. IMPORTANT: ensure that this step is DIRECTLY in line with the user's most \
recent explicit requests, and the task you were working on immediately before this summary \
request. If your last task was concluded, then only list next steps if they are explicitly in \
line with the users request. Do not start on tangential requests or really old requests that \
were already completed without confirming with the user first.\n\
                       If there is a next step, include direct quotes from the most recent \
conversation showing exactly what task you were working on and where you left off. This should \
be verbatim to ensure there's no drift in task interpretation.\n\
\n\
Format your output as:\n\
\n\
<analysis>\n\
[Your thought process, ensuring all points are covered thoroughly and accurately]\n\
</analysis>\n\
\n\
<summary>\n\
1. Primary Request and Intent:\n\
   [Detailed description]\n\
\n\
2. Key Technical Concepts:\n\
   - [Concept 1]\n\
   - [Concept 2]\n\
\n\
3. Files and Code Sections:\n\
   - [File Name 1]\n\
      - [Summary of why this file is important]\n\
      - [Summary of the changes made to this file, if any]\n\
      - [Important Code Snippet]\n\
\n\
4. Errors and fixes:\n\
    - [Detailed description of error 1]:\n\
      - [How you fixed the error]\n\
\n\
5. Problem Solving:\n\
   [Description of solved problems and ongoing troubleshooting]\n\
\n\
6. All user messages:\n\
    - [Detailed non tool use user message]\n\
\n\
7. Pending Tasks:\n\
   - [Task 1]\n\
\n\
8. Current Work:\n\
   [Precise description of current work]\n\
\n\
9. Optional Next Step:\n\
   [Optional Next step to take]\n\
</summary>\n\
\n\
Please provide your summary based on the conversation so far, following this structure and \
ensuring precision and thoroughness in your response.";

/// Build the compaction prompt, optionally with custom instructions appended.
pub fn get_compact_prompt(custom_instructions: Option<&str>) -> String {
    let mut prompt = format!("{}{}", NO_TOOLS_PREAMBLE, BASE_COMPACT_PROMPT);

    if let Some(instructions) = custom_instructions {
        let trimmed = instructions.trim();
        if !trimmed.is_empty() {
            prompt.push_str(&format!("\n\nAdditional Instructions:\n{}", trimmed));
        }
    }

    prompt.push_str(NO_TOOLS_TRAILER);
    prompt
}

/// Format the raw compact summary by stripping `<analysis>` and cleaning up
/// `<summary>` XML tags.  Mirrors `formatCompactSummary` from TypeScript
/// prompt.ts.
pub fn format_compact_summary(raw: &str) -> String {
    // Strip <analysis>…</analysis> block (scratchpad, not useful in context)
    let without_analysis = {
        if let (Some(start), Some(end)) = (raw.find("<analysis>"), raw.find("</analysis>")) {
            let before = &raw[..start];
            let after = &raw[end + "</analysis>".len()..];
            format!("{}{}", before, after)
        } else {
            raw.to_string()
        }
    };

    // Extract and reformat <summary>…</summary>
    let formatted = if let (Some(start), Some(end)) = (
        without_analysis.find("<summary>"),
        without_analysis.find("</summary>"),
    ) {
        let before = &without_analysis[..start];
        let content = without_analysis[start + "<summary>".len()..end].trim();
        let after = &without_analysis[end + "</summary>".len()..];
        format!("{}Summary:\n{}{}", before, content, after)
    } else {
        without_analysis
    };

    // Collapse multiple blank lines
    let mut result = String::new();
    let mut blank_count = 0usize;
    for line in formatted.lines() {
        if line.trim().is_empty() {
            blank_count += 1;
            if blank_count <= 1 {
                result.push('\n');
            }
        } else {
            blank_count = 0;
            result.push_str(line);
            result.push('\n');
        }
    }

    result.trim().to_string()
}

// ---------------------------------------------------------------------------
// Threshold helpers
// ---------------------------------------------------------------------------

// Return the effective context-window size in tokens for the given model.
// context_window_for_model() removed -- callers now pass context_window: u64
// pre-resolved via provider.context_window(model).
// These are approximate; the API enforces the real limits server-side.

/// Determine token-warning state given current input token count and model.
///
/// Thresholds default to TypeScript autoCompact.ts (`WARNING_PCT` / `CRITICAL_PCT`)
/// but are now configurable via `--compact-warning-pct` /
/// `--compact-critical-pct`. Use [`calculate_token_warning_state_with`] to
/// pass non-default thresholds.
pub fn calculate_token_warning_state(input_tokens: u64, context_window: u64) -> TokenWarningState {
    calculate_token_warning_state_with(input_tokens, context_window, WARNING_PCT, CRITICAL_PCT)
}

/// Threshold-parameterised variant of [`calculate_token_warning_state`].
///
/// Uses the default warning-buffer cap. Prefer
/// [`calculate_token_warning_state_full`] when you have a `Config` to pull a
/// tuned value from.
pub fn calculate_token_warning_state_with(
    input_tokens: u64,
    context_window: u64,
    warning_pct: f64,
    critical_pct: f64,
) -> TokenWarningState {
    calculate_token_warning_state_full(
        input_tokens,
        context_window,
        warning_pct,
        critical_pct,
        WARNING_THRESHOLD_BUFFER_TOKENS,
    )
}

/// Fully-parameterised variant of [`calculate_token_warning_state`].
pub fn calculate_token_warning_state_full(
    input_tokens: u64,
    context_window: u64,
    warning_pct: f64,
    critical_pct: f64,
    warning_buffer_tokens: u64,
) -> TokenWarningState {
    let window = context_window;
    let pct = input_tokens as f64 / window as f64;

    if pct >= critical_pct {
        TokenWarningState::Critical
    } else if pct >= warning_pct || window.saturating_sub(input_tokens) <= warning_buffer_tokens {
        TokenWarningState::Warning
    } else {
        TokenWarningState::Ok
    }
}

/// Return `true` when auto-compaction should fire.
///
/// Uses the default trigger fraction. Prefer [`should_auto_compact_with`]
/// when you have a `Config` to pull a tuned value from.
pub fn should_auto_compact(
    input_tokens: u64,
    context_window: u64,
    state: &AutoCompactState,
) -> bool {
    should_auto_compact_with(
        input_tokens,
        context_window,
        state,
        AUTOCOMPACT_TRIGGER_FRACTION,
    )
}

/// Fraction-parameterised variant of [`should_auto_compact`].
pub fn should_auto_compact_with(
    input_tokens: u64,
    context_window: u64,
    state: &AutoCompactState,
    trigger_fraction: f64,
) -> bool {
    if state.disabled {
        return false;
    }
    let window = context_window;
    let threshold = (window as f64 * trigger_fraction) as u64;
    input_tokens >= threshold
}

// ---------------------------------------------------------------------------
// Core compaction logic
// ---------------------------------------------------------------------------

/// Summarise `messages[..split_at]` using the Anthropic API using the
/// carefully crafted compaction prompt from TypeScript prompt.ts.
/// Returns a new conversation: [summary user msg] + messages[split_at..].
async fn summarise_head(
    client: &dyn cc_api::LlmProvider,
    messages: &[Message],
    split_at: usize,
    model: &str,
    max_summary_tokens: u32,
) -> Result<Vec<Message>, ClaudeError> {
    if split_at == 0 {
        return Ok(messages.to_vec());
    }

    let head = &messages[..split_at];

    // Build a transcript string for the summarisation prompt.
    let mut transcript = String::new();
    let original_count = head.len();
    let original_token_estimate = estimate_tokens_for_messages(head);

    for msg in head {
        let role_label = match msg.role {
            Role::User => "Human",
            Role::Assistant => "Assistant",
        };
        let text = msg.get_all_text();
        if !text.is_empty() {
            transcript.push_str(&format!("{}: {}\n\n", role_label, text));
        }
        // Also render tool use/result blocks
        if let MessageContent::Blocks(blocks) = &msg.content {
            for block in blocks {
                match block {
                    ContentBlock::ToolUse { name, input, id } => {
                        transcript.push_str(&format!(
                            "[Tool Call: {} (id={})]\nInput: {}\n\n",
                            name, id, input
                        ));
                    }
                    ContentBlock::ToolResult {
                        tool_use_id,
                        content,
                        is_error,
                    } => {
                        let result_text = match content {
                            cc_core::types::ToolResultContent::Text(t) => t.as_str().to_string(),
                            cc_core::types::ToolResultContent::Blocks(_) => {
                                "[complex content]".to_string()
                            }
                        };
                        let error_flag = if is_error.unwrap_or(false) {
                            " [ERROR]"
                        } else {
                            ""
                        };
                        transcript.push_str(&format!(
                            "[Tool Result (id={}){}]\n{}\n\n",
                            tool_use_id, error_flag, result_text
                        ));
                    }
                    _ => {}
                }
            }
        }
    }

    let compact_prompt = get_compact_prompt(None);

    let user_content = format!(
        "{}\n\n<conversation_to_summarize original_messages=\"{}\" estimated_tokens=\"{}\">\n{}\n</conversation_to_summarize>",
        compact_prompt,
        original_count,
        original_token_estimate,
        transcript
    );

    let api_msgs = vec![ApiMessage {
        role: "user".to_string(),
        content: Value::String(user_content),
    }];

    let request = CreateMessageRequest::builder(model, max_summary_tokens)
        .messages(api_msgs)
        .system(SystemPrompt::Text(
            "You are a helpful assistant that creates concise yet thorough conversation summaries. \
             Preserve all technical details, file names, code snippets, and decisions that would \
             be important for continuing the work. Follow the structured format exactly."
                .to_string(),
        ))
        .build();

    // Use a null handler since we just want the final accumulated message.
    let handler: Arc<dyn StreamHandler> = Arc::new(cc_api::streaming::NullStreamHandler);
    let mut rx = client.create_message_stream(request, handler).await?;
    let mut acc = StreamAccumulator::new();

    while let Some(evt) = rx.recv().await {
        acc.on_event(&evt);
        if matches!(evt, StreamEvent::MessageStop) {
            break;
        }
    }

    let (summary_msg, _usage, _stop) = acc.finish();
    let raw_summary = summary_msg.get_all_text();

    if raw_summary.is_empty() {
        return Err(ClaudeError::Other("Compact summary was empty".to_string()));
    }

    let formatted_summary = format_compact_summary(&raw_summary);

    // Build the new conversation:
    //   [user: compact summary preamble] [recent tail messages]
    let compact_notice = Message::user(format!(
        "This session is being continued from a previous conversation that ran out of context. \
         The summary below covers the earlier portion of the conversation (originally {} messages, \
         ~{} tokens).\n\n{}",
        original_count, original_token_estimate, formatted_summary
    ));

    let mut new_messages = vec![compact_notice];
    new_messages.extend_from_slice(&messages[split_at..]);

    Ok(new_messages)
}

/// Compact `messages` in-place, replacing the head with a summary.
/// Returns the new messages vector on success.
///
/// Uses the default `KEEP_RECENT_MESSAGES` constant. Callers that need a
/// configurable "keep recent" count (e.g. via `--compact-keep-recent-messages`)
/// should call [`compact_conversation_with_keep`] instead.
pub async fn compact_conversation(
    client: &dyn cc_api::LlmProvider,
    messages: &[Message],
    model: &str,
    summary_cap: u32,
) -> Result<Vec<Message>, ClaudeError> {
    compact_conversation_with_keep(client, messages, model, KEEP_RECENT_MESSAGES, summary_cap).await
}

/// Configurable variant of [`compact_conversation`] that lets the caller
/// pick how many recent messages survive verbatim
/// (mirrors `Config::effective_compact_keep_recent_messages`).
pub async fn compact_conversation_with_keep(
    client: &dyn cc_api::LlmProvider,
    messages: &[Message],
    model: &str,
    keep_recent: usize,
    summary_cap: u32,
) -> Result<Vec<Message>, ClaudeError> {
    let total = messages.len();

    if total <= keep_recent + 1 {
        debug!(total, "Too few messages to compact – keeping everything");
        return Ok(messages.to_vec());
    }

    // Split: summarise everything except the most recent `keep_recent` messages.
    let split_at = total.saturating_sub(keep_recent);

    info!(
        total,
        split_at,
        keep = keep_recent,
        "Compacting conversation"
    );

    // Cap summary tokens to the model's output limit (Qwen3=16K, DeepSeek=64K),
    // bounded by the user-configurable summary cap.
    let max_summary = client.max_output_tokens(model).min(summary_cap);
    summarise_head(client, messages, split_at, model, max_summary).await
}

/// Auto-compact `messages` if needed.  Updates `state` in place.
/// Returns `Some(new_messages)` if compaction ran, `None` otherwise.
///
/// Uses the default `KEEP_RECENT_MESSAGES` constant. For a configurable
/// keep-recent count use [`auto_compact_if_needed_with_keep`].
pub async fn auto_compact_if_needed(
    client: &dyn cc_api::LlmProvider,
    messages: &[Message],
    input_tokens: u64,
    model: &str,
    state: &mut AutoCompactState,
    context_window: u64,
    summary_cap: u32,
) -> Option<Vec<Message>> {
    auto_compact_if_needed_with_keep(
        client,
        messages,
        input_tokens,
        model,
        state,
        context_window,
        KEEP_RECENT_MESSAGES,
        summary_cap,
    )
    .await
}

/// Configurable variant of [`auto_compact_if_needed`].
#[allow(clippy::too_many_arguments)]
pub async fn auto_compact_if_needed_with_keep(
    client: &dyn cc_api::LlmProvider,
    messages: &[Message],
    input_tokens: u64,
    model: &str,
    state: &mut AutoCompactState,
    context_window: u64,
    keep_recent: usize,
    summary_cap: u32,
) -> Option<Vec<Message>> {
    auto_compact_if_needed_with_keep_and_limit(
        client,
        messages,
        input_tokens,
        model,
        state,
        context_window,
        keep_recent,
        summary_cap,
        MAX_CONSECUTIVE_FAILURES,
    )
    .await
}

/// Fully-configurable variant of [`auto_compact_if_needed_with_keep`].
/// `max_retries` opens the circuit breaker after this many consecutive
/// compaction failures. Plumbed by the query loop from
/// `Config::effective_max_compact_retries()` (CLI: --max-compact-retries).
///
/// Uses the default auto-compact trigger fraction. Prefer
/// [`auto_compact_if_needed_full`] to override the trigger fraction with the
/// `Config`-supplied value.
#[allow(clippy::too_many_arguments)]
pub async fn auto_compact_if_needed_with_keep_and_limit(
    client: &dyn cc_api::LlmProvider,
    messages: &[Message],
    input_tokens: u64,
    model: &str,
    state: &mut AutoCompactState,
    context_window: u64,
    keep_recent: usize,
    summary_cap: u32,
    max_retries: u32,
) -> Option<Vec<Message>> {
    auto_compact_if_needed_full(
        client,
        messages,
        input_tokens,
        model,
        state,
        context_window,
        keep_recent,
        summary_cap,
        max_retries,
        AUTOCOMPACT_TRIGGER_FRACTION,
    )
    .await
}

/// Fully-parameterised variant: also takes `trigger_fraction`.
#[allow(clippy::too_many_arguments)]
pub async fn auto_compact_if_needed_full(
    client: &dyn cc_api::LlmProvider,
    messages: &[Message],
    input_tokens: u64,
    model: &str,
    state: &mut AutoCompactState,
    context_window: u64,
    keep_recent: usize,
    summary_cap: u32,
    max_retries: u32,
    trigger_fraction: f64,
) -> Option<Vec<Message>> {
    if !should_auto_compact_with(input_tokens, context_window, state, trigger_fraction) {
        return None;
    }

    info!(
        input_tokens,
        model,
        compaction_count = state.compaction_count,
        "Auto-compact triggered"
    );

    match compact_conversation_with_keep(client, messages, model, keep_recent, summary_cap).await {
        Ok(new_msgs) => {
            state.on_success();
            info!(
                original_count = messages.len(),
                new_count = new_msgs.len(),
                "Auto-compact complete"
            );
            Some(new_msgs)
        }
        Err(e) => {
            warn!(error = %e, "Auto-compact failed");
            state.on_failure_with_limit(max_retries);
            None
        }
    }
}

// ---------------------------------------------------------------------------
// Reactive Compact (T1-1) — fires on usage data, not after turn end
// ---------------------------------------------------------------------------
//
// The TypeScript source uses a `ReactiveCompact` class with GrowthBook
// feature flags and a subscription to the streaming API's token-usage
// events.  In the Rust port we model the same behaviour with plain async
// functions and an env-var feature gate (`CLAUDE_REACTIVE_COMPACT=1`).
//
// Phase overview (mirrors reactiveCompact.ts):
//   1. Check usage with `should_compact` / `should_context_collapse`.
//   2. Strip image blocks from the conversation before compacting
//      (reduces the size of the prompt sent to the summariser).
//   3. Call `summarise_head` to generate a compact summary.
//   4. Re-inject recently-modified files (up to 5) as context.
//      (In the Rust port this phase is a no-op stub — the TUI layer owns
//      file-tracking; this file intentionally avoids the filesystem.)

/// Trigger classification for reactive compact.
#[derive(Debug, Clone)]
pub enum CompactTrigger {
    /// Normal 90 %-threshold compact.
    TokenThreshold {
        tokens_used: u64,
        context_limit: u64,
    },
    /// Caller requested an unconditional compact.
    Forced,
}

/// Result returned by `reactive_compact` and `context_collapse`.
#[derive(Debug, Clone)]
pub struct CompactResult {
    /// The new (reduced) message list.
    pub messages: Vec<cc_core::types::Message>,
    /// Formatted summary text injected at the head of `messages`.
    pub summary: String,
    /// Rough estimate of how many tokens were freed.
    pub tokens_freed: u64,
}

/// Return `true` when reactive compact should fire (≥ 90 % of context window).
///
/// Threshold is intentionally identical to `AUTOCOMPACT_TRIGGER_FRACTION` so
/// that exactly one of the two paths (proactive auto-compact vs reactive
/// compact) fires, chosen by the `CLAUDE_REACTIVE_COMPACT` gate.
pub fn should_compact(tokens_used: u64, context_limit: u64) -> bool {
    should_compact_with(tokens_used, context_limit, REACTIVE_COMPACT_THRESHOLD)
}

/// Threshold-parameterised variant of [`should_compact`].
/// `threshold_pct` mirrors `Config::reactive_compact_threshold`
/// (CLI: `--reactive-compact-threshold`).
pub fn should_compact_with(tokens_used: u64, context_limit: u64, threshold_pct: f64) -> bool {
    if context_limit == 0 {
        return false;
    }
    let threshold = (context_limit as f64 * threshold_pct) as u64;
    tokens_used >= threshold
}

/// Return `true` when the emergency context-collapse should fire.
///
/// Context-collapse is a last-resort measure: it produces an ultra-short
/// summary and keeps only the most recent user turn so that the next API call
/// can succeed even when the conversation is severely over-limit.
///
/// `threshold_override` lets the caller plumb a user-configured fraction;
/// passing `None` falls back to `CONTEXT_COLLAPSE_THRESHOLD` (the historical
/// 0.99 default).
pub fn should_context_collapse(
    tokens_used: u64,
    context_limit: u64,
    threshold_override: Option<f64>,
) -> bool {
    if context_limit == 0 {
        return false;
    }
    let frac = threshold_override.unwrap_or(CONTEXT_COLLAPSE_THRESHOLD);
    let threshold = (context_limit as f64 * frac) as u64;
    tokens_used >= threshold
}

/// Snip the middle of the conversation, keeping:
///   - the first message (usually the system/context bootstrap), and
///   - the `keep_n_newest` most-recent messages.
///
/// Returns `(new_messages, rough_tokens_freed)`.
///
/// Mirrors `snipCompact` from TypeScript (no API call required — purely local).
pub fn snip_compact(
    messages: Vec<cc_core::types::Message>,
    keep_n_newest: usize,
) -> (Vec<cc_core::types::Message>, u64) {
    let total = messages.len();
    if total <= keep_n_newest + 1 {
        // Nothing to snip.
        return (messages, 0);
    }

    // Keep: messages[0] (first/system message) + messages[total-keep_n_newest..]
    let snip_start = 1usize;
    let snip_end = total.saturating_sub(keep_n_newest);

    if snip_start >= snip_end {
        return (messages, 0);
    }

    // Estimate how many tokens the snipped range held.
    let snipped_tokens = estimate_tokens_for_messages(&messages[snip_start..snip_end]) as u64;

    let mut result = Vec::with_capacity(1 + keep_n_newest);
    result.push(messages[0].clone());
    result.extend_from_slice(&messages[snip_end..]);

    (result, snipped_tokens)
}

/// Compute the index into `messages` such that the tail starting at that
/// index fits within `token_budget` tokens.
///
/// Returns the cut index (0 = keep everything, messages.len() = keep nothing).
/// Iterates from the newest message backwards, accumulating token estimates
/// until the budget is exhausted.
pub fn calculate_messages_to_keep_index(
    messages: &[cc_core::types::Message],
    token_budget: u64,
) -> usize {
    if messages.is_empty() {
        return 0;
    }

    let mut accumulated: u64 = 0;
    let mut keep_from = messages.len(); // default: keep nothing (index past end)

    for (i, msg) in messages.iter().enumerate().rev() {
        let est = estimate_tokens_for_messages(std::slice::from_ref(msg)) as u64;
        if accumulated + est > token_budget {
            // This message would push us over budget — stop here.
            keep_from = i + 1;
            break;
        }
        accumulated += est;
        keep_from = i;
    }

    keep_from
}

/// Remove image and document blocks from a message list before compacting.
///
/// Vision payload tokens are expensive and carry no information that a text
/// summary needs. This walks two levels:
///
/// 1. Top-level user/assistant blocks (e.g. images attached to a prompt).
/// 2. Nested `ToolResult` payloads — since commit 4, tool results can carry
///    `ToolResultContent::Blocks` with embedded Image / Document blocks
///    (multimodal file ingestion). Strip those too, replacing the structured
///    content with its textual fallback if present, or a placeholder
///    otherwise. Otherwise reactive_compact would still pay the image-token
///    cost in the summarisation API call.
///
/// Mirrors the TypeScript `stripImages` helper used inside
/// `reactiveCompact.ts`, but extended for nested tool_result payloads.
pub(crate) fn strip_images(messages: Vec<cc_core::types::Message>) -> Vec<cc_core::types::Message> {
    use cc_core::types::{ContentBlock, MessageContent, ToolResultContent};

    fn is_visual(b: &ContentBlock) -> bool {
        matches!(
            b,
            ContentBlock::Image { .. } | ContentBlock::Document { .. }
        )
    }

    /// Collapse the structured tool_result blocks down to plain text.
    /// Preserves any leading `Text` blocks (the textual caption written by
    /// the tool) and drops Image / Document payloads.
    fn flatten_tool_result_blocks(blocks: &[ContentBlock]) -> String {
        let captions: Vec<&str> = blocks
            .iter()
            .filter_map(|b| match b {
                ContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        if captions.is_empty() {
            "[image removed for compaction]".to_string()
        } else {
            captions.join("\n")
        }
    }

    messages
        .into_iter()
        .map(|mut msg| {
            if let MessageContent::Blocks(ref mut blocks) = msg.content {
                // 1. Strip top-level visual blocks.
                blocks.retain(|b| !is_visual(b));

                // 2. Recurse into ToolResult.Blocks payloads.
                for b in blocks.iter_mut() {
                    if let ContentBlock::ToolResult {
                        content: ToolResultContent::Blocks(inner),
                        ..
                    } = b
                    {
                        if inner.iter().any(is_visual) {
                            let text_fallback = flatten_tool_result_blocks(inner);
                            // Replace the structured content with its
                            // textual fallback — providers can still read
                            // it post-compact, and we no longer pay the
                            // image-token cost.
                            if let ContentBlock::ToolResult { content, .. } = b {
                                *content = ToolResultContent::Text(text_fallback);
                            }
                        }
                    }
                }

                // If stripping left only an empty block list, collapse to a
                // placeholder text so the conversation remains parseable.
                if blocks.is_empty() {
                    msg.content =
                        MessageContent::Text("[image removed for compaction]".to_string());
                }
            }
            msg
        })
        .collect()
}

/// Run reactive compact: summarise the oldest messages and return a trimmed
/// conversation.
///
/// Feature gate: only call this when
/// `cc_core::feature_gates::is_feature_enabled("reactive_compact")` is true.
///
/// The `cancel` token is checked before the API call so the user can abort
/// a long-running compact.
pub async fn reactive_compact(
    messages: Vec<cc_core::types::Message>,
    client: &dyn cc_api::LlmProvider,
    config: &crate::QueryConfig,
    cancel: tokio_util::sync::CancellationToken,
    recently_modified: &[std::path::PathBuf],
) -> Result<CompactResult, cc_core::error::ClaudeError> {
    if cancel.is_cancelled() {
        return Err(cc_core::error::ClaudeError::Cancelled);
    }

    let total = messages.len();
    if total == 0 {
        return Ok(CompactResult {
            messages: vec![],
            summary: String::new(),
            tokens_freed: 0,
        });
    }

    // Phase 2: strip images before the compact API call.
    let stripped = strip_images(messages.clone());

    // Phase 1 + 3: summarise the head (all but the most recent
    // `compact_keep_recent_messages`), then replace the old head with the
    // summary message.
    let keep_recent = config.compact_keep_recent_messages;
    let split_at = total.saturating_sub(keep_recent);
    if split_at == 0 {
        // Too few messages; nothing to summarise.
        return Ok(CompactResult {
            messages,
            summary: String::new(),
            tokens_freed: 0,
        });
    }

    let original_token_estimate = estimate_tokens_for_messages(&stripped[..split_at]) as u64;

    let max_summary = client
        .max_output_tokens(&config.model)
        .min(config.compact_summary_max_tokens);
    let mut new_messages =
        summarise_head(client, &stripped, split_at, &config.model, max_summary).await?;

    // The summary lives as the first message in new_messages.
    let summary_text = new_messages
        .first()
        .map(|m| m.get_all_text())
        .unwrap_or_default();

    // Phase 4: re-inject recently modified file context. Caps come from
    // `Config::compact_reinject_max_files` /
    // `Config::compact_reinject_max_file_bytes` (CLI:
    // --compact-reinject-max-files / --compact-reinject-max-file-bytes).
    let max_files = config.compact_reinject_max_files;
    let max_file_bytes = config.compact_reinject_max_file_bytes;
    let mut injected = 0;
    for path in recently_modified.iter().take(max_files.saturating_mul(3)) {
        if injected >= max_files {
            break;
        }
        let meta = match std::fs::metadata(path) {
            Ok(m) => m,
            Err(_) => continue,
        };
        if meta.len() > max_file_bytes {
            continue;
        }
        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(_) => continue,
        };
        let file_name = path.display().to_string();
        let text = format!("<file path=\"{}\">\n{}\n</file>", file_name, content);
        new_messages.push(cc_core::types::Message::user(text));
        injected += 1;
    }

    let tokens_after = estimate_tokens_for_messages(&new_messages) as u64;
    let tokens_freed = original_token_estimate.saturating_sub(tokens_after);

    Ok(CompactResult {
        messages: new_messages,
        summary: summary_text,
        tokens_freed,
    })
}

/// Emergency context collapse: produce an ultra-short summary that distils
/// the entire conversation into the minimum needed to continue, then keep
/// only the most recent user turn.
///
/// Use only when `should_context_collapse()` returns `true` — i.e. the
/// context is at ≥ 97 % capacity and a regular reactive compact is unlikely
/// to free enough space.
pub async fn context_collapse(
    messages: Vec<cc_core::types::Message>,
    client: &dyn cc_api::LlmProvider,
    config: &crate::QueryConfig,
) -> Result<CompactResult, cc_core::error::ClaudeError> {
    use cc_api::{
        ApiMessage, CreateMessageRequest, StreamAccumulator, StreamEvent, StreamHandler,
        SystemPrompt,
    };
    use serde_json::Value;
    use std::sync::Arc;

    let total = messages.len();
    if total == 0 {
        return Ok(CompactResult {
            messages: vec![],
            summary: String::new(),
            tokens_freed: 0,
        });
    }

    let original_tokens = estimate_tokens_for_messages(&messages) as u64;

    // Build a concise transcript for the collapse prompt.
    let mut transcript = String::new();
    for msg in &messages {
        let role = match msg.role {
            cc_core::types::Role::User => "Human",
            cc_core::types::Role::Assistant => "Assistant",
        };
        let text = msg.get_all_text();
        if !text.is_empty() {
            transcript.push_str(&format!("{}: {}\n\n", role, text));
        }
    }

    let collapse_prompt = format!(
        "EMERGENCY CONTEXT COLLAPSE — the conversation is at critical capacity.\n\
         Produce an ULTRA-SHORT (max 500 words) emergency summary that captures:\n\
         1. The user's most recent explicit request.\n\
         2. The single most important decision made so far.\n\
         3. Any file names or code snippets that are ESSENTIAL to continue.\n\
         4. What was being worked on immediately before this collapse.\n\
         Respond with plain text only — no XML tags, no tool calls.\n\n\
         <conversation>\n{}\n</conversation>",
        transcript
    );

    let api_msgs = vec![ApiMessage {
        role: "user".to_string(),
        content: Value::String(collapse_prompt),
    }];

    let request = CreateMessageRequest::builder(&config.model, 1_000)
        .messages(api_msgs)
        .system(SystemPrompt::Text(
            "You are a conversation summariser. Produce an emergency ultra-short \
             summary as instructed. Plain text only."
                .to_string(),
        ))
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

    let (summary_msg, _usage, _stop) = acc.finish();
    let summary_text = summary_msg.get_all_text();

    if summary_text.is_empty() {
        return Err(cc_core::error::ClaudeError::Other(
            "Context-collapse summary was empty".to_string(),
        ));
    }

    // Keep only: the synthetic summary + the most recent user turn.
    let collapse_notice = cc_core::types::Message::user(format!(
        "[EMERGENCY CONTEXT COLLAPSE — conversation condensed to stay within limits]\n\n{}",
        summary_text
    ));

    // Find the last user message in the original list.
    let last_user = messages
        .iter()
        .rev()
        .find(|m| m.role == cc_core::types::Role::User)
        .cloned();

    let mut new_messages = vec![collapse_notice];
    if let Some(last) = last_user {
        new_messages.push(last);
    }

    let tokens_after = estimate_tokens_for_messages(&new_messages) as u64;
    let tokens_freed = original_tokens.saturating_sub(tokens_after);

    Ok(CompactResult {
        messages: new_messages,
        summary: summary_text,
        tokens_freed,
    })
}

// Threshold constants for reactive compact / context-collapse.
/// Reactive compact fires at 95 % of the context window.
const REACTIVE_COMPACT_THRESHOLD: f64 = 0.95;
/// Context collapse (emergency) fires at 99 % of the context window.
const CONTEXT_COLLAPSE_THRESHOLD: f64 = 0.99;

// ---------------------------------------------------------------------------
// T4-5: Collapse read/search results (mirrors src/utils/collapseReadSearch.ts)
// ---------------------------------------------------------------------------

/// Replace repeated reads of the same file with a single summary.
///
/// When the same file is read more than once in the conversation, replaces
/// all but the last read with `[Content shown N time(s); showing last occurrence only]`.
pub fn collapse_read_tool_results(
    messages: Vec<cc_core::types::Message>,
    fingerprint_chars: usize,
) -> Vec<cc_core::types::Message> {
    use cc_core::types::{ContentBlock, MessageContent, ToolResultContent};
    use std::collections::HashMap;

    // Helper: extract a fingerprint string from ToolResultContent.
    let fingerprint = |content: &ToolResultContent| -> Option<String> {
        match content {
            ToolResultContent::Text(t) => Some(t.chars().take(fingerprint_chars).collect()),
            ToolResultContent::Blocks(_) => None,
        }
    };

    // First pass: find all file-read tool results and count by fingerprint.
    let mut read_counts: HashMap<String, usize> = HashMap::new();
    for msg in &messages {
        if let MessageContent::Blocks(blocks) = &msg.content {
            for block in blocks {
                if let ContentBlock::ToolResult { content, .. } = block {
                    if let Some(key) = fingerprint(content) {
                        *read_counts.entry(key).or_insert(0) += 1;
                    }
                }
            }
        }
    }

    // Second pass: replace intermediate (non-last) occurrences.
    let mut seen: HashMap<String, usize> = HashMap::new();
    messages
        .into_iter()
        .map(|mut msg| {
            if let MessageContent::Blocks(ref mut blocks) = msg.content {
                for block in blocks.iter_mut() {
                    if let ContentBlock::ToolResult { content, .. } = block {
                        if let Some(key) = fingerprint(content) {
                            let count = read_counts.get(&key).copied().unwrap_or(1);
                            if count > 1 {
                                let seen_count = seen.entry(key.clone()).or_insert(0);
                                *seen_count += 1;
                                if *seen_count < count {
                                    // Replace intermediate occurrences.
                                    *content = ToolResultContent::Text(format!(
                                        "[Content shown {} time(s); showing last occurrence only]",
                                        count
                                    ));
                                }
                            }
                        }
                    }
                }
            }
            msg
        })
        .collect()
}

/// Deduplicate grep/glob search results that appear multiple times.
///
/// If the same search was run more than once (same query), keep only the
/// most recent result; replace earlier results with a truncation notice.
pub fn collapse_search_results(
    messages: Vec<cc_core::types::Message>,
    fingerprint_chars: usize,
) -> Vec<cc_core::types::Message> {
    use cc_core::types::{ContentBlock, MessageContent, ToolResultContent};
    use std::collections::HashSet;

    let fingerprint = |content: &ToolResultContent| -> Option<String> {
        match content {
            ToolResultContent::Text(t) => Some(t.chars().take(fingerprint_chars).collect()),
            ToolResultContent::Blocks(_) => None,
        }
    };

    let mut seen_results: HashSet<String> = HashSet::new();

    // Iterate in reverse to keep the latest occurrence.
    let mut result: Vec<cc_core::types::Message> = messages
        .into_iter()
        .rev()
        .map(|mut msg| {
            if let MessageContent::Blocks(ref mut blocks) = msg.content {
                for block in blocks.iter_mut() {
                    if let ContentBlock::ToolResult { content, .. } = block {
                        if let Some(fp) = fingerprint(content) {
                            if !seen_results.insert(fp) {
                                *content = ToolResultContent::Text(
                                    "[Duplicate search result; content shown in a later turn]"
                                        .to_string(),
                                );
                            }
                        }
                    }
                }
            }
            msg
        })
        .collect();

    result.reverse();
    result
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use cc_core::types::Message;

    fn make_user(text: &str) -> Message {
        Message::user(text)
    }

    fn make_assistant(text: &str) -> Message {
        // No UUID set — relies on the no-UUID grouping path in group_messages_for_compact.
        Message::assistant(text)
    }

    // ---- TokenWarningState --------------------------------------------------

    #[test]
    fn test_warning_state_ok() {
        // 50 % of 200k = 100k tokens — should be Ok
        let state = calculate_token_warning_state(100_000, 200_000);
        assert_eq!(state, TokenWarningState::Ok);
    }

    #[test]
    fn test_warning_state_warning() {
        // 92 % of 200k = 184k tokens — should be Warning (threshold now 90%)
        let state = calculate_token_warning_state(184_000, 200_000);
        assert_eq!(state, TokenWarningState::Warning);
    }

    #[test]
    fn test_warning_state_critical() {
        // 99 % of 200k = 198k tokens — should be Critical (threshold now 98%)
        let state = calculate_token_warning_state(198_000, 200_000);
        assert_eq!(state, TokenWarningState::Critical);
    }

    #[test]
    fn test_warning_state_boundary_80pct() {
        // 80 % of 200k = 160k tokens — should be Ok now (threshold raised to 90%)
        let state = calculate_token_warning_state(160_000, 200_000);
        assert_eq!(state, TokenWarningState::Ok);
    }

    #[test]
    fn test_warning_state_boundary_95pct() {
        // 95 % of 200k = 190k tokens — should be Warning now (critical raised to 98%)
        let state = calculate_token_warning_state(190_000, 200_000);
        assert_eq!(state, TokenWarningState::Warning);
    }

    // ---- should_auto_compact ------------------------------------------------

    #[test]
    fn test_should_not_compact_when_disabled() {
        let state = AutoCompactState {
            disabled: true,
            ..Default::default()
        };
        assert!(!should_auto_compact(195_000, 200_000, &state));
    }

    #[test]
    fn test_should_compact_at_95pct() {
        let state = AutoCompactState::default();
        // 95 % of 200k = 190k — should trigger (threshold now 95%)
        assert!(should_auto_compact(190_000, 200_000, &state));
    }

    #[test]
    fn test_should_not_compact_below_90pct() {
        let state = AutoCompactState::default();
        // 70 % of 200k = 140k — should NOT trigger
        assert!(!should_auto_compact(140_000, 200_000, &state));
    }

    // ---- Circuit breaker ----------------------------------------------------

    #[test]
    fn test_circuit_breaker_opens_after_failures() {
        let mut state = AutoCompactState::default();
        assert!(!state.disabled);
        for _ in 0..MAX_CONSECUTIVE_FAILURES {
            state.on_failure();
        }
        assert!(state.disabled);
    }

    #[test]
    fn test_circuit_breaker_resets_on_success() {
        let mut state = AutoCompactState::default();
        state.on_failure();
        state.on_failure();
        state.on_success();
        assert_eq!(state.consecutive_failures, 0);
        assert!(!state.disabled);
    }

    // ---- Message grouping ---------------------------------------------------

    #[test]
    fn test_group_messages_simple() {
        let messages = vec![
            make_user("Hello"),
            make_assistant("Hi there"),
            make_user("How are you?"),
            make_assistant("I'm fine"),
        ];

        let groups = group_messages_for_compact(&messages);
        // Should produce 2 groups: one per assistant turn boundary
        assert_eq!(groups.len(), 2);
        // First group: user + first assistant
        assert_eq!(groups[0].messages.len(), 2);
        // Second group: second user + second assistant
        assert_eq!(groups[1].messages.len(), 2);
    }

    #[test]
    fn test_group_empty() {
        let groups = group_messages_for_compact(&[]);
        assert!(groups.is_empty());
    }

    #[test]
    fn test_group_only_user_messages() {
        // No assistant messages → everything in one group
        let messages = vec![make_user("A"), make_user("B"), make_user("C")];
        let groups = group_messages_for_compact(&messages);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].messages.len(), 3);
    }

    // ---- format_compact_summary --------------------------------------------

    #[test]
    fn test_format_strips_analysis() {
        let raw = "<analysis>This is scratchpad text.</analysis>\n\
                   <summary>This is the real content.</summary>";
        let formatted = format_compact_summary(raw);
        assert!(!formatted.contains("<analysis>"));
        assert!(!formatted.contains("scratchpad text"));
        assert!(formatted.contains("real content"));
    }

    #[test]
    fn test_format_replaces_summary_tags() {
        let raw = "<summary>Content here</summary>";
        let formatted = format_compact_summary(raw);
        assert!(!formatted.contains("<summary>"));
        assert!(formatted.contains("Summary:"));
        assert!(formatted.contains("Content here"));
    }

    #[test]
    fn test_format_passthrough_when_no_tags() {
        let raw = "Plain text summary without any XML tags.";
        let formatted = format_compact_summary(raw);
        assert_eq!(formatted, raw);
    }

    // ---- get_compact_prompt ------------------------------------------------

    #[test]
    fn test_compact_prompt_contains_no_tools_preamble() {
        let prompt = get_compact_prompt(None);
        assert!(prompt.contains("CRITICAL: Respond with TEXT ONLY"));
        assert!(prompt.contains("Do NOT call any tools"));
    }

    #[test]
    fn test_compact_prompt_contains_sections() {
        let prompt = get_compact_prompt(None);
        assert!(prompt.contains("Primary Request and Intent"));
        assert!(prompt.contains("Key Technical Concepts"));
        assert!(prompt.contains("Files and Code Sections"));
        assert!(prompt.contains("Errors and fixes"));
        assert!(prompt.contains("Pending Tasks"));
        assert!(prompt.contains("Current Work"));
    }

    #[test]
    fn test_compact_prompt_with_custom_instructions() {
        let prompt = get_compact_prompt(Some("Focus on Rust type system changes."));
        assert!(prompt.contains("Additional Instructions:"));
        assert!(prompt.contains("Focus on Rust type system changes."));
    }

    #[test]
    fn test_compact_prompt_empty_custom_instructions_ignored() {
        let prompt_none = get_compact_prompt(None);
        let prompt_empty = get_compact_prompt(Some("   "));
        assert_eq!(prompt_none, prompt_empty);
    }

    // context_window_for_model tests removed — context window now comes from provider.

    // ---- estimate_tokens_for_messages --------------------------------------

    #[test]
    fn test_token_estimate_nonempty() {
        let msgs = vec![make_user("Hello, world!")];
        let est = estimate_tokens_for_messages(&msgs);
        // "Hello, world!" = 13 chars → 13/4 = 3 rough tokens → 3*4/3 = 4 padded
        assert!(est > 0);
    }

    // ---- strip_images recursion into ToolResult.Blocks ---------------------
    //
    // Pin the invariant: structured tool_result payloads that carry image or
    // document blocks must be flattened to text before the compact API call.
    // Otherwise reactive_compact would pay the image-token cost on every
    // compact pass, defeating the point of compaction.

    fn image_block() -> cc_core::types::ContentBlock {
        cc_core::types::ContentBlock::Image {
            source: cc_core::types::ImageSource {
                source_type: "base64".to_string(),
                media_type: Some("image/png".to_string()),
                data: Some("iVBORw0KGgo=".to_string()),
                url: None,
            },
        }
    }

    fn user_with_blocks(blocks: Vec<cc_core::types::ContentBlock>) -> Message {
        use cc_core::types::{MessageContent, Role};
        Message {
            role: Role::User,
            content: MessageContent::Blocks(blocks),
            uuid: None,
            cost: None,
        }
    }

    #[test]
    fn strip_images_removes_top_level_image() {
        use cc_core::types::{ContentBlock, MessageContent};
        let msg = user_with_blocks(vec![
            ContentBlock::Text {
                text: "caption".to_string(),
            },
            image_block(),
        ]);
        let stripped = strip_images(vec![msg]);
        assert_eq!(stripped.len(), 1);
        if let MessageContent::Blocks(blocks) = &stripped[0].content {
            assert_eq!(blocks.len(), 1, "image must be removed");
            assert!(matches!(&blocks[0], ContentBlock::Text { .. }));
        } else {
            panic!("expected Blocks content, got {:?}", stripped[0].content);
        }
    }

    #[test]
    fn strip_images_flattens_tool_result_blocks_preserving_captions() {
        use cc_core::types::{ContentBlock, MessageContent, ToolResultContent};
        let inner = vec![
            ContentBlock::Text {
                text: "[chart.png — bar chart, Q1 sales]".to_string(),
            },
            image_block(),
        ];
        let msg = user_with_blocks(vec![ContentBlock::ToolResult {
            tool_use_id: "tu_1".to_string(),
            content: ToolResultContent::Blocks(inner),
            is_error: None,
        }]);
        let stripped = strip_images(vec![msg]);
        if let MessageContent::Blocks(blocks) = &stripped[0].content {
            match &blocks[0] {
                ContentBlock::ToolResult {
                    content: ToolResultContent::Text(t),
                    ..
                } => {
                    assert!(
                        t.contains("chart.png"),
                        "caption must survive flattening, got: {t}"
                    );
                }
                other => panic!("expected Text-flavored ToolResult, got {:?}", other),
            }
        } else {
            panic!("expected Blocks content");
        }
    }

    #[test]
    fn strip_images_leaves_text_only_tool_result_blocks_intact() {
        // A tool_result that carries only Text blocks (e.g. a structured table
        // dialect) should NOT be flattened — it's cheap, structure may matter.
        use cc_core::types::{ContentBlock, MessageContent, ToolResultContent};
        let inner = vec![
            ContentBlock::Text {
                text: "row 1".to_string(),
            },
            ContentBlock::Text {
                text: "row 2".to_string(),
            },
        ];
        let msg = user_with_blocks(vec![ContentBlock::ToolResult {
            tool_use_id: "tu_2".to_string(),
            content: ToolResultContent::Blocks(inner),
            is_error: None,
        }]);
        let stripped = strip_images(vec![msg]);
        if let MessageContent::Blocks(blocks) = &stripped[0].content {
            assert!(matches!(
                &blocks[0],
                ContentBlock::ToolResult {
                    content: ToolResultContent::Blocks(_),
                    ..
                }
            ));
        } else {
            panic!("expected Blocks content");
        }
    }

    #[test]
    fn strip_images_uses_placeholder_when_no_caption() {
        // Tool result that carried ONLY an image — no caption to preserve.
        // Must fall back to a placeholder string so providers can still parse it.
        use cc_core::types::{ContentBlock, MessageContent, ToolResultContent};
        let msg = user_with_blocks(vec![ContentBlock::ToolResult {
            tool_use_id: "tu_3".to_string(),
            content: ToolResultContent::Blocks(vec![image_block()]),
            is_error: None,
        }]);
        let stripped = strip_images(vec![msg]);
        if let MessageContent::Blocks(blocks) = &stripped[0].content {
            match &blocks[0] {
                ContentBlock::ToolResult {
                    content: ToolResultContent::Text(t),
                    ..
                } => {
                    assert!(
                        t.contains("image removed"),
                        "must emit placeholder, got: {t}"
                    );
                }
                other => panic!("expected Text-flavored ToolResult, got {:?}", other),
            }
        } else {
            panic!("expected Blocks content");
        }
    }
}
