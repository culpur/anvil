use serde::Deserialize;

use crate::error::ApiError;
use crate::types::{MessageRequest, MessageResponse};

use super::openai_compat::{MessageStream, OpenAiCompatClient};
use super::{Provider, ProviderFuture};

pub const DEFAULT_OLLAMA_BASE_URL: &str = "http://localhost:11434";
pub const OLLAMA_HOST_ENV: &str = "OLLAMA_HOST";

/// A thin wrapper around `OpenAiCompatClient` that targets a local Ollama
/// instance.  Ollama exposes an OpenAI-compatible `/v1/chat/completions`
/// endpoint, so no format translation is needed — only the base URL and auth
/// treatment (no key required) differ.
#[derive(Debug, Clone)]
pub struct OllamaClient {
    inner: OpenAiCompatClient,
}

impl OllamaClient {
    /// Create a client pointing at the given base URL with no auth.
    #[must_use]
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            inner: OpenAiCompatClient::new_no_auth(base_url),
        }
    }

    /// Create a client reading `OLLAMA_HOST` from the environment, falling
    /// back to `http://localhost:11434`.
    #[must_use]
    pub fn from_env() -> Self {
        let base_url = std::env::var(OLLAMA_HOST_ENV)
            .ok()
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| DEFAULT_OLLAMA_BASE_URL.to_string());
        Self::new(base_url)
    }

    /// Probe Ollama's `/api/tags` endpoint to confirm the daemon is running.
    /// Returns `true` if reachable, `false` otherwise.  Never propagates
    /// network errors — a failure simply means Ollama is not available.
    pub async fn is_available(&self) -> bool {
        let base_url = self.inner.base_url();
        let url = format!("{}/api/tags", base_url.trim_end_matches('/'));
        reqwest::Client::new()
            .get(&url)
            .timeout(std::time::Duration::from_secs(2))
            .send()
            .await
            .map(|response| response.status().is_success())
            .unwrap_or(false)
    }

    /// Return the list of locally available Ollama model names by querying
    /// `/api/tags`.
    pub async fn list_models(&self) -> Result<Vec<String>, ApiError> {
        let base_url = self.inner.base_url();
        let url = format!("{}/api/tags", base_url.trim_end_matches('/'));
        let response = reqwest::Client::new()
            .get(&url)
            .send()
            .await
            .map_err(ApiError::from)?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(ApiError::Api {
                status,
                error_type: None,
                message: Some("Ollama /api/tags request failed".to_string()),
                body,
                retryable: false,
            });
        }

        let payload = response.json::<OllamaTagsResponse>().await?;
        Ok(payload.models.into_iter().map(|m| m.name).collect())
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

impl Provider for OllamaClient {
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

#[derive(Debug, Deserialize)]
struct OllamaTagsResponse {
    models: Vec<OllamaModelEntry>,
}

#[derive(Debug, Deserialize)]
struct OllamaModelEntry {
    name: String,
}
