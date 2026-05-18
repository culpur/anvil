//! Strategy-switching reminder builder (task #636 Component 2).

use std::collections::VecDeque;

use super::stuck_detector::{StuckPattern, ToolEvent};

const MAX_ATTEMPTS_IN_BODY: usize = 8;
const ERROR_RENDER_MAX: usize = 500;

#[must_use]
pub fn summarize_failed_attempts(
    pattern: Option<&StuckPattern>,
    window: &VecDeque<ToolEvent>,
) -> String {
    let failures: Vec<&ToolEvent> = window.iter().filter(|e| e.is_error()).collect();
    if pattern.is_none() && failures.is_empty() {
        return String::new();
    }

    let mut out = String::new();
    if let Some(p) = pattern {
        out.push_str(&format!("Stuck pattern detected: {}. ", p.summary()));
    }
    out.push_str("Things tried this turn that didn't work:\n");

    let render: Vec<&&ToolEvent> = failures
        .iter()
        .rev()
        .take(MAX_ATTEMPTS_IN_BODY)
        .collect();

    if render.is_empty() {
        out.push_str("- (no individual failures captured in the recent window)\n");
    } else {
        for ev in render.iter().rev() {
            let error_blurb = truncate(
                ev.error.as_deref().unwrap_or("(unknown error)"),
                ERROR_RENDER_MAX,
            );
            out.push_str(&format!(
                "- {}(args#{:016x}) -> {}\n",
                ev.tool_name, ev.args_hash, error_blurb
            ));
        }
    }

    out.push_str(
        "\nStep back. Reason about WHY the current approach isn't working. \
         Consider a different strategy. You have permission to abandon the \
         current path.",
    );
    out
}

#[must_use]
pub fn wrap_as_system_reminder(body: &str) -> String {
    if body.is_empty() {
        return String::new();
    }
    format!("<system-reminder>\n{body}\n</system-reminder>")
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push('\u{2026}');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reflection::stuck_detector::StuckDetector;
    use std::collections::VecDeque;
    use std::path::PathBuf;

    fn err_event(tool: &str, msg: &str) -> ToolEvent {
        ToolEvent::new(tool, &format!("args-for-{tool}"), Some(msg.to_string()), None)
    }

    #[test]
    fn summarize_failed_attempts_truncates_long_errors() {
        let long: String = "x".repeat(600);
        let ev = ToolEvent {
            tool_name: "Bash".to_string(),
            args_hash: 0xdead_beef,
            error: Some(long.clone()),
            touched_file: None,
            timestamp: std::time::SystemTime::now(),
        };
        let mut window = VecDeque::new();
        window.push_back(ev);

        let body = summarize_failed_attempts(None, &window);
        assert!(body.contains("Bash(args#"), "must list the failing tool");
        let xs = body.chars().filter(|c| *c == 'x').count();
        assert!(
            xs <= 500,
            "long error must be truncated to <=500 chars, got {xs}"
        );
        assert!(body.contains('\u{2026}'), "truncation marker must appear");
        assert!(body.contains("Step back."), "footer must be present");
    }

    #[test]
    fn summarize_failed_attempts_empty_when_no_failures() {
        let window: VecDeque<ToolEvent> = VecDeque::new();
        assert_eq!(summarize_failed_attempts(None, &window), "");

        let mut populated = VecDeque::new();
        populated.push_back(ToolEvent::new("Bash", "ls", None, None));
        assert_eq!(summarize_failed_attempts(None, &populated), "");
    }

    #[test]
    fn summarize_includes_pattern_label_when_provided() {
        let mut det = StuckDetector::with_defaults();
        det.begin_turn();
        for _ in 0..3 {
            det.observe(ToolEvent::new("Bash", "x", Some("E".into()), None));
        }
        let window = det.window().clone();
        let body = summarize_failed_attempts(
            Some(&StuckPattern::ToolLoop {
                tool: "Bash".to_string(),
                count: 3,
            }),
            &window,
        );
        assert!(body.contains("Stuck pattern detected: repeated"));
        assert!(body.contains("Bash"));
    }

    #[test]
    fn wrap_as_system_reminder_returns_empty_on_empty_body() {
        assert_eq!(wrap_as_system_reminder(""), "");
        assert_eq!(
            wrap_as_system_reminder("hello"),
            "<system-reminder>\nhello\n</system-reminder>"
        );
    }

    #[test]
    fn summarize_caps_attempt_count_to_max() {
        let mut window = VecDeque::new();
        for i in 0..20 {
            window.push_back(err_event("Bash", &format!("err-{i}")));
        }
        let body = summarize_failed_attempts(None, &window);
        assert!(
            body.matches("Bash(args#").count() <= MAX_ATTEMPTS_IN_BODY,
            "expected <={} attempts in body, got {}",
            MAX_ATTEMPTS_IN_BODY,
            body.matches("Bash(args#").count()
        );
    }

    #[test]
    fn summarize_includes_touched_files_via_pattern_label() {
        let p = StuckPattern::Oscillation {
            file: PathBuf::from("/x/y.rs"),
            edits: 3,
        };
        let mut window = VecDeque::new();
        window.push_back(err_event("Edit", "permission denied"));
        let body = summarize_failed_attempts(Some(&p), &window);
        assert!(body.contains("/x/y.rs"));
        assert!(body.contains("Edit(args#"));
    }
}
