//! Share client — publishes read-only conversation snapshots to the
//! passage-culpur.net relay under the `/v1/share/` namespace.
//!
//! This is DISTINCT from `/remote-control`.  Remote-control exposes bidirectional
//! full-instance control; share creates a lightweight, ephemeral, read-only URL
//! for a single tab's conversation.
//!
//! ## Relay endpoint contract (for the passage-culpur relay team)
//!
//! ```text
//! POST   https://api.culpur.net/v1/share/sessions
//!   Body:     { "snapshot": { ... }, "ttl_seconds": 86400 }
//!   Auth:     none (capability URL pattern)
//!   Response: { "share_id": "<uuid>", "url": "https://share.anvilhub.culpur.net/<uuid>",
//!               "expires_at": "<ISO-8601>" }
//!
//! GET    https://api.culpur.net/v1/share/sessions/:id
//!   Response: snapshot JSON (for read-only viewer)
//!
//! DELETE https://api.culpur.net/v1/share/sessions/:id
//!   Response: { "ok": true }
//! ```
//!
//! The client gracefully degrades when the relay is unreachable: it returns
//! `ShareError::RelayUnavailable` and the caller displays a human-readable
//! "Share unavailable" message.

use regex::Regex;
use serde::{Deserialize, Serialize};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

// ─── Relay base URL ─────────────────────────────────────────────────────────

const SHARE_RELAY_BASE: &str = "https://api.culpur.net";

// ─── Rate limiter ────────────────────────────────────────────────────────────

/// Per-process share rate limiter — max 10 shares per rolling 60-minute window.
struct RateLimiter {
    /// Unix timestamps (seconds) of recent share calls.
    timestamps: Vec<u64>,
}

impl RateLimiter {
    const fn new() -> Self {
        Self {
            timestamps: Vec::new(),
        }
    }

    /// Returns `true` when a new share is permitted; records the timestamp on
    /// success.  Returns `false` if the rate limit (10/hr) would be exceeded.
    fn check_and_record(&mut self) -> bool {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or(Duration::ZERO)
            .as_secs();
        let window_start = now.saturating_sub(3600);
        // Prune timestamps outside the rolling window.
        self.timestamps.retain(|&ts| ts >= window_start);
        if self.timestamps.len() >= 10 {
            return false;
        }
        self.timestamps.push(now);
        true
    }
}

static RATE_LIMITER: OnceLock<Mutex<RateLimiter>> = OnceLock::new();

fn rate_limiter() -> &'static Mutex<RateLimiter> {
    RATE_LIMITER.get_or_init(|| Mutex::new(RateLimiter::new()))
}

// ─── Secret scrubber ─────────────────────────────────────────────────────────

/// Patterns that identify API-key-like strings in snapshot content.
///
/// These mirror the patterns used by `content_filter.rs` so the two surfaces
/// stay in sync.  Add new patterns here AND in `BUILTIN_SECRET_PATTERNS` when
/// extending.
static SCRUB_PATTERNS: &[&str] = &[
    // AWS Access Key
    r"AKIA[A-Z0-9]{16}",
    // GitHub tokens
    r"ghp_[a-zA-Z0-9]{36}",
    r"gho_[a-zA-Z0-9]{36}",
    r"ghs_[a-zA-Z0-9]{36}",
    // OpenAI / Anthropic
    r"sk-[a-zA-Z0-9]{48}",
    r"sk-proj-[a-zA-Z0-9\-_]{40,}",
    r"sk-svcacct-[a-zA-Z0-9\-_]{40,}",
    r"sk-ant-[a-zA-Z0-9\-_]{40,}",
    // Slack
    r"xox[bpoa]-[0-9A-Za-z\-]{10,}",
    // Stripe
    r"sk_(?:live|test)_[a-zA-Z0-9]{24,}",
    // Bearer tokens
    r"Bearer\s+[A-Za-z0-9\-_\.]{40,}",
    // Private key headers
    r"-----BEGIN\s+(?:RSA\s+|EC\s+|OPENSSH\s+)?PRIVATE KEY-----",
    // Generic high-entropy hex sequences (SHA-256 / API key style)
    r"\b[0-9a-fA-F]{32,64}\b",
];

/// Compile all scrub patterns into `Regex` objects exactly once.
fn scrub_regexes() -> &'static Vec<Regex> {
    static COMPILED: OnceLock<Vec<Regex>> = OnceLock::new();
    COMPILED.get_or_init(|| {
        SCRUB_PATTERNS
            .iter()
            .filter_map(|p| Regex::new(p).ok())
            .collect()
    })
}

/// Scrub `text` in-place, replacing any API-key-like matches with
/// `[REDACTED]`.
///
/// Returns the cleaned string.  Call this on every message content before
/// building the snapshot payload.
pub fn scrub_secrets(text: &str) -> String {
    let mut result = text.to_string();
    for re in scrub_regexes() {
        result = re.replace_all(&result, "[REDACTED]").into_owned();
    }
    result
}

// ─── Snapshot types ──────────────────────────────────────────────────────────

/// A single message in the share snapshot.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShareMessage {
    pub role: String,
    pub content: String,
}

/// The snapshot payload sent to the relay.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShareSnapshot {
    pub tab_name: String,
    pub model: String,
    pub created_at: String,
    pub messages: Vec<ShareMessage>,
}

impl ShareSnapshot {
    /// Build a snapshot from raw tab data, scrubbing secrets from every message.
    #[must_use]
    pub fn build(
        tab_name: impl Into<String>,
        model: impl Into<String>,
        messages: Vec<ShareMessage>,
    ) -> Self {
        let scrubbed: Vec<ShareMessage> = messages
            .into_iter()
            .map(|m| ShareMessage {
                role: m.role,
                content: scrub_secrets(&m.content),
            })
            .collect();

        let created_at = {
            let secs = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or(Duration::ZERO)
                .as_secs();
            // Format: YYYY-MM-DDTHH:MM:SSZ (manual; avoids chrono dep in this crate)
            format_unix_utc(secs)
        };

        Self {
            tab_name: tab_name.into(),
            model: model.into(),
            created_at,
            messages: scrubbed,
        }
    }
}

/// Format a Unix timestamp as a simple ISO-8601 UTC string.
///
/// We avoid pulling in `chrono` here — the runtime crate doesn't depend on it
/// and adding a datetime dependency for one formatting call is not worth it.
/// Format produced: `YYYY-MM-DDTHH:MM:SSZ`.
fn format_unix_utc(secs: u64) -> String {
    // Days since epoch, broken down to Y/M/D via a simple algorithm.
    let (year, month, day, hour, minute, second) = unix_to_ymd_hms(secs);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

/// Decompose Unix seconds into (year, month, day, hour, min, sec) in UTC.
///
/// Uses the algorithm from Howard Hinnant's date library (public domain).
#[allow(clippy::many_single_char_names)]
fn unix_to_ymd_hms(secs: u64) -> (u32, u32, u32, u32, u32, u32) {
    let second = (secs % 60) as u32;
    let minutes = secs / 60;
    let minute = (minutes % 60) as u32;
    let hours = minutes / 60;
    let hour = (hours % 24) as u32;
    let days = hours / 24;

    // Days from epoch to civil date (Gregorian), Hinnant algorithm.
    let z = days as i64 + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = (y + i64::from(month <= 2)) as u32;

    (year, month, day, hour, minute, second)
}

// ─── Request / response types ────────────────────────────────────────────────

#[derive(Debug, Serialize)]
struct CreateShareRequest {
    snapshot: ShareSnapshot,
    ttl_seconds: u64,
}

#[derive(Debug, Deserialize)]
struct CreateShareResponse {
    share_id: String,
    url: String,
    expires_at: String,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)] // response is deserialized but caller doesn't read fields; keeps the shape documented
struct DeleteShareResponse {
    ok: bool,
}

// ─── Error type ──────────────────────────────────────────────────────────────

/// Errors that can occur during share operations.
#[derive(Debug)]
pub enum ShareError {
    /// The relay endpoint returned an unexpected HTTP status or the request
    /// timed out.  The inner string carries the HTTP status or low-level error.
    RelayUnavailable(String),
    /// The relay returned a 404 — it may not have the endpoint yet.
    RelayNotFound,
    /// The relay response body could not be deserialized.
    ParseError(String),
    /// The local rate limit (10 shares/hr) has been exceeded.
    RateLimitExceeded,
}

impl std::fmt::Display for ShareError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RelayUnavailable(msg) => write!(f, "Share unavailable (relay unreachable): {msg}"),
            Self::RelayNotFound => write!(
                f,
                "Share is temporarily unavailable (relay endpoint not yet deployed)"
            ),
            Self::ParseError(msg) => write!(f, "Share: unexpected relay response: {msg}"),
            Self::RateLimitExceeded => {
                write!(f, "Rate limit: 10 shares/hour. Try again later.")
            }
        }
    }
}

// ─── Active share record ─────────────────────────────────────────────────────

/// A live share returned after a successful `POST /v1/share/sessions`.
#[derive(Debug, Clone)]
pub struct ActiveShare {
    pub share_id: String,
    pub url: String,
    pub tab_id: String,
    pub tab_name: String,
    /// Unix timestamp of when the share was created.
    pub created_at_secs: u64,
    /// Unix timestamp of when the share expires.
    pub expires_at_secs: u64,
    /// Human-readable expiry string from the relay response.
    pub expires_at: String,
}

impl ActiveShare {
    /// Returns a human-friendly "expires in X" string relative to now.
    #[must_use]
    pub fn expires_in_display(&self) -> String {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or(Duration::ZERO)
            .as_secs();
        if self.expires_at_secs <= now {
            return "expired".to_string();
        }
        let remaining = self.expires_at_secs - now;
        let hours = remaining / 3600;
        let minutes = (remaining % 3600) / 60;
        if hours >= 1 {
            format!("{hours}h {minutes}m")
        } else {
            format!("{minutes}m")
        }
    }
}

// ─── ShareClient ─────────────────────────────────────────────────────────────

/// Async client for the share relay.
///
/// Designed to be used from synchronous handlers via
/// `tokio::runtime::Handle::block_on`.
pub struct ShareClient {
    base_url: String,
    http: reqwest::Client,
}

impl ShareClient {
    /// Build a client pointing at `base_url` (default: `SHARE_RELAY_BASE`).
    #[must_use]
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(10))
                .build()
                .unwrap_or_default(),
        }
    }

    /// Build a client using the default relay URL.
    #[must_use]
    pub fn default_client() -> Self {
        Self::new(SHARE_RELAY_BASE)
    }

    /// `POST /v1/share/sessions` — create a new share and return the `ActiveShare`.
    ///
    /// Checks the local rate limiter before making the network call.
    pub async fn create_share(
        &self,
        tab_id: impl Into<String>,
        tab_name: impl Into<String>,
        snapshot: ShareSnapshot,
        ttl_seconds: u64,
    ) -> Result<ActiveShare, ShareError> {
        // Rate limit gate — checked before any network call.
        {
            let mut limiter = rate_limiter()
                .lock()
                .map_err(|_| ShareError::RelayUnavailable("internal lock poisoned".to_string()))?;
            if !limiter.check_and_record() {
                return Err(ShareError::RateLimitExceeded);
            }
        }

        let tab_id = tab_id.into();
        let tab_name = tab_name.into();

        let url = format!("{}/v1/share/sessions", self.base_url);
        let body = CreateShareRequest {
            snapshot: snapshot.clone(),
            ttl_seconds,
        };

        let resp = self
            .http
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| ShareError::RelayUnavailable(e.to_string()))?;

        let status = resp.status();
        if status.as_u16() == 404 {
            return Err(ShareError::RelayNotFound);
        }
        if !status.is_success() {
            return Err(ShareError::RelayUnavailable(format!("HTTP {status}")));
        }

        let parsed: CreateShareResponse = resp
            .json()
            .await
            .map_err(|e| ShareError::ParseError(e.to_string()))?;

        // Parse `expires_at` back to Unix seconds for `expires_in_display`.
        let expires_at_secs = parse_iso8601_to_unix(&parsed.expires_at)
            .unwrap_or_else(|| {
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or(Duration::ZERO)
                    .as_secs()
                    + ttl_seconds
            });

        let created_at_secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or(Duration::ZERO)
            .as_secs();

        Ok(ActiveShare {
            share_id: parsed.share_id,
            url: parsed.url,
            tab_id,
            tab_name,
            created_at_secs,
            expires_at_secs,
            expires_at: parsed.expires_at,
        })
    }

    /// `DELETE /v1/share/sessions/:id` — revoke a share.
    ///
    /// Uses the `share_id` as a capability token (no auth header required).
    pub async fn delete_share(&self, share_id: &str) -> Result<(), ShareError> {
        let url = format!("{}/v1/share/sessions/{}", self.base_url, share_id);
        let resp = self
            .http
            .delete(&url)
            .send()
            .await
            .map_err(|e| ShareError::RelayUnavailable(e.to_string()))?;

        let status = resp.status();
        if status.as_u16() == 404 {
            // Already gone — treat as success.
            return Ok(());
        }
        if !status.is_success() {
            return Err(ShareError::RelayUnavailable(format!("HTTP {status}")));
        }
        Ok(())
    }
}

// ─── ISO-8601 parser (minimal) ───────────────────────────────────────────────

/// Parse a simple ISO-8601 UTC string (`YYYY-MM-DDTHH:MM:SSZ`) to Unix seconds.
///
/// Returns `None` when the string is malformed.
fn parse_iso8601_to_unix(s: &str) -> Option<u64> {
    // Accept: "2026-04-21T12:00:00Z" or "2026-04-21T12:00:00+00:00"
    let s = s.trim_end_matches('Z').trim_end_matches("+00:00");
    let parts: Vec<&str> = s.splitn(2, 'T').collect();
    if parts.len() != 2 {
        return None;
    }
    let date_parts: Vec<u32> = parts[0].split('-').filter_map(|p| p.parse().ok()).collect();
    let time_parts: Vec<u32> = parts[1].split(':').filter_map(|p| p.parse().ok()).collect();
    if date_parts.len() != 3 || time_parts.len() != 3 {
        return None;
    }
    let (year, month, day) = (date_parts[0], date_parts[1], date_parts[2]);
    let (hour, minute, second) = (time_parts[0], time_parts[1], time_parts[2]);
    // Convert to Unix via the inverse of our format function.
    ymd_hms_to_unix(year, month, day, hour, minute, second)
}

/// Inverse of `unix_to_ymd_hms` — convert civil date/time (UTC) to Unix secs.
fn ymd_hms_to_unix(year: u32, month: u32, day: u32, hour: u32, minute: u32, second: u32) -> Option<u64> {
    // Days from civil date to Unix epoch (Hinnant algorithm, inverse).
    let y = if month <= 2 {
        i64::from(year) - 1
    } else {
        i64::from(year)
    };
    let m = i64::from(month);
    let d = i64::from(day);
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    if days < 0 {
        return None;
    }
    let secs = days as u64 * 86_400
        + u64::from(hour) * 3600
        + u64::from(minute) * 60
        + u64::from(second);
    Some(secs)
}

// ─── Blocking wrapper ────────────────────────────────────────────────────────

/// Blocking wrapper around [`ShareClient`] for use in synchronous command
/// handlers that cannot `.await` (mirrors the `BlockingHubClient` pattern).
pub struct BlockingShareClient {
    inner: ShareClient,
    rt: tokio::runtime::Handle,
}

impl BlockingShareClient {
    /// Build from an existing tokio runtime handle.
    #[must_use]
    pub fn new(base_url: impl Into<String>, rt: tokio::runtime::Handle) -> Self {
        Self {
            inner: ShareClient::new(base_url),
            rt,
        }
    }

    /// Build using the default relay URL and an existing tokio handle.
    #[must_use]
    pub fn default_client(rt: tokio::runtime::Handle) -> Self {
        Self::new(SHARE_RELAY_BASE, rt)
    }

    pub fn create_share(
        &self,
        tab_id: impl Into<String>,
        tab_name: impl Into<String>,
        snapshot: ShareSnapshot,
        ttl_seconds: u64,
    ) -> Result<ActiveShare, ShareError> {
        self.rt.block_on(
            self.inner
                .create_share(tab_id, tab_name, snapshot, ttl_seconds),
        )
    }

    pub fn delete_share(&self, share_id: &str) -> Result<(), ShareError> {
        self.rt.block_on(self.inner.delete_share(share_id))
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Scrubber ──────────────────────────────────────────────────────────────

    #[test]
    fn scrubber_clean_text_passes_through() {
        let clean = "Hello, world!  This is a normal conversation message.";
        assert_eq!(scrub_secrets(clean), clean);
    }

    #[test]
    fn scrubber_removes_openai_key() {
        let text = "my key is sk-abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUV done";
        let result = scrub_secrets(text);
        assert!(!result.contains("sk-abcde"), "OpenAI key should be redacted");
        assert!(result.contains("[REDACTED]"), "should have REDACTED marker");
    }

    #[test]
    fn scrubber_removes_anthropic_key() {
        let text = "token: sk-ant-api03-abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOP end";
        let result = scrub_secrets(text);
        assert!(!result.contains("sk-ant-api"), "Anthropic key should be redacted");
        assert!(result.contains("[REDACTED]"));
    }

    #[test]
    fn scrubber_removes_aws_key() {
        let text = "export AWS_ACCESS_KEY_ID=AKIAIOSFODNN7EXAMPLE";
        let result = scrub_secrets(text);
        assert!(!result.contains("AKIAIOSFODNN7EXAMPLE"), "AWS key should be redacted");
    }

    #[test]
    fn scrubber_removes_private_key_header() {
        let text = "-----BEGIN RSA PRIVATE KEY-----\nMIIEpAIBAAKCAQEA...";
        let result = scrub_secrets(text);
        assert!(!result.contains("BEGIN RSA PRIVATE KEY"), "PK header should be redacted");
    }

    // ── Rate limiter ──────────────────────────────────────────────────────────

    #[test]
    fn rate_limiter_allows_up_to_ten() {
        let mut limiter = RateLimiter::new();
        for i in 0..10 {
            assert!(limiter.check_and_record(), "call {i} should be allowed");
        }
        assert!(!limiter.check_and_record(), "11th call should be rejected");
    }

    #[test]
    fn rate_limiter_window_expires() {
        let mut limiter = RateLimiter::new();
        // Manually backdate 10 timestamps to 2 hours ago.
        let old_ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or(Duration::ZERO)
            .as_secs()
            .saturating_sub(7300); // > 2 hours ago
        for _ in 0..10 {
            limiter.timestamps.push(old_ts);
        }
        // All 10 are outside the 1-hour window — a new share should be allowed.
        assert!(limiter.check_and_record(), "should allow after old timestamps expire");
    }

    // ── Snapshot builder ──────────────────────────────────────────────────────

    #[test]
    fn snapshot_build_scrubs_messages() {
        let messages = vec![
            ShareMessage {
                role: "user".to_string(),
                // Anthropic key: sk-ant- followed by ≥40 chars to match the scrubber regex.
                content: "my api key is sk-ant-api03-abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQR"
                    .to_string(),
            },
            ShareMessage {
                role: "assistant".to_string(),
                content: "Got it, I see your key.".to_string(),
            },
        ];
        let snap = ShareSnapshot::build("Main", "claude-sonnet-4-6", messages);
        assert!(
            !snap.messages[0].content.contains("sk-ant"),
            "API key should be scrubbed from snapshot"
        );
        assert_eq!(snap.tab_name, "Main");
        assert_eq!(snap.model, "claude-sonnet-4-6");
    }

    // ── ISO-8601 round-trip ───────────────────────────────────────────────────

    #[test]
    fn format_and_parse_unix_timestamp_round_trips() {
        // Pick a known Unix timestamp: 2026-04-20T00:00:00Z
        let secs: u64 = 1_776_643_200;
        let formatted = format_unix_utc(secs);
        assert_eq!(formatted, "2026-04-20T00:00:00Z");
        let parsed = parse_iso8601_to_unix(&formatted).expect("should parse back");
        assert_eq!(parsed, secs);
    }

    // ── ShareSnapshot serialization ───────────────────────────────────────────

    #[test]
    fn snapshot_serializes_to_expected_json_shape() {
        let snap = ShareSnapshot {
            tab_name: "Main".to_string(),
            model: "claude-sonnet-4-6".to_string(),
            created_at: "2026-04-20T00:00:00Z".to_string(),
            messages: vec![
                ShareMessage { role: "user".to_string(), content: "hello".to_string() },
                ShareMessage { role: "assistant".to_string(), content: "hi".to_string() },
            ],
        };
        let json = serde_json::to_string(&snap).expect("serialization should not fail");
        assert!(json.contains("\"tab_name\""));
        assert!(json.contains("\"messages\""));
        assert!(json.contains("\"role\""));
    }
}
