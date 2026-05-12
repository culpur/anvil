//! Turn execution logic: drives the API request/tool-use loop for a single turn.

use crate::session::{ContentBlock, ConversationMessage};
use crate::usage::{TokenUsage, UsageTracker};

use super::{
    ApiClient, ApiRequest, AssistantEvent, RuntimeError, ToolExecutor, TurnSummary,
};
use super::permission_gate::evaluate_and_execute;
use super::usage_tracking::collect_and_record;
use crate::auto_mode::AutoModeConfig;
use crate::hooks::{HookRunner, PostToolBatchPayload};
use crate::permissions::{PermissionPolicy, PermissionPrompter};
use crate::permissions::reviewer::Reviewer;
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
    reviewer: &Reviewer,
    auto_mode: &AutoModeConfig,
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

        // v2.2.11: track per-tool timing for PostToolBatch payload.
        let tool_count = pending_tool_uses.len();
        let mut durations_ms: Vec<u64> = Vec::with_capacity(tool_count);
        let mut success_count: usize = 0;
        let mut failure_count: usize = 0;

        for (tool_use_id, tool_name, input) in pending_tool_uses {
            // Emit tool_use event before execution (no-op when OTel is disabled).
            crate::otel::tool_use(&tool_name, &tool_use_id, "", "");

            let start = std::time::Instant::now();
            let result_message = evaluate_and_execute(
                tool_use_id.clone(),
                tool_name.clone(),
                &input,
                permission_policy,
                prompter,
                hook_runner,
                tool_executor,
                reviewer,
                auto_mode,
            );
            let elapsed_ms = start.elapsed().as_millis() as u64;
            durations_ms.push(elapsed_ms);

            // Determine success/failure from the result message.
            let is_err = result_message
                .blocks
                .iter()
                .any(|b| matches!(b, ContentBlock::ToolResult { is_error, .. } if *is_error));
            if is_err {
                failure_count += 1;
            } else {
                success_count += 1;
            }

            // Emit tool_result event (no-op when OTel is disabled).
            let output_size = result_message.blocks.iter()
                .find_map(|b| {
                    if let ContentBlock::ToolResult { output, .. } = b {
                        Some(output.len() as u64)
                    } else {
                        None
                    }
                })
                .unwrap_or(0);
            crate::otel::tool_result(&tool_name, &tool_use_id, !is_err, elapsed_ms, output_size);

            session.messages.push(result_message.clone());
            tool_results.push(result_message);
        }

        // v2.2.11: fire PostToolBatch once per completed tool batch.
        let _ = hook_runner.run_post_tool_batch(&PostToolBatchPayload {
            tool_count,
            durations_ms,
            success_count,
            failure_count,
        });
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

    // A properly-terminated stream with no content blocks means the model
    // emitted just a stop token. This happens legitimately after a tool-call
    // turn — Ollama and some OpenAI-compatible backends will answer a
    // successful tool_result with an empty message rather than a
    // follow-up paragraph. Treat it as "nothing more to say" and return an
    // empty-text assistant message so the caller can break the turn loop
    // cleanly. We keep the `!finished` guard above as the real error
    // signal for truncated/disconnected streams.
    if blocks.is_empty() {
        blocks.push(ContentBlock::Text {
            text: String::new(),
        });
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

#[cfg(test)]
mod tests {
    use super::build_assistant_message;
    use crate::session::ContentBlock;
    use crate::usage::TokenUsage;
    use super::super::AssistantEvent;

    #[test]
    fn empty_stream_after_tool_result_is_not_an_error() {
        // Ollama (and some OpenAI-compatible backends) answer a successful
        // tool_result with just a MessageStop — no text, no further tool calls.
        // Before the fix, build_assistant_message returned
        // "assistant stream produced no content" and killed the turn. Regression
        // test: an empty-but-terminated stream yields a single empty-text block,
        // not an error.
        let events = vec![
            AssistantEvent::Usage(TokenUsage {
                input_tokens: 100,
                output_tokens: 0,
                cache_creation_input_tokens: 0,
                cache_read_input_tokens: 0,
            }),
            AssistantEvent::MessageStop,
        ];

        let (message, usage) = build_assistant_message(events)
            .expect("empty-but-terminated stream must not error");

        assert!(usage.is_some(), "usage should propagate");
        assert_eq!(message.blocks.len(), 1, "exactly one placeholder block");
        match &message.blocks[0] {
            ContentBlock::Text { text } => assert_eq!(text, ""),
            other => panic!("expected empty Text block, got {:?}", other),
        }
    }

    #[test]
    fn stream_without_message_stop_is_still_an_error() {
        // Truncated / disconnected streams should still fail loud. Only the
        // empty-after-MessageStop case is treated as "done."
        let events = vec![AssistantEvent::TextDelta("partial".to_string())];
        let err = build_assistant_message(events)
            .expect_err("truncated stream must error");
        let rendered = err.to_string();
        assert!(
            rendered.contains("ended without a message stop"),
            "expected truncated-stream error, got: {rendered}"
        );
    }

    #[test]
    fn normal_text_response_still_works() {
        let events = vec![
            AssistantEvent::TextDelta("Hello ".to_string()),
            AssistantEvent::TextDelta("world.".to_string()),
            AssistantEvent::MessageStop,
        ];
        let (message, _) =
            build_assistant_message(events).expect("text response should parse");
        assert_eq!(message.blocks.len(), 1);
        match &message.blocks[0] {
            ContentBlock::Text { text } => assert_eq!(text, "Hello world."),
            other => panic!("expected Text block, got {:?}", other),
        }
    }
}
