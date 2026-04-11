# Uppli Code — Roadmap

## FAIT

### Fondations (3-5 avril)
- ✅ CLI uppli-code complet (42 tools, 982 tests)
- ✅ Extension VS Code (fork Claudix → Uppli Code)
- ✅ Branding complet (0 ref Claude Code)
- ✅ Mode hybride R2/V4 (reasoner → fast auto)
- ✅ 6 freins originaux levés

### Multi-provider + Sécurité (6-7 avril)
- ✅ Architecture multi-provider (5 providers, LlmProvider trait)
- ✅ OS Keychain (macOS, Linux)
- ✅ Onboarding TUI interactif
- ✅ Sécurité Anthropic : 9 modules supprimés, 0 URL claude.ai

### LSP + UX (6-7 avril)
- ✅ LSP auto-détection 22 language servers
- ✅ Session persistence JSONL + --resume
- ✅ Erreurs API humanisées

### Review + Fixes (9 avril)
- ✅ 3 CRITICAL + 5 HIGH + 10 MEDIUM + 7 LOW corrigés
- ✅ 0 warnings, cargo fmt clean

### MCP Server (10 avril)
- ✅ `--mcp-server` mode (SuperAgent via stdio)
- ✅ Sub-agents utilisent le provider global
- ✅ Streaming notifications temps réel

### AstEdit + RAG (10 avril)
- ✅ **AstEdit tool** — édition structurelle via ast-grep, indentation automatique
- ✅ **AstGrepHelper tool** — RAG vectoriel local (fastembed, 106 patterns)
- ✅ **Post-edit linting** — syntax check Python/Shell/JS/Ruby après chaque edit
- ✅ **Détection de boucle** — nudge quand le modèle répète la même action
- ✅ **Tool expertise DB** — descriptions riches, tips on_error, alternatives
- ✅ **System prompt neutre** — pas de restriction sur les outils, le modèle choisit

### Benchmark SWE-bench (10 avril)
- ✅ **20/20 issues astropy avec diff produit (100%)**
- ✅ Pipeline benchmark via MCP (script Python + uppli-code --mcp-server)
- ✅ Progression : 33% → 55% → 70% → 85% → 95% → **100% diff produit**

---

## BENCHMARK RESULTS

### SWE-bench Verified — 20 issues astropy (repo le plus dur)

| Metric | Score |
|--------|-------|
| **Diff produit** | **20/20 (100%)** |
| Modèle | Qwen 3.6 Plus (2026-04-02) |
| Coût moyen/issue | ~$0.02 |
| Temps moyen/issue | ~60s |
| Agent | uppli-code v0.1.0 |

### Progression

| Étape | Score | Changement clé |
|-------|-------|----------------|
| Baseline | 33% | Edit tool basique, mauvais modèle |
| + Fix modèle | 40% | qwen3.6-plus au lieu de qwen3-235b |
| + Edit description | 55% | "3 lines context before/after" |
| + Expertise DB + loop detection | 70% | Tips on_error, Grep mode content |
| + AstEdit (ast-grep) | 85% | Édition structurelle, indentation auto |
| + RAG vectoriel + AstGrepHelper | 95% | Pattern examples avant exécution |
| + Fix YAML → sg run | **100%** | Commande directe sans fichier temporaire |

### Comparaison (diff produit, pas tests validés)

| Agent | Modèle | Score officiel | Notre score (20 issues) | Coût/M tokens |
|-------|--------|---------------|------------------------|---------------|
| Claude Code | Opus 4.6 | 80.9% | — | $15.00 |
| Qwen Code | Qwen 3.6 Plus | 78.8% | — | $0.29 |
| **uppli-code** | **Qwen 3.6 Plus** | **à valider (500 issues)** | **100% diff** | **$0.29** |

> **Note** : notre score de 100% est sur 20 issues astropy avec vérification visuelle des diffs,
> pas avec le harness officiel SWE-bench (tests unitaires). Le score validé nécessite
> les 500 issues avec Docker + tests FAIL_TO_PASS/PASS_TO_PASS.

---

## CE QU'ON A ET QU'ILS N'ONT PAS

| Feature | uppli-code | Claude Code | OpenCode | Qwen Code |
|---------|-----------|-------------|----------|-----------|
| Multi-provider | ✅ 5 providers | ❌ Anthropic only | ❌ OpenAI only | ⚠️ Multi |
| MCP Server (SuperAgent) | ✅ | ❌ | ❌ | ❌ |
| **AstEdit (ast-grep)** | ✅ | ❌ | ❌ | ❌ |
| **RAG vectoriel pour tools** | ✅ | ❌ | ❌ | ❌ |
| Post-edit linting + revert | ✅ | ❌ | ❌ | ❌ |
| Hybrid think/fast auto | ✅ | ❌ | ❌ | ❌ |
| OS keychain natif | ✅ | ⚠️ OAuth | ❌ | ❌ |
| 42 tools | ✅ | ✅ 33 | ❌ 12 | ⚠️ ~20 |
| VS Code extension | ✅ | ✅ | ❌ | ❌ |
| Tool expertise database | ✅ | ❌ | ❌ | ❌ |
| Stuck-loop detection | ✅ | ❌ | ❌ | ❌ |

---

## A FAIRE

### P0 — Benchmark officiel
- [ ] Full SWE-bench Verified (500 issues) avec harness Docker
- [ ] Score validé avec tests FAIL_TO_PASS / PASS_TO_PASS
- [ ] Publication du score

### P1 — SuperAgent
- [ ] MCP bidirectionnel (permissions, inject prompts pendant query)
- [ ] Multi-worker (master pilote N uppli-code en parallèle)
- [ ] Docker worker pour CI/cloud

### P2 — TUI V2
- [ ] React/Ink TUI (remplace ratatui)
- [ ] Distribution npm + binary Rust
- [ ] Protocol bridge HTTP/SSE local

### P3 — Publication
- [ ] Git cleanup BFG
- [ ] README.md pro
- [ ] Homebrew tap
- [ ] GitHub Actions CI/CD

### P4 — Nice to have
- [ ] SQLite sessions
- [ ] OpenRouter provider (75+ modèles)
- [ ] RAG vectoriel avec fastembed pour tous les tools
- [ ] Fine-tuning Qwen sur ast-grep patterns (self-hosted)
