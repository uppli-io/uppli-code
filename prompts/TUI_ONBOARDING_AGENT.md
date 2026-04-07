Tu travailles sur Uppli Code — /Users/sayahfarid/uppli-code/claurst/src-rust/

CONTEXTE : Le CLI est prêt, multi-provider (DeepSeek, Qwen3, Ollama, Mistral). L'architecture provider est dans crates/api/src/provider.rs (ProviderPreset, AuthConfig, ProviderCapabilities). Le registre est dans provider_factory.rs (provider_registry(), find_preset()). Le keychain OS est dans crates/core/src/keychain.rs.

PROBLÈME : L'onboarding (choix provider + clé API) se fait sur stdout brut AVANT le TUI. L'utilisateur tape uppli-code et voit un menu texte moche, puis le TUI se lance. Claude Code fait un onboarding interactif DANS le TUI. Nous aussi.

OBJECTIF : Réécrire l'onboarding dans le TUI ratatui. Supprimer l'onboarding CLI dans main.rs (cherche "Welcome to Uppli Code").

Le fichier existant est crates/tui/src/onboarding_dialog.rs. Il a 2 pages statiques (Welcome, KeyBindings). Il faut le transformer en flow interactif :

Page 1 — Choix provider
Liste les providers depuis cc_api::provider_registry(). Navigation flèche haut/bas. Enter pour valider. Afficher pour chaque provider : nom, description, si clé requise ou non.

Page 2 — Clé API (si auth.required)
Champ de saisie pour la clé. Stocker dans le keychain via cc_core::keychain::store_key(). Afficher "Tip: you can also set DASHSCOPE_API_KEY env var" (env var depuis preset.auth.env_vars[0]). Skip cette page si le provider ne nécessite pas de clé (Ollama).

Page 3 — Choix modèle
Lister les modèles depuis provider.capabilities().known_models. Navigation flèche. Enter pour valider. Proposer le fast model optionnel.

Page 4 — Confirmation
Résumé : provider, modèle, fast model. Enter pour lancer.

Sauvegarder tout dans ~/.uppli/settings.json (provider, model, fastModel, supportsThinking). La clé va dans le keychain, PAS dans le JSON.

AUSSI : 
- /model dans le TUI ne change que le modèle du provider actif (pas le provider)
- /provider dans le TUI relance l'onboarding complet
- Fixer les "Claude" dans onboarding_dialog.rs lignes 131-135

CONTRAINTES :
- Pattern ratatui existant : State struct + render function + key handling dans app.rs
- Regarder comment model_picker.rs gère la navigation (flèches, filter, Enter) pour s'inspirer
- Compiler après chaque changement
- Le TUI dit "Claude" à 2 endroits dans l'onboarding, corriger

Travaille dans un worktree : git worktree add /tmp/uppli-wt-tui-onboarding -b feat/tui-onboarding

IMPORTANT : Tu es un architecte, pas un exécutant. Lis tout le code concerné, conçois ta solution, puis implémente. Ce code sera publié en open source.
