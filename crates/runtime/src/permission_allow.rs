//! Task #717 / community issue anthropics/claude-code#61077.
//!
//! `permissions.allow` rules in `settings.json` are an allowlist of tool
//! call patterns the user has pre-approved. When a matching call comes
//! through the permission gate, the prompter MUST NOT fire — the tool
//! runs straight away. Before task #717 Anvil parsed the array out of
//! settings.json (see `crate::import::settings`) but never consulted it
//! at the gate, so MCP tools and any other tool listed under `allow`
//! kept hitting the manual-approval prompt anyway.
//!
//! The matcher reuses the safe wildcard primitive from
//! [`crate::tool_pattern::tool_pattern_matches`] (CC-139-B5 security
//! contract) so patterns like `mcp__github__*` only match by literal
//! prefix and never by substring.
//!
//! Pattern grammar (parallel to `crate::auto_mode`):
//!   - `ToolName` — exact match against the tool name (any input).
//!   - `ToolPrefix*` — prefix match on the tool name (any input).
//!   - `ToolName(arg-pattern)` — match only when the tool's serialised
//!     input also matches `arg-pattern` under the same safe rules.
//!
//! MCP tool names follow the canonical `mcp__<server>__<tool>` shape, so
//! `mcp__github__*` matches every tool exposed by the `github` server,
//! and `mcp__github__create_issue` matches just that one.

use crate::tool_pattern::tool_pattern_matches;

/// User-supplied allowlist parsed from `settings.json#/permissions/allow`.
///
/// Empty by default — an unconfigured `permissions.allow` MUST NOT
/// surface as "allow all". Callers gate the allowlist BEFORE the
/// prompter; auto-mode hard-deny, hook deny, and reviewer deny still
/// override (they ranked above the prompter to begin with).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PermissionAllowList {
    patterns: Vec<String>,
}

impl PermissionAllowList {
    /// Build an allowlist from a list of pattern strings, trimming
    /// whitespace and dropping empty entries (empty strings would
    /// otherwise match every call via exact-match against an empty tool
    /// name — defence-in-depth).
    #[must_use]
    pub fn from_patterns<I, S>(patterns: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let cleaned = patterns
            .into_iter()
            .map(|s| s.as_ref().trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        Self { patterns: cleaned }
    }

    /// Read-only access to the underlying patterns (mainly for tests and
    /// `/doctor`-style inspection).
    #[must_use]
    pub fn patterns(&self) -> &[String] {
        &self.patterns
    }

    /// Returns `true` when this `(tool_name, input)` pair matches any
    /// allow pattern.
    #[must_use]
    pub fn matches(&self, tool_name: &str, input: &str) -> bool {
        self.patterns
            .iter()
            .any(|pat| allow_pattern_matches(pat, tool_name, input))
    }
}

/// Internal matcher — `Tool`, `Tool*`, and `Tool(arg-pattern)` forms.
///
/// Kept private + mirrored on `auto_mode::pattern_matches` so any future
/// grammar drift between allow / hard-deny is loud at review time.
fn allow_pattern_matches(pattern: &str, tool_name: &str, input: &str) -> bool {
    if let Some(open) = pattern.find('(') {
        if !pattern.ends_with(')') {
            return false;
        }
        let tool_part = &pattern[..open];
        let arg_part = &pattern[open + 1..pattern.len() - 1];
        return tool_pattern_matches(tool_part, tool_name)
            && tool_pattern_matches(arg_part, input);
    }
    tool_pattern_matches(pattern, tool_name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(patterns: &[&str]) -> PermissionAllowList {
        PermissionAllowList::from_patterns(patterns.iter().map(|s| (*s).to_string()))
    }

    #[test]
    fn empty_allowlist_matches_nothing() {
        let c = PermissionAllowList::default();
        assert!(!c.matches("Read", "{}"));
        assert!(!c.matches("mcp__github__create_issue", "{}"));
    }

    #[test]
    fn exact_tool_name_matches_any_input() {
        let c = cfg(&["Read"]);
        assert!(c.matches("Read", "{}"));
        assert!(c.matches("Read", "anything"));
        assert!(!c.matches("Write", "{}"));
    }

    #[test]
    fn mcp_exact_pattern_matches_just_that_tool() {
        // Task #717 contract: `permissions.allow = ["mcp__test__hello"]`
        // permits exactly `mcp__test__hello` and nothing else.
        let c = cfg(&["mcp__test__hello"]);
        assert!(c.matches("mcp__test__hello", "{}"));
        assert!(!c.matches("mcp__test__goodbye", "{}"));
        assert!(!c.matches("mcp__other__hello", "{}"));
    }

    #[test]
    fn mcp_wildcard_pattern_matches_server_prefix() {
        // Task #717 contract: `permissions.allow = ["mcp__test__*"]`
        // permits every tool from the `test` MCP server.
        let c = cfg(&["mcp__test__*"]);
        assert!(c.matches("mcp__test__hello", "{}"));
        assert!(c.matches("mcp__test__goodbye", "{}"));
        assert!(c.matches("mcp__test__list_things", "{}"));
        // Different server — must NOT match.
        assert!(!c.matches("mcp__other__hello", "{}"));
        // CC-139-B5 substring-attack guard: an evil tool name that
        // CONTAINS the prefix later in the string must NOT match.
        assert!(!c.matches("evil-mcp__test__hello", "{}"));
    }

    #[test]
    fn arg_pattern_form_requires_both_to_match() {
        let c = cfg(&["Bash(npm:*)"]);
        assert!(c.matches("Bash", "npm:install"));
        assert!(c.matches("Bash", "npm:test"));
        // Tool name matches but arg doesn't.
        assert!(!c.matches("Bash", "rm -rf /tmp"));
        // Arg would match in spirit but tool doesn't.
        assert!(!c.matches("OtherShell", "npm:install"));
    }

    #[test]
    fn empty_string_pattern_is_dropped_during_parse() {
        // Defence-in-depth: an empty pattern entered as `""` in the
        // settings array must NOT match every tool. The cleaner in
        // `from_patterns` drops the entry entirely so the matcher can
        // never see it.
        let c = cfg(&["", "  ", "\t"]);
        assert!(c.patterns().is_empty());
        assert!(!c.matches("Read", "{}"));
        assert!(!c.matches("", ""));
    }

    #[test]
    fn whitespace_around_patterns_is_trimmed() {
        let c = cfg(&["  Read  "]);
        assert_eq!(c.patterns(), &["Read".to_string()]);
        assert!(c.matches("Read", "{}"));
    }

    #[test]
    fn multiple_patterns_any_match_allows() {
        let c = cfg(&["Read", "mcp__test__*", "Bash(npm:*)"]);
        assert!(c.matches("Read", "{}"));
        assert!(c.matches("mcp__test__hello", "{}"));
        assert!(c.matches("Bash", "npm:install"));
        assert!(!c.matches("Write", "{}"));
        assert!(!c.matches("Bash", "rm -rf /"));
    }

    #[test]
    fn catch_all_star_matches_everything() {
        // `*` is the documented catch-all. Power users opt in
        // deliberately; the matcher honours it.
        let c = cfg(&["*"]);
        assert!(c.matches("Read", "{}"));
        assert!(c.matches("mcp__anything__here", "{}"));
        assert!(c.matches("Bash", "rm -rf /"));
    }

    #[test]
    fn malformed_paren_is_rejected() {
        let c = cfg(&["Bash(rm -rf"]);
        assert!(!c.matches("Bash", "rm -rf /"));
    }

    // ── Task #717 integration: gate-equivalence asserts ────────────────
    //
    // These tests prove the contract the permission gate relies on:
    // an exact MCP-tool pattern and an MCP-server wildcard both make
    // `matches()` return `true` for `mcp__test__hello`. The gate's
    // call site uses exactly this return value to skip the
    // PermissionPrompt — proven in the `permission_gate.rs` test
    // module that calls `evaluate_and_execute` with the allowlist.

    #[test]
    fn gate_contract_exact_mcp_pattern() {
        let c = cfg(&["mcp__test__hello"]);
        assert!(
            c.matches("mcp__test__hello", "{}"),
            "permissions.allow = [mcp__test__hello] must allow mcp__test__hello"
        );
    }

    #[test]
    fn gate_contract_mcp_wildcard_pattern() {
        let c = cfg(&["mcp__test__*"]);
        assert!(
            c.matches("mcp__test__hello", "{}"),
            "permissions.allow = [mcp__test__*] must allow mcp__test__hello"
        );
    }
}
