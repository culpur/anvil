//! Turn execution logic: drives the API request/tool-use loop for a single turn.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicBool, Ordering};

use crate::prompt_section::PromptSection;
use crate::reflection::{ToolEvent, TurnState};
use crate::session::{ContentBlock, ConversationMessage};
use crate::usage::{TokenUsage, UsageTracker};

use super::{
    ApiClient, ApiRequest, AssistantEvent, RuntimeError, ToolExecutor, TurnSummary,
};
use super::permission_gate::evaluate_and_execute;
use super::usage_tracking::collect_and_record;
use crate::auto_mode::AutoModeConfig;
use crate::compact::{compact_session_reactive, CompactionConfig};
use crate::hooks::{
    HookRunner, PostToolBatchPayload, StopHookBlockCounter, StopHookCapDecision,
    StopHookDecision, StopHookPayload,
};
use crate::permission_memory::PermissionMemory;
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
    system_prompt: &[PromptSection],
    max_iterations: usize,
    usage_tracker: &mut UsageTracker,
    hook_runner: &HookRunner,
    prompter: &mut Option<&mut dyn PermissionPrompter>,
    reviewer: &Reviewer,
    auto_mode: &AutoModeConfig,
    allow_list: &crate::permission_allow::PermissionAllowList,
    permission_memory: Option<&Arc<Mutex<PermissionMemory>>>,
    cancel_token: &Arc<AtomicBool>,
    stop_hook_counter: &mut StopHookBlockCounter,
    compaction_config: &CompactionConfig,
    reflection: &mut TurnState,
) -> Result<TurnSummary, RuntimeError> {
    // v2.2.17 (task #636): begin the reflection turn. Bumps the
    // detector turn id (used for quiet-window suppression) and clears
    // the per-turn scratchpad / pending pattern.
    reflection.begin_turn();

    let result = run_turn_inner_body(
        session,
        api_client,
        tool_executor,
        permission_policy,
        system_prompt,
        max_iterations,
        usage_tracker,
        hook_runner,
        prompter,
        reviewer,
        auto_mode,
        allow_list,
        permission_memory,
        cancel_token,
        stop_hook_counter,
        compaction_config,
        reflection,
    );

    // v2.2.17 (task #636): end the reflection turn (clears scratchpad).
    // Rolling window + pending pattern survive across turns by design —
    // a stuck pattern detected on turn N must influence turn N+1.
    reflection.end_turn();
    result
}

#[allow(clippy::too_many_arguments)]
fn run_turn_inner_body<C: ApiClient, T: ToolExecutor>(
    session: &mut Session,
    api_client: &mut C,
    tool_executor: &mut T,
    permission_policy: &PermissionPolicy,
    system_prompt: &[PromptSection],
    max_iterations: usize,
    usage_tracker: &mut UsageTracker,
    hook_runner: &HookRunner,
    prompter: &mut Option<&mut dyn PermissionPrompter>,
    reviewer: &Reviewer,
    auto_mode: &AutoModeConfig,
    allow_list: &crate::permission_allow::PermissionAllowList,
    permission_memory: Option<&Arc<Mutex<PermissionMemory>>>,
    cancel_token: &Arc<AtomicBool>,
    stop_hook_counter: &mut StopHookBlockCounter,
    compaction_config: &CompactionConfig,
    reflection: &mut TurnState,
) -> Result<TurnSummary, RuntimeError> {
    let mut assistant_messages: Vec<ConversationMessage> = Vec::new();
    let mut tool_results: Vec<ConversationMessage> = Vec::new();
    let mut iterations = 0_usize;
    // Task #564: per-turn counter of reactive compactions, capped by
    // `compaction_config.reactive_max_retries`.  Reset for every new
    // call to run_turn_inner.
    let mut reactive_retries: u32 = 0;

    loop {
        if cancel_token.load(Ordering::SeqCst) {
            return Err(RuntimeError::cancelled());
        }

        iterations += 1;
        if iterations > max_iterations {
            return Err(RuntimeError::new(
                "conversation loop exceeded the maximum number of iterations",
            ));
        }

        // v2.2.17 (task #636): before the next inference call, drain any
        // pending strategy reminder + scratchpad block and inject it as
        // a user-role message. The model treats <system-reminder> tags
        // as system framing; this is how we surface "you've been stuck
        // and previously tried these things — try a different approach"
        // without taking the autonomy axis away from the model.
        let reminder = reflection.drain_reminder_for_next_call();
        if !reminder.is_empty() {
            session
                .messages
                .push(ConversationMessage::user_text(reminder));
        }

        let request = ApiRequest {
            system_prompt: system_prompt.to_vec(),
            messages: session.messages.clone(),
        };
        api_client.set_cancel_token(Arc::clone(cancel_token));
        // Task #564: catch provider context-overflow errors and react
        // with compaction before retrying the same turn.
        let events = match api_client.stream(request) {
            Ok(ev) => ev,
            Err(err) => {
                if let Some(overflow) = err.context_too_long_overflow() {
                    if compaction_config.reactive_enabled
                        && reactive_retries < compaction_config.reactive_max_retries
                    {
                        reactive_retries += 1;
                        let result = compact_session_reactive(
                            session,
                            overflow,
                            *compaction_config,
                        );
                        // Install the compacted session in place and re-loop.
                        *session = result.compacted_session;
                        // Reset the iteration count for the retry so we
                        // don't burn the max_iterations budget on the
                        // reactive path.
                        iterations -= 1;
                        continue;
                    }
                }
                return Err(err);
            }
        };
        if cancel_token.load(Ordering::SeqCst) {
            return Err(RuntimeError::cancelled());
        }
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
            // Task #566: fire Stop hooks. If any returns
            // {"decision":"block","reason":"..."} we keep the loop alive
            // by injecting `reason` as a user-role message (until the
            // per-session cap is hit).  Otherwise the turn ends.
            let session_id = crate::session_ctx::get()
                .map(|c| c.session_id.clone())
                .unwrap_or_default();
            let result = hook_runner.run_stop(&StopHookPayload {
                session_id,
                turn_count: iterations as u64,
                block_count: stop_hook_counter.blocks_seen(),
            });
            match result.decision {
                StopHookDecision::Allow => {
                    break;
                }
                StopHookDecision::Block { reason } => {
                    match stop_hook_counter.record_block() {
                        StopHookCapDecision::Continue => {
                            // Inject the reason as a new user message so
                            // the model re-engages on the next turn.
                            session.messages.push(
                                crate::session::ConversationMessage::user_text(reason),
                            );
                            continue;
                        }
                        StopHookCapDecision::AllowStop { warning: _ } => {
                            // Cap exceeded: surface a synthetic user
                            // message noting the cap before stopping so
                            // the transcript records what happened, then
                            // exit the loop.
                            break;
                        }
                    }
                }
            }
        }

        // v2.2.11: track per-tool timing for PostToolBatch payload.
        let tool_count = pending_tool_uses.len();
        let mut durations_ms: Vec<u64> = Vec::with_capacity(tool_count);
        let mut success_count: usize = 0;
        let mut failure_count: usize = 0;

        for (tool_use_id, tool_name, input) in pending_tool_uses {
            if cancel_token.load(Ordering::SeqCst) {
                return Err(RuntimeError::cancelled());
            }
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
                allow_list,
                permission_memory,
            );
            let elapsed_ms = start.elapsed().as_millis() as u64;
            durations_ms.push(elapsed_ms);

            // Determine success/failure from the result message.
            let (is_err, error_output) = extract_tool_result_status(&result_message);
            if is_err {
                failure_count += 1;
            } else {
                success_count += 1;
            }

            // v2.2.17 (task #636): feed the reflection module. The
            // detector consumes every event (so it can see error
            // density across the rolling 20-event window); the
            // scratchpad only sees failures (so the "previously
            // tried" reminder lists dead ends only).
            let touched_file = detect_touched_file(&tool_name, &input);
            let reflection_error = if is_err { error_output.clone() } else { None };
            reflection.observe_tool_event(ToolEvent::new(
                tool_name.clone(),
                &input,
                reflection_error,
                touched_file,
            ));
            if is_err {
                reflection.record_failure(
                    &tool_name,
                    &input,
                    error_output.as_deref().unwrap_or("(tool error)"),
                );
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

/// Pull `is_error` and the error output text out of a tool result message,
/// for both PostToolBatch accounting and reflection scratchpad feeding.
fn extract_tool_result_status(msg: &ConversationMessage) -> (bool, Option<String>) {
    for block in &msg.blocks {
        if let ContentBlock::ToolResult { is_error, output, .. } = block {
            return (*is_error, if *is_error { Some(output.clone()) } else { None });
        }
    }
    (false, None)
}

/// Heuristic: infer the `touched_file` for the reflection detector's
/// oscillation check. Only specific edit-shaped tools count — Bash /
/// generic shell tools don't claim a single file even when they touch
/// one, so the oscillation signal would be too noisy otherwise.
///
/// We try `serde_json` first (most edit tools serialize a JSON object
/// with a `file_path` / `path` / `notebook_path` field); on parse failure
/// we silently return `None`. Bad input is non-fatal for reflection —
/// the observe_tool_event call still records the args hash + error so
/// the other three detectors (ToolLoop, Thrashing, InferenceStall) work.
fn detect_touched_file(tool_name: &str, input: &str) -> Option<PathBuf> {
    let is_edit_shaped = matches!(
        tool_name,
        "Edit" | "Write" | "MultiEdit" | "NotebookEdit" | "edit_file" | "write_file"
    );
    if !is_edit_shaped {
        return None;
    }
    let value: serde_json::Value = serde_json::from_str(input).ok()?;
    let obj = value.as_object()?;
    for key in ["file_path", "path", "notebook_path"] {
        if let Some(s) = obj.get(key).and_then(serde_json::Value::as_str) {
            let trimmed = s.trim();
            if !trimmed.is_empty() {
                return Some(PathBuf::from(trimmed));
            }
        }
    }
    None
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
