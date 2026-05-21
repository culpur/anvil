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
    ///
    /// ## Task #719 — Bash env-var assignment pre-pass
    ///
    /// CC parity v2.1.145-B1. When `tool_name == "Bash"`, a leading
    /// Bourne-syntax env-var prefix (`KEY=value [KEY=value ...] cmd ...`)
    /// is treated as additional "input" candidates that each `hard_deny`
    /// pattern is also tested against. Without this pre-pass, a deny rule
    /// like `Bash(SECRET=*)` would be bypassed by `SECRET=hacked git push`
    /// because the existing matcher only checks the full command string
    /// (which technically starts with `SECRET=` and would match — but a
    /// more typical bypass is a deny rule `Bash(git push *)` that fails
    /// to match the same command because its prefix is the assignment,
    /// not `git push`). The pre-pass extracts each assignment and matches
    /// patterns against it in isolation, so the example
    /// `SECRET=hacked git push` matches both `Bash(SECRET=*)` AND
    /// `Bash(git push *)` deny rules.
    ///
    /// Chained commands (`;`, `&&`, `||`) are not split — only the FIRST
    /// command's env prefix is examined. This matches CC's behavior: each
    /// command in a chain triggers its own permission flow when executed.
    #[must_use]
    pub fn matches_hard_deny(&self, tool_name: &str, input: &str) -> bool {
        if self
            .hard_deny
            .iter()
            .any(|pat| pattern_matches(pat, tool_name, input))
        {
            return true;
        }

        // Task #719: Bash env-var assignment pre-pass.
        if tool_name == "Bash" {
            let assignments = parse_leading_env_assignments(input);
            // Also re-evaluate the input with the env prefix stripped, so a
            // rule like `Bash(git push *)` matches `SECRET=hacked git push`.
            let stripped = strip_leading_env_assignments(input);
            for assignment in &assignments {
                if self
                    .hard_deny
                    .iter()
                    .any(|pat| pattern_matches(pat, tool_name, assignment))
                {
                    return true;
                }
            }
            if !assignments.is_empty() && stripped != input {
                if self
                    .hard_deny
                    .iter()
                    .any(|pat| pattern_matches(pat, tool_name, stripped))
                {
                    return true;
                }
            }
        }

        false
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

/// Parse leading Bourne-syntax `KEY=value` env-var assignments from a
/// Bash command string. Returns each assignment as a literal `KEY=value`
/// substring (no shell expansion — `$(curl bad)` stays as-is, intentionally).
///
/// Stops at the first token that is NOT a valid env assignment.
/// `KEY` must match `[A-Za-z_][A-Za-z0-9_]*`. The value extends to the
/// next unquoted whitespace; balanced single/double quotes are honored
/// so a value like `KEY="hello world"` is one token.
///
/// `env VAR=x cmd` returns `[]` because the first token is `env` (a
/// command name), not an assignment.
///
/// Task #719: SECURITY pre-pass for `matches_hard_deny`.
fn parse_leading_env_assignments(command: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = command.trim_start();
    while let Some((assignment, after)) = take_leading_assignment(rest) {
        out.push(assignment);
        rest = after.trim_start();
    }
    out
}

/// Strip leading env assignments from a Bash command, returning the
/// portion that runs after them. If the input has no env prefix, returns
/// the original string unchanged.
fn strip_leading_env_assignments(command: &str) -> &str {
    let mut rest = command.trim_start();
    loop {
        match take_leading_assignment(rest) {
            Some((_, after)) => rest = after.trim_start(),
            None => return rest,
        }
    }
}

/// Try to consume one `KEY=value` token from the front of `input`.
/// Returns `Some((assignment_text, remaining))` if successful, `None`
/// otherwise (the first token is not an assignment).
fn take_leading_assignment(input: &str) -> Option<(String, &str)> {
    let bytes = input.as_bytes();
    if bytes.is_empty() {
        return None;
    }
    // KEY = [A-Za-z_][A-Za-z0-9_]*
    let first = bytes[0];
    if !(first.is_ascii_alphabetic() || first == b'_') {
        return None;
    }
    let mut i = 1;
    while i < bytes.len() {
        let b = bytes[i];
        if b.is_ascii_alphanumeric() || b == b'_' {
            i += 1;
        } else {
            break;
        }
    }
    if i >= bytes.len() || bytes[i] != b'=' {
        return None;
    }
    // Consume value: scan until unquoted whitespace.
    let value_start = i + 1;
    let mut j = value_start;
    let mut quote: Option<u8> = None;
    while j < bytes.len() {
        let b = bytes[j];
        match quote {
            Some(q) => {
                if b == q {
                    quote = None;
                }
                // Bourne does not interpret `\` inside single quotes;
                // does inside double quotes. For matcher purposes we
                // treat both literally — the goal is to find the token
                // boundary, not to faithfully evaluate the value.
                j += 1;
            }
            None => {
                if b == b'\'' || b == b'"' {
                    quote = Some(b);
                    j += 1;
                } else if b.is_ascii_whitespace() {
                    break;
                } else {
                    j += 1;
                }
            }
        }
    }
    if quote.is_some() {
        // Unterminated quote — give up so we don't misclassify.
        return None;
    }
    let assignment = input[..j].to_string();
    Some((assignment, &input[j..]))
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

    // ─── Task #719 — Bash env-var assignment permission bypass ──────────────
    //
    // Three regression tests covering the security pre-pass spec'd in
    // docs/cc-parity-audit-2026-05-21.md TASK-H (CC v2.1.145-B1):
    //
    //   1. `SECRET=hacked echo hi` with deny=`Bash(SECRET=*)` → REFUSED.
    //   2. `echo SECRET=hacked` with same deny → ALLOWED (it's an argument).
    //   3. `LANG=en_US cmd` with no rules → ALLOWED (sanity).
    //
    // Plus the orthogonal "env-prefix doesn't shadow the real command rule":
    //   4. `SECRET=hacked git push` with deny=`Bash(git push *)` → REFUSED
    //      (env-stripped pre-pass surfaces the real command for matching).

    #[test]
    fn bash_env_assignment_matches_deny_rule_for_the_assignment() {
        // Spec test 1: a deny rule on the bare env-var name catches the
        // bypass attempt. Without the pre-pass, `Bash(SECRET=*)` would
        // technically match here too (prefix matches), but only because the
        // assignment happens to be at the front of the string. The point
        // of the pre-pass is that the rule's intent — "deny anything that
        // sets SECRET" — holds regardless of position.
        let c = cfg(&["Bash(SECRET=*)"]);
        assert!(
            c.matches_hard_deny("Bash", "SECRET=hacked echo hi"),
            "SECRET=hacked echo hi must be denied by Bash(SECRET=*)"
        );
    }

    #[test]
    fn bash_env_lookalike_in_argv_does_not_match() {
        // Spec test 2: `echo SECRET=hacked` has no env assignment — the
        // string "SECRET=hacked" is an argument to `echo`, not a leading
        // assignment. Must NOT match.
        let c = cfg(&["Bash(SECRET=*)"]);
        assert!(
            !c.matches_hard_deny("Bash", "echo SECRET=hacked"),
            "echo SECRET=hacked is an argument, not an env assignment"
        );
    }

    #[test]
    fn bash_lang_assignment_no_rules_is_allowed() {
        // Spec test 3: empty deny list → nothing is denied even with
        // env assignments present.
        let c = AutoModeConfig::default();
        assert!(!c.matches_hard_deny("Bash", "LANG=en_US cmd"));
    }

    #[test]
    fn bash_env_prefix_does_not_shadow_command_deny_rule() {
        // The other half of the bypass: a user blocks `git push` with
        // `Bash(git push *)`. An adversary tries to circumvent by
        // prefixing an env assignment. The env-stripped pre-pass must
        // surface the real command so the rule fires.
        let c = cfg(&["Bash(git push *)"]);
        assert!(
            c.matches_hard_deny("Bash", "SECRET=hacked git push origin main"),
            "env prefix must not hide the real command from deny rules"
        );
        // Sanity: without the prefix it still works.
        assert!(c.matches_hard_deny("Bash", "git push origin main"));
    }

    #[test]
    fn bash_env_var_with_command_substitution_treated_as_literal() {
        // `KEY=$(curl bad.com) cmd` — the assignment value is a command
        // substitution. The pre-pass must NOT expand it; the matcher
        // sees the literal substring. A pattern like `Bash(KEY=*)` matches
        // because the assignment starts with KEY=.
        let c = cfg(&["Bash(KEY=*)"]);
        assert!(c.matches_hard_deny("Bash", "KEY=$(curl bad.com) echo hi"));
    }

    #[test]
    fn bash_env_chained_commands_only_first_env_examined() {
        // `cmd; VAR=x cmd2` — per spec, only the first command's env is
        // checked. The chained second command will trigger its own
        // permission flow when (and if) it executes.
        let c = cfg(&["Bash(VAR=*)"]);
        // VAR= is NOT a leading assignment on this composite string —
        // the first token is `cmd;`, not an assignment.
        assert!(!c.matches_hard_deny("Bash", "cmd; VAR=x cmd2"));
    }

    #[test]
    fn bash_env_command_named_env_does_not_match_assignment_rule() {
        // `env VAR=x cmd` — `env` is the command, NOT an assignment.
        // The pre-pass yields no env-var tokens (because `env` doesn't
        // start with [A-Za-z_]…`=`). A rule `Bash(VAR=*)` must NOT match.
        let c = cfg(&["Bash(VAR=*)"]);
        assert!(!c.matches_hard_deny("Bash", "env VAR=x cmd"));
    }

    #[test]
    fn bash_env_assignment_only_applies_to_bash_tool() {
        // Pre-pass is Bash-specific: a non-Bash tool with the same input
        // string must NOT pick up env-var matching.
        let c = cfg(&["other_tool(SECRET=*)"]);
        // For "other_tool", we still check the input directly — this
        // matches because the input starts with "SECRET=".
        assert!(c.matches_hard_deny("other_tool", "SECRET=hacked"));
        // But the env-assignment splitter is only invoked for Bash. We
        // confirm that with a non-Bash tool we don't accidentally apply
        // the Bash-specific env stripping.
        let c2 = cfg(&["other_tool(git push *)"]);
        assert!(
            !c2.matches_hard_deny("other_tool", "SECRET=x git push"),
            "env-prefix stripping must be Bash-tool-specific"
        );
    }

    // ─── Helpers used by Task #719 tests ───────────────────────────────────

    #[test]
    fn parse_leading_env_assignments_basic() {
        assert_eq!(
            super::parse_leading_env_assignments("FOO=bar baz qux"),
            vec!["FOO=bar".to_string()]
        );
    }

    #[test]
    fn parse_leading_env_assignments_chained() {
        assert_eq!(
            super::parse_leading_env_assignments("A=1 B=2 C=3 cmd"),
            vec!["A=1".to_string(), "B=2".to_string(), "C=3".to_string()]
        );
    }

    #[test]
    fn parse_leading_env_assignments_quoted_value() {
        assert_eq!(
            super::parse_leading_env_assignments("FOO=\"bar baz\" cmd"),
            vec!["FOO=\"bar baz\"".to_string()]
        );
    }

    #[test]
    fn parse_leading_env_assignments_none_when_first_is_cmd() {
        assert!(super::parse_leading_env_assignments("echo hi").is_empty());
        assert!(super::parse_leading_env_assignments("env FOO=bar cmd").is_empty());
        assert!(super::parse_leading_env_assignments("9NOT=valid cmd").is_empty());
    }

    #[test]
    fn strip_leading_env_assignments_keeps_command_intact() {
        assert_eq!(
            super::strip_leading_env_assignments("FOO=bar BAZ=qux git push"),
            "git push"
        );
        assert_eq!(
            super::strip_leading_env_assignments("echo hi"),
            "echo hi"
        );
    }
}
