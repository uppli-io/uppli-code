// Patch tool: apply unified diffs via `git apply`.
//
// Unlike the Edit tool (exact string match), the Patch tool accepts standard
// unified diff format — the same format LLMs see in millions of GitHub PRs.
// Git handles fuzzy matching, line offset tolerance, and 3-way merge natively.
//
// This is the only coding agent that uses git's patch machinery directly.
// Advantages over exact string replacement:
//   - Tolerant to whitespace/indentation differences
//   - Handles line offset (patch applies even if code shifted)
//   - Multi-file patches in a single operation
//   - 3-way merge as fallback
//   - LLMs are trained on this format (GitHub diffs)

use crate::{PermissionLevel, Tool, ToolContext, ToolResult};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};
use std::path::PathBuf;
use tracing::debug;

pub struct PatchTool;

#[derive(Debug, Deserialize)]
struct PatchInput {
    /// Unified diff to apply. Can cover one or multiple files.
    /// Format: standard `git diff` / `diff -u` output.
    diff: String,

    /// Working directory where the patch applies (default: cwd).
    #[serde(default)]
    working_dir: Option<String>,
}

#[async_trait]
impl Tool for PatchTool {
    fn name(&self) -> &str {
        "Patch"
    }

    fn description(&self) -> &str {
        "Apply a unified diff patch to one or more files using git. \
         Accepts standard unified diff format (like `git diff` output). \
         More robust than Edit for complex changes: tolerates line offsets, \
         handles multi-file patches, and uses git's 3-way merge as fallback. \
         Use this when Edit fails or when modifying multiple files at once."
    }

    fn permission_level(&self) -> PermissionLevel {
        PermissionLevel::Write
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "diff": {
                    "type": "string",
                    "description": "Unified diff to apply (standard git diff format). \
                        Example:\n\
                        --- a/file.py\n\
                        +++ b/file.py\n\
                        @@ -10,3 +10,4 @@\n\
                         existing line\n\
                        +new line\n\
                         another existing line"
                },
                "working_dir": {
                    "type": "string",
                    "description": "Working directory for the patch (default: project root)"
                }
            },
            "required": ["diff"]
        })
    }

    async fn execute(&self, input: Value, ctx: &ToolContext) -> ToolResult {
        let params: PatchInput = match serde_json::from_value(input) {
            Ok(p) => p,
            Err(e) => return ToolResult::error(format!("Invalid input: {}", e)),
        };

        if params.diff.trim().is_empty() {
            return ToolResult::error("diff is empty".to_string());
        }

        let work_dir = params
            .working_dir
            .as_ref()
            .map(|d| ctx.resolve_path(d))
            .unwrap_or_else(|| ctx.working_dir.clone());

        // Extract file paths from the diff for permission checks.
        let files = extract_files_from_diff(&params.diff);
        if files.is_empty() {
            return ToolResult::error(
                "Could not parse file paths from diff. Make sure the diff \
                 starts with --- a/path and +++ b/path lines."
                    .to_string(),
            );
        }

        // Permission check for each file.
        for file in &files {
            let abs_path = work_dir.join(file);
            if let Err(e) = ctx.check_permission(
                self.name(),
                &format!("Patch {}", abs_path.display()),
                false,
            ) {
                return ToolResult::error(e.to_string());
            }
        }

        debug!(
            files = ?files,
            diff_len = params.diff.len(),
            "Applying patch"
        );

        // Normalize the diff: ensure it ends with a newline (git requires it)
        // and fix common LLM mistakes in diff formatting.
        let diff = normalize_diff(&params.diff);

        // Write diff to a temp file.
        let tmp_dir = std::env::temp_dir();
        let patch_path = tmp_dir.join(format!("uppli_patch_{}.diff", uuid::Uuid::new_v4()));
        debug!(diff_len = diff.len(), "Writing patch to {:?}", patch_path);
        if let Err(e) = tokio::fs::write(&patch_path, &diff).await {
            return ToolResult::error(format!("Failed to write temp patch file: {}", e));
        }

        // Strategy 1: git apply (strict)
        let result = run_git_apply(&work_dir, &patch_path, &[]).await;
        if result.success {
            let _ = tokio::fs::remove_file(&patch_path).await;
            return build_success_result(ctx, &work_dir, &files, &result, "git apply").await;
        }

        // Strategy 2: git apply --3way (fuzzy with 3-way merge)
        debug!("Strict apply failed, trying --3way");
        let result_3way = run_git_apply(&work_dir, &patch_path, &["--3way"]).await;
        if result_3way.success {
            let _ = tokio::fs::remove_file(&patch_path).await;
            return build_success_result(ctx, &work_dir, &files, &result_3way, "git apply --3way")
                .await;
        }

        // Strategy 3: git apply --reject (apply what we can, show rejections)
        debug!("3-way merge failed, trying --reject");
        let result_reject = run_git_apply(&work_dir, &patch_path, &["--reject"]).await;
        let _ = tokio::fs::remove_file(&patch_path).await;

        if result_reject.success {
            return build_success_result(
                ctx,
                &work_dir,
                &files,
                &result_reject,
                "git apply --reject (partial)",
            )
            .await;
        }

        // All strategies failed — return detailed error.
        let mut msg = format!(
            "Patch could not be applied to {}.\n\n",
            work_dir.display()
        );
        msg.push_str("git apply output:\n");
        msg.push_str(&result.stderr);
        msg.push_str("\n\ngit apply --3way output:\n");
        msg.push_str(&result_3way.stderr);
        msg.push_str("\n\nMake sure the diff is valid unified diff format and the ");
        msg.push_str("context lines match the current file content.");

        // Clean up .rej files if any
        for file in &files {
            let rej = work_dir.join(format!("{}.rej", file));
            let _ = tokio::fs::remove_file(&rej).await;
        }

        ToolResult::error(msg)
    }
}

// ---------------------------------------------------------------------------
// Git apply runner
// ---------------------------------------------------------------------------

struct ApplyResult {
    success: bool,
    stdout: String,
    stderr: String,
}

async fn run_git_apply(work_dir: &PathBuf, patch_path: &PathBuf, extra_args: &[&str]) -> ApplyResult {
    let mut cmd = tokio::process::Command::new("git");
    cmd.current_dir(work_dir)
        .arg("apply")
        .arg("--verbose");
    for arg in extra_args {
        cmd.arg(arg);
    }
    cmd.arg(patch_path);

    match cmd.output().await {
        Ok(output) => ApplyResult {
            success: output.status.success(),
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        },
        Err(e) => ApplyResult {
            success: false,
            stdout: String::new(),
            stderr: format!("Failed to run git apply: {}", e),
        },
    }
}

// ---------------------------------------------------------------------------
// Success result builder
// ---------------------------------------------------------------------------

async fn build_success_result(
    ctx: &ToolContext,
    work_dir: &PathBuf,
    files: &[String],
    result: &ApplyResult,
    method: &str,
) -> ToolResult {
    let mut msg = format!(
        "Patch applied successfully via {} ({} file{}).",
        method,
        files.len(),
        if files.len() != 1 { "s" } else { "" }
    );

    // Syntax-check each modified file.  If any fail, revert the whole patch.
    let mut lint_errors = Vec::new();
    for file in files {
        let abs_path = work_dir.join(file);
        let lint = crate::lint::check_syntax(&abs_path).await;
        if !lint.ok {
            lint_errors.push(crate::lint::format_lint_error(&lint, &abs_path));
        }
    }
    if !lint_errors.is_empty() {
        // Revert: git checkout the modified files.
        for file in files {
            let _ = tokio::process::Command::new("git")
                .current_dir(work_dir)
                .args(["checkout", "--", file])
                .output()
                .await;
        }
        return ToolResult::error(format!(
            "Patch applied but syntax check failed — reverted.\n\n{}",
            lint_errors.join("\n\n")
        ));
    }

    // Show which files were modified.
    for file in files {
        msg.push_str(&format!("\n  - {}", file));

        // Record file change for diff viewer.
        let abs_path = work_dir.join(file);
        if let Ok(new_content) = tokio::fs::read(&abs_path).await {
            ctx.record_file_change(abs_path, &[], &new_content, "Patch");
        }
    }

    // Show verbose output if any.
    if !result.stdout.is_empty() {
        msg.push_str(&format!("\n\n{}", result.stdout.trim()));
    }

    // LSP diagnostics for modified files.
    {
        let lsp = cc_core::lsp::global_lsp_manager();
        let mut mgr = lsp.lock().await;
        for file in files {
            let abs_path = work_dir.join(file).to_string_lossy().to_string();
            if let Ok(content) = tokio::fs::read_to_string(work_dir.join(file)).await {
                let _ = mgr.open_file(&abs_path, work_dir).await;
                let _ = mgr.notify_file_changed(&abs_path, &content).await;
                let _ = mgr.notify_file_saved(&abs_path).await;
            }
        }
        drop(mgr);
        // Brief wait for diagnostics.
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        let mgr = lsp.lock().await;
        let mut diag_count = 0;
        for file in files {
            let abs_path = work_dir.join(file).to_string_lossy().to_string();
            let diags = mgr.get_diagnostics_for_file(&abs_path);
            if !diags.is_empty() && diag_count == 0 {
                msg.push_str("\n\nLSP Diagnostics:");
            }
            for d in &diags {
                msg.push_str(&format!(
                    "\n  {:?} {}:{}:{} — {}",
                    d.severity, d.file, d.line, d.column, d.message
                ));
                diag_count += 1;
            }
        }
    }

    ToolResult::success(msg).with_metadata(json!({
        "files": files,
        "method": method,
    }))
}

// ---------------------------------------------------------------------------
// Diff parser
// ---------------------------------------------------------------------------

/// Normalize a unified diff to fix common LLM formatting mistakes:
/// - Ensure trailing newline (git requires it)
/// - Ensure context lines start with a space (LLMs sometimes strip it)
/// - Fix \r\n → \n
fn normalize_diff(raw: &str) -> String {
    let mut lines: Vec<String> = Vec::new();
    let mut in_hunk = false;

    for line in raw.replace("\r\n", "\n").lines() {
        if line.starts_with("@@") {
            in_hunk = true;
            lines.push(line.to_string());
        } else if line.starts_with("---") || line.starts_with("+++") {
            in_hunk = false;
            lines.push(line.to_string());
        } else if line.starts_with('+') || line.starts_with('-') || line.starts_with(' ') {
            lines.push(line.to_string());
        } else if in_hunk && !line.is_empty() {
            // Context line missing leading space — add it.
            // This is the most common LLM mistake in diff generation.
            lines.push(format!(" {}", line));
        } else if in_hunk && line.is_empty() {
            // Empty line in a hunk = context line (space only).
            lines.push(" ".to_string());
        } else {
            lines.push(line.to_string());
        }
    }

    let mut result = lines.join("\n");
    if !result.ends_with('\n') {
        result.push('\n');
    }
    result
}

/// Extract file paths from a unified diff.
/// Looks for `+++ b/path/to/file` lines.
fn extract_files_from_diff(diff: &str) -> Vec<String> {
    let mut files = Vec::new();
    for line in diff.lines() {
        if let Some(path) = line.strip_prefix("+++ b/") {
            let path = path.trim();
            if !path.is_empty() && path != "/dev/null" {
                files.push(path.to_string());
            }
        }
    }
    files.dedup();
    files
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_files_single() {
        let diff = "\
--- a/src/main.py
+++ b/src/main.py
@@ -1,3 +1,4 @@
 line1
+line2
 line3";
        let files = extract_files_from_diff(diff);
        assert_eq!(files, vec!["src/main.py"]);
    }

    #[test]
    fn test_extract_files_multi() {
        let diff = "\
--- a/foo.rs
+++ b/foo.rs
@@ -1 +1 @@
-old
+new
--- a/bar.rs
+++ b/bar.rs
@@ -1 +1 @@
-old
+new";
        let files = extract_files_from_diff(diff);
        assert_eq!(files, vec!["foo.rs", "bar.rs"]);
    }

    #[test]
    fn test_extract_files_dev_null_ignored() {
        let diff = "\
--- /dev/null
+++ b/new_file.py
@@ -0,0 +1 @@
+content";
        let files = extract_files_from_diff(diff);
        assert_eq!(files, vec!["new_file.py"]);
    }

    #[test]
    fn test_extract_files_empty_diff() {
        assert!(extract_files_from_diff("").is_empty());
        assert!(extract_files_from_diff("not a diff").is_empty());
    }
}
