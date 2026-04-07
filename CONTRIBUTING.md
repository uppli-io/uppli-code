# Contributing

We accept PRs. Here's how to get started.

## Setup

```bash
git clone https://github.com/uppli-io/uppli-code.git
cd uppli-code/src-rust
cargo build
cargo test
```

Rust 1.75+ required. The build takes about 20 seconds on first run.

## Before submitting a PR

```bash
cargo fmt          # format everything
cargo clippy       # must be 0 warnings
cargo test         # ~270 tests, all must pass
```

If clippy complains, fix it. If a test breaks, fix it. Don't disable warnings.

## Where to contribute

**Good first issues:** look for `good first issue` label on GitHub.

**Provider support:** adding a new LLM provider is the easiest way to contribute. See `crates/api/src/provider_factory.rs` for the registry and `crates/api/src/openai_provider.rs` for the OpenAI-compatible implementation. Most providers need a `ProviderPreset` entry and nothing else.

**Tool improvements:** tools live in `crates/tools/src/`. Each tool is a struct that implements the `Tool` trait.

**TUI:** the terminal interface is in `crates/tui/src/` using ratatui.

## Architecture

The codebase is a Rust workspace with 11 crates. The main ones:

`api` has the `LlmProvider` trait and all provider implementations. Every provider describes itself fully (models, auth, pricing, thinking support) so the rest of the code never needs to know which provider is active.

`query` runs the agentic loop. It sends messages to the provider, processes tool calls, feeds results back, handles compaction when context gets large.

`cli` is the binary. Handles onboarding, TUI mode, headless mode, and the SDK protocol for VS Code.

`core` has config, permissions, keychain integration, cost tracking, and shared types.

## Style

Write idiomatic Rust. Match existing patterns. Don't add abstractions unless they pay for themselves.

Keep comments short and only where the code isn't self-explanatory. No doc comments on obvious getters.

## Commit messages

Start with a verb. Keep the first line under 72 chars. Body is optional but welcome for non-trivial changes.

```
fix: qwen3 phantom tool_call with empty id
feat: provider registry for zero-match onboarding
refactor: consolidate context_window into provider trait
```

## API keys

Never commit API keys. They go in env vars or the OS keychain, not in code or config files. If you see one in the codebase, that's a bug.

## Questions

Open an issue. We're responsive.
