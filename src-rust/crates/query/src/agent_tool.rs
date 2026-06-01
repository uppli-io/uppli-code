// AgentTool: spawn a sub-agent to handle a complex sub-task.
//
// Lives in cc-query (not cc-tools) to avoid a circular dependency:
//   cc-tools would need cc-query, but cc-query already needs cc-tools.
//
// The AgentTool creates a nested query loop with its own context, enabling
// the model to delegate complex work to specialized sub-agents. Each sub-agent:
//   - Runs its own agentic loop
//   - Has access to all tools (except AgentTool itself, preventing infinite recursion)
//   - Returns its final output as the tool result

use async_trait::async_trait;
use cc_api::LlmProvider;
use cc_core::types::Message;
use cc_tools::{PermissionLevel, Tool, ToolContext, ToolResult};
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info};

use crate::{global_provider, run_query_loop, QueryConfig, QueryOutcome};

pub struct AgentTool;

#[derive(Debug, Deserialize)]
struct AgentInput {
    /// Short description of the agent's task (used for logging).
    description: String,
    /// The complete task prompt to send as the first user message.
    prompt: String,
    /// Optional: which tools to make available (defaults to all minus AgentTool).
    #[serde(default)]
    tools: Option<Vec<String>>,
    /// Optional: system prompt override for the sub-agent.
    #[serde(default)]
    system_prompt: Option<String>,
    /// Optional: max turns for the sub-agent (default: inherits from parent).
    #[serde(default)]
    max_turns: Option<u32>,
    /// Optional: model override for this sub-agent.
    #[serde(default)]
    model: Option<String>,
}

#[async_trait]
impl Tool for AgentTool {
    fn name(&self) -> &str {
        cc_core::constants::TOOL_NAME_AGENT
    }

    fn description(&self) -> &str {
        "Launch a new agent to handle complex, multi-step tasks autonomously. \
         The agent runs its own agentic loop with access to tools and returns \
         its final result. Use this to delegate sub-tasks, run parallel \
         workstreams, or handle tasks that require many tool calls."
    }

    fn permission_level(&self) -> PermissionLevel {
        // The agent inherits parent permissions; no extra level required.
        PermissionLevel::None
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "description": {
                    "type": "string",
                    "description": "Short description of the agent's task (3-5 words)"
                },
                "prompt": {
                    "type": "string",
                    "description": "The complete task for the agent to perform"
                },
                "tools": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "List of tool names to make available. Defaults to all tools."
                },
                "system_prompt": {
                    "type": "string",
                    "description": "Optional system prompt override for the sub-agent"
                },
                "max_turns": {
                    "type": "number",
                    "description": "Maximum number of turns for the sub-agent (default: inherits from parent)"
                },
                "model": {
                    "type": "string",
                    "description": "Optional model to use for this agent"
                }
            },
            "required": ["description", "prompt"]
        })
    }

    async fn execute(&self, input: Value, ctx: &ToolContext) -> ToolResult {
        let params: AgentInput = match serde_json::from_value(input) {
            Ok(p) => p,
            Err(e) => return ToolResult::error(format!("Invalid input: {}", e)),
        };

        info!(description = %params.description, "Spawning sub-agent");

        // Reuse the parent's provider — sub-agents should talk to the same
        // LLM endpoint with the same auth, not hardcode a specific provider.
        let client: Arc<dyn LlmProvider> = match global_provider() {
            Some(p) => Arc::clone(p),
            None => {
                return ToolResult::error(
                    "No LLM provider configured — cannot spawn sub-agent".to_string(),
                )
            }
        };

        // Build the tool list for the sub-agent.
        // Always exclude AgentTool itself to prevent unbounded recursion.
        let all = cc_tools::all_tools();
        let agent_tools: Vec<Box<dyn Tool>> = if let Some(ref allowed) = params.tools {
            all.into_iter()
                .filter(|t| allowed.contains(&t.name().to_string()))
                .collect()
        } else {
            all.into_iter()
                .filter(|t| t.name() != cc_core::constants::TOOL_NAME_AGENT)
                .collect()
        };

        // Resolve model: explicit override > provider default.
        let caps = client.capabilities();
        let model = params
            .model
            .filter(|m| !m.is_empty())
            .unwrap_or_else(|| caps.default_model.clone());

        let system_prompt = params.system_prompt.unwrap_or_else(|| {
            // Build the default system prompt, optionally augmented with
            // agent definitions contributed by installed plugins.
            let mut prompt = "You are a specialized AI agent helping with a specific sub-task. \
             Complete the task thoroughly and return your findings."
                .to_string();

            // Append plugin-contributed agent definitions so the sub-agent
            // is aware of any specialised agents declared by plugins.
            if let Some(registry) = cc_plugins::global_plugin_registry() {
                let mut agent_defs = String::new();
                for agent_dir in registry.all_agent_paths() {
                    if let Ok(entries) = std::fs::read_dir(&agent_dir) {
                        for entry in entries.flatten() {
                            let p = entry.path();
                            if p.extension().is_some_and(|e| e == "md") {
                                if let Ok(content) = std::fs::read_to_string(&p) {
                                    let name =
                                        p.file_stem().and_then(|s| s.to_str()).unwrap_or("agent");
                                    agent_defs.push_str(&format!(
                                        "\n\n## Agent: {}\n{}",
                                        name,
                                        content.trim()
                                    ));
                                }
                            }
                        }
                    }
                }
                if !agent_defs.is_empty() {
                    prompt.push_str("\n\nThe following specialized agents are available:");
                    prompt.push_str(&agent_defs);
                }
            }

            prompt
        });

        let query_config = QueryConfig {
            model,
            max_tokens: caps.default_max_tokens,
            max_turns: params
                .max_turns
                .unwrap_or(cc_core::constants::MAX_TURNS_DEFAULT),
            system_prompt: Some(system_prompt),
            append_system_prompt: None,
            output_style: ctx.config.effective_output_style(),
            output_style_prompt: ctx.config.resolve_output_style_prompt(),
            working_directory: Some(ctx.working_dir.display().to_string()),
            thinking_budget: caps.default_thinking_budget,
            temperature: None,
            tool_result_budget: cc_core::constants::DEFAULT_TOOL_RESULT_BUDGET,
            effort_level: Some(cc_core::effort::EffortLevel::High),
            command_queue: None,
            skill_index: None,
            max_total_tokens: None,
            max_budget_usd: None,
            fallback_model: caps.fast_model.clone(),
        };

        // Run the sub-agent loop.
        let mut messages = vec![Message::user(params.prompt)];
        let cancel = CancellationToken::new();

        let outcome = run_query_loop(
            client.as_ref(),
            &mut messages,
            &agent_tools,
            ctx,
            &query_config,
            ctx.cost_tracker.clone(),
            None, // no event forwarding for sub-agents
            cancel,
            None, // no pending message queue for sub-agents
        )
        .await;

        match outcome {
            QueryOutcome::EndTurn { message, usage } => {
                let text = message.get_all_text();
                debug!(
                    description = %params.description,
                    output_tokens = usage.output_tokens,
                    "Sub-agent completed"
                );
                ToolResult::success(text)
            }
            QueryOutcome::MaxTokens {
                partial_message, ..
            } => {
                let text = partial_message.get_all_text();
                ToolResult::success(format!("{}\n\n[Note: Agent hit max_tokens limit]", text))
            }
            QueryOutcome::Cancelled => ToolResult::error("Sub-agent was cancelled".to_string()),
            QueryOutcome::Error(e) => ToolResult::error(format!("Sub-agent error: {}", e)),
            QueryOutcome::BudgetExceeded {
                spent_tokens,
                spent_cost_usd,
                limit_tokens,
                limit_cost_usd,
                trigger,
            } => {
                let detail = match trigger {
                    crate::BudgetTrigger::Tokens => format!(
                        "token budget exceeded ({} of {} tokens)",
                        spent_tokens,
                        limit_tokens.unwrap_or(0)
                    ),
                    crate::BudgetTrigger::CostUsd => format!(
                        "USD budget exceeded (${:.4} of ${:.4})",
                        spent_cost_usd,
                        limit_cost_usd.unwrap_or(0.0)
                    ),
                };
                ToolResult::error(format!("Sub-agent stopped: {}", detail))
            }
        }
    }
}
