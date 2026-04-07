// Team tools: create and disband multi-agent swarm teams.
//
// TeamCreateTool – create a new team, writing its config to
//                  ~/.uppli/teams/{team_name}/config.json
// TeamDeleteTool – remove an existing team directory
//
// Tool names match TypeScript: "TeamCreate" and "TeamDelete".
//
// The Rust port does not implement the full swarm runtime (tmux panes,
// agent spawning, task relay, etc.) — it stores the team config on disk
// so that external tooling can read it, and returns structured output
// matching the TypeScript shape.

use crate::{PermissionLevel, Tool, ToolContext, ToolResult};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

fn teams_base_dir() -> Option<std::path::PathBuf> {
    dirs::home_dir().map(|h| h.join(".uppli").join("teams"))
}

fn team_dir(team_name: &str) -> Option<std::path::PathBuf> {
    teams_base_dir().map(|b| b.join(sanitize_name(team_name)))
}

/// Sanitize a team name to a safe directory component: keep alphanumerics,
/// hyphens, underscores; replace everything else with hyphens.
fn sanitize_name(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// On-disk schema
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize)]
struct TeamMember {
    agent_id: String,
    name: String,
    agent_type: String,
    joined_at: u64,
}

#[derive(Debug, Serialize, Deserialize)]
struct TeamConfig {
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    created_at: u64,
    lead_agent_id: String,
    lead_session_id: String,
    members: Vec<TeamMember>,
}

fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// TeamCreateTool
// ---------------------------------------------------------------------------

pub struct TeamCreateTool;

#[derive(Debug, Deserialize)]
struct TeamCreateInput {
    team_name: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    agent_type: Option<String>,
}

#[async_trait]
impl Tool for TeamCreateTool {
    fn name(&self) -> &str {
        "TeamCreate"
    }

    fn description(&self) -> &str {
        "Create a new team for coordinating multiple agents. \
         Writes the team configuration to ~/.uppli/teams/{team_name}/config.json \
         and initialises an empty task list. Returns the team name, config file path, \
         and the lead agent ID."
    }

    fn permission_level(&self) -> PermissionLevel {
        PermissionLevel::Write
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "team_name": {
                    "type": "string",
                    "description": "Name for the new team to create."
                },
                "description": {
                    "type": "string",
                    "description": "Optional team description / purpose."
                },
                "agent_type": {
                    "type": "string",
                    "description": "Type/role of the team lead (e.g. \"researcher\"). Defaults to \"team-lead\"."
                }
            },
            "required": ["team_name"]
        })
    }

    async fn execute(&self, input: Value, ctx: &ToolContext) -> ToolResult {
        let params: TeamCreateInput = match serde_json::from_value(input) {
            Ok(p) => p,
            Err(e) => return ToolResult::error(format!("Invalid input: {}", e)),
        };

        if params.team_name.trim().is_empty() {
            return ToolResult::error("team_name is required for TeamCreate".to_string());
        }

        let safe_name = sanitize_name(&params.team_name);
        let agent_type = params
            .agent_type
            .as_deref()
            .unwrap_or("team-lead")
            .to_string();

        // Derive a deterministic lead agent ID: "team-lead@{team_name}"
        let lead_agent_id = format!("team-lead@{}", safe_name);

        let dir = match team_dir(&params.team_name) {
            Some(d) => d,
            None => return ToolResult::error("Could not determine home directory".to_string()),
        };

        // Check if a team with this name already exists; if so, generate a
        // unique name by appending a short UUID suffix.
        let (final_name, final_dir) = if dir.exists() {
            let suffix = &Uuid::new_v4().to_string()[..6];
            let new_name = format!("{}-{}", safe_name, suffix);
            let new_dir = match team_dir(&new_name) {
                Some(d) => d,
                None => return ToolResult::error("Could not determine home directory".to_string()),
            };
            (new_name, new_dir)
        } else {
            (safe_name, dir)
        };

        // Create directory
        if let Err(e) = tokio::fs::create_dir_all(&final_dir).await {
            return ToolResult::error(format!("Failed to create team directory: {}", e));
        }

        let now = now_millis();

        let config = TeamConfig {
            name: final_name.clone(),
            description: params.description,
            created_at: now,
            lead_agent_id: lead_agent_id.clone(),
            lead_session_id: ctx.session_id.clone(),
            members: vec![TeamMember {
                agent_id: lead_agent_id.clone(),
                name: "team-lead".to_string(),
                agent_type,
                joined_at: now,
            }],
        };

        let config_json = match serde_json::to_string_pretty(&config) {
            Ok(j) => j,
            Err(e) => return ToolResult::error(format!("Serialisation error: {}", e)),
        };

        let config_path = final_dir.join("config.json");
        if let Err(e) = tokio::fs::write(&config_path, &config_json).await {
            return ToolResult::error(format!("Failed to write config.json: {}", e));
        }

        // Write empty tasks list
        let tasks_path = final_dir.join("tasks.json");
        if let Err(e) = tokio::fs::write(&tasks_path, "[]").await {
            return ToolResult::error(format!("Failed to write tasks.json: {}", e));
        }

        let team_file_path = config_path.to_string_lossy().to_string();

        ToolResult::success(
            serde_json::json!({
                "team_name": final_name,
                "team_file_path": team_file_path,
                "lead_agent_id": lead_agent_id
            })
            .to_string(),
        )
    }
}

// ---------------------------------------------------------------------------
// TeamDeleteTool
// ---------------------------------------------------------------------------

pub struct TeamDeleteTool;

#[derive(Debug, Deserialize)]
struct TeamDeleteInput {
    team_name: String,
}

#[async_trait]
impl Tool for TeamDeleteTool {
    fn name(&self) -> &str {
        "TeamDelete"
    }

    fn description(&self) -> &str {
        "Clean up team and task directories when the swarm is complete. \
         Removes ~/.uppli/teams/{team_name}/ and returns a confirmation. \
         The team name is taken from the input (not auto-detected from session context \
         in the Rust port)."
    }

    fn permission_level(&self) -> PermissionLevel {
        PermissionLevel::Write
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "team_name": {
                    "type": "string",
                    "description": "Name of the team to delete."
                }
            },
            "required": ["team_name"]
        })
    }

    async fn execute(&self, input: Value, _ctx: &ToolContext) -> ToolResult {
        let params: TeamDeleteInput = match serde_json::from_value(input) {
            Ok(p) => p,
            Err(e) => return ToolResult::error(format!("Invalid input: {}", e)),
        };

        if params.team_name.trim().is_empty() {
            return ToolResult::error("team_name is required for TeamDelete".to_string());
        }

        let dir = match team_dir(&params.team_name) {
            Some(d) => d,
            None => return ToolResult::error("Could not determine home directory".to_string()),
        };

        if !dir.exists() {
            return ToolResult::error(format!(
                "Team '{}' not found (no directory at {}).",
                params.team_name,
                dir.display()
            ));
        }

        if let Err(e) = tokio::fs::remove_dir_all(&dir).await {
            return ToolResult::error(format!(
                "Failed to remove team directory '{}': {}",
                dir.display(),
                e
            ));
        }

        ToolResult::success(
            serde_json::json!({
                "success": true,
                "message": format!("Cleaned up directories for team \"{}\"", params.team_name),
                "team_name": params.team_name
            })
            .to_string(),
        )
    }
}
