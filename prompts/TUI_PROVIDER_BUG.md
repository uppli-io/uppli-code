# Bug: TUI affiche deepseek-reasoner au lieu de qwen3.6-plus

## Symptôme
Le mode headless (-p) fonctionne avec alibaba/qwen3.6-plus.
Le TUI affiche "deepseek-reasoner" en bas à gauche et envoie ce modèle à DashScope → 404.

## Cause probable
Le TUI dans run_interactive() a un chemin différent du headless pour résoudre le modèle.
Le QueryConfig est probablement construit avant le provider ou avec les mauvaises valeurs.

## À vérifier
1. Dans run_interactive(), comment le modèle est propagé à l'App
2. L'App.model_name est initialisé d'où ?
3. Le base_query_config.model est-il correct quand on arrive dans run_interactive ?
4. Y a-t-il un DEFAULT_MODEL hardcodé quelque part dans le chemin TUI ?

## Comment reproduire
```bash
# Settings: provider=alibaba, model=qwen3.6-plus
# Keychain: alibaba key present
# Pas de env var
uppli-code   # → TUI avec deepseek-reasoner
uppli-code -p "salut"  # → marche avec qwen
```

## Fichiers
- crates/cli/src/main.rs : run_interactive(), lignes ~2080+
- crates/tui/src/app.rs : App::new(), model_name
- crates/query/src/lib.rs : QueryConfig, effective_model
