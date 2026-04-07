# Next session plan

4 problems to fix, ordered by user impact.

## 1. TUI bugs (bloquant)

The TUI has never been validated visually with Qwen 3.6-Plus. The model name, onboarding dialog, Enter key, and error display need manual testing then fixing.

Files: crates/cli/src/main.rs (run_interactive), crates/tui/src/app.rs, crates/tui/src/render.rs

Steps:
- Launch uppli-code in a terminal
- Check model name in status bar (should say qwen3.6-plus, not deepseek-reasoner)
- Type "salut" and press Enter — should get a short response
- Type /provider — onboarding dialog should open
- Check /model — model picker should show Qwen models
- Check /help, /cost, /status
- Type a coding request during streaming — should not block input
- Press ESC during tool execution — should cancel
- Trigger an API error — should show human message not JSON

## 2. Session storage (high)

The session_storage.rs module exists but is not wired in main.rs. No JSONL files are created. --resume does not work.

Files: crates/core/src/session_storage.rs, crates/cli/src/main.rs

Steps:
- Read session_storage.rs to understand the API (TranscriptEntry, append_entry, etc.)
- In run_interactive and run_headless, after each query turn, call append_entry
- On --resume flag, load the session from JSONL and prepopulate messages Vec
- Test: run a session, exit, --resume, check context is preserved

## 3. Auto-generate UPPLI.md (medium)

When uppli-code opens in a project without UPPLI.md, it should scan and create one so the model knows the tech stack in one-shot mode.

Files: crates/cli/src/main.rs, crates/core/src/lib.rs (ContextBuilder)

Steps:
- In ContextBuilder::build_system_context, if no UPPLI.md exists, run Glob on the cwd to list key files (package.json, Cargo.toml, requirements.txt, go.mod, etc.)
- Build a short description: "Python FastAPI project with SQLite" from the file names
- Either inject directly into context (ephemeral) or create UPPLI.md on disk (persistent)
- Ephemeral is safer for v1 (don't write to user's project without asking)

## 4. API error humanization (low)

API errors (401, 404, 429, etc.) show raw JSON in the TUI status bar. Should parse and show a human message.

Files: crates/api/src/openai_provider.rs, crates/query/src/lib.rs

Steps:
- In the provider's error path, parse the JSON error body
- Extract the "message" field from {"error": {"message": "..."}}
- Return a ClaudeError with the human message, not the raw JSON
- In the TUI, errors already go to status_message — just make the message cleaner
