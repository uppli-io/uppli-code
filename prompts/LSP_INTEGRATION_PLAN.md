# LSP Integration Plan

## Ce qui existe déjà

Le code est là, pas branché.

- `crates/core/src/lsp.rs` (1141 lignes) — LspClient complet : start, initialize, open/change/save/close document, get_diagnostics, shutdown. LspManager avec register_server, extension_map. Global singleton via `global_lsp_manager()`.
- `crates/tools/src/lsp_tool.rs` (109 lignes) — Tool "LSP" qui query les diagnostics pour un fichier. Déjà dans le registre tools.
- `crates/core/src/lib.rs:593` — Config `lsp_servers: Vec<LspServerConfig>` dans Settings.
- Settings.json a le champ `lsp_servers: []` (vide).

## Ce qui manque

1. **Démarrage** : personne ne lit `config.lsp_servers` et ne fait `register_server()` + `start()` au boot.
2. **Auto-détection** : pas de détection automatique des language servers installés.
3. **Injection dans le contexte** : après un Edit/Write, les diagnostics ne sont pas injectés dans le prochain tour du query loop.
4. **File watching** : quand le modèle édite un fichier, le LSP doit être notifié (didChange/didSave).

## Plan d'implémentation

### Phase 1 — Brancher le démarrage (1 jour)

Fichier: `crates/cli/src/main.rs`

Au démarrage (après la création du tool_ctx, avant le run_interactive/run_headless):
1. Lire `config.lsp_servers` 
2. Pour chaque config, appeler `global_lsp_manager().lock().await.register_server(config)`
3. Démarrer les clients : `manager.start_all(&working_dir).await`

Il faut ajouter `start_all` dans LspManager:
```rust
pub async fn start_all(&mut self, root_uri: &str) -> anyhow::Result<()> {
    for config in &self.configs {
        let mut client = LspClient::start(config.clone()).await?;
        client.initialize(root_uri).await?;
        self.clients.insert(config.name.clone(), client);
    }
    Ok(())
}
```

### Phase 2 — Auto-détection des language servers (1 jour)

Fichier: `crates/core/src/lsp.rs`

Fonction `detect_installed_servers()` qui:
1. Cherche `pyright` / `pylsp` dans PATH → config Python
2. Cherche `typescript-language-server` / `tsserver` dans PATH → config TypeScript
3. Cherche `rust-analyzer` dans PATH → config Rust
4. Cherche `gopls` dans PATH → config Go

Retourne un `Vec<LspServerConfig>` pré-rempli.

Appelé au démarrage si `config.lsp_servers` est vide:
```rust
if config.lsp_servers.is_empty() {
    config.lsp_servers = lsp::detect_installed_servers();
}
```

### Phase 3 — Notification après Edit/Write (2 jours)

Fichier: `crates/tools/src/file_edit.rs` et `crates/tools/src/file_write.rs`

Après chaque Edit ou Write réussi:
1. Appeler `manager.change_document(uri, new_content).await`
2. Appeler `manager.save_document(uri).await`
3. Attendre 100ms pour les diagnostics
4. Si des erreurs existent, les ajouter au résultat du tool

```rust
// Dans file_edit.rs, après l'écriture réussie:
let lsp = cc_core::lsp::global_lsp_manager();
let mut mgr = lsp.lock().await;
if let Ok(()) = mgr.change_document(&file_uri, &new_content).await {
    mgr.save_document(&file_uri).await.ok();
    tokio::time::sleep(Duration::from_millis(100)).await;
    let diags = mgr.get_diagnostics_for_file(&absolute_path);
    if !diags.is_empty() {
        result.push_str("\n\nLSP Diagnostics:\n");
        result.push_str(&cc_core::lsp::LspClient::format_diagnostics(&diags));
    }
}
```

### Phase 4 — Diagnostics dans le system prompt (1 jour)

Fichier: `crates/core/src/system_prompt.rs` ou `crates/query/src/lib.rs`

Avant chaque appel API, vérifier s'il y a des diagnostics actifs et les injecter:

```rust
let lsp = cc_core::lsp::global_lsp_manager();
let mgr = lsp.lock().await;
let all_diags = mgr.all_diagnostics();
if !all_diags.is_empty() {
    let diag_text = cc_core::lsp::LspClient::format_diagnostics(&all_diags);
    // Ajouter comme message système avant le prochain tour
}
```

### Phase 5 — Config dans settings.json (0.5 jour)

Exemple de config:
```json
{
  "config": {
    "lsp_servers": [
      {
        "name": "pyright",
        "command": "pyright-langserver",
        "args": ["--stdio"],
        "file_patterns": ["*.py"],
        "extension_to_language": { ".py": "python" }
      },
      {
        "name": "typescript",
        "command": "typescript-language-server",
        "args": ["--stdio"],
        "file_patterns": ["*.ts", "*.tsx", "*.js", "*.jsx"],
        "extension_to_language": { ".ts": "typescript", ".tsx": "typescriptreact" }
      }
    ]
  }
}
```

## Résultat attendu

Avant: le modèle écrit du code, lance `pytest`, voit les erreurs, corrige, relance.
Après: le modèle écrit du code, le LSP dit immédiatement "erreur ligne 42: type incompatible", le modèle corrige avant même de lancer les tests.

C'est 3-5x moins de tours pour du code qui compile.

## Fichiers à modifier
- `crates/cli/src/main.rs` — démarrage des LSP servers
- `crates/core/src/lsp.rs` — `start_all()`, `detect_installed_servers()`
- `crates/tools/src/file_edit.rs` — notification après edit
- `crates/tools/src/file_write.rs` — notification après write
- `crates/query/src/lib.rs` — injection diagnostics dans le contexte

## Dépendances
Aucune nouvelle crate. Tout est dans le code existant.

## Risques
- Les language servers mettent du temps à démarrer (pyright ~2s, rust-analyzer ~5s)
- Solution: démarrage en background, disponibilité progressive
- Les diagnostics arrivent en async (publishDiagnostics notification)
- Solution: le code existant les cache dans `DashMap<String, Vec<LspDiagnostic>>`
