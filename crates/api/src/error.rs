use std::env::VarError;
use std::fmt::{Display, Formatter};
use std::time::Duration;

#[derive(Debug)]
pub enum ApiError {
    MissingCredentials {
        provider: &'static str,
        env_vars: &'static [&'static str],
    },
    ExpiredOAuthToken,
    Auth(String),
    InvalidApiKeyEnv(VarError),
    Http(reqwest::Error),
    Io(std::io::Error),
    Json(serde_json::Error),
    Api {
        status: reqwest::StatusCode,
        error_type: Option<String>,
        message: Option<String>,
        body: String,
        retryable: bool,
        /// Parsed value of the `Retry-After` response header (seconds), if
        /// present and valid.  Only populated on 429 responses; `None`
        /// everywhere else.
        retry_after_secs: Option<u64>,
        /// Human-readable provider hint appended to 5xx error messages (#568,
        /// CC-143-B).  `None` leaves the message unchanged.
        /// Example: `"Ollama at localhost:11434 — check the local daemon is running"`
        provider_hint: Option<String>,
    },
    /// The stream produced no data for longer than the dead-air timeout.
    StreamStalled {
        elapsed_ms: u64,
    },
    RetriesExhausted {
        attempts: u32,
        last_error: Box<ApiError>,
    },
    InvalidSseFrame(&'static str),
    BackoffOverflow {
        attempt: u32,
        base_delay: Duration,
    },
}

impl ApiError {
    #[must_use]
    pub const fn missing_credentials(
        provider: &'static str,
        env_vars: &'static [&'static str],
    ) -> Self {
        Self::MissingCredentials { provider, env_vars }
    }

    /// Attach a provider hint to an `ApiError::Api` error.
    ///
    /// For 5xx errors the hint is appended to the Display output so users
    /// see actionable guidance (e.g. "check Ollama daemon", "check AWS health")
    /// instead of a generic HTTP error string.  Has no effect on non-`Api`
    /// variants (they are returned unchanged).
    #[must_use]
    pub fn with_provider_hint(self, hint: impl Into<String>) -> Self {
        match self {
            Self::Api {
                status,
                error_type,
                message,
                body,
                retryable,
                retry_after_secs,
                ..
            } => Self::Api {
                status,
                error_type,
                message,
                body,
                retryable,
                retry_after_secs,
                provider_hint: Some(hint.into()),
            },
            other => other,
        }
    }

    /// Build a provider hint string from a provider display name and base URL.
    ///
    /// Produces copy appropriate for the provider:
    /// - Anthropic / api.anthropic.com → names Anthropic + status page
    /// - Ollama / localhost            → tells the user to check the daemon
    /// - Other                         → names the provider and URL
    #[must_use]
    pub fn provider_hint_for(provider_display: &str, base_url: &str) -> String {
        let lower = provider_display.to_lowercase();
        if lower.contains("anthropic") || base_url.contains("api.anthropic.com") {
            "Anthropic API at api.anthropic.com — check status at status.anthropic.com".to_string()
        } else if lower.contains("ollama")
            || base_url.contains("localhost:11434")
            || base_url.contains("127.0.0.1:11434")
        {
            format!("Ollama at {base_url} — check the local daemon is running")
        } else if lower.contains("aws") || lower.contains("bedrock") {
            "AWS Bedrock — check AWS service health".to_string()
        } else if lower.contains("openai") {
            format!("OpenAI API at {base_url}")
        } else {
            // Generic: extract the hostname from the URL for a terse message.
            let host = base_url
                .trim_start_matches("https://")
                .trim_start_matches("http://")
                .split('/')
                .next()
                .unwrap_or(base_url);
            format!("{provider_display} at {host}")
        }
    }

    /// Task #564: detect context-overflow errors across providers and
    /// return the model-reported overflow in tokens (best-effort; `0`
    /// when the body doesn't carry a parseable count).
    ///
    /// Recognized patterns:
    /// - **Anthropic**: HTTP 400 with `error.type == "invalid_request_error"`
    ///   AND a message containing "prompt is too long".
    /// - **OpenAI** (and OpenAI-compat / Azure / Copilot / Cursor):
    ///   HTTP 400 with body containing "context_length_exceeded" or
    ///   "maximum context length" or "context window".
    /// - **Ollama**: HTTP 500 (or 400) with body containing "context length"
    ///   or "ggml" + "context".
    ///
    /// Returns `None` when the error does not match any known overflow
    /// pattern.  Callers should NOT retry on `Some(_)` without compacting.
    #[must_use]
    pub fn context_too_long_overflow(&self) -> Option<u32> {
        let Self::Api {
            status,
            error_type,
            message,
            body,
            ..
        } = self
        else {
            return None;
        };
        let code = status.as_u16();
        // Pull the lowercased searchable text from message + body so we
        // only run the substring scan once.
        let mut haystack = String::new();
        if let Some(m) = message {
            haystack.push_str(&m.to_lowercase());
            haystack.push('\n');
        }
        haystack.push_str(&body.to_lowercase());
        let et = error_type.as_deref().unwrap_or("").to_lowercase();

        let matched = if code == 400 {
            // Anthropic: invalid_request_error + "prompt is too long".
            (et == "invalid_request_error" && haystack.contains("prompt is too long"))
                // OpenAI / Azure / Copilot / Cursor common phrasings.
                || haystack.contains("context_length_exceeded")
                || haystack.contains("maximum context length")
                || haystack.contains("context window")
                || haystack.contains("prompt is too long")
                || haystack.contains("context length")
                || haystack.contains("string too long")
        } else if code == 500 {
            // Ollama: surfaces context overflow as a 500 with "context length"
            // / "context window" in the body.  Some local llama.cpp builds
            // mention "ggml" + "context".
            haystack.contains("context length")
                || haystack.contains("context window")
                || (haystack.contains("ggml") && haystack.contains("context"))
        } else {
            false
        };

        if !matched {
            return None;
        }
        Some(parse_overflow_tokens(&haystack).unwrap_or(0))
    }

    #[must_use]
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::Http(error) => error.is_connect() || error.is_timeout() || error.is_request(),
            Self::Api { retryable, .. } => *retryable,
            Self::RetriesExhausted { last_error, .. } => last_error.is_retryable(),
            Self::MissingCredentials { .. }
            | Self::ExpiredOAuthToken
            | Self::Auth(_)
            | Self::InvalidApiKeyEnv(_)
            | Self::Io(_)
            | Self::Json(_)
            | Self::InvalidSseFrame(_)
            | Self::BackoffOverflow { .. }
            | Self::StreamStalled { .. } => false,
        }
    }

    /// Return the server-supplied `Retry-After` hint in seconds, if present.
    /// Only `ApiError::Api` carries this value (populated from the 429
    /// response header); all other variants return `None`.
    #[must_use]
    pub fn retry_after_secs(&self) -> Option<u64> {
        match self {
            Self::Api { retry_after_secs, .. } => *retry_after_secs,
            _ => None,
        }
    }
}

impl Display for ApiError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingCredentials { provider, env_vars } => write!(
                f,
                "missing {provider} credentials; export {} before calling the {provider} API",
                env_vars.join(" or ")
            ),
            Self::ExpiredOAuthToken => {
                write!(
                    f,
                    "saved OAuth token is expired and no refresh token is available"
                )
            }
            Self::Auth(message) => write!(f, "auth error: {message}"),
            Self::InvalidApiKeyEnv(error) => {
                write!(f, "failed to read credential environment variable: {error}")
            }
            Self::Http(error) => write!(f, "http error: {error}"),
            Self::Io(error) => write!(f, "io error: {error}"),
            Self::Json(error) => write!(f, "json error: {error}"),
            Self::StreamStalled { elapsed_ms } => write!(
                f,
                "stream stalled after {elapsed_ms}ms of no data"
            ),
            Self::Api {
                status,
                error_type,
                message,
                body,
                provider_hint,
                ..
            } => {
                let base = match (error_type, message) {
                    (Some(error_type), Some(message)) => {
                        format!("api returned {status} ({error_type}): {message}")
                    }
                    _ => format!("api returned {status}: {body}"),
                };
                // On 5xx responses, append the provider-specific hint so
                // users on 3P providers (Ollama, OpenAI-compat, etc.) get
                // actionable guidance instead of generic Anthropic copy.
                if status.is_server_error()
                    && let Some(hint) = provider_hint
                    && !hint.is_empty()
                {
                    write!(f, "{base} [{hint}]")
                } else {
                    write!(f, "{base}")
                }
            }
            Self::RetriesExhausted {
                attempts,
                last_error,
            } => write!(f, "api failed after {attempts} attempts: {last_error}"),
            Self::InvalidSseFrame(message) => write!(f, "invalid sse frame: {message}"),
            Self::BackoffOverflow {
                attempt,
                base_delay,
            } => write!(
                f,
                "retry backoff overflowed on attempt {attempt} with base delay {base_delay:?}"
            ),
        }
    }
}

impl std::error::Error for ApiError {}

/// Task #564: best-effort extraction of a model-reported overflow
/// token count from a (lowercased) overflow error body.  Looks for
/// phrases like "exceeds the context window of 200000 tokens" or
/// "tokens (...) > 128000" and returns the first plausible integer
/// after the marker.  Returns `None` when no integer can be located.
fn parse_overflow_tokens(haystack_lower: &str) -> Option<u32> {
    // Strategy: scan for the first integer >= 1024 in the haystack.
    // Provider messages mention 1024+ token windows; smaller values
    // (HTTP statuses, error codes, etc.) are unlikely to be meaningful
    // overflow figures and are skipped.
    let bytes = haystack_lower.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_digit() {
            let start = i;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
            if let Ok(n) = haystack_lower[start..i].parse::<u32>() {
                if n >= 1024 {
                    return Some(n);
                }
            }
        } else {
            i += 1;
        }
    }
    None
}

impl From<reqwest::Error> for ApiError {
    fn from(value: reqwest::Error) -> Self {
        Self::Http(value)
    }
}

impl From<std::io::Error> for ApiError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<serde_json::Error> for ApiError {
    fn from(value: serde_json::Error) -> Self {
        Self::Json(value)
    }
}

impl From<VarError> for ApiError {
    fn from(value: VarError) -> Self {
        Self::InvalidApiKeyEnv(value)
    }
}

#[cfg(test)]
mod tests {
    use super::ApiError;

    fn make_api_error(status_u16: u16) -> ApiError {
        ApiError::Api {
            status: reqwest::StatusCode::from_u16(status_u16).unwrap(),
            error_type: None,
            message: Some("test error".to_string()),
            body: String::new(),
            retryable: status_u16 >= 500,
            retry_after_secs: None,
            provider_hint: None,
        }
    }

    #[test]
    fn error_5xx_anthropic_names_anthropic() {
        let err = make_api_error(503).with_provider_hint(
            ApiError::provider_hint_for("Anthropic", "api.anthropic.com"),
        );
        let msg = err.to_string();
        assert!(
            msg.contains("status.anthropic.com"),
            "expected Anthropic status URL in 5xx msg: {msg}"
        );
        assert!(
            msg.contains("Anthropic API"),
            "expected 'Anthropic API' in msg: {msg}"
        );
    }

    #[test]
    fn error_5xx_ollama_names_ollama_host() {
        let err = make_api_error(503).with_provider_hint(
            ApiError::provider_hint_for("Ollama", "http://localhost:11434"),
        );
        let msg = err.to_string();
        assert!(
            msg.contains("Ollama") && msg.contains("localhost"),
            "expected Ollama + localhost in 5xx msg: {msg}"
        );
        assert!(
            msg.contains("daemon"),
            "expected daemon check hint: {msg}"
        );
    }

    #[test]
    fn error_5xx_openai_compat_names_provider_url() {
        let err = make_api_error(503).with_provider_hint(
            ApiError::provider_hint_for("Groq", "https://api.groq.com/openai/v1"),
        );
        let msg = err.to_string();
        assert!(
            msg.contains("Groq") || msg.contains("api.groq.com"),
            "expected Groq or its URL in 5xx msg: {msg}"
        );
    }

    // ─── Task #564: context-too-long detection across providers ─────────────

    fn make_api_error_full(
        status_u16: u16,
        error_type: Option<&str>,
        message: Option<&str>,
        body: &str,
    ) -> ApiError {
        ApiError::Api {
            status: reqwest::StatusCode::from_u16(status_u16).unwrap(),
            error_type: error_type.map(String::from),
            message: message.map(String::from),
            body: body.to_string(),
            retryable: false,
            retry_after_secs: None,
            provider_hint: None,
        }
    }

    #[test]
    fn anthropic_provider_detects_prompt_too_long_returns_context_too_long() {
        let err = make_api_error_full(
            400,
            Some("invalid_request_error"),
            Some("prompt is too long: 250000 tokens > 200000 maximum"),
            r#"{"error":{"type":"invalid_request_error","message":"prompt is too long: 250000 tokens > 200000 maximum"}}"#,
        );
        let overflow = err
            .context_too_long_overflow()
            .expect("Anthropic 400 prompt-too-long must map to ContextTooLong");
        // The first parseable >= 1024 integer is 250000.
        assert!(
            overflow >= 1024,
            "expected non-trivial overflow token count, got {overflow}"
        );
    }

    #[test]
    fn openai_provider_detects_context_length_exceeded() {
        let err = make_api_error_full(
            400,
            Some("invalid_request_error"),
            Some(
                "This model's maximum context length is 128000 tokens. \
                 However, your messages resulted in 130000 tokens.",
            ),
            r#"{"error":{"code":"context_length_exceeded","message":"maximum context length"}}"#,
        );
        assert!(err.context_too_long_overflow().is_some());
    }

    #[test]
    fn ollama_provider_detects_context_overflow() {
        // Ollama surfaces overflow as a 500 with a body mentioning
        // "context length" (the daemon's wording for window overflow).
        let err = make_api_error_full(
            500,
            None,
            Some("context length exceeded"),
            "context length exceeded by 4096 tokens",
        );
        let overflow = err
            .context_too_long_overflow()
            .expect("Ollama 500 context-length must map to ContextTooLong");
        assert!(overflow >= 1024);
    }

    #[test]
    fn context_too_long_returns_none_for_unrelated_errors() {
        // A 400 for a different reason must not be confused for overflow.
        let err = make_api_error_full(
            400,
            Some("invalid_request_error"),
            Some("model parameter is required"),
            r#"{"error":"model parameter is required"}"#,
        );
        assert!(err.context_too_long_overflow().is_none());

        // A 503 (server-error) must not match either.
        let err2 = make_api_error_full(503, None, Some("upstream busy"), "{}");
        assert!(err2.context_too_long_overflow().is_none());
    }

    #[test]
    fn error_5xx_never_says_claude_status_for_3p() {
        for (provider, url) in &[
            ("Ollama", "http://localhost:11434"),
            ("Groq", "https://api.groq.com"),
            ("OpenAI", "https://api.openai.com"),
        ] {
            let err = make_api_error(503)
                .with_provider_hint(ApiError::provider_hint_for(provider, url));
            let msg = err.to_string();
            assert!(
                !msg.contains("status.claude.com"),
                "3P provider {provider} must not mention status.claude.com: {msg}"
            );
        }
    }
}
