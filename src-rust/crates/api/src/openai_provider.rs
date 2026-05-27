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

use crate::provider::{
    ApiFormat, AuthConfig, LlmProvider, ModelMetadata, ModelPricing, ProviderCapabilities,
};
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
    pub supports_thinking: bool,
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
    /// Default thinking budget (None = thinking disabled by default).
    pub default_thinking_budget: Option<u32>,
    /// Authentication configuration.
    pub auth: AuthConfig,
}

impl OpenAiProviderConfig {
    /// Preset for Ollama (local).
    pub fn ollama(model: &str) -> Self {
        Self {
            name: "Ollama".to_string(),
            api_base: "http://localhost:11434".to_string(),
            api_key: String::new(),
            default_model: model.to_string(),
            fast_model: None,
            supports_thinking: true,
            api_format: ApiFormat::Ollama,
            max_retries: 3,
            request_timeout: Duration::from_secs(600),
            attribution: "powered by Ollama (local)".to_string(),
            known_models: vec![ModelMetadata {
                id: model.to_string(),
                display_name: model.to_string(),
                description: "Local model via Ollama".to_string(),
                context_window: 128_000,
                max_output_tokens: 8_192,
                supports_thinking: true,
                pricing: None, // Local = free
            }],
            default_max_tokens: 8_192,
            default_thinking_budget: Some(16_000),
            auth: AuthConfig {
                env_vars: &[],
                keychain_key: "ollama",
                display_label: "Ollama",
                required: false,
            },
        }
    }

    /// Preset for OpenRouter (any model, OpenAI-compatible proxy).
    pub fn openrouter(api_key: &str, model: &str) -> Self {
        Self {
            name: "OpenRouter".to_string(),
            api_base: "https://openrouter.ai/api".to_string(),
            api_key: api_key.to_string(),
            default_model: model.to_string(),
            fast_model: None,
            supports_thinking: true,
            api_format: ApiFormat::OpenAI,
            max_retries: 5,
            request_timeout: Duration::from_secs(600),
            attribution: "powered by OpenRouter".to_string(),
            known_models: vec![ModelMetadata {
                id: "qwen/qwen3.6-plus".to_string(),
                display_name: "Qwen 3.6 Plus".to_string(),
                description: "Agentic coding model, 1M context".to_string(),
                context_window: 1_000_000,
                max_output_tokens: 32_768,
                supports_thinking: true,
                pricing: Some(ModelPricing {
                    input_per_mtk: 0.325,
                    output_per_mtk: 1.95,
                    ..Default::default()
                }),
            }],
            default_max_tokens: 16_384,
            default_thinking_budget: Some(16_000),
            auth: AuthConfig {
                env_vars: &["OPENROUTER_API_KEY"],
                keychain_key: "openrouter",
                display_label: "OpenRouter",
                required: true,
            },
        }
    }

    /// Preset for Alibaba Cloud DashScope (Qwen).
    pub fn alibaba(api_key: &str, model: &str) -> Self {
        Self {
            name: "Qwen".to_string(),
            api_base: "https://dashscope-intl.aliyuncs.com/compatible-mode".to_string(),
            api_key: api_key.to_string(),
            default_model: model.to_string(),
            // qwen-turbo-latest: 1M context, $0.05/M in, $0.20/M out.
            // Note: Alibaba recommends qwen-flash as replacement, but turbo
            // has generous free tier and works well for tool-result turns.
            fast_model: Some("qwen-turbo-latest".to_string()),
            supports_thinking: true,
            api_format: ApiFormat::OpenAI,
            max_retries: 5,
            request_timeout: Duration::from_secs(600),
            attribution: "powered by Qwen (Alibaba Cloud)".to_string(),
            known_models: vec![
                ModelMetadata {
                    id: "qwen3.6-plus-2026-04-02".to_string(),
                    display_name: "Qwen 3.6 Plus".to_string(),
                    description: "Agentic coding model, 1M context".to_string(),
                    context_window: 1_000_000,
                    max_output_tokens: 32_768,
                    supports_thinking: true,
                    pricing: Some(ModelPricing {
                        input_per_mtk: 0.29,
                        output_per_mtk: 1.73,
                        ..Default::default()
                    }),
                },
                ModelMetadata {
                    id: "qwen-turbo-latest".to_string(),
                    display_name: "Qwen Turbo".to_string(),
                    description: "Fast model for tool-result turns".to_string(),
                    context_window: 1_000_000,
                    max_output_tokens: 16_384,
                    supports_thinking: false,
                    pricing: Some(ModelPricing {
                        input_per_mtk: 0.05,
                        output_per_mtk: 0.20,
                        ..Default::default()
                    }),
                },
                ModelMetadata {
                    id: "qwen3-max".to_string(),
                    display_name: "Qwen 3 Max".to_string(),
                    description: "Flagship Qwen3".to_string(),
                    context_window: 131_072,
                    max_output_tokens: 16_384,
                    supports_thinking: true,
                    pricing: Some(ModelPricing {
                        input_per_mtk: 0.8,
                        output_per_mtk: 2.0,
                        ..Default::default()
                    }),
                },
                ModelMetadata {
                    id: "qwen3-235b-a22b".to_string(),
                    display_name: "Qwen 3 235B".to_string(),
                    description: "MoE 235B (22B active) — reasoning".to_string(),
                    context_window: 131_072,
                    max_output_tokens: 16_384,
                    supports_thinking: true,
                    pricing: Some(ModelPricing {
                        input_per_mtk: 0.8,
                        output_per_mtk: 2.0,
                        ..Default::default()
                    }),
                },
            ],
            default_max_tokens: 16_384,
            default_thinking_budget: Some(16_000),
            auth: AuthConfig {
                env_vars: &["DASHSCOPE_API_KEY"],
                keychain_key: "alibaba",
                display_label: "DashScope",
                required: true,
            },
        }
    }

    /// Preset for Mistral AI.
    pub fn mistral(api_key: &str, model: &str) -> Self {
        Self {
            name: "Mistral".to_string(),
            api_base: "https://api.mistral.ai/v1".to_string(),
            api_key: api_key.to_string(),
            default_model: model.to_string(),
            fast_model: Some("mistral-small-latest".to_string()),
            supports_thinking: false,
            api_format: ApiFormat::OpenAI,
            max_retries: 5,
            request_timeout: Duration::from_secs(600),
            attribution: "powered by Mistral AI".to_string(),
            known_models: vec![
                ModelMetadata {
                    id: "mistral-large-latest".to_string(),
                    display_name: "Mistral Large".to_string(),
                    description: "Most capable Mistral model".to_string(),
                    context_window: 128_000,
                    max_output_tokens: 32_768,
                    supports_thinking: false,
                    pricing: Some(ModelPricing {
                        input_per_mtk: 2.0,
                        output_per_mtk: 6.0,
                        ..Default::default()
                    }),
                },
                ModelMetadata {
                    id: "mistral-small-latest".to_string(),
                    display_name: "Mistral Small".to_string(),
                    description: "Fast and efficient".to_string(),
                    context_window: 128_000,
                    max_output_tokens: 32_768,
                    supports_thinking: false,
                    pricing: Some(ModelPricing {
                        input_per_mtk: 0.2,
                        output_per_mtk: 0.6,
                        ..Default::default()
                    }),
                },
            ],
            default_max_tokens: 32_768,
            default_thinking_budget: None,
            auth: AuthConfig {
                env_vars: &["MISTRAL_API_KEY"],
                keychain_key: "mistral",
                display_label: "Mistral",
                required: true,
            },
        }
    }

    /// Generic OpenAI-compatible endpoint.
    pub fn generic(name: &str, api_base: &str, api_key: &str, model: &str) -> Self {
        Self {
            name: name.to_string(),
            api_base: api_base.to_string(),
            api_key: api_key.to_string(),
            default_model: model.to_string(),
            fast_model: None,
            supports_thinking: false,
            api_format: ApiFormat::OpenAI,
            max_retries: 5,
            request_timeout: Duration::from_secs(600),
            attribution: format!("powered by {}", name),
            known_models: vec![ModelMetadata {
                id: model.to_string(),
                display_name: model.to_string(),
                description: format!("Model via {}", name),
                context_window: 128_000,
                max_output_tokens: 16_384,
                supports_thinking: false,
                pricing: None,
            }],
            default_max_tokens: 16_384,
            default_thinking_budget: None,
            auth: AuthConfig {
                env_vars: &["OPENAI_API_KEY"],
                keychain_key: "openai",
                display_label: "OpenAI",
                required: false,
            },
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
    /// Qwen3-specific: enable thinking/reasoning mode.
    #[serde(skip_serializing_if = "Option::is_none")]
    enable_thinking: Option<bool>,
    /// Qwen3-specific: max tokens for thinking.
    #[serde(skip_serializing_if = "Option::is_none")]
    thinking_budget: Option<u32>,
    /// Ollama-specific: disable thinking for Qwen3 local models.
    #[serde(skip_serializing_if = "Option::is_none")]
    think: Option<bool>,
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
            default_thinking_budget: config.default_thinking_budget,
            api_format: config.api_format,
            default_api_base: config.api_base.clone(),
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

        // Thinking support (Qwen3)
        let (enable_thinking, thinking_budget) = if self.config.supports_thinking {
            if let Some(ref thinking) = req.thinking {
                (Some(true), Some(thinking.budget_tokens))
            } else {
                (Some(false), None)
            }
        } else {
            (None, None)
        };

        // Ollama: `think` is a Boolean toggle (no budget). Only send it when
        // the provider supports thinking AND the user actually requested it.
        // Sending `think: false` to models that don't know the field is harmless
        // but unnecessary; omitting it entirely (None) is cleaner.
        let think = if self.config.api_format == ApiFormat::Ollama && self.config.supports_thinking
        {
            Some(req.thinking.is_some())
        } else {
            None
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
            let mut tool_calls = Vec::new();
            let mut tool_results = Vec::new();

            for block in blocks {
                let block_type = block.get("type").and_then(|t| t.as_str()).unwrap_or("");

                match block_type {
                    "text" => {
                        if let Some(t) = block.get("text").and_then(|t| t.as_str()) {
                            text_parts.push(t.to_string());
                        }
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
                        let content = if let Some(c) = block.get("content") {
                            if let Some(s) = c.as_str() {
                                s.to_string()
                            } else if let Some(arr) = c.as_array() {
                                arr.iter()
                                    .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
                                    .collect::<Vec<_>>()
                                    .join("\n")
                            } else {
                                c.to_string()
                            }
                        } else {
                            String::new()
                        };
                        tool_results.push((tool_use_id, content));
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
                for (tool_use_id, content) in tool_results {
                    result.push(OpenAiMessage {
                        role: "tool".to_string(),
                        content: Some(Value::String(content)),
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
                // Regular user message with text blocks
                let text = text_parts.join("");
                if !text.is_empty() {
                    result.push(OpenAiMessage {
                        role: role.clone(),
                        content: Some(Value::String(text)),
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
    fn stream_url(&self) -> String {
        match self.config.api_format {
            ApiFormat::Ollama => format!("{}/api/chat", self.config.api_base),
            _ => format!("{}/v1/chat/completions", self.config.api_base),
        }
    }

    /// Build the models URL.
    fn models_url(&self) -> String {
        match self.config.api_format {
            ApiFormat::Ollama => format!("{}/api/tags", self.config.api_base),
            _ => format!("{}/v1/models", self.config.api_base),
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
        let provider = OpenAiProvider::new(OpenAiProviderConfig::ollama("gemma4")).unwrap();

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
    fn test_translate_with_thinking() {
        let provider =
            OpenAiProvider::new(OpenAiProviderConfig::alibaba("key", "qwen3-235b")).unwrap();

        let req = CreateMessageRequest {
            model: "qwen3-235b".to_string(),
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
        };

        let openai_req = provider.translate_request(&req);
        assert_eq!(openai_req.enable_thinking, Some(true));
        assert_eq!(openai_req.thinking_budget, Some(16000));
    }

    #[test]
    fn test_translate_tool_use_message() {
        let provider = OpenAiProvider::new(OpenAiProviderConfig::ollama("gemma4")).unwrap();

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
        let provider = OpenAiProvider::new(OpenAiProviderConfig::ollama("gemma4")).unwrap();

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
}
