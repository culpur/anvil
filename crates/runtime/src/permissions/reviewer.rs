//! Synchronous reviewer gate for destructive and credential-leaking tool inputs.
//!
//! This is a **deterministic regex scanner**, not an LLM agent.  It runs
//! entirely in-process with no network calls, so it adds negligible latency
//! when enabled and zero overhead when disabled.
//!
//! Rationale for sync/deterministic design: Anvil's freedom-first positioning
//! requires no telemetry and no surprise latency.  A real LLM-based reviewer
//! would add per-tool API round-trips.  The deterministic scanner covers the
//! high-value cases (rm -rf variants, force-push, DROP TABLE, leaked keys)
//! that justify a gate without the cost.  If a richer ML-based reviewer is
//! ever warranted, it should be opt-in behind a separate feature flag and
//! reviewed with Maverick before implementation.

use std::sync::OnceLock;

use regex::Regex;

// ---------------------------------------------------------------------------
// Config types
// ---------------------------------------------------------------------------

/// How the reviewer behaves after it is enabled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ReviewerMode {
    /// Scan and annotate the approval prompt; never auto-deny.
    /// `manual` in the JSON config maps here.
    #[default]
    Manual,
    /// Scan and annotate the approval prompt, **or** auto-deny when
    /// `block_action` is `Deny`.  `auto_review` in the JSON config maps here.
    AutoReview,
    /// Reviewer is switched off entirely (same as `enabled: false`).
    Off,
}

/// What the reviewer does when it finds a match.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BlockAction {
    /// Annotate the approval prompt with a warning; the user still decides.
    #[default]
    Ask,
    /// Auto-deny without showing a prompt.
    Deny,
}

/// Reviewer configuration.  Mirrors the `permissions.reviewer` JSON block.
///
/// When `enabled` is `false` (the default) or `mode` is `Off`, every call
/// to [`Reviewer::review`] returns [`ReviewResult::clear`] immediately.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewerConfig {
    pub enabled: bool,
    pub mode: ReviewerMode,
    pub block_action: BlockAction,
    /// User-supplied extra destructive patterns (appended to defaults).
    pub extra_destructive_patterns: Vec<String>,
    /// User-supplied extra credential patterns (appended to defaults).
    pub extra_credential_patterns: Vec<String>,
}

impl Default for ReviewerConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            mode: ReviewerMode::AutoReview,
            block_action: BlockAction::Ask,
            extra_destructive_patterns: Vec::new(),
            extra_credential_patterns: Vec::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// Default pattern sets (compiled once per process via OnceLock)
// ---------------------------------------------------------------------------

/// Default destructive command patterns.
static DEFAULT_DESTRUCTIVE: &[&str] = &[
    r"rm\s+-[rRfF]*r[fF]?\s+/",
    r"rm\s+-[rRfF]*r[fF]?\s+~",
    // Match --force not followed by a hyphen (avoids matching --force-with-lease).
    // The regex crate does not support lookahead, so we match --force followed by
    // whitespace, end-of-input, or newline.  A bare `--force` at end of string is
    // also captured via the alternation `--force\z`.
    r"git\s+push\s+--force(?:\s|\z)",
    r"(?i)drop\s+(table|database)",
    r"(?i)DELETE\s+FROM",
];

/// Default credential / secret patterns.
static DEFAULT_CREDENTIAL: &[&str] = &[
    r"AKIA[0-9A-Z]{16}",
    r"ghp_[a-zA-Z0-9]{36}",
    r"sk-(ant|proj|svcacct)-",
    r"xoxb-",
];

// ---------------------------------------------------------------------------
// Result type
// ---------------------------------------------------------------------------

/// Outcome of a single [`Reviewer::review`] call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewResult {
    /// Human-readable descriptions of every pattern that fired.
    pub matched_patterns: Vec<String>,
    /// `Warn` when the reviewer wants the prompt annotated;
    /// `Deny` when `block_action = deny` **and** at least one pattern fired.
    pub recommendation: Recommendation,
}

impl ReviewResult {
    /// Build a clear (no-match) result — fast-path for the disabled case.
    #[must_use]
    pub fn clear() -> Self {
        Self {
            matched_patterns: Vec::new(),
            recommendation: Recommendation::Allow,
        }
    }

    /// `true` when no patterns matched.
    #[must_use]
    pub fn is_clear(&self) -> bool {
        self.matched_patterns.is_empty()
    }
}

/// What the reviewer recommends the gate do next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Recommendation {
    /// No pattern fired; proceed normally.
    Allow,
    /// Pattern(s) fired; annotate the prompt with `warning`.
    Warn { warning: String },
    /// Pattern(s) fired and `block_action = deny`; reject without prompting.
    Deny { reason: String },
}

// ---------------------------------------------------------------------------
// Reviewer
// ---------------------------------------------------------------------------

/// The compiled reviewer.  Build with [`Reviewer::new`] and cache for the
/// lifetime of the session; regex compilation happens once at construction.
pub struct Reviewer {
    /// When `false` or `mode == Off`, [`review`] is a guaranteed no-op.
    active: bool,
    mode: ReviewerMode,
    block_action: BlockAction,
    /// All compiled patterns (destructive + credential).
    compiled: Vec<(String, Regex)>,
}

impl Reviewer {
    /// Build a `Reviewer` from the given config.
    ///
    /// Invalid regex strings are **logged to stderr and skipped** — they do
    /// not cause a panic or abort construction.
    #[must_use]
    pub fn new(config: &ReviewerConfig) -> Self {
        if !config.enabled || config.mode == ReviewerMode::Off {
            return Self {
                active: false,
                mode: config.mode,
                block_action: config.block_action,
                compiled: Vec::new(),
            };
        }

        let mut patterns: Vec<(String, Regex)> = Vec::new();

        // Build the full pattern list: defaults + user extensions.
        let destructive_sources = DEFAULT_DESTRUCTIVE
            .iter()
            .copied()
            .map(ToString::to_string)
            .chain(config.extra_destructive_patterns.iter().cloned());

        let credential_sources = DEFAULT_CREDENTIAL
            .iter()
            .copied()
            .map(ToString::to_string)
            .chain(config.extra_credential_patterns.iter().cloned());

        for raw in destructive_sources.chain(credential_sources) {
            match Regex::new(&raw) {
                Ok(re) => patterns.push((raw, re)),
                Err(err) => {
                    eprintln!(
                        "anvil reviewer: skipping invalid regex {raw:?}: {err}"
                    );
                }
            }
        }

        Self {
            active: true,
            mode: config.mode,
            block_action: config.block_action,
            compiled: patterns,
        }
    }

    /// Scan `tool_input` against all compiled patterns and return a
    /// [`ReviewResult`].
    ///
    /// - **Disabled / Off**: returns [`ReviewResult::clear`] immediately.
    /// - **Manual mode**: may return `Warn` but never `Deny`.
    /// - **AutoReview + block_action=Ask**: returns `Warn` when matched.
    /// - **AutoReview + block_action=Deny**: returns `Deny` when matched.
    #[must_use]
    pub fn review(&self, _tool_name: &str, tool_input: &str) -> ReviewResult {
        if !self.active {
            return ReviewResult::clear();
        }

        let mut matched: Vec<String> = Vec::new();
        for (raw, re) in &self.compiled {
            if re.is_match(tool_input) {
                matched.push(format!("matches /{raw}/"));
            }
        }

        if matched.is_empty() {
            return ReviewResult::clear();
        }

        let summary = matched.join(", ");

        let recommendation = match (self.mode, self.block_action) {
            (ReviewerMode::AutoReview, BlockAction::Deny) => Recommendation::Deny {
                reason: format!("Reviewer flagged: {summary}"),
            },
            // Manual mode never auto-denies regardless of block_action.
            _ => Recommendation::Warn {
                warning: format!("Reviewer flagged: {summary}"),
            },
        };

        ReviewResult {
            matched_patterns: matched,
            recommendation,
        }
    }
}

// ---------------------------------------------------------------------------
// Cached global compiled patterns (for tests / benchmark reference)
// ---------------------------------------------------------------------------

/// Access the default destructive regexes, compiled once.
#[allow(dead_code)]
pub fn default_destructive_regexes() -> &'static Vec<Regex> {
    static CELL: OnceLock<Vec<Regex>> = OnceLock::new();
    CELL.get_or_init(|| {
        DEFAULT_DESTRUCTIVE
            .iter()
            .filter_map(|p| Regex::new(p).ok())
            .collect()
    })
}

/// Access the default credential regexes, compiled once.
#[allow(dead_code)]
pub fn default_credential_regexes() -> &'static Vec<Regex> {
    static CELL: OnceLock<Vec<Regex>> = OnceLock::new();
    CELL.get_or_init(|| {
        DEFAULT_CREDENTIAL
            .iter()
            .filter_map(|p| Regex::new(p).ok())
            .collect()
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn enabled_ask() -> ReviewerConfig {
        ReviewerConfig {
            enabled: true,
            mode: ReviewerMode::AutoReview,
            block_action: BlockAction::Ask,
            ..Default::default()
        }
    }

    fn enabled_deny() -> ReviewerConfig {
        ReviewerConfig {
            enabled: true,
            mode: ReviewerMode::AutoReview,
            block_action: BlockAction::Deny,
            ..Default::default()
        }
    }

    // ── 1 ──────────────────────────────────────────────────────────────────
    #[test]
    fn reviewer_disabled_returns_clear() {
        let reviewer = Reviewer::new(&ReviewerConfig::default()); // enabled: false
        let result = reviewer.review("bash", "rm -rf /");
        assert!(result.is_clear(), "disabled reviewer must return clear");
        assert_eq!(result.recommendation, Recommendation::Allow);
    }

    // ── 2 ──────────────────────────────────────────────────────────────────
    #[test]
    fn reviewer_detects_rm_rf_root() {
        let reviewer = Reviewer::new(&enabled_ask());
        let result = reviewer.review("bash", "rm -rf /");
        assert!(!result.is_clear());
        assert!(
            matches!(&result.recommendation, Recommendation::Warn { warning } if warning.contains("Reviewer flagged")),
            "unexpected recommendation: {:?}",
            result.recommendation
        );
    }

    // ── 3 ──────────────────────────────────────────────────────────────────
    #[test]
    fn reviewer_detects_rm_rf_home() {
        let reviewer = Reviewer::new(&enabled_ask());
        let result = reviewer.review("bash", "rm -rf ~/important");
        assert!(!result.is_clear(), "rm -rf ~ must be flagged");
    }

    // ── 4 ──────────────────────────────────────────────────────────────────
    #[test]
    fn reviewer_detects_force_push_without_lease() {
        let reviewer = Reviewer::new(&enabled_ask());
        let result = reviewer.review("bash", "git push --force origin main");
        assert!(!result.is_clear(), "git push --force must be flagged");
    }

    // ── 5 ──────────────────────────────────────────────────────────────────
    #[test]
    fn reviewer_allows_force_with_lease() {
        let reviewer = Reviewer::new(&enabled_ask());
        let result = reviewer.review("bash", "git push --force-with-lease origin main");
        assert!(
            result.is_clear(),
            "--force-with-lease must not be flagged, got: {:?}",
            result.matched_patterns
        );
    }

    // ── 6 ──────────────────────────────────────────────────────────────────
    #[test]
    fn reviewer_detects_drop_table_sql() {
        let reviewer = Reviewer::new(&enabled_ask());
        let result = reviewer.review("db_query", "DROP TABLE users");
        assert!(!result.is_clear(), "DROP TABLE must be flagged");
    }

    // ── 7 ──────────────────────────────────────────────────────────────────
    #[test]
    fn reviewer_detects_aws_key_pattern() {
        let reviewer = Reviewer::new(&enabled_ask());
        let result = reviewer.review("write_file", "aws_key = AKIAIOSFODNN7EXAMPLE");
        assert!(!result.is_clear(), "AWS key pattern must be flagged");
    }

    // ── 8 ──────────────────────────────────────────────────────────────────
    #[test]
    fn reviewer_detects_github_pat_pattern() {
        let reviewer = Reviewer::new(&enabled_ask());
        let pat = format!("token = ghp_{}", "a".repeat(36));
        let result = reviewer.review("write_file", &pat);
        assert!(!result.is_clear(), "GitHub PAT must be flagged");
    }

    // ── 9 ──────────────────────────────────────────────────────────────────
    #[test]
    fn reviewer_user_extension_appends_to_defaults() {
        let config = ReviewerConfig {
            enabled: true,
            mode: ReviewerMode::AutoReview,
            block_action: BlockAction::Ask,
            extra_destructive_patterns: vec![r"kubectl\s+delete\s+ns".to_string()],
            extra_credential_patterns: Vec::new(),
        };
        let reviewer = Reviewer::new(&config);

        // Default pattern still works.
        let rm_result = reviewer.review("bash", "rm -rf /");
        assert!(!rm_result.is_clear(), "default pattern must still fire");

        // User-supplied pattern also fires.
        let k8s_result = reviewer.review("bash", "kubectl delete ns production");
        assert!(!k8s_result.is_clear(), "user-supplied pattern must fire");
    }

    // ── 10 ─────────────────────────────────────────────────────────────────
    #[test]
    fn reviewer_invalid_regex_skipped_not_crash() {
        let config = ReviewerConfig {
            enabled: true,
            mode: ReviewerMode::AutoReview,
            block_action: BlockAction::Ask,
            extra_destructive_patterns: vec![
                r"[invalid(regex".to_string(), // intentionally bad
                r"rm\s+-rf\s+/".to_string(),   // valid, must still work
            ],
            extra_credential_patterns: Vec::new(),
        };
        // Must not panic.
        let reviewer = Reviewer::new(&config);
        let result = reviewer.review("bash", "rm -rf /");
        assert!(!result.is_clear(), "valid pattern after bad one must still fire");
    }

    // ── 11 ─────────────────────────────────────────────────────────────────
    #[test]
    fn reviewer_block_action_deny_returns_deny() {
        let reviewer = Reviewer::new(&enabled_deny());
        let result = reviewer.review("bash", "rm -rf /");
        assert!(
            matches!(&result.recommendation, Recommendation::Deny { reason } if reason.contains("Reviewer flagged")),
            "expected Deny recommendation, got {:?}",
            result.recommendation
        );
    }

    // ── 12 ─────────────────────────────────────────────────────────────────
    #[test]
    fn reviewer_block_action_ask_returns_warn_only() {
        let reviewer = Reviewer::new(&enabled_ask());
        let result = reviewer.review("bash", "rm -rf /");
        assert!(
            matches!(&result.recommendation, Recommendation::Warn { .. }),
            "block_action=ask must produce Warn, not Deny: {:?}",
            result.recommendation
        );
        // Confirm it is NOT a Deny.
        assert!(
            !matches!(result.recommendation, Recommendation::Deny { .. }),
            "must not produce Deny when block_action=ask"
        );
    }

    // ── 13 ─────────────────────────────────────────────────────────────────
    #[test]
    fn reviewer_idempotent_reviewing_same_input_twice() {
        let reviewer = Reviewer::new(&enabled_ask());
        let input = "rm -rf /";
        let first = reviewer.review("bash", input);
        let second = reviewer.review("bash", input);
        assert_eq!(
            first.matched_patterns, second.matched_patterns,
            "reviewing the same input twice must yield identical matched_patterns"
        );
        assert_eq!(
            first.recommendation, second.recommendation,
            "reviewing the same input twice must yield identical recommendation"
        );
    }

    // ── 14 ─────────────────────────────────────────────────────────────────
    /// Patterns are compiled at construction; calling `review` many times
    /// must not rebuild them.  We verify this structurally: the compiled vec
    /// length is fixed after `new` and equals the number of valid patterns.
    #[test]
    fn reviewer_compiles_patterns_once() {
        let config = enabled_ask();
        let reviewer = Reviewer::new(&config);
        let expected_count = DEFAULT_DESTRUCTIVE.len() + DEFAULT_CREDENTIAL.len();
        assert_eq!(
            reviewer.compiled.len(),
            expected_count,
            "compiled pattern count must equal default destructive + default credential"
        );

        // Calling review multiple times must not grow the compiled vec.
        let _ = reviewer.review("bash", "rm -rf /");
        let _ = reviewer.review("bash", "DROP TABLE x");
        assert_eq!(reviewer.compiled.len(), expected_count);
    }
}
