// cc-api: Anthropic API client with streaming SSE support for the Uppli Code
// Rust port.
//
// Handles:
// - POST /v1/messages with streaming
// - SSE event parsing (message_start, content_block_start, content_block_delta,
//   content_block_stop, message_delta, message_stop, error)
// - Delta types: text_delta, input_json_delta, thinking_delta, signature_delta
// - Rate-limit (429) and overloaded (529) retry with exponential back-off
// - Authentication via API key from env or config

use cc_core::constants::{ANTHROPIC_API_VERSION, ANTHROPIC_BETA_HEADER};
use cc_core::error::ClaudeError;
use cc_core::types::{ContentBlock, Message, MessageContent, Role, ToolDefinition, UsageInfo};
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tracing::{debug, warn};

// ---------------------------------------------------------------------------
// Public re-exports
// ---------------------------------------------------------------------------
pub use client::AnthropicClient;
pub use openai_provider::{OpenAiProvider, OpenAiProviderConfig};
pub use provider::{
    ApiFormat, AuthConfig, LlmProvider, ModelMetadata, ModelPricing, ProviderCapabilities,
    ProviderPreset,
};
pub use provider_factory::{default_preset, find_preset, provider_registry};
pub use streaming::{StreamEvent, StreamHandler};
pub use types::*;

// ---------------------------------------------------------------------------
// Abstract LLM provider trait
// ---------------------------------------------------------------------------
pub mod provider;

// ---------------------------------------------------------------------------
// Provider TOML registry (replaces the hardcoded preset functions and the
// static REGISTRY array). One .toml file per provider in crates/api/presets/.
// ---------------------------------------------------------------------------
pub mod providers;

// ---------------------------------------------------------------------------
// OpenAI-compatible provider (Ollama, Alibaba/DashScope, generic)
// ---------------------------------------------------------------------------
pub mod openai_provider;

// ---------------------------------------------------------------------------
// Provider factory — create a provider from config
// ---------------------------------------------------------------------------
pub mod provider_factory;
pub use provider_factory::create_provider;

// ---------------------------------------------------------------------------
// request / response types
// ---------------------------------------------------------------------------
pub mod types {
    use super::*;

    /// The request body sent to `POST /v1/messages`.
    #[derive(Debug, Clone, Serialize)]
    pub struct CreateMessageRequest {
        pub model: String,
        pub max_tokens: u32,
        pub messages: Vec<ApiMessage>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub system: Option<SystemPrompt>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub tools: Option<Vec<ApiToolDefinition>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub temperature: Option<f32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub top_p: Option<f32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub top_k: Option<u32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub stop_sequences: Option<Vec<String>>,
        pub stream: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub thinking: Option<ThinkingConfig>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub output_config: Option<OutputConfig>,
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct ThinkingConfig {
        #[serde(rename = "type")]
        pub thinking_type: String,
        pub budget_tokens: u32,
    }

    impl ThinkingConfig {
        pub fn enabled(budget: u32) -> Self {
            Self {
                thinking_type: "enabled".to_string(),
                budget_tokens: if budget < 4000 { 16000 } else { budget },
            }
        }
    }

    /// Provider-specific reasoning effort control (Anthropic-format field).
    ///
    /// DeepSeek's Anthropic-compatible API ignores `thinking.budget_tokens` and
    /// instead reads `output_config.effort` to set the reasoning depth.
    /// See https://api-docs.deepseek.com/guides/thinking_mode
    ///
    /// Accepted effort values per DeepSeek docs: `"high"` (default) or `"max"`.
    /// Anthropic's own API silently ignores this field, so it is safe to send
    /// on either backend.
    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct OutputConfig {
        #[serde(skip_serializing_if = "Option::is_none")]
        pub effort: Option<String>,
    }

    impl OutputConfig {
        pub fn effort(level: impl Into<String>) -> Self {
            Self {
                effort: Some(level.into()),
            }
        }
    }

    /// System prompt - either a single string or structured blocks with cache.
    #[derive(Debug, Clone, Serialize, Deserialize)]
    #[serde(untagged)]
    pub enum SystemPrompt {
        Text(String),
        Blocks(Vec<SystemBlock>),
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct SystemBlock {
        #[serde(rename = "type")]
        pub block_type: String,
        pub text: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub cache_control: Option<CacheControl>,
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct CacheControl {
        #[serde(rename = "type")]
        pub control_type: String,
    }

    impl CacheControl {
        pub fn ephemeral() -> Self {
            Self {
                control_type: "ephemeral".to_string(),
            }
        }
    }

    /// Simplified message type for the API wire format.
    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct ApiMessage {
        pub role: String,
        pub content: Value,
    }

    impl From<&Message> for ApiMessage {
        fn from(msg: &Message) -> Self {
            let role = match msg.role {
                Role::User => "user",
                Role::Assistant => "assistant",
            };
            let content = match &msg.content {
                MessageContent::Text(t) => Value::String(t.clone()),
                MessageContent::Blocks(blocks) => {
                    serde_json::to_value(blocks).unwrap_or(Value::Null)
                }
            };
            Self {
                role: role.to_string(),
                content,
            }
        }
    }

    /// Tool definition in the API wire format.
    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct ApiToolDefinition {
        pub name: String,
        pub description: String,
        pub input_schema: Value,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub cache_control: Option<CacheControl>,
    }

    impl From<&ToolDefinition> for ApiToolDefinition {
        fn from(td: &ToolDefinition) -> Self {
            Self {
                name: td.name.clone(),
                description: td.description.clone(),
                input_schema: td.input_schema.clone(),
                cache_control: None,
            }
        }
    }

    /// Non-streaming response from `POST /v1/messages`.
    #[derive(Debug, Clone, Deserialize)]
    pub struct CreateMessageResponse {
        pub id: String,
        #[serde(rename = "type")]
        pub response_type: String,
        pub role: String,
        pub content: Vec<Value>,
        pub model: String,
        pub stop_reason: Option<String>,
        pub stop_sequence: Option<String>,
        pub usage: UsageInfo,
    }

    /// Error body returned by the API.
    #[derive(Debug, Clone, Deserialize)]
    pub struct ApiErrorResponse {
        #[serde(rename = "type")]
        pub error_type: String,
        pub error: ApiErrorDetail,
    }

    #[derive(Debug, Clone, Deserialize)]
    pub struct ApiErrorDetail {
        #[serde(rename = "type")]
        pub error_type: String,
        pub message: String,
    }
}

// ---------------------------------------------------------------------------
// SSE streaming types
// ---------------------------------------------------------------------------
pub mod streaming {
    use super::*;

    /// Events emitted by the streaming SSE parser.
    #[derive(Debug, Clone)]
    pub enum StreamEvent {
        /// The overall message has started; carries the message id and model.
        MessageStart {
            id: String,
            model: String,
            usage: UsageInfo,
        },
        /// A new content block has begun.
        ContentBlockStart {
            index: usize,
            content_block: ContentBlock,
        },
        /// Incremental delta for an existing content block.
        ContentBlockDelta { index: usize, delta: ContentDelta },
        /// A content block is finished.
        ContentBlockStop { index: usize },
        /// Final message-level delta (stop_reason, usage).
        MessageDelta {
            stop_reason: Option<String>,
            usage: Option<UsageInfo>,
        },
        /// The message is complete.
        MessageStop,
        /// An error occurred during streaming.
        Error { error_type: String, message: String },
        /// A ping/keep-alive event.
        Ping,
    }

    /// The delta payload inside a `content_block_delta` event.
    #[derive(Debug, Clone, Serialize, Deserialize)]
    #[serde(tag = "type", rename_all = "snake_case")]
    pub enum ContentDelta {
        TextDelta { text: String },
        InputJsonDelta { partial_json: String },
        ThinkingDelta { thinking: String },
        SignatureDelta { signature: String },
    }

    /// Trait for anything that wants to consume streaming events in real time.
    pub trait StreamHandler: Send + Sync {
        fn on_event(&self, event: &StreamEvent);
    }

    /// A no-op handler useful for non-interactive / batch mode.
    pub struct NullStreamHandler;
    impl StreamHandler for NullStreamHandler {
        fn on_event(&self, _event: &StreamEvent) {}
    }
}

// ---------------------------------------------------------------------------
// SSE line parser
// ---------------------------------------------------------------------------
mod sse_parser {
    /// Parsed SSE frame.
    #[derive(Debug)]
    pub struct SseFrame {
        pub event: Option<String>,
        pub data: String,
    }

    /// Incrementally accumulates raw bytes/lines and yields complete frames.
    pub struct SseLineParser {
        event_type: Option<String>,
        data_buf: String,
    }

    impl SseLineParser {
        pub fn new() -> Self {
            Self {
                event_type: None,
                data_buf: String::new(),
            }
        }

        /// Feed one line (without the trailing newline).  Returns `Some(frame)`
        /// when a blank line signals the end of an event.
        pub fn feed_line(&mut self, line: &str) -> Option<SseFrame> {
            if line.is_empty() {
                // Blank line = end of event
                if self.data_buf.is_empty() && self.event_type.is_none() {
                    return None; // spurious blank line
                }
                let frame = SseFrame {
                    event: self.event_type.take(),
                    data: std::mem::take(&mut self.data_buf),
                };
                return Some(frame);
            }

            if let Some(rest) = line.strip_prefix("event:") {
                self.event_type = Some(rest.trim().to_string());
            } else if let Some(rest) = line.strip_prefix("data:") {
                if !self.data_buf.is_empty() {
                    self.data_buf.push('\n');
                }
                self.data_buf.push_str(rest.trim());
            } else if line.starts_with(':') {
                // SSE comment / keep-alive – ignore
            }

            None
        }
    }
}

// ---------------------------------------------------------------------------
// Models endpoint types (public)
// ---------------------------------------------------------------------------

/// A model entry returned by `GET /v1/models`.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct AvailableModel {
    pub id: String,
    pub display_name: Option<String>,
    /// Unix timestamp of when the model was created (seconds).
    pub created_at: Option<i64>,
}

// ---------------------------------------------------------------------------
// Anthropic client
// ---------------------------------------------------------------------------
pub mod client {
    use super::*;

    /// Configuration for the HTTP client.
    #[derive(Debug, Clone)]
    pub struct ClientConfig {
        pub api_key: String,
        pub api_base: String,
        pub api_version: String,
        pub beta_features: String,
        pub max_retries: u32,
        pub initial_retry_delay: Duration,
        pub max_retry_delay: Duration,
        pub request_timeout: Duration,
        /// When true, send `Authorization: Bearer <api_key>` instead of `x-api-key`.
        /// Used for Claude.ai subscription (OAuth user:inference scope) tokens.
        pub use_bearer_auth: bool,
    }

    impl Default for ClientConfig {
        fn default() -> Self {
            Self {
                api_key: String::new(),
                api_base: cc_core::constants::ANTHROPIC_API_BASE.to_string(),
                api_version: ANTHROPIC_API_VERSION.to_string(),
                beta_features: ANTHROPIC_BETA_HEADER.to_string(),
                max_retries: 8,
                initial_retry_delay: Duration::from_secs(2),
                max_retry_delay: Duration::from_secs(30),
                request_timeout: Duration::from_secs(600),
                use_bearer_auth: false,
            }
        }
    }

    /// The main Anthropic API client.
    ///
    /// The capabilities (model list, pricing, supports_vision, auth) are
    /// loaded ONCE from the TOML preset (`crates/api/presets/<name>.toml`)
    /// at construction time and stored here. There is no second source of
    /// truth — editing the TOML edits the runtime behaviour. The previous
    /// design hard-coded the same caps in a `OnceLock`, which forced a
    /// "TOML matches static" consistency test to guard against drift.
    /// That duplication is now gone.
    pub struct AnthropicClient {
        http: reqwest::Client,
        config: ClientConfig,
        pub(crate) capabilities: provider::ProviderCapabilities,
    }

    impl AnthropicClient {
        /// Build a new client given an HTTP config and a fully-built
        /// `ProviderCapabilities`. The caps are usually produced by the
        /// TOML loader (`providers::loader::registry().find("deepseek")`),
        /// but tests may pass a hand-built struct.
        pub fn new(
            config: ClientConfig,
            capabilities: provider::ProviderCapabilities,
        ) -> anyhow::Result<Self> {
            if config.api_key.is_empty() {
                return Err(anyhow::anyhow!(
                    "Anthropic API key is required. Set ANTHROPIC_API_KEY or pass --api-key."
                ));
            }

            let http = reqwest::Client::builder()
                .timeout(config.request_timeout)
                .build()?;

            Ok(Self {
                http,
                config,
                capabilities,
            })
        }

        /// Convenience constructor that fetches the DeepSeek preset from
        /// the TOML loader and resolves the API key from config / env.
        /// Single line of indirection between deepseek.toml and the
        /// runtime client.
        pub fn from_config(cfg: &cc_core::config::Config) -> anyhow::Result<Self> {
            let api_key = cfg
                .resolve_api_key()
                .ok_or_else(|| anyhow::anyhow!("No API key found"))?;
            let api_base = cfg.resolve_api_base();
            let loaded = crate::providers::loader::registry()
                .find("deepseek")
                .ok_or_else(|| anyhow::anyhow!("deepseek preset missing from TOML registry"))?;

            Self::new(
                ClientConfig {
                    api_key,
                    api_base,
                    ..Default::default()
                },
                loaded.capabilities.clone(),
            )
        }

        // ---- Provider-side block degradation -----------------------------
        //
        // The CLI is provider-agnostic — it always emits the richest
        // representation (Image, Document, ...). It is the PROVIDER's job
        // to translate or degrade those blocks for the model behind it.
        // For AnthropicClient, the wire format IS Anthropic native, so:
        //   - When the model supports vision → forward blocks verbatim.
        //   - When it doesn't (DeepSeek today) → replace Image and
        //     Document blocks with a Text block carrying a caption,
        //     across both user messages and nested tool_result.Blocks.
        // The model never sees a block it can't read, but the textual
        // signal is preserved so it can still reason about the payload.
        pub(crate) fn degrade_blocks_if_needed(&self, request: &mut CreateMessageRequest) {
            // Direct field access — the trait method has the same name
            // so calling `self.capabilities()` here would recurse.
            if self.capabilities.supports_vision {
                return;
            }
            for msg in request.messages.iter_mut() {
                degrade_value(&mut msg.content);
            }
        }

        // ---- Non-streaming create message --------------------------------

        /// Send a non-streaming `POST /v1/messages` and return the full response.
        pub async fn create_message(
            &self,
            mut request: CreateMessageRequest,
        ) -> Result<CreateMessageResponse, ClaudeError> {
            request.stream = false;
            self.degrade_blocks_if_needed(&mut request);
            let body = serde_json::to_value(&request).map_err(ClaudeError::Json)?;

            let resp = self.send_with_retry(&body).await?;
            let status = resp.status();
            let text = resp.text().await.map_err(ClaudeError::Http)?;

            if !status.is_success() {
                return Err(self.parse_api_error(status.as_u16(), &text));
            }

            serde_json::from_str(&text).map_err(ClaudeError::Json)
        }

        // ---- Streaming create message ------------------------------------

        /// Send a streaming `POST /v1/messages`.  Events are dispatched to the
        /// provided `handler` in real time, and also forwarded into the returned
        /// channel so the caller can drive a select loop.
        pub async fn create_message_stream(
            &self,
            mut request: CreateMessageRequest,
            handler: Arc<dyn StreamHandler>,
        ) -> Result<mpsc::Receiver<StreamEvent>, ClaudeError> {
            request.stream = true;
            self.degrade_blocks_if_needed(&mut request);
            let body = serde_json::to_value(&request).map_err(ClaudeError::Json)?;

            let resp = self.send_with_retry(&body).await?;
            let status = resp.status();

            if !status.is_success() {
                let text = resp.text().await.map_err(ClaudeError::Http)?;
                return Err(self.parse_api_error(status.as_u16(), &text));
            }

            let (tx, rx) = mpsc::channel(256);

            // Spawn a task that reads the SSE byte stream and emits events.
            tokio::spawn(async move {
                if let Err(e) = Self::process_sse_stream(resp, handler, tx.clone()).await {
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

        // ---- Models list ------------------------------------------------

        /// Fetch available models from `GET /v1/models`.
        ///
        /// Returns a list of models the current API key has access to.
        /// Falls back gracefully: returns an empty `Vec` on any error so
        /// callers can fall back to the hardcoded default list instead of
        /// surfacing an error.
        pub async fn fetch_available_models(&self) -> anyhow::Result<Vec<crate::AvailableModel>> {
            let url = format!("{}/v1/models", self.config.api_base);

            let mut req = self
                .http
                .get(&url)
                .header("anthropic-version", &self.config.api_version)
                .header("content-type", "application/json");
            req = if self.config.use_bearer_auth {
                req.header("Authorization", format!("Bearer {}", &self.config.api_key))
            } else {
                req.header("x-api-key", &self.config.api_key)
            };

            let resp = req.send().await?;

            if !resp.status().is_success() {
                anyhow::bail!("models endpoint returned {}", resp.status());
            }

            #[derive(serde::Deserialize)]
            struct ModelsResponse {
                data: Vec<crate::AvailableModel>,
            }

            let body: ModelsResponse = resp.json().await?;
            Ok(body.data)
        }

        // ---- Internal helpers --------------------------------------------

        /// Build the common request and execute with retry logic.
        async fn send_with_retry(&self, body: &Value) -> Result<reqwest::Response, ClaudeError> {
            let url = format!("{}/v1/messages", self.config.api_base);
            let mut attempts = 0u32;
            let mut delay = self.config.initial_retry_delay;

            loop {
                attempts += 1;

                // Use Bearer auth for Claude.ai OAuth tokens; x-api-key for regular keys.
                let mut req = self
                    .http
                    .post(&url)
                    .header("anthropic-version", &self.config.api_version)
                    .header("anthropic-beta", &self.config.beta_features)
                    .header("content-type", "application/json")
                    .header("accept", "text/event-stream");
                req = if self.config.use_bearer_auth {
                    req.header("Authorization", format!("Bearer {}", &self.config.api_key))
                } else {
                    req.header("x-api-key", &self.config.api_key)
                };
                let req = req.json(body);

                let resp = req.send().await.map_err(ClaudeError::Http)?;
                let status = resp.status().as_u16();

                // 200-299: success
                if resp.status().is_success() {
                    return Ok(resp);
                }

                // 429 (rate limit) or 529 (overloaded): retry
                if (status == 429 || status == 529) && attempts <= self.config.max_retries {
                    // Honour Retry-After header if present
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
                    delay = (delay * 2).min(self.config.max_retry_delay);
                    continue;
                }

                // Non-retryable error – return immediately
                let text = resp.text().await.unwrap_or_default();
                return Err(self.parse_api_error(status, &text));
            }
        }

        /// Parse an API error body into a typed `ClaudeError`.
        fn parse_api_error(&self, status: u16, body: &str) -> ClaudeError {
            if let Ok(err) = serde_json::from_str::<ApiErrorResponse>(body) {
                match status {
                    401 => ClaudeError::Auth(err.error.message),
                    429 => ClaudeError::RateLimit,
                    529 => ClaudeError::ApiStatus {
                        status,
                        message: format!("Overloaded: {}", err.error.message),
                    },
                    _ => ClaudeError::ApiStatus {
                        status,
                        message: err.error.message,
                    },
                }
            } else {
                ClaudeError::ApiStatus {
                    status,
                    message: body.to_string(),
                }
            }
        }

        /// Read an SSE byte stream, parse frames, and emit `StreamEvent`s.
        async fn process_sse_stream(
            resp: reqwest::Response,
            handler: Arc<dyn StreamHandler>,
            tx: mpsc::Sender<StreamEvent>,
        ) -> Result<(), ClaudeError> {
            use sse_parser::SseLineParser;

            let mut parser = SseLineParser::new();
            let mut byte_stream = resp.bytes_stream();
            let mut leftover = String::new();

            while let Some(chunk_result) = byte_stream.next().await {
                let chunk = chunk_result.map_err(ClaudeError::Http)?;
                let text = String::from_utf8_lossy(&chunk);

                // Prepend any leftover from the previous chunk
                let combined = if leftover.is_empty() {
                    text.to_string()
                } else {
                    let mut s = std::mem::take(&mut leftover);
                    s.push_str(&text);
                    s
                };

                // Split into lines.  If the chunk doesn't end with a newline
                // the last piece is an incomplete line – stash it.
                let mut lines: Vec<&str> = combined.split('\n').collect();
                if !combined.ends_with('\n') {
                    leftover = lines.pop().unwrap_or("").to_string();
                }

                for line in lines {
                    let line = line.trim_end_matches('\r');
                    if let Some(frame) = parser.feed_line(line) {
                        if let Some(event) = Self::frame_to_event(&frame.event, &frame.data) {
                            handler.on_event(&event);
                            if tx.send(event).await.is_err() {
                                // Receiver dropped – stop reading.
                                return Ok(());
                            }
                        }
                    }
                }
            }

            Ok(())
        }

        /// Convert a parsed SSE frame into a typed `StreamEvent`.
        fn frame_to_event(event_type: &Option<String>, data: &str) -> Option<StreamEvent> {
            let event_name = event_type.as_deref().unwrap_or("");

            match event_name {
                "ping" => Some(StreamEvent::Ping),

                "message_start" => {
                    let v: Value = serde_json::from_str(data).ok()?;
                    let msg = v.get("message")?;
                    let id = msg.get("id")?.as_str()?.to_string();
                    let model = msg.get("model")?.as_str()?.to_string();
                    let usage = msg
                        .get("usage")
                        .and_then(|u| serde_json::from_value::<UsageInfo>(u.clone()).ok())
                        .unwrap_or_default();

                    Some(StreamEvent::MessageStart { id, model, usage })
                }

                "content_block_start" => {
                    let v: Value = serde_json::from_str(data).ok()?;
                    let index = v.get("index")?.as_u64()? as usize;
                    let block_value = v.get("content_block")?;
                    let content_block: ContentBlock =
                        serde_json::from_value(block_value.clone()).ok()?;
                    Some(StreamEvent::ContentBlockStart {
                        index,
                        content_block,
                    })
                }

                "content_block_delta" => {
                    let v: Value = serde_json::from_str(data).ok()?;
                    let index = v.get("index")?.as_u64()? as usize;
                    let delta_value = v.get("delta")?;
                    let delta: streaming::ContentDelta =
                        serde_json::from_value(delta_value.clone()).ok()?;
                    Some(StreamEvent::ContentBlockDelta { index, delta })
                }

                "content_block_stop" => {
                    let v: Value = serde_json::from_str(data).ok()?;
                    let index = v.get("index")?.as_u64()? as usize;
                    Some(StreamEvent::ContentBlockStop { index })
                }

                "message_delta" => {
                    let v: Value = serde_json::from_str(data).ok()?;
                    let delta = v.get("delta")?;
                    let stop_reason = delta
                        .get("stop_reason")
                        .and_then(|s| s.as_str())
                        .map(|s| s.to_string());
                    let usage = v
                        .get("usage")
                        .and_then(|u| serde_json::from_value::<UsageInfo>(u.clone()).ok());
                    Some(StreamEvent::MessageDelta { stop_reason, usage })
                }

                "message_stop" => Some(StreamEvent::MessageStop),

                "error" => {
                    let v: Value = serde_json::from_str(data).ok()?;
                    let error = v.get("error")?;
                    let error_type = error
                        .get("type")
                        .and_then(|s| s.as_str())
                        .unwrap_or("unknown")
                        .to_string();
                    let message = error
                        .get("message")
                        .and_then(|s| s.as_str())
                        .unwrap_or("Unknown error")
                        .to_string();
                    Some(StreamEvent::Error {
                        error_type,
                        message,
                    })
                }

                _ => {
                    debug!(event = event_name, "Unhandled SSE event type");
                    None
                }
            }
        }
    }

    // ────────────────────────────────────────────────────────────────────
    // Block rejection for non-vision-capable models (iso Claude Code UX)
    // ────────────────────────────────────────────────────────────────────
    //
    // The CLI is provider-agnostic — it always emits the richest blocks
    // (Image, Document). The provider's job is then trivial:
    //   - Vision-capable model → blocks forwarded verbatim on the wire.
    //   - Non-vision model → reject loudly. The block is replaced with
    //     an EXPLICIT error in the surrounding tool_result so the LLM
    //     sees `is_error: true` and a clear message telling the user
    //     to switch provider. No silent caption that would let the
    //     model pretend it had read the file.
    //
    // Walks ApiMessage.content in place:
    //   - tool_result with Image/Document inside → tool_result becomes
    //     a single error text block, parent `is_error` flipped true.
    //   - Top-level Image/Document inside a user message → replaced
    //     with an error text block. There is no tool_result wrapper at
    //     this layer, so the model just sees a clear refusal message.
    //   - Strings, plain text, tool_use, thinking, etc. → untouched.
    pub(super) fn degrade_value(content: &mut Value) {
        if let Value::Array(blocks) = content {
            for block in blocks.iter_mut() {
                reject_block(block);
            }
        }
    }

    fn reject_block(block: &mut Value) {
        let block_type = block
            .get("type")
            .and_then(|t| t.as_str())
            .map(|s| s.to_string());
        match block_type.as_deref() {
            Some("image") => {
                let msg = unsupported_block_message("image", block);
                *block = serde_json::json!({ "type": "text", "text": msg });
            }
            Some("document") => {
                let msg = unsupported_block_message("document", block);
                *block = serde_json::json!({ "type": "text", "text": msg });
            }
            Some("tool_result") => reject_tool_result(block),
            _ => {}
        }
    }

    /// Rewrite a tool_result that carries an unsupported block. Walk its
    /// inner blocks; if ANY visual block is found, replace the whole
    /// inner content with a single error text and flip is_error=true.
    fn reject_tool_result(block: &mut Value) {
        let Some(inner) = block.get_mut("content") else {
            return;
        };
        // Only Array content can carry blocks worth inspecting.
        let Value::Array(inner_blocks) = inner else {
            return;
        };
        let mut error_messages: Vec<String> = Vec::new();
        let mut surviving_text: Vec<String> = Vec::new();
        for b in inner_blocks.iter() {
            let t = b.get("type").and_then(|v| v.as_str()).unwrap_or("");
            match t {
                "image" => error_messages.push(unsupported_block_message("image", b)),
                "document" => error_messages.push(unsupported_block_message("document", b)),
                "text" => {
                    if let Some(s) = b.get("text").and_then(|v| v.as_str()) {
                        surviving_text.push(s.to_string());
                    }
                }
                _ => {}
            }
        }
        if error_messages.is_empty() {
            return; // nothing to reject in this tool_result
        }
        // Compose final text: captions first (so the model has context),
        // then the error message naming the failure mode and the fix.
        let mut composed = String::new();
        for s in surviving_text {
            composed.push_str(&s);
            if !composed.ends_with('\n') {
                composed.push('\n');
            }
        }
        for e in error_messages {
            composed.push_str(&e);
            composed.push('\n');
        }
        // Replace inner content with a single text block, flip is_error.
        if let Some(obj) = block.as_object_mut() {
            obj.insert(
                "content".to_string(),
                Value::Array(vec![serde_json::json!({
                    "type": "text",
                    "text": composed.trim_end().to_string(),
                })]),
            );
            obj.insert("is_error".to_string(), Value::Bool(true));
        }
    }

    /// Build the error message a non-vision provider returns for an
    /// Image / Document block. Preserves the original title (filename
    /// for documents) and media_type / url hints so the model can
    /// reason about what was refused.
    fn unsupported_block_message(kind: &str, block: &Value) -> String {
        let title_hint = block
            .get("title")
            .and_then(|t| t.as_str())
            .map(|t| format!(" \"{}\"", t))
            .unwrap_or_default();
        let media = block
            .get("source")
            .and_then(|s| s.get("media_type"))
            .and_then(|m| m.as_str());
        let url = block
            .get("source")
            .and_then(|s| s.get("url"))
            .and_then(|u| u.as_str());
        let detail = match (media, url) {
            (Some(m), Some(u)) => format!(" ({}, url={})", m, u),
            (Some(m), None) => format!(" ({})", m),
            (None, Some(u)) => format!(" (url={})", u),
            (None, None) => String::new(),
        };
        format!(
            "[ERROR: {}{}{} cannot be read — the current provider's model does not \
             support vision. Restart uppli-code with a vision-capable provider \
             (e.g. `uppli-code --provider glm`) to process this file. The current \
             session cannot be salvaged; ask the user to relaunch.]",
            kind, title_hint, detail
        )
    }
}

// ---------------------------------------------------------------------------
// LlmProvider implementation for AnthropicClient
// ---------------------------------------------------------------------------

#[async_trait::async_trait]
impl provider::LlmProvider for client::AnthropicClient {
    async fn create_message_stream(
        &self,
        request: CreateMessageRequest,
        handler: Arc<dyn StreamHandler>,
    ) -> Result<mpsc::Receiver<StreamEvent>, ClaudeError> {
        self.create_message_stream(request, handler).await
    }

    async fn create_message(
        &self,
        request: CreateMessageRequest,
    ) -> Result<CreateMessageResponse, ClaudeError> {
        self.create_message(request).await
    }

    async fn list_models(&self) -> Vec<AvailableModel> {
        self.fetch_available_models().await.unwrap_or_default()
    }

    fn capabilities(&self) -> &provider::ProviderCapabilities {
        // Caps are loaded from `crates/api/presets/deepseek.toml` at
        // construction time (see `AnthropicClient::new` /
        // `from_config`) and stored on the client. Single source of
        // truth: edit the TOML, the runtime behaviour follows.
        &self.capabilities
    }

    // model_supports_thinking, fast_model_for, context_window, max_output_tokens
    // all use the default implementations that query known_models — no overrides needed.
}

// ---------------------------------------------------------------------------
// Convenience builder for CreateMessageRequest
// ---------------------------------------------------------------------------

impl CreateMessageRequest {
    /// Create a minimal request builder.
    pub fn builder(model: impl Into<String>, max_tokens: u32) -> CreateMessageRequestBuilder {
        CreateMessageRequestBuilder {
            model: model.into(),
            max_tokens,
            messages: vec![],
            system: None,
            tools: None,
            temperature: None,
            top_p: None,
            top_k: None,
            stop_sequences: None,
            thinking: None,
            output_config: None,
        }
    }
}

pub struct CreateMessageRequestBuilder {
    model: String,
    max_tokens: u32,
    messages: Vec<ApiMessage>,
    system: Option<SystemPrompt>,
    tools: Option<Vec<ApiToolDefinition>>,
    temperature: Option<f32>,
    top_p: Option<f32>,
    top_k: Option<u32>,
    stop_sequences: Option<Vec<String>>,
    thinking: Option<ThinkingConfig>,
    output_config: Option<OutputConfig>,
}

impl CreateMessageRequestBuilder {
    pub fn messages(mut self, msgs: Vec<ApiMessage>) -> Self {
        self.messages = msgs;
        self
    }

    pub fn add_message(mut self, msg: ApiMessage) -> Self {
        self.messages.push(msg);
        self
    }

    pub fn system(mut self, s: SystemPrompt) -> Self {
        self.system = Some(s);
        self
    }

    pub fn system_text(mut self, text: impl Into<String>) -> Self {
        self.system = Some(SystemPrompt::Text(text.into()));
        self
    }

    pub fn tools(mut self, tools: Vec<ApiToolDefinition>) -> Self {
        self.tools = Some(tools);
        self
    }

    pub fn temperature(mut self, t: f32) -> Self {
        self.temperature = Some(t);
        self
    }

    pub fn top_p(mut self, p: f32) -> Self {
        self.top_p = Some(p);
        self
    }

    pub fn top_k(mut self, k: u32) -> Self {
        self.top_k = Some(k);
        self
    }

    pub fn stop_sequences(mut self, seqs: Vec<String>) -> Self {
        self.stop_sequences = Some(seqs);
        self
    }

    pub fn thinking(mut self, config: ThinkingConfig) -> Self {
        self.thinking = Some(config);
        self
    }

    /// Set the `output_config` field (DeepSeek-specific effort control).
    /// See `OutputConfig` for details.
    pub fn output_config(mut self, config: OutputConfig) -> Self {
        self.output_config = Some(config);
        self
    }

    pub fn build(self) -> CreateMessageRequest {
        CreateMessageRequest {
            model: self.model,
            max_tokens: self.max_tokens,
            messages: self.messages,
            system: self.system,
            tools: self.tools,
            temperature: self.temperature,
            top_p: self.top_p,
            top_k: self.top_k,
            stop_sequences: self.stop_sequences,
            stream: true,
            thinking: self.thinking,
            output_config: self.output_config,
        }
    }
}

// ---------------------------------------------------------------------------
// Accumulated message builder – reconstructs a full Message from stream events
// ---------------------------------------------------------------------------

/// Collects streaming events and produces a finished `Message` plus usage info.
pub struct StreamAccumulator {
    id: Option<String>,
    model: Option<String>,
    content_blocks: Vec<ContentBlock>,
    /// Partial accumulators keyed by block index.
    partials: std::collections::HashMap<usize, PartialBlock>,
    stop_reason: Option<String>,
    usage: UsageInfo,
}

#[derive(Debug)]
enum PartialBlock {
    Text(String),
    ToolUse {
        id: String,
        name: String,
        json_buf: String,
    },
    Thinking {
        thinking_buf: String,
        signature_buf: String,
    },
}

impl Default for StreamAccumulator {
    fn default() -> Self {
        Self::new()
    }
}

impl StreamAccumulator {
    pub fn new() -> Self {
        Self {
            id: None,
            model: None,
            content_blocks: vec![],
            partials: Default::default(),
            stop_reason: None,
            usage: UsageInfo::default(),
        }
    }

    /// Feed a stream event. Call this for every event received from the stream.
    pub fn on_event(&mut self, event: &StreamEvent) {
        match event {
            StreamEvent::MessageStart { id, model, usage } => {
                self.id = Some(id.clone());
                self.model = Some(model.clone());
                self.usage = usage.clone();
            }

            StreamEvent::ContentBlockStart {
                index,
                content_block,
            } => {
                let partial = match content_block {
                    ContentBlock::Text { text } => PartialBlock::Text(text.clone()),
                    ContentBlock::ToolUse { id, name, .. } => PartialBlock::ToolUse {
                        id: id.clone(),
                        name: name.clone(),
                        json_buf: String::new(),
                    },
                    ContentBlock::Thinking {
                        thinking,
                        signature,
                    } => PartialBlock::Thinking {
                        thinking_buf: thinking.clone(),
                        signature_buf: signature.clone(),
                    },
                    _ => return,
                };
                self.partials.insert(*index, partial);
            }

            StreamEvent::ContentBlockDelta { index, delta } => {
                if let Some(partial) = self.partials.get_mut(index) {
                    match (partial, delta) {
                        (PartialBlock::Text(buf), streaming::ContentDelta::TextDelta { text }) => {
                            buf.push_str(text);
                        }
                        (
                            PartialBlock::ToolUse { json_buf, .. },
                            streaming::ContentDelta::InputJsonDelta { partial_json },
                        ) => {
                            json_buf.push_str(partial_json);
                        }
                        (
                            PartialBlock::Thinking { thinking_buf, .. },
                            streaming::ContentDelta::ThinkingDelta { thinking },
                        ) => {
                            thinking_buf.push_str(thinking);
                        }
                        (
                            PartialBlock::Thinking { signature_buf, .. },
                            streaming::ContentDelta::SignatureDelta { signature },
                        ) => {
                            signature_buf.push_str(signature);
                        }
                        _ => {}
                    }
                }
            }

            StreamEvent::ContentBlockStop { index } => {
                if let Some(partial) = self.partials.remove(index) {
                    let block = match partial {
                        PartialBlock::Text(text) => ContentBlock::Text { text },
                        PartialBlock::ToolUse { id, name, json_buf } => {
                            let input = serde_json::from_str(&json_buf)
                                .unwrap_or(Value::Object(Default::default()));
                            ContentBlock::ToolUse { id, name, input }
                        }
                        PartialBlock::Thinking {
                            thinking_buf,
                            signature_buf,
                        } => ContentBlock::Thinking {
                            thinking: thinking_buf,
                            signature: signature_buf,
                        },
                    };
                    self.content_blocks.push(block);
                }
            }

            StreamEvent::MessageDelta { stop_reason, usage } => {
                if let Some(sr) = stop_reason {
                    self.stop_reason = Some(sr.clone());
                }
                if let Some(u) = usage {
                    // The delta usage usually only has output_tokens;
                    // add them to the running total.
                    self.usage.output_tokens += u.output_tokens;
                }
            }

            StreamEvent::MessageStop => {}
            StreamEvent::Ping => {}
            StreamEvent::Error { .. } => {}
        }
    }

    /// Finalize and produce the accumulated `Message`.
    pub fn finish(self) -> (Message, UsageInfo, Option<String>) {
        let msg = Message::assistant_blocks(self.content_blocks);
        (msg, self.usage, self.stop_reason)
    }

    pub fn stop_reason(&self) -> Option<&str> {
        self.stop_reason.as_deref()
    }

    pub fn usage(&self) -> &UsageInfo {
        &self.usage
    }

    pub fn model(&self) -> Option<&str> {
        self.model.as_deref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ─── degrade_value: provider-side block rejection (iso Claude Code) ───
    //
    // The CLI is provider-agnostic — it emits Image/Document blocks
    // unconditionally. When the backing model doesn't support vision
    // (DeepSeek today), AnthropicClient rewrites those blocks as
    // EXPLICIT error text. Tool_result wrappers also get is_error=true
    // so the LLM sees the failure clearly, instead of silently
    // accepting a fallback caption it might mistake for the real
    // content. Iso Claude Code: "il marche ou il erreur, pas de
    // dégradation cachée".

    #[test]
    fn degrade_value_top_level_image_becomes_explicit_error() {
        let mut content = serde_json::json!([
            {
                "type": "image",
                "source": {
                    "type": "base64",
                    "media_type": "image/png",
                    "data": "iVBORw0KGgo="
                }
            }
        ]);
        client::degrade_value(&mut content);
        let arr = content.as_array().expect("still an array");
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["type"], "text");
        let text = arr[0]["text"].as_str().unwrap();
        assert!(
            text.contains("ERROR"),
            "must be an explicit error, got: {}",
            text
        );
        assert!(text.contains("image"));
        assert!(text.contains("image/png"));
        assert!(
            text.contains("Restart") || text.contains("relaunch") || text.contains("--provider"),
            "must instruct user to relaunch with a different provider, got: {}",
            text
        );
    }

    #[test]
    fn degrade_value_top_level_document_becomes_explicit_error() {
        let mut content = serde_json::json!([
            {
                "type": "document",
                "source": {
                    "type": "base64",
                    "media_type": "application/pdf",
                    "data": "JVBERi0="
                }
            }
        ]);
        client::degrade_value(&mut content);
        let arr = content.as_array().expect("still an array");
        assert_eq!(arr[0]["type"], "text");
        let text = arr[0]["text"].as_str().unwrap();
        assert!(text.contains("ERROR"));
        assert!(text.contains("document"));
        assert!(text.contains("application/pdf"));
    }

    #[test]
    fn degrade_value_preserves_text_blocks_alongside_rejected_images() {
        // Top-level user message with mixed text and image: the text
        // survives, the image becomes an error block in place.
        let mut content = serde_json::json!([
            { "type": "text", "text": "hello" },
            {
                "type": "image",
                "source": {"type": "base64", "media_type": "image/jpeg", "data": "x"}
            },
            { "type": "text", "text": "world" }
        ]);
        client::degrade_value(&mut content);
        let arr = content.as_array().unwrap();
        assert_eq!(arr.len(), 3);
        assert_eq!(arr[0]["text"], "hello");
        assert_eq!(arr[1]["type"], "text");
        assert!(arr[1]["text"].as_str().unwrap().contains("ERROR"));
        assert_eq!(arr[2]["text"], "world");
    }

    #[test]
    fn degrade_value_tool_result_with_image_flips_is_error_true() {
        // Critical invariant: when a tool_result contains an Image the
        // provider can't read, the WHOLE tool_result is marked
        // is_error=true so the LLM treats it as a failed tool call.
        // The textual caption inside the tool_result survives so the
        // model still has context about what was attempted.
        let mut content = serde_json::json!([
            {
                "type": "tool_result",
                "tool_use_id": "tu_1",
                "content": [
                    { "type": "text", "text": "[Image: foo.png, 1x1]" },
                    {
                        "type": "image",
                        "source": {"type": "base64", "media_type": "image/png", "data": "x"}
                    }
                ]
            }
        ]);
        client::degrade_value(&mut content);
        let tr = &content.as_array().unwrap()[0];
        assert_eq!(tr["type"], "tool_result");
        assert_eq!(
            tr["is_error"], true,
            "tool_result carrying an unsupported block must flip is_error=true"
        );
        let inner = tr["content"].as_array().unwrap();
        // Composed text: surviving caption + error message.
        let composed = inner[0]["text"].as_str().unwrap();
        assert!(
            composed.contains("foo.png"),
            "caption must survive: {}",
            composed
        );
        assert!(
            composed.contains("ERROR"),
            "error must be present: {}",
            composed
        );
    }

    #[test]
    fn degrade_value_tool_result_text_only_stays_intact() {
        // A tool_result with only text content should NOT be flipped to
        // is_error — only visual rejections trigger that.
        let mut content = serde_json::json!([
            {
                "type": "tool_result",
                "tool_use_id": "tu_2",
                "content": [
                    { "type": "text", "text": "row 1\nrow 2" }
                ]
            }
        ]);
        client::degrade_value(&mut content);
        let tr = &content.as_array().unwrap()[0];
        assert_eq!(tr["type"], "tool_result");
        assert!(
            tr.get("is_error").is_none() || tr["is_error"] == false,
            "text-only tool_result must NOT be flagged is_error"
        );
    }

    #[test]
    fn degrade_value_ignores_plain_string_content() {
        let mut content = serde_json::json!("plain user message");
        let before = content.clone();
        client::degrade_value(&mut content);
        assert_eq!(content, before);
    }

    #[test]
    fn deepseek_client_rejects_image_block_end_to_end() {
        // End-to-end: build a CreateMessageRequest with an Image block,
        // run it through `degrade_blocks_if_needed`, assert the wire
        // body carries an EXPLICIT error text (no silent caption).
        let client = deepseek_test_client();
        let mut req = CreateMessageRequest::builder("deepseek-v4-pro", 4096).build();
        req.messages.push(ApiMessage {
            role: "user".to_string(),
            content: serde_json::json!([
                { "type": "text", "text": "What's in this image?" },
                {
                    "type": "image",
                    "source": {"type": "base64", "media_type": "image/png", "data": "x"}
                }
            ]),
        });
        client.degrade_blocks_if_needed(&mut req);
        let body = serde_json::to_value(&req).unwrap();
        let blocks = body["messages"][0]["content"].as_array().unwrap();
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0]["type"], "text"); // original text intact
        assert_eq!(blocks[1]["type"], "text"); // ex-image, now text-ERROR
        let err_text = blocks[1]["text"].as_str().unwrap();
        assert!(err_text.contains("ERROR"));
        assert!(err_text.contains("image"));
    }

    #[test]
    fn degrade_value_preserves_document_title_in_caption() {
        // A DOCX or PDF block typically carries a `title` field with
        // the original filename. That's the single most useful hint
        // for a model that can't see the bytes — preserve it.
        let mut content = serde_json::json!([
            {
                "type": "document",
                "source": {"type": "base64", "media_type": "application/pdf", "data": "x"},
                "title": "Contract-v3-final.pdf"
            }
        ]);
        client::degrade_value(&mut content);
        let caption = content[0]["text"].as_str().unwrap();
        assert!(
            caption.contains("Contract-v3-final.pdf"),
            "title must survive degradation, got: {}",
            caption
        );
        assert!(caption.contains("application/pdf"));
    }

    // (test deep-recursion removed: the Anthropic wire format does not
    // nest tool_result inside tool_result; that case can't appear in
    // real traffic, so the synthetic test became noise after the
    // degrade→reject behavior change.)

    #[test]
    fn degrade_value_url_source_includes_url_in_error() {
        let mut content = serde_json::json!([
            {
                "type": "image",
                "source": {"type": "url", "url": "https://example.com/x.png"}
            }
        ]);
        client::degrade_value(&mut content);
        let err_text = content[0]["text"].as_str().unwrap();
        assert!(err_text.contains("ERROR"));
        assert!(err_text.contains("https://example.com/x.png"));
    }

    #[test]
    fn test_sse_parser_basic() {
        let mut parser = sse_parser::SseLineParser::new();
        assert!(parser.feed_line("event: message_start").is_none());
        assert!(parser
            .feed_line(r#"data: {"message":{"id":"m1","model":"claude","usage":{"input_tokens":0,"output_tokens":0}}}"#)
            .is_none());
        let frame = parser.feed_line("").expect("should produce frame");
        assert_eq!(frame.event.as_deref(), Some("message_start"));
        assert!(frame.data.contains("m1"));
    }

    #[test]
    fn test_create_message_request_builder() {
        let req = CreateMessageRequest::builder("claude-opus-4-6", 4096)
            .system_text("You are helpful.")
            .temperature(0.7)
            .build();
        assert_eq!(req.model, "claude-opus-4-6");
        assert_eq!(req.max_tokens, 4096);
        assert!(req.stream);
    }

    #[test]
    fn test_request_without_output_config_omits_field() {
        let req = CreateMessageRequest::builder("deepseek-v4-pro", 4096).build();
        let json = serde_json::to_string(&req).unwrap();
        assert!(
            !json.contains("output_config"),
            "no output_config set → JSON must not contain the field, got: {json}"
        );
    }

    #[test]
    fn test_request_with_output_config_effort_max_serializes_correctly() {
        // DeepSeek Anthropic API expects: {"output_config": {"effort": "max"}}
        // See https://api-docs.deepseek.com/guides/thinking_mode
        let req = CreateMessageRequest::builder("deepseek-v4-pro", 4096)
            .output_config(OutputConfig::effort("max"))
            .build();
        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(
            json["output_config"]["effort"], "max",
            "expected output_config.effort = \"max\", got: {json}"
        );
    }

    #[test]
    fn test_output_config_effort_high_serializes_correctly() {
        let req = CreateMessageRequest::builder("deepseek-v4-pro", 4096)
            .output_config(OutputConfig::effort("high"))
            .build();
        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(json["output_config"]["effort"], "high");
    }

    // ── PR B regression: do NOT send output_config on the wire ─────────────
    //
    // The query loop previously called `req_builder.output_config(...)`
    // unconditionally when an effort_level was set. That sent
    // `output_config: { "effort": "max" }` to DeepSeek V4 Pro, which puts
    // the model into its deepest reasoning mode and silently breaks tool
    // calling (Mercer bench: Write=0).
    //
    // This test pins the regression: a request built like the post-PR-B
    // query loop (thinking enabled, no explicit output_config call) MUST
    // serialize without the `output_config` field. The OutputConfig type
    // and builder method remain available, but no production call site
    // populates them.

    #[test]
    fn test_request_with_thinking_and_no_explicit_output_config_omits_field() {
        let req = CreateMessageRequest::builder("deepseek-v4-pro", 4096)
            .thinking(ThinkingConfig::enabled(64_000))
            .build();
        let json = serde_json::to_string(&req).unwrap();
        assert!(
            json.contains("\"thinking\""),
            "thinking field must be present, got: {json}"
        );
        assert!(
            !json.contains("output_config"),
            "output_config must NOT be present (PR B revert — would re-trigger \
             DeepSeek V4 Pro tool-calling regression), got: {json}"
        );
    }

    // ── DeepSeek known_models metadata (PR A — TDD) ────────────────────────
    //
    // Pin the documented DeepSeek v4 model metadata so:
    //   - default_model is present in known_models (not unknown fallback)
    //   - fast_model is a different, cheaper model than default
    //   - deprecated models stay listed for back-compat but flagged

    /// Build a `ProviderCapabilities` for DeepSeek by reading the
    /// TOML preset. Single source of truth — the same caps the
    /// runtime client uses.
    fn deepseek_test_caps() -> provider::ProviderCapabilities {
        crate::providers::loader::registry()
            .find("deepseek")
            .expect("deepseek preset must exist")
            .capabilities
            .clone()
    }

    /// Build a fake AnthropicClient that won't be used over the wire.
    fn deepseek_test_client() -> client::AnthropicClient {
        let cfg = client::ClientConfig {
            api_key: "test-key-not-used".to_string(),
            ..Default::default()
        };
        client::AnthropicClient::new(cfg, deepseek_test_caps()).expect("test client builds")
    }

    fn deepseek_caps_for_test() -> &'static provider::ProviderCapabilities {
        use provider::LlmProvider;
        let client = Box::leak(Box::new(deepseek_test_client()));
        client.capabilities()
    }

    #[test]
    fn test_deepseek_known_models_contains_v4_pro_and_flash_with_thinking() {
        let caps = deepseek_caps_for_test();
        let v4_pro = caps
            .known_models
            .iter()
            .find(|m| m.id == "deepseek-v4-pro")
            .expect("deepseek-v4-pro must be in known_models (it is the default_model)");
        assert!(
            v4_pro.supports_thinking,
            "deepseek-v4-pro must support thinking"
        );
        assert!(
            v4_pro.context_window >= 128_000,
            "deepseek-v4-pro context_window should be >= 128k, got {}",
            v4_pro.context_window
        );

        let v4_flash = caps
            .known_models
            .iter()
            .find(|m| m.id == "deepseek-v4-flash")
            .expect("deepseek-v4-flash must be in known_models (it is the fast_model)");
        assert!(
            v4_flash.supports_thinking,
            "deepseek-v4-flash must support thinking (used as fast model in hybrid mode)"
        );
    }

    #[test]
    fn test_deepseek_default_model_supports_thinking() {
        use provider::LlmProvider;
        let client = deepseek_test_client();
        let default_model = client.capabilities().default_model.clone();
        assert!(
            client.model_supports_thinking(&default_model),
            "default_model '{}' must support thinking (otherwise --effort silently drops on the default)",
            default_model
        );
    }

    // Note: the previous `test_deepseek_toml_caps_match_static_anthropic_client_caps`
    // test has been REMOVED. After AnthropicClient was wired through the
    // TOML loader (commit "refactor(api): AnthropicClient loads caps from
    // deepseek.toml"), there is no longer a static OnceLock to compare
    // against — the runtime caps ARE the TOML caps. Asserting "TOML == TOML"
    // would be vacuous.

    #[test]
    fn test_deepseek_fast_model_differs_from_default() {
        let caps = deepseek_caps_for_test();
        assert_eq!(caps.default_model, "deepseek-v4-pro");
        assert_eq!(
            caps.fast_model.as_deref(),
            Some("deepseek-v4-flash"),
            "fast_model must be v4-flash (different from default), otherwise hybrid mode pays v4-pro prices on every tool-result turn"
        );
    }

    #[test]
    fn test_deepseek_deprecated_models_kept_for_back_compat_and_flagged() {
        let caps = deepseek_caps_for_test();
        let reasoner = caps
            .known_models
            .iter()
            .find(|m| m.id == "deepseek-reasoner")
            .expect("deepseek-reasoner kept in known_models for back-compat (old user configs)");
        let chat = caps
            .known_models
            .iter()
            .find(|m| m.id == "deepseek-chat")
            .expect("deepseek-chat kept in known_models for back-compat");
        assert!(
            reasoner.description.to_lowercase().contains("deprecat"),
            "deepseek-reasoner description must flag deprecation, got: '{}'",
            reasoner.description
        );
        assert!(
            chat.description.to_lowercase().contains("deprecat"),
            "deepseek-chat description must flag deprecation, got: '{}'",
            chat.description
        );
    }

    #[test]
    fn test_stream_accumulator_text() {
        let mut acc = StreamAccumulator::new();
        acc.on_event(&StreamEvent::MessageStart {
            id: "m1".into(),
            model: "claude".into(),
            usage: UsageInfo::default(),
        });
        acc.on_event(&StreamEvent::ContentBlockStart {
            index: 0,
            content_block: ContentBlock::Text {
                text: String::new(),
            },
        });
        acc.on_event(&StreamEvent::ContentBlockDelta {
            index: 0,
            delta: streaming::ContentDelta::TextDelta {
                text: "Hello ".into(),
            },
        });
        acc.on_event(&StreamEvent::ContentBlockDelta {
            index: 0,
            delta: streaming::ContentDelta::TextDelta {
                text: "world!".into(),
            },
        });
        acc.on_event(&StreamEvent::ContentBlockStop { index: 0 });
        acc.on_event(&StreamEvent::MessageDelta {
            stop_reason: Some("end_turn".into()),
            usage: None,
        });
        acc.on_event(&StreamEvent::MessageStop);

        let (msg, _usage, stop) = acc.finish();
        assert_eq!(msg.get_text(), Some("Hello world!"));
        assert_eq!(stop.as_deref(), Some("end_turn"));
    }
}
