# uppli-code

Open source coding agent. Works with DeepSeek, Qwen3, Ollama, Mistral, or any OpenAI-compatible endpoint. Written in Rust.

Think Claude Code, but you pick your model and your provider. No lock-in, no subscription wall.

## Benchmark

**20/20 (100%) diff produced on SWE-bench Verified astropy** — the hardest repo in the benchmark.

| Agent | Model | Cost/M tokens | Astropy (20 issues) |
|-------|-------|---------------|---------------------|
| **uppli-code** | **Qwen 3.6 Plus** | **$0.29** | **100% diff produced** |
| Claude Code | Opus 4.6 | $15.00 | 80.9% (full benchmark) |
| Qwen Code | Qwen 3.6 Plus | $0.29 | 78.8% (full benchmark) |

> Diff produced = the agent generated a patch for each issue. Full SWE-bench validation (Docker + unit tests) pending. Scores for Claude Code and Qwen Code are on the full 500-issue benchmark, not directly comparable.

### What got us there

| Optimization | Before | After |
|-------------|--------|-------|
| AstEdit (ast-grep structural editing) | Edit fails on indentation | AST handles indentation automatically |
| RAG vectoriel (pattern examples) | Model writes wrong patterns | Model gets examples before writing |
| Post-edit linting | Broken code stays | Syntax errors caught immediately |
| Loop detection | Model repeats same Grep 25x | Nudge after 3 repeats |
| System prompt (plan + verify) | Model edits without thinking | Model plans, edits, verifies |

### Agent configuration (MCP server mode)

| Parameter | uppli-code | Claude Code Opus | Advantage |
|-----------|-----------|-----------------|-----------|
| `max_turns` | **250** | 200 | +25% more attempts |
| `thinking_budget` | **131,072** | 128,000 | > Claude |
| `max_tokens` | **32,768** | 16,384 | **2x output** |
| `context_window` | **1,000,000** | 200,000 | **5x context** |
| `tool_result_budget` | **0 (no truncation)** | ~100K chars | Full history |
| `compaction` | Never (100K << 800K) | Triggered often | No info loss |
| `fallback_model` | None (full reasoning) | Sonnet 4.6 | Consistent quality |
| `edit tool` | **Edit + AstEdit** | str_replace only | AST-level precision |
| `RAG for tools` | **✅ 106 patterns** | ❌ | Better tool usage |
| `post-edit lint` | **✅ auto syntax check** | ❌ | Catches errors |
| `cost/M tokens (in)` | **$0.29** | $15.00 | **50x cheaper** |
| `cost/M tokens (out)` | **$1.73** | $75.00 | **43x cheaper** |

## Why

Claude Code is good but closed. You pay Anthropic, you use their models, you can't see the source. We wanted something we could run with Qwen 3.6 at $0.29/M tokens, or a local Ollama model for free. So we built it.

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

| Provider | Default model | Fast model | Thinking | Key env var |
|----------|---------------|------------|----------|-------------|
| DeepSeek | deepseek-v4-pro | deepseek-v4-flash | yes | `DEEPSEEK_API_KEY` |
| Alibaba (Qwen) | qwen3.6-plus | qwen-turbo-latest | yes | `DASHSCOPE_API_KEY` |
| OpenRouter | qwen/qwen3.6-plus | — | yes | `OPENROUTER_API_KEY` |
| Mistral | mistral-large-latest | mistral-small-latest | no | `MISTRAL_API_KEY` |
| Ollama | (passed via `--model`) | — | depends on model | none |
| OpenAI-compat | (passed via `--model`) | — | depends | `OPENAI_API_KEY` |

Switch with `--provider`:

```bash
uppli-code --provider alibaba
uppli-code --provider ollama
uppli-code --provider deepseek
```

## Budget & cost

uppli-code **caps in tokens** and **reports cost in USD**. They are two independent signals:

- **Cap** — `--max-tokens-total <N>` aborts the session when cumulative tokens
  (input + output + cache creation + cache read) reach `N`. Objective,
  provider-reported, never drifts. Exit code `2`. In `--output-format json`
  or `stream-json` mode a `{"type":"budget_exceeded","tokens":N,"limit_tokens":N}`
  event is emitted on stderr.

- **Cost display** — best-effort USD from configured per-model pricing.
  Shown in the TUI status bar, `/cost`, `/status`, `/usage` slash commands,
  attached to `BridgeEvent::TurnComplete.usage` (web UI / SDK consumers),
  and stamped into each assistant `MessageCost.cost_usd` for session
  storage. **Not authoritative — see your provider dashboard for billing.**

Why decoupled: pricing drifts (promos, tariff changes), tokens don't. Earlier
versions used `--max-budget-usd <f64>` which fired at the wrong threshold whenever
pricing drifted. The flag is now removed (see CHANGELOG).

**Cap is evaluated between turns**, not mid-turn. A `--max-tokens-total 1000`
session that's mid-way through a turn consuming 50k tokens will complete that
turn first (the API call is already paid for; the model output is preserved)
and then abort. The stopping point is "first turn boundary ≥ N", not a hard
ceiling. Plan a margin if the cap is critical.

```bash
# Stop the session after 100k cumulative tokens
uppli-code --max-tokens-total 100000 --print "do the task"

# Combine with --effort: cap is independent of reasoning depth
uppli-code --max-tokens-total 50000 --effort max --print "deep task"
```

## Key features

### AstEdit — structural code editing

Unlike text-based Edit tools (used by Claude Code, Qwen Code), AstEdit operates on the Abstract Syntax Tree via [ast-grep](https://ast-grep.github.io/). It understands code structure and handles indentation automatically. No other CLI agent has this.

```
AstEdit(
  file: "file.py",
  pattern: "re.compile($ARG)",
  rewrite: "re.compile($ARG, re.IGNORECASE)"
)
```

### RAG-powered tool guidance

Local vector store (fastembed, 106 ast-grep patterns) helps the model choose the right pattern syntax before writing code. The model calls `AstGrepHelper` and gets relevant examples.

### CodeAudit — pre-fix structural analysis

7 analyzers run in parallel on a source file before the model touches it: AST patterns, consistency (outlier detection), control flow, data flow tracing, predicate logic (associativity, boundary conditions, completeness), symbol table, and semgrep community rules. The model gets a full picture of every structural anomaly so it fixes the root cause, not just the symptom.

### Patch — git-native diff application

Accepts standard unified diffs and applies them via `git apply` with 3-way merge fallback. Tolerant to whitespace and line offset. Multi-file patches in a single call. LLMs are trained on this format (millions of GitHub diffs), so they produce better patches than exact string replacements.

### Post-edit linting

Every file modification is syntax-checked immediately (5s timeout, auto language detection). Broken edits are caught before the model moves on. Unknown file types pass silently.

### ToolExpertise — intelligent tool selection

Knowledge base per tool: when to use it, when not to, tips, error recovery hints, alternatives. The model picks the right tool for the job instead of defaulting to Edit for everything.

### MCP Server (SuperAgent)

Run `uppli-code --mcp-server` to expose it as an MCP tool. Orchestrate from Claude Code, another uppli-code, or any MCP client. The SuperAgent pattern: a master agent pilots multiple workers.

```bash
uppli-code --mcp-server --provider alibaba
```

### Hybrid mode

When your provider has a reasoning model and a fast model, uppli-code switches between them automatically. Think model for planning, fast model for tool results.

### 42 tools

Read, Edit, AstEdit, Write, Bash, Grep, Glob, WebFetch, WebSearch, Agent, AstGrepHelper, TodoWrite, Notebook, MCP tools, and more.

## Architecture

```
src-rust/crates/
  cli/      — entry point, MCP server mode, onboarding
  api/      — LlmProvider trait, 5 providers, SSE streaming
  core/     — config, types, permissions, keychain, LSP
  query/    — agentic loop, tool dispatch, compaction
  tools/    — 42 tool implementations
  rag/      — local vector RAG (fastembed)
  tui/      — terminal UI (ratatui)
  mcp/      — MCP client
  bridge/   — remote control protocol
```

## Adding a provider

Three changes today:

1. Add a `ProviderType` variant in `crates/core/src/config.rs`
2. Add a `ProviderPreset` in `crates/api/src/provider_factory.rs` and a match arm in `default_model_for`
3. If the wire format is new, add a provider impl (most reuse `OpenAiProvider`)

The registry drives the CLI, onboarding, and model picker.

### Adding a model with pricing

Each entry in `known_models` carries optional pricing for the cost display:

```rust
ModelMetadata {
    id: "deepseek-v4-pro".to_string(),
    display_name: "DeepSeek V4 Pro".to_string(),
    description: "...".to_string(),
    context_window: 1_000_000,
    max_output_tokens: 384_000,
    supports_thinking: true,
    pricing: Some(ModelPricing {
        input_per_mtk: 0.435,          // USD per million input tokens
        output_per_mtk: 0.87,
        cache_creation_per_mtk: 0.0,
        cache_read_per_mtk: 0.003625,
    }),
}
```

Set `pricing: None` for local/free models (Ollama). When `None`, the cost line
is hidden across the UI and the bridge sends `cost_usd: null`. Pricing values
are best-effort; provider promos and tariff changes will drift them — they
don't affect the token-based budget cap.

(An upcoming PR S consolidates the per-provider hardcoded blocks into one
declarative TOML file per provider — see issue tracker.)

## Build

Rust 1.75+. Requires `sg` (ast-grep) for AstEdit: `brew install ast-grep`.

```bash
cd src-rust
cargo build --release
cargo test              # ~982 tests
cargo clippy            # 0 warnings
```

## Legal

This is a clean-room Rust reimplementation. No proprietary source code was copied. API protocols are not copyrightable (Oracle v. Google, US Supreme Court 2021).

## License

ELv2 (Elastic License v2). Free for non-competing use.
