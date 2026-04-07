# Feature parity with OpenCode

On a ✅ / On a pas ❌ / Partiel ⚠️

## TUI
- ✅ TUI ratatui (alt-screen)
- ✅ Markdown rendering dans le chat
- ✅ Help overlay (/help, ?)
- ✅ Model picker (/model, flèches)
- ✅ Permission dialog (y/Y/n)
- ✅ Session info dans la status bar
- ✅ Cancel generation (ESC, Ctrl+C)
- ⚠️ Diff viewer (existe mais pas testé avec Qwen)
- ⚠️ Syntax highlighting (partiel)
- ❌ Vim-like editor (vim mode existe mais pas complet)
- ❌ External editor ($EDITOR avec Ctrl+E)
- ❌ Sidebar file changes (sidebar existe mais pas connectée)
- ❌ Command palette (Ctrl+K)
- ❌ File picker / attachment
- ❌ Autocomplete paths dans l'input
- ❌ Image preview ANSI
- ❌ Logs page (Ctrl+L)
- ❌ 9 themes (on a 1 theme)

## Providers
- ✅ DeepSeek (via Anthropic-compat)
- ✅ Alibaba/Qwen (OpenAI-compat)
- ✅ Ollama (NDJSON)
- ✅ Mistral (OpenAI-compat)
- ✅ OpenAI-compat (generic)
- ❌ OpenAI natif
- ❌ Anthropic natif (Claude)
- ❌ Google Gemini
- ❌ GitHub Copilot
- ❌ AWS Bedrock
- ❌ Azure OpenAI
- ❌ Google VertexAI
- ❌ Groq
- ❌ OpenRouter
- ❌ xAI
- ❌ Auto-detect provider from env vars

## Tools
- ✅ Bash
- ✅ Read (view)
- ✅ Write
- ✅ Edit
- ✅ Glob
- ✅ Grep
- ✅ WebFetch
- ✅ WebSearch
- ✅ Agent (sub-agent)
- ✅ TodoWrite
- ✅ Tasks (create/get/update/list)
- ✅ NotebookEdit
- ✅ EnterPlanMode / ExitPlanMode
- ✅ EnterWorktree / ExitWorktree
- ✅ Cron (create/delete/list)
- ✅ Config
- ✅ Skill
- ✅ MCP tools
- ❌ ls (on utilise Glob/Bash)
- ❌ patch (multi-file unified patch en une op)
- ❌ diagnostics (LSP errors)
- ❌ sourcegraph (code search public)

## LSP
- ❌ Pas du tout intégré
- Le module lsp.rs existe dans core mais pas branché
- Impact : le modèle ne voit pas les erreurs de type en temps réel
- Effort : 2-3 semaines (lancer les language servers, parser les diagnostics, injecter dans le contexte)

## Sessions
- ⚠️ JSONL (vient d'être branché, headless + TUI)
- ❌ SQLite (OpenCode utilise SQLite, nous JSONL)
- ❌ Session switcher (Ctrl+A)
- ❌ Auto-generated titles
- ❌ Delete sessions
- ❌ Session search
- ⚠️ /resume (le module existe, pas testé)

## Context Management
- ✅ Auto-compact (95% du context window)
- ✅ Micro-compact
- ✅ Reactive compact
- ✅ Context collapse
- ✅ Snip compact
- ✅ UPPLI.md (mémoire projet)
- ⚠️ Reads .cursorrules, CLAUDE.md, etc. (code existe)

## Permissions
- ✅ Allow once
- ✅ Allow for session
- ✅ Deny
- ✅ Bypass mode (--dangerously-skip-permissions)
- ⚠️ Persistent permissions (code existe, bug corrigé)

## Custom Commands
- ✅ /init, /help, /clear, /cost, /model, /status, /version, /provider
- ✅ Skills (.uppli/commands/*.md)
- ❌ Command palette
- ❌ Named arguments dans les commands

## Config
- ✅ settings.json
- ✅ Provider config
- ✅ OS keychain
- ❌ JSON schema pour validation
- ❌ Auto-detect provider from API keys
- ❌ Shell config (custom shell path)

## Distribution
- ✅ Build from source (cargo build)
- ✅ GitHub Actions CI
- ❌ Homebrew tap
- ❌ Install script (curl)
- ❌ npm package
- ❌ AUR package
- ❌ Pre-built binaries

## Architecture
- ✅ Event-driven (tokio channels)
- ✅ Graceful shutdown (cancel tokens)
- ❌ Client/server (pas de séparation)
- ❌ Multi-session parallèle avec coordination

## Ce qu'on a et pas OpenCode
- ✅ Hybrid model switching (reasoner → fast auto)
- ✅ 5 niveaux de compaction
- ✅ Thinking budgets (4 niveaux)
- ✅ OS keychain (macOS + Linux)
- ✅ 33 tools (vs 12)
- ✅ VS Code extension (Claudix)
- ✅ Onboarding TUI interactif
- ✅ Provider registry (ajouter un provider = 1 preset)

## Priorités pour écraser OpenCode

### P0 — Bloquant
1. TUI qui marche (le test visuel de Sayah)
2. Session /resume fonctionnel

### P1 — Différenciant
3. LSP integration (diagnostics → le modèle voit les erreurs)
4. Auto-detect provider from API keys in env
5. Homebrew + install script

### P2 — Parité
6. Session switcher (Ctrl+A)
7. Auto-generated session titles
8. 3-4 themes additionnels
9. External editor (Ctrl+E)
10. patch tool (multi-file)
11. OpenRouter comme provider (accès à 75+ modèles d'un coup)

### P3 — Nice to have
12. Sourcegraph code search
13. Image attachment + preview
14. SQLite sessions
15. Command palette
16. Sidebar file changes
