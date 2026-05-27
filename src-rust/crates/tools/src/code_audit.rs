// CodeAudit — Structural analysis tool for bug detection.
//
// Runs 5 Python-based analyzers on a source file and returns a unified
// report of anomalies: inconsistent patterns, lossy error messages,
// data flow issues, and more.
//
// The model calls this BEFORE fixing a bug to get a complete picture
// of all structural issues in the file. This prevents partial fixes
// where the model corrects the reported symptom but misses related
// violations of the same invariant.
//
// Architecture:
//   Rust (this file) = thin wrapper that spawns the Python analyzer
//   Python (scripts/code_audit/) = 5 analyzers running in parallel

use crate::{PermissionLevel, Tool, ToolContext, ToolResult};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::time::Duration;
use tracing::debug;

pub struct CodeAuditTool;

#[derive(Debug, Deserialize)]
struct AuditInput {
    /// Path to the file to audit (absolute or relative to working directory).
    file_path: String,
    /// Programming language (auto-detected from extension if omitted).
    #[serde(default = "default_language")]
    language: String,
    /// Optional symbols to focus the analysis on (from the bug report).
    #[serde(default)]
    focus_symbols: Vec<String>,
}

fn default_language() -> String {
    "auto".to_string()
}

#[async_trait]
impl Tool for CodeAuditTool {
    fn name(&self) -> &str {
        "CodeAudit"
    }

    fn description(&self) -> &str {
        "Audit a source file for structural anomalies before fixing bugs. \
         Runs 5 static analyzers (AST patterns, data flow, control flow, \
         symbol table, consistency) and returns a report of all issues found. \
         Call this BEFORE editing to ensure your fix is complete — it surfaces \
         problems the bug report doesn't mention."
    }

    fn permission_level(&self) -> PermissionLevel {
        PermissionLevel::ReadOnly
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "file_path": {
                    "type": "string",
                    "description": "Path to the file to audit (absolute or relative to working directory)"
                },
                "language": {
                    "type": "string",
                    "description": "Programming language (auto-detected from extension if omitted)",
                    "default": "auto"
                },
                "focus_symbols": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Optional list of symbols to focus on (e.g., variable names from the bug report)"
                }
            },
            "required": ["file_path"]
        })
    }

    async fn execute(&self, input: Value, ctx: &ToolContext) -> ToolResult {
        let params: AuditInput = match serde_json::from_value(input) {
            Ok(p) => p,
            Err(e) => return ToolResult::error(format!("Invalid input: {}", e)),
        };

        // Resolve path
        let file_path = if std::path::Path::new(&params.file_path).is_absolute() {
            PathBuf::from(&params.file_path)
        } else {
            ctx.working_dir.join(&params.file_path)
        };

        if !file_path.exists() {
            return ToolResult::error(format!("File not found: {}", file_path.display()));
        }

        // Find the Python script
        let script_path = find_audit_script();
        if script_path.is_none() {
            return ToolResult::error(
                "CodeAudit script not found. Expected at scripts/code_audit/code_audit.py"
                    .to_string(),
            );
        }
        let script_path = script_path.unwrap();

        // Build command
        let mut args = vec![
            script_path.to_string_lossy().to_string(),
            file_path.to_string_lossy().to_string(),
            "--format".to_string(),
            "markdown".to_string(),
            "--language".to_string(),
            params.language,
        ];

        if !params.focus_symbols.is_empty() {
            args.push("--focus".to_string());
            args.push(params.focus_symbols.join(","));
        }

        debug!(file = %file_path.display(), "Running CodeAudit");

        // Spawn with timeout
        let output = match tokio::time::timeout(
            Duration::from_secs(10),
            tokio::process::Command::new("python3")
                .args(&args)
                .current_dir(&ctx.working_dir)
                .output(),
        )
        .await
        {
            Ok(Ok(output)) => output,
            Ok(Err(e)) => {
                return ToolResult::error(format!(
                    "Failed to run CodeAudit (is Python 3 installed?): {}",
                    e
                ));
            }
            Err(_) => {
                return ToolResult::error("CodeAudit timed out after 10 seconds.".to_string());
            }
        };

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();

        if !output.status.success() {
            return ToolResult::error(format!(
                "CodeAudit failed (exit {}):\n{}\n{}",
                output.status.code().unwrap_or(-1),
                stdout,
                stderr,
            ));
        }

        if stdout.trim().is_empty() {
            return ToolResult::success("CodeAudit: No anomalies found.".to_string());
        }

        ToolResult::success(stdout)
    }
}

/// Find the code_audit.py script by searching common locations.
fn find_audit_script() -> Option<PathBuf> {
    // 0. Environment variable override
    if let Ok(p) = std::env::var("UPPLI_CODE_AUDIT_SCRIPT") {
        let p = PathBuf::from(p);
        if p.exists() {
            return Some(p);
        }
    }

    // 1. Relative to current working directory (and ancestors)
    let candidates = [
        "scripts/code_audit/code_audit.py",
        "../scripts/code_audit/code_audit.py",
        "../../scripts/code_audit/code_audit.py",
        "../../../scripts/code_audit/code_audit.py",
    ];

    for c in &candidates {
        let p = PathBuf::from(c);
        if p.exists() {
            return Some(p);
        }
    }

    // 2. Relative to the binary location — walk ancestors until scripts/ is found.
    // Typical layout: <repo>/src-rust/target/release/uppli-code
    // Script lives at: <repo>/scripts/code_audit/code_audit.py
    // So we may need to go up multiple levels (release → target → src-rust → repo).
    if let Ok(exe) = std::env::current_exe() {
        let mut ancestor = exe.parent();
        for _ in 0..6 {
            if let Some(dir) = ancestor {
                let p = dir.join("scripts/code_audit/code_audit.py");
                if p.exists() {
                    return Some(p);
                }
                ancestor = dir.parent();
            } else {
                break;
            }
        }
    }

    // 2b. Walk ancestors of the current working directory.
    if let Ok(cwd) = std::env::current_dir() {
        let mut ancestor: Option<&std::path::Path> = Some(cwd.as_path());
        for _ in 0..8 {
            if let Some(dir) = ancestor {
                let p = dir.join("scripts/code_audit/code_audit.py");
                if p.exists() {
                    return Some(p);
                }
                ancestor = dir.parent();
            } else {
                break;
            }
        }
    }

    // 3. Home directory
    if let Some(home) = dirs::home_dir() {
        let p = home.join(".uppli/scripts/code_audit/code_audit.py");
        if p.exists() {
            return Some(p);
        }
    }

    None
}
