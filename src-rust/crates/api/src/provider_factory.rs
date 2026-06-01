// provider_factory.rs — Create an LlmProvider from configuration.
//
// PR S (2026-05-31): rewrote this from 374 hardcoded lines to a thin layer
// over `providers::loader`. The static `REGISTRY` array, the 5 hardcoded
// preset constructors (ollama/openrouter/alibaba/mistral/generic), and
// the `default_model_for` fallback function are all GONE. Adding a new
// provider is now: drop a `.toml` file in `crates/api/presets/` and one
// `include_str!` line in `loader::BUNDLED_TOMLS`.
//
// **Security:** API keys are NEVER read from settings.json. They come from
// environment variables (declared per-provider in the TOML) or the OS
// keychain. Same as before.

use crate::provider::{AuthConfig, LlmProvider, ProviderPreset};
use crate::providers::loader;
use cc_core::config::{Config, ProviderSettings, ProviderType};
use std::sync::OnceLock;

// ---------------------------------------------------------------------------
// Registry — derived once from the TOML loader
// ---------------------------------------------------------------------------
//
// `provider_registry()` returns a slice of `ProviderPreset` for the CLI's
// onboarding menu and the `--provider` flag completer. The presets are
// reconstructed at startup from the loader's `LoadedProvider` list.
//
// Why ProviderPreset is still its own type: it's used by code outside this
// crate (CLI onboarding, model picker UI) that doesn't want to depend on
// the loader's runtime types. Keeping it stable here is the contract.

fn build_registry() -> Vec<ProviderPreset> {
    loader::registry()
        .providers
        .iter()
        .map(|loaded| {
            // Leak aliases to &'static for ProviderPreset's &'static fields.
            // One-time leak at startup, total cost ~kilobytes.
            let aliases: Vec<&'static str> = loaded
                .aliases
                .iter()
                .map(|s| Box::leak(s.clone().into_boxed_str()) as &'static str)
                .collect();
            let aliases: &'static [&'static str] = Box::leak(aliases.into_boxed_slice());

            ProviderPreset {
                name: Box::leak(loaded.capabilities.name.clone().into_boxed_str()),
                aliases,
                display_name: Box::leak(loaded.capabilities.display_name.clone().into_boxed_str()),
                description: Box::leak(loaded.description.clone().into_boxed_str()),
                default_model: Box::leak(
                    loaded.capabilities.default_model.clone().into_boxed_str(),
                ),
                fast_model: loaded
                    .capabilities
                    .fast_model
                    .as_ref()
                    .map(|s| Box::leak(s.clone().into_boxed_str()) as &'static str),
                supports_thinking: loaded.capabilities.default_thinking_budget.is_some(),
                auth: loaded.capabilities.auth,
                provider_type: loaded.provider_type.clone(),
            }
        })
        .collect()
}

pub fn provider_registry() -> &'static [ProviderPreset] {
    static CACHED: OnceLock<Vec<ProviderPreset>> = OnceLock::new();
    CACHED.get_or_init(build_registry).as_slice()
}

/// Find a preset by name or alias (case-insensitive).
pub fn find_preset(name: &str) -> Option<&'static ProviderPreset> {
    provider_registry().iter().find(|p| p.matches(name))
}

/// The default provider preset (first in registry order).
pub fn default_preset() -> &'static ProviderPreset {
    &provider_registry()[0]
}

// ---------------------------------------------------------------------------
// Provider construction
// ---------------------------------------------------------------------------

/// Create an LLM provider from the application config.
pub fn create_provider(
    config: &Config,
    provider_override: Option<&str>,
) -> anyhow::Result<Box<dyn LlmProvider>> {
    let provider_name = if let Some(name) = provider_override {
        // Validate that the override matches a known provider.
        loader::registry()
            .find(name)
            .map(|p| p.capabilities.name.clone())
            .ok_or_else(|| {
                let names: Vec<&str> = loader::registry()
                    .providers
                    .iter()
                    .map(|p| p.capabilities.name.as_str())
                    .collect();
                anyhow::anyhow!(
                    "Unknown provider '{}'. Available: {}",
                    name,
                    names.join(", ")
                )
            })?
    } else {
        // Look up by provider_type from config.
        loader::registry()
            .find_by_type(&config.provider)
            .map(|p| p.capabilities.name.clone())
            .unwrap_or_else(|| loader::registry().default().capabilities.name.clone())
    };

    let loaded = loader::registry()
        .find(&provider_name)
        .ok_or_else(|| anyhow::anyhow!("Provider '{}' not loaded", provider_name))?;
    let provider_settings = config.providers.get(&loaded.provider_type.to_string());

    match loaded.provider_type {
        ProviderType::Deepseek => create_deepseek_provider(loaded, config, provider_settings),
        _ => create_openai_compat_provider(loaded, config, provider_settings),
    }
}

/// Resolve an API key using the provider's `AuthConfig`.
///
/// Priority:
///   1. Environment variables (highest — ephemeral, most secure)
///   2. OS keychain (macOS Keychain / Linux libsecret)
///   3. None (caller may fall back to config or prompt)
pub fn resolve_key(auth: &AuthConfig) -> Option<String> {
    for var in auth.env_vars {
        if let Ok(key) = std::env::var(var) {
            if !key.is_empty() && key.len() > 8 {
                return Some(key);
            }
        }
    }
    cc_core::keychain::get_key(auth.keychain_key)
}

// ---------------------------------------------------------------------------
// DeepSeek (Anthropic-compatible format — special client)
// ---------------------------------------------------------------------------

fn create_deepseek_provider(
    loaded: &loader::LoadedProvider,
    config: &Config,
    settings: Option<&ProviderSettings>,
) -> anyhow::Result<Box<dyn LlmProvider>> {
    let auth = &loaded.capabilities.auth;
    let api_key = resolve_key(auth)
        .or_else(|| config.api_key.clone().filter(|k| k.len() > 8))
        .ok_or_else(|| {
            anyhow::anyhow!(
                "No API key found. Set {} or use --api-key.",
                auth.env_vars.first().unwrap_or(&"DEEPSEEK_API_KEY")
            )
        })?;

    let api_base = settings
        .and_then(|s| s.api_base.clone())
        .unwrap_or_else(|| config.resolve_api_base());

    let use_bearer_auth = api_key.starts_with("eyJ");

    let client = crate::AnthropicClient::new(crate::client::ClientConfig {
        api_key,
        api_base,
        use_bearer_auth,
        ..Default::default()
    })?;

    Ok(Box::new(client))
}

// ---------------------------------------------------------------------------
// OpenAI-compatible providers (everything not DeepSeek)
// ---------------------------------------------------------------------------

fn create_openai_compat_provider(
    loaded: &loader::LoadedProvider,
    config: &Config,
    settings: Option<&ProviderSettings>,
) -> anyhow::Result<Box<dyn LlmProvider>> {
    // Resolve the model: CLI/settings override > preset default.
    let model = settings.and_then(|s| s.model.clone());

    // Resolve API key. CLI --api-key wins, then env / keychain.
    let api_key = if let Some(key) = config.api_key.clone().filter(|k| k.len() > 8) {
        key
    } else if loaded.capabilities.auth.required {
        resolve_key(&loaded.capabilities.auth).ok_or_else(|| {
            anyhow::anyhow!(
                "No API key found for {} provider. Set {} environment variable.",
                loaded.capabilities.auth.display_label,
                loaded
                    .capabilities
                    .auth
                    .env_vars
                    .first()
                    .unwrap_or(&"API_KEY"),
            )
        })?
    } else {
        String::new()
    };

    let mut cfg = crate::OpenAiProviderConfig::from_loaded(loaded, api_key, model);

    // Apply user overrides from settings.json (api_base, fast_model, supports_thinking).
    if let Some(s) = settings {
        if let Some(ref base) = s.api_base {
            cfg.api_base = base.clone();
        }
        if let Some(ref fm) = s.fast_model {
            cfg.fast_model = Some(fm.clone());
        }
        if let Some(thinking) = s.supports_thinking {
            cfg.supports_thinking = thinking;
        }
    }

    // CLI --api-base (UPPLI_API_BASE env) overrides everything.
    if let Ok(base) = std::env::var("UPPLI_API_BASE") {
        cfg.api_base = base;
    }

    Ok(Box::new(crate::OpenAiProvider::new(cfg)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_find_preset_by_name_and_alias() {
        assert!(find_preset("deepseek").is_some());
        assert!(find_preset("ds").is_some());
        assert_eq!(find_preset("ds").unwrap().name, "deepseek");
        assert!(find_preset("ollama").is_some());
        assert!(find_preset("local").is_some());
        assert_eq!(find_preset("local").unwrap().name, "ollama");
        assert!(find_preset("alibaba").is_some());
        assert!(find_preset("qwen").is_some());
        assert!(find_preset("dashscope").is_some());
        assert!(find_preset("mistral").is_some());
        assert!(find_preset("openai").is_some());
        assert!(find_preset("glm").is_some());
        assert!(find_preset("zhipu").is_some());
        assert!(find_preset("unknown").is_none());
    }

    #[test]
    fn test_registry_has_all_providers() {
        let registry = provider_registry();
        assert!(
            registry.len() >= 7,
            "should have at least 7 providers (deepseek, alibaba, openrouter, mistral, ollama, openai, glm), got {}",
            registry.len()
        );
        // Every preset has a non-empty name and description
        for p in registry {
            assert!(!p.name.is_empty());
            assert!(!p.description.is_empty());
        }
    }

    #[test]
    fn test_resolve_key_no_env_returns_none_or_keychain() {
        let auth = AuthConfig {
            env_vars: &["NONEXISTENT_KEY_XYZZY_12345"],
            keychain_key: "test-nonexistent",
            display_label: "Test",
            required: false,
        };
        // Just verify it doesn't panic.
        let _ = resolve_key(&auth);
    }

    #[test]
    fn test_default_preset_is_deepseek() {
        assert_eq!(default_preset().name, "deepseek");
    }
}
