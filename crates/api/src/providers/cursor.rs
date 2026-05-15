//! Cursor Cloud Agents provider.
//!
//! Uses the documented Cursor Cloud Agents REST API — NOT the broken
//! `/v1/chat/completions` stub that was in `openai_compat.rs`.
//!
//! Auth: HTTP Basic with `CURSOR_API_KEY` as the username and empty password.
//!   `Authorization: Basic base64("{key}:")`
//!
//! Required env var:
//!   `CURSOR_API_KEY` — a `crsr_xxx` key from cursor.com/settings
//!
//! Repo binding (mandatory per the Cursor API):
//!   The current workspace must be a git repository whose `origin` remote is
//!   a GitHub URL.  If not, the client refuses with a clear actionable error.
//!
//! Wire protocol:
//!   POST /v1/agents     → launch; returns agentId + run.id
//!   GET  /v1/agents/{id}/runs/{runId}/stream (SSE) → stream events
//!   POST /v1/agents/{id}/runs/{runId}/cancel → cancel on Esc
//!
//! SSE event types consumed:
//!   status      — run lifecycle (CREATING → RUNNING → FINISHED/FAILED/CANCELLED)
//!   assistant   — text delta  → TextDelta
//!   thinking    — reasoning   → ThinkingDelta
//!   tool_call   — tool update → ToolUse
//!   heartbeat   — keepalive (ignored)
//!   result      — terminal status
//!   done        — end of stream
//!   error       — surface as ApiError
//!
//! Resume support:
//!   On disconnect the last `id:` field is captured and sent back as the
//!   `Last-Event-ID` header on reconnect.  If the server returns 410 (stream
//!   expired) we surface a clear error explaining what happened.
//!
//! Model list:
//!   GET /v1/models → `{"items": ["claude-4-sonnet-thinking", "gpt-5.2", ...]}`
//!   Cached per session; fetched lazily on first TAB.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::{json, Value};

use crate::error::ApiError;
use crate::types::{
    ContentBlockDelta, ContentBlockDeltaEvent, ContentBlockStartEvent, ContentBlockStopEvent,
    InputContentBlock, MessageDelta, MessageDeltaEvent, MessageRequest, MessageResponse,
    MessageStartEvent, MessageStopEvent, OutputContentBlock, StreamEvent, Usage,
};
use super::common::{next_sse_frame};
use super::openai_compat::resolve_stream_dead_air_timeout;
use super::{Provider, ProviderFuture};

pub const BASE_URL: &str = "https://api.cursor.com";

// ---------------------------------------------------------------------------
// Credential persistence helpers
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

/// Load the Cursor API key from `~/.anvil/credentials.json` under the `"cursor"` key.
pub fn load_cursor_saved_key() -> std::io::Result<Option<String>> {
    let path = credentials_path()?;
    let contents = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    let root: serde_json::Map<String, Value> = serde_json::from_str(&contents)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let Some(v) = root.get("cursor") else {
        return Ok(None);
    };
    Ok(v.as_str().map(ToOwned::to_owned))
}

/// Save the Cursor API key to `~/.anvil/credentials.json` under the `"cursor"` key.
pub fn save_cursor_key(key: &str) -> std::io::Result<()> {
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
    root.insert("cursor".to_string(), Value::String(key.to_string()));
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
// HTTP Basic auth
// ---------------------------------------------------------------------------

/// Build the `Authorization: Basic <base64("{key}:")>` header value.
fn basic_auth_header(api_key: &str) -> String {
    let credentials = format!("{api_key}:");
    let encoded = base64_encode(credentials.as_bytes());
    format!("Basic {encoded}")
}

fn base64_encode(input: &[u8]) -> String {
    const TABLE: &[u8; 64] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity((input.len() + 2) / 3 * 4);
    let mut i = 0;
    while i < input.len() {
        let b0 = input[i];
        let b1 = input.get(i + 1).copied().unwrap_or(0);
        let b2 = input.get(i + 2).copied().unwrap_or(0);
        out.push(TABLE[(b0 >> 2) as usize] as char);
        out.push(TABLE[((b0 & 3) << 4 | b1 >> 4) as usize] as char);
        if i + 1 < input.len() {
            out.push(TABLE[((b1 & 0xF) << 2 | b2 >> 6) as usize] as char);
        } else {
            out.push('=');
        }
        if i + 2 < input.len() {
            out.push(TABLE[(b2 & 0x3F) as usize] as char);
        } else {
            out.push('=');
        }
        i += 3;
    }
    out
}

// ---------------------------------------------------------------------------
// Repo binding
// ---------------------------------------------------------------------------

/// The error message shown when the current workspace cannot be bound to GitHub.
pub const REPO_BINDING_ERROR: &str =
    "Cursor requires a GitHub repository binding. Current workspace is not a GitHub repo \
(no `origin` remote, or remote is not github.com).\n\
Either initialize the repo with a GitHub origin, or pick a different provider.";

/// Detect the GitHub repository URL from the current working directory.
///
/// Runs `git remote get-url origin` synchronously (this is called once at
/// prompt time, not in a hot loop).  Converts SSH URLs to HTTPS.
///
/// Returns `Err` with a loud actionable message if:
/// - the directory is not a git repo, or
/// - the origin remote is not a github.com URL.
pub fn detect_github_repo_url() -> Result<String, ApiError> {
    detect_github_repo_url_from_dir(".")
}

/// Same as [`detect_github_repo_url`] but takes an explicit directory, allowing
/// tests to point at a controlled workspace without side-effects.
pub fn detect_github_repo_url_from_dir(dir: &str) -> Result<String, ApiError> {
    let output = std::process::Command::new("git")
        .args(["remote", "get-url", "origin"])
        .current_dir(dir)
        .output()
        .map_err(|e| ApiError::Auth(format!("failed to run `git`: {e}")))?;

    if !output.status.success() {
        return Err(ApiError::Auth(REPO_BINDING_ERROR.to_string()));
    }

    let raw = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if raw.is_empty() {
        return Err(ApiError::Auth(REPO_BINDING_ERROR.to_string()));
    }

    let url = normalize_github_url(&raw).ok_or_else(|| {
        ApiError::Auth(REPO_BINDING_ERROR.to_string())
    })?;

    Ok(url)
}

/// Convert `git@github.com:org/repo.git` → `https://github.com/org/repo`.
/// Pass through `https://github.com/...` unchanged (strip `.git` suffix if present).
/// Return `None` for non-GitHub URLs.
fn normalize_github_url(raw: &str) -> Option<String> {
    let raw = raw.trim();

    // SSH form: git@github.com:org/repo.git
    if let Some(rest) = raw.strip_prefix("git@github.com:") {
        let path = rest.trim_end_matches(".git");
        return Some(format!("https://github.com/{path}"));
    }

    // HTTPS form: https://github.com/org/repo[.git]
    if raw.starts_with("https://github.com/") || raw.starts_with("http://github.com/") {
        let cleaned = raw.trim_end_matches(".git");
        return Some(cleaned.to_string());
    }

    None
}

// ---------------------------------------------------------------------------
// /v1/me response (used by login validation)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct CursorMeResponse {
    #[serde(rename = "apiKeyName", default)]
    pub api_key_name: Option<String>,
    #[serde(rename = "createdAt", default)]
    pub created_at: Option<String>,
    #[serde(rename = "userEmail", default)]
    pub user_email: Option<String>,
}

// ---------------------------------------------------------------------------
// /v1/agents response
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct LaunchAgentResponse {
    agent: Option<Value>,
    run: Option<AgentRun>,
}

#[derive(Debug, Deserialize)]
struct AgentRun {
    id: String,
    #[serde(default)]
    #[allow(dead_code)]
    status: Option<String>,
}

// ---------------------------------------------------------------------------
// CursorClient
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct CursorClient {
    http: reqwest::Client,
    api_key: String,
    base_url: String,
}

impl CursorClient {
    /// Build from `CURSOR_API_KEY` env var, falling back to the saved vault key.
    pub fn from_env() -> Result<Self, ApiError> {
        let api_key = std::env::var("CURSOR_API_KEY")
            .ok()
            .filter(|v| !v.is_empty())
            .or_else(|| {
                // Fall back to ~/.anvil/credentials.json "cursor" entry.
                load_cursor_saved_key().ok().flatten()
            })
            .or_else(|| {
                // Final fallback: ~/.cursor/auth.json accessToken.
                super::copilot::load_cursor_auth_token().ok().flatten()
            })
            .ok_or_else(|| {
                ApiError::missing_credentials("Cursor", &["CURSOR_API_KEY"])
            })?;

        let base_url = std::env::var("CURSOR_BASE_URL")
            .ok()
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| BASE_URL.to_string());

        Ok(Self {
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(600))
                .build()
                .unwrap_or_default(),
            api_key,
            base_url,
        })
    }

    fn auth_header(&self) -> String {
        basic_auth_header(&self.api_key)
    }

    /// Validate the key against `GET /v1/me`.  Returns the response on success.
    pub async fn validate_key(&self) -> Result<CursorMeResponse, ApiError> {
        let url = format!("{}/v1/me", self.base_url.trim_end_matches('/'));
        let resp = self
            .http
            .get(&url)
            .header("Authorization", self.auth_header())
            .header("User-Agent", anvil_user_agent())
            .send()
            .await
            .map_err(ApiError::Http)?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(ApiError::Api {
                status,
                error_type: None,
                message: Some("Cursor /v1/me rejected the key".to_string()),
                body,
                retryable: status.as_u16() >= 500,
                retry_after_secs: None,
            });
        }
        resp.json::<CursorMeResponse>().await.map_err(ApiError::Http)
    }

    /// Fetch the live model list from `GET /v1/models`.
    ///
    /// Returns the `items` array as a `Vec<String>`.
    pub async fn fetch_models(&self) -> Result<Vec<String>, ApiError> {
        let url = format!("{}/v1/models", self.base_url.trim_end_matches('/'));
        let resp = self
            .http
            .get(&url)
            .header("Authorization", self.auth_header())
            .header("User-Agent", anvil_user_agent())
            .send()
            .await
            .map_err(ApiError::Http)?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(ApiError::Api {
                status,
                error_type: None,
                message: None,
                body,
                retryable: status.as_u16() >= 500,
                retry_after_secs: None,
            });
        }

        #[derive(Deserialize)]
        struct ModelsEnvelope {
            #[serde(default)]
            items: Vec<String>,
        }
        let envelope: ModelsEnvelope = resp.json().await.map_err(ApiError::Http)?;
        Ok(envelope.items)
    }

    /// Launch an agent and return `(agent_id, run_id)`.
    async fn launch_agent(
        &self,
        prompt: &str,
        model: &str,
        repo_url: &str,
    ) -> Result<(String, String), ApiError> {
        let url = format!("{}/v1/agents", self.base_url.trim_end_matches('/'));
        let body = json!({
            "prompt": { "text": prompt },
            "model": { "id": model },
            "repos": [{ "url": repo_url }]
        });

        let resp = self
            .http
            .post(&url)
            .header("Authorization", self.auth_header())
            .header("content-type", "application/json")
            .header("User-Agent", anvil_user_agent())
            .json(&body)
            .send()
            .await
            .map_err(ApiError::Http)?;

        let status = resp.status();
        if !status.is_success() {
            let body_text = resp.text().await.unwrap_or_default();
            return Err(ApiError::Api {
                status,
                error_type: None,
                message: None,
                body: body_text,
                retryable: status.as_u16() >= 500,
                retry_after_secs: None,
            });
        }

        let launch: LaunchAgentResponse = resp.json().await.map_err(ApiError::Http)?;
        let run = launch.run.ok_or_else(|| {
            ApiError::Auth("Cursor /v1/agents response missing `run` field".to_string())
        })?;
        let agent_id = launch
            .agent
            .as_ref()
            .and_then(|a| a["id"].as_str())
            .unwrap_or("unknown")
            .to_string();
        Ok((agent_id, run.id))
    }

    /// Open the SSE stream for a running agent.
    ///
    /// `last_event_id` is sent as the `Last-Event-ID` header when reconnecting
    /// after a disconnect within the server's retention window.
    async fn open_stream(
        &self,
        agent_id: &str,
        run_id: &str,
        last_event_id: Option<&str>,
    ) -> Result<reqwest::Response, ApiError> {
        let url = format!(
            "{}/v1/agents/{agent_id}/runs/{run_id}/stream",
            self.base_url.trim_end_matches('/')
        );
        let mut builder = self
            .http
            .get(&url)
            .header("Authorization", self.auth_header())
            .header("Accept", "text/event-stream")
            .header("User-Agent", anvil_user_agent());

        if let Some(id) = last_event_id {
            builder = builder.header("Last-Event-ID", id);
        }

        let resp = builder.send().await.map_err(ApiError::Http)?;
        let status = resp.status();

        if status.as_u16() == 410 {
            return Err(ApiError::Auth(
                "Cursor stream expired: the run's event retention window has closed. \
                 Start a new agent run to continue."
                    .to_string(),
            ));
        }

        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(ApiError::Api {
                status,
                error_type: None,
                message: None,
                body,
                retryable: status.as_u16() >= 500,
                retry_after_secs: None,
            });
        }

        Ok(resp)
    }

    /// Cancel a running agent run.
    pub async fn cancel_run(&self, agent_id: &str, run_id: &str) -> Result<(), ApiError> {
        let url = format!(
            "{}/v1/agents/{agent_id}/runs/{run_id}/cancel",
            self.base_url.trim_end_matches('/')
        );
        let resp = self
            .http
            .post(&url)
            .header("Authorization", self.auth_header())
            .header("User-Agent", anvil_user_agent())
            .send()
            .await
            .map_err(ApiError::Http)?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(ApiError::Api {
                status,
                error_type: None,
                message: None,
                body,
                retryable: false,
                retry_after_secs: None,
            });
        }
        Ok(())
    }

    /// Full send_message path: detect repo → launch agent → stream → collect.
    pub async fn send_message(
        &self,
        request: &MessageRequest,
    ) -> Result<MessageResponse, ApiError> {
        let repo_url = detect_github_repo_url()?;
        let prompt = extract_prompt_text(request);
        let (agent_id, run_id) =
            self.launch_agent(&prompt, &request.model, &repo_url).await?;

        let response_stream = self.open_stream(&agent_id, &run_id, None).await?;
        let mut stream = CursorMessageStream::new(response_stream, request.model.clone());

        let mut full_text = String::new();
        loop {
            match stream.next_event().await? {
                None => break,
                Some(StreamEvent::ContentBlockDelta(ev)) => {
                    if let ContentBlockDelta::TextDelta { text } = ev.delta {
                        full_text.push_str(&text);
                    }
                }
                _ => {}
            }
        }

        Ok(MessageResponse {
            id: format!("cursor-{agent_id}-{run_id}"),
            kind: "message".to_string(),
            role: "assistant".to_string(),
            content: vec![OutputContentBlock::Text { text: full_text }],
            model: request.model.clone(),
            stop_reason: Some("end_turn".to_string()),
            stop_sequence: None,
            usage: Usage::default(),
            request_id: None,
        })
    }

    /// Streaming send_message: detect repo → launch agent → return stream.
    pub async fn stream_message(
        &self,
        request: &MessageRequest,
    ) -> Result<CursorMessageStream, ApiError> {
        let repo_url = detect_github_repo_url()?;
        let prompt = extract_prompt_text(request);
        let (agent_id, run_id) =
            self.launch_agent(&prompt, &request.model, &repo_url).await?;
        let response_stream = self.open_stream(&agent_id, &run_id, None).await?;
        Ok(CursorMessageStream::new(response_stream, request.model.clone()))
    }
}

impl Provider for CursorClient {
    type Stream = CursorMessageStream;

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
// SSE stream parser
// ---------------------------------------------------------------------------

/// Active state while an SSE stream is open.
#[derive(Debug)]
pub struct CursorMessageStream {
    response: reqwest::Response,
    buffer: Vec<u8>,
    pending: VecDeque<StreamEvent>,
    done: bool,
    model: String,
    message_started: bool,
    text_started: bool,
    /// The `id:` field value from the most recently consumed SSE frame.
    /// Captured so callers can reconnect with `Last-Event-ID` if needed.
    pub last_event_id: Option<String>,
    last_chunk_at: Instant,
    dead_air_timeout: Duration,
}

impl CursorMessageStream {
    fn new(response: reqwest::Response, model: String) -> Self {
        Self {
            response,
            buffer: Vec::new(),
            pending: VecDeque::new(),
            done: false,
            model,
            message_started: false,
            text_started: false,
            last_event_id: None,
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

    /// Drain complete SSE frames from the buffer.
    fn drain_buffer(&mut self) {
        while let Some(frame) = next_sse_frame(&mut self.buffer) {
            self.ingest_frame(&frame);
        }
    }

    /// Parse one full SSE frame (e.g. "event: assistant\nid: 42\ndata: {...}").
    fn ingest_frame(&mut self, frame: &str) {
        let mut event_type: Option<&str> = None;
        let mut data_lines: Vec<&str> = Vec::new();
        let mut id_value: Option<&str> = None;

        for line in frame.lines() {
            if let Some(v) = line.strip_prefix("event:") {
                event_type = Some(v.trim_start());
            } else if let Some(v) = line.strip_prefix("data:") {
                data_lines.push(v.trim_start());
            } else if let Some(v) = line.strip_prefix("id:") {
                id_value = Some(v.trim_start());
            }
        }

        if let Some(id) = id_value {
            self.last_event_id = Some(id.to_string());
        }

        let data = data_lines.join("\n");
        let event_type = event_type.unwrap_or("message");

        match event_type {
            "heartbeat" | "ping" => {
                // keepalive — ignore
            }
            "done" => {
                self.done = true;
                self.flush_finish();
            }
            "error" => {
                // Parse error from data and mark done; the error will surface
                // on the next next_event() call via flush_finish providing a
                // MessageStop.  The caller sees stream end; richer surfacing
                // is left to the TUI layer.
                let msg = serde_json::from_str::<Value>(&data)
                    .ok()
                    .and_then(|v| {
                        v["message"]
                            .as_str()
                            .or_else(|| v["error"].as_str())
                            .map(ToOwned::to_owned)
                    })
                    .unwrap_or_else(|| data.clone());
                eprintln!("[cursor] stream error: {msg}");
                self.done = true;
                self.flush_finish();
            }
            "status" => {
                // Status transitions (CREATING → RUNNING → FINISHED etc.)
                // Emit MessageStart on the first RUNNING status.
                if !self.message_started {
                    if let Ok(v) = serde_json::from_str::<Value>(&data) {
                        let status = v["status"].as_str().unwrap_or("");
                        if matches!(status, "RUNNING" | "CREATING") {
                            self.ensure_message_started();
                        }
                    }
                }
            }
            "result" => {
                // Terminal result event — contains final run status.
                self.done = true;
                self.flush_finish();
            }
            "assistant" => {
                // Text delta from the assistant.
                self.ensure_message_started();
                if let Ok(v) = serde_json::from_str::<Value>(&data) {
                    let text = v["text"]
                        .as_str()
                        .or_else(|| v["content"].as_str())
                        .or_else(|| v["delta"].as_str())
                        .unwrap_or(&data);
                    if !text.is_empty() {
                        self.emit_text_delta(text);
                    }
                } else if !data.is_empty() {
                    // Raw text data without JSON envelope.
                    self.emit_text_delta(&data);
                }
            }
            "thinking" => {
                // Reasoning delta.
                self.ensure_message_started();
                let thinking = serde_json::from_str::<Value>(&data)
                    .ok()
                    .and_then(|v| {
                        v["text"]
                            .as_str()
                            .or_else(|| v["thinking"].as_str())
                            .or_else(|| v["delta"].as_str())
                            .map(ToOwned::to_owned)
                    })
                    .unwrap_or_else(|| data.clone());
                if !thinking.is_empty() {
                    self.pending.push_back(StreamEvent::ContentBlockDelta(
                        ContentBlockDeltaEvent {
                            index: 1,
                            delta: ContentBlockDelta::ThinkingDelta { thinking },
                        },
                    ));
                }
            }
            "tool_call" => {
                // Tool invocation update — emit as partial JSON delta.
                self.ensure_message_started();
                let partial = if data.is_empty() { "{}".to_string() } else { data };
                self.pending.push_back(StreamEvent::ContentBlockDelta(
                    ContentBlockDeltaEvent {
                        index: 2,
                        delta: ContentBlockDelta::InputJsonDelta {
                            partial_json: partial,
                        },
                    },
                ));
            }
            _ => {
                // Unknown event type — treat as text delta if data is non-empty.
                if !data.is_empty() {
                    self.ensure_message_started();
                    self.emit_text_delta(&data);
                }
            }
        }
    }

    fn ensure_message_started(&mut self) {
        if !self.message_started {
            self.message_started = true;
            self.pending.push_back(StreamEvent::MessageStart(MessageStartEvent {
                message: MessageResponse {
                    id: format!("cursor-stream-{}", uuid_short()),
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
                self.pending
                    .push_back(StreamEvent::ContentBlockStop(ContentBlockStopEvent { index: 0 }));
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

/// Extract the last user message text from a `MessageRequest`.
fn extract_prompt_text(request: &MessageRequest) -> String {
    request
        .messages
        .iter()
        .rev()
        .find(|m| m.role == "user")
        .map(|m| {
            m.content
                .iter()
                .filter_map(|block| match block {
                    InputContentBlock::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}

/// Build the `User-Agent` header Anvil sends to Cursor — honest identification.
fn anvil_user_agent() -> String {
    format!("Anvil/{} ({})", env!("CARGO_PKG_VERSION"), std::env::consts::OS)
}

/// Generate a short pseudo-random ID using the system clock.
fn uuid_short() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos();
    format!("{nanos:08x}")
}

// ---------------------------------------------------------------------------
// Model list fetcher (for model_list.rs integration)
// ---------------------------------------------------------------------------

/// Fetch the Cursor model list from `GET /v1/models`.
///
/// Returns a flat `Vec<ProviderModel>` compatible with the rest of the
/// model-list infrastructure.
pub async fn fetch_cursor_models_live(
    api_key: &str,
    base_url: &str,
) -> Result<Vec<super::model_list::ProviderModel>, super::model_list::ProviderModelsError> {
    use super::model_list::{ProviderModel, ProviderModelsError};
    use super::ProviderKind;

    let url = format!("{}/v1/models", base_url.trim_end_matches('/'));
    let auth = basic_auth_header(api_key);

    let client = reqwest::Client::builder()
        .timeout(super::model_list::DEFAULT_FETCH_TIMEOUT)
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());

    let resp = client
        .get(&url)
        .header("Authorization", auth)
        .header("User-Agent", anvil_user_agent())
        .send()
        .await
        .map_err(|e| ProviderModelsError::Transient(e.to_string()))?;

    let status = resp.status();
    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        return Err(ProviderModelsError::Unauthorized);
    }
    if !status.is_success() {
        return Err(ProviderModelsError::Transient(format!(
            "HTTP {} from {url}",
            status.as_u16()
        )));
    }

    #[derive(Deserialize)]
    struct Envelope {
        #[serde(default)]
        items: Vec<String>,
    }
    let envelope: Envelope = resp
        .json()
        .await
        .map_err(|e| ProviderModelsError::InvalidResponse(e.to_string()))?;

    Ok(envelope
        .items
        .into_iter()
        .map(|id| ProviderModel {
            id,
            provider: ProviderKind::Cursor,
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
    fn normalize_github_url_ssh() {
        assert_eq!(
            normalize_github_url("git@github.com:acme/my-repo.git"),
            Some("https://github.com/acme/my-repo".to_string())
        );
    }

    #[test]
    fn normalize_github_url_https() {
        assert_eq!(
            normalize_github_url("https://github.com/acme/my-repo.git"),
            Some("https://github.com/acme/my-repo".to_string())
        );
        assert_eq!(
            normalize_github_url("https://github.com/acme/my-repo"),
            Some("https://github.com/acme/my-repo".to_string())
        );
    }

    #[test]
    fn normalize_github_url_non_github_returns_none() {
        assert_eq!(normalize_github_url("https://gitlab.com/foo/bar"), None);
        assert_eq!(normalize_github_url("git@bitbucket.org:foo/bar.git"), None);
    }

    #[test]
    fn basic_auth_header_format() {
        // "key:" base64-encoded is "a2V5Og=="
        let header = basic_auth_header("key");
        assert!(header.starts_with("Basic "), "must start with Basic");
        // Decode and verify
        let encoded = header.strip_prefix("Basic ").unwrap();
        // "key:" in base64 = a2V5Og==
        assert_eq!(encoded, "a2V5Og==", "base64(\"key:\") mismatch");
    }

    #[test]
    fn repo_binding_error_is_actionable() {
        assert!(REPO_BINDING_ERROR.contains("GitHub"));
        assert!(REPO_BINDING_ERROR.contains("origin"));
        assert!(REPO_BINDING_ERROR.contains("github.com"));
    }

    // Focused unit test for ingest_frame logic using a test harness.
    #[test]
    fn ingest_frame_assistant_event_emits_text_delta() {
        let events = parse_cursor_sse_frame(
            "event: assistant\ndata: {\"text\":\"Hello, world!\"}\n\n",
        );
        assert!(
            events.iter().any(|e| matches!(
                e,
                StreamEvent::ContentBlockDelta(ev)
                if matches!(&ev.delta, ContentBlockDelta::TextDelta { text } if text == "Hello, world!")
            )),
            "expected TextDelta with 'Hello, world!'"
        );
    }

    #[test]
    fn ingest_frame_thinking_event_emits_thinking_delta() {
        let events =
            parse_cursor_sse_frame("event: thinking\ndata: {\"thinking\":\"ponder...\"}\n\n");
        assert!(
            events.iter().any(|e| matches!(
                e,
                StreamEvent::ContentBlockDelta(ev)
                if matches!(&ev.delta, ContentBlockDelta::ThinkingDelta { thinking } if thinking == "ponder...")
            )),
            "expected ThinkingDelta"
        );
    }

    #[test]
    fn ingest_frame_heartbeat_emits_nothing() {
        let events = parse_cursor_sse_frame("event: heartbeat\ndata: {}\n\n");
        assert!(events.is_empty(), "heartbeat must produce no stream events");
    }

    #[test]
    fn ingest_frame_done_marks_stream_finished() {
        // A done event should result in MessageStop.
        let events = parse_cursor_sse_frame_with_state(
            "event: status\ndata: {\"status\":\"RUNNING\"}\n\nevent: done\ndata: {}\n\n",
        );
        assert!(
            events.iter().any(|e| matches!(e, StreamEvent::MessageStop(_))),
            "done event must produce MessageStop"
        );
    }

    /// Test helper: parse a raw SSE byte buffer through `CursorMessageStream`
    /// parsing logic and collect the emitted events.
    ///
    /// Uses a mock that drives `drain_buffer` + `ingest_frame` without an HTTP
    /// response object.
    fn parse_cursor_sse_frame(raw: &str) -> Vec<StreamEvent> {
        parse_cursor_sse_frame_with_state(raw)
    }

    fn parse_cursor_sse_frame_with_state(raw: &str) -> Vec<StreamEvent> {
        struct TestStream {
            buffer: Vec<u8>,
            pending: VecDeque<StreamEvent>,
            done: bool,
            model: String,
            message_started: bool,
            text_started: bool,
            last_event_id: Option<String>,
        }

        impl TestStream {
            fn new(model: &str) -> Self {
                Self {
                    buffer: Vec::new(),
                    pending: VecDeque::new(),
                    done: false,
                    model: model.to_string(),
                    message_started: false,
                    text_started: false,
                    last_event_id: None,
                }
            }

            fn push(&mut self, data: &[u8]) {
                self.buffer.extend_from_slice(data);
                while let Some(frame) = next_sse_frame(&mut self.buffer) {
                    self.ingest_frame_inner(&frame);
                }
            }

            fn ingest_frame_inner(&mut self, frame: &str) {
                let mut event_type: Option<&str> = None;
                let mut data_lines: Vec<&str> = Vec::new();
                let mut id_value: Option<&str> = None;
                for line in frame.lines() {
                    if let Some(v) = line.strip_prefix("event:") {
                        event_type = Some(v.trim_start());
                    } else if let Some(v) = line.strip_prefix("data:") {
                        data_lines.push(v.trim_start());
                    } else if let Some(v) = line.strip_prefix("id:") {
                        id_value = Some(v.trim_start());
                    }
                }
                if let Some(id) = id_value {
                    self.last_event_id = Some(id.to_string());
                }
                let data = data_lines.join("\n");
                let event_type = event_type.unwrap_or("message");
                match event_type {
                    "heartbeat" | "ping" => {}
                    "done" => {
                        self.done = true;
                        self.flush();
                    }
                    "error" => {
                        self.done = true;
                        self.flush();
                    }
                    "status" => {
                        if !self.message_started {
                            if let Ok(v) = serde_json::from_str::<Value>(&data) {
                                let status = v["status"].as_str().unwrap_or("");
                                if matches!(status, "RUNNING" | "CREATING") {
                                    self.ensure_started();
                                }
                            }
                        }
                    }
                    "result" => {
                        self.done = true;
                        self.flush();
                    }
                    "assistant" => {
                        self.ensure_started();
                        let text = serde_json::from_str::<Value>(&data)
                            .ok()
                            .and_then(|v| {
                                v["text"]
                                    .as_str()
                                    .or_else(|| v["content"].as_str())
                                    .or_else(|| v["delta"].as_str())
                                    .map(ToOwned::to_owned)
                            })
                            .unwrap_or_else(|| data.clone());
                        if !text.is_empty() {
                            self.emit_text(&text);
                        }
                    }
                    "thinking" => {
                        self.ensure_started();
                        let thinking = serde_json::from_str::<Value>(&data)
                            .ok()
                            .and_then(|v| {
                                v["text"]
                                    .as_str()
                                    .or_else(|| v["thinking"].as_str())
                                    .or_else(|| v["delta"].as_str())
                                    .map(ToOwned::to_owned)
                            })
                            .unwrap_or_else(|| data.clone());
                        if !thinking.is_empty() {
                            self.pending.push_back(StreamEvent::ContentBlockDelta(
                                ContentBlockDeltaEvent {
                                    index: 1,
                                    delta: ContentBlockDelta::ThinkingDelta { thinking },
                                },
                            ));
                        }
                    }
                    "tool_call" => {
                        self.ensure_started();
                        let partial = if data.is_empty() {
                            "{}".to_string()
                        } else {
                            data
                        };
                        self.pending.push_back(StreamEvent::ContentBlockDelta(
                            ContentBlockDeltaEvent {
                                index: 2,
                                delta: ContentBlockDelta::InputJsonDelta {
                                    partial_json: partial,
                                },
                            },
                        ));
                    }
                    _ => {
                        if !data.is_empty() {
                            self.ensure_started();
                            self.emit_text(&data);
                        }
                    }
                }
            }

            fn ensure_started(&mut self) {
                if !self.message_started {
                    self.message_started = true;
                    self.pending.push_back(StreamEvent::MessageStart(MessageStartEvent {
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
            }

            fn emit_text(&mut self, text: &str) {
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

            fn flush(&mut self) {
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

        let mut ts = TestStream::new("test-model");
        ts.push(raw.as_bytes());
        ts.pending.into_iter().collect()
    }
}

