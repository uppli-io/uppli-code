// PowerShell tool: execute PowerShell commands (Windows-native).
//
// On Windows, PowerShell provides richer scripting than cmd.exe.
// On non-Windows platforms, attempts to use `pwsh` (PowerShell Core).

use crate::{PermissionLevel, Tool, ToolContext, ToolResult};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tracing::debug;

pub struct PowerShellTool;

#[derive(Debug, Deserialize)]
struct PowerShellInput {
    command: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default = "default_timeout")]
    timeout: u64,
}

fn default_timeout() -> u64 {
    120_000
}

#[async_trait]
impl Tool for PowerShellTool {
    fn name(&self) -> &str {
        "PowerShell"
    }

    fn description(&self) -> &str {
        "Execute a PowerShell command. Use for Windows-native operations, .NET APIs, \
         registry access, and Windows-specific system administration."
    }

    fn permission_level(&self) -> PermissionLevel {
        PermissionLevel::Execute
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "command": { "type": "string", "description": "The PowerShell command to execute" },
                "description": { "type": "string", "description": "Description of what this command does" },
                "timeout": { "type": "number", "description": "Timeout in ms (default 120000)" }
            },
            "required": ["command"]
        })
    }

    async fn execute(&self, input: Value, ctx: &ToolContext) -> ToolResult {
        let params: PowerShellInput = match serde_json::from_value(input) {
            Ok(p) => p,
            Err(e) => return ToolResult::error(format!("Invalid input: {}", e)),
        };

        let desc = params.description.as_deref().unwrap_or(&params.command);
        if let Err(e) = ctx.check_permission(self.name(), desc, false) {
            return ToolResult::error(e.to_string());
        }

        // Determine the PowerShell executable
        let (exe, args) = if cfg!(windows) {
            (
                "powershell",
                vec!["-NoProfile", "-NonInteractive", "-Command"],
            )
        } else {
            // PowerShell Core on non-Windows
            ("pwsh", vec!["-NoProfile", "-NonInteractive", "-Command"])
        };

        debug!(command = %params.command, "Executing PowerShell command");

        let timeout_ms = params.timeout.min(600_000);
        let timeout_dur = Duration::from_millis(timeout_ms);

        let mut child = match Command::new(exe)
            .args(&args)
            .arg(&params.command)
            .current_dir(&ctx.working_dir)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .stdin(Stdio::null())
            .spawn()
        {
            Ok(c) => c,
            Err(e) => return ToolResult::error(format!("Failed to spawn PowerShell: {}", e)),
        };

        let stdout = child.stdout.take();
        let stderr = child.stderr.take();

        let result = tokio::time::timeout(timeout_dur, async {
            let mut stdout_lines = Vec::new();
            let mut stderr_lines = Vec::new();

            if let Some(out) = stdout {
                let mut lines = BufReader::new(out).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    stdout_lines.push(line);
                }
            }
            if let Some(err) = stderr {
                let mut lines = BufReader::new(err).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    stderr_lines.push(line);
                }
            }

            let status = child.wait().await;
            (stdout_lines, stderr_lines, status)
        })
        .await;

        match result {
            Ok((stdout_lines, stderr_lines, status)) => {
                let exit_code = status.map(|s| s.code().unwrap_or(-1)).unwrap_or(-1);
                let mut output = stdout_lines.join("\n");
                if !stderr_lines.is_empty() {
                    if !output.is_empty() {
                        output.push('\n');
                    }
                    output.push_str("STDERR:\n");
                    output.push_str(&stderr_lines.join("\n"));
                }
                if output.is_empty() {
                    output = "(no output)".to_string();
                }

                if exit_code != 0 {
                    ToolResult::error(format!(
                        "PowerShell exited with code {}\n{}",
                        exit_code, output
                    ))
                } else {
                    ToolResult::success(output)
                }
            }
            Err(_) => {
                let _ = child.kill().await;
                ToolResult::error(format!(
                    "PowerShell command timed out after {}ms",
                    timeout_ms
                ))
            }
        }
    }
}
