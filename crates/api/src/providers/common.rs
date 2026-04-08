/// Shared utilities for provider implementations: retry logic, HTTP helpers,
/// SSE frame parsing, and error mapping.
use std::time::Duration;

use serde::Deserialize;

use crate::error::ApiError;

// ---------------------------------------------------------------------------
// Default retry policy constants (used by both AnvilApiClient and
// OpenAiCompatClient to guarantee identical behaviour out of the box).
// ---------------------------------------------------------------------------

pub const DEFAULT_INITIAL_BACKOFF: Duration = Duration::from_millis(200);
pub const DEFAULT_MAX_BACKOFF: Duration = Duration::from_secs(2);
pub const DEFAULT_MAX_RETRIES: u32 = 2;

// ---------------------------------------------------------------------------
// Request-ID header names used by Anthropic, xAI, and OpenAI.
// ---------------------------------------------------------------------------

pub const REQUEST_ID_HEADER: &str = "request-id";
pub const ALT_REQUEST_ID_HEADER: &str = "x-request-id";

// ---------------------------------------------------------------------------
// Environment variable helpers
// ---------------------------------------------------------------------------

/// Read an environment variable and return `None` if it is absent or empty.
pub fn read_env_non_empty(key: &str) -> Result<Option<String>, ApiError> {
    match std::env::var(key) {
        Ok(value) if !value.is_empty() => Ok(Some(value)),
        Ok(_) | Err(std::env::VarError::NotPresent) => Ok(None),
        Err(error) => Err(ApiError::from(error)),
    }
}

// ---------------------------------------------------------------------------
// Request-ID extraction
// ---------------------------------------------------------------------------

/// Extract the request-ID from response headers, trying the primary header
/// name first and falling back to the alternate.
pub fn request_id_from_headers(headers: &reqwest::header::HeaderMap) -> Option<String> {
    headers
        .get(REQUEST_ID_HEADER)
        .or_else(|| headers.get(ALT_REQUEST_ID_HEADER))
        .and_then(|value| value.to_str().ok())
        .map(ToOwned::to_owned)
}

// ---------------------------------------------------------------------------
// Retryable status codes
// ---------------------------------------------------------------------------

/// Return `true` when the HTTP status code should trigger an automatic retry.
pub const fn is_retryable_status(status: reqwest::StatusCode) -> bool {
    matches!(status.as_u16(), 408 | 409 | 429 | 500 | 502 | 503 | 504)
}

// ---------------------------------------------------------------------------
// Exponential backoff
// ---------------------------------------------------------------------------

/// Compute the sleep duration before `attempt` (1-based).  Uses power-of-two
/// growth capped at `max_backoff`.  Returns `BackoffOverflow` if the shift
/// would overflow a `u32`.
pub fn backoff_for_attempt(
    attempt: u32,
    initial_backoff: Duration,
    max_backoff: Duration,
) -> Result<Duration, ApiError> {
    let Some(multiplier) = 1_u32.checked_shl(attempt.saturating_sub(1)) else {
        return Err(ApiError::BackoffOverflow {
            attempt,
            base_delay: initial_backoff,
        });
    };
    Ok(initial_backoff
        .checked_mul(multiplier)
        .map_or(max_backoff, |delay| delay.min(max_backoff)))
}

// ---------------------------------------------------------------------------
// Retry loop
// ---------------------------------------------------------------------------

/// Execute `send_fn` with automatic retries on retryable errors.
///
/// `max_retries` is the number of *extra* attempts after the first, so the
/// total attempt count is `max_retries + 1`.
///
/// Returns the successful `reqwest::Response` or a `RetriesExhausted` error
/// wrapping the last retryable error encountered.
pub async fn send_with_retry<F, Fut>(
    max_retries: u32,
    initial_backoff: Duration,
    max_backoff: Duration,
    send_fn: F,
) -> Result<reqwest::Response, ApiError>
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = Result<reqwest::Response, ApiError>>,
{
    let mut attempts = 0u32;

    let last_error = loop {
        attempts += 1;
        let retryable_error = match send_fn().await {
            Ok(response) => match expect_success(response).await {
                Ok(response) => return Ok(response),
                Err(error) if error.is_retryable() && attempts <= max_retries + 1 => error,
                Err(error) => return Err(error),
            },
            Err(error) if error.is_retryable() && attempts <= max_retries + 1 => error,
            Err(error) => return Err(error),
        };

        if attempts > max_retries {
            break retryable_error;
        }

        tokio::time::sleep(backoff_for_attempt(attempts, initial_backoff, max_backoff)?).await;
    };

    Err(ApiError::RetriesExhausted {
        attempts,
        last_error: Box::new(last_error),
    })
}

// ---------------------------------------------------------------------------
// Success / error response handling
// ---------------------------------------------------------------------------

/// Assert that `response` has a 2xx status code and return it unchanged.
///
/// For non-2xx responses the body is consumed, an attempt is made to parse
/// it as the common `{"error": {"type": "...", "message": "..."}}` envelope,
/// and the resulting `ApiError::Api` is returned.  Unknown envelope shapes
/// fall back gracefully — `error_type` / `message` will simply be `None`.
pub async fn expect_success(response: reqwest::Response) -> Result<reqwest::Response, ApiError> {
    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }

    let body = response.text().await.unwrap_or_default();
    let parsed = serde_json::from_str::<ErrorEnvelope>(&body).ok();
    let retryable = is_retryable_status(status);

    Err(ApiError::Api {
        status,
        error_type: parsed.as_ref().and_then(|e| e.error.error_type.clone()),
        message: parsed.as_ref().and_then(|e| e.error.message.clone()),
        body,
        retryable,
    })
}

/// A flexible error envelope that covers both the Anthropic API shape
/// (`{"type":"...", "message":"..."}` with required string fields) and the
/// OpenAI-compatible shape (same keys but nullable).  Both are decoded into
/// `Option<String>` so a single deserialization covers all providers.
#[derive(Debug, Deserialize)]
struct ErrorEnvelope {
    error: ErrorBody,
}

#[derive(Debug, Deserialize)]
struct ErrorBody {
    #[serde(rename = "type", default)]
    error_type: Option<String>,
    #[serde(default)]
    message: Option<String>,
}

// ---------------------------------------------------------------------------
// SSE frame splitting (OpenAI-compatible raw byte buffer)
// ---------------------------------------------------------------------------

/// Scan `buffer` for the next complete SSE frame boundary (`\n\n` or
/// `\r\n\r\n`).  When found, drain that range from `buffer` and return the
/// frame text *without* the trailing separator.
pub fn next_sse_frame(buffer: &mut Vec<u8>) -> Option<String> {
    let separator = buffer
        .windows(2)
        .position(|window| window == b"\n\n")
        .map(|position| (position, 2))
        .or_else(|| {
            buffer
                .windows(4)
                .position(|window| window == b"\r\n\r\n")
                .map(|position| (position, 4))
        })?;

    let (position, separator_len) = separator;
    let frame = buffer.drain(..position + separator_len).collect::<Vec<_>>();
    let frame_len = frame.len().saturating_sub(separator_len);
    Some(String::from_utf8_lossy(&frame[..frame_len]).into_owned())
}

/// Parse a raw SSE frame string into the `data:` payload lines joined as a
/// single string.  Returns `None` for empty frames, comment-only frames, and
/// the special `[DONE]` sentinel.
///
/// Unlike the Anthropic-specific `sse::parse_frame`, this function does **not**
/// attempt to deserialize into a typed event — callers are responsible for
/// deserializing the returned string into their own event type.
pub fn extract_sse_data(frame: &str) -> Option<String> {
    let trimmed = frame.trim();
    if trimmed.is_empty() {
        return None;
    }

    let mut data_lines = Vec::new();
    for line in trimmed.lines() {
        if line.starts_with(':') {
            continue;
        }
        if let Some(data) = line.strip_prefix("data:") {
            data_lines.push(data.trim_start().to_owned());
        }
    }

    if data_lines.is_empty() {
        return None;
    }

    let payload = data_lines.join("\n");
    if payload == "[DONE]" {
        return None;
    }

    Some(payload)
}

#[cfg(test)]
mod tests {
    use super::{
        backoff_for_attempt, extract_sse_data, is_retryable_status, next_sse_frame,
        read_env_non_empty,
    };
    use std::time::Duration;

    #[test]
    fn backoff_doubles_until_cap() {
        let initial = Duration::from_millis(10);
        let max = Duration::from_millis(25);
        assert_eq!(backoff_for_attempt(1, initial, max).unwrap(), Duration::from_millis(10));
        assert_eq!(backoff_for_attempt(2, initial, max).unwrap(), Duration::from_millis(20));
        assert_eq!(backoff_for_attempt(3, initial, max).unwrap(), Duration::from_millis(25));
    }

    #[test]
    fn retryable_status_set() {
        assert!(is_retryable_status(reqwest::StatusCode::TOO_MANY_REQUESTS));
        assert!(is_retryable_status(reqwest::StatusCode::INTERNAL_SERVER_ERROR));
        assert!(!is_retryable_status(reqwest::StatusCode::UNAUTHORIZED));
        assert!(!is_retryable_status(reqwest::StatusCode::BAD_REQUEST));
    }

    #[test]
    fn read_env_non_empty_returns_none_for_missing() {
        // Use an env key that is extremely unlikely to be set in CI.
        assert_eq!(
            read_env_non_empty("__ANVIL_TEST_ABSENT_KEY__").unwrap(),
            None
        );
    }

    #[test]
    fn next_sse_frame_splits_on_double_newline() {
        let mut buf = b"data: hello\n\ndata: world\n\n".to_vec();
        let first = next_sse_frame(&mut buf).unwrap();
        assert_eq!(first, "data: hello");
        let second = next_sse_frame(&mut buf).unwrap();
        assert_eq!(second, "data: world");
        assert!(next_sse_frame(&mut buf).is_none());
    }

    #[test]
    fn next_sse_frame_splits_on_crlf_double() {
        let mut buf = b"data: hello\r\n\r\ndata: world\r\n\r\n".to_vec();
        let first = next_sse_frame(&mut buf).unwrap();
        assert_eq!(first, "data: hello");
    }

    #[test]
    fn extract_sse_data_ignores_comments_and_done() {
        assert_eq!(extract_sse_data(": keepalive\n\n"), None);
        assert_eq!(extract_sse_data("data: [DONE]\n\n"), None);
        assert_eq!(
            extract_sse_data("data: {\"id\":\"1\"}\n\n"),
            Some("{\"id\":\"1\"}".to_owned())
        );
    }
}
