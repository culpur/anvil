//! Permission checking and prompt-driven authorization for tool use.

use crate::hooks::{
    FileChangeAction, FileChangedPayload, HookPermissionDecision, HookRunResult, HookRunner,
    PermissionDeniedPayload, PermissionDeniedSource, PermissionRequestPayload,
};
use crate::permissions::{
    PermissionOutcome, PermissionPolicy, PermissionPromptDecision, PermissionPrompter,
    PermissionRequest,
};
use crate::permissions::reviewer::{Recommendation, Reviewer};
use crate::session::ConversationMessage;
use crate::otel;

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
    reviewer: &Reviewer,
) -> ConversationMessage {
    // v2.2.11: fire PermissionRequest hooks before the policy gate.
    // A hook may inject a decision to short-circuit the normal prompt.
    let hook_permission = hook_runner.run_permission_request(&PermissionRequestPayload {
        tool: tool_name.clone(),
        input: input.to_string(),
        requested_mode: policy.active_mode().as_str().to_string(),
    });

    // W8: Run the reviewer before the approval prompt (or before any
    // policy decision that doesn't involve a human prompter).
    //
    // The reviewer is a synchronous deterministic scanner — no LLM, no
    // network.  It fires after PermissionRequest hooks so that hook-injected
    // Allow decisions bypass the reviewer (the hook has already made a
    // deliberate choice).
    let review = reviewer.review(&tool_name, input);

    // First valid hook decision short-circuits the permission system.
    let permission_outcome = match hook_permission.decision {
        Some(HookPermissionDecision::Allow) => {
            otel::permission_decision(&tool_name, "allow", "hook");
            PermissionOutcome::Allow
        }
        Some(HookPermissionDecision::Deny) => {
            otel::permission_decision(&tool_name, "deny", "hook");
            PermissionOutcome::Deny {
                reason: hook_permission
                    .messages
                    .first()
                    .cloned()
                    .unwrap_or_else(|| format!("PermissionRequest hook denied tool `{tool_name}`")),
            }
        }
        // Ask/Defer/None: fall through to reviewer gate → normal policy evaluation.
        Some(HookPermissionDecision::Ask) | Some(HookPermissionDecision::Defer) | None => {
            // W8 reviewer gate:
            //   - Deny recommendation → auto-deny without prompting.
            //   - Warn recommendation → annotate the input seen by the prompter.
            //   - Allow (no match) → pass through unchanged.
            match &review.recommendation {
                Recommendation::Deny { reason } => {
                    otel::permission_decision(&tool_name, "deny", "policy");
                    PermissionOutcome::Deny {
                        reason: reason.clone(),
                    }
                }
                Recommendation::Warn { warning } => {
                    // Annotate the prompt by wrapping the prompter.
                    let annotated_input = format!("[{warning}]\n\n{input}");
                    let has_prompter = prompter.is_some();
                    let outcome = if let Some(p) = prompter.as_mut() {
                        let mut annotating = AnnotatingPrompter {
                            inner: *p,
                            annotated_input: &annotated_input,
                        };
                        policy.authorize(&tool_name, input, Some(&mut annotating))
                    } else {
                        policy.authorize(&tool_name, input, None)
                    };
                    let source = if has_prompter { "user" } else { "policy" };
                    match &outcome {
                        PermissionOutcome::Allow => {
                            otel::permission_decision(&tool_name, "allow", source);
                        }
                        PermissionOutcome::Deny { .. } => {
                            otel::permission_decision(&tool_name, "deny", source);
                        }
                    }
                    outcome
                }
                Recommendation::Allow => {
                    // No reviewer match — normal policy path.
                    let has_prompter = prompter.is_some();
                    let outcome = if let Some(p) = prompter.as_mut() {
                        policy.authorize(&tool_name, input, Some(*p))
                    } else {
                        policy.authorize(&tool_name, input, None)
                    };
                    let source = if has_prompter { "user" } else { "policy" };
                    match &outcome {
                        PermissionOutcome::Allow => {
                            otel::permission_decision(&tool_name, "allow", source);
                        }
                        PermissionOutcome::Deny { .. } => {
                            otel::permission_decision(&tool_name, "deny", source);
                        }
                    }
                    outcome
                }
            }
        }
    };

    match permission_outcome {
        PermissionOutcome::Allow => {
            let pre_hook_result = hook_runner.run_pre_tool_use(&tool_name, input);
            if pre_hook_result.is_denied() {
                // v2.2.11: PreToolUse denial also fires PermissionDenied.
                let deny_message = format!("PreToolUse hook denied tool `{tool_name}`");
                otel::permission_decision(&tool_name, "deny", "hook");
                let _ = hook_runner.run_permission_denied(&PermissionDeniedPayload {
                    tool: tool_name.clone(),
                    input: input.to_string(),
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
            } else if matches!(review.recommendation, Recommendation::Deny { .. }) {
                // W8: reviewer auto-denied via policy.
                PermissionDeniedSource::User
            } else {
                PermissionDeniedSource::User
            };
            let _ = hook_runner.run_permission_denied(&PermissionDeniedPayload {
                tool: tool_name.clone(),
                input: input.to_string(),
                reason: reason.clone(),
                source,
            });
            ConversationMessage::tool_result(tool_use_id, tool_name, reason, true)
        }
    }
}

/// A `PermissionPrompter` wrapper that replaces the `input` field presented
/// to the inner prompter with a reviewer-annotated version.
///
/// This lets the user see the reviewer warning in the approval prompt without
/// altering the actual `input` string passed to the tool executor.
struct AnnotatingPrompter<'a> {
    inner: &'a mut dyn PermissionPrompter,
    /// Replacement input with warning prepended.
    annotated_input: &'a str,
}

impl PermissionPrompter for AnnotatingPrompter<'_> {
    fn decide(&mut self, request: &PermissionRequest) -> PermissionPromptDecision {
        let annotated = PermissionRequest {
            tool_name: request.tool_name.clone(),
            input: self.annotated_input.to_string(),
            current_mode: request.current_mode,
            required_mode: request.required_mode,
        };
        self.inner.decide(&annotated)
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
