# Uppli Code — Roadmap

## FAIT

### Session 3-5 avril — Fondations
- ✅ CLI uppli-code complet (21/21 tests, tous les tools)
- ✅ Extension VS Code connectée (Claudix fork → Uppli Code for VS Code)
- ✅ Branding complet (0 ref Claude Code dans binaire et extension)
- ✅ Sécurité (clé API retirée, cargo fmt, revue code)
- ✅ Mode hybride R2/V4 (reasoner pense, chat traite les outils)
- ✅ Thinking désactivé pour deepseek-chat (pas de routing implicite vers R2)
- ✅ DeepSeek context window 128K, output 64K, thinking 32K par défaut
- ✅ 6 freins originaux levés (turns, compact, tokens, prompts, trafic, thinking)

### Session 5-6 avril — Bugs critiques + LOT 1 + LOT 2
- ✅ TUI "Claude Code" → "Uppli Code" (37 fichiers, 0 refs)
- ✅ SDK preset → string custom (pas de .claude injecté par le SDK npm)
- ✅ Chinois traduit (0 strings visibles dans l'UI)
- ✅ ESC/interrupt via control_request protocol
- ✅ Concurrent stdin reader (tokio select! pour events + control)
- ✅ TUI freeze après sleep → SIGCONT handler
- ✅ Session path encoding base64 (aligné avec SDK npm)
- ✅ Slash commands interceptées ($0) : /help /clear /cost /mcp /status /model /version /provider
- ✅ 20 slash commands exposées dans get_claude_state
- ✅ set_model live via control_request + set_max_thinking_tokens toggle
- ✅ API key resolution (env var prioritaire, filter invalides)
- ✅ cargo fmt sur tout le codebase

### Session 6-7 avril — Multi-provider + Sécurité
- ✅ **Architecture multi-provider** (LlmProvider trait auto-descriptif)
  - DeepSeek (Anthropic SSE), Alibaba/Qwen3 (OpenAI SSE), Ollama (NDJSON), Mistral, Generic
  - Chaque provider déclare ses modèles, limites, pricing, auth, attribution
  - Ajouter un provider = 1 preset + 1 match arm, zéro changement ailleurs
- ✅ **Provider factory** avec résolution de clé sécurisée (env var → keychain → config)
- ✅ **OS Keychain** (macOS Keychain, Linux libsecret) — clés jamais en clair dans JSON
- ✅ **Onboarding interactif** : choix provider → clé API → sélection modèle → fast model
- ✅ **System prompt dynamique** par provider ("powered by Qwen3", "powered by DeepSeek")
- ✅ **context_window consolidé** (3 duplicatas supprimés → provider.context_window(model))
- ✅ **Cost tracking provider-driven** (pricing depuis ModelMetadata, plus de hardcoding Claude)
- ✅ **QueryConfig::from_provider()** (modèle, limites, thinking depuis capabilities)
- ✅ **Sécurité Anthropic phone-home** : 9 modules supprimés, bridge désactivé, OAuth neutralisé, 0 URL claude.ai/anthropic.com
- ✅ **Testé en production** : DeepSeek + Qwen3-235B (Singapour, thinking + tool use)

## EN COURS

### LOT 3 — Extension VS Code sync (~1 jour)

#### 3.1 canUseTool callback (permission dialogs)
- Le CLI émet control_request can_use_tool avant d'exécuter un outil
- L'extension affiche un dialog Allow/Deny
- L'utilisateur choisit → control_response → CLI exécute ou refuse
- **Complexité**: Haute — communication bidirectionnelle synchrone

#### 3.2 MCP servers status live
- ✅ Lire .mcp.json et ~/.uppli/settings.json au démarrage
- ✅ handleGetMcpServers lit les fichiers config

#### 3.3 Skills listing
- ✅ Scanner ~/.uppli/commands/*.md au démarrage

#### 3.4 Model picker TUI provider-agnostic (en cours — agent externe)
- Supprimer les modèles Claude hardcodés du picker
- Lire depuis provider.capabilities().known_models
- Fast model depuis provider.capabilities().fast_model

### LOT 4 — Polish / Publication (~1 jour)

#### 4.1 Traduction commentaires chinois (25K lignes)
- Script Python batch — invisible utilisateur mais critique pour contributeurs

#### 4.2 Logo/icône
- SVG pour sidebar VS Code + ASCII art TUI

#### 4.3 Documentation
- README.md professionnel avec screenshots
- Comparatif coûts : DeepSeek ($0.32/jour) vs Claude ($5.85/jour) vs Qwen3 (à mesurer)
- Guide installation multi-provider
- ARCHITECTURE.md

#### 4.4 Git cleanup avant publication
- BFG Repo-Cleaner pour supprimer les clés de l'historique
- Squash commits de debug

### Étapes restantes architecture provider
- [ ] Onboarding depuis registre providers (plus de match hardcodé)
- [ ] Nettoyage constantes deprecated (DEFAULT_MODEL, SONNET_MODEL, ANTHROPIC_API_BASE)

## FEATURES FUTURES

### Voice (STT + TTS)
- STT: Faster-Whisper (modèle small, local)
- TTS: Kokoro 82M (local)
- Toggle micro dans l'extension

### Agent Cloud Autonome
- Mode fire-and-forget avec Gemma 4 local ($0)
- Dashboard web pour suivi des tâches

### Publication
- VS Code Marketplace
- npm: `npm install -g uppli-code`
- Homebrew: `brew install uppli-code`
- Binaires pré-compilés (macOS ARM/Intel, Linux, Windows)
- GitHub Actions CI/CD

### UX Améliorations
- Meilleure gestion erreurs API (humaniser les JSON 401/404)
- Windows keychain (wincred)
- /provider switch live (sans redémarrer)
- Auto-détection Ollama local
