// Edition 2024: env::set_var/remove_var require unsafe
#![allow(unsafe_code)]

//! Per-session effort/reasoning slider.
//!
//! `EffortLevel` maps onto provider-specific knobs:
//!
//! - **Anthropic** (Claude models): `thinking.budget_tokens`
//!   - `Low`    → 2 048 tokens
//!   - `Medium` → 8 192 tokens  (default)
//!   - `High`   → 24 576 tokens
//!   - `Xhigh`  → 65 536 tokens (capped at `max_tokens - 4096` if model limit is lower)
//!
//! - **OpenAI / xAI / Codex** (o-series + reasoning-capable models):
//!   `reasoning.effort = "low" | "medium" | "high"`.  `Xhigh` maps to `high`.
//!
//! - **Gemini**: `thinkingBudget` tokens (same mapping as Anthropic).
//!
//! - **Ollama / local**: no provider knob; the level is stored for env passthrough
//!   only.  Every spawn of a tool or hook receives `ANVIL_EFFORT=<level>` in its
//!   environment so MCP servers and hooks can read it.
//!
//! # Precedence (highest to lowest)
//! 1. CLI flag `--effort <level>`
//! 2. Environment variable `ANVIL_EFFORT`
//! 3. Session override (`/effort <level>`)
//! 4. Config `effort_level` key in `settings.json`
//! 5. Hard default: `medium`

use serde::{Deserialize, Serialize};

/// The four effort tiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EffortLevel {
    Low,
    Medium,
    High,
    Xhigh,
}

impl EffortLevel {
    /// The canonical lowercase string for config / env passthrough.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Xhigh => "xhigh",
        }
    }

    /// Parse from any case-insensitive string.  Returns `None` for unknown values.
    #[must_use]
    pub fn from_str(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "low" => Some(Self::Low),
            "medium" => Some(Self::Medium),
            "high" => Some(Self::High),
            "xhigh" | "x-high" | "extra-high" | "max" => Some(Self::Xhigh),
            _ => None,
        }
    }

    /// Read from the `ANVIL_EFFORT` environment variable.
    /// Returns `None` when the variable is absent or empty.
    /// Prints a warning to stderr and returns `None` for unrecognised values.
    #[must_use]
    pub fn from_env() -> Option<Self> {
        let raw = std::env::var("ANVIL_EFFORT").ok()?;
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return None;
        }
        match Self::from_str(trimmed) {
            Some(level) => Some(level),
            None => {
                eprintln!(
                    "[anvil] warning: ANVIL_EFFORT={trimmed:?} is not a recognised effort level \
                     (low|medium|high|xhigh); ignoring"
                );
                None
            }
        }
    }

    /// Anthropic `budget_tokens` for this effort level.
    ///
    /// The returned value is the *uncapped* target.  Callers must apply
    /// `min(budget, model_max_tokens - 4096)` before sending to the API and
    /// should warn the user when the cap is applied.
    #[must_use]
    pub const fn anthropic_budget_tokens(self) -> u32 {
        match self {
            Self::Low => 2_048,
            Self::Medium => 8_192,
            Self::High => 24_576,
            Self::Xhigh => 65_536,
        }
    }

    /// OpenAI / xAI `reasoning.effort` string.
    ///
    /// `Xhigh` maps to `"high"` because the OpenAI API only accepts three values.
    #[must_use]
    pub const fn openai_reasoning_effort(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High | Self::Xhigh => "high",
        }
    }

    /// Gemini `thinkingBudget` value.
    ///
    /// Returns `-1` for `Xhigh` (Gemini's "dynamic" / unconstrained mode).
    #[must_use]
    pub const fn gemini_thinking_budget(self) -> i32 {
        match self {
            Self::Low => 2_048,
            Self::Medium => 8_192,
            Self::High => 24_576,
            Self::Xhigh => -1,
        }
    }

    /// Set `ANVIL_EFFORT` in the current process environment so that child
    /// processes (hooks, MCP servers) inherit the active level.
    ///
    /// This is a no-op for model calls; the level is wired into API requests
    /// by the provider layer, not via env var.
    pub fn apply_to_env(self) {
        // SAFETY: we are the only writer; the string is a valid C string.
        unsafe { std::env::set_var("ANVIL_EFFORT", self.as_str()); }
    }
}

impl Default for EffortLevel {
    fn default() -> Self {
        Self::Medium
    }
}

impl std::fmt::Display for EffortLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Resolve the effective `EffortLevel` from environment only (no session
/// override or config).  Used at process startup to initialise the env
/// passthrough before a session is available.
#[must_use]
pub fn resolve_effort_from_env() -> EffortLevel {
    EffortLevel::from_env().unwrap_or_default()
}

/// Resolve the effective `EffortLevel` from all sources except the CLI arg
/// (which is applied by the caller before calling this).
///
/// Priority: env var → session override → config default → `Medium`.
#[must_use]
pub fn resolve_effort(
    session_override: Option<EffortLevel>,
    config_default: Option<EffortLevel>,
) -> EffortLevel {
    EffortLevel::from_env()
        .or(session_override)
        .or(config_default)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, OnceLock};

    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    // ── parse_effort ────────────────────────────────────────────────────────

    #[test]
    fn parse_effort_canonical_lowercase() {
        assert_eq!(EffortLevel::from_str("low"), Some(EffortLevel::Low));
        assert_eq!(EffortLevel::from_str("medium"), Some(EffortLevel::Medium));
        assert_eq!(EffortLevel::from_str("high"), Some(EffortLevel::High));
        assert_eq!(EffortLevel::from_str("xhigh"), Some(EffortLevel::Xhigh));
    }

    #[test]
    fn parse_effort_uppercase_accepted() {
        assert_eq!(EffortLevel::from_str("LOW"), Some(EffortLevel::Low));
        assert_eq!(EffortLevel::from_str("MEDIUM"), Some(EffortLevel::Medium));
        assert_eq!(EffortLevel::from_str("HIGH"), Some(EffortLevel::High));
        assert_eq!(EffortLevel::from_str("XHIGH"), Some(EffortLevel::Xhigh));
    }

    #[test]
    fn parse_effort_xhigh_aliases() {
        assert_eq!(EffortLevel::from_str("x-high"), Some(EffortLevel::Xhigh));
        assert_eq!(EffortLevel::from_str("extra-high"), Some(EffortLevel::Xhigh));
        assert_eq!(EffortLevel::from_str("max"), Some(EffortLevel::Xhigh));
    }

    #[test]
    fn parse_effort_rejects_unknown() {
        assert_eq!(EffortLevel::from_str(""), None);
        assert_eq!(EffortLevel::from_str("ultra"), None);
        assert_eq!(EffortLevel::from_str("fast"), None);
        assert_eq!(EffortLevel::from_str("3"), None);
    }

    #[test]
    fn parse_effort_trims_whitespace() {
        assert_eq!(EffortLevel::from_str("  high  "), Some(EffortLevel::High));
    }

    // ── default ─────────────────────────────────────────────────────────────

    #[test]
    fn default_effort_is_medium() {
        assert_eq!(EffortLevel::default(), EffortLevel::Medium);
    }

    #[test]
    fn resolve_effort_defaults_to_medium_when_all_none() {
        let _guard = env_lock();
        unsafe { std::env::remove_var("ANVIL_EFFORT"); }
        let level = resolve_effort(None, None);
        assert_eq!(level, EffortLevel::Medium);
    }

    // ── provider mappings ────────────────────────────────────────────────────

    #[test]
    fn anthropic_budget_tokens_mapping() {
        assert_eq!(EffortLevel::Low.anthropic_budget_tokens(), 2_048);
        assert_eq!(EffortLevel::Medium.anthropic_budget_tokens(), 8_192);
        assert_eq!(EffortLevel::High.anthropic_budget_tokens(), 24_576);
        assert_eq!(EffortLevel::Xhigh.anthropic_budget_tokens(), 65_536);
    }

    #[test]
    fn openai_reasoning_effort_mapping() {
        assert_eq!(EffortLevel::Low.openai_reasoning_effort(), "low");
        assert_eq!(EffortLevel::Medium.openai_reasoning_effort(), "medium");
        assert_eq!(EffortLevel::High.openai_reasoning_effort(), "high");
        // xhigh folds to "high" because OpenAI only accepts three values
        assert_eq!(EffortLevel::Xhigh.openai_reasoning_effort(), "high");
    }

    #[test]
    fn gemini_thinking_budget_mapping() {
        assert_eq!(EffortLevel::Low.gemini_thinking_budget(), 2_048);
        assert_eq!(EffortLevel::Medium.gemini_thinking_budget(), 8_192);
        assert_eq!(EffortLevel::High.gemini_thinking_budget(), 24_576);
        // xhigh = -1 = dynamic mode
        assert_eq!(EffortLevel::Xhigh.gemini_thinking_budget(), -1);
    }

    // ── precedence ───────────────────────────────────────────────────────────

    #[test]
    fn env_var_overrides_session_and_config() {
        let _guard = env_lock();
        unsafe { std::env::set_var("ANVIL_EFFORT", "high"); }
        let level = resolve_effort(Some(EffortLevel::Low), Some(EffortLevel::Low));
        assert_eq!(level, EffortLevel::High);
        unsafe { std::env::remove_var("ANVIL_EFFORT"); }
    }

    #[test]
    fn session_override_beats_config_when_env_absent() {
        let _guard = env_lock();
        unsafe { std::env::remove_var("ANVIL_EFFORT"); }
        let level = resolve_effort(Some(EffortLevel::Xhigh), Some(EffortLevel::Low));
        assert_eq!(level, EffortLevel::Xhigh);
    }

    #[test]
    fn config_default_used_when_no_env_or_session() {
        let _guard = env_lock();
        unsafe { std::env::remove_var("ANVIL_EFFORT"); }
        let level = resolve_effort(None, Some(EffortLevel::Low));
        assert_eq!(level, EffortLevel::Low);
    }

    #[test]
    fn from_env_returns_none_when_unset() {
        let _guard = env_lock();
        unsafe { std::env::remove_var("ANVIL_EFFORT"); }
        assert_eq!(EffortLevel::from_env(), None);
    }

    #[test]
    fn from_env_returns_none_for_empty_string() {
        let _guard = env_lock();
        unsafe { std::env::set_var("ANVIL_EFFORT", ""); }
        assert_eq!(EffortLevel::from_env(), None);
        unsafe { std::env::remove_var("ANVIL_EFFORT"); }
    }

    #[test]
    fn from_env_parses_known_level() {
        let _guard = env_lock();
        unsafe { std::env::set_var("ANVIL_EFFORT", "xhigh"); }
        assert_eq!(EffortLevel::from_env(), Some(EffortLevel::Xhigh));
        unsafe { std::env::remove_var("ANVIL_EFFORT"); }
    }

    #[test]
    fn from_env_returns_none_for_garbage_and_warns() {
        let _guard = env_lock();
        unsafe { std::env::set_var("ANVIL_EFFORT", "turbo"); }
        // Should not panic; should return None (warning is printed to stderr)
        assert_eq!(EffortLevel::from_env(), None);
        unsafe { std::env::remove_var("ANVIL_EFFORT"); }
    }

    // ── display ──────────────────────────────────────────────────────────────

    #[test]
    fn display_matches_as_str() {
        for level in [
            EffortLevel::Low,
            EffortLevel::Medium,
            EffortLevel::High,
            EffortLevel::Xhigh,
        ] {
            assert_eq!(level.to_string(), level.as_str());
        }
    }

    // ── anthropic cap ────────────────────────────────────────────────────────

    #[test]
    fn xhigh_budget_capped_below_model_max() {
        // Demonstrate the cap logic that callers are expected to apply.
        let model_max_tokens: u32 = 16_384; // e.g. a smaller model
        let safe_reserve: u32 = 4_096;
        let target = EffortLevel::Xhigh.anthropic_budget_tokens();
        let capped = target.min(model_max_tokens.saturating_sub(safe_reserve));
        assert_eq!(capped, 12_288);
    }

    #[test]
    fn xhigh_budget_not_capped_for_large_model() {
        let model_max_tokens: u32 = 128_000; // e.g. Claude Sonnet
        let safe_reserve: u32 = 4_096;
        let target = EffortLevel::Xhigh.anthropic_budget_tokens();
        let capped = target.min(model_max_tokens.saturating_sub(safe_reserve));
        // 65536 < 123904 → no cap
        assert_eq!(capped, 65_536);
    }
}
