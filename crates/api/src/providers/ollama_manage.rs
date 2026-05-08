//! Management endpoints for the local Ollama daemon: `/api/pull`,
//! `/api/delete`, `/api/copy`, `/api/create`, `/api/tags`.
//!
//! These power the manage-side `/ollama` slash subcommands (Task #367, "L5c"):
//! `pull`, `rm`, `cp`, `create`.  The dangerous endpoints (delete, create,
//! pull) are gated by the CLI layer — this module only provides the
//! HTTP transport and a pure NDJSON progress parser.
//!
//! Honors the `ollama-cloud-auth` rule: ALL traffic is sent to the local
//! daemon (`OLLAMA_HOST` or `localhost:11434`), never `ollama.com` directly.
//! No new crate dependencies are introduced.

use std::error::Error;
use std::fmt::{self, Display, Formatter};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::ollama::{DEFAULT_OLLAMA_BASE_URL, OLLAMA_HOST_ENV};

// ─── Errors ──────────────────────────────────────────────────────────────────

#[derive(Debug)]
pub enum OllamaManageError {
    DaemonUnreachable(String),
    ModelNotInstalled(String),
    Http { status: u16, body: String },
    Stream(String),
    Parse(String),
}

impl Display for OllamaManageError {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::DaemonUnreachable(msg) => write!(f, "Ollama daemon unreachable: {msg}"),
            Self::ModelNotInstalled(name) => write!(f, "Model not installed: {name}"),
            Self::Http { status, body } => write!(f, "HTTP {status}: {body}"),
            Self::Stream(msg) => write!(f, "stream error: {msg}"),
            Self::Parse(msg) => write!(f, "parse error: {msg}"),
        }
    }
}

impl Error for OllamaManageError {}

// ─── Pull progress NDJSON parser (pure) ──────────────────────────────────────

/// One line of progress emitted by `/api/pull` or `/api/create`.
///
/// The Ollama daemon emits NDJSON: one JSON object per line, with at minimum
/// a `status` field (e.g. `"pulling manifest"`, `"verifying sha256 digest"`,
/// `"success"`).  Optional `digest`, `total`, and `completed` fields appear
/// during the bytewise pulling phase.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullProgress {
    pub status: String,
    #[serde(default)]
    pub digest: Option<String>,
    #[serde(default)]
    pub total: Option<u64>,
    #[serde(default)]
    pub completed: Option<u64>,
    /// Final-line error string emitted on daemon-side pull failure.
    /// Mutually exclusive with `status: "success"`.
    #[serde(default)]
    pub error: Option<String>,
}

impl PullProgress {
    /// Percentage 0..=100, or `None` when totals aren't known yet (e.g. the
    /// initial "pulling manifest" lines have no byte counters).
    #[must_use]
    pub fn percent(&self) -> Option<u8> {
        let total = self.total?;
        if total == 0 {
            return None;
        }
        let completed = self.completed.unwrap_or(0);
        let pct = ((completed as f64) / (total as f64) * 100.0).round();
        let clamped = pct.clamp(0.0, 100.0) as u8;
        Some(clamped)
    }

    #[must_use]
    pub fn is_terminal_success(&self) -> bool {
        self.status == "success" && self.error.is_none()
    }

    #[must_use]
    pub fn is_terminal_error(&self) -> bool {
        self.error.is_some()
    }
}

/// Parse a single NDJSON line from `/api/pull` or `/api/create`.
/// Returns `None` for blank lines or malformed JSON — callers should treat
/// `None` as "skip and keep streaming" rather than a fatal error.
#[must_use]
pub fn parse_pull_progress_line(line: &str) -> Option<PullProgress> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }
    serde_json::from_str::<PullProgress>(trimmed).ok()
}

// ─── Host resolution ─────────────────────────────────────────────────────────

fn resolve_host(explicit: &str) -> String {
    let candidate = if explicit.is_empty() {
        std::env::var(OLLAMA_HOST_ENV)
            .ok()
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| DEFAULT_OLLAMA_BASE_URL.to_string())
    } else {
        explicit.to_string()
    };
    candidate.trim_end_matches('/').to_string()
}

// ─── Tags (used by pre-flight "already installed" check) ─────────────────────

/// Probe `/api/tags` and return the list of locally-installed model names.
/// Used as a pre-flight check before issuing `/api/pull` so we can short-circuit
/// when the user asks for a model that's already on disk.
///
/// 3-second hard timeout.  All network errors collapse to
/// `DaemonUnreachable` rather than panicking — the caller is expected to
/// surface a friendly message and refuse the operation.
pub async fn list_installed_models(host: &str) -> Result<Vec<String>, OllamaManageError> {
    let host = resolve_host(host);
    let url = format!("{host}/api/tags");

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .build()
        .map_err(|e| OllamaManageError::DaemonUnreachable(e.to_string()))?;

    let response = client
        .get(&url)
        .send()
        .await
        .map_err(|e| OllamaManageError::DaemonUnreachable(e.to_string()))?;

    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(OllamaManageError::Http {
            status: status.as_u16(),
            body,
        });
    }

    let body: Value = response
        .json()
        .await
        .map_err(|e| OllamaManageError::Parse(e.to_string()))?;
    Ok(extract_tag_names(&body))
}

// ─── Streaming pull ──────────────────────────────────────────────────────────

/// Outcome of a streamed `/api/pull` or `/api/create` operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamOutcome {
    /// Daemon emitted a final `{"status":"success"}` line.
    Success,
    /// Daemon emitted a final `{"error":...}` line.
    Failed(String),
    /// Stream ended without a terminal status — usually a network drop or
    /// daemon restart mid-pull.  The caller should report the last status
    /// it saw.
    Incomplete { last_status: Option<String> },
}

/// POST a streaming JSON request to a daemon endpoint and consume the NDJSON
/// progress stream.  Each parsed [`PullProgress`] is passed to `on_progress`.
///
/// The HTTP body is the JSON value `body` (already shaped by the caller —
/// e.g. `{"name":"qwen3:8b","stream":true}` for `/api/pull`).  The endpoint
/// is `<host>/api/<endpoint>`.
///
/// Returns:
///   * `Ok(StreamOutcome::Success)` on a clean pull/create.
///   * `Ok(StreamOutcome::Failed(msg))` if the daemon emitted an `error` line.
///   * `Ok(StreamOutcome::Incomplete { last_status })` if the stream ended
///     without a terminal line (network drop / Ctrl+C).
///   * `Err` for HTTP/transport failures before the stream started.
pub async fn stream_progress<F>(
    host: &str,
    endpoint: &str,
    body: &Value,
    mut on_progress: F,
) -> Result<StreamOutcome, OllamaManageError>
where
    F: FnMut(&PullProgress),
{
    let host = resolve_host(host);
    let url = format!("{host}/api/{endpoint}");

    // Pulls / creates can take many minutes; do NOT set a global timeout.
    let client = reqwest::Client::builder()
        .build()
        .map_err(|e| OllamaManageError::DaemonUnreachable(e.to_string()))?;

    let mut response = client
        .post(&url)
        .json(body)
        .send()
        .await
        .map_err(|e| OllamaManageError::DaemonUnreachable(e.to_string()))?;

    let status = response.status();
    if !status.is_success() {
        let text = response.text().await.unwrap_or_default();
        return Err(OllamaManageError::Http {
            status: status.as_u16(),
            body: text,
        });
    }

    let mut buffer = String::new();
    let mut last_status: Option<String> = None;
    loop {
        match response.chunk().await {
            Ok(Some(bytes)) => {
                if let Ok(s) = std::str::from_utf8(&bytes) {
                    buffer.push_str(s);
                }
                while let Some(idx) = buffer.find('\n') {
                    let line: String = buffer.drain(..=idx).collect();
                    if let Some(p) = parse_pull_progress_line(&line) {
                        on_progress(&p);
                        if p.is_terminal_error() {
                            return Ok(StreamOutcome::Failed(
                                p.error.unwrap_or_else(|| "daemon reported error".into()),
                            ));
                        }
                        if p.is_terminal_success() {
                            return Ok(StreamOutcome::Success);
                        }
                        last_status = Some(p.status);
                    }
                }
            }
            Ok(None) => break,
            Err(e) => return Err(OllamaManageError::Stream(e.to_string())),
        }
    }
    // Drain any trailing buffer (rare — daemon usually trailing-newlines).
    if !buffer.is_empty() {
        if let Some(p) = parse_pull_progress_line(&buffer) {
            on_progress(&p);
            if p.is_terminal_error() {
                return Ok(StreamOutcome::Failed(
                    p.error.unwrap_or_else(|| "daemon reported error".into()),
                ));
            }
            if p.is_terminal_success() {
                return Ok(StreamOutcome::Success);
            }
            last_status = Some(p.status);
        }
    }
    Ok(StreamOutcome::Incomplete { last_status })
}

/// Pure: extract the `models[].name` array from `/api/tags` JSON.
/// Returns an empty Vec on missing/wrong-shape input rather than erroring,
/// so the "already installed" check fails open (i.e. proceeds with the pull)
/// rather than silently refusing.
#[must_use]
pub fn extract_tag_names(body: &Value) -> Vec<String> {
    body.get("models")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.get("name").and_then(Value::as_str).map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

// ─── Delete (`/ollama rm`) ───────────────────────────────────────────────────

/// POST `/api/delete` to remove a locally-installed model.
/// 200 → Ok.  404 → `ModelNotInstalled`.  Other non-2xx → `Http`.
pub async fn delete_model(host: &str, model: &str) -> Result<(), OllamaManageError> {
    let host = resolve_host(host);
    let url = format!("{host}/api/delete");

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|e| OllamaManageError::DaemonUnreachable(e.to_string()))?;

    let response = client
        .delete(&url)
        .json(&serde_json::json!({ "name": model }))
        .send()
        .await
        .map_err(|e| OllamaManageError::DaemonUnreachable(e.to_string()))?;

    let status = response.status();
    if status.as_u16() == 404 {
        return Err(OllamaManageError::ModelNotInstalled(model.to_string()));
    }
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(OllamaManageError::Http {
            status: status.as_u16(),
            body,
        });
    }
    Ok(())
}

// ─── Copy (`/ollama cp`) ─────────────────────────────────────────────────────

/// POST `/api/copy` to duplicate a local model under a new tag.
/// Non-destructive — the source is left untouched.
pub async fn copy_model(host: &str, source: &str, destination: &str) -> Result<(), OllamaManageError> {
    let host = resolve_host(host);
    let url = format!("{host}/api/copy");

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|e| OllamaManageError::DaemonUnreachable(e.to_string()))?;

    let response = client
        .post(&url)
        .json(&serde_json::json!({ "source": source, "destination": destination }))
        .send()
        .await
        .map_err(|e| OllamaManageError::DaemonUnreachable(e.to_string()))?;

    let status = response.status();
    if status.as_u16() == 404 {
        return Err(OllamaManageError::ModelNotInstalled(source.to_string()));
    }
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(OllamaManageError::Http {
            status: status.as_u16(),
            body,
        });
    }
    Ok(())
}

// ─── Confirmation gate (pure) ────────────────────────────────────────────────

/// Decision returned by [`evaluate_rm_confirmation`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RmConfirmation {
    Proceed,
    /// User typed nothing — treat as cancellation.
    EmptyInput,
    /// User typed something, but it did not match the expected model name
    /// exactly.  No deletion.
    Mismatch,
}

/// Pure decision: does `typed` exactly match `expected`?
///
/// Whitespace at the head/tail is trimmed (the typed input often has trailing
/// `\n` from the terminal).  After trimming, the comparison is byte-for-byte
/// case-sensitive — `qwen3:8b` and `Qwen3:8B` are NOT equivalent.
///
/// Empty input (after trimming) is treated as `EmptyInput` rather than
/// `Mismatch`, so the UI can show a more helpful "cancelled" message.
#[must_use]
pub fn evaluate_rm_confirmation(expected: &str, typed: &str) -> RmConfirmation {
    let trimmed = typed.trim();
    if trimmed.is_empty() {
        return RmConfirmation::EmptyInput;
    }
    if trimmed == expected {
        RmConfirmation::Proceed
    } else {
        RmConfirmation::Mismatch
    }
}

// ─── Modelfile template (pure) ───────────────────────────────────────────────

/// Default Modelfile body pre-populated into the `$EDITOR` buffer for
/// `/ollama create <name>`.  The user can edit any of these directives —
/// `FROM` is the only mandatory one and an empty body is treated as
/// "user cancelled the create".
#[must_use]
pub fn default_modelfile_template() -> &'static str {
    "FROM qwen3:8b\n\
     PARAMETER num_ctx 32768\n\
     PARAMETER temperature 0.7\n\
     SYSTEM \"\"\"\n\
     You are a helpful assistant.\n\
     \"\"\"\n"
}

/// A Modelfile is "effectively empty" when it has no non-whitespace,
/// non-comment content.  Used by `/ollama create` to abort cleanly when
/// the user closed the editor without saving anything meaningful.
#[must_use]
pub fn modelfile_is_effectively_empty(content: &str) -> bool {
    content
        .lines()
        .map(str::trim)
        .all(|line| line.is_empty() || line.starts_with('#'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parse_pull_progress_basic_status() {
        let p = parse_pull_progress_line(r#"{"status":"pulling manifest"}"#).unwrap();
        assert_eq!(p.status, "pulling manifest");
        assert!(p.digest.is_none());
        assert!(p.total.is_none());
        assert!(p.completed.is_none());
        assert!(!p.is_terminal_success());
        assert!(!p.is_terminal_error());
        assert!(p.percent().is_none());
    }

    #[test]
    fn parse_pull_progress_with_bytes() {
        let p = parse_pull_progress_line(
            r#"{"status":"pulling abc123","digest":"abc123","total":1000000,"completed":250000}"#,
        )
        .unwrap();
        assert_eq!(p.status, "pulling abc123");
        assert_eq!(p.digest.as_deref(), Some("abc123"));
        assert_eq!(p.total, Some(1_000_000));
        assert_eq!(p.completed, Some(250_000));
        assert_eq!(p.percent(), Some(25));
    }

    #[test]
    fn parse_pull_progress_verifying() {
        let p = parse_pull_progress_line(r#"{"status":"verifying sha256 digest"}"#).unwrap();
        assert_eq!(p.status, "verifying sha256 digest");
        assert!(!p.is_terminal_success());
    }

    #[test]
    fn parse_pull_progress_success() {
        let p = parse_pull_progress_line(r#"{"status":"success"}"#).unwrap();
        assert!(p.is_terminal_success());
        assert!(!p.is_terminal_error());
    }

    #[test]
    fn parse_pull_progress_error_line() {
        let p = parse_pull_progress_line(
            r#"{"status":"error","error":"pull model manifest: file does not exist"}"#,
        )
        .unwrap();
        assert!(p.is_terminal_error());
        assert!(!p.is_terminal_success());
        assert_eq!(
            p.error.as_deref(),
            Some("pull model manifest: file does not exist")
        );
    }

    #[test]
    fn parse_pull_progress_garbage_returns_none() {
        assert!(parse_pull_progress_line("not json at all").is_none());
        assert!(parse_pull_progress_line("{partial").is_none());
        assert!(parse_pull_progress_line("").is_none());
        assert!(parse_pull_progress_line("   ").is_none());
    }

    #[test]
    fn parse_pull_progress_zero_total_no_percent() {
        let p = parse_pull_progress_line(r#"{"status":"pulling x","total":0,"completed":0}"#)
            .unwrap();
        // total=0 => undefined percent, must NOT be 0% or NaN.
        assert!(p.percent().is_none());
    }

    #[test]
    fn parse_pull_progress_overage_clamps_to_100() {
        // Daemon occasionally reports completed > total at the tail end.
        let p = parse_pull_progress_line(r#"{"status":"pulling x","total":100,"completed":200}"#)
            .unwrap();
        assert_eq!(p.percent(), Some(100));
    }

    #[test]
    fn extract_tag_names_normal_case() {
        let body = json!({
            "models": [
                { "name": "qwen3:8b", "size": 1234 },
                { "name": "llama3.2:3b" },
            ]
        });
        assert_eq!(
            extract_tag_names(&body),
            vec!["qwen3:8b".to_string(), "llama3.2:3b".to_string()]
        );
    }

    #[test]
    fn extract_tag_names_missing_models_field() {
        // /api/tags can return `{}` on a freshly-started daemon with no models.
        // We must NOT panic — return an empty vec so the caller can proceed
        // with the pull.
        assert!(extract_tag_names(&json!({})).is_empty());
        assert!(extract_tag_names(&json!({ "models": null })).is_empty());
        assert!(extract_tag_names(&json!({ "models": "not an array" })).is_empty());
    }

    #[test]
    fn evaluate_rm_confirmation_match_proceeds() {
        assert_eq!(
            evaluate_rm_confirmation("qwen3:8b", "qwen3:8b"),
            RmConfirmation::Proceed
        );
    }

    #[test]
    fn evaluate_rm_confirmation_trims_trailing_whitespace() {
        // Terminal newline must not block the confirmation.
        assert_eq!(
            evaluate_rm_confirmation("qwen3:8b", "qwen3:8b\n"),
            RmConfirmation::Proceed
        );
        assert_eq!(
            evaluate_rm_confirmation("qwen3:8b", "  qwen3:8b  "),
            RmConfirmation::Proceed
        );
    }

    #[test]
    fn evaluate_rm_confirmation_case_sensitive() {
        // Case mismatch is treated as a typo, NOT a confirmation.  This is
        // intentional — Ollama tags are case-sensitive on disk.
        assert_eq!(
            evaluate_rm_confirmation("qwen3:8b", "Qwen3:8B"),
            RmConfirmation::Mismatch
        );
    }

    #[test]
    fn evaluate_rm_confirmation_mismatch_refuses() {
        assert_eq!(
            evaluate_rm_confirmation("qwen3:8b", "llama3.2"),
            RmConfirmation::Mismatch
        );
    }

    #[test]
    fn evaluate_rm_confirmation_empty() {
        assert_eq!(
            evaluate_rm_confirmation("qwen3:8b", ""),
            RmConfirmation::EmptyInput
        );
    }

    #[test]
    fn evaluate_rm_confirmation_whitespace_only() {
        assert_eq!(
            evaluate_rm_confirmation("qwen3:8b", "   \n  \t"),
            RmConfirmation::EmptyInput
        );
    }

    #[test]
    fn modelfile_template_starts_with_from() {
        assert!(default_modelfile_template().starts_with("FROM "));
    }

    #[test]
    fn modelfile_empty_detection() {
        assert!(modelfile_is_effectively_empty(""));
        assert!(modelfile_is_effectively_empty("   \n\n   "));
        assert!(modelfile_is_effectively_empty("# just a comment\n\n# another"));
        assert!(!modelfile_is_effectively_empty("FROM qwen3:8b"));
        assert!(!modelfile_is_effectively_empty("# header\nFROM qwen3:8b"));
    }
}
