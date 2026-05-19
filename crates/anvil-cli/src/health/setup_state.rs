//! Skip / defer state from task #666.
//!
//! Stored at `~/.anvil/setup_state.json`.  Owned by A1's wizard work; this
//! module is a *read-only* consumer that handles the file being missing
//! (fresh install, A1's PR not merged yet) gracefully — we treat every
//! component as not-skipped / not-deferred.
//!
//! Schema (forward-compatible — unknown keys ignored):
//!
//! ```json
//! {
//!   "components": {
//!     "qmd":    { "skip": true,  "skip_reason": "user said no in wizard" },
//!     "ollama": { "defer_until": "2026-05-25T07:00:00Z" }
//!   }
//! }
//! ```
//!
//! Semantics:
//! - `skip: true`  → NEVER probe; treat as `NotApplicable("skipped")`.
//! - `defer_until` → for Drift severity ONLY, suppress until that time;
//!   Broken severity always fires (cannot defer a hard failure).

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;

use super::report::Component;

/// Resolve `~/.anvil/setup_state.json` honouring `ANVIL_CONFIG_HOME`.
#[must_use]
pub fn setup_state_path() -> PathBuf {
    runtime::default_config_home().join("setup_state.json")
}

/// Read-only snapshot of the setup-state file.  Loaded once per probe sweep
/// (the file is tiny; no need to keep it long-lived).
#[derive(Debug, Default, Clone)]
pub struct SetupState {
    /// Best-effort raw view; the file is owned by A1 and we don't want to
    /// re-implement its schema.
    root: Value,
}

impl SetupState {
    /// Load from the default path.  Returns `Default` (empty) if the file
    /// is missing or unparseable — health probes must never fail because
    /// A1's file isn't there yet.
    #[must_use]
    pub fn load_default() -> Self {
        let path = setup_state_path();
        match std::fs::read_to_string(&path) {
            Ok(data) => match serde_json::from_str::<Value>(&data) {
                Ok(root) => Self { root },
                Err(_) => Self::default(),
            },
            Err(_) => Self::default(),
        }
    }

    /// Construct from a parsed JSON value (test helper).
    #[must_use]
    pub fn from_value(root: Value) -> Self {
        Self { root }
    }

    /// True iff the user explicitly skipped this component in the wizard.
    /// Such probes are short-circuited to `NotApplicable("skipped")`.
    #[must_use]
    pub fn is_skipped(&self, component: Component) -> bool {
        self.component_field_bool(component, "skip")
    }

    /// True iff this component is currently in a defer window (suppressed
    /// for Drift severity only — Broken always fires).
    #[must_use]
    pub fn is_deferred_now(&self, component: Component) -> bool {
        let Some(defer_until) = self.component_field_str(component, "defer_until") else {
            return false;
        };
        // Parse RFC-3339-ish.  We accept any value that ends with `Z` and
        // has 19+ chars in the form YYYY-MM-DDTHH:MM:SSZ.  No chrono dep
        // here — health probes must stay lightweight.
        let now_unix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        match parse_rfc3339_basic(&defer_until) {
            Some(deadline_unix) => now_unix < deadline_unix,
            None => false,
        }
    }

    fn component_field_bool(&self, component: Component, field: &str) -> bool {
        self.root
            .pointer(&format!("/components/{}/{field}", component.as_str()))
            .and_then(Value::as_bool)
            .unwrap_or(false)
    }

    fn component_field_str(&self, component: Component, field: &str) -> Option<String> {
        self.root
            .pointer(&format!("/components/{}/{field}", component.as_str()))
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
    }
}

/// Minimal RFC-3339 parser supporting `YYYY-MM-DDTHH:MM:SSZ`.
///
/// Returns Unix seconds, or `None` on malformed input.  Lightweight + no
/// external dep — this is just for the defer-until check.  Treats the
/// timestamp as UTC.
fn parse_rfc3339_basic(s: &str) -> Option<u64> {
    let s = s.trim();
    if s.len() < 20 || !s.ends_with('Z') {
        return None;
    }
    // YYYY-MM-DDTHH:MM:SSZ
    let year: u32 = s.get(0..4)?.parse().ok()?;
    let month: u32 = s.get(5..7)?.parse().ok()?;
    let day: u32 = s.get(8..10)?.parse().ok()?;
    let hour: u32 = s.get(11..13)?.parse().ok()?;
    let minute: u32 = s.get(14..16)?.parse().ok()?;
    let second: u32 = s.get(17..19)?.parse().ok()?;

    // Naive Unix-time computation (treats date as proleptic Gregorian).
    // Good enough for "is this past now?" — the user only sets defers
    // a few days out.
    let days = days_since_epoch(year as i64, month as i64, day as i64)?;
    let total = days
        .checked_mul(86400)?
        .checked_add(hour as i64 * 3600)?
        .checked_add(minute as i64 * 60)?
        .checked_add(second as i64)?;
    if total < 0 {
        return None;
    }
    Some(total as u64)
}

/// Days since 1970-01-01 for a proleptic Gregorian date.  Returns `None`
/// for clearly-invalid month/day.
fn days_since_epoch(year: i64, month: i64, day: i64) -> Option<i64> {
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || year < 1970 {
        return None;
    }
    let days_in_month: [i64; 12] = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let mut total: i64 = 0;
    for y in 1970..year {
        total += if is_leap(y) { 366 } else { 365 };
    }
    for m in 1..month {
        total += days_in_month[(m - 1) as usize];
        if m == 2 && is_leap(year) {
            total += 1;
        }
    }
    total += day - 1;
    Some(total)
}

const fn is_leap(year: i64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn empty_state_is_default() {
        let state = SetupState::default();
        for comp in Component::ALL {
            assert!(!state.is_skipped(*comp));
            assert!(!state.is_deferred_now(*comp));
        }
    }

    #[test]
    fn parses_skip_for_specific_component() {
        let state = SetupState::from_value(json!({
            "components": {
                "qmd": { "skip": true },
                "ollama": { "skip": false }
            }
        }));
        assert!(state.is_skipped(Component::Qmd));
        assert!(!state.is_skipped(Component::Ollama));
        assert!(!state.is_skipped(Component::Config));
    }

    #[test]
    fn defer_in_the_future_is_active() {
        // 2099-12-31T00:00:00Z is comfortably in the future.
        let state = SetupState::from_value(json!({
            "components": {
                "ollama": { "defer_until": "2099-12-31T00:00:00Z" }
            }
        }));
        assert!(state.is_deferred_now(Component::Ollama));
    }

    #[test]
    fn defer_in_the_past_is_inactive() {
        let state = SetupState::from_value(json!({
            "components": {
                "ollama": { "defer_until": "1970-01-02T00:00:00Z" }
            }
        }));
        assert!(!state.is_deferred_now(Component::Ollama));
    }

    #[test]
    fn malformed_defer_is_ignored() {
        let state = SetupState::from_value(json!({
            "components": {
                "ollama": { "defer_until": "not-a-date" }
            }
        }));
        assert!(!state.is_deferred_now(Component::Ollama));
    }

    #[test]
    fn unknown_component_in_state_doesnt_crash() {
        let state = SetupState::from_value(json!({
            "components": {
                "future_component_we_dont_know_about": { "skip": true }
            }
        }));
        // Just verify we don't panic on any known component.
        for comp in Component::ALL {
            let _ = state.is_skipped(*comp);
        }
    }

    #[test]
    fn parse_rfc3339_basic_round_trip() {
        // 2026-01-01T00:00:00Z = 1767225600
        assert_eq!(parse_rfc3339_basic("2026-01-01T00:00:00Z"), Some(1767225600));
    }
}
