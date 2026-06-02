# uppli-code

Open source coding agent. Reads every common file format (text, source code, CSV, JSON, PDF, XLSX, DOCX, ODF, ZIP, images, …). Works with DeepSeek, GLM (z.ai / Zhipu), Qwen3, Ollama, Mistral, OpenAI, or any OpenAI-compatible endpoint. Written in Rust.

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
| `thinking_budget` | **64,000 (--effort max)** | 128,000 | Tied to `--effort` (Low 8k → Max 64k) |
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

16MB binary. Starts in 50ms. Full agentic loop with tool use, file editing, bash execution, thinking mode, multi-turn context, **and a Read tool that handles 30+ file formats out of the box**.

## Quick start

```bash
git clone https://github.com/uppli-io/uppli-code.git
cd uppli-code/src-rust
cargo build --release
./target/release/uppli-code
```

First time you launch it, it asks you to pick a provider and enter your API key. The key goes in your OS keychain (macOS Keychain / Linux libsecret), not in a config file.

## Providers

uppli-code is **provider-agnostic**: pick one at startup, the CLI never branches on provider name. Each provider's capabilities (default model, pricing, vision support) live in a single declarative TOML file under `crates/api/presets/`.

| Provider | Default model | Vision | Thinking | Key env var |
|----------|---------------|--------|----------|-------------|
| DeepSeek | deepseek-v4-pro | no | yes | `DEEPSEEK_API_KEY` |
| Zhipu / GLM (z.ai) | glm-4.5v | **yes** | no | `ZHIPU_API_KEY` |
| Alibaba (Qwen) | qwen3.6-plus | no | yes | `DASHSCOPE_API_KEY` |
| OpenRouter | qwen/qwen3.6-plus | no | yes | `OPENROUTER_API_KEY` |
| Mistral | mistral-large-latest | no | no | `MISTRAL_API_KEY` |
| OpenAI | (via `--model`) | depends | depends | `OPENAI_API_KEY` |
| Ollama | (via `--model`) | depends | depends | none (local) |

**One provider per session.** Pick at startup, no switching mid-flight.

```bash
uppli-code --provider deepseek          # cheap, text-only
uppli-code --provider glm               # vision-capable (images + visual PDFs)
uppli-code --provider ollama --model llama3.1
```

### Vision behaviour

When the active provider's model can't see (DeepSeek, Mistral, Qwen-text, …) and a tool produces an image, the tool result is rewritten as a clear refusal so the model knows it failed:

> `[ERROR: image cannot be read — the current provider's model does not support vision. Restart uppli-code with a vision-capable provider (e.g. --provider glm) to process this file.]`

No silent fallback. The model relays the error to the user, who relaunches with the right provider. PDFs are a special case: text extract goes through to every provider (via `pdf-extract`), but the raw bytes are only forwarded when the model supports vision.

### File formats the Read tool handles

| Family | Formats | What you get |
|---|---|---|
| **Source / text** | `.txt .log .py .rs .js .ts .go .java .c .cpp .sh .sql .toml .yaml …` | line-numbered text, streaming cap at 10 MiB, Windows-1252 fallback for non-UTF-8 |
| **Structured text** | `.csv .tsv .json .jsonl .xml .html .md .ipynb` | text + validation banner (JSON parse, JSONL line check, HTML script-strip, notebook cell list) |
| **Image** | `.png .jpg .gif .webp .bmp` | base64 Image block on vision providers, caption-only error on the rest. Decompression-bomb resistant. |
| **PDF** | `.pdf` | text extraction (triple-shielded: spawn_blocking + timeout + catch_unwind) + Document block for vision. `pages: "1-5,7"` selector. |
| **Office (modern)** | `.xlsx .docx .pptx` | direct ZIP+XML parse (no calamine/docx-rs). XLSX rows, DOCX paragraphs, PPTX slide text. XML hardening (DOCTYPE refused, depth cap). |
| **OpenDocument** | `.odt .ods .odp` | same code path via `content.xml`. |
| **Legacy Office** | `.xls .doc .ppt` | graceful stub with `libreoffice --headless --convert-to xlsx` recipe. Not parsed (OLE2 readers are panic-prone). |
| **Archives** | `.zip .jar .war .ear .apk .ipa .epub .tar .tar.gz` | textual manifest (`<path>\t<size>\t<sha256>\t<inferred>`). Never extracts to disk. Zip-bomb / path-traversal / symlink refusal. |
| **Exotic archives** | `.tar.bz2 .tar.xz .tar.zst .7z .rar` | success stub with Bash recipe. C-toolchain deps deferred. |

Every read is gated by a 100 MiB pre-flight size cap. Magic bytes win over extension (a `.txt` that's actually a PDF gets a `[detected as PDF via magic bytes]` note).

## Budget & cost

uppli-code **caps in tokens** and **caps in USD** — independently or together.

- **Cap (tokens)** — `--max-tokens-total <N>` aborts the session when cumulative tokens (input + output + cache creation + cache read) reach `N`. Objective, provider-reported, never drifts. Exit code `2`.
- **Cap (USD)** — `--max-budget-usd <N>` aborts when estimated USD spend reaches `N`. Best-effort from configured pricing (drifts with provider promos). Use this for refacturation: catalog enrichment billed per-run to a client needs a € cap.
- **Both caps together** — set both; whichever fires first triggers the abort. The JSON event on stderr indicates which cap fired: `{"type":"budget_exceeded","trigger":"tokens"|"usd","spent_tokens":...,"spent_cost_usd":...,"limit_tokens":...,"limit_cost_usd":...}`.
- **Cost display** — best-effort USD from configured per-model pricing. Shown in the TUI status bar, `/cost`, `/status`, `/usage` slash commands, attached to `BridgeEvent::TurnComplete.usage`, stamped into each assistant message's `MessageCost.cost_usd`. **Not authoritative — see your provider dashboard for billing.**

**Cap is evaluated between turns**, not mid-turn. A `--max-tokens-total 1000` session mid-way through a turn consuming 50k tokens completes that turn first (the API call is already paid for; the model output is preserved) and then aborts. The stopping point is "first turn boundary ≥ N", not a hard ceiling.

```bash
# Stop after 100k cumulative tokens
uppli-code --max-tokens-total 100000 --print "do the task"

# Stop at $5 estimated spend (refacturation use case)
uppli-code --max-budget-usd 5.00 --print "do the task"

# Combine — whichever fires first wins
uppli-code --max-tokens-total 100000 --max-budget-usd 5.00 --print "do the task"

# Caps are independent of reasoning depth
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

### Multi-format file reading

Read any common file kind through a single tool. Magic-byte sniffing wins over extension. Decompression-bomb resistant. PDF triple-shielded (timeout + spawn_blocking + catch_unwind). XML hardening on every OOXML / ODF / SVG. Hard 100 MiB pre-flight cap. Detailed format coverage in the [Providers / File formats](#file-formats-the-read-tool-handles) section.

### MCP Server (SuperAgent)

Run `uppli-code --mcp-server` to expose it as an MCP tool. Orchestrate from Claude Code, another uppli-code, or any MCP client. The SuperAgent pattern: a master agent pilots multiple workers.

```bash
uppli-code --mcp-server --provider alibaba
```

### Hybrid mode

When your provider has a reasoning model and a fast model, uppli-code switches between them automatically. Think model for planning, fast model for tool results. Disabled for vision-capable providers (switching to a non-vision model mid-session would break tool results that contain images).

### 42 tools

Read, Edit, AstEdit, Write, Bash, Grep, Glob, WebFetch, WebSearch, Agent, AstGrepHelper, TodoWrite, Notebook, MCP tools, and more.

## Architecture

```
src-rust/crates/
  cli/      — entry point, MCP server mode, onboarding
  api/      — LlmProvider trait, 7 providers (TOML-declared), SSE streaming
  core/     — config, types, permissions, keychain, LSP
  query/    — agentic loop, tool dispatch, compaction
  tools/    — 42 tool implementations (file_read split into 13 sub-modules)
  rag/      — local vector RAG (fastembed)
  tui/      — terminal UI (ratatui)
  mcp/      — MCP client
  bridge/   — remote control protocol
```

### Provider-agnostic by design

- The CLI never inspects provider capabilities. It always emits the richest representation it has (text + images + documents).
- Each provider's translation layer adapts for its actual wire format. If the backing model can't read a block kind, the provider replaces it with an EXPLICIT error in the tool_result (`is_error: true`) — never a silent fallback.
- One flag per fact. The only provider capability flag is `supports_vision`. Adding `supports_audio` / `supports_video` / etc. would be parameter proliferation — formats the active model can't handle are surfaced as errors, not configured.

## Adding a provider

**Drop a TOML file.** One line elsewhere. That's it.

1. Create `crates/api/presets/myprovider.toml`:

```toml
schema_version = 1

[provider]
name = "myprovider"
aliases = ["mp"]
display_name = "My Provider"
description = "..."
attribution = "powered by My Provider"
provider_type = "openai_compat"   # or "deepseek" for Anthropic-format wire
api_format = "openai"             # or "anthropic"
api_base = "https://api.myprovider.com/v1"
supports_vision = false           # only flag — true if the default model accepts image blocks

[auth]
env_vars = ["MYPROVIDER_API_KEY"]
keychain_key = "myprovider"
display_label = "My Provider"

[defaults]
max_tokens = 8192
request_timeout_sec = 600
max_retries = 5

[[models]]
id = "my-default-model"
display_name = "My Default Model"
description = "Flagship"
context_window = 128000
max_output_tokens = 8192
supports_thinking = false
default = true

[models.pricing]
input_per_mtk = 0.50
output_per_mtk = 1.50
```

2. Add one line in `crates/api/src/providers/loader.rs::BUNDLED_TOMLS`:

```rust
("myprovider", include_str!("../../presets/myprovider.toml")),
```

3. Rebuild. The CLI, onboarding flow, and model picker pick it up automatically.

If your endpoint's URL doesn't follow the OpenAI `<base>/v1/chat/completions` convention (e.g. z.ai uses `<base>/v4/chat/completions`), include the version segment directly in `api_base` — uppli-code detects trailing `/vN` and skips the redundant `/v1/` prefix.

**Pricing is optional.** Local / free models (Ollama) set `pricing: None` per model. When `None`, the cost line is hidden in the TUI, slash commands, and bridge events. Pricing drifts with provider promos — it doesn't affect the token-based budget cap.

## Build

Rust 1.75+. Requires `sg` (ast-grep) for AstEdit: `brew install ast-grep`.

```bash
cd src-rust
cargo build --release
cargo test              # 1000+ tests
cargo clippy --workspace --all-targets -- -D warnings    # 0 warnings
```

## Legal

This is a clean-room Rust reimplementation. No proprietary source code was copied. API protocols are not copyrightable (Oracle v. Google, US Supreme Court 2021).

## License

ELv2 (Elastic License v2). Free for non-competing use.
