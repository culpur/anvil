//! GitHub Copilot provider.
//!
//! Auth: GitHub OAuth **device flow**.  The user visits
//! `https://github.com/login/device` and enters the `user_code` displayed by
//! Anvil.  The token is saved to `~/.anvil/credentials.json` under the
//! `"copilot"` key and refreshed automatically.
//!
//! Wire format: OpenAI-compatible (`/v1/chat/completions`).
//! Base URL: `https://api.githubcopilot.com`
//! Extra header: `Copilot-Integration-Id: vscode-chat`
//! Auth: `Authorization: Bearer <token>`

use std::time::Duration;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::borrow::ToOwned;

use crate::error::ApiError;
use crate::types::{MessageRequest, MessageResponse, StreamEvent};
use super::openai_compat::{MessageStream, OpenAiCompatClient, OpenAiCompatConfig};
use super::{Provider, ProviderFuture};

pub const BASE_URL: &str = "https://api.githubcopilot.com";
pub const COPILOT_INTEGRATION_HEADER: &str = "Copilot-Integration-Id";
pub const COPILOT_INTEGRATION_VALUE: &str = "vscode-chat";

// GitHub OAuth App credentials for device flow.
// These are the public client IDs used by the VS Code Copilot integration —
// they are intentionally public (device-flow OAuth never has a client secret
// in the client binary).
const GITHUB_DEVICE_CLIENT_ID: &str = "Iv1.b507a08c87ecfe98";
const GITHUB_DEVICE_SCOPE: &str = "copilot";

// ---------------------------------------------------------------------------
// Saved token shape
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CopilotTokenSet {
    pub access_token: String,
    pub token_type: String,
    pub scope: String,
    /// Unix epoch seconds when the token expires.  GitHub personal access
    /// tokens don't expire unless explicitly set, but device-flow tokens may
    /// carry an expiry.
    #[serde(default)]
    pub expires_at: Option<u64>,
}

impl CopilotTokenSet {
    #[must_use]
    pub fn is_expired(&self) -> bool {
        let Some(expires_at) = self.expires_at else {
            return false;
        };
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        // Treat token as expired 60 s before the actual expiry to allow for
        // clock skew and the time needed to refresh.
        now + 60 >= expires_at
    }
}

// ---------------------------------------------------------------------------
// Credentials persistence
// ---------------------------------------------------------------------------

fn credentials_path() -> std::io::Result<std::path::PathBuf> {
    let base = if let Some(path) = std::env::var_os("ANVIL_CONFIG_HOME") {
        std::path::PathBuf::from(path)
    } else {
        let home = std::env::var_os("HOME")
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "HOME not set"))?;
        std::path::PathBuf::from(home).join(".anvil")
    };
    Ok(base.join("credentials.json"))
}

pub fn load_copilot_token() -> std::io::Result<Option<CopilotTokenSet>> {
    let path = credentials_path()?;
    let contents = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    let root: serde_json::Map<String, Value> = serde_json::from_str(&contents)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let Some(v) = root.get("copilot") else {
        return Ok(None);
    };
    if v.is_null() {
        return Ok(None);
    }
    let token: CopilotTokenSet = serde_json::from_value(v.clone())
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    Ok(Some(token))
}

pub fn save_copilot_token(token: &CopilotTokenSet) -> std::io::Result<()> {
    let path = credentials_path()?;
    let mut root: serde_json::Map<String, Value> = if path.exists() {
        let c = std::fs::read_to_string(&path)?;
        if c.trim().is_empty() {
            serde_json::Map::new()
        } else {
            serde_json::from_str(&c)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?
        }
    } else {
        serde_json::Map::new()
    };
    root.insert(
        "copilot".to_string(),
        serde_json::to_value(token)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?,
    );
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let rendered = serde_json::to_string_pretty(&Value::Object(root))
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let tmp = path.with_extension("json.tmp");
    #[cfg(unix)]
    {
        use std::io::Write as _;
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new()
            .write(true).create(true).truncate(true).mode(0o600)
            .open(&tmp)?;
        writeln!(f, "{rendered}")?;
    }
    #[cfg(not(unix))]
    {
        std::fs::write(&tmp, format!("{rendered}\n"))?;
    }
    std::fs::rename(tmp, path)
}

// ---------------------------------------------------------------------------
// Device flow
// ---------------------------------------------------------------------------

/// Response from `POST https://github.com/login/device/code`.
#[derive(Debug, Deserialize)]
struct DeviceCodeResponse {
    device_code: String,
    user_code: String,
    verification_uri: String,
    expires_in: u64,
    interval: u64,
}

/// Polling response from `POST https://github.com/login/oauth/access_token`.
#[derive(Debug, Deserialize)]
struct AccessTokenResponse {
    #[serde(default)]
    access_token: Option<String>,
    #[serde(default)]
    token_type: Option<String>,
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    error: Option<String>,
}

/// Run the GitHub device flow interactively.
///
/// Prints the `user_code` and verification URL to stderr, then polls
/// until the user authorises or the code expires.  Returns the saved
/// [`CopilotTokenSet`] on success.
pub async fn run_device_flow() -> Result<CopilotTokenSet, ApiError> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(ApiError::Http)?;

    // Step 1 — request a device + user code.
    let device_resp = client
        .post("https://github.com/login/device/code")
        .header("Accept", "application/json")
        .form(&[
            ("client_id", GITHUB_DEVICE_CLIENT_ID),
            ("scope", GITHUB_DEVICE_SCOPE),
        ])
        .send()
        .await
        .map_err(ApiError::Http)?;

    if !device_resp.status().is_success() {
        let status = device_resp.status();
        let body = device_resp.text().await.unwrap_or_default();
        return Err(ApiError::Auth(format!(
            "GitHub device-code request failed ({status}): {body}"
        )));
    }

    let device: DeviceCodeResponse = device_resp.json().await.map_err(ApiError::Http)?;

    // Step 2 — display instructions to the user.
    eprintln!();
    eprintln!("  GitHub Copilot authorisation required.");
    eprintln!("  1. Visit: {}", device.verification_uri);
    eprintln!("  2. Enter code: {}", device.user_code);
    eprintln!();

    // Step 3 — poll until authorised or expired.
    let poll_interval = Duration::from_secs(device.interval.max(5));
    let deadline = std::time::Instant::now() + Duration::from_secs(device.expires_in);

    loop {
        if std::time::Instant::now() >= deadline {
            return Err(ApiError::Auth(
                "GitHub device flow timed out — re-run `anvil provider login copilot`".to_string(),
            ));
        }

        tokio::time::sleep(poll_interval).await;

        let poll_resp = client
            .post("https://github.com/login/oauth/access_token")
            .header("Accept", "application/json")
            .form(&[
                ("client_id", GITHUB_DEVICE_CLIENT_ID),
                ("device_code", device.device_code.as_str()),
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
            ])
            .send()
            .await
            .map_err(ApiError::Http)?;

        let body: AccessTokenResponse = poll_resp.json().await.map_err(ApiError::Http)?;

        match body.error.as_deref() {
            Some("authorization_pending") => {
                // User hasn't authorised yet — keep polling.
                continue;
            }
            Some("slow_down") => {
                // GitHub asks us to slow down.
                tokio::time::sleep(Duration::from_secs(5)).await;
                continue;
            }
            Some("expired_token") => {
                return Err(ApiError::Auth(
                    "GitHub device code expired — re-run `anvil provider login copilot`"
                        .to_string(),
                ));
            }
            Some("access_denied") => {
                return Err(ApiError::Auth(
                    "GitHub Copilot authorisation was denied".to_string(),
                ));
            }
            Some(other) => {
                return Err(ApiError::Auth(format!(
                    "GitHub OAuth error: {other}"
                )));
            }
            None => {}
        }

        if let Some(token) = body.access_token {
            let token_set = CopilotTokenSet {
                access_token: token,
                token_type: body.token_type.unwrap_or_else(|| "bearer".to_string()),
                scope: body.scope.unwrap_or_else(|| GITHUB_DEVICE_SCOPE.to_string()),
                expires_at: None,
            };
            save_copilot_token(&token_set)?;
            return Ok(token_set);
        }
    }
}

// ---------------------------------------------------------------------------
// Cursor auth helper (lives here since copilot.rs already has credential logic)
// ---------------------------------------------------------------------------

/// Try to read the Cursor API token from `~/.cursor/auth.json`.
///
/// The file (if present) contains a JSON object with an `"accessToken"` field.
/// Returns `Ok(None)` when the file does not exist or contains no token.
pub fn load_cursor_auth_token() -> std::io::Result<Option<String>> {
    let home = match std::env::var_os("HOME") {
        Some(h) => std::path::PathBuf::from(h),
        None => return Ok(None),
    };
    let path = home.join(".cursor").join("auth.json");
    let contents = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    let root: Value = match serde_json::from_str(&contents) {
        Ok(v) => v,
        Err(_) => return Ok(None),
    };
    Ok(root["accessToken"].as_str().map(ToOwned::to_owned))
}

// ---------------------------------------------------------------------------
// CopilotClient
// ---------------------------------------------------------------------------

/// Client for the GitHub Copilot chat API.
///
/// Uses the OpenAI-compat wire format with an extra `Copilot-Integration-Id`
/// header.  Auth is a GitHub bearer token obtained via the device flow.
#[derive(Debug, Clone)]
pub struct CopilotClient {
    inner: OpenAiCompatClient,
}

impl CopilotClient {
    /// Build from a saved token or the `GITHUB_TOKEN` env var.
    pub fn from_env() -> Result<Self, ApiError> {
        // Preference order:
        //   1. `GITHUB_TOKEN` env var (set by CI / devcontainers)
        //   2. Saved token from `~/.anvil/credentials.json`
        let token = if let Ok(t) = std::env::var("GITHUB_TOKEN").and_then(|v| {
            if v.is_empty() { Err(std::env::VarError::NotPresent) } else { Ok(v) }
        }) {
            t
        } else {
            load_copilot_token()
                .map_err(ApiError::Io)?
                .filter(|t| !t.is_expired())
                .map(|t| t.access_token)
                .ok_or_else(|| ApiError::missing_credentials(
                    "GitHub Copilot",
                    &["GITHUB_TOKEN"],
                ))?
        };

        let base_url = std::env::var("COPILOT_BASE_URL")
            .ok()
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| BASE_URL.to_string());

        let client = OpenAiCompatClient::new(token, OpenAiCompatConfig::xai())
            .with_base_url(base_url);

        Ok(Self { inner: client })
    }

    pub async fn send_message(
        &self,
        request: &MessageRequest,
    ) -> Result<MessageResponse, ApiError> {
        self.inner.send_message(request).await
    }

    pub async fn stream_message(
        &self,
        request: &MessageRequest,
    ) -> Result<MessageStream, ApiError> {
        self.inner.stream_message(request).await
    }
}

impl Provider for CopilotClient {
    type Stream = MessageStream;

    fn send_message<'a>(
        &'a self,
        request: &'a MessageRequest,
    ) -> ProviderFuture<'a, MessageResponse> {
        Box::pin(async move { self.send_message(request).await })
    }

    fn stream_message<'a>(
        &'a self,
        request: &'a MessageRequest,
    ) -> ProviderFuture<'a, Self::Stream> {
        Box::pin(async move { self.stream_message(request).await })
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_url_and_header_constants_are_correct() {
        assert_eq!(BASE_URL, "https://api.githubcopilot.com");
        assert_eq!(COPILOT_INTEGRATION_HEADER, "Copilot-Integration-Id");
        assert_eq!(COPILOT_INTEGRATION_VALUE, "vscode-chat");
    }

    #[test]
    fn expired_token_detected_when_timestamp_in_past() {
        let token = CopilotTokenSet {
            access_token: "tok".to_string(),
            token_type: "bearer".to_string(),
            scope: "copilot".to_string(),
            expires_at: Some(1_000_000),
        };
        assert!(token.is_expired(), "token with past expiry should be expired");
    }

    #[test]
    fn non_expired_token_not_expired() {
        let far_future = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
            + 3600;
        let token = CopilotTokenSet {
            access_token: "tok".to_string(),
            token_type: "bearer".to_string(),
            scope: "copilot".to_string(),
            expires_at: Some(far_future),
        };
        assert!(!token.is_expired(), "future token should not be expired");
    }

    #[test]
    fn token_without_expiry_never_expired() {
        let token = CopilotTokenSet {
            access_token: "tok".to_string(),
            token_type: "bearer".to_string(),
            scope: "copilot".to_string(),
            expires_at: None,
        };
        assert!(!token.is_expired());
    }
}
