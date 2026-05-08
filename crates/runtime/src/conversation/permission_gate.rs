//! Permission checking and prompt-driven authorization for tool use.

use crate::hooks::{
    FileChangeAction, FileChangedPayload, HookPermissionDecision, HookRunResult, HookRunner,
    PermissionDeniedPayload, PermissionDeniedSource, PermissionRequestPayload,
};
use crate::permissions::{PermissionOutcome, PermissionPolicy, PermissionPrompter};
use crate::session::ConversationMessage;

use super::ToolExecutor;

/// Tool names that produce file-system writes and must fire FileChanged on success.
const FILE_WRITE_TOOLS: &[&str] = &["write_file", "edit_file", "multi_edit_file"];

/// Decide whether to allow a tool call and execute it if permitted.
///
/// Returns the `ConversationMessage` (`tool_result`) that should be appended to
/// the session, regardless of whether the tool was allowed or denied.
pub(super) fn evaluate_and_execute<T: ToolExecutor>(
    tool_use_id: String,
    tool_name: String,
    input: &str,
    policy: &PermissionPolicy,
    prompter: &mut Option<&mut dyn PermissionPrompter>,
    hook_runner: &HookRunner,
    executor: &mut T,
) -> ConversationMessage {
    // v2.2.11: fire PermissionRequest hooks before the policy gate.
    // A hook may inject a decision to short-circuit the normal prompt.
    let hook_permission = hook_runner.run_permission_request(&PermissionRequestPayload {
        tool: tool_name.clone(),
        input: serde_json::from_str(input)
            .unwrap_or_else(|_| serde_json::Value::String(input.to_string())),
    });

    // First valid hook decision short-circuits the permission system.
    let permission_outcome = match hook_permission.decision {
        Some(HookPermissionDecision::Allow) => PermissionOutcome::Allow,
        Some(HookPermissionDecision::Deny) => PermissionOutcome::Deny {
            reason: hook_permission
                .messages
                .first()
                .cloned()
                .unwrap_or_else(|| format!("PermissionRequest hook denied tool `{tool_name}`")),
        },
        // Ask/Defer: fall through to normal policy evaluation.
        Some(HookPermissionDecision::Ask) | Some(HookPermissionDecision::Defer) | None => {
            if let Some(prompt) = prompter.as_mut() {
                policy.authorize(&tool_name, input, Some(*prompt))
            } else {
                policy.authorize(&tool_name, input, None)
            }
        }
    };

    match permission_outcome {
        PermissionOutcome::Allow => {
            let pre_hook_result = hook_runner.run_pre_tool_use(&tool_name, input);
            if pre_hook_result.is_denied() {
                // v2.2.11: PreToolUse denial also fires PermissionDenied.
                let deny_message = format!("PreToolUse hook denied tool `{tool_name}`");
                let _ = hook_runner.run_permission_denied(&PermissionDeniedPayload {
                    tool: tool_name.clone(),
                    input: serde_json::from_str(input)
                        .unwrap_or_else(|_| serde_json::Value::String(input.to_string())),
                    reason: deny_message.clone(),
                    source: PermissionDeniedSource::Hook,
                });
                ConversationMessage::tool_result(
                    tool_use_id,
                    tool_name,
                    format_hook_message(&pre_hook_result, &deny_message),
                    true,
                )
            } else {
                let (mut output, mut is_error) =
                    match executor.execute(&tool_name, input) {
                        Ok(output) => (output, false),
                        Err(error) => (error.to_string(), true),
                    };
                output = merge_hook_feedback(pre_hook_result.messages(), output, false);

                // v2.2.11: fire FileChanged after a successful write/edit tool.
                if !is_error && FILE_WRITE_TOOLS.contains(&tool_name.as_str()) {
                    if let Some(path) = extract_path_from_input(input) {
                        let action = if tool_name == "write_file" {
                            FileChangeAction::Write
                        } else {
                            FileChangeAction::Edit
                        };
                        let _ = hook_runner.run_file_changed(&FileChangedPayload { path, action });
                    }
                }

                let post_hook_result =
                    hook_runner.run_post_tool_use(&tool_name, input, &output, is_error);
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
            // v2.2.11: fire PermissionDenied after policy/user denial.
            let source = if hook_permission.decision == Some(HookPermissionDecision::Deny) {
                PermissionDeniedSource::Hook
            } else {
                PermissionDeniedSource::User
            };
            let _ = hook_runner.run_permission_denied(&PermissionDeniedPayload {
                tool: tool_name.clone(),
                input: serde_json::from_str(input)
                    .unwrap_or_else(|_| serde_json::Value::String(input.to_string())),
                reason: reason.clone(),
                source,
            });
            ConversationMessage::tool_result(tool_use_id, tool_name, reason, true)
        }
    }
}

/// Extract the file path from a write/edit tool input JSON.
/// Tries common field names: "path", "file_path", "file".
fn extract_path_from_input(input: &str) -> Option<String> {
    let val: serde_json::Value = serde_json::from_str(input).ok()?;
    for field in &["path", "file_path", "file"] {
        if let Some(s) = val.get(field).and_then(|v| v.as_str()) {
            return Some(s.to_string());
        }
    }
    None
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
