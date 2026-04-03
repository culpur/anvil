use crate::error::ApiError;
use crate::failover::{FailoverChain, FailoverEvent, format_failover_event};
use crate::providers::anvil_provider::{self, AuthSource, AnvilApiClient};
use crate::providers::ollama::{self, OllamaClient};
use crate::providers::openai_compat::{self, OpenAiCompatClient, OpenAiCompatConfig};
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

#[derive(Debug, Clone)]
pub enum ProviderClient {
    AnvilApi(AnvilApiClient),
    Xai(OpenAiCompatClient),
    OpenAi(OpenAiCompatClient),
    Ollama(OllamaClient),
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
        match providers::detect_provider_kind(&resolved_model) {
            ProviderKind::AnvilApi => Ok(Self::AnvilApi(match default_auth {
                Some(auth) => AnvilApiClient::from_auth(auth),
                None => AnvilApiClient::from_env()?,
            })),
            ProviderKind::Xai => Ok(Self::Xai(OpenAiCompatClient::from_env(
                OpenAiCompatConfig::xai(),
            )?)),
            ProviderKind::OpenAi => Ok(Self::OpenAi(OpenAiCompatClient::from_env(
                OpenAiCompatConfig::openai(),
            )?)),
            ProviderKind::Ollama => Ok(Self::Ollama(OllamaClient::from_env())),
        }
    }

    #[must_use]
    pub const fn provider_kind(&self) -> ProviderKind {
        match self {
            Self::AnvilApi(_) => ProviderKind::AnvilApi,
            Self::Xai(_) => ProviderKind::Xai,
            Self::OpenAi(_) => ProviderKind::OpenAi,
            Self::Ollama(_) => ProviderKind::Ollama,
        }
    }

    pub async fn send_message(
        &self,
        request: &MessageRequest,
    ) -> Result<MessageResponse, ApiError> {
        match self {
            Self::AnvilApi(client) => send_via_provider(client, request).await,
            Self::Xai(client) | Self::OpenAi(client) => send_via_provider(client, request).await,
            Self::Ollama(client) => send_via_provider(client, request).await,
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
            Self::Xai(client) | Self::OpenAi(client) => stream_via_provider(client, request)
                .await
                .map(MessageStream::OpenAiCompat),
            Self::Ollama(client) => stream_via_provider(client, request)
                .await
                .map(MessageStream::OpenAiCompat),
        }
    }
}

// Ollama uses the same OpenAI-compatible stream type — no separate variant needed.
#[derive(Debug)]
pub enum MessageStream {
    AnvilApi(anvil_provider::MessageStream),
    OpenAiCompat(openai_compat::MessageStream),
}

impl MessageStream {
    #[must_use]
    pub fn request_id(&self) -> Option<&str> {
        match self {
            Self::AnvilApi(stream) => stream.request_id(),
            Self::OpenAiCompat(stream) => stream.request_id(),
        }
    }

    pub async fn next_event(&mut self) -> Result<Option<StreamEvent>, ApiError> {
        match self {
            Self::AnvilApi(stream) => stream.next_event().await,
            Self::OpenAiCompat(stream) => stream.next_event().await,
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
        // FailoverChain::active_model takes &self — safe.
        self.chain.active_model()
    }

    /// Borrow the inner chain (for status display / management commands).
    #[must_use]
    pub fn chain(&self) -> &FailoverChain {
        &self.chain
    }

    /// Borrow the inner chain mutably.
    pub fn chain_mut(&mut self) -> &mut FailoverChain {
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
