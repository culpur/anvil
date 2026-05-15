//! AWS Bedrock provider with manual SigV4 request signing.
//!
//! Uses `InvokeModelWithResponseStream` for streaming and `InvokeModel` for
//! non-streaming.  No external AWS SDK — SigV4 is implemented directly using
//! the `hmac` and `sha2` crates (both already transitive dependencies).
//!
//! Required env vars:
//!   `AWS_ACCESS_KEY_ID`      — IAM access key
//!   `AWS_SECRET_ACCESS_KEY`  — IAM secret key
//!   `AWS_REGION`             — e.g. `us-east-1`
//!
//! Optional:
//!   `AWS_SESSION_TOKEN`      — temporary session token (STS / IAM role assumed)
//!   `AWS_BEDROCK_ENDPOINT`   — override the Bedrock endpoint (testing / PrivateLink)
//!
//! Model IDs follow the Bedrock naming convention, e.g.:
//!   `anthropic.claude-3-5-sonnet-20241022-v2:0`
//!   `amazon.nova-pro-v1:0`
//!   `meta.llama3-8b-instruct-v1:0`
//!
//! The Anthropic Bedrock wire format uses `/v1/messages` (same shape as the
//! Anthropic API). Other providers use the InvokeModel shape.  This provider
//! normalises everything into the Anvil `MessageResponse` type.

use std::collections::VecDeque;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use hmac::{Hmac, Mac};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::error::ApiError;
use crate::types::{
    ContentBlockDelta, ContentBlockDeltaEvent, ContentBlockStartEvent, ContentBlockStopEvent,
    MessageDelta, MessageDeltaEvent, MessageRequest, MessageResponse,
    MessageStartEvent, MessageStopEvent, OutputContentBlock, StreamEvent, Usage,
};
use super::common::{next_sse_frame, extract_sse_data};
use super::openai_compat::resolve_stream_dead_air_timeout;
use super::{Provider, ProviderFuture};

type HmacSha256 = Hmac<Sha256>;

const SERVICE: &str = "bedrock";
const SIGNING_ALGORITHM: &str = "AWS4-HMAC-SHA256";

// ---------------------------------------------------------------------------
// Credentials
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct AwsCredentials {
    pub access_key_id: String,
    pub secret_access_key: String,
    pub session_token: Option<String>,
    pub region: String,
}

impl AwsCredentials {
    fn from_env() -> Result<Self, ApiError> {
        let access_key_id = std::env::var("AWS_ACCESS_KEY_ID")
            .ok()
            .filter(|v| !v.is_empty())
            .ok_or_else(|| ApiError::missing_credentials(
                "AWS Bedrock",
                &["AWS_ACCESS_KEY_ID", "AWS_SECRET_ACCESS_KEY", "AWS_REGION"],
            ))?;
        let secret_access_key = std::env::var("AWS_SECRET_ACCESS_KEY")
            .ok()
            .filter(|v| !v.is_empty())
            .ok_or_else(|| ApiError::missing_credentials(
                "AWS Bedrock",
                &["AWS_ACCESS_KEY_ID", "AWS_SECRET_ACCESS_KEY", "AWS_REGION"],
            ))?;
        let region = std::env::var("AWS_REGION")
            .or_else(|_| std::env::var("AWS_DEFAULT_REGION"))
            .ok()
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| "us-east-1".to_string());
        let session_token = std::env::var("AWS_SESSION_TOKEN")
            .ok()
            .filter(|v| !v.is_empty());
        Ok(Self { access_key_id, secret_access_key, session_token, region })
    }
}

// ---------------------------------------------------------------------------
// SigV4 signing
// ---------------------------------------------------------------------------

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn sha256_hex(data: &[u8]) -> String {
    hex_encode(&Sha256::digest(data))
}

fn hmac_sha256(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut mac = HmacSha256::new_from_slice(key)
        .expect("HMAC can accept any key length");
    mac.update(data);
    mac.finalize().into_bytes().to_vec()
}

fn derive_signing_key(secret: &str, date: &str, region: &str, service: &str) -> Vec<u8> {
    let k_date = hmac_sha256(format!("AWS4{secret}").as_bytes(), date.as_bytes());
    let k_region = hmac_sha256(&k_date, region.as_bytes());
    let k_service = hmac_sha256(&k_region, service.as_bytes());
    hmac_sha256(&k_service, b"aws4_request")
}

struct SigV4Headers {
    authorization: String,
    x_amz_date: String,
    x_amz_security_token: Option<String>,
    x_amz_content_sha256: String,
}

fn sign_request(
    creds: &AwsCredentials,
    method: &str,
    url: &str,
    payload: &[u8],
) -> SigV4Headers {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs();
    // Format: YYYYMMDDTHHMMSSZ
    let dt = {
        let total_seconds = secs;
        let s = total_seconds % 60;
        let m = (total_seconds / 60) % 60;
        let h = (total_seconds / 3600) % 24;
        let days = total_seconds / 86400;
        // Simplified: use days-since-epoch to compute date (good enough for signing)
        let (y, mo, d) = days_to_ymd(days);
        format!("{y:04}{mo:02}{d:02}T{h:02}{m:02}{s:02}Z")
    };
    let date = &dt[..8]; // YYYYMMDD

    let payload_hash = sha256_hex(payload);

    // Parse URL for host and path+query
    let (host, path_and_query) = parse_url_for_signing(url);

    // Canonical headers (sorted, lowercase)
    let mut canonical_headers = format!("content-type:application/json\nhost:{host}\nx-amz-content-sha256:{payload_hash}\nx-amz-date:{dt}\n");
    let mut signed_headers = "content-type;host;x-amz-content-sha256;x-amz-date".to_string();

    if let Some(token) = &creds.session_token {
        canonical_headers.push_str(&format!("x-amz-security-token:{token}\n"));
        signed_headers.push_str(";x-amz-security-token");
    }

    let canonical_request = format!(
        "{method}\n{path_and_query}\n\n{canonical_headers}\n{signed_headers}\n{payload_hash}"
    );

    let credential_scope = format!("{date}/{}/{SERVICE}/aws4_request", creds.region);
    let string_to_sign = format!(
        "{SIGNING_ALGORITHM}\n{dt}\n{credential_scope}\n{}",
        sha256_hex(canonical_request.as_bytes())
    );

    let signing_key = derive_signing_key(&creds.secret_access_key, date, &creds.region, SERVICE);
    let signature = hex_encode(&hmac_sha256(&signing_key, string_to_sign.as_bytes()));

    let authorization = format!(
        "{SIGNING_ALGORITHM} Credential={}/{credential_scope}, SignedHeaders={signed_headers}, Signature={signature}",
        creds.access_key_id
    );

    SigV4Headers {
        authorization,
        x_amz_date: dt,
        x_amz_security_token: creds.session_token.clone(),
        x_amz_content_sha256: payload_hash,
    }
}

fn parse_url_for_signing(url: &str) -> (String, String) {
    // Strip scheme
    let after_scheme = url
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(url);
    let (authority, path_query) = after_scheme
        .split_once('/')
        .map(|(a, p)| (a.to_string(), format!("/{p}")))
        .unwrap_or_else(|| (after_scheme.to_string(), "/".to_string()));
    // Strip port from host for canonical headers
    let host = authority.split(':').next().unwrap_or(&authority).to_string();
    (host, path_query)
}

/// Minimal Gregorian calendar computation from days-since-epoch (Jan 1 1970).
/// Used only for SigV4 date strings; accuracy within this century is sufficient.
fn days_to_ymd(days: u64) -> (u32, u32, u32) {
    // 400-year cycle = 146097 days
    let z = days + 719468;
    let era = z / 146097;
    let doe = z % 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y as u32, m as u32, d as u32)
}

// ---------------------------------------------------------------------------
// Public signing helper for GET requests (used by model_list)
// ---------------------------------------------------------------------------

/// SigV4 headers result — re-exported so `model_list` can sign GET requests.
pub struct SigV4GetHeaders {
    pub authorization: String,
    pub x_amz_date: String,
    pub x_amz_security_token: Option<String>,
    pub x_amz_content_sha256: String,
}

/// Sign a GET request (empty body) for a given URL.
///
/// Called by the model-list fetcher which needs to call `GET /foundation-models`
/// but doesn't want to instantiate a full `BedrockClient` just for signing.
pub fn sign_request_get(creds: &AwsCredentials, method: &str, url: &str) -> SigV4GetHeaders {
    let sig = sign_request(creds, method, url, b"");
    SigV4GetHeaders {
        authorization: sig.authorization,
        x_amz_date: sig.x_amz_date,
        x_amz_security_token: sig.x_amz_security_token,
        x_amz_content_sha256: sig.x_amz_content_sha256,
    }
}

// ---------------------------------------------------------------------------
// BedrockClient
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct BedrockClient {
    http: reqwest::Client,
    creds: AwsCredentials,
    endpoint_base: String,
}

impl BedrockClient {
    /// Expose credentials for external signing helpers (e.g. model-list GET).
    #[must_use]
    pub const fn credentials(&self) -> &AwsCredentials {
        &self.creds
    }

    pub fn from_env() -> Result<Self, ApiError> {
        let creds = AwsCredentials::from_env()?;
        let endpoint_base = std::env::var("AWS_BEDROCK_ENDPOINT")
            .ok()
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| {
                format!("https://bedrock-runtime.{}.amazonaws.com", creds.region)
            });
        Ok(Self {
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(600))
                .build()
                .unwrap_or_default(),
            creds,
            endpoint_base,
        })
    }

    fn invoke_url(&self, model_id: &str, stream: bool) -> String {
        let action = if stream {
            "invoke-with-response-stream"
        } else {
            "invoke"
        };
        format!(
            "{}/model/{}/{}",
            self.endpoint_base.trim_end_matches('/'),
            percent_encode_model_id(model_id),
            action
        )
    }

    fn build_bedrock_payload(request: &MessageRequest) -> Vec<u8> {
        // Bedrock Anthropic models use the Messages API shape.
        // Other models (Nova, Llama, Titan) use a different shape;
        // we normalise to OpenAI-compat for non-Anthropic models.
        let model = &request.model;
        if model.starts_with("anthropic.") {
            let messages: Vec<Value> = request
                .messages
                .iter()
                .map(|m| {
                    let text = m
                        .content
                        .iter()
                        .filter_map(|b| match b {
                            crate::types::InputContentBlock::Text { text } => {
                                Some(text.clone())
                            }
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                    json!({ "role": m.role, "content": text })
                })
                .collect();
            let mut payload = json!({
                "anthropic_version": "bedrock-2023-05-31",
                "max_tokens": request.max_tokens,
                "messages": messages,
            });
            if let Some(system) = &request.system {
                payload["system"] = json!(system);
            }
            serde_json::to_vec(&payload).unwrap_or_default()
        } else {
            // Generic Bedrock converse shape (Amazon Titan, Nova, Meta Llama, etc.)
            let text = request
                .messages
                .last()
                .and_then(|m| {
                    m.content.iter().find_map(|b| match b {
                        crate::types::InputContentBlock::Text { text } => Some(text.clone()),
                        _ => None,
                    })
                })
                .unwrap_or_default();
            let payload = json!({
                "inputText": text,
                "textGenerationConfig": {
                    "maxTokenCount": request.max_tokens,
                    "temperature": 0.7,
                }
            });
            serde_json::to_vec(&payload).unwrap_or_default()
        }
    }

    fn apply_sigv4_headers(
        &self,
        builder: reqwest::RequestBuilder,
        url: &str,
        payload: &[u8],
    ) -> reqwest::RequestBuilder {
        let sig = sign_request(&self.creds, "POST", url, payload);
        let mut b = builder
            .header("Authorization", sig.authorization)
            .header("x-amz-date", sig.x_amz_date)
            .header("x-amz-content-sha256", sig.x_amz_content_sha256);
        if let Some(token) = sig.x_amz_security_token {
            b = b.header("x-amz-security-token", token);
        }
        b
    }

    pub async fn send_message(
        &self,
        request: &MessageRequest,
    ) -> Result<MessageResponse, ApiError> {
        let url = self.invoke_url(&request.model, false);
        let payload = Self::build_bedrock_payload(request);

        let builder = self
            .http
            .post(&url)
            .header("content-type", "application/json")
            .body(payload.clone());
        let builder = self.apply_sigv4_headers(builder, &url, &payload);

        let response = builder.send().await.map_err(ApiError::Http)?;
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

        let raw: Value = response.json().await.map_err(ApiError::Http)?;
        normalize_bedrock_response(&request.model, raw)
    }

    pub async fn stream_message(
        &self,
        request: &MessageRequest,
    ) -> Result<BedrockMessageStream, ApiError> {
        let url = self.invoke_url(&request.model, true);
        let payload = Self::build_bedrock_payload(request);

        let builder = self
            .http
            .post(&url)
            .header("content-type", "application/json")
            .body(payload.clone());
        let builder = self.apply_sigv4_headers(builder, &url, &payload);

        let response = builder.send().await.map_err(ApiError::Http)?;
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

        Ok(BedrockMessageStream {
            response,
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

impl Provider for BedrockClient {
    type Stream = BedrockMessageStream;

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

fn normalize_bedrock_response(model: &str, raw: Value) -> Result<MessageResponse, ApiError> {
    let text = if model.starts_with("anthropic.") {
        // Anthropic Bedrock: `content[0].text`
        raw["content"]
            .as_array()
            .and_then(|arr| arr.first())
            .and_then(|b| b["text"].as_str())
            .unwrap_or("")
            .to_string()
    } else {
        // Generic: `results[0].outputText` (Titan) or `generation` (Meta)
        raw["results"]
            .as_array()
            .and_then(|arr| arr.first())
            .and_then(|r| r["outputText"].as_str())
            .or_else(|| raw["generation"].as_str())
            .unwrap_or("")
            .to_string()
    };

    let stop_reason = raw["stop_reason"]
        .as_str()
        .or_else(|| raw["completionReason"].as_str())
        .map(|r| if r == "end_turn" { r } else { r })
        .map(ToOwned::to_owned);

    let input_tokens = raw["usage"]["input_tokens"].as_u64().unwrap_or(0) as u32;
    let output_tokens = raw["usage"]["output_tokens"].as_u64().unwrap_or(0) as u32;

    Ok(MessageResponse {
        id: format!("bedrock-{}", model),
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
// Streaming
// ---------------------------------------------------------------------------

/// Bedrock's streaming format for `InvokeModelWithResponseStream` uses an
/// event-stream binary protocol.  The HTTP response body is a sequence of
/// frames, each containing a base64-encoded JSON payload.  For Anthropic
/// models the event data is the standard Anthropic SSE event JSON.
///
/// We decode the event-stream frames and normalise to `StreamEvent`.
#[derive(Debug)]
pub struct BedrockMessageStream {
    response: reqwest::Response,
    pending: VecDeque<StreamEvent>,
    done: bool,
    model: String,
    message_started: bool,
    text_started: bool,
    last_chunk_at: Instant,
    dead_air_timeout: Duration,
}

impl BedrockMessageStream {
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
                    self.decode_event_stream_frame(&chunk);
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

    /// Bedrock event-stream frame: 4-byte total-length, 4-byte headers-length,
    /// 4-byte CRC, variable headers, payload bytes, 4-byte message CRC.
    /// We extract the payload JSON and decode the `bytes` field (base64).
    fn decode_event_stream_frame(&mut self, data: &[u8]) {
        if data.len() < 16 {
            return;
        }
        // Parse total-length and headers-length from first 8 bytes.
        let total_len = u32::from_be_bytes([data[0], data[1], data[2], data[3]]) as usize;
        let headers_len = u32::from_be_bytes([data[4], data[5], data[6], data[7]]) as usize;
        let payload_start = 12 + headers_len; // 4 prelude + 4 prelude-CRC + headers
        let payload_end = total_len.saturating_sub(4); // minus trailing message-CRC

        if payload_end <= payload_start || payload_end > data.len() {
            return;
        }

        let payload_bytes = &data[payload_start..payload_end];
        let Ok(payload_str) = std::str::from_utf8(payload_bytes) else {
            return;
        };

        // The payload is a JSON object with a `bytes` field (base64-encoded event JSON).
        if let Ok(outer) = serde_json::from_str::<Value>(payload_str) {
            if let Some(b64) = outer["bytes"].as_str() {
                if let Ok(decoded) = base64_decode(b64) {
                    if let Ok(inner) = serde_json::from_slice::<Value>(&decoded) {
                        self.ingest_anthropic_event(inner);
                        return;
                    }
                }
            }
            // Fallback: treat the outer payload as the event directly.
            self.ingest_anthropic_event(outer);
        }
    }

    fn ingest_anthropic_event(&mut self, v: Value) {
        let event_type = v["type"].as_str().unwrap_or("");
        match event_type {
            "message_start" => {
                if !self.message_started {
                    self.message_started = true;
                    self.pending.push_back(StreamEvent::MessageStart(MessageStartEvent {
                        message: MessageResponse {
                            id: v["message"]["id"]
                                .as_str()
                                .unwrap_or("")
                                .to_string(),
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
            "content_block_start" => {
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
            }
            "content_block_delta" => {
                if let Some(text) = v["delta"]["text"].as_str() {
                    if !text.is_empty() {
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
            }
            "content_block_stop" => {
                if self.text_started {
                    self.pending.push_back(StreamEvent::ContentBlockStop(
                        ContentBlockStopEvent { index: 0 },
                    ));
                }
            }
            "message_delta" => {
                let stop_reason = v["delta"]["stop_reason"]
                    .as_str()
                    .unwrap_or("end_turn")
                    .to_string();
                let input_tokens = v["usage"]["input_tokens"].as_u64().unwrap_or(0) as u32;
                let output_tokens = v["usage"]["output_tokens"].as_u64().unwrap_or(0) as u32;
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
            }
            "message_stop" => {
                self.pending.push_back(StreamEvent::MessageStop(MessageStopEvent {}));
                self.done = true;
            }
            _ => {}
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
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn percent_encode_model_id(model_id: &str) -> String {
    model_id.replace(':', "%3A").replace('/', "%2F")
}

fn base64_decode(input: &str) -> Result<Vec<u8>, ()> {
    const TABLE: &[u8; 64] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut lookup = [255u8; 256];
    for (i, &b) in TABLE.iter().enumerate() {
        lookup[b as usize] = i as u8;
    }
    let clean: Vec<u8> = input
        .bytes()
        .filter(|&b| b != b'=' && !b.is_ascii_whitespace())
        .collect();
    let mut out = Vec::with_capacity(clean.len() * 3 / 4);
    let mut i = 0;
    while i + 3 < clean.len() {
        let a = lookup[clean[i] as usize];
        let b = lookup[clean[i + 1] as usize];
        let c = lookup[clean[i + 2] as usize];
        let d = lookup[clean[i + 3] as usize];
        if a == 255 || b == 255 {
            return Err(());
        }
        out.push((a << 2) | (b >> 4));
        if c != 255 {
            out.push((b << 4) | (c >> 2));
        }
        if d != 255 {
            out.push((c << 6) | d);
        }
        i += 4;
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invoke_url_non_streaming() {
        // Edition 2024: env::set_var requires unsafe.
        #![allow(unsafe_code)]
        unsafe {
            std::env::set_var("AWS_ACCESS_KEY_ID", "AKIATEST");
            std::env::set_var("AWS_SECRET_ACCESS_KEY", "secret");
            std::env::set_var("AWS_REGION", "us-east-1");
        }
        let client = BedrockClient::from_env().expect("from_env");
        let url = client.invoke_url("anthropic.claude-3-5-sonnet-20241022-v2:0", false);
        assert!(
            url.contains("anthropic.claude-3-5-sonnet-20241022-v2%3A0"),
            "model ID colon must be percent-encoded"
        );
        assert!(url.ends_with("/invoke"), "non-streaming URL must end with /invoke");
        unsafe {
            std::env::remove_var("AWS_ACCESS_KEY_ID");
            std::env::remove_var("AWS_SECRET_ACCESS_KEY");
            std::env::remove_var("AWS_REGION");
        }
    }

    #[test]
    fn invoke_url_streaming() {
        #![allow(unsafe_code)]
        unsafe {
            std::env::set_var("AWS_ACCESS_KEY_ID", "AKIATEST");
            std::env::set_var("AWS_SECRET_ACCESS_KEY", "secret");
            std::env::set_var("AWS_REGION", "us-west-2");
        }
        let client = BedrockClient::from_env().expect("from_env");
        let url = client.invoke_url("amazon.nova-pro-v1:0", true);
        assert!(url.ends_with("/invoke-with-response-stream"));
        unsafe {
            std::env::remove_var("AWS_ACCESS_KEY_ID");
            std::env::remove_var("AWS_SECRET_ACCESS_KEY");
            std::env::remove_var("AWS_REGION");
        }
    }

    #[test]
    fn sigv4_produces_non_empty_authorization() {
        let creds = AwsCredentials {
            access_key_id: "AKIAIOSFODNN7EXAMPLE".to_string(),
            secret_access_key: "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY".to_string(),
            session_token: None,
            region: "us-east-1".to_string(),
        };
        let sig = sign_request(
            &creds,
            "POST",
            "https://bedrock-runtime.us-east-1.amazonaws.com/model/anthropic.claude-3-5-sonnet-20241022-v2%3A0/invoke",
            b"{}",
        );
        assert!(sig.authorization.starts_with(SIGNING_ALGORITHM));
        assert!(sig.authorization.contains("Credential=AKIAIOSFODNN7EXAMPLE/"));
        assert!(sig.authorization.contains("Signature="));
        assert!(!sig.x_amz_date.is_empty());
    }

    #[test]
    fn days_to_ymd_known_epoch() {
        // Day 0 = 1970-01-01
        assert_eq!(days_to_ymd(0), (1970, 1, 1));
        // Day 365 = 1971-01-01 (1970 is not a leap year)
        assert_eq!(days_to_ymd(365), (1971, 1, 1));
    }

    #[test]
    fn base64_decode_round_trip() {
        let original = b"Hello, Bedrock!";
        // Simple manual base64 encode for test
        let encoded = base64_encode_simple(original);
        let decoded = base64_decode(&encoded).expect("decode");
        assert_eq!(decoded, original);
    }

    fn base64_encode_simple(input: &[u8]) -> String {
        const TABLE: &[u8; 64] =
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::new();
        let mut i = 0;
        while i < input.len() {
            let b0 = input[i];
            let b1 = input.get(i + 1).copied().unwrap_or(0);
            let b2 = input.get(i + 2).copied().unwrap_or(0);
            out.push(TABLE[(b0 >> 2) as usize] as char);
            out.push(TABLE[((b0 & 3) << 4 | b1 >> 4) as usize] as char);
            if i + 1 < input.len() {
                out.push(TABLE[((b1 & 0xF) << 2 | b2 >> 6) as usize] as char);
            }
            if i + 2 < input.len() {
                out.push(TABLE[(b2 & 0x3F) as usize] as char);
            }
            i += 3;
        }
        out
    }

    #[test]
    fn from_env_errors_without_credentials() {
        #![allow(unsafe_code)]
        unsafe {
            std::env::remove_var("AWS_ACCESS_KEY_ID");
            std::env::remove_var("AWS_SECRET_ACCESS_KEY");
        }
        let result = BedrockClient::from_env();
        assert!(result.is_err(), "must error when AWS credentials are absent");
    }
}
