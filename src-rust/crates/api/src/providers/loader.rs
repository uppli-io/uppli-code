//! Compile-time bundled provider TOMLs + runtime conversion to
//! `ProviderCapabilities` and `ProviderPreset`.
//!
//! The bundled TOMLs are embedded in the binary via `include_str!` so there
//! is no runtime filesystem access. A future "user override directory"
//! (e.g. `~/.config/uppli/providers/*.toml`) can be added on top without
//! changing this module — read user files first, fall back to bundled.

use std::sync::OnceLock;

use crate::provider::{ApiFormat, AuthConfig, ModelMetadata, ProviderCapabilities};
use cc_core::config::ProviderType;
use cc_core::cost::ModelPricing;

use super::schema::{ApiFormatToml, ProviderConfigFile};

// ── Bundled provider files ────────────────────────────────────────────────
//
// Adding a new provider:
//   1. Create crates/api/presets/<name>.toml
//   2. Add an include_str! line below
//   3. Done — no Rust code to touch.

const BUNDLED_TOMLS: &[(&str, &str)] = &[
    ("deepseek", include_str!("../../presets/deepseek.toml")),
    ("alibaba", include_str!("../../presets/alibaba.toml")),
    ("openrouter", include_str!("../../presets/openrouter.toml")),
    ("mistral", include_str!("../../presets/mistral.toml")),
    ("ollama", include_str!("../../presets/ollama.toml")),
    ("openai", include_str!("../../presets/openai.toml")),
    ("glm", include_str!("../../presets/glm.toml")),
];

// ── Errors ────────────────────────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
pub enum LoadError {
    #[error("TOML parse error in provider '{provider}': {source}")]
    Parse {
        provider: String,
        #[source]
        source: toml::de::Error,
    },
    #[error("schema_version mismatch in provider '{provider}': expected 1, got {actual}")]
    SchemaVersion { provider: String, actual: u32 },
    #[error("provider '{provider}' has no models marked default = true")]
    NoDefaultModel { provider: String },
    #[error("provider '{provider}' has {count} models marked default = true (must be exactly 1)")]
    MultipleDefaults { provider: String, count: usize },
    #[error("provider '{provider}' has {count} models marked fast = true (at most 1 allowed)")]
    MultipleFastModels { provider: String, count: usize },
    #[error("provider '{provider}' uses unknown provider_type '{got}' (expected deepseek|ollama|alibaba|glm|openai_compat)")]
    UnknownProviderType { provider: String, got: String },
    #[error("provider '{provider}' has duplicate model id '{id}'")]
    DuplicateModelId { provider: String, id: String },
    #[error("filename '{filename}' does not match provider.name '{declared}' in TOML")]
    NameMismatch { filename: String, declared: String },
}

// ── Loaded registry ───────────────────────────────────────────────────────

/// A single provider loaded from TOML, ready for runtime use.
#[derive(Debug, Clone)]
pub struct LoadedProvider {
    pub capabilities: ProviderCapabilities,
    pub aliases: Vec<String>,
    pub provider_type: ProviderType,
    pub description: String,
    /// Wire-time settings from [defaults]: surface them so the OpenAI/Anthropic
    /// clients can read them without re-parsing the TOML.
    pub max_retries: u32,
    pub request_timeout_sec: u64,
}

impl LoadedProvider {
    /// True if `query` matches this provider's name or any alias (case-insensitive).
    pub fn matches(&self, query: &str) -> bool {
        let q = query.to_lowercase();
        self.capabilities.name == q || self.aliases.iter().any(|a| a.to_lowercase() == q)
    }
}

/// In-memory registry of all bundled providers.
#[derive(Debug, Clone)]
pub struct ProviderRegistry {
    pub providers: Vec<LoadedProvider>,
}

impl ProviderRegistry {
    pub fn find(&self, name: &str) -> Option<&LoadedProvider> {
        self.providers.iter().find(|p| p.matches(name))
    }

    pub fn find_by_type(&self, t: &ProviderType) -> Option<&LoadedProvider> {
        self.providers.iter().find(|p| &p.provider_type == t)
    }

    /// First provider in registry order (used as global default for onboarding).
    pub fn default(&self) -> &LoadedProvider {
        self.providers
            .first()
            .expect("registry must have at least one provider")
    }
}

// ── Loader entry points ───────────────────────────────────────────────────

/// Load all bundled provider TOMLs. Returns the registry on success, fails
/// hard on the first malformed file — bundled TOMLs are validated at compile
/// time via tests, so a runtime failure here means a build went out with a
/// broken preset.
pub fn load_all_bundled() -> Result<ProviderRegistry, LoadError> {
    let mut providers = Vec::with_capacity(BUNDLED_TOMLS.len());
    for (filename, src) in BUNDLED_TOMLS {
        providers.push(parse_and_validate(filename, src)?);
    }
    Ok(ProviderRegistry { providers })
}

/// Cached registry — loaded once on first access. Used by the factory.
pub fn registry() -> &'static ProviderRegistry {
    static CACHED: OnceLock<ProviderRegistry> = OnceLock::new();
    CACHED.get_or_init(|| {
        load_all_bundled()
            .unwrap_or_else(|e| panic!("BUG: bundled provider TOML failed to load at startup: {e}"))
    })
}

// ── Internals ─────────────────────────────────────────────────────────────

fn parse_and_validate(filename: &str, src: &str) -> Result<LoadedProvider, LoadError> {
    let cfg: ProviderConfigFile = toml::from_str(src).map_err(|e| LoadError::Parse {
        provider: filename.to_string(),
        source: e,
    })?;

    if cfg.schema_version != 1 {
        return Err(LoadError::SchemaVersion {
            provider: filename.to_string(),
            actual: cfg.schema_version,
        });
    }

    if cfg.provider.name != filename {
        return Err(LoadError::NameMismatch {
            filename: filename.to_string(),
            declared: cfg.provider.name.clone(),
        });
    }

    let provider_type = parse_provider_type(&cfg.provider.provider_type, filename)?;

    // Validate models: exactly 1 default, ≤ 1 fast, no duplicate ids.
    let default_count = cfg.models.iter().filter(|m| m.default).count();
    if default_count == 0 {
        return Err(LoadError::NoDefaultModel {
            provider: filename.to_string(),
        });
    }
    if default_count > 1 {
        return Err(LoadError::MultipleDefaults {
            provider: filename.to_string(),
            count: default_count,
        });
    }
    let fast_count = cfg.models.iter().filter(|m| m.fast).count();
    if fast_count > 1 {
        return Err(LoadError::MultipleFastModels {
            provider: filename.to_string(),
            count: fast_count,
        });
    }
    let mut seen_ids = std::collections::HashSet::new();
    for m in &cfg.models {
        if !seen_ids.insert(&m.id) {
            return Err(LoadError::DuplicateModelId {
                provider: filename.to_string(),
                id: m.id.clone(),
            });
        }
    }

    Ok(into_loaded(cfg, provider_type))
}

fn parse_provider_type(s: &str, filename: &str) -> Result<ProviderType, LoadError> {
    match s {
        "deepseek" => Ok(ProviderType::Deepseek),
        "ollama" => Ok(ProviderType::Ollama),
        "alibaba" => Ok(ProviderType::Alibaba),
        "glm" => Ok(ProviderType::Glm),
        "openai_compat" => Ok(ProviderType::OpenAiCompat),
        other => Err(LoadError::UnknownProviderType {
            provider: filename.to_string(),
            got: other.to_string(),
        }),
    }
}

fn into_loaded(cfg: ProviderConfigFile, provider_type: ProviderType) -> LoadedProvider {
    let default_model = cfg
        .models
        .iter()
        .find(|m| m.default)
        .map(|m| m.id.clone())
        .expect("validated by parse_and_validate");
    let fast_model = cfg.models.iter().find(|m| m.fast).map(|m| m.id.clone());

    let known_models = cfg
        .models
        .iter()
        .map(|m| ModelMetadata {
            id: m.id.clone(),
            display_name: m.display_name.clone(),
            description: m.description.clone(),
            context_window: m.context_window,
            max_output_tokens: m.max_output_tokens,
            supports_thinking: m.supports_thinking,
            pricing: m.pricing.map(|p| ModelPricing {
                input_per_mtk: p.input_per_mtk,
                output_per_mtk: p.output_per_mtk,
                cache_creation_per_mtk: p.cache_creation_per_mtk,
                cache_read_per_mtk: p.cache_read_per_mtk,
            }),
        })
        .collect();

    let api_format = match cfg.provider.api_format {
        ApiFormatToml::Anthropic => ApiFormat::Anthropic,
        ApiFormatToml::Openai => ApiFormat::OpenAI,
        ApiFormatToml::Ollama => ApiFormat::Ollama,
    };

    // Leak env_var strings to satisfy the `&'static [&'static str]` shape
    // of AuthConfig. The leak is one-time at startup (provider registry is
    // a OnceLock global) — total memory cost is a few hundred bytes.
    let env_vars: Vec<&'static str> = cfg
        .auth
        .env_vars
        .iter()
        .map(|s| Box::leak(s.clone().into_boxed_str()) as &'static str)
        .collect();
    let env_vars: &'static [&'static str] = Box::leak(env_vars.into_boxed_slice());
    let keychain_key: &'static str = Box::leak(cfg.auth.keychain_key.clone().into_boxed_str());
    let display_label: &'static str = Box::leak(cfg.auth.display_label.clone().into_boxed_str());

    let capabilities = ProviderCapabilities {
        name: cfg.provider.name.clone(),
        display_name: cfg.provider.display_name.clone(),
        attribution: cfg.provider.attribution.clone(),
        default_model,
        fast_model,
        known_models,
        default_max_tokens: cfg.defaults.max_tokens,
        default_thinking_budget: cfg.defaults.thinking_budget,
        api_format,
        default_api_base: cfg.provider.api_base.clone(),
        auth: AuthConfig {
            env_vars,
            keychain_key,
            display_label,
            required: cfg.auth.required,
        },
    };

    let max_retries = cfg.defaults.max_retries;
    let request_timeout_sec = cfg.defaults.request_timeout_sec;

    LoadedProvider {
        capabilities,
        aliases: cfg.provider.aliases.clone(),
        provider_type,
        description: cfg.provider.description.clone(),
        max_retries,
        request_timeout_sec,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_bundled_tomls_load() {
        let registry = load_all_bundled().expect("all bundled TOMLs must load");
        let names: Vec<&str> = registry
            .providers
            .iter()
            .map(|p| p.capabilities.name.as_str())
            .collect();
        assert!(names.contains(&"deepseek"));
        assert!(names.contains(&"alibaba"));
        assert!(names.contains(&"openrouter"));
        assert!(names.contains(&"mistral"));
        assert!(names.contains(&"ollama"));
        assert!(names.contains(&"openai"));
        assert!(names.contains(&"glm"));
    }

    #[test]
    fn deepseek_loaded_capabilities_match_audit() {
        let registry = load_all_bundled().unwrap();
        let ds = registry.find("deepseek").expect("deepseek present");
        assert_eq!(ds.capabilities.default_model, "deepseek-v4-pro");
        assert_eq!(
            ds.capabilities.fast_model.as_deref(),
            Some("deepseek-v4-flash")
        );
        let v4_pro = ds
            .capabilities
            .known_models
            .iter()
            .find(|m| m.id == "deepseek-v4-pro")
            .unwrap();
        assert_eq!(v4_pro.context_window, 1_000_000);
        assert_eq!(v4_pro.max_output_tokens, 384_000);
        assert!(v4_pro.supports_thinking);
        let p = v4_pro.pricing.expect("v4-pro must have pricing");
        assert!((p.input_per_mtk - 0.435).abs() < 1e-9);
        assert!((p.output_per_mtk - 0.87).abs() < 1e-9);
    }

    #[test]
    fn glm_loaded() {
        let registry = load_all_bundled().unwrap();
        let glm = registry.find("glm").expect("glm present");
        // Default model is now the vision variant (PR S follow-up: bench
        // needs PDF/image reading; text-only model can't do it).
        assert_eq!(glm.capabilities.default_model, "glm-4.6v");
        assert!(glm.matches("zhipu"));
        assert!(glm.matches("bigmodel"));
        // Verify glm-4.6 is still present as a non-default text-only option
        assert!(glm
            .capabilities
            .known_models
            .iter()
            .any(|m| m.id == "glm-4.6"));
    }

    #[test]
    fn alias_matching_is_case_insensitive() {
        let registry = load_all_bundled().unwrap();
        assert!(registry.find("DEEPSEEK").is_some());
        assert!(registry.find("DS").is_some());
        assert!(registry.find("Qwen").is_some());
    }

    #[test]
    fn registry_default_is_deepseek() {
        let registry = load_all_bundled().unwrap();
        assert_eq!(registry.default().capabilities.name, "deepseek");
    }

    #[test]
    fn malformed_toml_no_default_model_fails() {
        let src = r#"
schema_version = 1

[provider]
name = "broken"
display_name = "Broken"
description = "Has no default"
attribution = "test"
provider_type = "openai_compat"
api_format = "openai"
api_base = "https://example.com"

[auth]
keychain_key = "broken"
display_label = "Broken"

[defaults]
max_tokens = 1000

[[models]]
id = "only-model"
display_name = "Only"
context_window = 1000
max_output_tokens = 100
"#;
        let res = parse_and_validate("broken", src);
        assert!(matches!(res, Err(LoadError::NoDefaultModel { .. })));
    }

    #[test]
    fn filename_must_match_provider_name() {
        let src = r#"
schema_version = 1

[provider]
name = "wrong_name"
display_name = "X"
description = "X"
attribution = "X"
provider_type = "openai_compat"
api_format = "openai"
api_base = "https://example.com"

[auth]
keychain_key = "x"
display_label = "X"

[defaults]
max_tokens = 1000

[[models]]
id = "m"
display_name = "M"
context_window = 1000
max_output_tokens = 100
default = true
"#;
        let res = parse_and_validate("expected_name", src);
        assert!(matches!(res, Err(LoadError::NameMismatch { .. })));
    }
}
