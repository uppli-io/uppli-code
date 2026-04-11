// AstEdit tool: structural code search and rewrite via ast-grep.
//
// Unlike the Edit tool (text-level find/replace), AstEdit operates on the
// Abstract Syntax Tree. The model provides a structural pattern and a
// rewrite template — ast-grep finds matching AST nodes and transforms them
// with correct indentation automatically.
//
// Requires: `sg` (ast-grep) installed. Install: brew install ast-grep
//
// Why this exists:
//   Edit tool fails when the model gets indentation wrong (3 spaces vs 4).
//   AstEdit doesn't care about whitespace — it matches code structure and
//   regenerates with correct indentation from the AST.

use crate::{PermissionLevel, Tool, ToolContext, ToolResult};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};
use std::path::PathBuf;
use tracing::debug;

pub struct AstEditTool;

#[derive(Debug, Deserialize)]
struct AstEditInput {
    /// File to modify.
    file_path: String,
    /// Language (python, javascript, typescript, rust, go, java, etc.).
    language: String,
    /// ast-grep pattern to match. Use $VAR for single nodes, $$$VAR for
    /// multiple nodes. Example: "$X.delete_batch($$$ARGS)"
    pattern: String,
    /// Replacement code. Can reference captured variables from pattern.
    /// Indentation is handled automatically by the AST.
    rewrite: String,
}

#[async_trait]
impl Tool for AstEditTool {
    fn name(&self) -> &str {
        "AstEdit"
    }

    fn description(&self) -> &str {
        "Structural code search and rewrite using AST patterns. Unlike Edit \
         (text matching), AstEdit understands code structure and handles \
         indentation automatically. Use $VAR for single nodes, $$$VAR for \
         multiple nodes in patterns."
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
                    "description": "The file to modify"
                },
                "language": {
                    "type": "string",
                    "description": "Programming language (python, javascript, typescript, rust, go, java, c, cpp, etc.)"
                },
                "pattern": {
                    "type": "string",
                    "description": "ast-grep pattern to match. Use $VAR for single AST node, $$$VAR for multiple nodes. Example: '$X.delete_batch($$$ARGS)'"
                },
                "rewrite": {
                    "type": "string",
                    "description": "Replacement code using captured variables from pattern. Indentation is automatic. Example: '$X.delete_batch($$$ARGS)\\nsetattr(instance, pk, None)'"
                }
            },
            "required": ["file_path", "language", "pattern", "rewrite"]
        })
    }

    async fn execute(&self, input: Value, ctx: &ToolContext) -> ToolResult {
        let params: AstEditInput = match serde_json::from_value(input) {
            Ok(p) => p,
            Err(e) => return ToolResult::error(format!("Invalid input: {}", e)),
        };

        let path = ctx.resolve_path(&params.file_path);

        // Permission check
        if let Err(e) =
            ctx.check_permission(self.name(), &format!("AstEdit {}", path.display()), false)
        {
            return ToolResult::error(e.to_string());
        }

        // Check file exists
        if !path.exists() {
            return ToolResult::error(format!("File not found: {}", path.display()));
        }

        // Read original content for diff/revert
        let original = match tokio::fs::read_to_string(&path).await {
            Ok(c) => c,
            Err(e) => return ToolResult::error(format!("Failed to read {}: {}", path.display(), e)),
        };

        debug!(
            file = %path.display(),
            language = %params.language,
            pattern = %params.pattern,
            "AstEdit executing"
        );

        // Run ast-grep directly — no YAML file needed.
        let output = tokio::process::Command::new("sg")
            .args([
                "run",
                "--pattern", &params.pattern,
                "--rewrite", &params.rewrite,
                "--lang", &params.language,
                "--update-all",
                "--debug-query=ast",
            ])
            .arg(&path)
            .output()
            .await;

        let output = match output {
            Ok(o) => o,
            Err(e) => {
                return ToolResult::error(format!(
                    "Failed to run ast-grep (sg). Is it installed? (brew install ast-grep)\nError: {}", e
                ));
            }
        };

        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();

        // Check if changes were made
        let new_content = match tokio::fs::read_to_string(&path).await {
            Ok(c) => c,
            Err(e) => return ToolResult::error(format!("Failed to read after edit: {}", e)),
        };

        if new_content == original {
            return ToolResult::error(format!(
                "Pattern not found in {} (language: {}).\n\
                 Pattern: {}\n\n\
                 Pattern AST:\n{}{}\n\n\
                 Call AstGrepHelper to get pattern examples if needed.",
                path.display(),
                params.language,
                params.pattern,
                stdout.trim(),
                if stderr.trim().is_empty() {
                    String::new()
                } else {
                    format!("\n{}", stderr.trim())
                },
            ));
        }

        // Syntax check — warn but don't revert
        let lint = crate::lint::check_syntax(&path).await;

        ctx.record_file_change(
            path.clone(),
            original.as_bytes(),
            new_content.as_bytes(),
            self.name(),
        );

        // Count changes from stdout ("Applied N changes")
        let changes = stdout
            .lines()
            .find(|l| l.contains("Applied"))
            .unwrap_or("Applied changes");

        let mut msg = format!(
            "AstEdit: {} in {} (language: {}).",
            changes.trim(),
            path.display(),
            params.language,
        );

        if !lint.ok {
            msg.push_str(&format!(
                "\n\n⚠️ SYNTAX ERROR after edit:\n{}",
                lint.errors
            ));
        } else if let Some(lang) = lint.language {
            msg.push_str(&format!(" Syntax check passed ({}).", lang));
        }

        ToolResult::success(msg).with_metadata(json!({
            "file_path": path.display().to_string(),
            "language": params.language,
            "pattern": params.pattern,
        }))
    }
}

