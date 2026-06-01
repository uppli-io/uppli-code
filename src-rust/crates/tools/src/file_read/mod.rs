// FileRead tool: read files with optional line range, image support, PDF page ranges.
//
// PR B (in progress): split into format-specific submodules so every file
// type (text / image / PDF / OOXML / ODF / archive / structured) reaches the
// LLM properly. PR A landed the multimodal infrastructure (ToolResult.blocks
// + provider dispatch); PR B plugs the file ingestion side.
//
// Commit 1: pure relocation + empty submodule skeleton. Behaviour unchanged.
//           Subsequent commits add limits, dispatch, then real handlers.

mod archive;
mod caption;
mod detect;
mod image;
mod legacy_office;
mod limits;
mod odf;
mod ooxml;
mod output;
mod pdf;
mod structured;
mod tabular;
mod text;

use crate::{PermissionLevel, Tool, ToolContext, ToolResult};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};
use tracing::debug;

pub struct FileReadTool;

#[derive(Debug, Deserialize)]
struct FileReadInput {
    file_path: String,
    #[serde(default)]
    offset: Option<usize>,
    #[serde(default)]
    limit: Option<usize>,
}

#[async_trait]
impl Tool for FileReadTool {
    fn name(&self) -> &str {
        cc_core::constants::TOOL_NAME_FILE_READ
    }

    fn description(&self) -> &str {
        "Reads a file from the local filesystem. You can access any file directly. \
         By default reads up to 2000 lines from the beginning. Results are returned \
         with line numbers starting at 1. This tool can read images (PNG, JPG) and \
         PDF files."
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
                    "description": "The absolute path to the file to read"
                },
                "offset": {
                    "type": "number",
                    "description": "The line number to start reading from (1-based). Only provide if the file is too large to read at once."
                },
                "limit": {
                    "type": "number",
                    "description": "The number of lines to read. Only provide if the file is too large to read at once."
                }
            },
            "required": ["file_path"]
        })
    }

    async fn execute(&self, input: Value, ctx: &ToolContext) -> ToolResult {
        let params: FileReadInput = match serde_json::from_value(input) {
            Ok(p) => p,
            Err(e) => return ToolResult::error(format!("Invalid input: {}", e)),
        };

        let path = ctx.resolve_path(&params.file_path);
        debug!(path = %path.display(), "Reading file");

        // Check if file exists
        if !path.exists() {
            return ToolResult::error(format!("File not found: {}", path.display()));
        }

        // Check if it's a directory
        if path.is_dir() {
            return ToolResult::error(format!(
                "{} is a directory, not a file. Use Bash with `ls` to list directory contents.",
                path.display()
            ));
        }

        // Pre-flight size cap — the most important fix in PR B. The legacy
        // code path slurped the whole file into memory via read_to_string
        // with no guard, so a multi-GB log file would OOM the agent.
        // Bound the maximum size BEFORE any byte is read.
        match tokio::fs::metadata(&path).await {
            Ok(meta) if meta.len() > limits::MAX_FILE_BYTES => {
                return ToolResult::error(format!(
                    "[File too large: {} = {} > {} cap. \
                     Use Bash with head/tail/sed to read a slice, or split the file.]",
                    path.display(),
                    limits::human_bytes(meta.len()),
                    limits::human_bytes(limits::MAX_FILE_BYTES),
                ));
            }
            Ok(_) => {}
            Err(e) => {
                return ToolResult::error(format!(
                    "Failed to read file metadata for {}: {}",
                    path.display(),
                    e
                ));
            }
        }

        // Detect binary / image files by extension
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();

        let image_exts = ["png", "jpg", "jpeg", "gif", "bmp", "webp", "svg", "ico"];
        if image_exts.contains(&ext.as_str()) {
            return ToolResult::success(format!(
                "[Image file: {}. The image content has been captured for visual analysis.]",
                path.display()
            ));
        }

        if ext == "pdf" {
            return ToolResult::success(format!(
                "[PDF file: {}. Use the `pages` parameter to read specific page ranges.]",
                path.display()
            ));
        }

        // Read text file
        let content = match tokio::fs::read_to_string(&path).await {
            Ok(c) => c,
            Err(e) => {
                // Might be binary
                if e.kind() == std::io::ErrorKind::InvalidData {
                    return ToolResult::error(format!(
                        "File appears to be binary and cannot be displayed as text: {}",
                        path.display()
                    ));
                }
                return ToolResult::error(format!("Failed to read file: {}", e));
            }
        };

        if content.is_empty() {
            return ToolResult::success(format!("[File {} exists but is empty]", path.display()));
        }

        let lines: Vec<&str> = content.lines().collect();
        let total_lines = lines.len();

        let offset = params.offset.unwrap_or(0);
        let limit = params.limit.unwrap_or(2000);

        // Convert 1-based offset to 0-based index
        let start = if offset > 0 { offset - 1 } else { 0 };
        let end = (start + limit).min(total_lines);

        if start >= total_lines {
            return ToolResult::error(format!(
                "Offset {} exceeds total line count {} in {}",
                offset,
                total_lines,
                path.display()
            ));
        }

        let mut output = String::new();
        let width = format!("{}", end).len();

        for (i, line) in lines[start..end].iter().enumerate() {
            let line_num = start + i + 1;
            output.push_str(&format!("{:>width$}\t{}\n", line_num, line, width = width));
        }

        if end < total_lines {
            output.push_str(&format!(
                "\n... ({} more lines, {} total. Use offset/limit to read more.)\n",
                total_lines - end,
                total_lines
            ));
        }

        ToolResult::success(output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cc_core::config::Config;
    use cc_core::permissions::AutoPermissionHandler;
    use std::path::PathBuf;
    use std::sync::atomic::AtomicUsize;
    use std::sync::Arc;
    use tempfile::TempDir;

    fn test_ctx(working_dir: PathBuf) -> ToolContext {
        let handler = Arc::new(AutoPermissionHandler {
            mode: cc_core::config::PermissionMode::Default,
        });
        ToolContext {
            working_dir,
            permission_mode: cc_core::config::PermissionMode::Default,
            permission_handler: handler,
            cost_tracker: cc_core::cost::CostTracker::new(),
            session_id: "test".to_string(),
            file_history: Arc::new(parking_lot::Mutex::new(
                cc_core::file_history::FileHistory::new(),
            )),
            current_turn: Arc::new(AtomicUsize::new(0)),
            non_interactive: true,
            mcp_manager: None,
            config: Config::default(),
        }
    }

    #[tokio::test]
    async fn preflight_rejects_file_larger_than_cap() {
        // Create a sparse file just over MAX_FILE_BYTES so the cap fires.
        // `seek` + 1-byte write is enough — metadata().len() reads the
        // declared length, not the on-disk allocation, so the test
        // doesn't actually write 100 MiB.
        let tmp = TempDir::new().expect("tempdir");
        let path = tmp.path().join("huge.txt");
        let file = std::fs::File::create(&path).expect("create");
        use std::io::{Seek, SeekFrom, Write};
        let mut f = file;
        f.seek(SeekFrom::Start(limits::MAX_FILE_BYTES + 1))
            .expect("seek");
        f.write_all(b"x").expect("write 1 byte at the end");
        drop(f);

        let ctx = test_ctx(tmp.path().to_path_buf());
        let tool = FileReadTool;
        let input = json!({ "file_path": path.to_str().unwrap() });
        let result = tool.execute(input, &ctx).await;
        assert!(result.is_error, "huge file must be refused");
        assert!(
            result.content.contains("File too large"),
            "error must mention size cap, got: {}",
            result.content
        );
    }

    #[tokio::test]
    async fn preflight_passes_small_file() {
        let tmp = TempDir::new().expect("tempdir");
        let path = tmp.path().join("small.txt");
        std::fs::write(&path, "line one\nline two\n").expect("write");

        let ctx = test_ctx(tmp.path().to_path_buf());
        let tool = FileReadTool;
        let input = json!({ "file_path": path.to_str().unwrap() });
        let result = tool.execute(input, &ctx).await;
        assert!(!result.is_error, "small file must pass: {}", result.content);
        assert!(result.content.contains("line one"));
        assert!(result.content.contains("line two"));
    }
}
