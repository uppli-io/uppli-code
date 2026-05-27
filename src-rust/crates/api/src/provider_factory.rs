// provider_factory.rs — Create an LlmProvider from configuration.
//
// This is the single entry point for constructing provider instances.
// The CLI reads the config, picks the provider type, and calls
// `create_provider()` to get a boxed `dyn LlmProvider`.
//
// **Security:** API keys are NEVER read from settings.json.
// Each provider resolves its key from a dedicated environment variable:
//   - DeepSeek:  DEEPSEEK_API_KEY or ANTHROPIC_API_KEY
//   - Alibaba:   DASHSCOPE_API_KEY
//   - OpenAI:    OPENAI_API_KEY
//   - Ollama:    no key needed

use crate::provider::{AuthConfig, LlmProvider, ProviderPreset};
use cc_core::config::{Config, ProviderSettings, ProviderType};

// ---------------------------------------------------------------------------
// Provider registry — the single source of truth for all providers
// ---------------------------------------------------------------------------

/// Static registry of all known providers.  The CLI onboarding menu and
/// `--provider` flag parsing both iterate over this.
/// Adding a new provider = adding a `ProviderPreset` here + a factory arm below.
pub fn provider_registry() -> &'static [ProviderPreset] {
    static REGISTRY: &[ProviderPreset] = &[
        ProviderPreset {
            name: "deepseek",
            aliases: &["ds"],
            display_name: "DeepSeek",
            description: "deepseek-reasoner (cloud, API key)",
            default_model: "deepseek-reasoner",
            fast_model: Some("deepseek-chat"),
            supports_thinking: true,
            auth: AuthConfig {
                env_vars: &["DEEPSEEK_API_KEY", "ANTHROPIC_API_KEY"],
                keychain_key: "deepseek",
                display_label: "DeepSeek",
                required: true,
            },
            provider_type: ProviderType::Deepseek,
        },
        ProviderPreset {
            name: "alibaba",
            aliases: &["dashscope", "qwen"],
            display_name: "Qwen",
            description: "Qwen 3.6 Plus — agentic coding, 1M context",
            default_model: "qwen3.6-plus-2026-04-02",
            fast_model: Some("qwen-turbo-latest"),
            supports_thinking: true,
            auth: AuthConfig {
                env_vars: &["DASHSCOPE_API_KEY"],
                keychain_key: "alibaba",
                display_label: "DashScope",
                required: true,
            },
            provider_type: ProviderType::Alibaba,
        },
        ProviderPreset {
            name: "openrouter",
            aliases: &[],
            display_name: "OpenRouter",
            description: "Any model via OpenRouter proxy (Qwen, Claude, etc.)",
            default_model: "qwen/qwen3.6-plus",
            fast_model: None,
            supports_thinking: true,
            auth: AuthConfig {
                env_vars: &["OPENROUTER_API_KEY"],
                keychain_key: "openrouter",
                display_label: "OpenRouter",
                required: true,
            },
            provider_type: ProviderType::OpenAiCompat,
        },
        ProviderPreset {
            name: "mistral",
            aliases: &[],
            display_name: "Mistral",
            description: "Mistral Large (cloud, API key)",
            default_model: "mistral-large-latest",
            fast_model: Some("mistral-small-latest"),
            supports_thinking: false,
            auth: AuthConfig {
                env_vars: &["MISTRAL_API_KEY"],
                keychain_key: "mistral",
                display_label: "Mistral",
                required: true,
            },
            provider_type: ProviderType::OpenAiCompat,
        },
        ProviderPreset {
            name: "ollama",
            aliases: &["local"],
            display_name: "Ollama",
            description: "Local models: llama3, qwen2.5, gemma, etc. (no key)",
            default_model: "gemma4",
            fast_model: None,
            supports_thinking: false,
            auth: AuthConfig {
                env_vars: &[],
                keychain_key: "ollama",
                display_label: "Ollama",
                required: false,
            },
            provider_type: ProviderType::Ollama,
        },
        ProviderPreset {
            name: "openai",
            aliases: &["openai-compat", "openai", "custom"],
            display_name: "Custom endpoint",
            description: "Any OpenAI-compatible API (vLLM, LiteLLM, etc.)",
            default_model: "default",
            fast_model: None,
            supports_thinking: false,
            auth: AuthConfig {
                env_vars: &["OPENAI_API_KEY"],
                keychain_key: "openai",
                display_label: "API",
                required: false, // some custom endpoints don't need auth
            },
            provider_type: ProviderType::OpenAiCompat,
        },
    ];
    REGISTRY
}

/// Find a preset by name or alias (case-insensitive).
pub fn find_preset(name: &str) -> Option<&'static ProviderPreset> {
    provider_registry().iter().find(|p| p.matches(name))
}

/// The default provider preset (first in the registry).
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
    let (provider_type, preset_name) = if let Some(name) = provider_override {
        let preset = find_preset(name).ok_or_else(|| {
            let names: Vec<&str> = provider_registry().iter().map(|p| p.name).collect();
            anyhow::anyhow!(
                "Unknown provider '{}'. Available: {}",
                name,
                names.join(", ")
            )
        })?;
        (preset.provider_type.clone(), Some(preset.name))
    } else {
        (config.provider.clone(), None)
    };

    let provider_key = provider_type.to_string();
    let provider_settings = config.providers.get(&provider_key);

    match provider_type {
        ProviderType::Deepseek => create_deepseek_provider(config, provider_settings),
        _ => create_openai_provider(&provider_type, config, provider_settings, preset_name),
    }
}

/// Resolve an API key using the provider's `AuthConfig`.
///
/// Priority:
///   1. Environment variables (highest — ephemeral, most secure)
///   2. OS keychain (macOS Keychain / Linux libsecret)
///   3. None (caller may fall back to config or prompt)
pub fn resolve_key(auth: &crate::AuthConfig) -> Option<String> {
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
// DeepSeek (Anthropic-compatible format)
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// DeepSeek (Anthropic-compatible format — special case, own client)
// ---------------------------------------------------------------------------

fn create_deepseek_provider(
    config: &Config,
    settings: Option<&ProviderSettings>,
) -> anyhow::Result<Box<dyn LlmProvider>> {
    let auth = crate::AuthConfig {
        env_vars: &["DEEPSEEK_API_KEY", "ANTHROPIC_API_KEY"],
        keychain_key: "deepseek",
        display_label: "DeepSeek",
        required: true,
    };
    let api_key = resolve_key(&auth)
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
// OpenAI-compatible providers (Ollama, Alibaba, Mistral, generic)
// ---------------------------------------------------------------------------

/// Create an OpenAI-compatible provider. Works for all non-DeepSeek providers.
///
/// Flow:
///   1. Build the default preset config
///   2. Apply user overrides from settings.json
///   3. Resolve API key via the preset's AuthConfig
///   4. Construct the provider
fn create_openai_provider(
    provider_type: &ProviderType,
    config: &Config,
    settings: Option<&ProviderSettings>,
    preset_name: Option<&str>,
) -> anyhow::Result<Box<dyn LlmProvider>> {
    // Step 1: Build preset config.
    let model = settings.and_then(|s| s.model.clone()).unwrap_or_else(|| {
        // Use preset name directly if available, fall back to provider_type lookup
        preset_name
            .and_then(find_preset)
            .map(|p| p.default_model.to_string())
            .unwrap_or_else(|| default_model_for(provider_type))
    });

    let mut cfg = match (provider_type, preset_name) {
        (ProviderType::Ollama, _) => crate::OpenAiProviderConfig::ollama(&model),
        (ProviderType::Alibaba, _) => crate::OpenAiProviderConfig::alibaba("", &model),
        (_, Some("openrouter")) => crate::OpenAiProviderConfig::openrouter("", &model),
        (ProviderType::OpenAiCompat, _) => {
            let api_base = settings
                .and_then(|s| s.api_base.clone())
                .unwrap_or_else(|| "http://localhost:8080/v1".to_string());
            crate::OpenAiProviderConfig::generic("OpenAI-compat", &api_base, "", &model)
        }
        _ => unreachable!("DeepSeek handled separately"),
    };

    // Step 2: Apply user overrides from settings.json.
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

    // Step 2b: CLI --api-base (passed via UPPLI_API_BASE env) overrides everything.
    if let Ok(base) = std::env::var("UPPLI_API_BASE") {
        cfg.api_base = base;
    }

    // Step 3: Resolve API key via the preset's AuthConfig.
    // Always check config.api_key first (from --api-key flag).
    if let Some(ref key) = config.api_key {
        if key.len() > 8 {
            cfg.api_key = key.clone();
        }
    }
    if cfg.api_key.is_empty() && cfg.auth.required {
        let api_key = resolve_key(&cfg.auth)
            .or_else(|| config.api_key.clone().filter(|k| k.len() > 8))
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "No API key found for {} provider. Set {} environment variable.",
                    cfg.auth.display_label,
                    cfg.auth.env_vars.first().unwrap_or(&"API_KEY"),
                )
            })?;
        cfg.api_key = api_key;
    }

    // Step 4: Construct.
    Ok(Box::new(crate::OpenAiProvider::new(cfg)?))
}

fn default_model_for(provider_type: &ProviderType) -> String {
    // Use the preset's default_model to stay in sync.
    // Falls back to hardcoded values only if no preset is found.
    let preset_name = match provider_type {
        ProviderType::Ollama => "ollama",
        ProviderType::Alibaba => "alibaba",
        ProviderType::OpenAiCompat => "openai",
        ProviderType::Deepseek => "deepseek",
    };
    find_preset(preset_name)
        .map(|p| p.default_model.to_string())
        .unwrap_or_else(|| match provider_type {
            ProviderType::Ollama => "llama3".to_string(),
            ProviderType::Alibaba => "qwen3.6-plus-2026-04-02".to_string(),
            ProviderType::OpenAiCompat => "default".to_string(),
            ProviderType::Deepseek => "deepseek-reasoner".to_string(),
        })
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
        assert!(find_preset("unknown").is_none());
    }

    #[test]
    fn test_registry_has_all_providers() {
        let registry = provider_registry();
        assert!(registry.len() >= 5);
        // Every preset has a non-empty name and description
        for p in registry {
            assert!(!p.name.is_empty());
            assert!(!p.description.is_empty());
        }
    }

    #[test]
    fn test_resolve_key_no_env_returns_none_or_keychain() {
        let auth = crate::AuthConfig {
            env_vars: &["NONEXISTENT_KEY_XYZZY_12345"],
            keychain_key: "test-nonexistent",
            display_label: "Test",
            required: false,
        };
        // Just verify it doesn't panic.
        let _ = resolve_key(&auth);
    }
}
