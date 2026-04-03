mod client;
mod error;
pub mod failover;
mod providers;
mod sse;
mod types;

pub use client::{
    oauth_token_is_expired, read_base_url, read_ollama_base_url, read_xai_base_url,
    resolve_saved_oauth_token, resolve_startup_auth_source, FailoverClient, FailoverNotify,
    MessageStream, OAuthTokenSet, ProviderClient,
};
pub use error::ApiError;
pub use failover::{
    format_failover_event, FailoverChain, FailoverConfig, FailoverEntry, FailoverEvent,
    FailoverEventKind, UsageBudget,
};
pub use providers::anvil_provider::{AuthSource, AnvilApiClient, AnvilApiClient as ApiClient};
pub use providers::ollama::OllamaClient;
pub use providers::openai_compat::{OpenAiCompatClient, OpenAiCompatConfig};
pub use providers::{
    detect_provider_kind, max_tokens_for_model, provider_display_name, resolve_model_alias,
    ProviderKind,
};
pub use sse::{parse_frame, SseParser};
pub use types::{
    ContentBlockDelta, ContentBlockDeltaEvent, ContentBlockStartEvent, ContentBlockStopEvent,
    ImageSource, ImageSourceKind, InputContentBlock, InputMessage, MessageDelta, MessageDeltaEvent,
    MessageRequest, MessageResponse, MessageStartEvent, MessageStopEvent, OutputContentBlock,
    StreamEvent, ToolChoice, ToolDefinition, ToolResultContentBlock, Usage,
};
