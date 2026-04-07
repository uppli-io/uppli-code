// TodoWrite tool: task / todo list management.

use crate::{PermissionLevel, Tool, ToolContext, ToolResult};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};
use tracing::debug;

pub struct TodoWriteTool;

#[derive(Debug, Deserialize)]
struct TodoWriteInput {
    todos: Vec<TodoItem>,
}

#[derive(Debug, Clone, Deserialize)]
struct TodoItem {
    id: String,
    content: String,
    status: TodoStatus,
    #[serde(default)]
    #[allow(dead_code)]
    priority: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
enum TodoStatus {
    Pending,
    InProgress,
    Completed,
}

impl std::fmt::Display for TodoStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TodoStatus::Pending => write!(f, "pending"),
            TodoStatus::InProgress => write!(f, "in_progress"),
            TodoStatus::Completed => write!(f, "completed"),
        }
    }
}

#[async_trait]
impl Tool for TodoWriteTool {
    fn name(&self) -> &str {
        cc_core::constants::TOOL_NAME_TODO_WRITE
    }

    fn description(&self) -> &str {
        "Write and manage a todo/task list. Provide the complete list of todos \
         each time (this replaces the entire list). Use this to track progress \
         on multi-step tasks."
    }

    fn permission_level(&self) -> PermissionLevel {
        PermissionLevel::None
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "todos": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "id": { "type": "string" },
                            "content": { "type": "string" },
                            "status": {
                                "type": "string",
                                "enum": ["pending", "in_progress", "completed"]
                            },
                            "priority": { "type": "string" }
                        },
                        "required": ["id", "content", "status"]
                    },
                    "description": "The complete list of todo items"
                }
            },
            "required": ["todos"]
        })
    }

    async fn execute(&self, input: Value, _ctx: &ToolContext) -> ToolResult {
        let params: TodoWriteInput = match serde_json::from_value(input) {
            Ok(p) => p,
            Err(e) => return ToolResult::error(format!("Invalid input: {}", e)),
        };

        debug!(count = params.todos.len(), "Writing todo list");

        let total = params.todos.len();
        let completed = params
            .todos
            .iter()
            .filter(|t| matches!(t.status, TodoStatus::Completed))
            .count();
        let in_progress = params
            .todos
            .iter()
            .filter(|t| matches!(t.status, TodoStatus::InProgress))
            .count();
        let pending = total - completed - in_progress;

        let mut output = format!(
            "Todo list updated ({} total: {} pending, {} in progress, {} completed)\n\n",
            total, pending, in_progress, completed
        );

        for item in &params.todos {
            let icon = match item.status {
                TodoStatus::Pending => "[ ]",
                TodoStatus::InProgress => "[~]",
                TodoStatus::Completed => "[x]",
            };
            output.push_str(&format!("{} {} ({})\n", icon, item.content, item.id));
        }

        ToolResult::success(output).with_metadata(json!({
            "total": total,
            "completed": completed,
            "in_progress": in_progress,
            "pending": pending,
        }))
    }
}
