# System prompt rewrite

## What

The system prompt in `crates/core/src/system_prompt.rs` has 7 short constants totaling ~900 tokens. The real Claude Code prompt is ~15000 tokens. The port summarized everything in 5 lines per section. Qwen3 needs the full instructions to produce quality code.

## Where

File: `src-rust/crates/core/src/system_prompt.rs`

Constants to rewrite:
- `CORE_CAPABILITIES` (line 423) — currently 8 lines
- `TOOL_USE_GUIDELINES` (line 444) — currently 5 lines
- `ACTIONS_SECTION` (line 454) — currently 4 lines
- `SAFETY_GUIDELINES` (line 462) — currently 3 lines
- `CYBER_RISK_INSTRUCTION` (line 472) — fine as is
- `COORDINATOR_SYSTEM_PROMPT` (line 481) — fine as is

Also: `src-rust/crates/cli/src/system_prompt.txt` — the custom_instructions block. Remove duplication with the constants.

## Source

The real Claude Code system prompt is the one injected by Claude Code itself. You can see it at the top of any Claude Code conversation. Key sections to port:

1. **Doing tasks** — read before edit, prefer Edit over Write, don't add features beyond what was asked, security, no unnecessary abstractions
2. **Using your tools** — when to use each tool, when NOT to use bash for file ops, how to search
3. **Executing actions with care** — reversibility, blast radius, confirm destructive ops
4. **Committing changes with git** — step by step git workflow
5. **Creating pull requests** — PR format
6. **Tone and style** — concise, no emojis, file references format
7. **Output efficiency** — go straight to the point, no filler

## How

1. Read the system prompt at the top of this Claude Code conversation (it's in the system-reminder)
2. Extract the key sections
3. Adapt for provider-agnostic (replace "Claude" with generic terms)
4. Write into the Rust constants
5. Remove duplication from system_prompt.txt
6. Test: same task with old vs new prompt, compare quality

## After

The dump at /tmp/uppli-system-prompt-dump.txt should go from ~5000 chars to ~30000+ chars. The model gets real instructions instead of summaries.

Remove the debug dump code in query/lib.rs after testing.
