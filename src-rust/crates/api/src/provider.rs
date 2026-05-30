// provider.rs — Abstract LLM provider trait.
//
// A provider is a **self-contained, self-describing unit**.  The rest of the
// codebase reads everything it needs from the provider — model names, token
// limits, pricing, auth config, system prompt attribution.  Adding a new
// provider requires ONE preset + ONE factory entry, ZERO changes elsewhere.

use crate::streaming::{StreamEvent, StreamHandler};
use crate::{AvailableModel, CreateMessageRequest, CreateMessageResponse};
use async_trait::async_trait;
use cc_core::error::ClaudeError;
use std::sync::Arc;
use tokio::sync::mpsc;

// ---------------------------------------------------------------------------
// Per-model metadata
// ---------------------------------------------------------------------------

/// Re-export ModelPricing from cc_core so there's ONE canonical type.
pub use cc_core::cost::ModelPricing;

/// Complete metadata for a single model offered by a provider.
///
/// PR P v2 (2026-05-30): pricing restored (best-effort USD cost is a
/// product differentiator). Budget enforcement uses tokens via
/// `--max-tokens-total`; pricing display is decoupled and informational.
#[derive(Debug, Clone)]
pub struct ModelMetadata {
    /// Model identifier sent in API requests (e.g., "qwen3-235b-a22b").
    pub id: String,
    /// Human-readable name for the UI (e.g., "Qwen3 235B").
    pub display_name: String,
    /// Short description for the model picker.
    pub description: String,
    /// Context window size in tokens.
    pub context_window: u64,
    /// Maximum output tokens the model accepts.
    pub max_output_tokens: u32,
    /// Whether this model supports thinking/reasoning blocks.
    pub supports_thinking: bool,
    /// Token pricing (None = free / local / unknown — display shows "—").
    pub pricing: Option<ModelPricing>,
}

// ---------------------------------------------------------------------------
// Authentication config
// ---------------------------------------------------------------------------

/// How to resolve an API key for this provider.
///
/// All fields are `&'static` so presets can live in static memory.
#[derive(Debug, Clone, Copy)]
pub struct AuthConfig {
    /// Environment variable names to check, in priority order.
    pub env_vars: &'static [&'static str],
    /// Keychain entry name (e.g., "alibaba").
    pub keychain_key: &'static str,
    /// Human-readable label for interactive prompts (e.g., "DashScope").
    pub display_label: &'static str,
    /// Whether authentication is required (false for Ollama).
    pub required: bool,
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            env_vars: &["ANTHROPIC_API_KEY"],
            keychain_key: "default",
            display_label: "API",
            required: true,
        }
    }
}

// ---------------------------------------------------------------------------
// Provider preset — lightweight descriptor for onboarding / registry
// ---------------------------------------------------------------------------

/// A lightweight entry for the onboarding menu and provider registry.
/// Adding a new provider = adding a `ProviderPreset` + a factory arm.
/// Zero changes in the CLI.
#[derive(Debug, Clone)]
pub struct ProviderPreset {
    pub name: &'static str,
    pub aliases: &'static [&'static str],
    pub display_name: &'static str,
    pub description: &'static str,
    pub default_model: &'static str,
    pub fast_model: Option<&'static str>,
    pub supports_thinking: bool,
    pub auth: AuthConfig,
    pub provider_type: cc_core::config::ProviderType,
}

impl ProviderPreset {
    /// Whether this preset matches a given name (case-insensitive).
    pub fn matches(&self, query: &str) -> bool {
        let q = query.to_lowercase();
        self.name == q || self.aliases.iter().any(|a| *a == q)
    }
}

// ---------------------------------------------------------------------------
// Provider capabilities — the full self-description
// ---------------------------------------------------------------------------

/// Everything the rest of the codebase needs to know about a provider.
/// Consumers read this instead of hardcoded constants.
#[derive(Debug, Clone)]
pub struct ProviderCapabilities {
    // ── Identity ─────────────────────────────────────────────
    /// Machine-readable name (e.g., "deepseek").
    pub name: String,
    /// Human-readable display name (e.g., "DeepSeek").
    pub display_name: String,
    /// Attribution for the system prompt first line.
    /// e.g., "powered by DeepSeek" or "powered by Qwen3 (Alibaba Cloud)"
    pub attribution: String,

    // ── Models ───────────────────────────────────────────────
    /// Default (primary/reasoning) model identifier.
    pub default_model: String,
    /// Fast (non-reasoning) model for tool-result turns. None = no split.
    pub fast_model: Option<String>,
    /// All known models with full metadata.  Used by context_window(),
    /// max_output_tokens(), model_pricing(), model_supports_thinking().
    pub known_models: Vec<ModelMetadata>,

    // ── Token defaults ───────────────────────────────────────
    /// Default max_tokens for API requests.
    pub default_max_tokens: u32,
    /// Default thinking budget (None = thinking not supported by default).
    pub default_thinking_budget: Option<u32>,

    // ── API config ───────────────────────────────────────────
    /// Wire protocol family.
    pub api_format: ApiFormat,
    /// Default API base URL.
    pub default_api_base: String,

    // ── Auth ─────────────────────────────────────────────────
    pub auth: AuthConfig,
}

/// The wire protocol family a provider uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApiFormat {
    /// Anthropic `/v1/messages` with SSE (DeepSeek's /anthropic endpoint).
    Anthropic,
    /// OpenAI `/v1/chat/completions` with SSE (Alibaba, Mistral, many others).
    OpenAI,
    /// Ollama `/api/chat` with NDJSON streaming.
    Ollama,
}

impl std::fmt::Display for ApiFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ApiFormat::Anthropic => write!(f, "Anthropic"),
            ApiFormat::OpenAI => write!(f, "OpenAI"),
            ApiFormat::Ollama => write!(f, "Ollama"),
        }
    }
}

// ---------------------------------------------------------------------------
// The core trait
// ---------------------------------------------------------------------------

#[async_trait]
pub trait LlmProvider: Send + Sync {
    /// Send a streaming message request.
    async fn create_message_stream(
        &self,
        request: CreateMessageRequest,
        handler: Arc<dyn StreamHandler>,
    ) -> Result<mpsc::Receiver<StreamEvent>, ClaudeError>;

    /// Send a non-streaming message request.
    async fn create_message(
        &self,
        request: CreateMessageRequest,
    ) -> Result<CreateMessageResponse, ClaudeError>;

    /// Fetch available models from the provider's API.
    /// Returns empty vec on error — callers fall back to `known_models`.
    async fn list_models(&self) -> Vec<AvailableModel>;

    /// Full self-description of this provider.
    fn capabilities(&self) -> &ProviderCapabilities;

    // ── Derived helpers with sensible defaults ───────────────

    /// Whether the given model supports thinking/reasoning.
    /// Default: looks up `known_models`; for unknown models, falls back to
    /// whether the provider has a default thinking budget (i.e., the provider
    /// itself supports thinking, so an unknown model likely does too).
    fn model_supports_thinking(&self, model: &str) -> bool {
        let caps = self.capabilities();
        caps.known_models
            .iter()
            .find(|m| m.id == model)
            .map(|m| m.supports_thinking)
            .unwrap_or_else(|| caps.default_thinking_budget.is_some())
    }

    /// For hybrid mode: given a "slow" model, return the "fast" model.
    fn fast_model_for(&self, _model: &str) -> Option<&str> {
        self.capabilities().fast_model.as_deref()
    }

    /// Context window size (tokens) for a specific model.
    ///
    /// Falls back to `default_max_tokens * 4` (heuristic) if the model
    /// is not in `known_models`, rather than a hardcoded 128K which would
    /// be wrong for small local models (e.g., Ollama tinylama = 2K).
    fn context_window(&self, model: &str) -> u64 {
        self.capabilities()
            .known_models
            .iter()
            .find(|m| m.id == model)
            .map(|m| m.context_window)
            .unwrap_or_else(|| {
                // Heuristic: context ≈ 4× max output, floor at 8K.
                let fallback = (self.capabilities().default_max_tokens as u64) * 4;
                fallback.max(8_192)
            })
    }

    /// Max output tokens for a specific model.
    fn max_output_tokens(&self, model: &str) -> u32 {
        self.capabilities()
            .known_models
            .iter()
            .find(|m| m.id == model)
            .map(|m| m.max_output_tokens)
            .unwrap_or(self.capabilities().default_max_tokens)
    }

    /// Pricing for a specific model (None = free/unknown). Used to seed the
    /// CostTracker so USD cost can be displayed and persisted (cap enforcement
    /// uses tokens — see QueryConfig.max_total_tokens).
    fn model_pricing(&self, model: &str) -> Option<ModelPricing> {
        self.capabilities()
            .known_models
            .iter()
            .find(|m| m.id == model)
            .and_then(|m| m.pricing)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal provider implementation for testing default trait methods.
    struct TestProvider {
        caps: ProviderCapabilities,
    }

    #[async_trait]
    impl LlmProvider for TestProvider {
        async fn create_message_stream(
            &self,
            _req: CreateMessageRequest,
            _handler: Arc<dyn StreamHandler>,
        ) -> Result<mpsc::Receiver<StreamEvent>, ClaudeError> {
            unimplemented!()
        }

        async fn create_message(
            &self,
            _req: CreateMessageRequest,
        ) -> Result<CreateMessageResponse, ClaudeError> {
            unimplemented!()
        }

        async fn list_models(&self) -> Vec<AvailableModel> {
            vec![]
        }

        fn capabilities(&self) -> &ProviderCapabilities {
            &self.caps
        }
    }

    fn make_provider(known_models: Vec<ModelMetadata>, default_max_tokens: u32) -> TestProvider {
        TestProvider {
            caps: ProviderCapabilities {
                name: "test".to_string(),
                display_name: "Test".to_string(),
                attribution: "test".to_string(),
                default_model: "test-model".to_string(),
                fast_model: None,
                known_models,
                default_max_tokens,
                default_thinking_budget: None,
                api_format: ApiFormat::OpenAI,
                default_api_base: String::new(),
                auth: AuthConfig {
                    env_vars: &[],
                    keychain_key: "test",
                    display_label: "Test",
                    required: false,
                },
            },
        }
    }

    #[test]
    fn context_window_returns_known_model_value() {
        let p = make_provider(
            vec![ModelMetadata {
                id: "test-model".to_string(),
                display_name: "Test".to_string(),
                description: String::new(),
                context_window: 32_000,
                max_output_tokens: 4096,
                supports_thinking: false,
                pricing: None,
            }],
            4096,
        );
        assert_eq!(p.context_window("test-model"), 32_000);
    }

    #[test]
    fn context_window_unknown_model_uses_heuristic() {
        let p = make_provider(vec![], 4096);
        // Heuristic: 4096 * 4 = 16384, above 8K floor
        assert_eq!(p.context_window("unknown"), 16_384);
    }

    #[test]
    fn context_window_heuristic_respects_floor() {
        let p = make_provider(vec![], 1024);
        // Heuristic: 1024 * 4 = 4096, below 8K floor → returns 8192
        assert_eq!(p.context_window("unknown"), 8_192);
    }

    #[test]
    fn max_output_tokens_known_model() {
        let p = make_provider(
            vec![ModelMetadata {
                id: "m".to_string(),
                display_name: "M".to_string(),
                description: String::new(),
                context_window: 128_000,
                max_output_tokens: 16_384,
                supports_thinking: true,
                pricing: None,
            }],
            4096,
        );
        assert_eq!(p.max_output_tokens("m"), 16_384);
    }

    #[test]
    fn max_output_tokens_unknown_model_falls_back_to_default() {
        let p = make_provider(vec![], 8192);
        assert_eq!(p.max_output_tokens("unknown"), 8192);
    }

    #[test]
    fn model_supports_thinking_true() {
        let p = make_provider(
            vec![ModelMetadata {
                id: "thinker".to_string(),
                display_name: "T".to_string(),
                description: String::new(),
                context_window: 128_000,
                max_output_tokens: 8192,
                supports_thinking: true,
                pricing: None,
            }],
            4096,
        );
        assert!(p.model_supports_thinking("thinker"));
    }

    #[test]
    fn model_supports_thinking_false_for_unknown_no_budget() {
        let p = make_provider(vec![], 4096);
        assert!(!p.model_supports_thinking("unknown"));
    }

    #[test]
    fn model_supports_thinking_true_for_unknown_with_budget() {
        let mut p = make_provider(vec![], 4096);
        p.caps.default_thinking_budget = Some(16_000);
        assert!(p.model_supports_thinking("unknown"));
    }
}
