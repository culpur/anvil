//! Update-availability cache for the Anvil CLI.
//!
//! Persists the most recently observed "latest version" string to
//! `~/.anvil/update_check.json` along with the unix timestamp of the check.
//! When the cache is fresher than `MAX_AGE_SECS` (24h) the on-disk value is
//! returned without contacting the network; otherwise the upstream is queried
//! and the result is written back.
//!
//! ## Probe order
//!
//! 1. **`https://anvilhub.culpur.net/api/version`** (preferred)
//!    Wire-stable JSON served by anvilhub-web. Source of truth for the
//!    "what's the published version" question. Cached for 60s at the edge.
//! 2. **`https://api.github.com/repos/culpur/anvil/releases/latest`** (fallback)
//!    Used only when anvilhub is unreachable (DNS failure, timeout, any
//!    non-200 status). anvilhub being down must never block the update check.
//!
//! The probe source ("anvilhub" or "github") is recorded in the cache so the
//! TUI rail and operator tooling can tell which path was taken.
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

/// Which upstream produced the cached `latest_version`. Serialised as
/// `"anvilhub"` or `"github"` in the cache JSON.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UpdateSource {
    /// `https://anvilhub.culpur.net/api/version` — preferred path.
    Anvilhub,
    /// `https://api.github.com/repos/culpur/anvil/releases/latest` — fallback.
    Github,
}

/// On-disk cache entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateCheckCache {
    /// Latest version observed from the release feed, semver-style (no `v` prefix).
    pub latest_version: String,
    /// Unix timestamp (seconds) when the check ran.
    pub checked_at: u64,
    /// Which upstream produced this entry. Optional for backward compatibility
    /// with caches written before the anvilhub probe existed — defaults to
    /// `Github` on read when absent.
    #[serde(default = "default_source", skip_serializing_if = "Option::is_none")]
    pub source: Option<UpdateSource>,
}

fn default_source() -> Option<UpdateSource> {
    None
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

/// Default anvilhub `/api/version` endpoint. Overridable by tests via
/// `fetch_latest_from_anvilhub_url`.
const ANVILHUB_VERSION_URL: &str = "https://anvilhub.culpur.net/api/version";

/// Default GitHub Releases endpoint. Overridable by tests via
/// `fetch_latest_from_github_url`.
const GITHUB_RELEASES_URL: &str =
    "https://api.github.com/repos/culpur/anvil/releases/latest";

/// Hit a URL via curl with the standard 5s timeout and return its body on
/// HTTP success, or `None` on any failure (DNS, timeout, non-2xx, missing curl).
fn curl_get(url: &str) -> Option<String> {
    let output = Command::new("curl")
        .args(["-sfL", "--max-time", "5", "-H", "User-Agent: anvil-cli", url])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Parse the `latest_version` field out of an anvilhub `/api/version` body.
/// Returns `None` if the field is missing or empty.
fn parse_anvilhub_version(body: &str) -> Option<String> {
    let v = body.split("\"latest_version\"").nth(1)?.split('"').nth(1)?;
    if v.is_empty() {
        None
    } else {
        Some(v.trim_start_matches('v').to_string())
    }
}

/// Parse the `tag_name` field out of a GitHub Releases body and strip the
/// leading `v`. Returns `None` if absent.
fn parse_github_tag(body: &str) -> Option<String> {
    let tag = body.split("\"tag_name\"").nth(1)?.split('"').nth(1)?;
    if tag.is_empty() {
        None
    } else {
        Some(tag.trim_start_matches('v').to_string())
    }
}

/// Fetch the latest version from a custom anvilhub URL. Useful for tests with a
/// local mock server. Returns `None` when the probe fails or the JSON is
/// missing `latest_version`.
pub fn fetch_latest_from_anvilhub_url(url: &str) -> Option<String> {
    parse_anvilhub_version(&curl_get(url)?)
}

/// Fetch the latest version from a custom GitHub Releases URL. Useful for
/// tests with a local mock server. Returns `None` when the probe fails or the
/// JSON is missing `tag_name`.
pub fn fetch_latest_from_github_url(url: &str) -> Option<String> {
    parse_github_tag(&curl_get(url)?)
}

/// Hit the anvilhub `/api/version` endpoint and return the latest version
/// string (no `v` prefix) on success. Returns `None` when the probe fails.
fn fetch_latest_from_anvilhub() -> Option<String> {
    fetch_latest_from_anvilhub_url(ANVILHUB_VERSION_URL)
}

/// Hit the GitHub Releases endpoint and return the latest version string
/// (no `v` prefix) on success. Returns `None` when the probe fails.
fn fetch_latest_from_github() -> Option<String> {
    fetch_latest_from_github_url(GITHUB_RELEASES_URL)
}

/// Probe the upstream stack (anvilhub first, GitHub fallback) and return the
/// latest version plus the source that produced it. Returns `None` when both
/// probes fail.
fn fetch_latest_with_source() -> Option<(String, UpdateSource)> {
    fetch_latest_with_source_from(ANVILHUB_VERSION_URL, GITHUB_RELEASES_URL)
}

/// Test-friendly variant of `fetch_latest_with_source` that takes explicit
/// endpoints. anvilhub is tried first; on any failure GitHub is tried.
pub fn fetch_latest_with_source_from(
    anvilhub_url: &str,
    github_url: &str,
) -> Option<(String, UpdateSource)> {
    if let Some(v) = fetch_latest_from_anvilhub_url(anvilhub_url) {
        return Some((v, UpdateSource::Anvilhub));
    }
    let v = fetch_latest_from_github_url(github_url)?;
    Some((v, UpdateSource::Github))
}

/// Return the latest version string when newer than `current_version`, else `None`.
///
/// Uses the on-disk cache when fresh (< 24h old); otherwise probes the upstream
/// stack (anvilhub first, GitHub fallback) and refreshes the cache. The cache
/// is consulted FIRST even when stale, so a manually-dropped JSON file
/// overrides the network probe for that launch.
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

    // Step 2: cache stale or absent — probe anvilhub, fall back to GitHub.
    let (latest, source) = fetch_latest_with_source()?;
    let entry = UpdateCheckCache {
        latest_version: latest.clone(),
        checked_at: now_secs(),
        source: Some(source),
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
            source: Some(UpdateSource::Anvilhub),
        };
        let json = serde_json::to_string(&entry).expect("serialize");
        let parsed: UpdateCheckCache = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(parsed.latest_version, "2.3.0");
        assert_eq!(parsed.checked_at, 1_700_000_000);
        assert_eq!(parsed.source, Some(UpdateSource::Anvilhub));
    }

    #[test]
    fn cache_legacy_json_without_source_field() {
        // Caches written before the anvilhub probe existed lack `source`.
        // They must still parse cleanly, with source returning None.
        let legacy = r#"{"latest_version":"2.2.15","checked_at":1700000000}"#;
        let parsed: UpdateCheckCache = serde_json::from_str(legacy).expect("deserialize");
        assert_eq!(parsed.latest_version, "2.2.15");
        assert_eq!(parsed.source, None);
    }

    #[test]
    fn update_source_serialises_lowercase() {
        // Wire format MUST be lowercase "anvilhub" / "github" so the cache
        // file is human-readable and matches the JSON keys we emit.
        let json = serde_json::to_string(&UpdateSource::Anvilhub).unwrap();
        assert_eq!(json, "\"anvilhub\"");
        let json = serde_json::to_string(&UpdateSource::Github).unwrap();
        assert_eq!(json, "\"github\"");
    }

    /// Spin a single-request HTTP server on 127.0.0.1 that returns
    /// `status_line` + `body` for the first connection. Returns the bound
    /// URL the caller should curl. Crashes the test on any I/O error.
    fn spawn_one_shot_http(status_line: &'static str, body: &'static str) -> String {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                // Drain request headers (best-effort — we don't actually parse).
                let mut buf = [0u8; 1024];
                let _ = stream.read(&mut buf);
                let response = format!(
                    "HTTP/1.1 {status_line}\r\n\
                     Content-Type: application/json\r\n\
                     Content-Length: {}\r\n\
                     Connection: close\r\n\r\n\
                     {body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes());
            }
        });
        format!("http://127.0.0.1:{port}/")
    }

    #[test]
    fn fetch_prefers_anvilhub_when_200() {
        // anvilhub returns 200 → source must be Anvilhub. GitHub URL is set
        // to an unreachable port so a fallback would be detectable as a panic
        // or an empty result — neither should happen.
        let anvilhub_url = spawn_one_shot_http(
            "200 OK",
            "{\"latest_version\":\"2.4.7\",\"released_at\":\"x\"}",
        );
        let github_url = "http://127.0.0.1:1/"; // unroutable
        let (version, source) =
            fetch_latest_with_source_from(&anvilhub_url, github_url).expect("got a result");
        assert_eq!(version, "2.4.7");
        assert_eq!(source, UpdateSource::Anvilhub);
    }

    #[test]
    fn fetch_falls_through_to_github_on_anvilhub_404() {
        // anvilhub returns 404 (Cloudflare-style miss) → must fall through to
        // GitHub. Source must be Github.
        let anvilhub_url = spawn_one_shot_http("404 Not Found", "not here");
        let github_url = spawn_one_shot_http("200 OK", "{\"tag_name\":\"v2.4.8\"}");
        let (version, source) =
            fetch_latest_with_source_from(&anvilhub_url, &github_url).expect("got a result");
        assert_eq!(version, "2.4.8");
        assert_eq!(source, UpdateSource::Github);
    }
}
