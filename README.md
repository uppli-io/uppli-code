# uppli-code

Open source coding agent. Works with DeepSeek, Qwen3, Ollama, Mistral, or any OpenAI-compatible endpoint. Written in Rust.

Think Claude Code, but you pick your model and your provider. No lock-in, no subscription wall.

## Why

Claude Code is good but closed. You pay Anthropic, you use their models, you can't see the source. We wanted something we could run with DeepSeek R2 at $0.30/day or Qwen3-235B via Alibaba, or a local Ollama model for free. So we built it.

16MB binary. Starts in 50ms. Full agentic loop with tool use, file editing, bash execution, thinking mode, and multi-turn context.

## Quick start

```bash
git clone https://github.com/uppli-io/uppli-code.git
cd uppli-code/src-rust
cargo build --release
./target/release/uppli-code
```

First time you launch it, it asks you to pick a provider and enter your API key. The key goes in your OS keychain (macOS Keychain / Linux libsecret), not in a config file.

## Providers

| Provider | Models | Thinking | Key env var |
|----------|--------|----------|-------------|
| DeepSeek | deepseek-reasoner, deepseek-chat | yes | `DEEPSEEK_API_KEY` |
| Alibaba | qwen3-235b, qwen3-32b, qwen3-30b | yes | `DASHSCOPE_API_KEY` |
| Mistral | mistral-large, mistral-small | no | `MISTRAL_API_KEY` |
| Ollama | any local model | depends | none |
| OpenAI-compat | anything | depends | `OPENAI_API_KEY` |

Switch with `--provider`:

```bash
uppli-code --provider alibaba
uppli-code --provider ollama
uppli-code --provider deepseek   # default
```

Aliases work: `--provider qwen`, `--provider ds`, `--provider local`.

## Hybrid mode

When your provider has a reasoning model and a fast model (DeepSeek, Qwen3), uppli-code switches between them automatically. Your question goes to the big model with thinking. Tool results get processed by the fast one. Saves tokens without losing quality.

Set it up in `~/.uppli/settings.json`:

```json
{
  "config": {
    "provider": "alibaba",
    "providers": {
      "alibaba": {
        "model": "qwen3-235b-a22b",
        "fastModel": "qwen3-32b",
        "supportsThinking": true
      }
    }
  }
}
```

## Tools

File read/write/edit, bash, glob, grep, web fetch, web search, git, MCP servers, jupyter notebooks. 40+ tools, same coverage as Claude Code.

## VS Code

Works with the Uppli Code VS Code extension (separate repo). The CLI talks to the extension via stdin/stdout using the Claude Agent SDK protocol.

## Project layout

```
src-rust/
  crates/
    cli/      # entry point, onboarding, headless mode
    api/      # LlmProvider trait, providers, factory
    core/     # config, types, permissions, keychain
    query/    # agentic loop, tool dispatch, compaction
    tui/      # terminal UI (ratatui)
    tools/    # tool implementations
    commands/ # slash commands
    mcp/      # MCP server support
```

## Adding a provider

Two changes:

1. Add a `ProviderPreset` in `crates/api/src/provider_factory.rs`
2. If the wire format is new, add a provider impl (most reuse `OpenAiProvider`)

Nothing else. The registry drives the CLI, onboarding, and model picker.

## Build

Rust 1.75+.

```bash
cd src-rust
cargo build --release
cargo test              # ~270 tests
cargo clippy            # should be 0 warnings
```

## Legal

This is a clean-room Rust reimplementation. No proprietary source code was copied. API protocols are not copyrightable (Oracle v. Google, US Supreme Court 2021).

## License

MIT. See [LICENSE](LICENSE).
