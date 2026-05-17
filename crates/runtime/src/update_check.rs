//! Update-availability cache for the Anvil CLI.
//!
//! Persists the most recently observed "latest version" string to
//! `~/.anvil/update_check.json` along with the unix timestamp of the check.
//! When the cache is fresher than `MAX_AGE_SECS` (24h) the on-disk value is
//! returned without contacting the network; otherwise the GitHub Releases
//! API is queried and the result is written back.
//!
//! Endpoint: `https://api.github.com/repos/culpur/anvil/releases/latest` —
//! this is the same endpoint `anvil-cli/src/update.rs` (`check_for_update`)
//! hits for the legacy startup probe. The AnvilHub HTTPS frontend does not
//! currently expose a public `/api/version` or `/sha256` endpoint (probed
//! 2026-05-17, both return 404 from Cloudflare), so GitHub Releases is the
//! single source of truth for "what's the published version".
//!
//! The cache file's purpose is to:
//!   1. Let the rail show an "update available" line without waiting for a
//!      round-trip on every launch.
//!   2. Throttle network probes to once per 24h regardless of how many TUI
//!      launches happen.
//!   3. Allow the rail to be force-populated for testing (drop the JSON file
//!      with a future `latest_version`, no network needed).

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// How long an on-disk check is considered fresh, in seconds. 24h.
pub const MAX_AGE_SECS: u64 = 24 * 60 * 60;

/// On-disk cache entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateCheckCache {
    /// Latest version observed from the release feed, semver-style (no `v` prefix).
    pub latest_version: String,
    /// Unix timestamp (seconds) when the check ran.
    pub checked_at: u64,
}

/// Path to the cache file (`~/.anvil/update_check.json`).
///
/// Returns `None` when the home directory cannot be resolved.
pub fn cache_path() -> Option<PathBuf> {
    let home = dirs_next::home_dir()?;
    Some(home.join(".anvil").join("update_check.json"))
}

/// Read the cached check from disk if present.
///
/// Returns `None` when the file is absent, unreadable, or malformed.
pub fn read_cache() -> Option<UpdateCheckCache> {
    let path = cache_path()?;
    let bytes = fs::read(&path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Write a fresh cache entry. Creates the parent directory if needed.
/// Errors are swallowed silently — the cache is best-effort.
pub fn write_cache(entry: &UpdateCheckCache) {
    let Some(path) = cache_path() else { return; };
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let Ok(json) = serde_json::to_vec_pretty(entry) else { return; };
    let _ = fs::write(&path, json);
}

/// Current unix timestamp in seconds.
fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Simple semver comparison: returns `true` if `a` is strictly newer than `b`.
///
/// Splits each version on non-digit characters and compares the resulting
/// integer tuples lexicographically. Pre-release suffixes (`-rc1`, `-beta`)
/// are ignored — a `2.3.0-rc1` will compare equal to `2.3.0`. Callers that
/// need pre-release ordering should layer it on top.
pub fn version_is_newer(a: &str, b: &str) -> bool {
    let parse = |v: &str| -> Vec<u32> {
        v.split(|c: char| !c.is_ascii_digit())
            .filter(|s| !s.is_empty())
            .filter_map(|s| s.parse().ok())
            .collect()
    };
    let va = parse(a);
    let vb = parse(b);
    for i in 0..va.len().max(vb.len()) {
        let x = va.get(i).copied().unwrap_or(0);
        let y = vb.get(i).copied().unwrap_or(0);
        if x > y {
            return true;
        }
        if x < y {
            return false;
        }
    }
    false
}

/// Hit the GitHub Releases endpoint and return the latest version string
/// (no `v` prefix) on success. Returns `None` when the probe fails.
fn fetch_latest_from_github() -> Option<String> {
    let url = "https://api.github.com/repos/culpur/anvil/releases/latest";
    let output = Command::new("curl")
        .args(["-sfL", "--max-time", "5", "-H", "User-Agent: anvil-cli", url])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let body = String::from_utf8_lossy(&output.stdout);
    let tag = body.split("\"tag_name\"").nth(1)?.split('"').nth(1)?;
    Some(tag.trim_start_matches('v').to_string())
}

/// Return the latest version string when newer than `current_version`, else `None`.
///
/// Uses the on-disk cache when fresh (< 24h old); otherwise probes GitHub
/// and refreshes the cache. The cache is consulted FIRST even when stale, so
/// a manually-dropped JSON file overrides the network probe for that launch.
///
/// When the cache file's `latest_version` is newer than the running binary's
/// `current_version`, returns `Some(latest_version)` regardless of cache age —
/// this lets a stale-but-newer entry surface immediately.
pub fn check(current_version: &str) -> Option<String> {
    // Step 1: consult cache. If fresh OR already shows a newer version,
    // return without hitting the network.
    if let Some(cache) = read_cache() {
        let age = now_secs().saturating_sub(cache.checked_at);
        let fresh = age < MAX_AGE_SECS;
        let cached_newer = version_is_newer(&cache.latest_version, current_version);
        if fresh || cached_newer {
            return if cached_newer { Some(cache.latest_version) } else { None };
        }
    }

    // Step 2: cache stale or absent — hit GitHub.
    let latest = fetch_latest_from_github()?;
    let entry = UpdateCheckCache {
        latest_version: latest.clone(),
        checked_at: now_secs(),
    };
    write_cache(&entry);

    if version_is_newer(&latest, current_version) {
        Some(latest)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_is_newer_basic() {
        assert!(version_is_newer("2.3.0", "2.2.16"));
        assert!(version_is_newer("2.2.17", "2.2.16"));
        assert!(version_is_newer("3.0.0", "2.2.16"));
        assert!(!version_is_newer("2.2.16", "2.2.16"));
        assert!(!version_is_newer("2.2.15", "2.2.16"));
        assert!(!version_is_newer("2.2.16", "2.3.0"));
    }

    #[test]
    fn version_is_newer_handles_v_prefix() {
        // Caller is expected to strip `v`, but we don't crash if they don't.
        assert!(version_is_newer("2.3.0", "v2.2.16"));
    }

    #[test]
    fn version_is_newer_with_prerelease() {
        // Pre-release suffixes are split as additional integer tokens
        // (`-rc1` → trailing `1`), so `2.3.0-rc1` parses as `[2,3,0,1]` and
        // is considered NEWER than `2.3.0` (`[2,3,0]`). This is intentional
        // and conservative: if a user happens to have a pre-release tag
        // (e.g. `2.3.0-rc1`) on disk, we don't want to suppress the upgrade
        // signal to `2.3.0` final — they should still see the banner.
        assert!(version_is_newer("2.3.0-rc1", "2.3.0"));
        assert!(!version_is_newer("2.3.0", "2.3.0-rc1"));
    }

    #[test]
    fn cache_round_trip_json() {
        // Pure serde round-trip — does not touch disk.
        let entry = UpdateCheckCache {
            latest_version: "2.3.0".to_string(),
            checked_at: 1_700_000_000,
        };
        let json = serde_json::to_string(&entry).expect("serialize");
        let parsed: UpdateCheckCache = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(parsed.latest_version, "2.3.0");
        assert_eq!(parsed.checked_at, 1_700_000_000);
    }
}
