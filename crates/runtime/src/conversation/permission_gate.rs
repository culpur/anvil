//! Permission checking and prompt-driven authorization for tool use.

use crate::hooks::{HookRunResult, HookRunner};
use crate::permissions::{PermissionOutcome, PermissionPolicy, PermissionPrompter};
use crate::session::ConversationMessage;

use super::ToolExecutor;

/// Decide whether to allow a tool call and execute it if permitted.
///
/// Returns the `ConversationMessage` (tool_result) that should be appended to
/// the session, regardless of whether the tool was allowed or denied.
pub(super) fn evaluate_and_execute<T: ToolExecutor>(
    tool_use_id: String,
    tool_name: String,
    input: String,
    policy: &PermissionPolicy,
    prompter: &mut Option<&mut dyn PermissionPrompter>,
    hook_runner: &HookRunner,
    executor: &mut T,
) -> ConversationMessage {
    let permission_outcome = if let Some(prompt) = prompter.as_mut() {
        policy.authorize(&tool_name, &input, Some(*prompt))
    } else {
        policy.authorize(&tool_name, &input, None)
    };

    match permission_outcome {
        PermissionOutcome::Allow => {
            let pre_hook_result = hook_runner.run_pre_tool_use(&tool_name, &input);
            if pre_hook_result.is_denied() {
                let deny_message = format!("PreToolUse hook denied tool `{tool_name}`");
                ConversationMessage::tool_result(
                    tool_use_id,
                    tool_name,
                    format_hook_message(&pre_hook_result, &deny_message),
                    true,
                )
            } else {
                let (mut output, mut is_error) =
                    match executor.execute(&tool_name, &input) {
                        Ok(output) => (output, false),
                        Err(error) => (error.to_string(), true),
                    };
                output = merge_hook_feedback(pre_hook_result.messages(), output, false);

                let post_hook_result =
                    hook_runner.run_post_tool_use(&tool_name, &input, &output, is_error);
                if post_hook_result.is_denied() {
                    is_error = true;
                }
                output = merge_hook_feedback(
                    post_hook_result.messages(),
                    output,
                    post_hook_result.is_denied(),
                );

                ConversationMessage::tool_result(tool_use_id, tool_name, output, is_error)
            }
        }
        PermissionOutcome::Deny { reason } => {
            ConversationMessage::tool_result(tool_use_id, tool_name, reason, true)
        }
    }
}

fn format_hook_message(result: &HookRunResult, fallback: &str) -> String {
    if result.messages().is_empty() {
        fallback.to_string()
    } else {
        result.messages().join("\n")
    }
}

fn merge_hook_feedback(messages: &[String], output: String, denied: bool) -> String {
    if messages.is_empty() {
        return output;
    }

    let mut sections = Vec::new();
    if !output.trim().is_empty() {
        sections.push(output);
    }
    let label = if denied {
        "Hook feedback (denied)"
    } else {
        "Hook feedback"
    };
    sections.push(format!("{label}:\n{}", messages.join("\n")));
    sections.join("\n\n")
}
