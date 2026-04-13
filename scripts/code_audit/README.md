# CodeAudit — Vérification formelle structurelle pour uppli-code

## Pourquoi

Les LLMs fixent le symptôme local sans tracer la propriété/invariant sur tous les chemins du code. Démontré sur SWE-bench :
- **Issue 1** : 7 tentatives, le modèle ajoute des branches au lieu de fixer le format string. CodeAudit trouve le bug en 0.1s.
- **Issue 10** : le modèle fixe 1 bug sur 2. CodeAudit trouve les 2.
- **Avec le gate + sans contrainte TU** : le modèle fait enfin le bon fix (à 95%).

## En quoi c'est différent des linters

| | Linter classique (pylint, ruff, semgrep) | CodeAudit (vérification formelle) |
|---|---|---|
| **Scope** | 1 noeud AST isolé | N noeuds liés entre eux |
| **Question** | "Cette ligne est-elle correcte ?" | "Ces lignes sont-elles cohérentes entre elles ?" |
| **Détecte** | Erreurs syntaxiques, style, sécurité | Incohérences logiques, invariants violés |
| **Exemple** | `== None` → utilise `is None` | "condition compare des slices mais message affiche [0]" |
| **Pourquoi unique** | 20 000 règles existantes | **Personne ne fait ça** — patterns relationnels |

### Ce que les linters ne trouvent PAS

- Condition compare une liste mais le message d'erreur n'affiche que le premier élément
- 5/6 comparaisons utilisent `.upper()` mais la 6e ne le fait pas
- Variable normalisée dans un chemin d'exécution mais utilisée brute dans un autre
- Collection affichée directement dans un format string (crochets indésirables)
- Single raise qui sert 2 failure modes différents

Chaque ligne prise individuellement est **syntaxiquement correcte**. C'est la **relation entre les lignes** qui est cassée.

## Architecture

```
CodeAudit (Tool Rust — wrapper)
  │  spawn python3 scripts/code_audit/code_audit.py <file>
  │
  ├── ASTAnalyzer        → condition vs message, mutable defaults, bare except, format args
  ├── DataFlowTracer     → variable normalisée dans un chemin mais pas un autre
  ├── ControlFlowGraph   → chemins vers raise, single raise pour multiple failure modes
  ├── SymbolTable        → toutes les utilisations d'un symbole
  └── ConsistencyChecker → outlier dans un groupe de patterns similaires
          │
          ▼
    Rapport → modèle cross-référence avec le bug report → fix complet
```

## Patterns implémentés (14)

| # | Pattern | Analyseur | Générique |
|---|---------|-----------|-----------|
| 1 | Lossy error message (condition slice vs msg [0]) | ast | ✓ |
| 2 | Collection dans format string (crochets indésirables) | ast | ✓ |
| 3 | Mutable default argument | ast | ✓ |
| 4 | Bare except clause | ast | ✓ |
| 5 | None comparison avec == | ast | ✓ |
| 6 | Format string arg count mismatch | ast | ✓ |
| 7 | Inconsistent return (value vs bare) | ast | ✓ |
| 8 | String comparison sans case normalization | consistency | ✓ |
| 9 | re.compile() sans IGNORECASE | consistency | ✓ |
| 10 | startswith/endswith sans normalization | consistency | ✓ |
| 11 | Dict access style inconsistent ([] vs .get()) | consistency | ✓ |
| 12 | Error handling inconsistent (try/except) | consistency | ✓ |
| 13 | Variable transformée dans un chemin mais pas un autre | dataflow | ✓ |
| 14 | Single raise pour multiple failure modes | controlflow | ✓ |

## Mécanismes pour forcer l'appel

1. **Gate** : Edit/Write bloqué sur source files tant que CodeAudit n'a pas été appelé
2. **Post-edit check** : re-scan après l'edit, rejet si anomalies HIGH persistent
3. **System prompt** : "Before fixing a bug, call CodeAudit"
4. **PostToolUse nudge** : rappel après chaque Read sur un fichier source

## Roadmap

- [ ] Intégrer Semgrep comme backend pour les 20 000 patterns classiques
- [ ] Ajouter plus de patterns relationnels à partir du benchmark SWE-bench
- [ ] Multi-langage (Rust, JS, Go) via tree-sitter
- [ ] Fine-tuning du modèle pour suivre les rapports CodeAudit
- [ ] Post-edit check qui rejette si anomalies persistent
