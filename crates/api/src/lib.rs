mod client;
mod error;
pub mod failover;
pub mod ollama_tune;
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
pub use providers::ollama::{cloud_model_context_window, is_ollama_cloud_model, OllamaClient};
pub use providers::ollama_manage::{
    copy_model, default_modelfile_template, delete_model, evaluate_rm_confirmation,
    extract_tag_names, list_installed_models, modelfile_is_effectively_empty,
    parse_pull_progress_line, stream_progress, OllamaManageError, PullProgress, RmConfirmation,
    StreamOutcome,
};
pub use providers::ollama_registry::{
    available_quants, fetch_tag_manifest_size, list_registry_tags, normalize_quant,
    parse_tag_components, pick_best_match, RegistryError, RegistryTag,
};
pub use providers::ollama_show::{
    fetch_model_meta_cached, fetch_models_list, fetch_running_models, Architecture, ModelMeta,
    ModelMetaError, OllamaModel, OllamaModelDetails, Quantization, RunningModel,
};
pub use providers::ollama_tool_parser::{
    parse_ollama_text_for_tool_calls, silent_write_warning, ExtractedToolCall, OllamaParseResult,
    ParseSource, SilentWriteDetection,
};
pub use ollama_tune::{OllamaConfig, OllamaModelOverride};
pub use providers::openai_compat::{OpenAiCompatClient, OpenAiCompatConfig};
pub use providers::{
    detect_provider_kind, max_tokens_for_model, provider_display_name, resolve_model_alias,
    ProviderKind,
};
pub use sse::{parse_frame, SseParser};
pub use types::{
    CacheControl, CacheControlKind, ContentBlockDelta, ContentBlockDeltaEvent,
    ContentBlockStartEvent, ContentBlockStopEvent, ImageSource, ImageSourceKind, InputContentBlock,
    InputMessage, MessageDelta, MessageDeltaEvent, MessageRequest, MessageResponse,
    MessageStartEvent, MessageStopEvent, OutputContentBlock, StreamEvent, ToolChoice,
    ToolDefinition, ToolResultContentBlock, Usage,
};
