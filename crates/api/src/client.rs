use crate::error::ApiError;
use crate::failover::{FailoverChain, FailoverEvent, format_failover_event};
use crate::providers::anvil_provider::{self, AuthSource, AnvilApiClient};
use crate::providers::ollama::{self, OllamaClient};
use crate::providers::openai_compat::{self, OpenAiCompatClient, OpenAiCompatConfig};
use crate::providers::copilot::CopilotClient;
use crate::providers::azure::AzureOpenAiClient;
use crate::providers::bedrock::BedrockClient;
use crate::providers::{self, Provider, ProviderKind};
use crate::types::{MessageRequest, MessageResponse, StreamEvent};

async fn send_via_provider<P: Provider>(
    provider: &P,
    request: &MessageRequest,
) -> Result<MessageResponse, ApiError> {
    provider.send_message(request).await
}

async fn stream_via_provider<P: Provider>(
    provider: &P,
    request: &MessageRequest,
) -> Result<P::Stream, ApiError> {
    provider.stream_message(request).await
}

/// Routes a [`ProviderKind`] that uses the OpenAI-compatible wire format to its
/// [`OpenAiCompatConfig`].  Returns `None` for providers that need bespoke
/// clients (Anthropic, Ollama, Copilot, Azure, Bedrock).
fn openai_compat_config(kind: ProviderKind) -> Option<OpenAiCompatConfig> {
    match kind {
        ProviderKind::Xai => Some(OpenAiCompatConfig::xai()),
        ProviderKind::OpenAi => Some(OpenAiCompatConfig::openai()),
        ProviderKind::Gemini => Some(OpenAiCompatConfig::gemini()),
        ProviderKind::Fireworks => Some(OpenAiCompatConfig::fireworks()),
        ProviderKind::Groq => Some(OpenAiCompatConfig::groq()),
        ProviderKind::Mistral => Some(OpenAiCompatConfig::mistral()),
        ProviderKind::Perplexity => Some(OpenAiCompatConfig::perplexity()),
        ProviderKind::DeepSeek => Some(OpenAiCompatConfig::deepseek()),
        ProviderKind::TogetherAi => Some(OpenAiCompatConfig::togetherai()),
        ProviderKind::DeepInfra => Some(OpenAiCompatConfig::deepinfra()),
        ProviderKind::Cerebras => Some(OpenAiCompatConfig::cerebras()),
        ProviderKind::NvidiaNim => Some(OpenAiCompatConfig::nvidia_nim()),
        ProviderKind::HuggingFace => Some(OpenAiCompatConfig::huggingface()),
        ProviderKind::MoonshotAi => Some(OpenAiCompatConfig::moonshotai()),
        ProviderKind::Nebius => Some(OpenAiCompatConfig::nebius()),
        ProviderKind::OpenRouter => Some(OpenAiCompatConfig::openrouter()),
        ProviderKind::LmStudio => Some(OpenAiCompatConfig::lmstudio()),
        ProviderKind::Chutes => Some(OpenAiCompatConfig::chutes()),
        ProviderKind::Scaleway => Some(OpenAiCompatConfig::scaleway()),
        ProviderKind::Baseten => Some(OpenAiCompatConfig::baseten()),
        ProviderKind::MiniMax => Some(OpenAiCompatConfig::minimax()),
        ProviderKind::StackIt => Some(OpenAiCompatConfig::stackit()),
        ProviderKind::Cortecs => Some(OpenAiCompatConfig::cortecs()),
        ProviderKind::Ai302 => Some(OpenAiCompatConfig::ai302()),
        ProviderKind::Zai => Some(OpenAiCompatConfig::zai()),
        ProviderKind::OpenCode => Some(OpenAiCompatConfig::opencode()),
        ProviderKind::OpenCodeGo => Some(OpenAiCompatConfig::opencode_go()),
        ProviderKind::Alibaba => Some(OpenAiCompatConfig::alibaba()),
        ProviderKind::Antigravity => Some(OpenAiCompatConfig::antigravity()),
        ProviderKind::Cursor => Some(OpenAiCompatConfig::cursor()),
        // Bespoke clients
        ProviderKind::AnvilApi
        | ProviderKind::Ollama
        | ProviderKind::Copilot
        | ProviderKind::Azure
        | ProviderKind::Bedrock => None,
    }
}

#[derive(Debug, Clone)]
pub enum ProviderClient {
    AnvilApi(AnvilApiClient),
    /// All OpenAI-compatible providers (Group B + Gemini + xAI + OpenAI + Ollama).
    OpenAiCompat(OpenAiCompatClient, ProviderKind),
    Ollama(OllamaClient),
    Copilot(CopilotClient),
    Azure(AzureOpenAiClient),
    Bedrock(BedrockClient),
}

impl ProviderClient {
    pub fn from_model(model: &str) -> Result<Self, ApiError> {
        Self::from_model_with_default_auth(model, None)
    }

    pub fn from_model_with_default_auth(
        model: &str,
        default_auth: Option<AuthSource>,
    ) -> Result<Self, ApiError> {
        let resolved_model = providers::resolve_model_alias(model);
        let kind = providers::detect_provider_kind(&resolved_model);

        match kind {
            ProviderKind::AnvilApi => Ok(Self::AnvilApi(match default_auth {
                Some(auth) => AnvilApiClient::from_auth(auth),
                None => AnvilApiClient::from_env()?,
            })),
            ProviderKind::Ollama => Ok(Self::Ollama(OllamaClient::from_env())),
            ProviderKind::Copilot => Ok(Self::Copilot(CopilotClient::from_env()?)),
            ProviderKind::Azure => Ok(Self::Azure(AzureOpenAiClient::from_env()?)),
            ProviderKind::Bedrock => Ok(Self::Bedrock(BedrockClient::from_env()?)),
            other => {
                if let Some(config) = openai_compat_config(other) {
                    // LM Studio and Ollama local have no auth env — use new_no_auth.
                    let client = if config.api_key_env.is_empty() {
                        OpenAiCompatClient::new_no_auth(
                            openai_compat::read_base_url(config),
                        )
                    } else {
                        OpenAiCompatClient::from_env(config)?
                    };
                    Ok(Self::OpenAiCompat(client, other))
                } else {
                    Err(ApiError::Auth(format!(
                        "no client implementation for provider {other:?}"
                    )))
                }
            }
        }
    }

    #[must_use]
    pub const fn provider_kind(&self) -> ProviderKind {
        match self {
            Self::AnvilApi(_) => ProviderKind::AnvilApi,
            Self::OpenAiCompat(_, kind) => *kind,
            Self::Ollama(_) => ProviderKind::Ollama,
            Self::Copilot(_) => ProviderKind::Copilot,
            Self::Azure(_) => ProviderKind::Azure,
            Self::Bedrock(_) => ProviderKind::Bedrock,
        }
    }

    pub async fn send_message(
        &self,
        request: &MessageRequest,
    ) -> Result<MessageResponse, ApiError> {
        match self {
            Self::AnvilApi(client) => send_via_provider(client, request).await,
            Self::OpenAiCompat(client, _) => send_via_provider(client, request).await,
            Self::Ollama(client) => send_via_provider(client, request).await,
            Self::Copilot(client) => send_via_provider(client, request).await,
            Self::Azure(client) => send_via_provider(client, request).await,
            Self::Bedrock(client) => send_via_provider(client, request).await,
        }
    }

    pub async fn stream_message(
        &self,
        request: &MessageRequest,
    ) -> Result<MessageStream, ApiError> {
        match self {
            Self::AnvilApi(client) => stream_via_provider(client, request)
                .await
                .map(MessageStream::AnvilApi),
            Self::OpenAiCompat(client, _) => stream_via_provider(client, request)
                .await
                .map(|s| MessageStream::OpenAiCompat(s)),
            Self::Ollama(client) => stream_via_provider(client, request)
                .await
                .map(|s| MessageStream::OpenAiCompat(s)),
            Self::Copilot(client) => stream_via_provider(client, request)
                .await
                .map(|s| MessageStream::OpenAiCompat(s)),
            Self::Azure(client) => stream_via_provider(client, request)
                .await
                .map(|s| MessageStream::AzureStream(s)),
            Self::Bedrock(client) => stream_via_provider(client, request)
                .await
                .map(|s| MessageStream::BedrockStream(s)),
        }
    }
}

#[derive(Debug)]
pub enum MessageStream {
    AnvilApi(anvil_provider::MessageStream),
    OpenAiCompat(openai_compat::MessageStream),
    AzureStream(crate::providers::azure::AzureMessageStream),
    BedrockStream(crate::providers::bedrock::BedrockMessageStream),
}

impl MessageStream {
    #[must_use]
    pub fn request_id(&self) -> Option<&str> {
        match self {
            Self::AnvilApi(stream) => stream.request_id(),
            Self::OpenAiCompat(stream) => stream.request_id(),
            Self::AzureStream(stream) => stream.request_id(),
            Self::BedrockStream(_) => None,
        }
    }

    pub async fn next_event(&mut self) -> Result<Option<StreamEvent>, ApiError> {
        match self {
            Self::AnvilApi(stream) => stream.next_event().await,
            Self::OpenAiCompat(stream) => stream.next_event().await,
            Self::AzureStream(stream) => stream.next_event().await,
            Self::BedrockStream(stream) => stream.next_event().await,
        }
    }
}

pub use anvil_provider::{
    oauth_token_is_expired, resolve_saved_oauth_token, resolve_startup_auth_source, OAuthTokenSet,
};
#[must_use]
pub fn read_base_url() -> String {
    anvil_provider::read_base_url()
}

#[must_use]
pub fn read_xai_base_url() -> String {
    openai_compat::read_base_url(OpenAiCompatConfig::xai())
}

#[must_use]
pub fn read_ollama_base_url() -> String {
    std::env::var(ollama::OLLAMA_HOST_ENV)
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| ollama::DEFAULT_OLLAMA_BASE_URL.to_string())
}

// ---------------------------------------------------------------------------
// FailoverClient — wraps FailoverChain + ProviderClient with automatic
// provider switching on rate-limit (429) responses.
// ---------------------------------------------------------------------------

/// Callback invoked when a failover event occurs.  The caller receives the
/// formatted notification string and can display it in the TUI.
pub type FailoverNotify = Box<dyn Fn(&str) + Send + Sync>;

pub struct FailoverClient {
    chain: FailoverChain,
    notify: Option<FailoverNotify>,
}

impl FailoverClient {
    /// Build from `~/.anvil/failover.json`.  Notification callback is optional.
    #[must_use]
    pub fn from_config_file(notify: Option<FailoverNotify>) -> Self {
        Self {
            chain: FailoverChain::from_config_file(),
            notify,
        }
    }

    #[must_use]
    pub fn new(chain: FailoverChain, notify: Option<FailoverNotify>) -> Self {
        Self { chain, notify }
    }

    /// The model string for the currently active provider in the chain.
    #[must_use]
    pub fn active_model(&self) -> Option<&str> {
        self.chain.active_model()
    }

    /// Borrow the inner chain (for status display / management commands).
    #[must_use]
    pub const fn chain(&self) -> &FailoverChain {
        &self.chain
    }

    /// Borrow the inner chain mutably.
    pub const fn chain_mut(&mut self) -> &mut FailoverChain {
        &mut self.chain
    }

    /// Send a message using the active provider, failing over on 429.
    pub async fn send_message(
        &mut self,
        request: &MessageRequest,
    ) -> Result<MessageResponse, ApiError> {
        loop {
            let Some(idx) = self.chain.select_provider() else {
                return Err(ApiError::Auth(
                    "All providers in the failover chain are unavailable".to_string(),
                ));
            };
            let model = self
                .chain
                .model_at(idx)
                .ok_or_else(|| {
                    ApiError::Auth("Failover chain index out of range".to_string())
                })?
                .to_string();
            let client = ProviderClient::from_model(&model)?;

            match client.send_message(request).await {
                Ok(response) => return Ok(response),
                Err(ApiError::Api { status, .. }) if status.as_u16() == 429 => {
                    let event = self.chain.on_rate_limited(idx, None);
                    self.maybe_notify(event);
                }
                Err(other) => return Err(other),
            }
        }
    }

    /// Stream a message using the active provider, failing over on 429.
    pub async fn stream_message(
        &mut self,
        request: &MessageRequest,
    ) -> Result<MessageStream, ApiError> {
        loop {
            let Some(idx) = self.chain.select_provider() else {
                return Err(ApiError::Auth(
                    "All providers in the failover chain are unavailable".to_string(),
                ));
            };
            let model = self
                .chain
                .model_at(idx)
                .ok_or_else(|| {
                    ApiError::Auth("Failover chain index out of range".to_string())
                })?
                .to_string();
            let client = ProviderClient::from_model(&model)?;

            match client.stream_message(request).await {
                Ok(stream) => return Ok(stream),
                Err(ApiError::Api { status, .. }) if status.as_u16() == 429 => {
                    let event = self.chain.on_rate_limited(idx, None);
                    self.maybe_notify(event);
                }
                Err(other) => return Err(other),
            }
        }
    }

    fn maybe_notify(&self, event: Option<FailoverEvent>) {
        if let (Some(notify), Some(ev)) = (self.notify.as_ref(), event) {
            notify(&format_failover_event(&ev));
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::providers::{detect_provider_kind, resolve_model_alias, ProviderKind};

    #[test]
    fn resolves_existing_and_grok_aliases() {
        assert_eq!(resolve_model_alias("opus"), "claude-opus-4-6");
        assert_eq!(resolve_model_alias("grok"), "grok-3");
        assert_eq!(resolve_model_alias("grok-mini"), "grok-3-mini");
    }

    #[test]
    fn provider_detection_prefers_model_family() {
        assert_eq!(detect_provider_kind("grok-3"), ProviderKind::Xai);
        assert_eq!(
            detect_provider_kind("claude-sonnet-4-6"),
            ProviderKind::AnvilApi
        );
    }
}
