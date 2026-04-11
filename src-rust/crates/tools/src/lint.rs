// Post-edit syntax validation.
//
// Runs a fast, syntax-only check after file modifications to catch obvious
// errors before the model moves on.  Inspired by SWE-agent's integrated
// linting which improved SWE-bench pass rates by 3%.
//
// Design decisions:
//   - Only syntax checking, no style/lint rules (fast, no false positives).
//   - Timeout of 5 seconds per file (never blocks the agent).
//   - Unknown file types pass silently (no blocking edits to .md, .yaml, etc.).
//   - Returns structured output so the caller can revert and show the error.

use std::path::Path;
use std::time::Duration;
use tracing::debug;

/// Result of a syntax check on a single file.
pub struct LintResult {
    /// True if the file has valid syntax (or if we can't check it).
    pub ok: bool,
    /// The language we detected (None if unknown / not linted).
    pub language: Option<&'static str>,
    /// Compiler/interpreter error output (empty if ok).
    pub errors: String,
}

/// Run a fast syntax-only check on the given file.
///
/// Returns `LintResult { ok: true, .. }` for file types we don't know how
/// to check, so this never blocks edits to unknown languages.
pub async fn check_syntax(path: &Path) -> LintResult {
    let ext = match path.extension().and_then(|e| e.to_str()) {
        Some(e) => e,
        None => return pass(None),
    };

    let path_str = path.to_string_lossy().to_string();

    let (language, program, args): (&str, &str, Vec<&str>) = match ext {
        "py" => ("Python", "python3", vec!["-m", "py_compile", &path_str]),
        "rb" => ("Ruby", "ruby", vec!["-c", &path_str]),
        "sh" | "bash" => ("Shell", "bash", vec!["-n", &path_str]),
        "js" => ("JavaScript", "node", vec!["--check", &path_str]),
        "pl" | "pm" => ("Perl", "perl", vec!["-c", &path_str]),
        _ => return pass(None),
    };

    debug!(language, path = %path.display(), "Running syntax check");

    // Spawn the checker with a timeout.  If the binary isn't installed
    // or the check times out, we pass silently — never block the agent.
    let output = match tokio::time::timeout(
        Duration::from_secs(5),
        tokio::process::Command::new(program)
            .args(&args)
            .output(),
    )
    .await
    {
        Ok(Ok(output)) => output,
        Ok(Err(_)) => {
            // Binary not found or spawn error — pass silently.
            debug!(program, "Lint binary not available, skipping");
            return pass(Some(language));
        }
        Err(_) => {
            // Timeout — pass silently.
            debug!(program, "Lint timed out after 5s, skipping");
            return pass(Some(language));
        }
    };

    if output.status.success() {
        // Clean up .pyc files that py_compile creates.
        if ext == "py" {
            let _ = cleanup_pycache(path).await;
        }
        pass(Some(language))
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        // Some checkers output to stdout (ruby -c), others to stderr (python).
        let errors = if stderr.trim().is_empty() {
            stdout
        } else {
            stderr
        };
        LintResult {
            ok: false,
            language: Some(language),
            errors: errors.trim().to_string(),
        }
    }
}

/// Format a lint failure as a human-readable message for the model.
pub fn format_lint_error(result: &LintResult, path: &Path) -> String {
    let lang = result.language.unwrap_or("Unknown");
    format!(
        "Syntax error in {} ({}):\n{}\n\n\
         The edit was reverted. Fix the syntax and try again.",
        path.display(),
        lang,
        result.errors,
    )
}

fn pass(language: Option<&'static str>) -> LintResult {
    LintResult {
        ok: true,
        language,
        errors: String::new(),
    }
}

/// Remove __pycache__ directory that py_compile creates as a side effect.
async fn cleanup_pycache(path: &Path) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        let pycache = parent.join("__pycache__");
        if pycache.exists() {
            tokio::fs::remove_dir_all(&pycache).await?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tempfile::NamedTempFile;

    #[tokio::test]
    async fn valid_python_passes() {
        let tmp = NamedTempFile::with_suffix(".py").unwrap();
        std::fs::write(tmp.path(), "x = 1 + 2\nprint(x)\n").unwrap();
        let result = check_syntax(tmp.path()).await;
        assert!(result.ok);
        assert_eq!(result.language, Some("Python"));
    }

    #[tokio::test]
    async fn invalid_python_fails() {
        let tmp = NamedTempFile::with_suffix(".py").unwrap();
        std::fs::write(tmp.path(), "def foo(\n    x = 1\n").unwrap();
        let result = check_syntax(tmp.path()).await;
        assert!(!result.ok);
        assert_eq!(result.language, Some("Python"));
        assert!(!result.errors.is_empty());
    }

    #[tokio::test]
    async fn unknown_extension_passes() {
        let result = check_syntax(Path::new("/tmp/file.xyz")).await;
        assert!(result.ok);
        assert!(result.language.is_none());
    }

    #[tokio::test]
    async fn valid_shell_passes() {
        let tmp = NamedTempFile::with_suffix(".sh").unwrap();
        std::fs::write(tmp.path(), "#!/bin/bash\necho hello\n").unwrap();
        let result = check_syntax(tmp.path()).await;
        assert!(result.ok);
        assert_eq!(result.language, Some("Shell"));
    }

    #[tokio::test]
    async fn invalid_shell_fails() {
        let tmp = NamedTempFile::with_suffix(".sh").unwrap();
        std::fs::write(tmp.path(), "if [\n").unwrap();
        let result = check_syntax(tmp.path()).await;
        assert!(!result.ok);
        assert_eq!(result.language, Some("Shell"));
    }
}
