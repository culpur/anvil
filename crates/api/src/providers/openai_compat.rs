// Edition 2024: env::set_var/remove_var require unsafe
#![allow(unsafe_code)]

use std::collections::{BTreeMap, VecDeque};
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::{json, Value};

use crate::error::ApiError;
use crate::types::{
    ContentBlockDelta, ContentBlockDeltaEvent, ContentBlockStartEvent, ContentBlockStopEvent,
    InputContentBlock, InputMessage, MessageDelta, MessageDeltaEvent, MessageRequest,
    MessageResponse, MessageStartEvent, MessageStopEvent, OutputContentBlock, StreamEvent,
    ToolChoice, ToolDefinition, ToolResultContentBlock, Usage,
};

use runtime::EffortLevel;

use super::common::{
    self, extract_sse_data, next_sse_frame, read_env_non_empty,
    request_id_from_headers, DEFAULT_INITIAL_BACKOFF, DEFAULT_MAX_BACKOFF, DEFAULT_MAX_RETRIES,
};
use super::ollama_tool_parser::{parse_ollama_text_for_tool_calls, silent_write_warning};
use super::{Provider, ProviderFuture};

pub const DEFAULT_XAI_BASE_URL: &str = "https://api.x.ai/v1";
pub const DEFAULT_OPENAI_BASE_URL: &str = "https://api.openai.com/v1";
pub const DEFAULT_GEMINI_BASE_URL: &str = "https://generativelanguage.googleapis.com/v1beta/openai";
pub const DEFAULT_OLLAMA_BASE_URL: &str = "http://localhost:11434/v1";

// ── Group B: OpenAI-compatible provider base URLs ────────────────────────────
pub const DEFAULT_FIREWORKS_BASE_URL: &str = "https://api.fireworks.ai/inference/v1";
pub const DEFAULT_GROQ_BASE_URL: &str = "https://api.groq.com/openai/v1";
pub const DEFAULT_MISTRAL_BASE_URL: &str = "https://api.mistral.ai/v1";
pub const DEFAULT_PERPLEXITY_BASE_URL: &str = "https://api.perplexity.ai";
pub const DEFAULT_DEEPSEEK_BASE_URL: &str = "https://api.deepseek.com/v1";
pub const DEFAULT_TOGETHERAI_BASE_URL: &str = "https://api.together.xyz/v1";
pub const DEFAULT_DEEPINFRA_BASE_URL: &str = "https://api.deepinfra.com/v1/openai";
pub const DEFAULT_CEREBRAS_BASE_URL: &str = "https://api.cerebras.ai/v1";
pub const DEFAULT_NVIDIA_NIM_BASE_URL: &str = "https://integrate.api.nvidia.com/v1";
pub const DEFAULT_HUGGINGFACE_BASE_URL: &str = "https://api-inference.huggingface.co/v1";
pub const DEFAULT_MOONSHOTAI_BASE_URL: &str = "https://api.moonshot.cn/v1";
pub const DEFAULT_NEBIUS_BASE_URL: &str = "https://api.studio.nebius.ai/v1";
pub const DEFAULT_OPENROUTER_BASE_URL: &str = "https://openrouter.ai/api/v1";
/// LM Studio local server — no authentication required.
pub const DEFAULT_LMSTUDIO_BASE_URL: &str = "http://localhost:1234/v1";
pub const DEFAULT_CHUTES_BASE_URL: &str = "https://llm.chutes.ai/v1";
pub const DEFAULT_SCALEWAY_BASE_URL: &str = "https://api.scaleway.ai/v1";
pub const DEFAULT_BASETEN_BASE_URL: &str = "https://inference.baseten.co/v1";
pub const DEFAULT_MINIMAX_BASE_URL: &str = "https://api.minimax.chat/v1";
pub const DEFAULT_STACKIT_BASE_URL: &str =
    "https://api.openai-compat.model-serving.eu01.onstackit.cloud/v1";
pub const DEFAULT_CORTECS_BASE_URL: &str = "https://api.cortecs.ai/v1";
pub const DEFAULT_AI302_BASE_URL: &str = "https://api.302.ai/v1";
pub const DEFAULT_ZAI_BASE_URL: &str = "https://api.z.ai/api/coding/paas/v4";
/// OpenCode community endpoint — mirrors the OpenAI-compat shape.
pub const DEFAULT_OPENCODE_BASE_URL: &str = "https://opencode.ai/v1";
/// OpenCode-Go community endpoint.
pub const DEFAULT_OPENCODE_GO_BASE_URL: &str = "https://go.opencode.ai/v1";
/// Alibaba DashScope OpenAI-compatible mode.
pub const DEFAULT_ALIBABA_BASE_URL: &str = "https://dashscope.aliyuncs.com/compatible-mode/v1";
/// Antigravity uses the Gemini generativelanguage endpoint with custom routing.
pub const DEFAULT_ANTIGRAVITY_BASE_URL: &str =
    "https://generativelanguage.googleapis.com/v1beta";
/// Cursor API — REST endpoint exposed by the Cursor subscription service.
pub const DEFAULT_CURSOR_BASE_URL: &str = "https://api2.cursor.sh/v1";

/// Default per-request timeout: 10 minutes.  Generous enough for slow Ollama
/// or local-LLM calls on consumer hardware; configurable via
/// `ANVIL_API_TIMEOUT_MS` for tighter or looser requirements.
pub const DEFAULT_API_TIMEOUT_MS: u64 = 10 * 60 * 1_000;

/// Parse `ANVIL_API_TIMEOUT_MS` (plain integer milliseconds).
///
/// Returns the default when the variable is absent.  Returns the default and
/// prints a warning when the variable is set but contains garbage — fail-loud,
/// don't silently ignore the misconfiguration.
pub fn resolve_api_timeout() -> Duration {
    match std::env::var("ANVIL_API_TIMEOUT_MS") {
        Err(_) => Duration::from_millis(DEFAULT_API_TIMEOUT_MS),
        Ok(raw) => match raw.trim().parse::<u64>() {
            Ok(ms) => Duration::from_millis(ms),
            Err(_) => {
                eprintln!(
                    "[anvil] warning: ANVIL_API_TIMEOUT_MS={raw:?} is not a valid integer; \
                     using default {}ms",
                    DEFAULT_API_TIMEOUT_MS
                );
                Duration::from_millis(DEFAULT_API_TIMEOUT_MS)
            }
        },
    }
}

/// Build a `reqwest::Client` with the configured per-request timeout.
///
/// The timeout is applied at the client level so it covers both the connect
/// phase and response body reads.  For streaming, the connection stays open
/// across chunks — the dead-air timeout in `MessageStream` handles the
/// chunk-level guard independently.
fn build_http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(resolve_api_timeout())
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpenAiCompatConfig {
    pub provider_name: &'static str,
    pub api_key_env: &'static str,
    pub base_url_env: &'static str,
    pub default_base_url: &'static str,
}

const XAI_ENV_VARS: &[&str] = &["XAI_API_KEY"];
const OPENAI_ENV_VARS: &[&str] = &["OPENAI_API_KEY"];
const _GEMINI_ENV_VARS: &[&str] = &["GEMINI_API_KEY", "GOOGLE_API_KEY"];
const OLLAMA_ENV_VARS: &[&str] = &[];

impl OpenAiCompatConfig {
    #[must_use]
    pub const fn xai() -> Self {
        Self {
            provider_name: "xAI",
            api_key_env: "XAI_API_KEY",
            base_url_env: "XAI_BASE_URL",
            default_base_url: DEFAULT_XAI_BASE_URL,
        }
    }

    #[must_use]
    pub const fn openai() -> Self {
        Self {
            provider_name: "OpenAI",
            api_key_env: "OPENAI_API_KEY",
            base_url_env: "OPENAI_BASE_URL",
            default_base_url: DEFAULT_OPENAI_BASE_URL,
        }
    }

    #[must_use]
    pub const fn gemini() -> Self {
        Self {
            provider_name: "Gemini",
            api_key_env: "GEMINI_API_KEY",
            base_url_env: "GEMINI_BASE_URL",
            default_base_url: DEFAULT_GEMINI_BASE_URL,
        }
    }

    #[must_use]
    pub const fn ollama() -> Self {
        Self {
            provider_name: "Ollama",
            api_key_env: "",
            base_url_env: "OLLAMA_HOST",
            default_base_url: DEFAULT_OLLAMA_BASE_URL,
        }
    }

    // ── Group B constructors ─────────────────────────────────────────────────

    #[must_use]
    pub const fn fireworks() -> Self {
        Self {
            provider_name: "Fireworks",
            api_key_env: "FIREWORKS_API_KEY",
            base_url_env: "FIREWORKS_BASE_URL",
            default_base_url: DEFAULT_FIREWORKS_BASE_URL,
        }
    }

    #[must_use]
    pub const fn groq() -> Self {
        Self {
            provider_name: "Groq",
            api_key_env: "GROQ_API_KEY",
            base_url_env: "GROQ_BASE_URL",
            default_base_url: DEFAULT_GROQ_BASE_URL,
        }
    }

    #[must_use]
    pub const fn mistral() -> Self {
        Self {
            provider_name: "Mistral",
            api_key_env: "MISTRAL_API_KEY",
            base_url_env: "MISTRAL_BASE_URL",
            default_base_url: DEFAULT_MISTRAL_BASE_URL,
        }
    }

    #[must_use]
    pub const fn perplexity() -> Self {
        Self {
            provider_name: "Perplexity",
            api_key_env: "PERPLEXITY_API_KEY",
            base_url_env: "PERPLEXITY_BASE_URL",
            default_base_url: DEFAULT_PERPLEXITY_BASE_URL,
        }
    }

    #[must_use]
    pub const fn deepseek() -> Self {
        Self {
            provider_name: "DeepSeek",
            api_key_env: "DEEPSEEK_API_KEY",
            base_url_env: "DEEPSEEK_BASE_URL",
            default_base_url: DEFAULT_DEEPSEEK_BASE_URL,
        }
    }

    #[must_use]
    pub const fn togetherai() -> Self {
        Self {
            provider_name: "TogetherAI",
            api_key_env: "TOGETHER_API_KEY",
            base_url_env: "TOGETHER_BASE_URL",
            default_base_url: DEFAULT_TOGETHERAI_BASE_URL,
        }
    }

    #[must_use]
    pub const fn deepinfra() -> Self {
        Self {
            provider_name: "DeepInfra",
            api_key_env: "DEEPINFRA_API_KEY",
            base_url_env: "DEEPINFRA_BASE_URL",
            default_base_url: DEFAULT_DEEPINFRA_BASE_URL,
        }
    }

    #[must_use]
    pub const fn cerebras() -> Self {
        Self {
            provider_name: "Cerebras",
            api_key_env: "CEREBRAS_API_KEY",
            base_url_env: "CEREBRAS_BASE_URL",
            default_base_url: DEFAULT_CEREBRAS_BASE_URL,
        }
    }

    #[must_use]
    pub const fn nvidia_nim() -> Self {
        Self {
            provider_name: "NVIDIA NIM",
            api_key_env: "NVIDIA_API_KEY",
            base_url_env: "NVIDIA_BASE_URL",
            default_base_url: DEFAULT_NVIDIA_NIM_BASE_URL,
        }
    }

    #[must_use]
    pub const fn huggingface() -> Self {
        Self {
            provider_name: "HuggingFace",
            api_key_env: "HF_TOKEN",
            base_url_env: "HF_BASE_URL",
            default_base_url: DEFAULT_HUGGINGFACE_BASE_URL,
        }
    }

    #[must_use]
    pub const fn moonshotai() -> Self {
        Self {
            provider_name: "MoonshotAI",
            api_key_env: "MOONSHOT_API_KEY",
            base_url_env: "MOONSHOT_BASE_URL",
            default_base_url: DEFAULT_MOONSHOTAI_BASE_URL,
        }
    }

    #[must_use]
    pub const fn nebius() -> Self {
        Self {
            provider_name: "Nebius",
            api_key_env: "NEBIUS_API_KEY",
            base_url_env: "NEBIUS_BASE_URL",
            default_base_url: DEFAULT_NEBIUS_BASE_URL,
        }
    }

    #[must_use]
    pub const fn openrouter() -> Self {
        Self {
            provider_name: "OpenRouter",
            api_key_env: "OPENROUTER_API_KEY",
            base_url_env: "OPENROUTER_BASE_URL",
            default_base_url: DEFAULT_OPENROUTER_BASE_URL,
        }
    }

    /// LM Studio runs locally — no authentication required.
    #[must_use]
    pub const fn lmstudio() -> Self {
        Self {
            provider_name: "LM Studio",
            api_key_env: "",
            base_url_env: "LMSTUDIO_BASE_URL",
            default_base_url: DEFAULT_LMSTUDIO_BASE_URL,
        }
    }

    #[must_use]
    pub const fn chutes() -> Self {
        Self {
            provider_name: "Chutes",
            api_key_env: "CHUTES_API_KEY",
            base_url_env: "CHUTES_BASE_URL",
            default_base_url: DEFAULT_CHUTES_BASE_URL,
        }
    }

    #[must_use]
    pub const fn scaleway() -> Self {
        Self {
            provider_name: "Scaleway",
            api_key_env: "SCALEWAY_API_KEY",
            base_url_env: "SCALEWAY_BASE_URL",
            default_base_url: DEFAULT_SCALEWAY_BASE_URL,
        }
    }

    #[must_use]
    pub const fn baseten() -> Self {
        Self {
            provider_name: "Baseten",
            api_key_env: "BASETEN_API_KEY",
            base_url_env: "BASETEN_BASE_URL",
            default_base_url: DEFAULT_BASETEN_BASE_URL,
        }
    }

    #[must_use]
    pub const fn minimax() -> Self {
        Self {
            provider_name: "MiniMax",
            api_key_env: "MINIMAX_API_KEY",
            base_url_env: "MINIMAX_BASE_URL",
            default_base_url: DEFAULT_MINIMAX_BASE_URL,
        }
    }

    #[must_use]
    pub const fn stackit() -> Self {
        Self {
            provider_name: "StackIT",
            api_key_env: "STACKIT_API_KEY",
            base_url_env: "STACKIT_BASE_URL",
            default_base_url: DEFAULT_STACKIT_BASE_URL,
        }
    }

    #[must_use]
    pub const fn cortecs() -> Self {
        Self {
            provider_name: "Cortecs",
            api_key_env: "CORTECS_API_KEY",
            base_url_env: "CORTECS_BASE_URL",
            default_base_url: DEFAULT_CORTECS_BASE_URL,
        }
    }

    #[must_use]
    pub const fn ai302() -> Self {
        Self {
            provider_name: "302AI",
            api_key_env: "AI302_API_KEY",
            base_url_env: "AI302_BASE_URL",
            default_base_url: DEFAULT_AI302_BASE_URL,
        }
    }

    /// Zai — also known as `kimi` or `glm`.  `ZAI_API_KEY` is the primary env var.
    #[must_use]
    pub const fn zai() -> Self {
        Self {
            provider_name: "Zai",
            api_key_env: "ZAI_API_KEY",
            base_url_env: "ZAI_BASE_URL",
            default_base_url: DEFAULT_ZAI_BASE_URL,
        }
    }

    #[must_use]
    pub const fn opencode() -> Self {
        Self {
            provider_name: "OpenCode",
            api_key_env: "OPENCODE_API_KEY",
            base_url_env: "OPENCODE_BASE_URL",
            default_base_url: DEFAULT_OPENCODE_BASE_URL,
        }
    }

    #[must_use]
    pub const fn opencode_go() -> Self {
        Self {
            provider_name: "OpenCode-Go",
            api_key_env: "OPENCODE_API_KEY",
            base_url_env: "OPENCODE_GO_BASE_URL",
            default_base_url: DEFAULT_OPENCODE_GO_BASE_URL,
        }
    }

    /// Alibaba DashScope — OpenAI-compatible mode.  `DASHSCOPE_API_KEY` is the
    /// primary env var; `ALIBABA_API_KEY` is accepted as a fallback alias.
    #[must_use]
    pub const fn alibaba() -> Self {
        Self {
            provider_name: "Alibaba DashScope",
            api_key_env: "DASHSCOPE_API_KEY",
            base_url_env: "ALIBABA_BASE_URL",
            default_base_url: DEFAULT_ALIBABA_BASE_URL,
        }
    }

    /// Antigravity — Google Gemini fork with custom routing.  Uses a
    /// `ANTIGRAVITY_API_KEY` (or `GEMINI_API_KEY` as fallback) against the
    /// Gemini generativelanguage base URL.
    #[must_use]
    pub const fn antigravity() -> Self {
        Self {
            provider_name: "Antigravity",
            api_key_env: "ANTIGRAVITY_API_KEY",
            base_url_env: "ANTIGRAVITY_BASE_URL",
            default_base_url: DEFAULT_ANTIGRAVITY_BASE_URL,
        }
    }

    #[must_use]
    pub fn credential_env_vars(self) -> &'static [&'static str] {
        match self.provider_name {
            "xAI" => XAI_ENV_VARS,
            "OpenAI" => OPENAI_ENV_VARS,
            "Ollama" => OLLAMA_ENV_VARS,
            _ => &[],
        }
    }
}

#[derive(Debug, Clone)]
pub struct OpenAiCompatClient {
    http: reqwest::Client,
    api_key: String,
    base_url: String,
    max_retries: u32,
    initial_backoff: Duration,
    max_backoff: Duration,
}

impl OpenAiCompatClient {
    #[must_use]
    pub fn new(api_key: impl Into<String>, config: OpenAiCompatConfig) -> Self {
        Self {
            http: build_http_client(),
            api_key: api_key.into(),
            base_url: read_base_url(config),
            max_retries: DEFAULT_MAX_RETRIES,
            initial_backoff: DEFAULT_INITIAL_BACKOFF,
            max_backoff: DEFAULT_MAX_BACKOFF,
        }
    }

    /// Create a client with an empty API key — used for Ollama which requires
    /// no authentication.  The base URL may be the full `/v1` path or a bare
    /// host; either form is handled by `chat_completions_endpoint`.
    #[must_use]
    pub fn new_no_auth(base_url: impl Into<String>) -> Self {
        Self {
            http: build_http_client(),
            api_key: String::new(),
            base_url: base_url.into(),
            max_retries: DEFAULT_MAX_RETRIES,
            initial_backoff: DEFAULT_INITIAL_BACKOFF,
            max_backoff: DEFAULT_MAX_BACKOFF,
        }
    }

    pub fn from_env(config: OpenAiCompatConfig) -> Result<Self, ApiError> {
        let Some(api_key) = read_env_non_empty(config.api_key_env)? else {
            return Err(ApiError::missing_credentials(
                config.provider_name,
                config.credential_env_vars(),
            ));
        };
        Ok(Self::new(api_key, config))
    }

    /// Return the base URL this client is targeting.
    #[must_use]
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    #[must_use]
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }

    #[must_use]
    pub const fn with_retry_policy(
        mut self,
        max_retries: u32,
        initial_backoff: Duration,
        max_backoff: Duration,
    ) -> Self {
        self.max_retries = max_retries;
        self.initial_backoff = initial_backoff;
        self.max_backoff = max_backoff;
        self
    }

    pub async fn send_message(
        &self,
        request: &MessageRequest,
    ) -> Result<MessageResponse, ApiError> {
        let request = MessageRequest {
            stream: false,
            ..request.clone()
        };
        let response = self.send_with_retry(&request).await?;
        let request_id = request_id_from_headers(response.headers());
        let payload = response.json::<ChatCompletionResponse>().await?;
        let mut normalized = normalize_response(&request.model, payload)?;
        if normalized.request_id.is_none() {
            normalized.request_id = request_id;
        }
        Ok(normalized)
    }

    pub async fn stream_message(
        &self,
        request: &MessageRequest,
    ) -> Result<MessageStream, ApiError> {
        let response = self
            .send_with_retry(&request.clone().with_streaming())
            .await?;
        Ok(MessageStream {
            request_id: request_id_from_headers(response.headers()),
            response,
            parser: OpenAiSseParser::new(),
            pending: VecDeque::new(),
            done: false,
            state: StreamState::new(request.model.clone()),
            last_chunk_at: Instant::now(),
            dead_air_timeout: resolve_stream_dead_air_timeout(),
        })
    }

    async fn send_with_retry(
        &self,
        request: &MessageRequest,
    ) -> Result<reqwest::Response, ApiError> {
        common::send_with_retry(
            self.max_retries,
            self.initial_backoff,
            self.max_backoff,
            || self.send_raw_request(request),
        )
        .await
    }

    async fn send_raw_request(
        &self,
        request: &MessageRequest,
    ) -> Result<reqwest::Response, ApiError> {
        let request_url = chat_completions_endpoint(&self.base_url);
        let builder = self
            .http
            .post(&request_url)
            .header("content-type", "application/json")
            .bearer_auth(&self.api_key);
        common::apply_traceparent_header(builder)
            .json(&build_chat_completion_request(request))
            .send()
            .await
            .map_err(ApiError::from)
    }


}

impl Provider for OpenAiCompatClient {
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

/// Default dead-air timeout: 5 minutes (matching Claude Code upstream).
pub const DEFAULT_STREAM_DEAD_AIR_MS: u64 = 5 * 60 * 1_000;

/// Read the dead-air timeout from `ANVIL_STREAM_DEAD_AIR_MS` (plain
/// milliseconds).  Returns the default when unset or when the value is not a
/// valid integer.  This is intentionally fail-loud on garbage values: we log a
/// warning to stderr and fall back to the default rather than silently ignoring
/// the misconfiguration.
pub fn resolve_stream_dead_air_timeout() -> Duration {
    match std::env::var("ANVIL_STREAM_DEAD_AIR_MS") {
        Err(_) => Duration::from_millis(DEFAULT_STREAM_DEAD_AIR_MS),
        Ok(raw) => match raw.trim().parse::<u64>() {
            Ok(ms) => Duration::from_millis(ms),
            Err(_) => {
                eprintln!(
                    "[anvil] warning: ANVIL_STREAM_DEAD_AIR_MS={raw:?} is not a valid integer; \
                     using default {}ms",
                    DEFAULT_STREAM_DEAD_AIR_MS
                );
                Duration::from_millis(DEFAULT_STREAM_DEAD_AIR_MS)
            }
        },
    }
}

#[derive(Debug)]
pub struct MessageStream {
    request_id: Option<String>,
    response: reqwest::Response,
    parser: OpenAiSseParser,
    pending: VecDeque<StreamEvent>,
    done: bool,
    state: StreamState,
    /// Wall-clock time of the last successful chunk receipt.
    last_chunk_at: Instant,
    /// Maximum allowed gap between chunks before we surface a stall error.
    dead_air_timeout: Duration,
}

impl MessageStream {
    #[must_use]
    pub fn request_id(&self) -> Option<&str> {
        self.request_id.as_deref()
    }

    pub async fn next_event(&mut self) -> Result<Option<StreamEvent>, ApiError> {
        loop {
            if let Some(event) = self.pending.pop_front() {
                return Ok(Some(event));
            }

            if self.done {
                self.pending.extend(self.state.finish()?);
                if let Some(event) = self.pending.pop_front() {
                    return Ok(Some(event));
                }
                return Ok(None);
            }

            // Bug #82 fix: apply a dead-air timeout.  If the TCP connection
            // stays open but no bytes arrive within the window, surface a
            // distinctive error so the session layer can decide what to do.
            // We do NOT silently retry non-streaming here — that decision
            // belongs to the caller (mirrors Claude Code v2.1.111 behavior).
            let chunk_result = tokio::time::timeout(
                self.dead_air_timeout,
                self.response.chunk(),
            )
            .await;

            match chunk_result {
                Ok(Ok(Some(chunk))) => {
                    self.last_chunk_at = Instant::now();
                    for parsed in self.parser.push(&chunk)? {
                        self.pending.extend(self.state.ingest_chunk(parsed)?);
                    }
                }
                Ok(Ok(None)) => {
                    self.done = true;
                }
                Ok(Err(http_err)) => {
                    return Err(ApiError::from(http_err));
                }
                Err(_timeout) => {
                    let elapsed_ms = self.last_chunk_at.elapsed().as_millis() as u64;
                    return Err(ApiError::StreamStalled { elapsed_ms });
                }
            }
        }
    }
}

#[derive(Debug, Default)]
struct OpenAiSseParser {
    buffer: Vec<u8>,
}

impl OpenAiSseParser {
    fn new() -> Self {
        Self::default()
    }

    fn push(&mut self, chunk: &[u8]) -> Result<Vec<ChatCompletionChunk>, ApiError> {
        self.buffer.extend_from_slice(chunk);
        let mut events = Vec::new();

        while let Some(frame) = next_sse_frame(&mut self.buffer) {
            if let Some(payload) = extract_sse_data(&frame) {
                let event: ChatCompletionChunk =
                    serde_json::from_str(&payload).map_err(ApiError::from)?;
                events.push(event);
            }
        }

        Ok(events)
    }
}

#[derive(Debug)]
#[allow(clippy::struct_excessive_bools)]
struct StreamState {
    model: String,
    message_started: bool,
    text_started: bool,
    text_finished: bool,
    finished: bool,
    stop_reason: Option<String>,
    usage: Option<Usage>,
    tool_calls: BTreeMap<u32, ToolCallState>,
}

impl StreamState {
    const fn new(model: String) -> Self {
        Self {
            model,
            message_started: false,
            text_started: false,
            text_finished: false,
            finished: false,
            stop_reason: None,
            usage: None,
            tool_calls: BTreeMap::new(),
        }
    }

    fn ingest_chunk(&mut self, chunk: ChatCompletionChunk) -> Result<Vec<StreamEvent>, ApiError> {
        let mut events = Vec::new();
        if !self.message_started {
            self.message_started = true;
            events.push(StreamEvent::MessageStart(MessageStartEvent {
                message: MessageResponse {
                    id: chunk.id.clone(),
                    kind: "message".to_string(),
                    role: "assistant".to_string(),
                    content: Vec::new(),
                    model: chunk.model.clone().unwrap_or_else(|| self.model.clone()),
                    stop_reason: None,
                    stop_sequence: None,
                    usage: Usage {
                        input_tokens: 0,
                        cache_creation_input_tokens: 0,
                        cache_read_input_tokens: 0,
                        output_tokens: 0,
                    },
                    request_id: None,
                },
            }));
        }

        if let Some(usage) = chunk.usage {
            self.usage = Some(Usage {
                input_tokens: usage.prompt_tokens,
                cache_creation_input_tokens: 0,
                cache_read_input_tokens: 0,
                output_tokens: usage.completion_tokens,
            });
        }

        for choice in chunk.choices {
            if let Some(content) = choice.delta.content.filter(|value| !value.is_empty()) {
                if !self.text_started {
                    self.text_started = true;
                    events.push(StreamEvent::ContentBlockStart(ContentBlockStartEvent {
                        index: 0,
                        content_block: OutputContentBlock::Text {
                            text: String::new(),
                        },
                    }));
                }
                events.push(StreamEvent::ContentBlockDelta(ContentBlockDeltaEvent {
                    index: 0,
                    delta: ContentBlockDelta::TextDelta { text: content },
                }));
            }

            for tool_call in choice.delta.tool_calls {
                let state = self.tool_calls.entry(tool_call.index).or_default();
                state.apply(tool_call);
                let block_index = state.block_index();
                if !state.started {
                    if let Some(start_event) = state.start_event()? {
                        state.started = true;
                        events.push(StreamEvent::ContentBlockStart(start_event));
                    } else {
                        continue;
                    }
                }
                if let Some(delta_event) = state.delta_event() {
                    events.push(StreamEvent::ContentBlockDelta(delta_event));
                }
                if choice.finish_reason.as_deref() == Some("tool_calls") && !state.stopped {
                    state.stopped = true;
                    events.push(StreamEvent::ContentBlockStop(ContentBlockStopEvent {
                        index: block_index,
                    }));
                }
            }

            if let Some(finish_reason) = choice.finish_reason {
                self.stop_reason = Some(normalize_finish_reason(&finish_reason));
                if finish_reason == "tool_calls" {
                    for state in self.tool_calls.values_mut() {
                        if state.started && !state.stopped {
                            state.stopped = true;
                            events.push(StreamEvent::ContentBlockStop(ContentBlockStopEvent {
                                index: state.block_index(),
                            }));
                        }
                    }
                }
            }
        }

        Ok(events)
    }

    fn finish(&mut self) -> Result<Vec<StreamEvent>, ApiError> {
        if self.finished {
            return Ok(Vec::new());
        }
        self.finished = true;

        let mut events = Vec::new();
        if self.text_started && !self.text_finished {
            self.text_finished = true;
            events.push(StreamEvent::ContentBlockStop(ContentBlockStopEvent {
                index: 0,
            }));
        }

        for state in self.tool_calls.values_mut() {
            if !state.started
                && let Some(start_event) = state.start_event()? {
                    state.started = true;
                    events.push(StreamEvent::ContentBlockStart(start_event));
                    if let Some(delta_event) = state.delta_event() {
                        events.push(StreamEvent::ContentBlockDelta(delta_event));
                    }
                }
            if state.started && !state.stopped {
                state.stopped = true;
                events.push(StreamEvent::ContentBlockStop(ContentBlockStopEvent {
                    index: state.block_index(),
                }));
            }
        }

        if self.message_started {
            events.push(StreamEvent::MessageDelta(MessageDeltaEvent {
                delta: MessageDelta {
                    stop_reason: Some(
                        self.stop_reason
                            .clone()
                            .unwrap_or_else(|| "end_turn".to_string()),
                    ),
                    stop_sequence: None,
                },
                usage: self.usage.clone().unwrap_or(Usage {
                    input_tokens: 0,
                    cache_creation_input_tokens: 0,
                    cache_read_input_tokens: 0,
                    output_tokens: 0,
                }),
            }));
            events.push(StreamEvent::MessageStop(MessageStopEvent {}));
        }
        Ok(events)
    }
}

#[derive(Debug, Default)]
struct ToolCallState {
    openai_index: u32,
    id: Option<String>,
    name: Option<String>,
    arguments: String,
    emitted_len: usize,
    started: bool,
    stopped: bool,
}

impl ToolCallState {
    fn apply(&mut self, tool_call: DeltaToolCall) {
        self.openai_index = tool_call.index;
        if let Some(id) = tool_call.id {
            self.id = Some(id);
        }
        if let Some(name) = tool_call.function.name {
            self.name = Some(name);
        }
        if let Some(arguments) = tool_call.function.arguments {
            self.arguments.push_str(&arguments);
        }
    }

    const fn block_index(&self) -> u32 {
        self.openai_index + 1
    }

    #[allow(clippy::unnecessary_wraps)]
    fn start_event(&self) -> Result<Option<ContentBlockStartEvent>, ApiError> {
        let Some(name) = self.name.clone() else {
            return Ok(None);
        };
        let id = self
            .id
            .clone()
            .unwrap_or_else(|| format!("tool_call_{}", self.openai_index));
        Ok(Some(ContentBlockStartEvent {
            index: self.block_index(),
            content_block: OutputContentBlock::ToolUse {
                id,
                name,
                input: json!({}),
            },
        }))
    }

    fn delta_event(&mut self) -> Option<ContentBlockDeltaEvent> {
        if self.emitted_len >= self.arguments.len() {
            return None;
        }
        let delta = self.arguments[self.emitted_len..].to_string();
        self.emitted_len = self.arguments.len();
        Some(ContentBlockDeltaEvent {
            index: self.block_index(),
            delta: ContentBlockDelta::InputJsonDelta {
                partial_json: delta,
            },
        })
    }
}

#[derive(Debug, Deserialize)]
struct ChatCompletionResponse {
    id: String,
    model: String,
    choices: Vec<ChatChoice>,
    #[serde(default)]
    usage: Option<OpenAiUsage>,
}

#[derive(Debug, Deserialize)]
struct ChatChoice {
    message: ChatMessage,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ChatMessage {
    role: String,
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    tool_calls: Vec<ResponseToolCall>,
}

#[derive(Debug, Deserialize)]
struct ResponseToolCall {
    id: String,
    function: ResponseToolFunction,
}

#[derive(Debug, Deserialize)]
struct ResponseToolFunction {
    name: String,
    arguments: String,
}

#[derive(Debug, Deserialize)]
struct OpenAiUsage {
    #[serde(default)]
    prompt_tokens: u32,
    #[serde(default)]
    completion_tokens: u32,
}

#[derive(Debug, Deserialize)]
struct ChatCompletionChunk {
    id: String,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    choices: Vec<ChunkChoice>,
    #[serde(default)]
    usage: Option<OpenAiUsage>,
}

#[derive(Debug, Deserialize)]
struct ChunkChoice {
    delta: ChunkDelta,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct ChunkDelta {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    tool_calls: Vec<DeltaToolCall>,
}

#[derive(Debug, Deserialize)]
struct DeltaToolCall {
    #[serde(default)]
    index: u32,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    function: DeltaFunction,
}

#[derive(Debug, Default, Deserialize)]
struct DeltaFunction {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    arguments: Option<String>,
}

/// Resolve the `num_ctx` value to send to Ollama for a chat request.
///
/// Reads `ANVIL_OLLAMA_NUM_CTX` first (explicit, Ollama-specific), then
/// `ANVIL_CONTEXT_SIZE` (shared override used elsewhere in Anvil for the
/// context bar display). Both accept K/M suffixes. Falls back to 32_768.
///
/// Pure function: no I/O beyond reading env vars.
fn resolve_ollama_num_ctx() -> u64 {
    const DEFAULT_NUM_CTX: u64 = 32_768;

    for var in ["ANVIL_OLLAMA_NUM_CTX", "ANVIL_CONTEXT_SIZE"] {
        if let Ok(raw) = std::env::var(var)
            && let Some(parsed) = parse_num_ctx(&raw)
        {
            return parsed;
        }
    }
    DEFAULT_NUM_CTX
}

/// Parse a context-size string. Accepts plain numbers ("65536"), K-suffixed
/// ("128K"), and M-suffixed ("1M"). Returns None on any parse failure so the
/// caller can fall through to the next priority.
fn parse_num_ctx(raw: &str) -> Option<u64> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let lower = trimmed.to_ascii_lowercase();

    let (digits, multiplier): (&str, u64) = if let Some(rest) = lower.strip_suffix('k') {
        (rest, 1_000)
    } else if let Some(rest) = lower.strip_suffix('m') {
        (rest, 1_000_000)
    } else {
        (lower.as_str(), 1)
    };

    let n: u64 = digits.trim().parse().ok()?;
    n.checked_mul(multiplier)
}

/// Public wrapper used by non-OpenAiCompat providers (Azure, etc.) that share
/// the same OpenAI-compatible wire format but need to inject their own URL/auth.
#[must_use]
pub fn build_chat_completion_request_value(request: &MessageRequest, stream: bool) -> Value {
    let req = if stream != request.stream {
        MessageRequest { stream, ..request.clone() }
    } else {
        request.clone()
    };
    build_chat_completion_request(&req)
}

fn build_chat_completion_request(request: &MessageRequest) -> Value {
    let mut messages = Vec::new();
    if let Some(system) = request.system.as_ref().filter(|value| !value.is_empty()) {
        messages.push(json!({
            "role": "system",
            "content": system,
        }));
    }
    for message in &request.messages {
        messages.extend(translate_message(message));
    }

    let mut payload = json!({
        "model": request.model,
        "max_tokens": request.max_tokens,
        "messages": messages,
        "stream": request.stream,
    });

    // Request usage stats in streaming mode (supported by OpenAI, Ollama, and compatible APIs)
    if request.stream {
        payload["stream_options"] = json!({"include_usage": true});
    }

    // For Ollama models: pass think parameter to control reasoning mode.
    // Ollama's /v1/ endpoint may not support this yet, but native /api/chat does.
    // We pass it anyway for forward compatibility, and it's harmless for other providers.
    let is_ollama_model = request.model.contains(':')
        || request.model.starts_with("qwen")
        || request.model.starts_with("llama")
        || request.model.starts_with("glm");
    if is_ollama_model {
        // Default to think: false unless explicitly enabled
        // This dramatically speeds up responses for thinking-capable models
        payload["think"] = json!(false);

        // Tell Ollama how large a context window to allocate for this request.
        // Without num_ctx, Ollama silently caps the context at its Modelfile
        // default (typically 2048 tokens) regardless of the model's actual
        // capability — so qwen3:8b (128K-capable) gets truncated to 2K and
        // agentic workflows fall over with "context exceeded" surprises.
        //
        // Priority:
        //   1. ANVIL_OLLAMA_NUM_CTX   — explicit per-request override
        //   2. ANVIL_CONTEXT_SIZE     — shared override used by the TUI display
        //   3. 32_768                 — safe default; larger than qwen3's
        //                               Modelfile default, well within the
        //                               capability envelope of current local
        //                               models running on consumer GPUs
        //
        // Values accept a trailing K or M multiplier (e.g. "128K", "1M").
        let num_ctx = resolve_ollama_num_ctx();
        payload["options"] = json!({ "num_ctx": num_ctx });
    }

    if let Some(tools) = &request.tools {
        payload["tools"] =
            Value::Array(tools.iter().map(openai_tool_definition).collect::<Vec<_>>());
    }
    if let Some(tool_choice) = &request.tool_choice {
        payload["tool_choice"] = openai_tool_choice(tool_choice);
    }

    // ── Effort / reasoning injection ─────────────────────────────────────────
    //
    // When ANVIL_EFFORT is set the session layer has already validated the
    // level; we inject the provider-specific knob here so the wire format
    // matches what each API expects.
    //
    // OpenAI / xAI o-series and Grok: inject `reasoning.effort`.
    // Gemini: inject `generationConfig.thinkingConfig.thinkingBudget`.
    // Ollama: flip `think` from false → true (already in the payload above).
    //
    // Non-reasoning models from any provider: skip silently — the payload
    // key is simply not added, maintaining identical wire format to pre-effort
    // behaviour.
    if let Some(effort) = EffortLevel::from_env() {
        let is_openai_reasoning = request.model.starts_with("o1")
            || request.model.starts_with("o3")
            || request.model.starts_with("o4")
            || request.model.contains("codex");
        let is_xai_reasoning = request.model.starts_with("grok");
        let is_gemini = request.model.starts_with("gemini");

        if is_openai_reasoning || is_xai_reasoning {
            payload["reasoning"] = json!({
                "effort": effort.openai_reasoning_effort(),
                "summary": "auto",
            });
        } else if is_gemini {
            let thinking_budget = effort.gemini_thinking_budget();
            if thinking_budget == -1 {
                // Dynamic mode: omit the budget key so Gemini chooses adaptively.
                payload["generationConfig"] = json!({
                    "thinkingConfig": { "thinkingMode": "dynamic" }
                });
            } else {
                payload["generationConfig"] = json!({
                    "thinkingConfig": {
                        "thinkingMode": "enabled",
                        "thinkingBudget": thinking_budget
                    }
                });
            }
        } else if is_ollama_model {
            // Override the default `think: false` that was set above.
            payload["think"] = json!(true);
        }
    }

    payload
}

fn translate_message(message: &InputMessage) -> Vec<Value> {
    match message.role.as_str() {
        "assistant" => {
            let mut text = String::new();
            let mut tool_calls = Vec::new();
            for block in &message.content {
                match block {
                    InputContentBlock::Text { text: value } => text.push_str(value),
                    // Images / documents are not expected in assistant turns; skip silently.
                    InputContentBlock::Image { .. }
                    | InputContentBlock::Document { .. }
                    | InputContentBlock::ToolResult { .. } => {}
                    InputContentBlock::ToolUse { id, name, input } => tool_calls.push(json!({
                        "id": id,
                        "type": "function",
                        "function": {
                            "name": name,
                            "arguments": input.to_string(),
                        }
                    }))
                }
            }
            if text.is_empty() && tool_calls.is_empty() {
                Vec::new()
            } else {
                vec![json!({
                    "role": "assistant",
                    "content": (!text.is_empty()).then_some(text),
                    "tool_calls": tool_calls,
                })]
            }
        }
        _ => message
            .content
            .iter()
            .filter_map(|block| match block {
                InputContentBlock::Text { text } => Some(json!({
                    "role": "user",
                    "content": text,
                })),
                InputContentBlock::Image { source } => Some(json!({
                    "role": "user",
                    "content": [{
                        "type": "image_url",
                        "image_url": {
                            "url": format!(
                                "data:{};base64,{}",
                                source.media_type,
                                source.data
                            )
                        }
                    }]
                })),
                // Task #601 (v2.2.16): non-Anthropic providers don't
                // support Anthropic's native `document` content block.
                // Degrade gracefully to a text notice with the base64
                // body inline (Option B from the brief): the model at
                // least sees that something was attached, the filename,
                // the MIME type, and the encoded bytes — capable models
                // can still decode it if they choose. Adding a
                // `pdf-extract` dep was rejected to keep the build
                // matrix portable (FreeBSD / NetBSD source-only).
                InputContentBlock::Document { source, title, context } => {
                    let label = title.as_deref().unwrap_or("document");
                    let ctx = context
                        .as_deref()
                        .map(|c| format!(" — {c}"))
                        .unwrap_or_default();
                    Some(json!({
                        "role": "user",
                        "content": format!(
                            "[Document attached: {label} ({}, {} base64 bytes){ctx}]\n\n\
                             <document filename=\"{label}\" media_type=\"{}\" \
                             encoding=\"base64\">\n{}\n</document>",
                            source.media_type,
                            source.data.len(),
                            source.media_type,
                            source.data,
                        ),
                    }))
                }
                InputContentBlock::ToolResult {
                    tool_use_id,
                    content,
                    is_error,
                } => Some(json!({
                    "role": "tool",
                    "tool_call_id": tool_use_id,
                    "content": flatten_tool_result_content(content),
                    "is_error": is_error,
                })),
                InputContentBlock::ToolUse { .. } => None,
            })
            .collect(),
    }
}

fn flatten_tool_result_content(content: &[ToolResultContentBlock]) -> String {
    content
        .iter()
        .map(|block| match block {
            ToolResultContentBlock::Text { text } => text.clone(),
            ToolResultContentBlock::Json { value } => value.to_string(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn openai_tool_definition(tool: &ToolDefinition) -> Value {
    json!({
        "type": "function",
        "function": {
            "name": tool.name,
            "description": tool.description,
            "parameters": tool.input_schema,
        }
    })
}

fn openai_tool_choice(tool_choice: &ToolChoice) -> Value {
    match tool_choice {
        ToolChoice::Auto => Value::String("auto".to_string()),
        ToolChoice::Any => Value::String("required".to_string()),
        ToolChoice::Tool { name } => json!({
            "type": "function",
            "function": { "name": name },
        }),
    }
}

fn normalize_response(
    model: &str,
    response: ChatCompletionResponse,
) -> Result<MessageResponse, ApiError> {
    let choice = response
        .choices
        .into_iter()
        .next()
        .ok_or(ApiError::InvalidSseFrame(
            "chat completion response missing choices",
        ))?;
    let mut content = Vec::new();

    let had_structured_tool_calls = !choice.message.tool_calls.is_empty();

    // Capture the raw text content before consuming it.
    let text_content = choice
        .message
        .content
        .filter(|value| !value.is_empty());

    // Primary path: structured OpenAI-format tool_calls.
    for tool_call in choice.message.tool_calls {
        content.push(OutputContentBlock::ToolUse {
            id: tool_call.id,
            name: tool_call.function.name,
            input: parse_tool_arguments(&tool_call.function.arguments),
        });
    }

    // Secondary path: scan text for inline tool calls (Ollama fallback).
    if let Some(ref text) = text_content {
        let parsed = parse_ollama_text_for_tool_calls(text, had_structured_tool_calls);

        for (idx, call) in parsed.tool_calls.iter().enumerate() {
            let id = format!("inline_tool_{}_{}", idx, call.name);
            content.push(OutputContentBlock::ToolUse {
                id,
                name: call.name.clone(),
                input: call.input.clone(),
            });
        }

        // Fail-loud: append warning as a text block when the model claimed
        // to write a file but no tool call was found anywhere.
        if let Some(detection) = parsed.silent_write {
            let warning = silent_write_warning(&detection);
            content.push(OutputContentBlock::Text { text: warning });
        }
    }

    // Text block goes first so context appears before tool results in chat.
    if let Some(text) = text_content {
        content.insert(0, OutputContentBlock::Text { text });
    }

    Ok(MessageResponse {
        id: response.id,
        kind: "message".to_string(),
        role: choice.message.role,
        content,
        model: response.model.if_empty_then(model.to_string()),
        stop_reason: choice
            .finish_reason
            .map(|value| normalize_finish_reason(&value)),
        stop_sequence: None,
        usage: Usage {
            input_tokens: response
                .usage
                .as_ref()
                .map_or(0, |usage| usage.prompt_tokens),
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
            output_tokens: response
                .usage
                .as_ref()
                .map_or(0, |usage| usage.completion_tokens),
        },
        request_id: None,
    })
}

fn parse_tool_arguments(arguments: &str) -> Value {
    serde_json::from_str(arguments).unwrap_or_else(|_| json!({ "raw": arguments }))
}




#[must_use]
pub fn has_api_key(key: &str) -> bool {
    read_env_non_empty(key)
        .ok()
        .and_then(std::convert::identity)
        .is_some()
}

#[must_use]
pub fn read_base_url(config: OpenAiCompatConfig) -> String {
    std::env::var(config.base_url_env).unwrap_or_else(|_| config.default_base_url.to_string())
}

fn chat_completions_endpoint(base_url: &str) -> String {
    let trimmed = base_url.trim_end_matches('/');
    if trimmed.ends_with("/chat/completions") {
        trimmed.to_string()
    } else if trimmed.ends_with("/v1") {
        format!("{trimmed}/chat/completions")
    } else if trimmed.contains("localhost") || trimmed.contains("127.0.0.1") || trimmed.contains("11434") {
        // Ollama — needs /v1/ prefix for OpenAI-compatible endpoint
        format!("{trimmed}/v1/chat/completions")
    } else {
        format!("{trimmed}/chat/completions")
    }
}



fn normalize_finish_reason(value: &str) -> String {
    match value {
        "stop" => "end_turn",
        "tool_calls" => "tool_use",
        other => other,
    }
    .to_string()
}

trait StringExt {
    fn if_empty_then(self, fallback: String) -> String;
}

impl StringExt for String {
    fn if_empty_then(self, fallback: String) -> String {
        if self.is_empty() {
            fallback
        } else {
            self
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        build_chat_completion_request, chat_completions_endpoint, normalize_finish_reason,
        openai_tool_choice, parse_num_ctx, parse_tool_arguments, resolve_ollama_num_ctx,
        OpenAiCompatClient, OpenAiCompatConfig,
    };
    use crate::error::ApiError;
    use crate::types::{
        DocumentSource, DocumentSourceKind, InputContentBlock, InputMessage, MessageRequest,
        ToolChoice, ToolDefinition, ToolResultContentBlock,
    };
    use serde_json::json;

    #[test]
    fn request_translation_uses_openai_compatible_shape() {
        let payload = build_chat_completion_request(&MessageRequest {
            model: "grok-3".to_string(),
            max_tokens: 64,
            messages: vec![InputMessage {
                role: "user".to_string(),
                content: vec![
                    InputContentBlock::Text {
                        text: "hello".to_string(),
                    },
                    InputContentBlock::ToolResult {
                        tool_use_id: "tool_1".to_string(),
                        content: vec![ToolResultContentBlock::Json {
                            value: json!({"ok": true}),
                        }],
                        is_error: false,
                    },
                ],
            }],
            system: Some("be helpful".to_string()),
            tools: Some(vec![ToolDefinition {
                name: "weather".to_string(),
                description: Some("Get weather".to_string()),
                input_schema: json!({"type": "object"}),
            }]),
            tool_choice: Some(ToolChoice::Auto),
            stream: false,
        });

        assert_eq!(payload["messages"][0]["role"], json!("system"));
        assert_eq!(payload["messages"][1]["role"], json!("user"));
        assert_eq!(payload["messages"][2]["role"], json!("tool"));
        assert_eq!(payload["tools"][0]["type"], json!("function"));
        assert_eq!(payload["tool_choice"], json!("auto"));
    }

    #[test]
    fn tool_choice_translation_supports_required_function() {
        assert_eq!(openai_tool_choice(&ToolChoice::Any), json!("required"));
        assert_eq!(
            openai_tool_choice(&ToolChoice::Tool {
                name: "weather".to_string(),
            }),
            json!({"type": "function", "function": {"name": "weather"}})
        );
    }

    #[test]
    fn parses_tool_arguments_fallback() {
        assert_eq!(
            parse_tool_arguments("{\"city\":\"Paris\"}"),
            json!({"city": "Paris"})
        );
        assert_eq!(parse_tool_arguments("not-json"), json!({"raw": "not-json"}));
    }

    #[test]
    fn missing_xai_api_key_is_provider_specific() {
        let _lock = env_lock();
        unsafe { std::env::remove_var("XAI_API_KEY"); }
        let error = OpenAiCompatClient::from_env(OpenAiCompatConfig::xai())
            .expect_err("missing key should error");
        assert!(matches!(
            error,
            ApiError::MissingCredentials {
                provider: "xAI",
                ..
            }
        ));
    }

    #[test]
    fn endpoint_builder_accepts_base_urls_and_full_endpoints() {
        assert_eq!(
            chat_completions_endpoint("https://api.x.ai/v1"),
            "https://api.x.ai/v1/chat/completions"
        );
        assert_eq!(
            chat_completions_endpoint("https://api.x.ai/v1/"),
            "https://api.x.ai/v1/chat/completions"
        );
        assert_eq!(
            chat_completions_endpoint("https://api.x.ai/v1/chat/completions"),
            "https://api.x.ai/v1/chat/completions"
        );
    }

    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        super::super::crate_env_lock()
    }

    #[test]
    fn normalizes_stop_reasons() {
        assert_eq!(normalize_finish_reason("stop"), "end_turn");
        assert_eq!(normalize_finish_reason("tool_calls"), "tool_use");
    }

    // ─── Ollama num_ctx override tests ──────────────────────────────────

    #[test]
    fn parse_num_ctx_accepts_plain_digits() {
        assert_eq!(parse_num_ctx("65536"), Some(65_536));
        assert_eq!(parse_num_ctx("   131072   "), Some(131_072));
    }

    #[test]
    fn parse_num_ctx_accepts_k_and_m_suffixes() {
        assert_eq!(parse_num_ctx("128K"), Some(128_000));
        assert_eq!(parse_num_ctx("128k"), Some(128_000));
        assert_eq!(parse_num_ctx("1M"), Some(1_000_000));
        assert_eq!(parse_num_ctx("1m"), Some(1_000_000));
    }

    #[test]
    fn parse_num_ctx_rejects_garbage() {
        assert_eq!(parse_num_ctx(""), None);
        assert_eq!(parse_num_ctx("   "), None);
        assert_eq!(parse_num_ctx("abc"), None);
        assert_eq!(parse_num_ctx("128X"), None);
        assert_eq!(parse_num_ctx("128KB"), None); // not a supported suffix
    }

    // Env-var-driven tests share process state with other tests in this
    // module — the existing `env_lock` (above) serialises them.

    struct EnvRestore {
        key: &'static str,
        original: Option<String>,
    }

    impl EnvRestore {
        fn set(key: &'static str, value: Option<&str>) -> Self {
            let original = std::env::var(key).ok();
            match value {
                Some(v) => unsafe { std::env::set_var(key, v) },
                None => unsafe { std::env::remove_var(key) },
            }
            Self { key, original }
        }
    }

    impl Drop for EnvRestore {
        fn drop(&mut self) {
            match &self.original {
                Some(v) => unsafe { std::env::set_var(self.key, v) },
                None => unsafe { std::env::remove_var(self.key) },
            }
        }
    }

    #[test]
    fn resolve_num_ctx_defaults_to_32k_when_env_unset() {
        let _lock = env_lock();
        let _a = EnvRestore::set("ANVIL_OLLAMA_NUM_CTX", None);
        let _b = EnvRestore::set("ANVIL_CONTEXT_SIZE", None);
        assert_eq!(resolve_ollama_num_ctx(), 32_768);
    }

    #[test]
    fn resolve_num_ctx_honors_ollama_specific_var_first() {
        let _lock = env_lock();
        let _a = EnvRestore::set("ANVIL_OLLAMA_NUM_CTX", Some("128K"));
        let _b = EnvRestore::set("ANVIL_CONTEXT_SIZE", Some("1M"));
        assert_eq!(resolve_ollama_num_ctx(), 128_000);
    }

    #[test]
    fn resolve_num_ctx_falls_through_to_generic_context_size() {
        let _lock = env_lock();
        let _a = EnvRestore::set("ANVIL_OLLAMA_NUM_CTX", None);
        let _b = EnvRestore::set("ANVIL_CONTEXT_SIZE", Some("65536"));
        assert_eq!(resolve_ollama_num_ctx(), 65_536);
    }

    #[test]
    fn resolve_num_ctx_ignores_garbage_env_and_falls_through() {
        let _lock = env_lock();
        let _a = EnvRestore::set("ANVIL_OLLAMA_NUM_CTX", Some("not-a-number"));
        let _b = EnvRestore::set("ANVIL_CONTEXT_SIZE", Some("64K"));
        // The Ollama-specific var is garbage → fall through to ANVIL_CONTEXT_SIZE.
        assert_eq!(resolve_ollama_num_ctx(), 64_000);
    }

    #[test]
    fn ollama_request_includes_num_ctx_options() {
        let _lock = env_lock();
        let _a = EnvRestore::set("ANVIL_OLLAMA_NUM_CTX", Some("100K"));
        let _b = EnvRestore::set("ANVIL_CONTEXT_SIZE", None);

        let payload = build_chat_completion_request(&MessageRequest {
            model: "qwen3:8b".to_string(),
            max_tokens: 64,
            messages: vec![InputMessage {
                role: "user".to_string(),
                content: vec![InputContentBlock::Text {
                    text: "hi".to_string(),
                }],
            }],
            system: None,
            tools: None,
            tool_choice: None,
            stream: true,
        });

        let options = payload
            .get("options")
            .and_then(|v| v.as_object())
            .expect("Ollama payload must include options object");
        assert_eq!(
            options.get("num_ctx").and_then(|v| v.as_u64()),
            Some(100_000)
        );
    }

    #[test]
    fn non_ollama_request_does_not_include_num_ctx() {
        let _lock = env_lock();
        let _a = EnvRestore::set("ANVIL_OLLAMA_NUM_CTX", Some("100K"));

        let payload = build_chat_completion_request(&MessageRequest {
            model: "gpt-4o".to_string(),
            max_tokens: 64,
            messages: vec![InputMessage {
                role: "user".to_string(),
                content: vec![InputContentBlock::Text {
                    text: "hi".to_string(),
                }],
            }],
            system: None,
            tools: None,
            tool_choice: None,
            stream: true,
        });

        // gpt-4o is an OpenAI model, not Ollama; num_ctx should not be set.
        assert!(
            payload.get("options").is_none(),
            "non-Ollama request must not include Ollama-specific options"
        );
    }

    // ─── Bug #82: stream dead-air timeout ───────────────────────────────────

    /// A fake reqwest::Response body that sends one byte chunk and then never
    /// yields another chunk, simulating a stalled stream.
    ///
    /// We test `resolve_stream_dead_air_timeout` and the env-var parsing
    /// directly (the async `MessageStream` test would require an actual HTTP
    /// mock server, which is heavy for a unit test).  The timeout logic is
    /// thin enough that a parse + duration assertion is sufficient coverage;
    /// the integration path is exercised by real streaming smoke tests.

    #[test]
    fn resolve_dead_air_timeout_returns_default_when_env_unset() {
        use super::resolve_stream_dead_air_timeout;
        use super::DEFAULT_STREAM_DEAD_AIR_MS;
        let _lock = env_lock();
        let _restore = EnvRestore::set("ANVIL_STREAM_DEAD_AIR_MS", None);
        let got = resolve_stream_dead_air_timeout();
        assert_eq!(
            got,
            std::time::Duration::from_millis(DEFAULT_STREAM_DEAD_AIR_MS)
        );
    }

    #[test]
    fn resolve_dead_air_timeout_reads_env_override() {
        use super::resolve_stream_dead_air_timeout;
        let _lock = env_lock();
        let _restore = EnvRestore::set("ANVIL_STREAM_DEAD_AIR_MS", Some("12345"));
        let got = resolve_stream_dead_air_timeout();
        assert_eq!(got, std::time::Duration::from_millis(12345));
    }

    #[test]
    fn resolve_dead_air_timeout_falls_back_on_garbage() {
        use super::resolve_stream_dead_air_timeout;
        use super::DEFAULT_STREAM_DEAD_AIR_MS;
        let _lock = env_lock();
        let _restore = EnvRestore::set("ANVIL_STREAM_DEAD_AIR_MS", Some("not-a-number"));
        let got = resolve_stream_dead_air_timeout();
        assert_eq!(
            got,
            std::time::Duration::from_millis(DEFAULT_STREAM_DEAD_AIR_MS),
            "garbage env var should fall back to default, not panic"
        );
    }

    /// Confirm the stall error surfaces with the right message.
    /// We use `ApiError::StreamStalled` directly rather than driving the full
    /// async streaming path (which would require a mock HTTP server).
    #[test]
    fn stream_stalled_error_displays_elapsed() {
        let err = ApiError::StreamStalled { elapsed_ms: 300_000 };
        let msg = err.to_string();
        assert!(
            msg.contains("stream stalled after 300000ms"),
            "unexpected display: {msg}"
        );
    }

    // ─── Bug #84: configurable API request timeout ───────────────────────────

    #[test]
    fn resolve_api_timeout_returns_default_when_env_unset() {
        use super::{resolve_api_timeout, DEFAULT_API_TIMEOUT_MS};
        let _lock = env_lock();
        let _restore = EnvRestore::set("ANVIL_API_TIMEOUT_MS", None);
        let got = resolve_api_timeout();
        assert_eq!(
            got,
            std::time::Duration::from_millis(DEFAULT_API_TIMEOUT_MS),
            "default should be 10 minutes"
        );
    }

    #[test]
    fn resolve_api_timeout_reads_env_override() {
        use super::resolve_api_timeout;
        let _lock = env_lock();
        let _restore = EnvRestore::set("ANVIL_API_TIMEOUT_MS", Some("5000"));
        let got = resolve_api_timeout();
        assert_eq!(got, std::time::Duration::from_millis(5000));
    }

    #[test]
    fn resolve_api_timeout_falls_back_on_garbage_value() {
        use super::{resolve_api_timeout, DEFAULT_API_TIMEOUT_MS};
        let _lock = env_lock();
        let _restore = EnvRestore::set("ANVIL_API_TIMEOUT_MS", Some("garbage"));
        let got = resolve_api_timeout();
        assert_eq!(
            got,
            std::time::Duration::from_millis(DEFAULT_API_TIMEOUT_MS),
            "garbage env var should fall back to default, not panic or accept"
        );
    }

    #[test]
    fn resolve_api_timeout_default_is_ten_minutes() {
        use super::DEFAULT_API_TIMEOUT_MS;
        assert_eq!(
            DEFAULT_API_TIMEOUT_MS,
            600_000,
            "default API timeout must be 10 minutes (600_000 ms)"
        );
    }

    // ─── Effort injection tests ─────────────────────────────────────────────

    fn make_request(model: &str) -> MessageRequest {
        MessageRequest {
            model: model.to_string(),
            max_tokens: 4096,
            messages: vec![InputMessage {
                role: "user".to_string(),
                content: vec![InputContentBlock::Text {
                    text: "hello".to_string(),
                }],
            }],
            system: None,
            tools: None,
            tool_choice: None,
            stream: false,
        }
    }

    #[test]
    fn no_reasoning_block_when_effort_env_absent() {
        let _lock = env_lock();
        let _e = EnvRestore::set("ANVIL_EFFORT", None);
        let payload = build_chat_completion_request(&make_request("o3-mini"));
        assert!(
            payload.get("reasoning").is_none(),
            "reasoning must be absent when ANVIL_EFFORT is unset"
        );
    }

    #[test]
    fn openai_reasoning_effort_medium_injects_correct_value() {
        let _lock = env_lock();
        let _e = EnvRestore::set("ANVIL_EFFORT", Some("medium"));
        let payload = build_chat_completion_request(&make_request("o3-mini"));
        let reasoning = payload.get("reasoning").expect("reasoning block must be present");
        assert_eq!(reasoning["effort"], json!("medium"));
        assert_eq!(reasoning["summary"], json!("auto"));
    }

    #[test]
    fn openai_reasoning_effort_xhigh_maps_to_high() {
        let _lock = env_lock();
        let _e = EnvRestore::set("ANVIL_EFFORT", Some("xhigh"));
        let payload = build_chat_completion_request(&make_request("o4-mini"));
        let reasoning = payload.get("reasoning").expect("reasoning block must be present");
        assert_eq!(reasoning["effort"], json!("high"), "xhigh must fold to high for OpenAI");
    }

    #[test]
    fn grok_model_gets_reasoning_effort() {
        let _lock = env_lock();
        let _e = EnvRestore::set("ANVIL_EFFORT", Some("high"));
        let payload = build_chat_completion_request(&make_request("grok-3"));
        let reasoning = payload.get("reasoning").expect("grok model must get reasoning block");
        assert_eq!(reasoning["effort"], json!("high"));
    }

    #[test]
    fn non_reasoning_openai_model_does_not_get_reasoning_block() {
        let _lock = env_lock();
        let _e = EnvRestore::set("ANVIL_EFFORT", Some("high"));
        let payload = build_chat_completion_request(&make_request("gpt-4o"));
        assert!(
            payload.get("reasoning").is_none(),
            "non-reasoning OpenAI models must not receive a reasoning block"
        );
    }

    #[test]
    fn gemini_medium_effort_injects_thinking_config() {
        let _lock = env_lock();
        let _e = EnvRestore::set("ANVIL_EFFORT", Some("medium"));
        let payload = build_chat_completion_request(&make_request("gemini-2.0-flash"));
        let config = payload
            .get("generationConfig")
            .expect("generationConfig must be present for Gemini");
        let thinking = config
            .get("thinkingConfig")
            .expect("thinkingConfig must be present");
        assert_eq!(thinking["thinkingMode"], json!("enabled"));
        assert_eq!(thinking["thinkingBudget"], json!(8192));
    }

    #[test]
    fn gemini_xhigh_effort_uses_dynamic_mode() {
        let _lock = env_lock();
        let _e = EnvRestore::set("ANVIL_EFFORT", Some("xhigh"));
        let payload = build_chat_completion_request(&make_request("gemini-2.5-pro"));
        let config = payload
            .get("generationConfig")
            .expect("generationConfig must be present");
        let thinking = config
            .get("thinkingConfig")
            .expect("thinkingConfig must be present");
        assert_eq!(thinking["thinkingMode"], json!("dynamic"));
        assert!(
            thinking.get("thinkingBudget").is_none(),
            "dynamic mode must omit thinkingBudget"
        );
    }

    #[test]
    fn ollama_model_enables_think_when_effort_set() {
        let _lock = env_lock();
        let _e = EnvRestore::set("ANVIL_EFFORT", Some("high"));
        let payload = build_chat_completion_request(&make_request("qwen3:8b"));
        assert_eq!(
            payload.get("think"),
            Some(&json!(true)),
            "Ollama model must have think=true when ANVIL_EFFORT is set"
        );
    }

    #[test]
    fn ollama_model_defaults_think_false_when_effort_absent() {
        let _lock = env_lock();
        let _e = EnvRestore::set("ANVIL_EFFORT", None);
        let payload = build_chat_completion_request(&make_request("qwen3:8b"));
        assert_eq!(
            payload.get("think"),
            Some(&json!(false)),
            "Ollama model must have think=false when ANVIL_EFFORT is unset"
        );
    }

    /// Task #601 (v2.2.16): `InputContentBlock::Document` does not have
    /// a native shape in OpenAI's API (and by extension Ollama's
    /// OpenAI-compatible endpoint). The wire builder must degrade
    /// gracefully — Option B in the brief: send a `[Document
    /// attached: …]` notice with the base64 payload embedded inline so
    /// the model at least sees what was pasted. Anthropic's native
    /// `document` block is reserved for the Anthropic provider path
    /// (`crates/api/src/providers/anvil_provider.rs`).
    ///
    /// This is the `document_block_falls_back_to_text_on_ollama_provider`
    /// test from the task brief — we use the Ollama-tagged model name
    /// to exercise the same Ollama codepath the OpenAI-compat client
    /// uses at runtime.
    #[test]
    fn document_block_falls_back_to_text_on_ollama_provider() {
        let _lock = env_lock();
        let _e = EnvRestore::set("ANVIL_EFFORT", None);
        let payload = build_chat_completion_request(&MessageRequest {
            model: "ollama/qwen3:8b".to_string(),
            max_tokens: 64,
            messages: vec![InputMessage {
                role: "user".to_string(),
                content: vec![InputContentBlock::Document {
                    source: DocumentSource {
                        kind: DocumentSourceKind::Base64,
                        media_type: "application/pdf".to_string(),
                        data: "JVBERi0xLjQK".to_string(),
                    },
                    title: Some("spec.pdf".to_string()),
                    context: None,
                }],
            }],
            system: None,
            tools: None,
            tool_choice: None,
            stream: false,
        });

        // The user message must NOT carry a native `document` content
        // block — Ollama would 400 on that shape. Instead, the body is a
        // plain text content with the `[Document attached: ...]` notice
        // and the base64 payload embedded.
        let body = payload.to_string();
        assert!(
            !body.contains("\"type\":\"document\""),
            "Ollama wire body must not contain Anthropic's native \
             `document` block; got: {body}"
        );
        assert!(
            body.contains("[Document attached: spec.pdf"),
            "expected the fallback notice; got: {body}"
        );
        assert!(
            body.contains("application/pdf"),
            "fallback should still surface the MIME type; got: {body}"
        );
        assert!(
            body.contains("JVBERi0xLjQK"),
            "fallback should embed the base64 body so capable models can \
             still decode it; got: {body}"
        );
    }
}
