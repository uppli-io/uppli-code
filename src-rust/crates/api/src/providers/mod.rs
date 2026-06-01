//! Provider configuration loaded from TOML files (one per provider).
//!
//! Replaces the previous hardcoded constructors in `openai_provider.rs` and
//! the hardcoded `ProviderPreset` static array in `provider_factory.rs`.
//!
//! # Layout
//!
//! ```
//! crates/api/presets/
//!   deepseek.toml
//!   alibaba.toml
//!   openrouter.toml
//!   mistral.toml
//!   ollama.toml
//!   openai.toml
//!   glm.toml
//! ```
//!
//! Each TOML file is bundled into the binary via `include_str!` at compile
//! time (see `loader::BUNDLED_TOMLS`). The loader parses them into
//! `ProviderConfigFile` and converts each into a `ProviderCapabilities`
//! ready for `LlmProvider` implementations to consume.
//!
//! # Adding a new provider
//!
//! 1. Drop a `provider-name.toml` in `crates/api/presets/`.
//! 2. Add an `include_str!` line in `loader::BUNDLED_TOMLS`.
//! 3. Done. No Rust code to touch — no `match` arms, no `enum` variants
//!    outside the parser, no hardcoded constants.

pub mod loader;
pub mod schema;

pub use loader::{load_all_bundled, ProviderRegistry};
pub use schema::{
    ApiFormatToml, AuthToml, ProviderConfigFile, ProviderDefaultsToml, ProviderModelToml,
    ProviderPricingToml, ProviderToml,
};
