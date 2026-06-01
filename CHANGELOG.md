# Changelog

All notable changes to uppli-code are documented in this file.

## Unreleased

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
