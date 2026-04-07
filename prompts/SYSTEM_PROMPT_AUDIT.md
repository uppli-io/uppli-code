# System prompt audit — make Qwen3 match Claude Code quality

## Problem

Qwen3-235B scores higher than Opus on benchmarks but produces worse code via uppli-code than Claude Code. The model is good, the instructions are weak.

## What Claude Code does that we don't

1. System prompt is ~15K tokens with detailed behavioral instructions
2. Forces "read before edit" pattern
3. Forces "prefer Edit over Write" for existing files
4. Forces planning before action
5. Provides detailed tool usage guidelines per tool
6. Has agentic guardrails (retry on failure, verify after write)

## What we send

Our system_prompt.txt is 800 tokens. Generic "you help users with code".
The full system prompt from system_prompt.rs (ported from Claude Code) exists but may not be fully wired.

## Audit steps

1. Read crates/core/src/system_prompt.rs — what does build_system_prompt() actually produce?
2. Read crates/cli/src/main.rs — what gets sent to the API?
3. Compare with the Claude Code system prompt (leaked/documented)
4. Identify missing instructions that make the model:
   - Read before write
   - Use Edit instead of Write
   - Plan before acting
   - Verify after changes
   - Handle errors gracefully
5. Test with Qwen3 before and after adding instructions

## Hypothesis

The system_prompt.rs has all the right instructions (ported from Claude Code TS).
But our main.rs builds the prompt from system_prompt.txt (our 800 token version)
and may skip system_prompt.rs entirely. Need to verify the wiring.

## Expected outcome

Same model (Qwen3-235B), better instructions = better code output.
No Rust changes needed, just prompt engineering.
