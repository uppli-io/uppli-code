// FileEdit tool: exact string replacement in files.
//
// Simple find-and-replace. The model provides old_string (text to find)
// and new_string (replacement). For structural edits with automatic
// indentation, use AstEdit instead.

use crate::{PermissionLevel, Tool, ToolContext, ToolResult};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};
use tracing::debug;

pub struct FileEditTool;

#[derive(Debug, Deserialize)]
struct FileEditInput {
    file_path: String,
    old_string: String,
    new_string: String,
    #[serde(default)]
    replace_all: bool,
}

#[async_trait]
impl Tool for FileEditTool {
    fn name(&self) -> &str {
        cc_core::constants::TOOL_NAME_FILE_EDIT
    }

    fn description(&self) -> &str {
        "Find and replace exact text in a file. For structural code changes \
         with automatic indentation, use AstEdit instead. Syntax-checked \
         after edit."
    }

    fn permission_level(&self) -> PermissionLevel {
        PermissionLevel::Write
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "file_path": {
                    "type": "string",
                    "description": "The absolute path to the file to modify"
                },
                "old_string": {
                    "type": "string",
                    "description": "The exact text to find and replace. Include 3+ lines \
                        of context before and after to ensure a unique match."
                },
                "new_string": {
                    "type": "string",
                    "description": "The replacement text."
                },
                "replace_all": {
                    "type": "boolean",
                    "description": "Replace all occurrences (default false)"
                }
            },
            "required": ["file_path", "old_string", "new_string"]
        })
    }

    async fn execute(&self, input: Value, ctx: &ToolContext) -> ToolResult {
        let params: FileEditInput = match serde_json::from_value(input) {
            Ok(p) => p,
            Err(e) => return ToolResult::error(format!("Invalid input: {}", e)),
        };

        if params.old_string == params.new_string {
            return ToolResult::error("old_string and new_string must be different".to_string());
        }

        let path = ctx.resolve_path(&params.file_path);
        debug!(path = %path.display(), "Editing file");

        if let Err(e) =
            ctx.check_permission(self.name(), &format!("Edit {}", path.display()), false)
        {
            return ToolResult::error(e.to_string());
        }

        let content = match tokio::fs::read_to_string(&path).await {
            Ok(c) => c,
            Err(e) => {
                return ToolResult::error(format!("Failed to read {}: {}", path.display(), e))
            }
        };

        let count = content.matches(&params.old_string).count();

        if count == 0 {
            return ToolResult::error(format!(
                "old_string not found in {} — likely an indentation mismatch. \
                 Try AstEdit for structural changes with automatic indentation, \
                 or Bash with sed for regex replacement.",
                path.display()
            ));
        }

        if count > 1 && !params.replace_all {
            return ToolResult::error(format!(
                "old_string appears {} times in {}. Provide more context to \
                 make it unique, or set replace_all to true.",
                count,
                path.display()
            ));
        }

        let new_content = if params.replace_all {
            content.replace(&params.old_string, &params.new_string)
        } else {
            content.replacen(&params.old_string, &params.new_string, 1)
        };

        if let Err(e) = tokio::fs::write(&path, &new_content).await {
            return ToolResult::error(format!("Failed to write {}: {}", path.display(), e));
        }

        // Syntax check — warn but don't revert.
        let lint = crate::lint::check_syntax(&path).await;

        ctx.record_file_change(
            path.clone(),
            content.as_bytes(),
            new_content.as_bytes(),
            self.name(),
        );

        let replacements = if params.replace_all { count } else { 1 };
        let mut msg = format!(
            "Successfully edited {} ({} replacement{}).",
            path.display(),
            replacements,
            if replacements != 1 { "s" } else { "" }
        );
        if !lint.ok {
            msg.push_str(&format!(
                "\n\n⚠️ SYNTAX ERROR (edit applied but code is broken):\n{}",
                lint.errors
            ));
        } else if let Some(lang) = lint.language {
            msg.push_str(&format!(" Syntax check passed ({}).", lang));
        }

        // LSP diagnostics
        {
            let lsp = cc_core::lsp::global_lsp_manager();
            let mut mgr = lsp.lock().await;
            let abs_path = path.to_string_lossy().to_string();
            if let Ok(()) = mgr.open_file(&abs_path, &ctx.working_dir).await {
                let _ = mgr.notify_file_changed(&abs_path, &new_content).await;
                let _ = mgr.notify_file_saved(&abs_path).await;
                drop(mgr);
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                let mgr = lsp.lock().await;
                let diags = mgr.get_diagnostics_for_file(&abs_path);
                if !diags.is_empty() {
                    msg.push_str("\n\nLSP Diagnostics:\n");
                    for d in &diags {
                        msg.push_str(&format!(
                            "\n  {} {}:{}:{} — {}",
                            d.severity.as_str().to_uppercase(),
                            d.file,
                            d.line,
                            d.column,
                            d.message
                        ));
                    }
                }
            }
        }

        ToolResult::success(msg).with_metadata(json!({
            "file_path": path.display().to_string(),
            "replacements": replacements,
        }))
    }
}
