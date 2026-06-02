//! Serde schema for provider TOML files.
//!
//! Every TOML file under `crates/api/presets/` deserialises into a
//! `ProviderConfigFile`. The shape is intentionally close to the runtime
//! `ProviderCapabilities` so the conversion in `loader::into_capabilities`
//! is mechanical.
//!
//! `#[serde(deny_unknown_fields)]` is enabled on every struct so typos in
//! user-supplied TOML files (override directory) fail loudly with the
//! offending key, instead of being silently ignored.

use serde::Deserialize;

/// Top-level shape of a provider TOML file.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderConfigFile {
    /// File format version. v1 is the current schema. Mismatches fail
    /// loudly so future schema bumps don't silently misread old files.
    pub schema_version: u32,
    pub provider: ProviderToml,
    pub auth: AuthToml,
    pub defaults: ProviderDefaultsToml,
    /// Models known to this provider. At least one must be marked
    /// `default = true`.
    pub models: Vec<ProviderModelToml>,
}

/// The `[provider]` table.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderToml {
    pub name: String,
    #[serde(default)]
    pub aliases: Vec<String>,
    pub display_name: String,
    pub description: String,
    pub attribution: String,
    /// Maps to `cc_core::config::ProviderType`. Accepted values:
    /// "deepseek" | "ollama" | "alibaba" | "glm" | "openai_compat".
    pub provider_type: String,
    pub api_format: ApiFormatToml,
    pub api_base: String,
    /// Whether the provider's default model accepts image / document
    /// content blocks. Drives the multi-modal tool_result dispatch in
    /// the query loop. Defaults to false so adding a new provider
    /// can't accidentally claim vision support.
    #[serde(default)]
    pub supports_vision: bool,
    /// Wire-level "thinking" / reasoning dialect the provider's upstream
    /// API expects when `--effort` is set. Each value mirrors the
    /// provider's official spec — uppli-code is a transparent adapter,
    /// it never invents its own thinking dialect.
    ///
    /// - `anthropic_nested` → `thinking: {type: "enabled", budget_tokens: N}`
    ///   (Anthropic Messages API spec, z.ai for GLM-5+, future Claude
    ///   providers)
    /// - `qwen3` → `enable_thinking: true, thinking_budget: N`
    ///   (Alibaba DashScope Qwen3 docs)
    /// - `ollama_think` → `think: true`
    ///   (Ollama API spec for Qwen3 local models)
    /// - absent (None) → no thinking field on the wire (provider's
    ///   model decides on its own per its default behaviour)
    ///
    /// Default inference (when field omitted):
    /// api_format=anthropic → "anthropic_nested";
    /// api_format=ollama → "ollama_think";
    /// api_format=openai → None (most OpenAI-compat endpoints
    /// don't accept thinking fields; Qwen3 and z.ai opt in explicitly).
    ///
    /// **If your OpenAI-compat provider DOES accept a thinking dialect
    /// (z.ai uses Anthropic-nested, Alibaba DashScope uses Qwen3,
    /// OpenRouter uses its own normalized `reasoning` block — not yet
    /// supported here), declare `thinking_format` explicitly. Failing
    /// to declare it on a provider whose models have
    /// `supports_thinking = true` makes the loader refuse the config
    /// at startup with `LoadError::ThinkingFormatMissing` — a transparent
    /// adapter cannot silently drop a user's `--effort`.**
    #[serde(default)]
    pub thinking_format: Option<ThinkingFormatToml>,
}

/// Wire protocol family. Mirrors `provider::ApiFormat` but is a separate
/// type so the TOML schema stays self-contained.
#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ApiFormatToml {
    Anthropic,
    Openai,
    Ollama,
}

/// Thinking dialect declared by the provider. Each variant mirrors an
/// official upstream spec — see field comment on `ProviderToml::thinking_format`.
#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ThinkingFormatToml {
    /// Anthropic Messages API: `thinking: {type: "enabled", budget_tokens: N}`.
    /// Also the format z.ai expects on its OpenAI-compat wire for GLM-5+.
    AnthropicNested,
    /// Qwen3 / Alibaba DashScope: `enable_thinking: true, thinking_budget: N`.
    Qwen3,
    /// Ollama: `think: true`.
    OllamaThink,
}

/// The `[auth]` table.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthToml {
    #[serde(default)]
    pub env_vars: Vec<String>,
    pub keychain_key: String,
    pub display_label: String,
    #[serde(default = "default_required")]
    pub required: bool,
}

fn default_required() -> bool {
    true
}

/// The `[defaults]` table.
///
/// Note: PR D dropped `thinking_budget` — the budget is now derived
/// exclusively from `EffortLevel::thinking_budget_tokens()` in cc-core,
/// driven by the user's `--effort` flag. One source of truth, no
/// per-provider override.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderDefaultsToml {
    pub max_tokens: u32,
    #[serde(default = "default_timeout_sec")]
    pub request_timeout_sec: u64,
    #[serde(default = "default_max_retries")]
    pub max_retries: u32,
}

fn default_timeout_sec() -> u64 {
    600
}

fn default_max_retries() -> u32 {
    5
}

/// A single `[[models]]` entry.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderModelToml {
    pub id: String,
    pub display_name: String,
    #[serde(default)]
    pub description: String,
    pub context_window: u64,
    pub max_output_tokens: u32,
    #[serde(default)]
    pub supports_thinking: bool,
    /// Exactly one model per file must set this. Validated by the loader.
    #[serde(default)]
    pub default: bool,
    /// Optional fast-mode model for hybrid mode (tool-result turns). At most
    /// one per file.
    #[serde(default)]
    pub fast: bool,
    /// Flag the model as deprecated — kept for back-compat but hidden from
    /// onboarding picker. No effect on functionality.
    #[serde(default)]
    pub deprecated: bool,
    /// Optional pricing. `None` for free/local/unknown models.
    #[serde(default)]
    pub pricing: Option<ProviderPricingToml>,
}

/// The `[models.pricing]` inline table.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderPricingToml {
    pub input_per_mtk: f64,
    pub output_per_mtk: f64,
    #[serde(default)]
    pub cache_creation_per_mtk: f64,
    #[serde(default)]
    pub cache_read_per_mtk: f64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_minimal_provider() {
        let toml_src = r#"
schema_version = 1

[provider]
name = "test"
display_name = "Test"
description = "Test provider"
attribution = "test"
provider_type = "openai_compat"
api_format = "openai"
api_base = "https://example.com"

[auth]
keychain_key = "test"
display_label = "Test"

[defaults]
max_tokens = 16384

[[models]]
id = "test-model"
display_name = "Test Model"
context_window = 128000
max_output_tokens = 8192
default = true
"#;
        let cfg: ProviderConfigFile = toml::from_str(toml_src).expect("must parse");
        assert_eq!(cfg.schema_version, 1);
        assert_eq!(cfg.provider.name, "test");
        assert_eq!(cfg.models.len(), 1);
        assert!(cfg.models[0].default);
        assert!(cfg.auth.required, "required defaults to true");
        assert!(cfg.models[0].pricing.is_none(), "pricing defaults to None");
    }

    #[test]
    fn unknown_field_fails_loudly() {
        let toml_src = r#"
schema_version = 1
unknown_top_level_field = "should fail"

[provider]
name = "test"
display_name = "Test"
description = "Test"
attribution = "test"
provider_type = "openai_compat"
api_format = "openai"
api_base = "https://example.com"

[auth]
keychain_key = "test"
display_label = "Test"

[defaults]
max_tokens = 16384

[[models]]
id = "m"
display_name = "M"
context_window = 1000
max_output_tokens = 100
default = true
"#;
        let res: Result<ProviderConfigFile, _> = toml::from_str(toml_src);
        assert!(res.is_err(), "unknown field must fail");
    }

    #[test]
    fn pricing_inline_table_parses() {
        let toml_src = r#"
schema_version = 1

[provider]
name = "test"
display_name = "Test"
description = "Test"
attribution = "test"
provider_type = "openai_compat"
api_format = "openai"
api_base = "https://example.com"

[auth]
keychain_key = "test"
display_label = "Test"

[defaults]
max_tokens = 16384

[[models]]
id = "m"
display_name = "M"
context_window = 1000
max_output_tokens = 100
default = true

[models.pricing]
input_per_mtk = 0.5
output_per_mtk = 1.5
"#;
        let cfg: ProviderConfigFile = toml::from_str(toml_src).expect("must parse");
        let p = cfg.models[0].pricing.expect("pricing present");
        assert!((p.input_per_mtk - 0.5).abs() < 1e-9);
        assert!((p.output_per_mtk - 1.5).abs() < 1e-9);
        assert_eq!(p.cache_creation_per_mtk, 0.0, "cache defaults to 0");
    }
}
