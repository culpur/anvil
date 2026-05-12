//! Permission checking and prompt-driven authorization for tool use.

use crate::auto_mode::AutoModeConfig;
use crate::hooks::{
    FileChangeAction, FileChangedPayload, HookPermissionDecision, HookRunResult, HookRunner,
    PermissionDeniedPayload, PermissionDeniedSource, PermissionRequestPayload,
};
use crate::permissions::{
    PermissionMode, PermissionOutcome, PermissionPolicy, PermissionPromptDecision,
    PermissionPrompter, PermissionRequest,
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
    auto_mode: &AutoModeConfig,
) -> ConversationMessage {
    // CC-136-F2: auto-mode hard-deny short-circuit.
    //
    // When the active mode is WorkspaceWrite ("auto-mode") and the call
    // matches a user-listed hard-deny pattern, refuse without consulting
    // hooks, reviewer, prompter, or "allow once" memory. This is the
    // safety override that lets users opt into auto-mode for routine
    // work while reserving an unbypassable veto for specific operations.
    //
    // ReadOnly is already more restrictive than the deny list, and
    // DangerFullAccess is an explicit "no guardrails" mode — neither
    // consults this list.
    if policy.active_mode() == PermissionMode::WorkspaceWrite
        && auto_mode.matches_hard_deny(&tool_name, input)
    {
        let reason = format!(
            "tool `{tool_name}` is hard-denied in auto-mode by autoMode.hard_deny"
        );
        otel::permission_decision(&tool_name, "deny", "auto_mode_hard_deny");
        let _ = hook_runner.run_permission_denied(&PermissionDeniedPayload {
            tool: tool_name.clone(),
            input: input.to_string(),
            reason: reason.clone(),
            source: PermissionDeniedSource::User,
        });
        return ConversationMessage::tool_result(tool_use_id, tool_name, reason, true);
    }

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::RuntimeHookConfig;
    use crate::conversation::StaticToolExecutor;
    use crate::permissions::reviewer::ReviewerConfig;
    use crate::session::ContentBlock;

    fn empty_hook_runner() -> HookRunner {
        HookRunner::new(RuntimeHookConfig::default())
    }

    fn make_executor() -> StaticToolExecutor {
        StaticToolExecutor::new()
            .register("write_file", |_| Ok("<written>".to_string()))
            .register("edit_file", |_| Ok("<edited>".to_string()))
            .register("Bash", |_| Ok("<ran>".to_string()))
    }

    fn result_text(msg: &crate::session::ConversationMessage) -> (String, bool) {
        for block in &msg.blocks {
            if let ContentBlock::ToolResult { output, is_error, .. } = block {
                return (output.clone(), *is_error);
            }
        }
        (String::new(), false)
    }

    #[test]
    fn hard_deny_blocks_in_workspace_write_mode() {
        let policy = PermissionPolicy::new(crate::permissions::PermissionMode::WorkspaceWrite)
            .with_tool_requirement("edit_file", crate::permissions::PermissionMode::WorkspaceWrite);
        let mut prompter: Option<&mut dyn PermissionPrompter> = None;
        let runner = empty_hook_runner();
        let mut exec = make_executor();
        let reviewer = Reviewer::new(&ReviewerConfig::default());
        let auto_mode = AutoModeConfig {
            hard_deny: vec!["edit_file".into()],
        };

        let msg = evaluate_and_execute(
            "id-1".into(),
            "edit_file".into(),
            r#"{"path":"/tmp/x"}"#,
            &policy,
            &mut prompter,
            &runner,
            &mut exec,
            &reviewer,
            &auto_mode,
        );

        let (text, is_err) = result_text(&msg);
        assert!(is_err, "hard-deny must produce an error tool_result");
        assert!(
            text.contains("hard-denied in auto-mode"),
            "deny message should mention auto-mode hard-deny, got: {text}"
        );
    }

    #[test]
    fn hard_deny_skipped_in_read_only_mode() {
        // ReadOnly is *more* restrictive than the deny list; the normal policy
        // gate handles it. The auto-mode short-circuit must NOT fire here —
        // the message should reflect a normal policy denial, not the auto-mode
        // hard-deny path.
        let policy = PermissionPolicy::new(crate::permissions::PermissionMode::ReadOnly)
            .with_tool_requirement("edit_file", crate::permissions::PermissionMode::WorkspaceWrite);
        let mut prompter: Option<&mut dyn PermissionPrompter> = None;
        let runner = empty_hook_runner();
        let mut exec = make_executor();
        let reviewer = Reviewer::new(&ReviewerConfig::default());
        let auto_mode = AutoModeConfig {
            hard_deny: vec!["edit_file".into()],
        };

        let msg = evaluate_and_execute(
            "id-2".into(),
            "edit_file".into(),
            r#"{"path":"/tmp/x"}"#,
            &policy,
            &mut prompter,
            &runner,
            &mut exec,
            &reviewer,
            &auto_mode,
        );

        let (text, is_err) = result_text(&msg);
        assert!(is_err);
        assert!(
            !text.contains("hard-denied in auto-mode"),
            "ReadOnly path must use normal policy denial, not auto-mode hard-deny"
        );
    }

    #[test]
    fn hard_deny_skipped_in_danger_full_access_mode() {
        // DangerFullAccess is "I know what I'm doing" — auto-mode hard-deny
        // doesn't apply here either. The tool should run.
        let policy = PermissionPolicy::new(crate::permissions::PermissionMode::DangerFullAccess);
        let mut prompter: Option<&mut dyn PermissionPrompter> = None;
        let runner = empty_hook_runner();
        let mut exec = make_executor();
        let reviewer = Reviewer::new(&ReviewerConfig::default());
        let auto_mode = AutoModeConfig {
            hard_deny: vec!["edit_file".into()],
        };

        let msg = evaluate_and_execute(
            "id-3".into(),
            "edit_file".into(),
            r#"{"path":"/tmp/x"}"#,
            &policy,
            &mut prompter,
            &runner,
            &mut exec,
            &reviewer,
            &auto_mode,
        );

        let (text, is_err) = result_text(&msg);
        assert!(!is_err, "DangerFullAccess should run the tool");
        assert_eq!(text, "<edited>");
    }

    #[test]
    fn hard_deny_with_arg_pattern_matches_input() {
        // Bash needs to be allowed at WorkspaceWrite for the non-matching
        // path to actually reach the executor; otherwise normal policy
        // denial would shadow what we're trying to test.
        let policy = PermissionPolicy::new(crate::permissions::PermissionMode::WorkspaceWrite)
            .with_tool_requirement("Bash", crate::permissions::PermissionMode::WorkspaceWrite);
        let mut prompter: Option<&mut dyn PermissionPrompter> = None;
        let runner = empty_hook_runner();
        let mut exec = make_executor();
        let reviewer = Reviewer::new(&ReviewerConfig::default());
        let auto_mode = AutoModeConfig {
            hard_deny: vec!["Bash(rm -rf *)".into()],
        };

        // Matching call → denied.
        let msg = evaluate_and_execute(
            "id-4".into(),
            "Bash".into(),
            "rm -rf /tmp/x",
            &policy,
            &mut prompter,
            &runner,
            &mut exec,
            &reviewer,
            &auto_mode,
        );
        let (text, is_err) = result_text(&msg);
        assert!(is_err && text.contains("hard-denied"));

        // Non-matching call → executes normally.
        let msg2 = evaluate_and_execute(
            "id-5".into(),
            "Bash".into(),
            "echo hi",
            &policy,
            &mut prompter,
            &runner,
            &mut exec,
            &reviewer,
            &auto_mode,
        );
        let (text2, is_err2) = result_text(&msg2);
        assert!(!is_err2, "Bash echo should not be denied");
        assert_eq!(text2, "<ran>");
    }
}
