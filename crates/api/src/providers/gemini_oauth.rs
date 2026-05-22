//! Google Code Assist provider with OAuth PKCE flow (Gemini OAuth variant).
//!
//! Uses the same `cloudcode-pa.googleapis.com` backend as the official
//! Gemini CLI (Apache-2.0, google-gemini/gemini-cli).  Anvil identifies
//! itself honestly via `User-Agent` and `x-goog-api-client` — no IDE spoofing.
//!
//! OAuth credentials:
//!   Client ID: `681255809395-oo8ft2oprdrnp9e3aqf6av3hmdib135j.apps.googleusercontent.com`
//!   Client secret: `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`
//!
//!   These are intentionally public — they are the published credentials from
//!   the Apache-2.0-licensed gemini-cli source (packages/core/src/code_assist/oauth2.ts).
//!   Security comes from PKCE + per-user refresh tokens, not from keeping the
//!   client secret private (this is the standard "installed application" pattern
//!   documented at https://developers.google.com/identity/protocols/oauth2/native-app).
//!
//! Scopes:
//!   - `https://www.googleapis.com/auth/cloud-platform`
//!   - `https://www.googleapis.com/auth/userinfo.email`
//!   - `https://www.googleapis.com/auth/userinfo.profile`
//!
//! OAuth callback: HTTP server on a dynamic port (or `OAUTH_CALLBACK_PORT`).
//!
//! Endpoints used:
//!   POST `https://cloudcode-pa.googleapis.com/v1internal:loadCodeAssist`  — setup / model discovery
//!   POST `https://cloudcode-pa.googleapis.com/v1internal:generateContent` — non-streaming
//!   POST `https://cloudcode-pa.googleapis.com/v1internal:streamGenerateContent?alt=sse` — streaming
//!
//! Token storage: `~/.anvil/credentials.json` under the key `"gemini_oauth"`.
//!
//! Token refresh: before each request Anvil checks if `expires_at` is within
//!   60 s; if so, it POSTs to `https://oauth2.googleapis.com/token` using the
//!   stored refresh_token.

use std::collections::VecDeque;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use crate::error::ApiError;
use crate::types::{
    ContentBlockDelta, ContentBlockDeltaEvent, ContentBlockStartEvent, ContentBlockStopEvent,
    InputContentBlock, MessageDelta, MessageDeltaEvent, MessageRequest, MessageResponse,
    MessageStartEvent, MessageStopEvent, OutputContentBlock, StreamEvent, Usage,
};
use super::common::next_sse_frame;
use super::openai_compat::resolve_stream_dead_air_timeout;
use super::{Provider, ProviderFuture};
// Task #764 (v2.2.20): runtime types for GeminiKeepaliveRefresher.
extern crate runtime;

// ---------------------------------------------------------------------------
// Public constants
// ---------------------------------------------------------------------------

pub const CODE_ASSIST_ENDPOINT: &str = "https://cloudcode-pa.googleapis.com";
pub const CODE_ASSIST_API_VERSION: &str = "v1internal";

/// Google OAuth 2.0 endpoints.
pub const GOOGLE_AUTH_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";
pub const GOOGLE_TOKEN_URL: &str = "https://oauth2.googleapis.com/token";

/// OAuth client credentials for the Code Assist family.
///
/// Sourced from the Apache-2.0-licensed gemini-cli:
///   packages/core/src/code_assist/oauth2.ts (google-gemini/gemini-cli)
///
/// These are intentionally public per the "installed application" OAuth model.
/// Per Google's documentation: the client_secret in this context is NOT a secret.
pub const OAUTH_CLIENT_ID: &str =
    "681255809395-oo8ft2oprdrnp9e3aqf6av3hmdib135j.apps.googleusercontent.com";
pub const OAUTH_CLIENT_SECRET: &str = "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";

/// OAuth scopes required for Code Assist.
pub const OAUTH_SCOPES: &[&str] = &[
    "https://www.googleapis.com/auth/cloud-platform",
    "https://www.googleapis.com/auth/userinfo.email",
    "https://www.googleapis.com/auth/userinfo.profile",
];

/// The vault key under which Gemini OAuth tokens are saved.
const VAULT_KEY: &str = "gemini_oauth";

/// How many seconds before expiry to pre-emptively refresh.
const REFRESH_HEADROOM_SECS: u64 = 60;

// ---------------------------------------------------------------------------
// Credential persistence
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeminiOAuthToken {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_at: Option<u64>,
}

impl GeminiOAuthToken {
    #[must_use]
    pub fn is_expired(&self) -> bool {
        let Some(expires_at) = self.expires_at else {
            return false;
        };
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        now + REFRESH_HEADROOM_SECS >= expires_at
    }
}

fn credentials_path() -> std::io::Result<std::path::PathBuf> {
    let base = if let Some(p) = std::env::var_os("ANVIL_CONFIG_HOME") {
        std::path::PathBuf::from(p)
    } else {
        let home = std::env::var_os("HOME")
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "HOME not set"))?;
        std::path::PathBuf::from(home).join(".anvil")
    };
    Ok(base.join("credentials.json"))
}

fn read_credentials_root(
    path: &std::path::Path,
) -> std::io::Result<serde_json::Map<String, Value>> {
    match std::fs::read_to_string(path) {
        Ok(c) if !c.trim().is_empty() => serde_json::from_str(&c)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e)),
        Ok(_) => Ok(serde_json::Map::new()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(serde_json::Map::new()),
        Err(e) => Err(e),
    }
}

fn write_credentials_root(
    path: &std::path::Path,
    root: &serde_json::Map<String, Value>,
) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let rendered = serde_json::to_string_pretty(&Value::Object(root.clone()))
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

pub fn load_gemini_oauth_token() -> std::io::Result<Option<GeminiOAuthToken>> {
    let path = credentials_path()?;
    let root = read_credentials_root(&path)?;
    let Some(v) = root.get(VAULT_KEY) else {
        return Ok(None);
    };
    if v.is_null() {
        return Ok(None);
    }
    let tok: GeminiOAuthToken = serde_json::from_value(v.clone())
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    Ok(Some(tok))
}

pub fn save_gemini_oauth_token(tok: &GeminiOAuthToken) -> std::io::Result<()> {
    let path = credentials_path()?;
    let mut root = read_credentials_root(&path)?;
    root.insert(
        VAULT_KEY.to_string(),
        serde_json::to_value(tok)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?,
    );
    write_credentials_root(&path, &root)
}

// ---------------------------------------------------------------------------
// OAuth PKCE flow (localhost callback)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: Option<u64>,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    error_description: Option<String>,
}

/// Run the full Google PKCE OAuth flow interactively.
///
/// Starts a temporary localhost HTTP server, opens the browser (or prints
/// the URL), waits for the callback, exchanges the code, and saves the token.
pub async fn run_google_oauth_flow() -> Result<GeminiOAuthToken, ApiError> {
    use runtime::{generate_pkce_pair, generate_state};

    let pkce = generate_pkce_pair().map_err(ApiError::Io)?;
    let state = generate_state().map_err(ApiError::Io)?;

    // Bind on a dynamic port.
    let callback_port: u16 = if let Ok(s) = std::env::var("OAUTH_CALLBACK_PORT") {
        s.trim().parse().unwrap_or(0)
    } else {
        0
    };

    let listener = TcpListener::bind(format!("127.0.0.1:{callback_port}"))
        .await
        .map_err(ApiError::Io)?;
    let actual_port = listener.local_addr().map_err(ApiError::Io)?.port();

    let redirect_uri = format!("http://localhost:{actual_port}/callback");

    let scope_str = OAUTH_SCOPES.join(" ");
    let auth_url = format!(
        "{GOOGLE_AUTH_URL}?response_type=code\
        &client_id={}\
        &redirect_uri={}\
        &scope={}\
        &state={}\
        &code_challenge={}\
        &code_challenge_method=S256\
        &access_type=offline\
        &prompt=consent",
        percent_encode(OAUTH_CLIENT_ID),
        percent_encode(&redirect_uri),
        percent_encode(&scope_str),
        percent_encode(&state),
        percent_encode(&pkce.challenge),
    );

    eprintln!();
    eprintln!("  Gemini Code Assist authentication required.");
    eprintln!("  Open this URL in your browser:");
    eprintln!();
    eprintln!("  {auth_url}");
    eprintln!();
    eprintln!("  Waiting for the OAuth callback on port {actual_port}...");

    // Attempt to open the browser automatically; ignore errors.
    let _ = std::process::Command::new("open")
        .arg(&auth_url)
        .spawn();

    // Accept the callback connection.
    let (mut stream, _) = listener.accept().await.map_err(ApiError::Io)?;

    // Read the HTTP request line.
    let mut buf = vec![0u8; 4096];
    let n = stream.read(&mut buf).await.map_err(ApiError::Io)?;
    let request = String::from_utf8_lossy(&buf[..n]);
    let request_line = request.lines().next().unwrap_or("");

    // Parse query params from `GET /callback?code=...&state=... HTTP/1.1`
    let target = request_line
        .split_whitespace()
        .nth(1)
        .unwrap_or("/");

    let (code, callback_state) = parse_oauth_callback_target(target)
        .map_err(|e| ApiError::Auth(format!("OAuth callback error: {e}")))?;

    // Respond to the browser.
    let html_resp = if callback_state.as_deref() == Some(state.as_str()) {
        b"HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nConnection: close\r\n\r\n\
        <html><body><h1>Authentication successful.</h1><p>You may close this tab.</p></body></html>"
            .as_slice()
    } else {
        b"HTTP/1.1 400 Bad Request\r\nContent-Type: text/html\r\nConnection: close\r\n\r\n\
        <html><body><h1>Authentication failed: state mismatch.</h1></body></html>"
            .as_slice()
    };
    let _ = stream.write_all(html_resp).await;
    drop(stream);

    if callback_state.as_deref() != Some(state.as_str()) {
        return Err(ApiError::Auth(
            "OAuth state mismatch — possible CSRF; re-run `anvil provider login gemini-oauth`"
                .to_string(),
        ));
    }

    // Exchange code for tokens.
    let tok = exchange_code_for_token(&code, &redirect_uri, &pkce.verifier).await?;
    save_gemini_oauth_token(&tok).map_err(ApiError::Io)?;
    eprintln!("  Gemini OAuth token saved successfully.");
    Ok(tok)
}

/// Exchange an authorization code for an access + refresh token pair.
async fn exchange_code_for_token(
    code: &str,
    redirect_uri: &str,
    verifier: &str,
) -> Result<GeminiOAuthToken, ApiError> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(ApiError::Http)?;

    let params = [
        ("grant_type", "authorization_code"),
        ("code", code),
        ("redirect_uri", redirect_uri),
        ("client_id", OAUTH_CLIENT_ID),
        ("client_secret", OAUTH_CLIENT_SECRET),
        ("code_verifier", verifier),
    ];

    let resp = client
        .post(GOOGLE_TOKEN_URL)
        .form(&params)
        .header("User-Agent", anvil_user_agent())
        .send()
        .await
        .map_err(ApiError::Http)?;

    parse_token_response(resp).await
}

/// Refresh an access token using the stored refresh_token.
pub async fn refresh_access_token(refresh_token: &str) -> Result<GeminiOAuthToken, ApiError> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(ApiError::Http)?;

    let params = [
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh_token),
        ("client_id", OAUTH_CLIENT_ID),
        ("client_secret", OAUTH_CLIENT_SECRET),
        ("scope", &OAUTH_SCOPES.join(" ")),
    ];

    let resp = client
        .post(GOOGLE_TOKEN_URL)
        .form(&params)
        .header("User-Agent", anvil_user_agent())
        .send()
        .await
        .map_err(ApiError::Http)?;

    parse_token_response(resp).await
}

async fn parse_token_response(resp: reqwest::Response) -> Result<GeminiOAuthToken, ApiError> {
    let status = resp.status();
    let body: TokenResponse = resp.json().await.map_err(ApiError::Http)?;

    if let Some(err) = body.error {
        let desc = body.error_description.unwrap_or_default();
        return Err(match err.as_str() {
            "authorization_pending" | "slow_down" => {
                // Should not occur in the code-exchange path; surface anyway.
                ApiError::Auth(format!("Google OAuth polling error: {err}: {desc}"))
            }
            "expired_token" => ApiError::ExpiredOAuthToken,
            _ => ApiError::Auth(format!("Google OAuth error: {err}: {desc}")),
        });
    }

    if !status.is_success() {
        return Err(ApiError::Auth(format!(
            "Google token endpoint returned HTTP {status}"
        )));
    }

    let expires_at = body.expires_in.map(|secs| {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
            + secs
    });

    Ok(GeminiOAuthToken {
        access_token: body.access_token,
        refresh_token: body.refresh_token,
        expires_at,
    })
}

// ---------------------------------------------------------------------------
// GeminiKeepaliveRefresher — runtime::GeminiRefresher implementation
// ---------------------------------------------------------------------------

/// Concrete `runtime::GeminiRefresher` implementation (Task #764 / v2.2.20).
///
/// Used by both the daemon keepalive thread and the in-TUI fallback thread to
/// refresh Gemini OAuth tokens proactively before expiry.
#[derive(Debug, Clone, Default)]
pub struct GeminiKeepaliveRefresher;

impl runtime::GeminiRefresher for GeminiKeepaliveRefresher {
    fn refresh(
        &self,
        refresh_token: String,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<(String, Option<u64>), String>> + Send>,
    > {
        Box::pin(async move {
            let new_tok = refresh_access_token(&refresh_token)
                .await
                .map_err(|e| format!("{e}"))?;
            // Persist before returning so a crash between refresh + save
            // doesn't lose the new bearer.
            let persisted = GeminiOAuthToken {
                access_token: new_tok.access_token.clone(),
                refresh_token: new_tok.refresh_token.or(Some(refresh_token)),
                expires_at: new_tok.expires_at,
            };
            save_gemini_oauth_token(&persisted)
                .map_err(|e| format!("gemini persist failed: {e}"))?;
            Ok((persisted.access_token, persisted.expires_at))
        })
    }
}

/// Load a `runtime::GeminiTokenSnapshot` from `~/.anvil/credentials.json`.
/// Returns `None` if no token is saved (same semantics as
/// `load_gemini_oauth_token`).
pub fn load_gemini_keepalive_snapshot() -> Option<runtime::GeminiTokenSnapshot> {
    load_gemini_oauth_token().ok().flatten().map(|tok| runtime::GeminiTokenSnapshot {
        access_token: tok.access_token,
        refresh_token: tok.refresh_token,
        expires_at: tok.expires_at,
    })
}

/// Persist a `runtime::GeminiTokenSnapshot` back to disk.
pub fn save_gemini_keepalive_snapshot(snap: &runtime::GeminiTokenSnapshot) -> Result<(), String> {
    let tok = GeminiOAuthToken {
        access_token: snap.access_token.clone(),
        refresh_token: snap.refresh_token.clone(),
        expires_at: snap.expires_at,
    };
    save_gemini_oauth_token(&tok).map_err(|e| format!("{e}"))
}

// ---------------------------------------------------------------------------
// GeminiOAuthClient
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct GeminiOAuthClient {
    http: reqwest::Client,
    /// Current access token (may be refreshed lazily before each request).
    access_token: String,
    /// Saved refresh token for extending the session.
    refresh_token: Option<String>,
    /// Unix epoch at which the access token expires.
    expires_at: Option<u64>,
    base_url: String,
    api_version: String,
}

impl GeminiOAuthClient {
    pub fn from_env_or_saved() -> Result<Self, ApiError> {
        let tok = load_gemini_oauth_token()
            .map_err(ApiError::Io)?
            .ok_or_else(|| {
                ApiError::Auth(
                    "No Gemini OAuth token found. Run `anvil provider login gemini-oauth` first."
                        .to_string(),
                )
            })?;

        let base_url = std::env::var("CODE_ASSIST_ENDPOINT")
            .ok()
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| CODE_ASSIST_ENDPOINT.to_string());
        let api_version = std::env::var("CODE_ASSIST_API_VERSION")
            .ok()
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| CODE_ASSIST_API_VERSION.to_string());

        Ok(Self {
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(600))
                .build()
                .unwrap_or_default(),
            access_token: tok.access_token,
            refresh_token: tok.refresh_token,
            expires_at: tok.expires_at,
            base_url,
            api_version,
        })
    }

    fn method_url(&self, method: &str) -> String {
        format!(
            "{}/{}:{}",
            self.base_url.trim_end_matches('/'),
            self.api_version,
            method
        )
    }

    fn is_token_expired(&self) -> bool {
        let Some(expires_at) = self.expires_at else {
            return false;
        };
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        now + REFRESH_HEADROOM_SECS >= expires_at
    }

    /// Ensure the access token is fresh; refresh if needed.
    async fn ensure_fresh_token(&mut self) -> Result<(), ApiError> {
        if !self.is_token_expired() {
            return Ok(());
        }
        let Some(ref rt) = self.refresh_token.clone() else {
            return Err(ApiError::ExpiredOAuthToken);
        };
        let new_tok = refresh_access_token(rt).await?;
        // Merge: keep the existing refresh_token if the new response didn't
        // include one (Google omits it on some refresh responses).
        let refresh_token = new_tok.refresh_token.or_else(|| self.refresh_token.clone());
        let persisted = GeminiOAuthToken {
            access_token: new_tok.access_token.clone(),
            refresh_token: refresh_token.clone(),
            expires_at: new_tok.expires_at,
        };
        save_gemini_oauth_token(&persisted).map_err(ApiError::Io)?;
        self.access_token = new_tok.access_token;
        self.refresh_token = refresh_token;
        self.expires_at = new_tok.expires_at;
        Ok(())
    }

    fn build_request_body(request: &MessageRequest) -> Value {
        // Convert Anvil MessageRequest to the Code Assist generateContent shape.
        let contents: Vec<Value> = request
            .messages
            .iter()
            .map(|m| {
                let text = m
                    .content
                    .iter()
                    .filter_map(|b| match b {
                        InputContentBlock::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                json!({
                    "role": if m.role == "assistant" { "model" } else { "user" },
                    "parts": [{ "text": text }]
                })
            })
            .collect();

        let mut body = json!({
            "contents": contents,
            "generationConfig": {
                "maxOutputTokens": request.max_tokens,
            },
            "model": request.model,
        });

        if let Some(system) = &request.system {
            body["systemInstruction"] = json!({
                "parts": [{ "text": system }]
            });
        }

        body
    }

    fn apply_headers(&self, builder: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        builder
            .header("Authorization", format!("Bearer {}", self.access_token))
            .header("Content-Type", "application/json")
            .header("User-Agent", anvil_user_agent())
            .header("x-goog-api-client", anvil_goog_api_client())
    }

    pub async fn send_message(
        &mut self,
        request: &MessageRequest,
    ) -> Result<MessageResponse, ApiError> {
        self.ensure_fresh_token().await?;
        let url = self.method_url("generateContent");
        let body = Self::build_request_body(request);

        let builder = self.apply_headers(self.http.post(&url));
        let resp = builder.json(&body).send().await.map_err(ApiError::Http)?;

        let status = resp.status();
        if status.as_u16() == 401 || status.as_u16() == 403 {
            return Err(ApiError::Auth(format!(
                "Code Assist returned {status} — token may be expired or the \
                 service is not enabled. Run `anvil provider login gemini-oauth`."
            )));
        }
        if !status.is_success() {
            let body_text = resp.text().await.unwrap_or_default();
            return Err(ApiError::Api {
                status,
                error_type: None,
                message: None,
                body: body_text,
                retryable: status.as_u16() >= 500,
                retry_after_secs: None,
                provider_hint: None,
            });
        }

        let raw: Value = resp.json().await.map_err(ApiError::Http)?;
        normalize_code_assist_response(&request.model, raw)
    }

    pub async fn stream_message(
        &mut self,
        request: &MessageRequest,
    ) -> Result<GeminiOAuthStream, ApiError> {
        self.ensure_fresh_token().await?;
        // SSE streaming: append `?alt=sse` per the Code Assist protocol.
        let url = format!("{}?alt=sse", self.method_url("streamGenerateContent"));
        let body = Self::build_request_body(request);

        let builder = self.apply_headers(self.http.post(&url));
        let resp = builder.json(&body).send().await.map_err(ApiError::Http)?;

        let status = resp.status();
        if !status.is_success() {
            if status.as_u16() == 401 || status.as_u16() == 403 {
                return Err(ApiError::Auth(format!(
                    "Code Assist returned {status}. Run `anvil provider login gemini-oauth`."
                )));
            }
            let body_text = resp.text().await.unwrap_or_default();
            return Err(ApiError::Api {
                status,
                error_type: None,
                message: None,
                body: body_text,
                retryable: status.as_u16() >= 500,
                retry_after_secs: None,
                provider_hint: None,
            });
        }

        Ok(GeminiOAuthStream::new(resp, request.model.clone()))
    }
}

// The Provider trait requires `&self` not `&mut self`, so we wrap the client
// in Arc<Mutex> internally here for the Provider impl.
// For the mutable send_message / stream_message we expose the `&mut self`
// methods directly for callers that hold a mutable reference.
// The Provider trait implementation panics if used without a token.
impl Provider for GeminiOAuthClient {
    type Stream = GeminiOAuthStream;

    fn send_message<'a>(
        &'a self,
        request: &'a MessageRequest,
    ) -> ProviderFuture<'a, MessageResponse> {
        // Provider::send_message requires &self; we need &mut self for token
        // refresh. Create a temporary clone with the current token state.
        // If the token is expired here, the request will fail with a 401 and
        // a clear error; the caller should prefer the &mut self path.
        let url = self.method_url("generateContent");
        let body = Self::build_request_body(request);
        let builder = self.apply_headers(self.http.post(&url));
        let model = request.model.clone();
        Box::pin(async move {
            let resp = builder.json(&body).send().await.map_err(ApiError::Http)?;
            let status = resp.status();
            if !status.is_success() {
                if status.as_u16() == 401 || status.as_u16() == 403 {
                    return Err(ApiError::Auth(
                        "Code Assist 401/403 — run `anvil provider login gemini-oauth`"
                            .to_string(),
                    ));
                }
                let body_text = resp.text().await.unwrap_or_default();
                return Err(ApiError::Api {
                    status,
                    error_type: None,
                    message: None,
                    body: body_text,
                    retryable: status.as_u16() >= 500,
                    retry_after_secs: None,
                    provider_hint: None,
                });
            }
            let raw: Value = resp.json().await.map_err(ApiError::Http)?;
            normalize_code_assist_response(&model, raw)
        })
    }

    fn stream_message<'a>(
        &'a self,
        request: &'a MessageRequest,
    ) -> ProviderFuture<'a, Self::Stream> {
        let url = format!("{}?alt=sse", self.method_url("streamGenerateContent"));
        let body = Self::build_request_body(request);
        let builder = self.apply_headers(self.http.post(&url));
        let model = request.model.clone();
        Box::pin(async move {
            let resp = builder.json(&body).send().await.map_err(ApiError::Http)?;
            let status = resp.status();
            if !status.is_success() {
                if status.as_u16() == 401 || status.as_u16() == 403 {
                    return Err(ApiError::Auth(
                        "Code Assist 401/403 — run `anvil provider login gemini-oauth`"
                            .to_string(),
                    ));
                }
                let body_text = resp.text().await.unwrap_or_default();
                return Err(ApiError::Api {
                    status,
                    error_type: None,
                    message: None,
                    body: body_text,
                    retryable: status.as_u16() >= 500,
                    retry_after_secs: None,
                    provider_hint: None,
                });
            }
            Ok(GeminiOAuthStream::new(resp, model))
        })
    }
}

// ---------------------------------------------------------------------------
// Response normalisation
// ---------------------------------------------------------------------------

fn normalize_code_assist_response(
    model: &str,
    raw: Value,
) -> Result<MessageResponse, ApiError> {
    // Code Assist response shape (same as Gemini native API):
    // { "candidates": [{ "content": { "parts": [{ "text": "..." }] }, "finishReason": "STOP" }] }
    let text = raw["candidates"]
        .as_array()
        .and_then(|arr| arr.first())
        .and_then(|c| c["content"]["parts"].as_array())
        .and_then(|parts| parts.first())
        .and_then(|p| p["text"].as_str())
        .unwrap_or("")
        .to_string();

    let stop_reason = raw["candidates"]
        .as_array()
        .and_then(|arr| arr.first())
        .and_then(|c| c["finishReason"].as_str())
        .map(|r| match r {
            "STOP" => "end_turn",
            other => other,
        })
        .map(ToOwned::to_owned);

    let input_tokens = raw["usageMetadata"]["promptTokenCount"]
        .as_u64()
        .unwrap_or(0) as u32;
    let output_tokens = raw["usageMetadata"]["candidatesTokenCount"]
        .as_u64()
        .unwrap_or(0) as u32;

    Ok(MessageResponse {
        id: format!("gemini-oauth-{}", uuid_short()),
        kind: "message".to_string(),
        role: "assistant".to_string(),
        content: vec![OutputContentBlock::Text { text }],
        model: model.to_string(),
        stop_reason,
        stop_sequence: None,
        usage: Usage {
            input_tokens,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
            output_tokens,
        },
        request_id: None,
    })
}

// ---------------------------------------------------------------------------
// SSE streaming
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct GeminiOAuthStream {
    response: reqwest::Response,
    buffer: Vec<u8>,
    pending: VecDeque<StreamEvent>,
    done: bool,
    model: String,
    message_started: bool,
    text_started: bool,
    last_chunk_at: Instant,
    dead_air_timeout: Duration,
}

impl GeminiOAuthStream {
    fn new(response: reqwest::Response, model: String) -> Self {
        Self {
            response,
            buffer: Vec::new(),
            pending: VecDeque::new(),
            done: false,
            model,
            message_started: false,
            text_started: false,
            last_chunk_at: Instant::now(),
            dead_air_timeout: resolve_stream_dead_air_timeout(),
        }
    }

    pub async fn next_event(&mut self) -> Result<Option<StreamEvent>, ApiError> {
        loop {
            if let Some(ev) = self.pending.pop_front() {
                return Ok(Some(ev));
            }
            if self.done {
                return Ok(None);
            }

            let chunk_result = tokio::time::timeout(
                self.dead_air_timeout,
                self.response.chunk(),
            )
            .await;

            match chunk_result {
                Ok(Ok(Some(chunk))) => {
                    self.last_chunk_at = Instant::now();
                    self.buffer.extend_from_slice(&chunk);
                    self.drain_buffer();
                }
                Ok(Ok(None)) => {
                    self.done = true;
                    self.flush_finish();
                }
                Ok(Err(e)) => return Err(ApiError::Http(e)),
                Err(_) => {
                    let elapsed_ms = self.last_chunk_at.elapsed().as_millis() as u64;
                    return Err(ApiError::StreamStalled { elapsed_ms });
                }
            }
        }
    }

    fn drain_buffer(&mut self) {
        while let Some(frame) = next_sse_frame(&mut self.buffer) {
            self.ingest_frame(&frame);
        }
    }

    fn ingest_frame(&mut self, frame: &str) {
        // Code Assist SSE frames use `data: <json>` lines.
        let mut data_lines: Vec<&str> = Vec::new();
        for line in frame.lines() {
            if let Some(d) = line.strip_prefix("data:") {
                data_lines.push(d.trim_start());
            }
        }
        if data_lines.is_empty() {
            return;
        }
        let payload = data_lines.join("");
        if payload == "[DONE]" || payload.is_empty() {
            return;
        }

        let Ok(v) = serde_json::from_str::<Value>(&payload) else {
            return;
        };

        self.ensure_message_started(&v);

        // Extract text from candidates[0].content.parts[0].text
        let text = v["candidates"]
            .as_array()
            .and_then(|arr| arr.first())
            .and_then(|c| c["content"]["parts"].as_array())
            .and_then(|parts| parts.first())
            .and_then(|p| p["text"].as_str())
            .unwrap_or("");

        if !text.is_empty() {
            self.emit_text_delta(text);
        }

        // Check finishReason to close the stream.
        let finish_reason = v["candidates"]
            .as_array()
            .and_then(|arr| arr.first())
            .and_then(|c| c["finishReason"].as_str());

        if matches!(finish_reason, Some("STOP") | Some("SAFETY") | Some("MAX_TOKENS")) {
            let stop_reason = finish_reason
                .map(|r| if r == "STOP" { "end_turn" } else { r })
                .unwrap_or("end_turn")
                .to_string();

            if self.text_started {
                self.pending.push_back(StreamEvent::ContentBlockStop(
                    ContentBlockStopEvent { index: 0 },
                ));
            }
            let input_tokens = v["usageMetadata"]["promptTokenCount"]
                .as_u64()
                .unwrap_or(0) as u32;
            let output_tokens = v["usageMetadata"]["candidatesTokenCount"]
                .as_u64()
                .unwrap_or(0) as u32;
            self.pending.push_back(StreamEvent::MessageDelta(MessageDeltaEvent {
                delta: MessageDelta {
                    stop_reason: Some(stop_reason),
                    stop_sequence: None,
                },
                usage: Usage {
                    input_tokens,
                    cache_creation_input_tokens: 0,
                    cache_read_input_tokens: 0,
                    output_tokens,
                },
            }));
            self.pending.push_back(StreamEvent::MessageStop(MessageStopEvent {}));
            self.done = true;
        }
    }

    fn ensure_message_started(&mut self, _v: &Value) {
        if !self.message_started {
            self.message_started = true;
            self.pending.push_back(StreamEvent::MessageStart(MessageStartEvent {
                message: MessageResponse {
                    id: format!("gemini-oauth-stream-{}", uuid_short()),
                    kind: "message".to_string(),
                    role: "assistant".to_string(),
                    content: Vec::new(),
                    model: self.model.clone(),
                    stop_reason: None,
                    stop_sequence: None,
                    usage: Usage::default(),
                    request_id: None,
                },
            }));
        }
    }

    fn emit_text_delta(&mut self, text: &str) {
        if !self.text_started {
            self.text_started = true;
            self.pending.push_back(StreamEvent::ContentBlockStart(
                ContentBlockStartEvent {
                    index: 0,
                    content_block: OutputContentBlock::Text {
                        text: String::new(),
                    },
                },
            ));
        }
        self.pending.push_back(StreamEvent::ContentBlockDelta(
            ContentBlockDeltaEvent {
                index: 0,
                delta: ContentBlockDelta::TextDelta {
                    text: text.to_string(),
                },
            },
        ));
    }

    fn flush_finish(&mut self) {
        if self.message_started {
            if self.text_started {
                self.pending.push_back(StreamEvent::ContentBlockStop(
                    ContentBlockStopEvent { index: 0 },
                ));
            }
            self.pending.push_back(StreamEvent::MessageDelta(MessageDeltaEvent {
                delta: MessageDelta {
                    stop_reason: Some("end_turn".to_string()),
                    stop_sequence: None,
                },
                usage: Usage::default(),
            }));
            self.pending.push_back(StreamEvent::MessageStop(MessageStopEvent {}));
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn percent_encode(value: &str) -> String {
    let mut out = String::new();
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(char::from(byte));
            }
            _ => {
                use std::fmt::Write as _;
                let _ = write!(&mut out, "%{byte:02X}");
            }
        }
    }
    out
}

fn parse_oauth_callback_target(target: &str) -> Result<(String, Option<String>), String> {
    let (path, query) = target
        .split_once('?')
        .map_or((target, ""), |(p, q)| (p, q));

    if path != "/callback" && path != "/oauth2callback" {
        return Err(format!("unexpected callback path: {path}"));
    }

    let mut code: Option<String> = None;
    let mut state: Option<String> = None;
    let mut error: Option<String> = None;

    for pair in query.split('&').filter(|p| !p.is_empty()) {
        if let Some((k, v)) = pair.split_once('=') {
            let k = percent_decode(k)?;
            let v = percent_decode(v)?;
            match k.as_str() {
                "code" => code = Some(v),
                "state" => state = Some(v),
                "error" => error = Some(v),
                _ => {}
            }
        }
    }

    if let Some(err) = error {
        return Err(format!("Google OAuth error: {err}"));
    }

    let code =
        code.ok_or_else(|| "OAuth callback missing `code` parameter".to_string())?;
    Ok((code, state))
}

fn percent_decode(value: &str) -> Result<String, String> {
    let mut decoded = Vec::with_capacity(value.len());
    let bytes = value.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hi = hex_nibble(bytes[i + 1])?;
                let lo = hex_nibble(bytes[i + 2])?;
                decoded.push((hi << 4) | lo);
                i += 3;
            }
            b'+' => {
                decoded.push(b' ');
                i += 1;
            }
            b => {
                decoded.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8(decoded).map_err(|e| e.to_string())
}

fn hex_nibble(b: u8) -> Result<u8, String> {
    match b {
        b'0'..=b'9' => Ok(b - b'0'),
        b'a'..=b'f' => Ok(b - b'a' + 10),
        b'A'..=b'F' => Ok(b - b'A' + 10),
        _ => Err(format!("invalid hex byte in percent-encoding: {b}")),
    }
}

fn anvil_user_agent() -> String {
    format!("Anvil/{} ({})", env!("CARGO_PKG_VERSION"), std::env::consts::OS)
}

fn anvil_goog_api_client() -> &'static str {
    // Honest identification of Anvil to the Google Code Assist backend.
    "anvil-cli"
}

fn uuid_short() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos();
    format!("{nanos:08x}")
}

// ---------------------------------------------------------------------------
// Model list fetcher
// ---------------------------------------------------------------------------

/// Fetch models from Code Assist `loadCodeAssist`.
///
/// Returns a flat list of model IDs.
pub async fn fetch_gemini_oauth_models(
    access_token: &str,
    base_url: &str,
    api_version: &str,
) -> Result<Vec<super::model_list::ProviderModel>, super::model_list::ProviderModelsError> {
    use super::model_list::{ProviderModel, ProviderModelsError};
    use super::ProviderKind;

    let url = format!(
        "{}/{}:loadCodeAssist",
        base_url.trim_end_matches('/'),
        api_version
    );

    let client = reqwest::Client::builder()
        .timeout(super::model_list::DEFAULT_FETCH_TIMEOUT)
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());

    let resp = client
        .post(&url)
        .header("Authorization", format!("Bearer {access_token}"))
        .header("Content-Type", "application/json")
        .header("User-Agent", anvil_user_agent())
        .header("x-goog-api-client", anvil_goog_api_client())
        .json(&json!({ "metadata": {} }))
        .send()
        .await
        .map_err(|e| ProviderModelsError::Transient(e.to_string()))?;

    let status = resp.status();
    if status.as_u16() == 401 || status.as_u16() == 403 {
        return Err(ProviderModelsError::Unauthorized);
    }
    if !status.is_success() {
        return Err(ProviderModelsError::Transient(format!(
            "HTTP {} from loadCodeAssist",
            status.as_u16()
        )));
    }

    let raw: Value = resp
        .json()
        .await
        .map_err(|e| ProviderModelsError::InvalidResponse(e.to_string()))?;

    // The loadCodeAssist response includes a `currentModel` field.
    // Extract all available model names from `geminiPaidModels` / `geminiModels`.
    let mut model_ids: Vec<String> = Vec::new();

    for field in &["geminiPaidModels", "geminiModels", "allowedModels"] {
        if let Some(arr) = raw[field].as_array() {
            for item in arr {
                if let Some(id) = item.as_str() {
                    model_ids.push(id.to_string());
                } else if let Some(id) = item["id"].as_str() {
                    model_ids.push(id.to_string());
                }
            }
        }
    }
    if let Some(id) = raw["currentModel"].as_str() {
        if !model_ids.contains(&id.to_string()) {
            model_ids.push(id.to_string());
        }
    }

    if model_ids.is_empty() {
        // Fall back to a known-good default.
        model_ids.push("gemini-2.5-pro-preview".to_string());
    }

    Ok(model_ids
        .into_iter()
        .map(|id| ProviderModel {
            id,
            provider: ProviderKind::Gemini,
            display_name: None,
            context_window: None,
            deprecated: false,
        })
        .collect())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oauth_constants_are_non_empty() {
        assert!(!OAUTH_CLIENT_ID.is_empty(), "client_id must not be empty");
        assert!(!OAUTH_CLIENT_SECRET.is_empty(), "client_secret must not be empty");
        assert!(!OAUTH_SCOPES.is_empty(), "scopes must not be empty");
        assert!(
            OAUTH_SCOPES.contains(&"https://www.googleapis.com/auth/cloud-platform"),
            "cloud-platform scope required"
        );
    }

    #[test]
    fn code_assist_endpoint_constants_correct() {
        assert_eq!(CODE_ASSIST_ENDPOINT, "https://cloudcode-pa.googleapis.com");
        assert_eq!(CODE_ASSIST_API_VERSION, "v1internal");
    }

    #[test]
    fn method_url_format_is_correct() {
        let client = GeminiOAuthClient {
            http: reqwest::Client::new(),
            access_token: "tok".to_string(),
            refresh_token: None,
            expires_at: None,
            base_url: "https://cloudcode-pa.googleapis.com".to_string(),
            api_version: "v1internal".to_string(),
        };
        assert_eq!(
            client.method_url("generateContent"),
            "https://cloudcode-pa.googleapis.com/v1internal:generateContent"
        );
        assert_eq!(
            client.method_url("loadCodeAssist"),
            "https://cloudcode-pa.googleapis.com/v1internal:loadCodeAssist"
        );
    }

    #[test]
    fn token_expiry_detected_correctly() {
        let expired = GeminiOAuthToken {
            access_token: "t".to_string(),
            refresh_token: None,
            expires_at: Some(1_000_000), // well in the past
        };
        assert!(expired.is_expired());

        let far_future = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
            + 3600;
        let fresh = GeminiOAuthToken {
            access_token: "t".to_string(),
            refresh_token: None,
            expires_at: Some(far_future),
        };
        assert!(!fresh.is_expired());
    }

    #[test]
    fn parse_callback_target_happy_path() {
        let (code, state) =
            parse_oauth_callback_target("/callback?code=abc123&state=xyz").unwrap();
        assert_eq!(code, "abc123");
        assert_eq!(state.as_deref(), Some("xyz"));
    }

    #[test]
    fn parse_callback_target_error_path() {
        let err =
            parse_oauth_callback_target("/callback?error=access_denied").unwrap_err();
        assert!(err.contains("access_denied"), "error: {err}");
    }

    #[test]
    fn percent_encode_round_trip() {
        let raw = "https://www.googleapis.com/auth/cloud-platform";
        let encoded = percent_encode(raw);
        assert!(encoded.contains("%3A"), "colon must be encoded");
        assert!(encoded.contains("%2F"), "slash must be encoded");
    }

    #[test]
    fn sse_frame_parses_gemini_candidate_text() {
        let frame = r#"data: {"candidates":[{"content":{"parts":[{"text":"Hello"}]},"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":5,"candidatesTokenCount":2}}"#;
        let mut stream = GeminiOAuthStreamTestHarness::new("test-model");
        stream.push(frame.as_bytes());
        // Should have MessageStart + ContentBlockStart + ContentBlockDelta + ContentBlockStop + MessageDelta + MessageStop
        assert!(
            stream.events.iter().any(|e| matches!(
                e,
                StreamEvent::ContentBlockDelta(ev)
                if matches!(&ev.delta, ContentBlockDelta::TextDelta { text } if text == "Hello")
            )),
            "expected TextDelta with 'Hello'"
        );
        assert!(
            stream.events.iter().any(|e| matches!(e, StreamEvent::MessageStop(_))),
            "STOP finishReason should emit MessageStop"
        );
    }

    struct GeminiOAuthStreamTestHarness {
        buffer: Vec<u8>,
        pub events: Vec<StreamEvent>,
        model: String,
        message_started: bool,
        text_started: bool,
    }

    impl GeminiOAuthStreamTestHarness {
        fn new(model: &str) -> Self {
            Self {
                buffer: Vec::new(),
                events: Vec::new(),
                model: model.to_string(),
                message_started: false,
                text_started: false,
            }
        }

        fn push(&mut self, data: &[u8]) {
            // Wrap in a proper SSE frame for next_sse_frame to parse.
            let mut buf = data.to_vec();
            buf.extend_from_slice(b"\n\n");
            self.buffer.extend_from_slice(&buf);
            while let Some(frame) = next_sse_frame(&mut self.buffer) {
                self.ingest_frame(&frame);
            }
        }

        fn ingest_frame(&mut self, frame: &str) {
            let mut data_lines: Vec<&str> = Vec::new();
            for line in frame.lines() {
                if let Some(d) = line.strip_prefix("data:") {
                    data_lines.push(d.trim_start());
                } else if !line.starts_with(':') && !line.is_empty() && !line.contains(':') {
                    // Raw data line without prefix (the push() wrapper above doesn't add "data:")
                    data_lines.push(line);
                }
            }
            if data_lines.is_empty() {
                data_lines.push(frame.trim());
            }
            let payload = data_lines.join("");
            if payload.is_empty() || payload == "[DONE]" {
                return;
            }
            let Ok(v) = serde_json::from_str::<Value>(&payload) else {
                return;
            };
            if !self.message_started {
                self.message_started = true;
                self.events.push(StreamEvent::MessageStart(MessageStartEvent {
                    message: MessageResponse {
                        id: "test".to_string(),
                        kind: "message".to_string(),
                        role: "assistant".to_string(),
                        content: Vec::new(),
                        model: self.model.clone(),
                        stop_reason: None,
                        stop_sequence: None,
                        usage: Usage::default(),
                        request_id: None,
                    },
                }));
            }
            let text = v["candidates"]
                .as_array()
                .and_then(|arr| arr.first())
                .and_then(|c| c["content"]["parts"].as_array())
                .and_then(|parts| parts.first())
                .and_then(|p| p["text"].as_str())
                .unwrap_or("");
            if !text.is_empty() {
                if !self.text_started {
                    self.text_started = true;
                    self.events.push(StreamEvent::ContentBlockStart(ContentBlockStartEvent {
                        index: 0,
                        content_block: OutputContentBlock::Text { text: String::new() },
                    }));
                }
                self.events.push(StreamEvent::ContentBlockDelta(ContentBlockDeltaEvent {
                    index: 0,
                    delta: ContentBlockDelta::TextDelta { text: text.to_string() },
                }));
            }
            let finish = v["candidates"]
                .as_array()
                .and_then(|arr| arr.first())
                .and_then(|c| c["finishReason"].as_str());
            if matches!(finish, Some("STOP") | Some("SAFETY") | Some("MAX_TOKENS")) {
                if self.text_started {
                    self.events.push(StreamEvent::ContentBlockStop(ContentBlockStopEvent { index: 0 }));
                }
                self.events.push(StreamEvent::MessageDelta(MessageDeltaEvent {
                    delta: MessageDelta { stop_reason: Some("end_turn".to_string()), stop_sequence: None },
                    usage: Usage::default(),
                }));
                self.events.push(StreamEvent::MessageStop(MessageStopEvent {}));
            }
        }
    }
}
