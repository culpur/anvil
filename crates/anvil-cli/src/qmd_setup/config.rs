//! Reads + writes the `qmd` section of `~/.anvil/config.json`.
//!
//! Schema (the wizard writes ALL fields when in `InstallAndIndex` or
//! `UseExisting` states):
//!
//! ```jsonc
//! {
//!   "qmd": {
//!     "enabled": true,
//!     "binary_path": "/opt/homebrew/bin/qmd",
//!     "default_collection": "anvil-default",
//!     "refresh_strategy": "launchd|systemd-user|cron|manual",
//!     "embed_backend": "ollama:nomic-embed-text|openai:text-embedding-3-small|ollama-cloud|none"
//!   }
//! }
//! ```
//!
//! The `Defer` state writes `"qmd": { "deferred": { "remaining": 5 } }`
//! so the next-session rail nudge knows to surface the prompt 5 more
//! times.
//!
//! The wizard reuses [`crate::wizard::wizard_save_config`] to merge
//! these values into the on-disk file so existing keys (vault, ollama,
//! providers, …) are preserved.

use std::path::PathBuf;

use serde_json::{Map, Value, json};

/// Build the `qmd` JSON object for the InstallAndIndex / UseExisting
/// path.
#[must_use]
pub fn build_enabled_section(
    binary_path: &str,
    default_collection: &str,
    refresh_strategy: &str,
    embed_backend: &str,
) -> Value {
    json!({
        "enabled": true,
        "binary_path": binary_path,
        "default_collection": default_collection,
        "refresh_strategy": refresh_strategy,
        "embed_backend": embed_backend,
    })
}

/// Build the `qmd` JSON object for the SkipPermanent path.
#[must_use]
pub fn build_skipped_section() -> Value {
    json!({
        "enabled": false,
        "skipped_at": chrono_now_iso8601(),
    })
}

/// Build the `qmd` JSON object for the Defer path. `remaining` is the
/// number of nudges to display before auto-promoting back to the
/// modal (5 per task #666 spec).
#[must_use]
pub fn build_deferred_section(remaining: u32) -> Value {
    json!({
        "enabled": false,
        "deferred": { "remaining": remaining, "since": chrono_now_iso8601() },
    })
}

/// Wrap a `qmd` section into the full config delta the wizard merges.
#[must_use]
pub fn into_wizard_config_delta(qmd_section: Value) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("qmd".to_string(), qmd_section);
    m
}

/// Path of the QMD section's parent config file. Honors
/// `ANVIL_CONFIG_HOME` per the v2.2.17 rule.
#[must_use]
pub fn config_path() -> PathBuf {
    runtime::default_config_home().join("config.json")
}

/// Read the current `qmd.enabled` value. Returns:
/// - `Some(true)`  when the user has run setup (regardless of state).
/// - `Some(false)` when the user has explicitly skipped / deferred.
/// - `None`        when the wizard has never written the section.
#[must_use]
pub fn read_enabled() -> Option<bool> {
    let raw = std::fs::read_to_string(config_path()).ok()?;
    let val: Value = serde_json::from_str(&raw).ok()?;
    val.get("qmd")
        .and_then(|q| q.get("enabled"))
        .and_then(Value::as_bool)
}

/// Read the configured embed backend string.
#[must_use]
pub fn read_embed_backend() -> Option<String> {
    let raw = std::fs::read_to_string(config_path()).ok()?;
    let val: Value = serde_json::from_str(&raw).ok()?;
    val.get("qmd")
        .and_then(|q| q.get("embed_backend"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// Read the configured refresh strategy.
#[must_use]
pub fn read_refresh_strategy() -> Option<String> {
    let raw = std::fs::read_to_string(config_path()).ok()?;
    let val: Value = serde_json::from_str(&raw).ok()?;
    val.get("qmd")
        .and_then(|q| q.get("refresh_strategy"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// Read the configured QMD binary path.
#[must_use]
pub fn read_binary_path() -> Option<PathBuf> {
    let raw = std::fs::read_to_string(config_path()).ok()?;
    let val: Value = serde_json::from_str(&raw).ok()?;
    val.get("qmd")
        .and_then(|q| q.get("binary_path"))
        .and_then(Value::as_str)
        .map(PathBuf::from)
}

/// Decrement the defer counter by 1, or remove the deferred block
/// when the counter hits zero. Returns the new counter value (or
/// `None` if the section is absent).
pub fn tick_defer_counter() -> Option<u32> {
    let path = config_path();
    let raw = std::fs::read_to_string(&path).ok()?;
    let mut val: Value = serde_json::from_str(&raw).ok()?;
    let remaining = val
        .get("qmd")
        .and_then(|q| q.get("deferred"))
        .and_then(|d| d.get("remaining"))
        .and_then(Value::as_u64)?;
    let new_remaining = remaining.saturating_sub(1) as u32;
    if let Some(q) = val.get_mut("qmd").and_then(Value::as_object_mut) {
        if new_remaining == 0 {
            q.remove("deferred");
        } else if let Some(d) = q.get_mut("deferred").and_then(Value::as_object_mut) {
            d.insert("remaining".to_string(), json!(new_remaining));
        }
    }
    std::fs::write(
        &path,
        serde_json::to_string_pretty(&val).unwrap_or_else(|_| "{}".to_string()),
    )
    .ok()?;
    Some(new_remaining)
}

/// ISO-8601 timestamp without pulling in chrono. We use the
/// `SystemTime → seconds since epoch → format!` shortcut; the
/// schema only needs human-readable, not strictly parseable.
fn chrono_now_iso8601() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("unix:{now}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_enabled_section_has_all_fields() {
        let v = build_enabled_section(
            "/opt/homebrew/bin/qmd",
            "anvil-default",
            "launchd",
            "ollama:nomic-embed-text",
        );
        assert_eq!(v["enabled"], json!(true));
        assert_eq!(v["binary_path"], json!("/opt/homebrew/bin/qmd"));
        assert_eq!(v["default_collection"], json!("anvil-default"));
        assert_eq!(v["refresh_strategy"], json!("launchd"));
        assert_eq!(v["embed_backend"], json!("ollama:nomic-embed-text"));
    }

    #[test]
    fn build_skipped_section_is_disabled() {
        let v = build_skipped_section();
        assert_eq!(v["enabled"], json!(false));
        assert!(v["skipped_at"].is_string());
    }

    #[test]
    fn build_deferred_section_carries_counter() {
        let v = build_deferred_section(5);
        assert_eq!(v["enabled"], json!(false));
        assert_eq!(v["deferred"]["remaining"], json!(5));
    }

    #[test]
    fn into_wizard_config_delta_wraps_under_qmd_key() {
        let v = build_enabled_section("qmd", "c", "manual", "none");
        let m = into_wizard_config_delta(v);
        assert!(m.contains_key("qmd"));
    }
}
