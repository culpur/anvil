//! Auto-mode (workspace-write) hard-deny short-circuit.
//!
//! CC parity CC-136-F2. When the active permission mode is auto (workspace-
//! write), the user can list explicit `Tool` or `Tool(arg-pattern)` strings
//! in `settings.json` under `autoMode.hard_deny` to forbid them outright —
//! no hook can override, no prompt is shown, no "allow once" is offered.
//!
//! This is the safety override for users who want auto-mode for routine
//! work but never want certain operations attempted (e.g. `Bash(rm -rf *)`,
//! `write_file(/etc/*)`, `Bash(git push --force*)`).
//!
//! ## Pattern grammar
//!
//! - `Tool` — match any call to that tool name (exact name match).
//! - `Tool*` — prefix-match the tool name (per [`crate::tool_pattern`]).
//! - `Tool(arg-pattern)` — only deny when the **input** also matches
//!   `arg-pattern` under the same safe rules (prefix-with-trailing-`*`).
//!   The wrapping `Tool(` and trailing `)` are literal; only what's inside
//!   is the pattern.
//!
//! ReadOnly and DangerFullAccess modes ignore this list — ReadOnly is more
//! restrictive than the deny list, and DangerFullAccess is an explicit
//! "I know what I'm doing" mode where the user has chosen no guardrails.

use crate::tool_pattern::tool_pattern_matches;

/// Runtime config block parsed from `settings.json#/autoMode`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AutoModeConfig {
    /// User-supplied tool patterns that must never run in auto-mode.
    pub hard_deny: Vec<String>,
}

impl AutoModeConfig {
    /// Returns `true` when this `(tool_name, input)` pair matches any
    /// `hard_deny` entry. The list is small (~tens of entries at most) so
    /// a linear scan is fine.
    #[must_use]
    pub fn matches_hard_deny(&self, tool_name: &str, input: &str) -> bool {
        self.hard_deny
            .iter()
            .any(|pat| pattern_matches(pat, tool_name, input))
    }
}

/// Internal matcher: parses one `hard_deny` pattern and tests it against
/// `(tool_name, input)`.
fn pattern_matches(pattern: &str, tool_name: &str, input: &str) -> bool {
    // `Tool(arg-pattern)` form.
    if let Some(open) = pattern.find('(') {
        if !pattern.ends_with(')') {
            // Malformed: opening paren but no closer. Reject — we don't
            // silently truncate.
            return false;
        }
        let tool_part = &pattern[..open];
        let arg_part = &pattern[open + 1..pattern.len() - 1];
        return tool_pattern_matches(tool_part, tool_name)
            && tool_pattern_matches(arg_part, input);
    }

    // Bare `Tool` or `Tool*` form: match on tool name only.
    tool_pattern_matches(pattern, tool_name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(patterns: &[&str]) -> AutoModeConfig {
        AutoModeConfig {
            hard_deny: patterns.iter().map(|s| (*s).to_string()).collect(),
        }
    }

    #[test]
    fn empty_config_denies_nothing() {
        let c = AutoModeConfig::default();
        assert!(!c.matches_hard_deny("Bash", "rm -rf /"));
        assert!(!c.matches_hard_deny("write_file", "anything"));
    }

    #[test]
    fn bare_tool_name_matches_any_input() {
        let c = cfg(&["edit_file"]);
        assert!(c.matches_hard_deny("edit_file", r#"{"path":"/etc/passwd"}"#));
        assert!(c.matches_hard_deny("edit_file", "anything else"));
        assert!(!c.matches_hard_deny("write_file", "/etc/passwd"));
    }

    #[test]
    fn tool_name_wildcard_prefix_matches() {
        let c = cfg(&["multi_*"]);
        assert!(c.matches_hard_deny("multi_edit_file", "x"));
        assert!(c.matches_hard_deny("multi_grep", "x"));
        assert!(!c.matches_hard_deny("edit_file", "x"));
    }

    #[test]
    fn arg_pattern_requires_both_to_match() {
        let c = cfg(&["Bash(rm -rf *)"]);
        assert!(c.matches_hard_deny("Bash", "rm -rf /tmp"));
        assert!(c.matches_hard_deny("Bash", "rm -rf ~"));
        // Tool-name match but arg doesn't match → no deny.
        assert!(!c.matches_hard_deny("Bash", "ls -la"));
        // Arg would match in spirit but tool name doesn't → no deny.
        assert!(!c.matches_hard_deny("edit_file", "rm -rf /tmp"));
    }

    #[test]
    fn arg_pattern_exact_match() {
        let c = cfg(&["Bash(make clean)"]);
        assert!(c.matches_hard_deny("Bash", "make clean"));
        assert!(!c.matches_hard_deny("Bash", "make"));
        assert!(!c.matches_hard_deny("Bash", "make clean install"));
    }

    #[test]
    fn arg_pattern_safe_against_substring_attack() {
        // The point of CC-139-B5: substring sneak-through must not be possible.
        // `Bash(foo*)` with input `evil-foo-bar` must NOT match because the
        // prefix check is strict.
        let c = cfg(&["Bash(foo*)"]);
        assert!(c.matches_hard_deny("Bash", "foo bar"));
        assert!(!c.matches_hard_deny("Bash", "evil-foo-bar"));
    }

    #[test]
    fn malformed_open_paren_without_close_is_rejected() {
        let c = cfg(&["Bash(rm -rf"]);
        assert!(!c.matches_hard_deny("Bash", "rm -rf /tmp"));
    }

    #[test]
    fn multiple_patterns_any_match_denies() {
        let c = cfg(&["edit_file", "Bash(rm -rf *)", "write_file(/etc/*)"]);
        assert!(c.matches_hard_deny("edit_file", "x"));
        assert!(c.matches_hard_deny("Bash", "rm -rf ~"));
        assert!(c.matches_hard_deny("write_file", "/etc/hosts"));
        assert!(!c.matches_hard_deny("read_file", "/etc/hosts"));
        assert!(!c.matches_hard_deny("Bash", "echo hi"));
    }

    #[test]
    fn star_alone_in_tool_position_matches_every_tool() {
        // Catch-all: equivalent to "deny everything in auto-mode" — useful
        // for power users who want auto-mode disabled entirely without
        // changing modes.
        let c = cfg(&["*"]);
        assert!(c.matches_hard_deny("Bash", "ls"));
        assert!(c.matches_hard_deny("edit_file", "anything"));
        assert!(c.matches_hard_deny("read_file", "/tmp/x"));
    }
}
