//! Binome (`--groom`) orchestrator — deterministic 3-step pipeline.
//!
//! # Flow
//!
//! 1. **Worker draft** — worker receives the user prompt, does the task
//!    (tools, edits, commands), produces a first answer.
//! 2. **Peer review** — peer (Plan mode, read-only) receives worker's
//!    draft + the original task, produces a critique.
//! 3. **Worker final** — worker receives the peer's critique and either
//!    incorporates it or explains why it disagrees, producing the final
//!    answer shown to the user.
//!
//! # Why this shape and not chat
//!
//! A free-form chat loop between worker and peer depends on the model
//! following a routing protocol (prefix, tool call, JSON, whatever) —
//! small local models don't do this reliably. This pipeline never asks
//! the model to *decide* to consult the peer: consultation is
//! mechanical, always happens exactly once, and costs exactly three
//! LLM calls. Deterministic by construction.
//!
//! # Stopping
//!
//! No stop detection. Always exactly three turns. If the peer has
//! nothing to say the worker sees that and moves on. Bounded cost,
//! no runaway.

use std::sync::Arc;

use cc_api::LlmProvider;
use cc_core::cost::CostTracker;
use cc_core::types::{ContentBlock, Message, MessageContent};
use cc_query::{run_query_loop, QueryConfig, QueryOutcome};
use cc_tools::{Tool, ToolContext};
use tokio_util::sync::CancellationToken;

/// Run the binome pipeline. Returns when the final worker turn
/// completes (or an error surfaces from a loop).
#[allow(clippy::too_many_arguments)]
pub async fn run_binome(
    client: Arc<dyn LlmProvider>,
    tools: Arc<Vec<Box<dyn Tool>>>,
    worker_tool_ctx: ToolContext,
    peer_tool_ctx: ToolContext,
    worker_config: QueryConfig,
    peer_config: QueryConfig,
    cost_tracker: Arc<CostTracker>,
    user_prompt: String,
) -> anyhow::Result<()> {
    let cancel = CancellationToken::new();

    // ---- Step 1: Worker draft -----------------------------------------
    let mut worker_msgs = vec![Message::user(user_prompt.clone())];
    let draft = run_one_turn(
        "worker/draft",
        client.as_ref(),
        &mut worker_msgs,
        tools.as_slice(),
        &worker_tool_ctx,
        &worker_config,
        cost_tracker.clone(),
        cancel.clone(),
    )
    .await?;

    eprintln!("── worker draft ─────────────────────────────");
    println!("{}", draft);

    // ---- Step 2: Peer review ------------------------------------------
    // Peer gets the original task and the worker's full draft, asked for
    // critique. Plan-mode ToolContext guarantees the peer cannot mutate
    // state even if the model hallucinates a tool call.
    let peer_seed = format!(
        "Original task:\n{task}\n\nWorker's draft reply:\n{draft}\n\n\
         Review the draft. Call out anything wrong, risky, or missing. \
         If the draft is fine, say so briefly.",
        task = user_prompt,
        draft = draft,
    );
    let mut peer_msgs = vec![Message::user(peer_seed)];
    let review = run_one_turn(
        "peer/review",
        client.as_ref(),
        &mut peer_msgs,
        tools.as_slice(),
        &peer_tool_ctx,
        &peer_config,
        cost_tracker.clone(),
        cancel.clone(),
    )
    .await?;

    eprintln!("── peer review ──────────────────────────────");
    println!("{}", review);

    // ---- Step 3: Worker final ------------------------------------------
    // Append the review as a user message on the worker's own history —
    // it keeps the full tool-call context from step 1 and can now refine.
    worker_msgs.push(Message::user(format!(
        "Peer reviewer (read-only, senior eng) says:\n{review}\n\n\
         Incorporate useful feedback, push back on anything you disagree \
         with, and produce the final answer for the user.",
        review = review,
    )));
    let final_answer = run_one_turn(
        "worker/final",
        client.as_ref(),
        &mut worker_msgs,
        tools.as_slice(),
        &worker_tool_ctx,
        &worker_config,
        cost_tracker.clone(),
        cancel.clone(),
    )
    .await?;

    eprintln!("── worker final ─────────────────────────────");
    println!("{}", final_answer);

    Ok(())
}

/// Drive one end_turn-to-end_turn run of the query loop and return the
/// concatenated text of the final assistant message.
#[allow(clippy::too_many_arguments)]
async fn run_one_turn(
    label: &str,
    client: &dyn LlmProvider,
    messages: &mut Vec<Message>,
    tools: &[Box<dyn Tool>],
    tool_ctx: &ToolContext,
    config: &QueryConfig,
    cost_tracker: Arc<CostTracker>,
    cancel: CancellationToken,
) -> anyhow::Result<String> {
    let outcome = run_query_loop(
        client,
        messages,
        tools,
        tool_ctx,
        config,
        cost_tracker,
        None,
        cancel,
        None,
    )
    .await;
    match outcome {
        QueryOutcome::EndTurn { message, .. }
        | QueryOutcome::MaxTokens {
            partial_message: message,
            ..
        } => Ok(extract_text(&message)),
        QueryOutcome::Cancelled => {
            anyhow::bail!("{label}: cancelled");
        }
        QueryOutcome::Error(e) => {
            anyhow::bail!("{label}: {e}");
        }
        QueryOutcome::BudgetExceeded {
            cost_usd,
            limit_usd,
        } => {
            anyhow::bail!("{label}: budget exceeded ${cost_usd:.4} > ${limit_usd:.4}");
        }
    }
}

/// Concatenate all text content blocks of a message into one string.
fn extract_text(msg: &Message) -> String {
    match &msg.content {
        MessageContent::Text(s) => s.clone(),
        MessageContent::Blocks(blocks) => {
            let mut out = String::new();
            for b in blocks {
                if let ContentBlock::Text { text } = b {
                    if !out.is_empty() {
                        out.push('\n');
                    }
                    out.push_str(text);
                }
            }
            out
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_text_handles_plain_string() {
        let m = Message::assistant("hello");
        assert_eq!(extract_text(&m), "hello");
    }

    #[test]
    fn extract_text_joins_text_blocks_and_skips_others() {
        let m = Message {
            role: cc_core::types::Role::Assistant,
            content: MessageContent::Blocks(vec![
                ContentBlock::Text {
                    text: "first".into(),
                },
                ContentBlock::ToolUse {
                    id: "t1".into(),
                    name: "Read".into(),
                    input: serde_json::json!({}),
                },
                ContentBlock::Text {
                    text: "second".into(),
                },
            ]),
            uuid: None,
            cost: None,
        };
        assert_eq!(extract_text(&m), "first\nsecond");
    }
}
