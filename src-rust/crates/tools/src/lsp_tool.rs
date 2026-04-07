// LSPTool — query language server diagnostics for a file.
//
// Ported from src/tools/LSPTool/ in the TypeScript source.  Returns errors,
// warnings, and hints emitted by any configured language server for the
// requested file.

use crate::{PermissionLevel, Tool, ToolContext, ToolResult};
use async_trait::async_trait;
use serde_json::Value;

pub struct LspTool;

#[async_trait]
impl Tool for LspTool {
    fn name(&self) -> &str {
        "LSP"
    }

    fn description(&self) -> &str {
        "Query language server diagnostics for a file. Returns errors, warnings, and hints \
         reported by any language server configured for the file type."
    }

    fn permission_level(&self) -> PermissionLevel {
        PermissionLevel::ReadOnly
    }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "file_path": {
                    "type": "string",
                    "description": "Absolute or working-directory-relative path to the file to \
                                    get diagnostics for."
                }
            },
            "required": ["file_path"]
        })
    }

    async fn execute(&self, input: Value, ctx: &ToolContext) -> ToolResult {
        let file_path_raw = match input.get("file_path").and_then(|v| v.as_str()) {
            Some(p) => p.to_string(),
            None => return ToolResult::error("file_path is required"),
        };

        // Resolve to an absolute path.
        let path = if std::path::Path::new(&file_path_raw).is_absolute() {
            file_path_raw.clone()
        } else {
            ctx.working_dir
                .join(&file_path_raw)
                .to_string_lossy()
                .into_owned()
        };

        let lsp_manager_arc = cc_core::lsp::global_lsp_manager();

        // Open the file so the LSP server knows about it and begins sending
        // diagnostics (textDocument/didOpen notification).
        {
            let mut manager = lsp_manager_arc.lock().await;
            if let Err(_e) = manager.open_file(&path, &ctx.working_dir).await {
                // No LSP server is configured for this file type — return a
                // graceful informational message rather than an error.
                return ToolResult::success(format!(
                    "No LSP servers configured for '{}'. \
                     Configure language servers in settings to enable diagnostics.",
                    path
                ));
            }
        }

        // Give the server a short window to deliver diagnostics via the
        // textDocument/publishDiagnostics notification (at most 50 ms).
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // Read cached diagnostics.
        let diagnostics = {
            let manager = lsp_manager_arc.lock().await;
            manager.get_diagnostics_for_file(&path)
        };

        if diagnostics.is_empty() {
            return ToolResult::success(format!("No diagnostics for '{}'.", path));
        }

        let output = diagnostics
            .iter()
            .map(|d| {
                let sev = d.severity.as_str().to_uppercase();
                let source = d.source.as_deref().unwrap_or("lsp");
                let code_part = d
                    .code
                    .as_deref()
                    .map(|c| format!(" [{}]", c))
                    .unwrap_or_default();
                format!(
                    "[{}] {}:{}:{} — {}  ({}){}",
                    sev, d.file, d.line, d.column, d.message, source, code_part
                )
            })
            .collect::<Vec<_>>()
            .join("\n");

        ToolResult::success(output)
    }
}
