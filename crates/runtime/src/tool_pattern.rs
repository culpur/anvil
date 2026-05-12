//! Safe wildcard matching for tool allow-rules.
//!
//! CC-139-B5 SECURITY parity. A common wildcard-rule bug is to accept
//! `Skill(name *)` as an allow-rule and naively implement it as
//! `target.contains(prefix)` instead of `target.starts_with(prefix)`.
//! The former is exploitable — `Skill(name foo)` would accept any
//! tool name containing "foo" anywhere, including `evil-foo-tool`.
//!
//! This module provides the canonical matching primitive. Anvil's
//! current tool-permission surface doesn't yet expose wildcard
//! allow-rules — when it does, callers MUST use this fn rather than
//! roll their own.
//!
//! Rules:
//!   - A pattern of `"*"` alone matches anything.
//!   - A pattern ending in `"*"` matches by literal prefix only (the
//!     `*` is the only wildcard character recognised).
//!   - All other characters are literal — `*` in the middle or at the
//!     start is rejected as malformed and matches nothing (we don't
//!     support substring or suffix patterns; those are footguns).
//!   - Matching is case-sensitive — tool names in CC and Anvil are
//!     case-sensitive identifiers.

/// Returns `true` iff `target` matches `pattern` under the safe rules
/// described in the module-level docs.
#[must_use]
pub fn tool_pattern_matches(pattern: &str, target: &str) -> bool {
    // Catch-all.
    if pattern == "*" {
        return true;
    }

    // Trailing-only wildcard: pattern is `<literal>*`.
    if let Some(prefix) = pattern.strip_suffix('*') {
        // Reject malformed patterns with embedded or leading `*`s,
        // which would be silently truncated otherwise.
        if prefix.contains('*') {
            return false;
        }
        return target.starts_with(prefix);
    }

    // No wildcard at all — exact match.
    if !pattern.contains('*') {
        return pattern == target;
    }

    // Any other use of `*` (leading, embedded, multiple) is rejected.
    false
}

#[cfg(test)]
mod tests {
    use super::tool_pattern_matches;

    // ── Exact-match cases ────────────────────────────────────────────────

    #[test]
    fn exact_match_matches_identical() {
        assert!(tool_pattern_matches("Read", "Read"));
        assert!(tool_pattern_matches("Skill(name foo)", "Skill(name foo)"));
    }

    #[test]
    fn exact_match_rejects_different() {
        assert!(!tool_pattern_matches("Read", "Write"));
        assert!(!tool_pattern_matches("Skill(name foo)", "Skill(name bar)"));
    }

    #[test]
    fn case_sensitive() {
        assert!(!tool_pattern_matches("Read", "read"));
        assert!(!tool_pattern_matches("SKILL", "Skill"));
    }

    // ── Catch-all `*` ────────────────────────────────────────────────────

    #[test]
    fn star_matches_anything() {
        assert!(tool_pattern_matches("*", ""));
        assert!(tool_pattern_matches("*", "Read"));
        assert!(tool_pattern_matches("*", "Skill(name evil)"));
        assert!(tool_pattern_matches("*", "anything at all"));
    }

    // ── Trailing-wildcard prefix match ──────────────────────────────────

    #[test]
    fn trailing_star_matches_prefix() {
        assert!(tool_pattern_matches("Skill(name *", "Skill(name foo"));
        assert!(tool_pattern_matches("Skill(name *", "Skill(name foo)"));
        assert!(tool_pattern_matches("Read*", "Read"));
        assert!(tool_pattern_matches("Read*", "ReadFile"));
        assert!(tool_pattern_matches("Read*", "Read anything here"));
    }

    #[test]
    fn trailing_star_rejects_non_prefix() {
        // The whole point of the security fix: substring match must not
        // sneak in.
        assert!(!tool_pattern_matches("Skill(name foo*", "Skill(name bar)"));
        assert!(!tool_pattern_matches("Skill(name foo*", "evil-Skill(name foo)"));
        assert!(!tool_pattern_matches("Read*", "WriteRead"));
        assert!(!tool_pattern_matches("Read*", "ReReadFile"));
    }

    #[test]
    fn empty_prefix_with_star_acts_like_catch_all() {
        // "*" is the catch-all; "" stripped from "*" is the same case.
        assert!(tool_pattern_matches("*", "Read"));
    }

    // ── Footgun-rejection: malformed wildcards ──────────────────────────

    #[test]
    fn leading_star_is_rejected() {
        // No suffix patterns; this prevents `*Tool` from matching
        // every tool name that happens to end in "Tool".
        assert!(!tool_pattern_matches("*Read", "Read"));
        assert!(!tool_pattern_matches("*Read", "WriteRead"));
        assert!(!tool_pattern_matches("*", "Read")  == false);  // sanity
    }

    #[test]
    fn embedded_star_is_rejected() {
        // No substring patterns; `Read*File` would otherwise tempt a
        // naive globber.
        assert!(!tool_pattern_matches("Read*File", "ReadFile"));
        assert!(!tool_pattern_matches("Read*File", "ReadEvilFile"));
    }

    #[test]
    fn multiple_stars_are_rejected() {
        assert!(!tool_pattern_matches("Read**", "Read"));
        assert!(!tool_pattern_matches("**", "anything"));
        assert!(!tool_pattern_matches("a*b*", "anything"));
    }
}
