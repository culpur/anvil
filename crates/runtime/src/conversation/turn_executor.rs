//! Turn execution logic: drives the API request/tool-use loop for a single turn.

use crate::session::{ContentBlock, ConversationMessage};
use crate::usage::{TokenUsage, UsageTracker};

use super::{
    ApiClient, ApiRequest, AssistantEvent, RuntimeError, ToolExecutor, TurnSummary,
};
use super::permission_gate::evaluate_and_execute;
use super::usage_tracking::collect_and_record;
use crate::hooks::HookRunner;
use crate::permissions::{PermissionPolicy, PermissionPrompter};
use crate::session::Session;

/// Run the inner agentic loop for one turn.
///
/// Loops until the assistant emits a stop event with no pending tool uses,
/// or until `max_iterations` is exhausted.
#[allow(clippy::too_many_arguments)]
pub(super) fn run_turn_inner<C: ApiClient, T: ToolExecutor>(
    session: &mut Session,
    api_client: &mut C,
    tool_executor: &mut T,
    permission_policy: &PermissionPolicy,
    system_prompt: &[String],
    max_iterations: usize,
    usage_tracker: &mut UsageTracker,
    hook_runner: &HookRunner,
    prompter: &mut Option<&mut dyn PermissionPrompter>,
) -> Result<TurnSummary, RuntimeError> {
    let mut assistant_messages = Vec::new();
    let mut tool_results = Vec::new();
    let mut iterations = 0;

    loop {
        iterations += 1;
        if iterations > max_iterations {
            return Err(RuntimeError::new(
                "conversation loop exceeded the maximum number of iterations",
            ));
        }

        let request = ApiRequest {
            system_prompt: system_prompt.to_vec(),
            messages: session.messages.clone(),
        };
        let events = api_client.stream(request)?;
        let (assistant_message, usage) = build_assistant_message(events)?;
        if let Some(u) = usage {
            collect_and_record(usage_tracker, u);
        }

        let pending_tool_uses = assistant_message
            .blocks
            .iter()
            .filter_map(|block| match block {
                ContentBlock::ToolUse { id, name, input } => {
                    Some((id.clone(), name.clone(), input.clone()))
                }
                _ => None,
            })
            .collect::<Vec<_>>();

        session.messages.push(assistant_message.clone());
        assistant_messages.push(assistant_message);

        if pending_tool_uses.is_empty() {
            break;
        }

        for (tool_use_id, tool_name, input) in pending_tool_uses {
            let result_message = evaluate_and_execute(
                tool_use_id,
                tool_name,
                input,
                permission_policy,
                prompter,
                hook_runner,
                tool_executor,
            );
            session.messages.push(result_message.clone());
            tool_results.push(result_message);
        }
    }

    Ok(TurnSummary {
        assistant_messages,
        tool_results,
        iterations,
        usage: usage_tracker.cumulative_usage(),
    })
}

/// Build a `ConversationMessage` from a stream of `AssistantEvent`s.
pub(super) fn build_assistant_message(
    events: Vec<AssistantEvent>,
) -> Result<(ConversationMessage, Option<TokenUsage>), RuntimeError> {
    let mut text = String::new();
    let mut blocks = Vec::new();
    let mut finished = false;
    let mut usage = None;

    for event in events {
        match event {
            AssistantEvent::TextDelta(delta) => text.push_str(&delta),
            AssistantEvent::ToolUse { id, name, input } => {
                flush_text_block(&mut text, &mut blocks);
                blocks.push(ContentBlock::ToolUse { id, name, input });
            }
            AssistantEvent::Usage(value) => usage = Some(value),
            AssistantEvent::MessageStop => {
                finished = true;
            }
        }
    }

    flush_text_block(&mut text, &mut blocks);

    if !finished {
        return Err(RuntimeError::new(
            "assistant stream ended without a message stop event",
        ));
    }
    if blocks.is_empty() {
        return Err(RuntimeError::new("assistant stream produced no content"));
    }

    Ok((
        ConversationMessage::assistant_with_usage(blocks, usage),
        usage,
    ))
}

fn flush_text_block(text: &mut String, blocks: &mut Vec<ContentBlock>) {
    if !text.is_empty() {
        blocks.push(ContentBlock::Text {
            text: std::mem::take(text),
        });
    }
}
