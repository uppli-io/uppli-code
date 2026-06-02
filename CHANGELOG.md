# Changelog

All notable changes to uppli-code are documented in this file.

## Unreleased

### Architecture: AnthropicClient reads caps from deepseek.toml (PR C, full DRY)

- **AnthropicClient no longer hardcodes DeepSeek's capabilities.** Previously the same `ProviderCapabilities` (model list, pricing, supports_vision, default_model, etc.) lived in BOTH `crates/api/presets/deepseek.toml` AND a `OnceLock` inside `AnthropicClient::capabilities()`. Editing one without the other silently lied to the runtime — guarded only by a workspace consistency test.
- **Now there is one source of truth: the TOML.** `AnthropicClient::new(cfg, caps)` accepts a `ProviderCapabilities` constructed by the loader. The factory branch for DeepSeek (`provider_factory::create_deepseek_provider`) passes `loaded.capabilities.clone()` so the runtime caps come directly from `deepseek.toml`. The `from_config` convenience constructor does the same lookup automatically.
- **Removed**: the OnceLock in `lib.rs::client::AnthropicClient::capabilities()` and the `⚠ ADVISORY ONLY` block in `deepseek.toml`. The `test_deepseek_toml_caps_match_static_anthropic_client_caps` test was deleted (it would assert TOML == TOML, which is vacuous).
- The provider construction path is now homogeneous across all 7 providers — each one reads its caps from its TOML preset and there is no second declaration anywhere.

### Architecture: CLI provider-agnostic + provider rejects, never degrades silently (PR C)

User's directive: **"CLI agnostique du provider, et si le provider ne sait pas faire un truc ça retourne une erreur, ça évite 40 millions de paramètres."** Translated into code:

- **The CLI no longer consults provider capabilities to decide tool_result shape.** Previously `cc-query::run_query_loop` checked `ProviderCapabilities` to choose between `ToolResultContent::Blocks` and `Text`. Now the CLI always emits the richest representation when a tool returned structured blocks; the provider's translation layer is responsible for adapting.
- **Provider rejects rather than silently caps**. When the active model doesn't support a block kind (Image / Document on DeepSeek today), the provider rewrites the surrounding `tool_result` with an EXPLICIT error: `is_error: true` and a single text block reading `[ERROR: image "foo.png" (image/png) cannot be read — the current provider does not support vision. Switch to a vision-capable provider (e.g. --provider glm) or convert the file to text out-of-band.]`. The model never receives a caption it might mistake for the real content.
- **One flag, one fact**: `supports_tool_result_blocks` is REMOVED across the entire codebase (ProviderCapabilities, ProviderToml schema, loader, OpenAiProviderConfig, AnthropicClient static caps, all 7 TOML presets, model_picker test fixture, consistency test). The flag had become dead weight after PR A's dispatch removal — keeping it would have invited "40 millions de paramètres" drift. The sole remaining provider capability flag is `supports_vision`.
- `AnthropicClient::degrade_blocks_if_needed` is the provider-side rejection gate for DeepSeek's Anthropic wire. Top-level Image/Document blocks in user messages become error text inline; visual blocks nested inside a `tool_result` flip the wrapper to `is_error=true` and compose surviving captions + the error message into one text payload.
- `OpenAiProvider::translate_message` already did the equivalent dispatch via `self.capabilities.supports_vision`. Symmetric across both wires; no code change needed there.
- **Removed**: `pub fn blocks_carry_visual_payload` from `cc-query` (no in-tree callers). Reverts the dispatch logic from PR A commit "feat(query): dispatch ToolResult.blocks based on provider capabilities".
- Doc rewrite across `crates/tools/src/{lib.rs, file_read/output.rs, file_read/caption.rs}` to reflect the new contract: the query loop forwards blocks verbatim; the provider decides what to do.
- 8 unit tests + 1 end-to-end test pin the rejection path (top-level Image → error, top-level Document → error, mixed text+image, tool_result flips is_error, text-only tool_result stays intact, plain-string no-op, url-source includes URL in error, document title preserved in error, end-to-end via real `CreateMessageRequest`).

### Multimodal file ingestion (PR B)

- **`Read` tool now handles every file kind a user can paste at the agent.** Previous behaviour: only text via `read_to_string` (hard-errored on non-UTF-8), with placeholder strings for images and PDFs. New behaviour: per-format dispatch through magic-byte sniffing.
  - **Text / source code**: streaming cap at 10 MiB, lossy Windows-1252 fallback for non-UTF-8 input (previously hard-errored "appears to be binary"), per-line truncation at 16 384 chars. The legacy `<num>\t<line>\n` format is preserved byte-for-byte.
  - **Images (PNG / JPEG / GIF / WebP / BMP)**: emitted as `ContentBlock::Image` (base64) on vision-capable providers, with a textual caption for the rest. Decompression-bomb resistant via a header-only dimension probe before any pixel decode. ICO returns a stub; SVG routes to the text path.
  - **PDF**: text extraction via `pdf-extract` triple-shielded (`spawn_blocking` + `tokio::time::timeout(30s)` + `catch_unwind`). PDFs ≤ 5 MiB also get a `ContentBlock::Document` payload for vision providers. New `pages: Option<String>` parameter supports range selection like `"1-5,7,9-10"` — closes the dead-advice bug where the legacy description promoted this parameter without declaring it in the schema.
  - **OOXML (XLSX / DOCX / PPTX)**: zip + quick-xml extraction. XLSX returns the first sheet's rows (cap 500) with `\t`-joined values; DOCX returns paragraph text; PPTX returns per-slide text (cap 20 slides). XML hardening: `<!DOCTYPE>` rejected, depth cap 128, path-traversal guards on slide enumeration.
  - **ODF (ODT / ODS / ODP)**: same code path as OOXML via the `content.xml` payload.
  - **Legacy XLS / DOC / PPT**: graceful stub with a `libreoffice --headless --convert-to xlsx ...` recipe. `is_error = false` so the model can route around the gap.
  - **Archives**: ZIP / JAR / WAR / EAR / APK / IPA / EPUB and TAR / TAR.GZ produce a textual manifest (`<path>\t<size>\t<sha256_first8>\t<inferred_format>`). NEVER extracted to disk. Compression-ratio + cumulative-decompressed-bytes guards against bombs. Encrypted ZIPs and TAR symlinks / `..` traversal entries are refused.
  - **`tar.bz2 / tar.xz / tar.zst / 7z / rar`**: graceful stubs with Bash recipes. The bz2 / xz / zstd / 7z C-toolchain deps are deferred to a follow-up PR.
  - **Pre-flight cap**: every read goes through a `fs::metadata().len() > MAX_FILE_BYTES (100 MiB)` guard before any byte is read — closes the OOM exposure on multi-GB files.
  - **Sniff-vs-extension disagreement note**: when magic bytes pick a binary kind the filename didn't predict (e.g. a `.txt` that is actually a PDF), the tool prepends `[detected as <KIND> via magic bytes; extension said <KIND>]` to the output.

### Breaking changes

- **`QueryOutcome::BudgetExceeded` shape changed.** Was `{ tokens, limit_tokens }`, now `{ spent_tokens, spent_cost_usd, limit_tokens, limit_cost_usd, trigger }`. The JSON event for `--output-format json` / `stream-json` carries the same keys + a `trigger: "tokens" | "usd"` field that says which cap fired.

### Flag changes

- **`--max-tokens-total <u64>`** — abort the session when cumulative tokens reach N. Objective cap, never drifts. New in this release.
- **`--max-budget-usd <f64>`** — abort the session when estimated USD cost reaches N (best-effort from configured pricing). Restored after the v1 removal: refacturation use cases (catalog enrichment with per-run cost billed to a client) need a € cap. Both caps may be set simultaneously; whichever fires first triggers `BudgetExceeded`. The event carries both `spent_tokens` and `spent_cost_usd` plus a `trigger` field.
  - Why we kept both: tokens are objective (cap fires deterministically), USD is the refacturation unit (best-effort but matches the billing contract). Decoupled by design.

- **`QueryOutcome::BudgetExceeded` field rename.** Was `{ cost_usd, limit_usd }`, now `{ tokens, limit_tokens }`. JSON output schema for `--output-format json`/`stream-json` emits `{"type": "budget_exceeded", "tokens": N, "limit_tokens": N}` on stderr with exit code 2.

### Bug fixes

- The budget guard previously aborted **before** appending the assistant's last response to the conversation, losing the model's final message on the turn that triggered the cap. Now the message is persisted first, then the cap is evaluated — consumers see the response the API already paid for.

### Internals

- `CostTracker` keeps `total_cost_usd()` (derived from per-model pricing) alongside `total_tokens()`. Pricing is seeded from `LlmProvider::model_pricing()` at session start.
- `ModelMetadata.pricing: Option<ModelPricing>` is back on every model. `None` means free/unknown (Ollama, generic OpenAI-compat endpoints).
- Assistant messages now stamp their per-turn `cost_usd` into `MessageCost`. **Note:** per-turn cost is computed as `total_cost_usd() - previous_total`, which is correct because pricing is seeded once at startup. If a future change calls `CostTracker::set_pricing` mid-session (e.g. provider switch), per-turn deltas would re-baseline incorrectly — to be revisited then.
- `BridgeEvent::TurnComplete` payload includes `usage: Option<BridgeUsage>` with `{input_tokens, output_tokens, cost_usd}` for the web UI / SDK consumers.
- JSON output (`--output-format json` / `stream-json`) `result` records carry both `total_tokens` (u64) and `total_cost_usd` (f64). Schema-locked in `cc-query` tests so downstream consumers (refacturation pipelines) don't break silently on a rename.
- TUI status bar live shows `${cost:.4}` when pricing is configured, blank when unknown (Ollama).
- Slash commands `/cost`, `/status`, `/usage`, `/stats`, `/insights`, `/extra-usage` all surface cost consistently when available.

### Known limitations

- **Asymmetric loss on budget hit during a `tool_use` turn.** If the cap fires
  on a turn whose `stop_reason == "tool_use"`, the assistant message (model's
  reasoning text + tool_use blocks) is intentionally NOT persisted, because
  pushing a `tool_use` without its `tool_result` would create an orphan the
  Anthropic API rejects on resume/replay (see fix for review finding #3).
  The trade-off is that the final pre-cap reasoning is lost from session
  history — symmetric to bug #4's fix preserving text on `end_turn`, but
  inverted for `tool_use`. There is no clean solution without synthesising
  a fake `tool_result`, which we explicitly chose not to do.

- **JSON `result` record carries both `tokens` and `total_tokens` keys with
  the same value.** `tokens` is the v1 key, `total_tokens` is the v2 key
  matching the format of other emissions. Both are kept for one release
  cycle to ease consumer migration. `tokens` will be removed in a future
  release; consumers should migrate to `total_tokens`.
