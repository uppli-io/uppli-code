// openai_provider.rs — OpenAI-compatible LLM provider.
//
// Supports any API that speaks the OpenAI `/v1/chat/completions` format:
//   - Alibaba Cloud / DashScope (Qwen3-235B)
//   - Ollama (Gemma 4, Qwen3 local, etc.)
//   - Any OpenAI-compatible endpoint
//
// Translates between the internal Anthropic-style request format used by the
// query loop and the OpenAI wire protocol.  Streaming uses SSE with
// `data: [DONE]` termination (standard OpenAI) or NDJSON (Ollama).

use crate::provider::{ApiFormat, AuthConfig, LlmProvider, ModelMetadata, ProviderCapabilities};
use crate::streaming::{ContentDelta, StreamEvent, StreamHandler};
use crate::types::CreateMessageRequest;
use crate::{AvailableModel, CreateMessageResponse};
use async_trait::async_trait;
use cc_core::error::ClaudeError;
use cc_core::types::{ContentBlock, UsageInfo};
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tracing::{debug, warn};

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// Configuration for an OpenAI-compatible provider.
///
/// Each preset (ollama, alibaba, mistral, generic) provides full self-description:
/// models with metadata, auth config, attribution text.  Adding a new provider
/// = adding a new preset here + one match arm in the factory.
#[derive(Debug, Clone)]
pub struct OpenAiProviderConfig {
    pub name: String,
    pub api_base: String,
    pub api_key: String,
    pub default_model: String,
    pub fast_model: Option<String>,
    pub api_format: ApiFormat,
    pub max_retries: u32,
    pub request_timeout: Duration,
    // ── Self-description (new) ─────────────────────────────────
    /// Attribution for the system prompt (e.g., "powered by Qwen3 (Alibaba)").
    pub attribution: String,
    /// Known models with metadata (context window, limits, pricing).
    pub known_models: Vec<ModelMetadata>,
    /// Default max output tokens for this provider.
    pub default_max_tokens: u32,
    /// Whether the default model accepts image / document blocks.
    pub supports_vision: bool,
    /// Wire-level thinking dialect, mirrored from the TOML preset.
    /// `None` means no thinking on the wire — the provider's model
    /// decides on its own.
    pub thinking_format: Option<crate::provider::ThinkingFormat>,
    /// Authentication configuration.
    pub auth: AuthConfig,
}

impl OpenAiProviderConfig {
    /// Construct an OpenAiProviderConfig from a TOML-loaded provider, an
    /// API key, and an optional model override. This is the only way to
    /// build a config — the previous hardcoded preset functions (ollama,
    /// openrouter, alibaba, mistral, generic) were deleted in PR S.
    ///
    /// `model_override`: if Some, use it as default_model; otherwise use
    /// the model marked `default = true` in the TOML.
    pub fn from_loaded(
        loaded: &crate::providers::LoadedProvider,
        api_key: String,
        model_override: Option<String>,
    ) -> Self {
        let caps = &loaded.capabilities;
        let default_model = model_override.unwrap_or_else(|| caps.default_model.clone());
        Self {
            name: caps.display_name.clone(),
            api_base: caps.default_api_base.clone(),
            api_key,
            default_model,
            fast_model: caps.fast_model.clone(),
            api_format: caps.api_format,
            max_retries: loaded.max_retries,
            request_timeout: Duration::from_secs(loaded.request_timeout_sec),
            attribution: caps.attribution.clone(),
            known_models: caps.known_models.clone(),
            default_max_tokens: caps.default_max_tokens,
            supports_vision: caps.supports_vision,
            thinking_format: caps.thinking_format,
            auth: caps.auth,
        }
    }
}

// ---------------------------------------------------------------------------
// OpenAI request/response types
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
struct OpenAiRequest {
    model: String,
    messages: Vec<OpenAiMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    top_p: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stop: Option<Vec<String>>,
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<OpenAiTool>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    enable_thinking: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    thinking_budget: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    think: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    thinking: Option<crate::types::ThinkingConfig>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct OpenAiMessage {
    role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<OpenAiToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct OpenAiToolCall {
    id: String,
    #[serde(rename = "type")]
    call_type: String,
    function: OpenAiFunction,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct OpenAiFunction {
    name: String,
    /// OpenAI/standard: JSON-encoded string (`Value::String`).
    /// Ollama: JSON object (`Value::Object`).
    /// Using `Value` so serde serialises the right shape for each provider.
    arguments: Value,
}

#[derive(Debug, Serialize)]
struct OpenAiTool {
    #[serde(rename = "type")]
    tool_type: String,
    function: OpenAiToolFunction,
}

#[derive(Debug, Serialize)]
struct OpenAiToolFunction {
    name: String,
    description: String,
    parameters: Value,
}

/// A single streaming chunk from the OpenAI API.
#[derive(Debug, Deserialize)]
struct OpenAiStreamChunk {
    id: Option<String>,
    model: Option<String>,
    choices: Vec<OpenAiChoice>,
    usage: Option<OpenAiUsage>,
}

#[derive(Debug, Deserialize)]
struct OpenAiChoice {
    #[allow(dead_code)]
    index: Option<usize>,
    delta: Option<OpenAiDelta>,
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OpenAiDelta {
    #[allow(dead_code)]
    role: Option<String>,
    content: Option<String>,
    tool_calls: Option<Vec<OpenAiToolCallDelta>>,
    /// Qwen3: reasoning/thinking content.
    reasoning_content: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OpenAiToolCallDelta {
    index: Option<usize>,
    id: Option<String>,
    #[serde(rename = "type")]
    #[allow(dead_code)]
    call_type: Option<String>,
    function: Option<OpenAiFunctionDelta>,
}

#[derive(Debug, Deserialize)]
struct OpenAiFunctionDelta {
    name: Option<String>,
    arguments: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OpenAiUsage {
    prompt_tokens: Option<u64>,
    completion_tokens: Option<u64>,
    #[allow(dead_code)]
    total_tokens: Option<u64>,
}

/// Ollama NDJSON response chunk (different from OpenAI SSE).
#[derive(Debug, Deserialize)]
struct OllamaChatChunk {
    model: Option<String>,
    message: Option<OllamaChatMessage>,
    done: Option<bool>,
    eval_count: Option<u64>,
    prompt_eval_count: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct OllamaChatMessage {
    #[allow(dead_code)]
    role: Option<String>,
    content: Option<String>,
    /// Ollama Qwen3 thinking content (when `think: true` is enabled).
    thinking: Option<String>,
    /// Ollama tool calls — arguments are a pre-parsed JSON object (unlike OpenAI which sends a string).
    tool_calls: Option<Vec<OllamaToolCall>>,
}

#[derive(Debug, Deserialize)]
struct OllamaToolCall {
    function: Option<OllamaToolFunction>,
}

#[derive(Debug, Deserialize)]
struct OllamaToolFunction {
    name: Option<String>,
    /// Already-parsed JSON object (Ollama sends objects, not strings like OpenAI).
    arguments: Option<Value>,
}

// ---------------------------------------------------------------------------
// The provider implementation
// ---------------------------------------------------------------------------

pub struct OpenAiProvider {
    http: reqwest::Client,
    config: OpenAiProviderConfig,
    capabilities: ProviderCapabilities,
}

impl OpenAiProvider {
    pub fn new(config: OpenAiProviderConfig) -> anyhow::Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(config.request_timeout)
            .build()?;

        let capabilities = ProviderCapabilities {
            name: config.name.clone(),
            display_name: config.name.clone(),
            attribution: config.attribution.clone(),
            default_model: config.default_model.clone(),
            fast_model: config.fast_model.clone(),
            known_models: config.known_models.clone(),
            default_max_tokens: config.default_max_tokens,
            api_format: config.api_format,
            default_api_base: config.api_base.clone(),
            supports_vision: config.supports_vision,
            thinking_format: config.thinking_format,
            auth: config.auth,
        };

        Ok(Self {
            http,
            config,
            capabilities,
        })
    }

    /// Translate our internal Anthropic-format request to OpenAI format.
    fn translate_request(&self, req: &CreateMessageRequest) -> OpenAiRequest {
        let mut messages = Vec::new();

        // System prompt → system message
        if let Some(ref system) = req.system {
            let text = match system {
                crate::types::SystemPrompt::Text(t) => t.clone(),
                crate::types::SystemPrompt::Blocks(blocks) => blocks
                    .iter()
                    .map(|b| b.text.as_str())
                    .collect::<Vec<_>>()
                    .join("\n\n"),
            };
            messages.push(OpenAiMessage {
                role: "system".to_string(),
                content: Some(Value::String(text)),
                tool_calls: None,
                tool_call_id: None,
                name: None,
            });
        }

        // Convert conversation messages
        for api_msg in &req.messages {
            let converted = self.translate_message(api_msg);
            messages.extend(converted);
        }

        // Convert tools
        let tools = req.tools.as_ref().map(|api_tools| {
            api_tools
                .iter()
                .map(|t| OpenAiTool {
                    tool_type: "function".to_string(),
                    function: OpenAiToolFunction {
                        name: t.name.clone(),
                        description: t.description.clone(),
                        parameters: t.input_schema.clone(),
                    },
                })
                .collect()
        });

        // Mirror each provider's upstream thinking dialect verbatim.
        use crate::provider::ThinkingFormat;
        let user_wants_thinking = req.thinking.is_some();
        let budget = req.thinking.as_ref().map(|t| t.budget_tokens);

        let (enable_thinking, thinking_budget, think, thinking_nested) =
            match (self.config.thinking_format, user_wants_thinking) {
                (Some(ThinkingFormat::Qwen3), _) => (
                    Some(user_wants_thinking),
                    if user_wants_thinking { budget } else { None },
                    None,
                    None,
                ),
                (Some(ThinkingFormat::OllamaThink), _) => {
                    (None, None, Some(user_wants_thinking), None)
                }
                (Some(ThinkingFormat::AnthropicNested), true) => {
                    (None, None, None, req.thinking.clone())
                }
                _ => (None, None, None, None),
            };

        // Clamp max_tokens to the model's output limit (from known_models metadata).
        let model_max = self
            .config
            .known_models
            .iter()
            .find(|m| m.id == req.model)
            .map(|m| m.max_output_tokens)
            .unwrap_or(self.config.default_max_tokens);
        let clamped_max_tokens = req.max_tokens.min(model_max);

        OpenAiRequest {
            model: req.model.clone(),
            messages,
            max_tokens: Some(clamped_max_tokens),
            temperature: req.temperature,
            top_p: req.top_p,
            stop: req.stop_sequences.clone(),
            stream: true,
            tools,
            enable_thinking,
            thinking_budget,
            think,
            thinking: thinking_nested,
        }
    }

    /// Translate a single Anthropic API message to one or more OpenAI messages.
    ///
    /// A single Anthropic assistant message with tool_use blocks produces:
    ///   1. An assistant message with tool_calls
    ///
    /// A user message with tool_result blocks produces:
    ///   1. One "tool" message per result
    fn translate_message(&self, msg: &crate::types::ApiMessage) -> Vec<OpenAiMessage> {
        let role = &msg.role;

        // Simple string content
        if let Some(text) = msg.content.as_str() {
            return vec![OpenAiMessage {
                role: role.clone(),
                content: Some(Value::String(text.to_string())),
                tool_calls: None,
                tool_call_id: None,
                name: None,
            }];
        }

        // Array of content blocks (Anthropic format)
        if let Some(blocks) = msg.content.as_array() {
            let mut result = Vec::new();
            let mut text_parts = Vec::new();
            // Image parts as OpenAI vision-format parts:
            //   {"type": "image_url", "image_url": {"url": "data:<media>;base64,<data>"}}
            // Used when the target provider supports OpenAI-compatible vision
            // (GLM-4.6v, Qwen-VL, etc.). Falls back to dropping for providers
            // that don't (we don't try to OCR client-side).
            let mut image_parts: Vec<Value> = Vec::new();
            let mut tool_calls = Vec::new();
            // (tool_use_id, text_parts, image_parts) — split so we can emit
            // either a flat string (text-only providers) or an OpenAI vision
            // multi-part Array (vision-capable providers like GLM-4.6v).
            // (tool_use_id, text_parts, image_parts, document_count)
            let mut tool_results: Vec<(String, Vec<String>, Vec<Value>, usize)> = Vec::new();
            // Top-level document blocks (outside tool_result) — counted
            // separately so the user-message branch can surface an error.
            let mut top_level_document_count: usize = 0;

            for block in blocks {
                let block_type = block.get("type").and_then(|t| t.as_str()).unwrap_or("");

                match block_type {
                    "text" => {
                        if let Some(t) = block.get("text").and_then(|t| t.as_str()) {
                            text_parts.push(t.to_string());
                        }
                    }
                    "image" => {
                        // Anthropic image block → OpenAI vision `image_url` part.
                        // Vision-capable providers (GLM-4.5v, Qwen-VL, GPT-4o)
                        // accept the data: URI / remote URL via this shape.
                        if let Some(url) = image_url_from_source(block_type, block) {
                            image_parts.push(serde_json::json!({
                                "type": "image_url",
                                "image_url": { "url": url }
                            }));
                        }
                    }
                    "document" => {
                        // PDF blocks at the top level of a user message.
                        // z.ai live: forwarding via image_url returns
                        // `API error 400: 图片输入格式/解析错误`. Track the
                        // count; the user-message branch downstream
                        // surfaces an explicit error per the iso UX
                        // contract (same as AnthropicClient flipping
                        // is_error=true on a non-vision provider).
                        top_level_document_count += 1;
                    }
                    "thinking" => {
                        // Thinking blocks from Anthropic format have no equivalent in
                        // OpenAI message format. Qwen3 uses `enable_thinking` param +
                        // `reasoning_content` field instead. Skip during translation.
                    }
                    "tool_use" => {
                        let id = block
                            .get("id")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        let name = block
                            .get("name")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        let input = block
                            .get("input")
                            .cloned()
                            .unwrap_or(Value::Object(Default::default()));
                        // Ollama expects arguments as a JSON object; OpenAI/others
                        // expect a JSON-encoded string.
                        let arguments = if self.config.api_format == ApiFormat::Ollama {
                            input
                        } else {
                            match serde_json::to_string(&input) {
                                Ok(a) => Value::String(a),
                                Err(e) => {
                                    warn!(error = %e, tool = %name, "Failed to serialize tool arguments");
                                    Value::String(format!("{:?}", input))
                                }
                            }
                        };
                        tool_calls.push(OpenAiToolCall {
                            id,
                            call_type: "function".to_string(),
                            function: OpenAiFunction { name, arguments },
                        });
                    }
                    "tool_result" => {
                        let tool_use_id = block
                            .get("tool_use_id")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        // Walk the inner content and split it into three
                        // channels: textual fallback (always populated),
                        // image parts (sent only when supports_vision), and
                        // document count (always surfaced as an error
                        // because the OpenAI image_url channel rejects
                        // application/pdf with a 400 regardless of caps).
                        let mut inner_text_parts: Vec<String> = Vec::new();
                        let mut inner_image_parts: Vec<Value> = Vec::new();
                        let mut inner_document_count: usize = 0;

                        if let Some(c) = block.get("content") {
                            if let Some(s) = c.as_str() {
                                inner_text_parts.push(s.to_string());
                            } else if let Some(arr) = c.as_array() {
                                for inner in arr {
                                    let inner_type =
                                        inner.get("type").and_then(|v| v.as_str()).unwrap_or("");
                                    match inner_type {
                                        "text" => {
                                            if let Some(t) =
                                                inner.get("text").and_then(|v| v.as_str())
                                            {
                                                inner_text_parts.push(t.to_string());
                                            }
                                        }
                                        "image" => {
                                            if let Some(url) =
                                                image_url_from_source(inner_type, inner)
                                            {
                                                inner_image_parts.push(serde_json::json!({
                                                    "type": "image_url",
                                                    "image_url": { "url": url }
                                                }));
                                            }
                                        }
                                        "document" => {
                                            // PDF blocks rejected by the
                                            // OpenAI vision channel. Track
                                            // the count so we can surface
                                            // an explicit error downstream
                                            // — iso the AnthropicClient
                                            // rejection contract.
                                            inner_document_count += 1;
                                        }
                                        _ => {
                                            // Unknown nested block — fall back
                                            // to its text representation if any.
                                            if let Some(t) =
                                                inner.get("text").and_then(|v| v.as_str())
                                            {
                                                inner_text_parts.push(t.to_string());
                                            }
                                        }
                                    }
                                }
                            } else {
                                inner_text_parts.push(c.to_string());
                            }
                        }
                        tool_results.push((
                            tool_use_id,
                            inner_text_parts,
                            inner_image_parts,
                            inner_document_count,
                        ));
                    }
                    _ => {}
                }
            }

            // Emit assistant message (with optional tool_calls)
            if role == "assistant" {
                let tc = if tool_calls.is_empty() {
                    None
                } else {
                    Some(tool_calls)
                };
                // OpenAI API requires either content or tool_calls on assistant
                // messages. If thinking blocks were the only content (no text,
                // no tool_calls), emit an empty string so the API accepts it.
                let content = if !text_parts.is_empty() {
                    Some(Value::String(text_parts.join("")))
                } else if tc.is_some() {
                    None // tool_calls present, content can be null
                } else {
                    Some(Value::String(String::new())) // empty placeholder
                };
                result.push(OpenAiMessage {
                    role: "assistant".to_string(),
                    content,
                    tool_calls: tc,
                    tool_call_id: None,
                    name: None,
                });
            } else if !tool_results.is_empty() {
                // User message with tool results → emit as "tool" role messages.
                // Ollama doesn't use tool_call_id (no IDs in its tool call responses).
                let include_tool_call_id = self.config.api_format != ApiFormat::Ollama;
                let provider_supports_vision = self.capabilities.supports_vision;
                for (tool_use_id, text_parts, image_parts, document_count) in tool_results {
                    let text = text_parts.join("\n");
                    // Compute which failure modes apply:
                    //   * images present + !vision   → image error
                    //   * documents present (always) → document error
                    let rejected_image_count = if !provider_supports_vision {
                        image_parts.len()
                    } else {
                        0
                    };
                    let error_text =
                        unsupported_blocks_error_text(rejected_image_count, document_count);
                    let mut composed_text = text;
                    if !error_text.is_empty() {
                        if !composed_text.is_empty() && !composed_text.ends_with('\n') {
                            composed_text.push('\n');
                        }
                        composed_text.push_str(&error_text);
                    }
                    let content_value = if !image_parts.is_empty() && provider_supports_vision {
                        // Vision-capable provider — emit an OpenAI multi-part
                        // array with the textual content (+ any document
                        // rejection text appended) first, then each image
                        // part. Documents still produce an error even on
                        // vision providers because the wire format can't
                        // transmit them.
                        let mut parts: Vec<Value> = Vec::new();
                        if !composed_text.is_empty() {
                            parts
                                .push(serde_json::json!({ "type": "text", "text": composed_text }));
                        }
                        parts.extend(image_parts);
                        Value::Array(parts)
                    } else {
                        // No image parts forwarded — flat string is the
                        // legal shape. composed_text already carries any
                        // error text for rejected images and/or documents.
                        Value::String(composed_text)
                    };
                    result.push(OpenAiMessage {
                        role: "tool".to_string(),
                        content: Some(content_value),
                        tool_calls: None,
                        tool_call_id: if include_tool_call_id {
                            Some(tool_use_id)
                        } else {
                            None
                        },
                        name: None,
                    });
                }
            } else {
                // Regular user message with text and/or image / document blocks.
                let text = text_parts.join("");
                let has_images = !image_parts.is_empty();
                let provider_supports_vision = self.capabilities.supports_vision;
                let rejected_image_count = if !provider_supports_vision {
                    image_parts.len()
                } else {
                    0
                };
                let error_text =
                    unsupported_blocks_error_text(rejected_image_count, top_level_document_count);
                let mut composed_text = text;
                if !error_text.is_empty() {
                    if !composed_text.is_empty() && !composed_text.ends_with('\n') {
                        composed_text.push('\n');
                    }
                    composed_text.push_str(&error_text);
                }
                if has_images && provider_supports_vision {
                    let mut parts: Vec<Value> = Vec::new();
                    if !composed_text.is_empty() {
                        parts.push(serde_json::json!({ "type": "text", "text": composed_text }));
                    }
                    parts.extend(image_parts);
                    result.push(OpenAiMessage {
                        role: role.clone(),
                        content: Some(Value::Array(parts)),
                        tool_calls: None,
                        tool_call_id: None,
                        name: None,
                    });
                } else if !composed_text.is_empty() {
                    result.push(OpenAiMessage {
                        role: role.clone(),
                        content: Some(Value::String(composed_text)),
                        tool_calls: None,
                        tool_call_id: None,
                        name: None,
                    });
                }
            }

            return result;
        }

        // Fallback: pass content as-is
        vec![OpenAiMessage {
            role: role.clone(),
            content: Some(msg.content.clone()),
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }]
    }

    /// Parse OpenAI SSE stream into StreamEvents.
    ///
    /// State machine tracks which content blocks are open so the
    /// `StreamAccumulator` receives the correct Start/Delta/Stop sequence.
    async fn process_openai_sse(
        resp: reqwest::Response,
        handler: Arc<dyn StreamHandler>,
        tx: mpsc::Sender<StreamEvent>,
    ) -> Result<(), ClaudeError> {
        // Helper to emit + send in one call.
        macro_rules! emit {
            ($evt:expr) => {{
                let evt = $evt;
                handler.on_event(&evt);
                let _ = tx.send(evt).await;
            }};
        }

        let mut byte_stream = resp.bytes_stream();
        let mut leftover = String::new();
        let mut sent_start = false;

        // Block tracking — mirrors the Anthropic content_block indexing.
        let mut next_block: usize = 0; // next available block index
        let mut thinking_open = false; // is a Thinking block currently open?
        let mut text_open = false; // is a Text block currently open?
        let mut text_block_idx: usize = 0; // index of the open Text block
        let mut message_stopped = false; // guard against double MessageStop
                                         // tool_call OpenAI index → (block_index, id, name, args_buf)
        let mut tool_blocks: std::collections::HashMap<usize, (usize, String, String, String)> =
            std::collections::HashMap::new();

        while let Some(chunk_result) = byte_stream.next().await {
            let chunk = chunk_result.map_err(ClaudeError::Http)?;
            let text = String::from_utf8_lossy(&chunk);

            let combined = if leftover.is_empty() {
                text.to_string()
            } else {
                let mut s = std::mem::take(&mut leftover);
                s.push_str(&text);
                s
            };

            let mut lines: Vec<&str> = combined.split('\n').collect();
            if !combined.ends_with('\n') {
                leftover = lines.pop().unwrap_or("").to_string();
            }

            for line in lines {
                let line = line.trim();
                if line.is_empty() || line.starts_with(':') {
                    continue;
                }

                let data = match line.strip_prefix("data: ") {
                    Some(d) => d.trim(),
                    None => continue,
                };

                if data == "[DONE]" {
                    if !message_stopped {
                        emit!(StreamEvent::MessageStop);
                        // Not strictly needed (we return below) but keeps
                        // the invariant clean for anyone reading this code.
                        #[allow(unused_assignments)]
                        {
                            message_stopped = true;
                        }
                    }
                    return Ok(());
                }

                let chunk: OpenAiStreamChunk = match serde_json::from_str(data) {
                    Ok(c) => c,
                    Err(e) => {
                        // Log at warn level so SSE corruption is visible in logs.
                        // debug was too quiet — parsing failures silently dropped data.
                        warn!(error = %e, data = data, "Failed to parse SSE chunk — skipping");
                        continue;
                    }
                };

                // Emit MessageStart on the very first chunk.
                if !sent_start {
                    emit!(StreamEvent::MessageStart {
                        id: chunk.id.clone().unwrap_or_default(),
                        model: chunk.model.clone().unwrap_or_default(),
                        usage: UsageInfo::default(),
                    });
                    sent_start = true;
                }

                for choice in &chunk.choices {
                    if let Some(ref delta) = choice.delta {
                        // ── Reasoning / thinking (Qwen3) ──────────────
                        if let Some(ref reasoning) = delta.reasoning_content {
                            if !reasoning.is_empty() {
                                if !thinking_open {
                                    emit!(StreamEvent::ContentBlockStart {
                                        index: next_block,
                                        content_block: ContentBlock::Thinking {
                                            thinking: String::new(),
                                            signature: String::new(),
                                        },
                                    });
                                    thinking_open = true;
                                    // Don't increment next_block yet — it's
                                    // incremented when the block is closed.
                                }
                                emit!(StreamEvent::ContentBlockDelta {
                                    index: next_block,
                                    delta: ContentDelta::ThinkingDelta {
                                        thinking: reasoning.clone(),
                                    },
                                });
                            }
                        }

                        // ── Text content ──────────────────────────────
                        if let Some(ref content) = delta.content {
                            if !content.is_empty() {
                                // Close thinking block first if transitioning.
                                if thinking_open {
                                    emit!(StreamEvent::ContentBlockStop { index: next_block });
                                    next_block += 1;
                                    thinking_open = false;
                                }

                                // Open a text block if none is active.
                                if !text_open {
                                    text_block_idx = next_block;
                                    emit!(StreamEvent::ContentBlockStart {
                                        index: text_block_idx,
                                        content_block: ContentBlock::Text {
                                            text: String::new(),
                                        },
                                    });
                                    text_open = true;
                                    // next_block is advanced when the text
                                    // block is closed (on finish or tool_calls).
                                }

                                emit!(StreamEvent::ContentBlockDelta {
                                    index: text_block_idx,
                                    delta: ContentDelta::TextDelta {
                                        text: content.clone(),
                                    },
                                });
                            }
                        }

                        // ── Tool calls ────────────────────────────────
                        if let Some(ref tc_deltas) = delta.tool_calls {
                            for tc_delta in tc_deltas {
                                let tc_idx = tc_delta.index.unwrap_or(0);

                                // New tool call starting (has a non-empty `id`).
                                // Some providers (Qwen3) send deltas with id=""
                                // alongside argument continuations — only skip the
                                // block creation, NOT the arguments that follow.
                                if let Some(ref id) = tc_delta.id {
                                    if !id.is_empty() {
                                        // Close text block if still open.
                                        if text_open {
                                            emit!(StreamEvent::ContentBlockStop {
                                                index: text_block_idx
                                            });
                                            next_block = text_block_idx + 1;
                                            text_open = false;
                                        }

                                        let name = tc_delta
                                            .function
                                            .as_ref()
                                            .and_then(|f| f.name.clone())
                                            .unwrap_or_default();

                                        let blk_idx = next_block;
                                        next_block += 1;

                                        tool_blocks.insert(
                                            tc_idx,
                                            (blk_idx, id.clone(), name.clone(), String::new()),
                                        );

                                        emit!(StreamEvent::ContentBlockStart {
                                            index: blk_idx,
                                            content_block: ContentBlock::ToolUse {
                                                id: id.clone(),
                                                name,
                                                input: Value::Object(Default::default()),
                                            },
                                        });
                                    }
                                    // id="" phantom: skip block creation but
                                    // fall through to argument accumulation below.
                                }

                                // Accumulate function arguments — guard against
                                // phantom deltas that arrive before any real
                                // tool_call block was opened (Qwen3 edge case).
                                if let Some(ref func) = tc_delta.function {
                                    if let Some(ref args) = func.arguments {
                                        if let Some(tb) = tool_blocks.get_mut(&tc_idx) {
                                            tb.3.push_str(args);
                                            emit!(StreamEvent::ContentBlockDelta {
                                                index: tb.0,
                                                delta: ContentDelta::InputJsonDelta {
                                                    partial_json: args.clone(),
                                                },
                                            });
                                        } else {
                                            debug!(
                                                tc_idx = tc_idx,
                                                "Ignoring tool_call args for unknown block index \
                                                 (likely phantom delta)"
                                            );
                                        }
                                    }
                                }
                            }
                        }
                    }

                    // ── finish_reason → close all open blocks ─────────
                    if let Some(ref reason) = choice.finish_reason {
                        if thinking_open {
                            emit!(StreamEvent::ContentBlockStop { index: next_block });
                            thinking_open = false;
                        }
                        if text_open {
                            emit!(StreamEvent::ContentBlockStop {
                                index: text_block_idx
                            });
                            text_open = false;
                        }
                        for (blk_idx, ..) in tool_blocks.values() {
                            emit!(StreamEvent::ContentBlockStop { index: *blk_idx });
                        }
                        tool_blocks.clear();

                        let stop_reason = match reason.as_str() {
                            "stop" => "end_turn",
                            "length" => "max_tokens",
                            "tool_calls" => "tool_use",
                            other => other,
                        };

                        let usage = chunk.usage.as_ref().map(|u| UsageInfo {
                            input_tokens: u.prompt_tokens.unwrap_or(0),
                            output_tokens: u.completion_tokens.unwrap_or(0),
                            ..Default::default()
                        });

                        emit!(StreamEvent::MessageDelta {
                            stop_reason: Some(stop_reason.to_string()),
                            usage,
                        });

                        // Some APIs send finish_reason without a [DONE] line.
                        // Emit MessageStop as a safety net — guarded by flag
                        // so we never emit it twice.
                        if !message_stopped {
                            emit!(StreamEvent::MessageStop);
                            message_stopped = true;
                        }
                    }
                }
            }
        }

        Ok(())
    }

    /// Parse Ollama NDJSON stream into StreamEvents.
    ///
    /// Key differences from OpenAI SSE:
    /// - NDJSON lines, not `data:` prefix SSE
    /// - `done: true` on final line
    /// - Tool calls are objects with pre-parsed `arguments` (not JSON strings)
    /// - No incremental tool_call deltas — full tool call arrives in a single chunk
    ///
    /// Allocation strategy: raw bytes are appended into a persistent
    /// `leftover` buffer. We find the last newline, process all complete
    /// lines in-place (via `split('\n')`), and keep the incomplete
    /// trailing fragment for the next iteration. This avoids a Vec
    /// allocation per chunk.
    async fn process_ollama_stream(
        resp: reqwest::Response,
        handler: Arc<dyn StreamHandler>,
        tx: mpsc::Sender<StreamEvent>,
    ) -> Result<(), ClaudeError> {
        macro_rules! emit {
            ($evt:expr) => {{
                let evt = $evt;
                handler.on_event(&evt);
                let _ = tx.send(evt).await;
            }};
        }

        let mut byte_stream = resp.bytes_stream();
        let mut leftover = Vec::<u8>::new();
        let mut sent_start = false;

        // Block tracking — lazily opened so tool-only responses get no spurious text block.
        let mut next_block: usize = 0;
        let mut text_open = false;
        let mut text_block_idx: usize = 0;
        let mut thinking_open = false;
        let mut thinking_block_idx: usize = 0;
        let mut had_tool_calls = false;

        while let Some(chunk_result) = byte_stream.next().await {
            let chunk = chunk_result.map_err(ClaudeError::Http)?;
            leftover.extend_from_slice(&chunk);

            // Find the last newline — everything before it is complete
            // lines; everything after stays in the buffer.
            let split_pos = match leftover.iter().rposition(|&b| b == b'\n') {
                Some(pos) => pos + 1,
                None => continue, // no complete line yet
            };

            // Safety: we only feed UTF-8 JSON from the server. Lossy
            // conversion keeps us resilient to stray bytes without panicking.
            let complete = String::from_utf8_lossy(&leftover[..split_pos]).to_string();
            leftover.drain(..split_pos);

            for line in complete.split('\n') {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }

                let chunk: OllamaChatChunk = match serde_json::from_str(line) {
                    Ok(c) => c,
                    Err(e) => {
                        debug!(error = %e, line = line, "Failed to parse Ollama chunk");
                        continue;
                    }
                };

                if !sent_start {
                    emit!(StreamEvent::MessageStart {
                        id: uuid_v4(),
                        model: chunk.model.clone().unwrap_or_default(),
                        usage: UsageInfo::default(),
                    });
                    sent_start = true;
                }

                if let Some(ref msg) = chunk.message {
                    // ── Thinking content (Qwen3 on Ollama with `think: true`) ──
                    if let Some(ref thinking) = msg.thinking {
                        if !thinking.is_empty() {
                            if !thinking_open {
                                thinking_block_idx = next_block;
                                next_block += 1;
                                emit!(StreamEvent::ContentBlockStart {
                                    index: thinking_block_idx,
                                    content_block: ContentBlock::Thinking {
                                        thinking: String::new(),
                                        signature: String::new(),
                                    },
                                });
                                thinking_open = true;
                            }
                            emit!(StreamEvent::ContentBlockDelta {
                                index: thinking_block_idx,
                                delta: ContentDelta::ThinkingDelta {
                                    thinking: thinking.clone(),
                                },
                            });
                        }
                    }

                    // ── Text content — lazily open text block ────────────
                    if let Some(ref content) = msg.content {
                        if !content.is_empty() {
                            // Thinking must close before text opens.
                            if thinking_open {
                                emit!(StreamEvent::ContentBlockStop {
                                    index: thinking_block_idx
                                });
                                thinking_open = false;
                            }
                            if !text_open {
                                text_block_idx = next_block;
                                next_block += 1;
                                emit!(StreamEvent::ContentBlockStart {
                                    index: text_block_idx,
                                    content_block: ContentBlock::Text {
                                        text: String::new(),
                                    },
                                });
                                text_open = true;
                            }
                            emit!(StreamEvent::ContentBlockDelta {
                                index: text_block_idx,
                                delta: ContentDelta::TextDelta {
                                    text: content.clone(),
                                },
                            });
                        }
                    }

                    // ── Tool calls (Ollama sends full objects, not partial JSON) ──
                    if let Some(ref tool_calls) = msg.tool_calls {
                        if !tool_calls.is_empty() {
                            // Close any open thinking or text block before emitting tool use blocks.
                            if thinking_open {
                                emit!(StreamEvent::ContentBlockStop {
                                    index: thinking_block_idx
                                });
                                thinking_open = false;
                            }
                            if text_open {
                                emit!(StreamEvent::ContentBlockStop {
                                    index: text_block_idx
                                });
                                text_open = false;
                            }

                            for tc in tool_calls {
                                if let Some(ref func) = tc.function {
                                    let name = func.name.clone().unwrap_or_default();
                                    let args = func
                                        .arguments
                                        .clone()
                                        .unwrap_or(Value::Object(Default::default()));
                                    let blk_idx = next_block;
                                    next_block += 1;
                                    // Ollama doesn't provide tool call IDs — generate one.
                                    let tc_id = format!("toolu_{}", blk_idx);
                                    had_tool_calls = true;

                                    emit!(StreamEvent::ContentBlockStart {
                                        index: blk_idx,
                                        content_block: ContentBlock::ToolUse {
                                            id: tc_id,
                                            name,
                                            input: args.clone(),
                                        },
                                    });
                                    // Send arguments as a single InputJsonDelta
                                    // (Ollama gives us the full object at once).
                                    let args_json = serde_json::to_string(&args)
                                        .unwrap_or_else(|_| "{}".to_string());
                                    emit!(StreamEvent::ContentBlockDelta {
                                        index: blk_idx,
                                        delta: ContentDelta::InputJsonDelta {
                                            partial_json: args_json,
                                        },
                                    });
                                    emit!(StreamEvent::ContentBlockStop { index: blk_idx });
                                }
                            }
                        }
                    }
                }

                if chunk.done.unwrap_or(false) {
                    // Close any still-open blocks in order.
                    if thinking_open {
                        emit!(StreamEvent::ContentBlockStop {
                            index: thinking_block_idx
                        });
                        thinking_open = false;
                    }
                    if text_open {
                        emit!(StreamEvent::ContentBlockStop {
                            index: text_block_idx
                        });
                        text_open = false;
                    }

                    let usage = UsageInfo {
                        input_tokens: chunk.prompt_eval_count.unwrap_or(0),
                        output_tokens: chunk.eval_count.unwrap_or(0),
                        ..Default::default()
                    };

                    let stop_reason = if had_tool_calls {
                        "tool_use"
                    } else {
                        "end_turn"
                    };

                    emit!(StreamEvent::MessageDelta {
                        stop_reason: Some(stop_reason.to_string()),
                        usage: Some(usage),
                    });

                    emit!(StreamEvent::MessageStop);
                }
            }
        }

        Ok(())
    }

    /// Build the streaming URL based on provider format.
    ///
    /// Most OpenAI-compatible endpoints follow the convention
    /// `<api_base>/v1/chat/completions`. Some — notably z.ai / Zhipu's
    /// bigmodel API — already include the version segment in their
    /// `api_base` (e.g. `https://api.z.ai/api/paas/v4`) and the real
    /// path is `<api_base>/chat/completions` without an extra `/v1/`.
    /// Detect this via the trailing version segment and skip the
    /// redundant `/v1/` to avoid a 404.
    fn stream_url(&self) -> String {
        match self.config.api_format {
            ApiFormat::Ollama => format!("{}/api/chat", self.config.api_base),
            _ => format!(
                "{}{}/chat/completions",
                self.config.api_base,
                if base_already_versioned(&self.config.api_base) {
                    ""
                } else {
                    "/v1"
                }
            ),
        }
    }

    /// Build the models URL. Same versioning rule as `stream_url`.
    fn models_url(&self) -> String {
        match self.config.api_format {
            ApiFormat::Ollama => format!("{}/api/tags", self.config.api_base),
            _ => format!(
                "{}{}/models",
                self.config.api_base,
                if base_already_versioned(&self.config.api_base) {
                    ""
                } else {
                    "/v1"
                }
            ),
        }
    }

    /// Send request with retry logic.
    async fn send_with_retry(
        &self,
        url: &str,
        body: &Value,
    ) -> Result<reqwest::Response, ClaudeError> {
        let mut attempts = 0u32;
        let mut delay = Duration::from_secs(2);

        loop {
            attempts += 1;

            let mut req = self
                .http
                .post(url)
                .header("content-type", "application/json");

            if !self.config.api_key.is_empty() {
                req = req.header("Authorization", format!("Bearer {}", &self.config.api_key));
            }

            let resp = req.json(body).send().await.map_err(ClaudeError::Http)?;
            let status = resp.status().as_u16();

            if resp.status().is_success() {
                return Ok(resp);
            }

            if (status == 429 || status == 529) && attempts <= self.config.max_retries {
                let retry_after = resp
                    .headers()
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.parse::<u64>().ok())
                    .map(Duration::from_secs);

                let wait = retry_after.unwrap_or(delay);
                warn!(
                    status,
                    attempt = attempts,
                    wait_secs = wait.as_secs(),
                    "Retryable API error, backing off"
                );
                tokio::time::sleep(wait).await;
                delay = (delay * 2).min(Duration::from_secs(30));
                continue;
            }

            let text = resp.text().await.unwrap_or_else(|e| {
                warn!(error = %e, "Failed to read error response body");
                String::new()
            });
            // Extract human-readable message from JSON error body
            let message = serde_json::from_str::<Value>(&text)
                .ok()
                .and_then(|v| {
                    v.get("error")
                        .and_then(|e| e.get("message"))
                        .and_then(|m| m.as_str())
                        .map(|s| s.to_string())
                })
                .unwrap_or(text);
            return Err(ClaudeError::ApiStatus { status, message });
        }
    }
}

// ---------------------------------------------------------------------------
// LlmProvider trait implementation
// ---------------------------------------------------------------------------

#[async_trait]
impl LlmProvider for OpenAiProvider {
    async fn create_message_stream(
        &self,
        request: CreateMessageRequest,
        handler: Arc<dyn StreamHandler>,
    ) -> Result<mpsc::Receiver<StreamEvent>, ClaudeError> {
        let openai_req = self.translate_request(&request);
        let url = self.stream_url();
        let body = serde_json::to_value(&openai_req).map_err(ClaudeError::Json)?;

        // Diagnostic body dump under RUST_LOG=trace — invaluable for the
        // GLM/z.ai "400 invalid api parameter" class of errors where the
        // server returns no detail. Pretty-printed; truncated for sanity.
        if tracing::enabled!(tracing::Level::TRACE) {
            let preview = serde_json::to_string_pretty(&body)
                .unwrap_or_else(|_| "<unserialisable>".to_string());
            let cut = preview.chars().take(50000).collect::<String>();
            tracing::trace!(url = %url, body = %cut, "openai_provider outgoing request");
        }

        let resp = self.send_with_retry(&url, &body).await?;
        let (tx, rx) = mpsc::channel(256);

        let api_format = self.config.api_format;
        tokio::spawn(async move {
            let result = match api_format {
                ApiFormat::Ollama => Self::process_ollama_stream(resp, handler, tx.clone()).await,
                _ => Self::process_openai_sse(resp, handler, tx.clone()).await,
            };
            if let Err(e) = result {
                let _ = tx
                    .send(StreamEvent::Error {
                        error_type: "stream_error".into(),
                        message: e.to_string(),
                    })
                    .await;
            }
        });

        Ok(rx)
    }

    async fn create_message(
        &self,
        request: CreateMessageRequest,
    ) -> Result<CreateMessageResponse, ClaudeError> {
        let mut openai_req = self.translate_request(&request);
        openai_req.stream = false;
        // Qwen3 requires enable_thinking=false for non-streaming calls.
        if openai_req.enable_thinking == Some(true) {
            openai_req.enable_thinking = Some(false);
        }
        let url = self.stream_url();
        let body = serde_json::to_value(&openai_req).map_err(ClaudeError::Json)?;

        let resp = self.send_with_retry(&url, &body).await?;
        let status = resp.status();
        let text = resp.text().await.map_err(ClaudeError::Http)?;

        if !status.is_success() {
            return Err(ClaudeError::ApiStatus {
                status: status.as_u16(),
                message: text,
            });
        }

        // Parse OpenAI response and convert to our internal format
        let openai_resp: Value = serde_json::from_str(&text).map_err(ClaudeError::Json)?;

        let id = openai_resp
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let model = openai_resp
            .get("model")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let mut content = Vec::new();
        if let Some(choices) = openai_resp.get("choices").and_then(|v| v.as_array()) {
            for choice in choices {
                if let Some(msg) = choice.get("message") {
                    // Text content
                    if let Some(c) = msg.get("content").and_then(|v| v.as_str()) {
                        if !c.is_empty() {
                            content.push(serde_json::json!({"type": "text", "text": c}));
                        }
                    }
                    // Tool calls
                    if let Some(tcs) = msg.get("tool_calls").and_then(|v| v.as_array()) {
                        for tc in tcs {
                            let id = tc.get("id").and_then(|v| v.as_str()).unwrap_or("");
                            if let Some(func) = tc.get("function") {
                                let name = func.get("name").and_then(|v| v.as_str()).unwrap_or("");
                                let args_str = func
                                    .get("arguments")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("{}");
                                let input: Value = serde_json::from_str(args_str)
                                    .unwrap_or(Value::Object(Default::default()));
                                content.push(serde_json::json!({
                                    "type": "tool_use",
                                    "id": id,
                                    "name": name,
                                    "input": input,
                                }));
                            }
                        }
                    }
                    // Reasoning content (Qwen3)
                    if let Some(reasoning) = msg.get("reasoning_content").and_then(|v| v.as_str()) {
                        if !reasoning.is_empty() {
                            content.insert(
                                0,
                                serde_json::json!({
                                    "type": "thinking",
                                    "thinking": reasoning,
                                    "signature": "",
                                }),
                            );
                        }
                    }
                }
            }
        }

        let stop_reason = openai_resp
            .get("choices")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("finish_reason"))
            .and_then(|v| v.as_str())
            .map(|r| match r {
                "stop" => "end_turn",
                "length" => "max_tokens",
                "tool_calls" => "tool_use",
                other => other,
            })
            .map(|s| s.to_string());

        let usage_obj = openai_resp.get("usage");
        let usage = UsageInfo {
            input_tokens: usage_obj
                .and_then(|u| u.get("prompt_tokens"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0),
            output_tokens: usage_obj
                .and_then(|u| u.get("completion_tokens"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0),
            ..Default::default()
        };

        Ok(CreateMessageResponse {
            id,
            response_type: "message".to_string(),
            role: "assistant".to_string(),
            content,
            model,
            stop_reason,
            stop_sequence: None,
            usage,
        })
    }

    async fn list_models(&self) -> Vec<AvailableModel> {
        let url = self.models_url();
        let mut req = self.http.get(&url);
        if !self.config.api_key.is_empty() {
            req = req.header("Authorization", format!("Bearer {}", &self.config.api_key));
        }

        let resp = match req.send().await {
            Ok(r) => r,
            Err(e) => {
                warn!(url = %url, error = %e, "list_models request failed");
                return vec![];
            }
        };

        if !resp.status().is_success() {
            warn!(url = %url, status = %resp.status(), "list_models returned non-success");
            return vec![];
        }

        let body: Value = match resp.json().await {
            Ok(v) => v,
            Err(e) => {
                warn!(error = %e, "list_models: failed to parse JSON response");
                return vec![];
            }
        };

        // OpenAI format: { "data": [{ "id": "...", "created": ... }] }
        // Ollama format: { "models": [{ "name": "...", "modified_at": "..." }] }
        let models_array = body
            .get("data")
            .or_else(|| body.get("models"))
            .and_then(|v| v.as_array());

        match models_array {
            Some(arr) => arr
                .iter()
                .filter_map(|m| {
                    let id = m
                        .get("id")
                        .or_else(|| m.get("name"))
                        .and_then(|v| v.as_str())?
                        .to_string();
                    let created_at = m.get("created").and_then(|v| v.as_i64());
                    Some(AvailableModel {
                        id: id.clone(),
                        display_name: Some(id),
                        created_at,
                    })
                })
                .collect(),
            None => vec![],
        }
    }

    fn capabilities(&self) -> &ProviderCapabilities {
        &self.capabilities
    }

    // model_supports_thinking, fast_model_for, context_window, max_output_tokens,
    // model_pricing — all use the default implementations that query known_models.
    // No overrides needed. Adding a model = adding a ModelMetadata entry in the preset.
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Convert an Anthropic-format `image` / `document` content block (as a
/// JSON object) into an OpenAI vision `image_url` value string (`data:...`
/// URI or remote URL). Returns `None` when the source is missing or
/// malformed so the caller can skip emitting the part.
///
/// `block_type` ("image" | "document") drives the default media_type
/// fallback: a malformed image block defaults to `image/png`, a document
/// block to `application/pdf`. Without this the data: URI would lie
/// about the payload and confuse downstream model preprocessing.
fn image_url_from_source(block_type: &str, block: &Value) -> Option<String> {
    let source = block.get("source")?;
    let source_type = source.get("type").and_then(|v| v.as_str()).unwrap_or("");
    match source_type {
        "base64" => {
            let default_media = if block_type == "document" {
                "application/pdf"
            } else {
                "image/png"
            };
            let media_type = source
                .get("media_type")
                .and_then(|v| v.as_str())
                .unwrap_or(default_media);
            let data = source.get("data").and_then(|v| v.as_str()).unwrap_or("");
            if data.is_empty() {
                return None;
            }
            Some(format!("data:{};base64,{}", media_type, data))
        }
        "url" => {
            let url = source.get("url").and_then(|v| v.as_str()).unwrap_or("");
            if url.is_empty() {
                None
            } else {
                Some(url.to_string())
            }
        }
        _ => None,
    }
}

/// Detect whether `api_base` already ends with a `/vN` segment so the
/// stream/models URL builders don't tack on a redundant `/v1/`. Used to
/// support z.ai / Zhipu (api_base = "...paas/v4") whose real chat path
/// is just `<api_base>/chat/completions` — no extra `/v1/`.
fn base_already_versioned(api_base: &str) -> bool {
    // Match trailing /vDIGITS (optionally with trailing /).
    let trimmed = api_base.trim_end_matches('/');
    if let Some(last) = trimmed.rsplit('/').next() {
        if let Some(rest) = last.strip_prefix('v') {
            return !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit());
        }
    }
    false
}

/// Compose explicit-error text for visual blocks the OpenAI-compat wire
/// cannot transmit to the model. Used by the translation layer to mirror
/// the AnthropicClient rejection behaviour across both wire families —
/// iso UX: every non-transmissible block becomes a loud `[ERROR: ...]`,
/// never a silent drop.
///
/// Two failure modes are handled, each with its own message because the
/// remediation differs:
///   * `image_count` > 0 AND model does not support vision → the
///     PROVIDER model can't read images; user must restart with a
///     vision-capable provider.
///   * `document_count` > 0 (regardless of supports_vision) → the wire
///     format itself (OpenAI vision `image_url`) does NOT accept PDFs;
///     z.ai live test returns `API error 400: 图片输入格式/解析错误`.
///     The PDF's text extract is shipped in the surviving text content,
///     so the model still has signal — but it must know the raw bytes
///     never reached it.
fn unsupported_blocks_error_text(image_count: usize, document_count: usize) -> String {
    let mut messages: Vec<String> = Vec::new();
    if image_count > 0 {
        let plural = if image_count > 1 { "blocks" } else { "block" };
        messages.push(format!(
            "[ERROR: {} image {} cannot be read — the current provider's \
             model does not support vision. Restart uppli-code with a \
             vision-capable provider (e.g. `uppli-code --provider glm`) to \
             process image files. The current session cannot be salvaged; \
             ask the user to relaunch.]",
            image_count, plural
        ));
    }
    if document_count > 0 {
        let plural = if document_count > 1 {
            "blocks"
        } else {
            "block"
        };
        messages.push(format!(
            "[ERROR: {} document {} (PDF) cannot be transmitted to \
             OpenAI-compatible providers; only true images are accepted on \
             the `image_url` channel. The text extract from the PDF is in \
             the accompanying content — use that. To forward the raw PDF \
             bytes for vision analysis of layouts/figures, use an \
             Anthropic-format provider whose model accepts document blocks \
             natively.]",
            document_count, plural
        ));
    }
    messages.join("\n")
}

fn uuid_v4() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("chatcmpl-{:x}", ts)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_translate_simple_request() {
        let provider = OpenAiProvider::new(OpenAiProviderConfig::from_loaded(
            crate::providers::loader::registry().find("ollama").unwrap(),
            String::new(),
            Some("gemma4".to_string()),
        ))
        .unwrap();

        let req = CreateMessageRequest {
            model: "gemma4".to_string(),
            max_tokens: 4096,
            messages: vec![crate::types::ApiMessage {
                role: "user".to_string(),
                content: Value::String("Hello".to_string()),
            }],
            system: Some(crate::types::SystemPrompt::Text(
                "You are helpful.".to_string(),
            )),
            tools: None,
            temperature: Some(0.7),
            top_p: None,
            top_k: None,
            stop_sequences: None,
            stream: true,
            thinking: None,
            output_config: None,
        };

        let openai_req = provider.translate_request(&req);
        assert_eq!(openai_req.model, "gemma4");
        assert_eq!(openai_req.messages.len(), 2); // system + user
        assert_eq!(openai_req.messages[0].role, "system");
        assert_eq!(openai_req.messages[1].role, "user");
        assert_eq!(openai_req.temperature, Some(0.7));
        assert_eq!(openai_req.think, Some(false)); // Ollama supports thinking, but this request has thinking: None → explicitly disable
    }

    #[test]
    fn test_translate_user_message_with_image_block() {
        // PR S follow-up: image blocks must NOT be silently dropped. They
        // must be serialised as OpenAI vision-format parts so multimodal
        // providers (GLM-4.6v, Qwen-VL) actually receive the image bytes.
        let provider = OpenAiProvider::new(OpenAiProviderConfig::from_loaded(
            crate::providers::loader::registry().find("glm").unwrap(),
            "test-key-long-enough".to_string(),
            Some("glm-4.6v".to_string()),
        ))
        .unwrap();

        let req = CreateMessageRequest {
            model: "glm-4.6v".to_string(),
            max_tokens: 4096,
            messages: vec![crate::types::ApiMessage {
                role: "user".to_string(),
                content: serde_json::json!([
                    { "type": "text", "text": "Read this invoice" },
                    {
                        "type": "image",
                        "source": {
                            "type": "base64",
                            "media_type": "image/png",
                            "data": "iVBORw0KGgo=" // tiny fake PNG payload
                        }
                    }
                ]),
            }],
            system: None,
            tools: None,
            temperature: None,
            top_p: None,
            top_k: None,
            stop_sequences: None,
            stream: true,
            thinking: None,
            output_config: None,
        };

        let openai_req = provider.translate_request(&req);
        // Find the user message and confirm its content is a list of parts
        // including the image_url.
        let user_msg = openai_req
            .messages
            .iter()
            .find(|m| m.role == "user")
            .expect("user message present");
        let content = user_msg
            .content
            .as_ref()
            .expect("content must be set when image is present");
        let parts = content
            .as_array()
            .expect("content must be a list of parts when images are present (not a plain string)");
        assert_eq!(parts.len(), 2, "should have one text part + one image part");
        // Find the image_url part
        let image_part = parts
            .iter()
            .find(|p| p.get("type").and_then(|v| v.as_str()) == Some("image_url"))
            .expect("image_url part must be present (was silently dropped before)");
        let url = image_part
            .get("image_url")
            .and_then(|v| v.get("url"))
            .and_then(|v| v.as_str())
            .expect("image_url.url must be a string");
        assert!(
            url.starts_with("data:image/png;base64,"),
            "url must be a data URI with the correct media type, got: {}",
            url
        );
        assert!(
            url.contains("iVBORw0KGgo="),
            "url must contain the base64 payload, got: {}",
            url
        );
    }

    #[test]
    fn test_translate_user_message_with_pdf_document_block() {
        // Document blocks must NOT reach z.ai via image_url (400 on
        // application/pdf). The previous fix silently dropped them —
        // that violated the "iso UX, error if not supported" contract
        // because the Anthropic wire flips is_error=true in the same
        // case. Current behaviour (iso): the translation layer emits
        // an EXPLICIT [ERROR: ... document ... cannot be transmitted
        // ...] line in the message content so the model knows the
        // PDF bytes never reached it. The PDF's text extract (placed
        // by pdf::read_pdf in a sibling text block) still goes through.
        let provider = OpenAiProvider::new(OpenAiProviderConfig::from_loaded(
            crate::providers::loader::registry().find("glm").unwrap(),
            "test-key-long-enough".to_string(),
            Some("glm-4.5v".to_string()),
        ))
        .unwrap();

        let req = CreateMessageRequest {
            model: "glm-4.5v".to_string(),
            max_tokens: 4096,
            messages: vec![crate::types::ApiMessage {
                role: "user".to_string(),
                content: serde_json::json!([
                    { "type": "text", "text": "Read this invoice" },
                    {
                        "type": "document",
                        "source": {
                            "type": "base64",
                            "media_type": "application/pdf",
                            "data": "JVBERi0xLjQK"
                        }
                    }
                ]),
            }],
            system: None,
            tools: None,
            temperature: None,
            top_p: None,
            top_k: None,
            stop_sequences: None,
            stream: true,
            thinking: None,
            output_config: None,
        };

        let openai_req = provider.translate_request(&req);
        let user_msg = openai_req
            .messages
            .iter()
            .find(|m| m.role == "user")
            .expect("user message present");
        let content = user_msg.content.as_ref().expect("content set");
        // Document block dropped → no image_url part remains. Whether
        // the content is a plain string or a single-element Array
        // depends on whether the surviving text triggered the
        // multi-part path; either way there must be NO image_url
        // referencing application/pdf.
        let has_pdf_image_url = match content {
            Value::Array(parts) => parts.iter().any(|p| {
                p.get("type").and_then(|v| v.as_str()) == Some("image_url")
                    && p.get("image_url")
                        .and_then(|v| v.get("url"))
                        .and_then(|v| v.as_str())
                        .is_some_and(|u| u.contains("application/pdf"))
            }),
            _ => false,
        };
        assert!(
            !has_pdf_image_url,
            "Document blocks MUST NOT be forwarded as image_url on the OpenAI-compat wire (z.ai rejects with 400)"
        );
        // The accompanying text must survive — that is where the
        // model sees the PDF content.
        let surviving_text = match content {
            Value::String(s) => s.clone(),
            Value::Array(parts) => parts
                .iter()
                .filter_map(|p| {
                    if p.get("type").and_then(|v| v.as_str()) == Some("text") {
                        p.get("text")
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string())
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>()
                .join(""),
            _ => String::new(),
        };
        assert!(
            surviving_text.contains("Read this invoice"),
            "text block must survive: {}",
            surviving_text
        );
        // NEW contract: document rejection is explicit, not silent.
        assert!(
            surviving_text.contains("ERROR") && surviving_text.contains("document"),
            "document rejection must be surfaced as explicit ERROR text (iso \
             AnthropicClient is_error=true), got: {}",
            surviving_text
        );
    }

    #[test]
    fn tool_result_document_on_vision_provider_still_errors() {
        // Even on a vision-capable OpenAI-compat provider (glm-4.5v),
        // PDF documents in a tool_result must surface an error: the
        // OpenAI image_url channel rejects application/pdf with a 400
        // regardless of model capability. The image_url path is reserved
        // for true images. Test asserts the rejection is explicit, not
        // silent — closes the iso-UX gap with AnthropicClient.
        let provider = glm_provider_for_test();
        let msg = crate::types::ApiMessage {
            role: "user".to_string(),
            content: serde_json::json!([
                {
                    "type": "tool_result",
                    "tool_use_id": "tu_pdf",
                    "content": [
                        { "type": "text", "text": "[PDF: report.pdf, 12 pages]\nExtracted text: Q1 results..." },
                        {
                            "type": "document",
                            "source": {
                                "type": "base64",
                                "media_type": "application/pdf",
                                "data": "JVBERi0="
                            }
                        }
                    ]
                }
            ]),
        };
        let translated = provider.translate_message(&msg);
        assert_eq!(translated.len(), 1);
        assert_eq!(translated[0].role, "tool");
        let content = translated[0].content.as_ref().expect("content set");
        let text_payload = content.as_str().expect(
            "document-only tool_result must emit a flat string \
                     (no image_url parts forwarded)",
        );
        // Text extract must survive.
        assert!(text_payload.contains("Q1 results"));
        // Explicit error must be present.
        assert!(
            text_payload.contains("ERROR") && text_payload.contains("document"),
            "document rejection must be a loud [ERROR: ...] block, got: {}",
            text_payload
        );
        assert!(
            text_payload.contains("Anthropic-format provider"),
            "remediation must mention the Anthropic-format alternative for vision-PDF, got: {}",
            text_payload
        );
    }

    fn make_thinking_req(model: &str) -> CreateMessageRequest {
        CreateMessageRequest {
            model: model.to_string(),
            max_tokens: 4096,
            messages: vec![crate::types::ApiMessage {
                role: "user".to_string(),
                content: Value::String("Think about this".to_string()),
            }],
            system: None,
            tools: None,
            temperature: None,
            top_p: None,
            top_k: None,
            stop_sequences: None,
            stream: true,
            thinking: Some(crate::types::ThinkingConfig::enabled(16000)),
            output_config: None,
        }
    }

    #[test]
    fn thinking_qwen3_dialect_emitted_on_alibaba() {
        // Alibaba's thinking_format = Qwen3 → enable_thinking + thinking_budget.
        let provider = OpenAiProvider::new(OpenAiProviderConfig::from_loaded(
            crate::providers::loader::registry()
                .find("alibaba")
                .unwrap(),
            "test-key-long-enough".to_string(),
            Some("qwen3-235b".to_string()),
        ))
        .unwrap();
        let openai_req = provider.translate_request(&make_thinking_req("qwen3-235b"));
        assert_eq!(openai_req.enable_thinking, Some(true));
        assert_eq!(openai_req.thinking_budget, Some(16000));
        assert!(
            openai_req.thinking.is_none(),
            "Qwen3 must NOT emit nested thinking"
        );
        assert!(openai_req.think.is_none(), "Qwen3 must NOT emit `think`");
    }

    #[test]
    fn thinking_anthropic_nested_dialect_emitted_on_glm() {
        // z.ai's thinking_format = AnthropicNested → thinking: {type, budget_tokens}.
        // Verified live with curl: GLM-5 on z.ai expects this shape.
        let provider = OpenAiProvider::new(OpenAiProviderConfig::from_loaded(
            crate::providers::loader::registry().find("glm").unwrap(),
            "test-key-long-enough".to_string(),
            Some("glm-5".to_string()),
        ))
        .unwrap();
        let openai_req = provider.translate_request(&make_thinking_req("glm-5"));
        let nested = openai_req
            .thinking
            .as_ref()
            .expect("GLM must emit nested thinking config");
        assert_eq!(nested.thinking_type, "enabled");
        assert_eq!(nested.budget_tokens, 16000);
        assert!(
            openai_req.enable_thinking.is_none(),
            "Anthropic-nested dialect must NOT emit Qwen3 enable_thinking"
        );
        assert!(openai_req.thinking_budget.is_none());
        assert!(openai_req.think.is_none());
    }

    #[test]
    fn thinking_ollama_dialect_emitted_on_ollama() {
        // Ollama's thinking_format defaults to OllamaThink via api_format.
        let provider = OpenAiProvider::new(OpenAiProviderConfig::from_loaded(
            crate::providers::loader::registry().find("ollama").unwrap(),
            String::new(),
            Some("qwen3:14b".to_string()),
        ))
        .unwrap();
        let openai_req = provider.translate_request(&make_thinking_req("qwen3:14b"));
        assert_eq!(openai_req.think, Some(true));
        assert!(openai_req.thinking.is_none());
        assert!(openai_req.enable_thinking.is_none());
    }

    #[test]
    fn thinking_no_dialect_emits_nothing_on_mistral() {
        // Mistral has no thinking_format declared (and api_format=openai
        // doesn't auto-infer). Even when --effort is set, NO thinking
        // field should hit the wire: Mistral's API would reject it.
        let provider = OpenAiProvider::new(OpenAiProviderConfig::from_loaded(
            crate::providers::loader::registry()
                .find("mistral")
                .unwrap(),
            "test-key-long-enough".to_string(),
            Some("mistral-large-latest".to_string()),
        ))
        .unwrap();
        let openai_req = provider.translate_request(&make_thinking_req("mistral-large-latest"));
        assert!(openai_req.thinking.is_none());
        assert!(openai_req.enable_thinking.is_none());
        assert!(openai_req.thinking_budget.is_none());
        assert!(openai_req.think.is_none());
    }

    #[test]
    fn thinking_anthropic_nested_emits_nothing_when_user_disabled() {
        // Pins the (AnthropicNested, user_wants_thinking=false) branch:
        // even though the provider has a thinking dialect declared, if
        // the user did NOT set --effort (req.thinking is None), nothing
        // hits the wire. A future refactor of the match can't silently
        // change this without flipping this test.
        let provider = OpenAiProvider::new(OpenAiProviderConfig::from_loaded(
            crate::providers::loader::registry().find("glm").unwrap(),
            "test-key-long-enough".to_string(),
            Some("glm-5".to_string()),
        ))
        .unwrap();
        let mut req = make_thinking_req("glm-5");
        req.thinking = None;
        let openai_req = provider.translate_request(&req);
        assert!(openai_req.thinking.is_none());
        assert!(openai_req.enable_thinking.is_none());
        assert!(openai_req.thinking_budget.is_none());
        assert!(openai_req.think.is_none());
    }

    #[test]
    fn test_translate_tool_use_message() {
        let provider = OpenAiProvider::new(OpenAiProviderConfig::from_loaded(
            crate::providers::loader::registry().find("ollama").unwrap(),
            String::new(),
            Some("gemma4".to_string()),
        ))
        .unwrap();

        let tool_use_msg = crate::types::ApiMessage {
            role: "assistant".to_string(),
            content: serde_json::json!([
                {"type": "text", "text": "Let me read that file."},
                {"type": "tool_use", "id": "tu_1", "name": "Read", "input": {"path": "/tmp/test"}}
            ]),
        };

        let translated = provider.translate_message(&tool_use_msg);
        assert_eq!(translated.len(), 1);
        assert_eq!(translated[0].role, "assistant");
        assert!(translated[0].tool_calls.is_some());
        let tc = translated[0].tool_calls.as_ref().unwrap();
        assert_eq!(tc.len(), 1);
        assert_eq!(tc[0].function.name, "Read");
    }

    #[test]
    fn test_translate_tool_result_message() {
        let provider = OpenAiProvider::new(OpenAiProviderConfig::from_loaded(
            crate::providers::loader::registry().find("ollama").unwrap(),
            String::new(),
            Some("gemma4".to_string()),
        ))
        .unwrap();

        let tool_result_msg = crate::types::ApiMessage {
            role: "user".to_string(),
            content: serde_json::json!([
                {"type": "tool_result", "tool_use_id": "tu_1", "content": "file contents here"}
            ]),
        };

        let translated = provider.translate_message(&tool_result_msg);
        assert_eq!(translated.len(), 1);
        assert_eq!(translated[0].role, "tool");
        assert_eq!(translated[0].tool_call_id, None);
    }

    // ---- tool_result vision dispatch (commit 6) ----------------------------
    //
    // Pin the two-way branch in the tool_result translation:
    //   Vision-capable provider (GLM) + image in payload
    //     → multi-part Array of {text, image_url}
    //   Text-only provider (Ollama / Mistral / OpenAI default) + image
    //     → flat string (image dropped — text fallback survives)
    //   Any provider + text-only inner blocks
    //     → flat string (legacy shape)

    fn glm_provider_for_test() -> OpenAiProvider {
        OpenAiProvider::new(OpenAiProviderConfig::from_loaded(
            crate::providers::loader::registry().find("glm").unwrap(),
            "test-key-not-used".to_string(),
            None,
        ))
        .unwrap()
    }

    fn ollama_provider_for_test() -> OpenAiProvider {
        OpenAiProvider::new(OpenAiProviderConfig::from_loaded(
            crate::providers::loader::registry().find("ollama").unwrap(),
            String::new(),
            None,
        ))
        .unwrap()
    }

    fn make_tool_result_with_image_blocks() -> crate::types::ApiMessage {
        crate::types::ApiMessage {
            role: "user".to_string(),
            content: serde_json::json!([
                {
                    "type": "tool_result",
                    "tool_use_id": "tu_visual",
                    "content": [
                        {"type": "text", "text": "[chart.png — Q1 revenue]"},
                        {
                            "type": "image",
                            "source": {
                                "type": "base64",
                                "media_type": "image/png",
                                "data": "iVBORw0KGgo="
                            }
                        }
                    ]
                }
            ]),
        }
    }

    #[test]
    fn tool_result_image_on_vision_provider_emits_multipart_array() {
        let provider = glm_provider_for_test();
        let translated = provider.translate_message(&make_tool_result_with_image_blocks());
        assert_eq!(translated.len(), 1);
        assert_eq!(translated[0].role, "tool");
        let content = translated[0].content.as_ref().expect("content set");
        let arr = content
            .as_array()
            .expect("vision provider must emit a multi-part Array");
        assert_eq!(arr.len(), 2, "expected text + image part, got: {arr:?}");
        assert_eq!(arr[0].get("type").and_then(|v| v.as_str()), Some("text"));
        assert!(arr[0]
            .get("text")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .contains("chart.png"));
        assert_eq!(
            arr[1].get("type").and_then(|v| v.as_str()),
            Some("image_url")
        );
        let url = arr[1]
            .get("image_url")
            .and_then(|v| v.get("url"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        assert!(
            url.starts_with("data:image/png;base64,"),
            "url should be a data URI, got: {url}"
        );
    }

    #[test]
    fn tool_result_image_on_text_only_provider_rejects_with_explicit_error() {
        // Iso UX with AnthropicClient: a tool_result containing an image
        // on a non-vision provider must surface an EXPLICIT error in the
        // tool message content, not silently drop the image. The caption
        // (chart.png) is preserved so the model has context; the error
        // line tells the user to relaunch with a vision-capable provider.
        let provider = ollama_provider_for_test();
        let translated = provider.translate_message(&make_tool_result_with_image_blocks());
        assert_eq!(translated.len(), 1);
        assert_eq!(translated[0].role, "tool");
        let content = translated[0].content.as_ref().expect("content set");
        let s = content
            .as_str()
            .expect("text-only provider must emit a flat string (no multi-part Array)");
        assert!(
            s.contains("chart.png"),
            "caption must survive as the textual context, got: {s}"
        );
        assert!(
            s.contains("ERROR"),
            "must emit an EXPLICIT error so the model knows the image was not read, got: {s}"
        );
        assert!(
            s.contains("Restart") || s.contains("--provider"),
            "must instruct the user to relaunch with a vision-capable provider, got: {s}"
        );
    }

    #[test]
    fn base_already_versioned_recognises_v4_suffix() {
        assert!(base_already_versioned("https://api.z.ai/api/paas/v4"));
        assert!(base_already_versioned("https://api.z.ai/api/paas/v4/"));
        assert!(base_already_versioned(
            "https://open.bigmodel.cn/api/paas/v4"
        ));
        assert!(base_already_versioned("https://example.com/v123"));
    }

    #[test]
    fn base_already_versioned_rejects_non_version_suffixes() {
        assert!(!base_already_versioned("https://api.openai.com"));
        assert!(!base_already_versioned("https://api.mistral.ai"));
        assert!(!base_already_versioned("https://api.deepseek.com"));
        assert!(!base_already_versioned("https://api.example.com/v"));
        assert!(!base_already_versioned("https://api.example.com/vapor"));
    }

    #[test]
    fn top_level_image_on_text_only_provider_rejects_with_explicit_error() {
        // Top-level user message (NOT inside a tool_result) carrying an
        // Image on a non-vision OpenAI-compat provider must also reject
        // loudly. Pre-PR this would have silently forwarded the
        // image_url to the endpoint, which would 400 with a low-level
        // error far from the model's awareness.
        let provider = ollama_provider_for_test();
        let msg = crate::types::ApiMessage {
            role: "user".to_string(),
            content: serde_json::json!([
                { "type": "text", "text": "Describe this image." },
                {
                    "type": "image",
                    "source": {
                        "type": "base64",
                        "media_type": "image/png",
                        "data": "iVBORw0KGgo="
                    }
                }
            ]),
        };
        let translated = provider.translate_message(&msg);
        assert_eq!(translated.len(), 1);
        let content = translated[0]
            .content
            .as_ref()
            .expect("content set")
            .as_str()
            .expect("must be a flat string when vision is unsupported");
        assert!(content.contains("Describe this image."));
        assert!(content.contains("ERROR"));
        assert!(content.contains("Restart") || content.contains("--provider"));
    }

    #[test]
    fn tool_result_text_only_blocks_stay_flat_string() {
        let provider = glm_provider_for_test();
        let msg = crate::types::ApiMessage {
            role: "user".to_string(),
            content: serde_json::json!([
                {
                    "type": "tool_result",
                    "tool_use_id": "tu_text",
                    "content": [
                        {"type": "text", "text": "row 1"},
                        {"type": "text", "text": "row 2"}
                    ]
                }
            ]),
        };
        let translated = provider.translate_message(&msg);
        let content = translated[0].content.as_ref().expect("content set");
        // No image_parts → keep the legacy flat-string shape even on a
        // vision-capable provider. The multi-part array is reserved for
        // payloads that actually carry images.
        assert!(
            content.is_string(),
            "text-only inner blocks must NOT be wrapped in a multi-part Array, \
             got: {content:?}"
        );
    }

    #[test]
    fn image_url_from_source_base64_image_default_media() {
        // Malformed image block (no media_type) → defaults to image/png so
        // downstream model preprocessing doesn't mis-parse the data URI.
        let block = serde_json::json!({
            "type": "image",
            "source": { "type": "base64", "data": "iVBORw0KGgo=" }
        });
        let url = image_url_from_source("image", &block).expect("must build url");
        assert!(url.starts_with("data:image/png;base64,"), "got: {url}");
    }

    #[test]
    fn image_url_from_source_base64_document_defaults_to_pdf() {
        // Malformed document block (no media_type) → defaults to
        // application/pdf so the URI doesn't lie about the payload.
        let block = serde_json::json!({
            "type": "document",
            "source": { "type": "base64", "data": "JVBERi0=" }
        });
        let url = image_url_from_source("document", &block).expect("must build url");
        assert!(
            url.starts_with("data:application/pdf;base64,"),
            "got: {url}"
        );
    }

    #[test]
    fn image_url_from_source_url_passthrough() {
        let block = serde_json::json!({
            "type": "image",
            "source": { "type": "url", "url": "https://example.com/x.png" }
        });
        let url = image_url_from_source("image", &block).expect("must build url");
        assert_eq!(url, "https://example.com/x.png");
    }

    #[test]
    fn image_url_from_source_empty_data_returns_none() {
        let block = serde_json::json!({
            "type": "image",
            "source": { "type": "base64", "data": "" }
        });
        assert!(image_url_from_source("image", &block).is_none());
    }

    #[test]
    fn image_url_from_source_unknown_source_type_returns_none() {
        let block = serde_json::json!({
            "type": "image",
            "source": { "type": "magic", "data": "x" }
        });
        assert!(image_url_from_source("image", &block).is_none());
    }

    /// Simulate an OpenAI SSE stream and verify the StreamAccumulator
    /// produces correct output. This tests the full pipeline:
    /// SSE bytes → process_openai_sse → StreamEvent → StreamAccumulator → Message
    #[tokio::test]
    async fn test_openai_sse_produces_valid_message() {
        use crate::StreamAccumulator;

        // Simulate a simple text response SSE stream.
        let _sse_data = "\
data: {\"id\":\"chatcmpl-1\",\"model\":\"gemma4\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"\"},\"finish_reason\":null}]}\n\n\
data: {\"id\":\"chatcmpl-1\",\"model\":\"gemma4\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Hello \"},\"finish_reason\":null}]}\n\n\
data: {\"id\":\"chatcmpl-1\",\"model\":\"gemma4\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"world!\"},\"finish_reason\":null}]}\n\n\
data: {\"id\":\"chatcmpl-1\",\"model\":\"gemma4\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":5,\"total_tokens\":15}}\n\n\
data: [DONE]\n\n";

        // Create a mock HTTP response from the SSE bytes.
        // We can't easily mock reqwest::Response, so test the accumulator
        // with manually constructed events instead. This tests the contract
        // that process_openai_sse must fulfill.
        let mut acc = StreamAccumulator::new();

        // MessageStart
        acc.on_event(&StreamEvent::MessageStart {
            id: "chatcmpl-1".into(),
            model: "gemma4".into(),
            usage: UsageInfo::default(),
        });

        // ContentBlockStart for text
        acc.on_event(&StreamEvent::ContentBlockStart {
            index: 0,
            content_block: ContentBlock::Text {
                text: String::new(),
            },
        });

        // Two text deltas
        acc.on_event(&StreamEvent::ContentBlockDelta {
            index: 0,
            delta: ContentDelta::TextDelta {
                text: "Hello ".into(),
            },
        });
        acc.on_event(&StreamEvent::ContentBlockDelta {
            index: 0,
            delta: ContentDelta::TextDelta {
                text: "world!".into(),
            },
        });

        // ContentBlockStop
        acc.on_event(&StreamEvent::ContentBlockStop { index: 0 });

        // MessageDelta with stop reason
        acc.on_event(&StreamEvent::MessageDelta {
            stop_reason: Some("end_turn".into()),
            usage: Some(UsageInfo {
                input_tokens: 10,
                output_tokens: 5,
                ..Default::default()
            }),
        });

        acc.on_event(&StreamEvent::MessageStop);

        let (msg, usage, stop) = acc.finish();
        assert_eq!(msg.get_text(), Some("Hello world!"));
        assert_eq!(stop.as_deref(), Some("end_turn"));
        assert_eq!(usage.output_tokens, 5);
    }

    #[tokio::test]
    async fn test_thinking_then_text_blocks() {
        use crate::StreamAccumulator;

        // Simulate: thinking block (index 0) → text block (index 1).
        // This is the sequence process_openai_sse must produce for Qwen3.
        let mut acc = StreamAccumulator::new();

        acc.on_event(&StreamEvent::MessageStart {
            id: "chatcmpl-2".into(),
            model: "qwen3-235b".into(),
            usage: UsageInfo::default(),
        });

        // Thinking block at index 0
        acc.on_event(&StreamEvent::ContentBlockStart {
            index: 0,
            content_block: ContentBlock::Thinking {
                thinking: String::new(),
                signature: String::new(),
            },
        });
        acc.on_event(&StreamEvent::ContentBlockDelta {
            index: 0,
            delta: ContentDelta::ThinkingDelta {
                thinking: "Let me think...".into(),
            },
        });
        acc.on_event(&StreamEvent::ContentBlockStop { index: 0 });

        // Text block at index 1
        acc.on_event(&StreamEvent::ContentBlockStart {
            index: 1,
            content_block: ContentBlock::Text {
                text: String::new(),
            },
        });
        acc.on_event(&StreamEvent::ContentBlockDelta {
            index: 1,
            delta: ContentDelta::TextDelta {
                text: "The answer is 42.".into(),
            },
        });
        acc.on_event(&StreamEvent::ContentBlockStop { index: 1 });

        acc.on_event(&StreamEvent::MessageDelta {
            stop_reason: Some("end_turn".into()),
            usage: None,
        });
        acc.on_event(&StreamEvent::MessageStop);

        let (msg, _, stop) = acc.finish();
        assert_eq!(msg.get_text(), Some("The answer is 42."));
        assert_eq!(stop.as_deref(), Some("end_turn"));
    }

    // Wire-payload snapshot tests: pin the JSON shape of translate_request
    // across the four thinking-dialect branches. Effort→budget mapping is the
    // canonical table in crates/core/src/effort.rs.

    /// Translate a thinking request through the given preset and return the serialized wire payload.
    fn snapshot_wire_payload(
        provider_preset: &str,
        model: &str,
        budget: Option<u32>,
    ) -> serde_json::Value {
        let provider = OpenAiProvider::new(OpenAiProviderConfig::from_loaded(
            crate::providers::loader::registry()
                .find(provider_preset)
                .unwrap_or_else(|| panic!("unknown provider preset '{}'", provider_preset)),
            "test-key-long-enough".to_string(),
            Some(model.to_string()),
        ))
        .unwrap();

        let req = CreateMessageRequest {
            model: model.to_string(),
            max_tokens: 4096,
            messages: vec![crate::types::ApiMessage {
                role: "user".to_string(),
                content: Value::String("Think about this".to_string()),
            }],
            system: None,
            tools: None,
            temperature: None,
            top_p: None,
            top_k: None,
            stop_sequences: None,
            stream: true,
            thinking: budget.map(crate::types::ThinkingConfig::enabled),
            output_config: None,
        };

        let openai_req = provider.translate_request(&req);
        serde_json::to_value(&openai_req).expect("OpenAiRequest must serialize")
    }

    /// None = field absent; Some = field present with the given value.
    fn assert_thinking_fields(
        payload: &serde_json::Value,
        enable_thinking: Option<bool>,
        thinking_budget: Option<u64>,
        think: Option<bool>,
        thinking_nested: Option<u64>,
    ) {
        let obj = payload.as_object().expect("payload must be a JSON object");

        match enable_thinking {
            Some(b) => assert_eq!(
                obj.get("enable_thinking"),
                Some(&serde_json::Value::Bool(b)),
                "enable_thinking expected {} in payload {:?}",
                b,
                obj
            ),
            None => assert!(
                !obj.contains_key("enable_thinking"),
                "enable_thinking must be ABSENT but found {:?} in {:?}",
                obj.get("enable_thinking"),
                obj
            ),
        }

        match thinking_budget {
            Some(n) => assert_eq!(
                obj.get("thinking_budget").and_then(|v| v.as_u64()),
                Some(n),
                "thinking_budget expected {} in payload {:?}",
                n,
                obj
            ),
            None => assert!(
                !obj.contains_key("thinking_budget"),
                "thinking_budget must be ABSENT but found {:?} in {:?}",
                obj.get("thinking_budget"),
                obj
            ),
        }

        match think {
            Some(b) => assert_eq!(
                obj.get("think"),
                Some(&serde_json::Value::Bool(b)),
                "think expected {} in payload {:?}",
                b,
                obj
            ),
            None => assert!(
                !obj.contains_key("think"),
                "think must be ABSENT but found {:?} in {:?}",
                obj.get("think"),
                obj
            ),
        }

        match thinking_nested {
            Some(n) => {
                let nested = obj
                    .get("thinking")
                    .expect("thinking nested object expected to be present");
                let nested_obj = nested
                    .as_object()
                    .expect("thinking field must be a JSON object");
                assert_eq!(
                    nested_obj.get("type").and_then(|v| v.as_str()),
                    Some("enabled"),
                    "thinking.type must equal 'enabled' (Anthropic spec)"
                );
                assert_eq!(
                    nested_obj.get("budget_tokens").and_then(|v| v.as_u64()),
                    Some(n),
                    "thinking.budget_tokens expected {} got {:?}",
                    n,
                    nested_obj.get("budget_tokens")
                );
            }
            None => assert!(
                !obj.contains_key("thinking"),
                "thinking nested object must be ABSENT but found {:?} in {:?}",
                obj.get("thinking"),
                obj
            ),
        }
    }

    // ── alibaba (Qwen3 dialect) ────────────────────────────────────────

    #[test]
    fn snapshot_alibaba_none_emits_enable_thinking_false_no_budget() {
        let payload = snapshot_wire_payload("alibaba", "qwen3-235b-a22b", None);
        assert_thinking_fields(&payload, Some(false), None, None, None);
    }

    #[test]
    fn snapshot_alibaba_low_emits_enable_thinking_true_budget_8000() {
        let payload = snapshot_wire_payload("alibaba", "qwen3-235b-a22b", Some(8_000));
        assert_thinking_fields(&payload, Some(true), Some(8_000), None, None);
    }

    #[test]
    fn snapshot_alibaba_medium_emits_enable_thinking_true_budget_16000() {
        let payload = snapshot_wire_payload("alibaba", "qwen3-235b-a22b", Some(16_000));
        assert_thinking_fields(&payload, Some(true), Some(16_000), None, None);
    }

    #[test]
    fn snapshot_alibaba_high_emits_enable_thinking_true_budget_32000() {
        let payload = snapshot_wire_payload("alibaba", "qwen3-235b-a22b", Some(32_000));
        assert_thinking_fields(&payload, Some(true), Some(32_000), None, None);
    }

    #[test]
    fn snapshot_alibaba_max_emits_enable_thinking_true_budget_64000() {
        let payload = snapshot_wire_payload("alibaba", "qwen3-235b-a22b", Some(64_000));
        assert_thinking_fields(&payload, Some(true), Some(64_000), None, None);
    }

    // ── glm (AnthropicNested dialect) ──────────────────────────────────

    #[test]
    fn snapshot_glm_none_emits_nothing() {
        let payload = snapshot_wire_payload("glm", "glm-5", None);
        assert_thinking_fields(&payload, None, None, None, None);
    }

    #[test]
    fn snapshot_glm_low_emits_anthropic_nested_budget_8000() {
        let payload = snapshot_wire_payload("glm", "glm-5", Some(8_000));
        assert_thinking_fields(&payload, None, None, None, Some(8_000));
    }

    #[test]
    fn snapshot_glm_medium_emits_anthropic_nested_budget_16000() {
        let payload = snapshot_wire_payload("glm", "glm-5", Some(16_000));
        assert_thinking_fields(&payload, None, None, None, Some(16_000));
    }

    #[test]
    fn snapshot_glm_high_emits_anthropic_nested_budget_32000() {
        let payload = snapshot_wire_payload("glm", "glm-5", Some(32_000));
        assert_thinking_fields(&payload, None, None, None, Some(32_000));
    }

    #[test]
    fn snapshot_glm_max_emits_anthropic_nested_budget_64000() {
        let payload = snapshot_wire_payload("glm", "glm-5", Some(64_000));
        assert_thinking_fields(&payload, None, None, None, Some(64_000));
    }

    // ── ollama (OllamaThink dialect) ───────────────────────────────────

    #[test]
    fn snapshot_ollama_none_emits_think_false() {
        // Ollama's `think` is a bool — no budget channel exists. Even with
        // no --effort, the flag is sent to explicitly disable.
        let payload = snapshot_wire_payload("ollama", "qwen3:14b", None);
        assert_thinking_fields(&payload, None, None, Some(false), None);
    }

    #[test]
    fn snapshot_ollama_low_emits_think_true_budget_ignored() {
        let payload = snapshot_wire_payload("ollama", "qwen3:14b", Some(8_000));
        assert_thinking_fields(&payload, None, None, Some(true), None);
    }

    #[test]
    fn snapshot_ollama_max_emits_think_true_budget_ignored() {
        let payload = snapshot_wire_payload("ollama", "qwen3:14b", Some(64_000));
        assert_thinking_fields(&payload, None, None, Some(true), None);
    }

    #[test]
    fn snapshot_mistral_none_emits_nothing() {
        let payload = snapshot_wire_payload("mistral", "mistral-large-latest", None);
        assert_thinking_fields(&payload, None, None, None, None);
    }

    #[test]
    fn snapshot_mistral_max_emits_nothing_even_with_effort() {
        let payload = snapshot_wire_payload("mistral", "mistral-large-latest", Some(64_000));
        assert_thinking_fields(&payload, None, None, None, None);
    }

    #[test]
    fn snapshot_openai_max_emits_nothing_even_with_effort() {
        let payload = snapshot_wire_payload("openai", "default", Some(64_000));
        assert_thinking_fields(&payload, None, None, None, None);
    }

    #[test]
    fn snapshot_openrouter_max_emits_nothing_even_with_effort() {
        // OpenRouter declares no thinking_format — --effort must drop
        // silently rather than leak a foreign dialect on the wire.
        let payload = snapshot_wire_payload("openrouter", "qwen/qwen3.6-plus", Some(64_000));
        assert_thinking_fields(&payload, None, None, None, None);
    }
}
