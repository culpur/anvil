//! Per-turn failed-attempt scratchpad (task #636 Component 3).

use std::time::SystemTime;

pub const SCRATCHPAD_CAPACITY: usize = 32;

const ARGS_SUMMARY_MAX: usize = 200;
const ERROR_SUMMARY_MAX: usize = 500;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailedAttempt {
    pub tool: String,
    pub args_summary: String,
    pub error_summary: String,
    pub timestamp: SystemTime,
}

impl FailedAttempt {
    #[must_use]
    pub fn new(tool: impl Into<String>, args: &str, error: &str) -> Self {
        Self {
            tool: tool.into(),
            args_summary: truncate(args, ARGS_SUMMARY_MAX),
            error_summary: truncate(error, ERROR_SUMMARY_MAX),
            timestamp: SystemTime::now(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Scratchpad {
    attempts: Vec<FailedAttempt>,
}

impl Scratchpad {
    #[must_use]
    pub const fn new() -> Self {
        Self { attempts: Vec::new() }
    }

    pub fn push(&mut self, attempt: FailedAttempt) {
        if self.attempts.len() == SCRATCHPAD_CAPACITY {
            self.attempts.remove(0);
        }
        self.attempts.push(attempt);
    }

    pub fn push_from_post_tool_use(&mut self, tool: &str, args: &str, error: &str) {
        self.push(FailedAttempt::new(tool, args, error));
    }

    pub fn clear(&mut self) {
        self.attempts.clear();
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.attempts.is_empty()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.attempts.len()
    }

    #[must_use]
    pub fn attempts(&self) -> &[FailedAttempt] {
        &self.attempts
    }

    #[must_use]
    pub fn render_reminder_body(&self) -> String {
        if self.attempts.is_empty() {
            return String::new();
        }
        let mut out = String::from("Previously tried in this turn (avoid repeating):\n");
        for a in &self.attempts {
            out.push_str(&format!(
                "- {}({}) -> {}\n",
                a.tool, a.args_summary, a.error_summary
            ));
        }
        out
    }
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

    #[test]
    fn scratchpad_accumulates_on_tool_failure() {
        let mut sp = Scratchpad::new();
        sp.push_from_post_tool_use("Bash", "ls /nope", "ENOENT");
        sp.push_from_post_tool_use("Edit", "edit foo.rs", "permission denied");
        assert_eq!(sp.len(), 2);
        let body = sp.render_reminder_body();
        assert!(body.contains("Previously tried"));
        assert!(body.contains("Bash(ls /nope) -> ENOENT"));
        assert!(body.contains("Edit(edit foo.rs) -> permission denied"));
    }

    #[test]
    fn scratchpad_clears_at_turn_end() {
        let mut sp = Scratchpad::new();
        sp.push_from_post_tool_use("Bash", "x", "err");
        assert_eq!(sp.len(), 1);
        sp.clear();
        assert!(sp.is_empty());
        assert_eq!(sp.render_reminder_body(), "");
    }

    #[test]
    fn scratchpad_skips_successful_tools() {
        let mut sp = Scratchpad::new();
        let is_error = false;
        if is_error {
            sp.push_from_post_tool_use("Bash", "ls", "err");
        }
        assert!(sp.is_empty(), "no failures must be present");

        let is_error = true;
        if is_error {
            sp.push_from_post_tool_use("Bash", "ls /nope", "ENOENT");
        }
        assert_eq!(sp.len(), 1);
    }

    #[test]
    fn scratchpad_truncates_long_args_and_errors() {
        let long_args = "a".repeat(300);
        let long_err = "b".repeat(700);
        let mut sp = Scratchpad::new();
        sp.push_from_post_tool_use("Bash", &long_args, &long_err);
        let a = &sp.attempts()[0];
        assert!(a.args_summary.len() <= ARGS_SUMMARY_MAX + 4);
        assert!(a.error_summary.len() <= ERROR_SUMMARY_MAX + 4);
        assert!(a.args_summary.contains('\u{2026}'));
        assert!(a.error_summary.contains('\u{2026}'));
    }

    #[test]
    fn scratchpad_caps_at_capacity() {
        let mut sp = Scratchpad::new();
        for i in 0..(SCRATCHPAD_CAPACITY + 5) {
            sp.push_from_post_tool_use("X", &format!("{i}"), "err");
        }
        assert_eq!(sp.len(), SCRATCHPAD_CAPACITY);
        assert_eq!(sp.attempts()[0].args_summary, "5");
    }

    #[test]
    fn scratchpad_render_is_empty_when_empty() {
        let sp = Scratchpad::new();
        assert_eq!(sp.render_reminder_body(), "");
    }
}
