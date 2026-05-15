//! Azure OpenAI provider.
//!
//! Auth: `api-key` header (primary) or `Authorization: Bearer <AAD token>`
//! (when `AZURE_AD_TOKEN` is set).
//!
//! Required env vars:
//!   `AZURE_OPENAI_ENDPOINT`        — e.g. `https://myresource.openai.azure.com`
//!   `AZURE_OPENAI_API_KEY`         — API key
//!   `AZURE_OPENAI_DEPLOYMENT_NAME` — deployment name (e.g. `gpt-4o`)
//!   `AZURE_OPENAI_API_VERSION`     — e.g. `2025-02-01`
//!
//! Optional:
//!   `AZURE_AD_TOKEN` — AAD bearer token (takes precedence over api-key).
//!
//! URL pattern:
//!   `{endpoint}/openai/deployments/{deployment}/chat/completions?api-version={version}`

use std::time::Duration;
use std::collections::VecDeque;
use std::time::Instant;

use serde::Deserialize;
use serde_json::{json, Value};

use crate::error::ApiError;
use crate::types::{
    ContentBlockDelta, ContentBlockDeltaEvent, ContentBlockStartEvent, ContentBlockStopEvent,
    MessageDelta, MessageDeltaEvent, MessageRequest, MessageResponse,
    MessageStartEvent, MessageStopEvent, OutputContentBlock, StreamEvent, Usage,
};
use super::common::{extract_sse_data, next_sse_frame, request_id_from_headers};
use super::openai_compat::{
    build_chat_completion_request_value, resolve_stream_dead_air_timeout,
};
use super::{Provider, ProviderFuture};

pub const DEFAULT_API_VERSION: &str = "2025-02-01";

/// Build the Azure OpenAI chat-completions URL.
///
/// Format: `{endpoint}/openai/deployments/{deployment}/chat/completions?api-version={version}`
#[must_use]
pub fn build_azure_url(endpoint: &str, deployment: &str, api_version: &str) -> String {
    let base = endpoint.trim_end_matches('/');
    format!(
        "{base}/openai/deployments/{deployment}/chat/completions?api-version={api_version}"
    )
}

fn read_env(key: &str) -> Result<String, ApiError> {
    std::env::var(key)
        .ok()
        .filter(|v| !v.is_empty())
        .ok_or_else(|| ApiError::Auth(format!("Azure OpenAI: {key} is required but not set")))
}

// ---------------------------------------------------------------------------
// AzureOpenAiClient
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct AzureOpenAiClient {
    http: reqwest::Client,
    url: String,
    api_key: Option<String>,
    aad_token: Option<String>,
}

impl AzureOpenAiClient {
    pub fn from_env() -> Result<Self, ApiError> {
        let endpoint = read_env("AZURE_OPENAI_ENDPOINT")?;
        let deployment = read_env("AZURE_OPENAI_DEPLOYMENT_NAME")?;
        let api_version = std::env::var("AZURE_OPENAI_API_VERSION")
            .ok()
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| DEFAULT_API_VERSION.to_string());

        let api_key = std::env::var("AZURE_OPENAI_API_KEY")
            .ok()
            .filter(|v| !v.is_empty());
        let aad_token = std::env::var("AZURE_AD_TOKEN")
            .ok()
            .filter(|v| !v.is_empty());

        if api_key.is_none() && aad_token.is_none() {
            return Err(ApiError::missing_credentials(
                "Azure OpenAI",
                &["AZURE_OPENAI_API_KEY", "AZURE_AD_TOKEN"],
            ));
        }

        let url = build_azure_url(&endpoint, &deployment, &api_version);

        Ok(Self {
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(600))
                .build()
                .unwrap_or_default(),
            url,
            api_key,
            aad_token,
        })
    }

    fn apply_auth(&self, builder: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        if let Some(token) = &self.aad_token {
            builder.bearer_auth(token)
        } else if let Some(key) = &self.api_key {
            builder.header("api-key", key.as_str())
        } else {
            builder
        }
    }

    pub async fn send_message(
        &self,
        request: &MessageRequest,
    ) -> Result<MessageResponse, ApiError> {
        let payload = build_chat_completion_request_value(request, false);
        let builder = self
            .http
            .post(&self.url)
            .header("content-type", "application/json");
        let builder = self.apply_auth(builder);
        let response = builder
            .json(&payload)
            .send()
            .await
            .map_err(ApiError::Http)?;

        let status = response.status();
        if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
            return Err(ApiError::Auth(format!(
                "Azure OpenAI returned {status} — check AZURE_OPENAI_API_KEY or AZURE_AD_TOKEN"
            )));
        }
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(ApiError::Api {
                status,
                error_type: None,
                message: None,
                body,
                retryable: status.as_u16() >= 500,
                retry_after_secs: None,
            });
        }

        let request_id = request_id_from_headers(response.headers());
        let raw: Value = response.json().await.map_err(ApiError::Http)?;
        normalize_azure_response(request_id, &request.model, raw)
    }

    pub async fn stream_message(
        &self,
        request: &MessageRequest,
    ) -> Result<AzureMessageStream, ApiError> {
        let payload = build_chat_completion_request_value(request, true);
        let builder = self
            .http
            .post(&self.url)
            .header("content-type", "application/json");
        let builder = self.apply_auth(builder);
        let response = builder
            .json(&payload)
            .send()
            .await
            .map_err(ApiError::Http)?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(ApiError::Api {
                status,
                error_type: None,
                message: None,
                body,
                retryable: status.as_u16() >= 500,
                retry_after_secs: None,
            });
        }

        Ok(AzureMessageStream {
            request_id: request_id_from_headers(response.headers()),
            response,
            buffer: Vec::new(),
            pending: VecDeque::new(),
            done: false,
            model: request.model.clone(),
            message_started: false,
            text_started: false,
            last_chunk_at: Instant::now(),
            dead_air_timeout: resolve_stream_dead_air_timeout(),
        })
    }
}

impl Provider for AzureOpenAiClient {
    type Stream = AzureMessageStream;

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
// Response normalisation
// ---------------------------------------------------------------------------

fn normalize_azure_response(
    request_id: Option<String>,
    model: &str,
    raw: Value,
) -> Result<MessageResponse, ApiError> {
    let id = raw["id"].as_str().unwrap_or("").to_string();
    let resp_model = raw["model"].as_str().unwrap_or(model).to_string();
    let choice = raw["choices"]
        .as_array()
        .and_then(|arr| arr.first())
        .ok_or(ApiError::InvalidSseFrame("Azure response missing choices"))?;
    let text = choice["message"]["content"].as_str().unwrap_or("").to_string();
    let finish_reason = choice["finish_reason"].as_str().map(|r| match r {
        "stop" => "end_turn".to_string(),
        other => other.to_string(),
    });
    let input_tokens = raw["usage"]["prompt_tokens"].as_u64().unwrap_or(0) as u32;
    let output_tokens = raw["usage"]["completion_tokens"].as_u64().unwrap_or(0) as u32;

    Ok(MessageResponse {
        id,
        kind: "message".to_string(),
        role: "assistant".to_string(),
        content: vec![OutputContentBlock::Text { text }],
        model: resp_model,
        stop_reason: finish_reason,
        stop_sequence: None,
        usage: Usage {
            input_tokens,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
            output_tokens,
        },
        request_id,
    })
}

// ---------------------------------------------------------------------------
// Streaming (re-uses SSE helpers from openai_compat)
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct AzureMessageStream {
    request_id: Option<String>,
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

impl AzureMessageStream {
    #[must_use]
    pub fn request_id(&self) -> Option<&str> {
        self.request_id.as_deref()
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
            ).await;

            match chunk_result {
                Ok(Ok(Some(chunk))) => {
                    self.last_chunk_at = Instant::now();
                    self.buffer.extend_from_slice(&chunk);
                    while let Some(frame) = next_sse_frame(&mut self.buffer) {
                        if let Some(data) = extract_sse_data(&frame) {
                            if data == "[DONE]" {
                                self.done = true;
                                self.flush_finish();
                                break;
                            }
                            if let Ok(v) = serde_json::from_str::<Value>(&data) {
                                self.ingest_chunk(v);
                            }
                        }
                    }
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

    fn ingest_chunk(&mut self, v: Value) {
        if !self.message_started {
            self.message_started = true;
            let id = v["id"].as_str().unwrap_or("").to_string();
            let model = v["model"].as_str().unwrap_or(&self.model).to_string();
            self.pending.push_back(StreamEvent::MessageStart(MessageStartEvent {
                message: MessageResponse {
                    id,
                    kind: "message".to_string(),
                    role: "assistant".to_string(),
                    content: Vec::new(),
                    model,
                    stop_reason: None,
                    stop_sequence: None,
                    usage: Usage::default(),
                    request_id: None,
                },
            }));
        }

        if let Some(choices) = v["choices"].as_array() {
            for choice in choices {
                if let Some(text) = choice["delta"]["content"].as_str() {
                    if !text.is_empty() {
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
                }

                let finish = choice["finish_reason"].as_str();
                if finish.is_some() {
                    let stop_reason = finish
                        .map(|r| if r == "stop" { "end_turn" } else { r })
                        .unwrap_or("end_turn")
                        .to_string();
                    if self.text_started {
                        self.pending.push_back(StreamEvent::ContentBlockStop(
                            ContentBlockStopEvent { index: 0 },
                        ));
                    }
                    self.pending.push_back(StreamEvent::MessageDelta(MessageDeltaEvent {
                        delta: MessageDelta {
                            stop_reason: Some(stop_reason),
                            stop_sequence: None,
                        },
                        usage: Usage::default(),
                    }));
                    self.pending.push_back(StreamEvent::MessageStop(MessageStopEvent {}));
                }
            }
        }
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
            self.message_started = false;
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_azure_url_correctly() {
        let url = build_azure_url(
            "https://myresource.openai.azure.com",
            "gpt-4o",
            "2025-02-01",
        );
        assert_eq!(
            url,
            "https://myresource.openai.azure.com/openai/deployments/gpt-4o/chat/completions?api-version=2025-02-01"
        );
    }

    #[test]
    fn builds_azure_url_strips_trailing_slash() {
        let url = build_azure_url(
            "https://myresource.openai.azure.com/",
            "gpt-4o-mini",
            "2025-02-01",
        );
        assert!(url.contains("deployments/gpt-4o-mini/chat/completions"));
        assert!(!url.contains("//openai"));
    }

    #[test]
    fn from_env_errors_when_endpoint_missing() {
        // Edition 2024: env::remove_var requires unsafe.
        #![allow(unsafe_code)]
        unsafe {
            std::env::remove_var("AZURE_OPENAI_ENDPOINT");
        }
        let result = AzureOpenAiClient::from_env();
        assert!(result.is_err(), "must error when AZURE_OPENAI_ENDPOINT is absent");
    }
}
