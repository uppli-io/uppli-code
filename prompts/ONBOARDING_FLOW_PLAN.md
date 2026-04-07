# Onboarding TUI — Plan

## Flow utilisateur

```
uppli-code
    │
    ▼
┌─────────────────────────────────┐
│  Uppli Code                     │
│                                 │
│  Choose your provider:          │
│                                 │
│  ▸ DeepSeek    cloud, thinking  │
│    Qwen3       cloud, thinking  │
│    Mistral     cloud            │
│    Ollama      local, free      │
│    Custom      your endpoint    │
│                                 │
│  ↑↓ navigate   Enter confirm    │
└─────────────────────────────────┘
    │ Enter
    ▼
┌─────────────────────────────────┐
│  DashScope API key              │
│                                 │
│  ┌───────────────────────────┐  │
│  │ sk-************************│  │
│  └───────────────────────────┘  │
│                                 │
│  Tip: export DASHSCOPE_API_KEY  │
│                                 │
│  Enter confirm   Esc back       │
└─────────────────────────────────┘
    │ Enter (skip si Ollama)
    ▼
┌─────────────────────────────────┐
│  Choose your model:             │
│                                 │
│  ▸ qwen3-235b-a22b  reasoning  │
│    qwen3-32b         fast       │
│    qwen3-30b-a3b     ultra fast │
│                                 │
│  Fast model for tool results:   │
│  ▸ qwen3-32b (recommended)     │
│    none                         │
│                                 │
│  ↑↓ navigate   Enter confirm    │
└─────────────────────────────────┘
    │ Enter
    ▼
┌─────────────────────────────────┐
│  Ready                          │
│                                 │
│  Provider:  Qwen3               │
│  Model:     qwen3-235b-a22b    │
│  Fast:      qwen3-32b          │
│  Key:       ****3b68 (keychain) │
│                                 │
│  Enter to start coding          │
└─────────────────────────────────┘
    │ Enter
    ▼
  TUI normal (prompt input)
```

## Quand ça se déclenche

- Premier lancement (pas de provider dans settings.json, pas de clé dans keychain)
- Commande /provider dans le TUI
- Flag --setup

## Quand ça ne se déclenche PAS

- Config déjà faite (provider + modèle dans settings)
- Clé déjà dans keychain ou env var
- Mode headless (-p "prompt") → erreur claire si pas de clé
- /model → ouvre le model picker du provider actif, pas l'onboarding

## State machine

```
enum OnboardingStep {
    ProviderChoice { selected: usize },
    ApiKey { input: String, cursor: usize, error: Option<String> },
    ModelChoice { selected: usize, fast_selected: usize },
    Confirm,
}
```

Chaque step a :
- son render (Paragraph + style)
- ses key handlers (Up/Down/Enter/Esc/Char)
- ses données (lues depuis provider_registry / known_models)

## Navigation

- Flèche haut/bas : naviguer dans les listes
- Enter : valider le choix, passer à l'étape suivante
- Esc : revenir à l'étape précédente (sauf ProviderChoice → dismiss)
- Char : saisie dans le champ API key
- Backspace : effacer dans le champ API key
- Tab : pas utilisé

## Données

Les listes viennent de :
- Providers : cc_api::provider_registry()
- Modèles : preset.known_models (hardcodé dans le preset) + fetch API en async si possible
- Fast model : preset.fast_model suggestion

## Persistance

Au Confirm :
1. Clé → cc_core::keychain::store_key(preset.auth.keychain_key, &key)
2. Config → settings.json (provider, model, fastModel, supportsThinking)
3. has_completed_onboarding → true dans settings

## Fichiers à modifier

- crates/tui/src/onboarding_dialog.rs → réécriture complète
- crates/tui/src/app.rs → key handling pour le nouveau flow
- crates/tui/src/render.rs → render du nouveau flow
- crates/cli/src/main.rs → supprimer le bloc "Welcome to Uppli Code" stdin
- crates/cli/src/main.rs → si pas de provider configuré + mode TUI → app.onboarding_dialog.show()

## Ce qu'on ne fait PAS (v0.2)

- SSO Uppli (OAuth, gestion budget par dev)
- Switch de provider en live sans restart
- Fetch async des modèles depuis l'API pendant l'onboarding
- Validation de la clé API en temps réel (on vérifie au premier appel)
