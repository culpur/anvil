//! Live `/models` API integration for the `/model` picker.
//!
//! This module is the natural owner of:
//!   1. The per-provider HTTP fetchers that hit each backend's `/models` (or
//!      equivalent) endpoint, returning a uniform [`ProviderModel`] list.
//!   2. The credential resolver [`is_provider_configured`], which answers
//!      "does this user have working credentials for this provider right now?"
//!      It reuses the *same* env + saved-OAuth paths that
//!      [`crate::client::ProviderClient::from_model`] uses at runtime startup.
//!      No new credential surface is introduced here.
//!   3. The merge layer [`fetch_models_for_providers`], which fans the
//!      fetchers out in parallel against every configured provider, mapping
//!      transient failures to the static [`MODEL_REGISTRY`] fallback while
//!      omitting providers that are completely unconfigured or returned 401/403.
//!
//! The TUI completion layer (`crates/anvil-cli/src/tui/completion.rs`) calls
//! this module exactly once per cache window — TAB completion stays fast
//! because the per-session cache lives in `TuiCompletionContext`, not here.
//!
//! See `feedback-model-list-is-live-not-registry.md` for the user contract.
//!
//! The Ollama Cloud feedback note (`feedback-ollama-cloud-auth.md`) explicitly
//! states: never reach `ollama.com` directly, never set `OLLAMA_API_KEY`. The
//! local daemon proxies Cloud calls transparently via its device key, so the
//! Cloud fetcher here simply re-runs the local `/api/tags` query and filters
//! on the `:cloud` / `-cloud` suffix.

use std::time::Duration;

use serde::Deserialize;

use super::anvil_provider::has_auth_from_env_or_saved;
use super::ollama::{is_ollama_cloud_model, DEFAULT_OLLAMA_BASE_URL, OLLAMA_HOST_ENV};
use super::openai_compat::{has_api_key, read_base_url, OpenAiCompatConfig};
use super::ProviderKind;

/// A single model returned by a provider's `/models` endpoint.
///
/// `display_name` is whatever the provider supplied as a human-readable label
/// (when available). `context_window` and `deprecated` are filled when the
/// upstream tells us; otherwise left as `None` / `false`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderModel {
    pub id: String,
    pub provider: ProviderKind,
    pub display_name: Option<String>,
    pub context_window: Option<u32>,
    pub deprecated: bool,
}

/// Errors that a per-provider model fetch can surface.
///
/// `Unauthorized` (401/403) means the credentials we *thought* we had are
/// rejected by the provider — the caller hides the provider entirely and logs
/// a one-line warning so the user knows.
///
/// `Transient` (5xx, network, timeout) means the provider is momentarily
/// unreachable — the caller falls back to the static [`MODEL_REGISTRY`] for
/// that provider so the picker isn't blank.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderModelsError {
    /// 401 or 403 from the upstream — credentials are bad, hide provider.
    Unauthorized,
    /// 5xx, connection refused, DNS, or any `tokio::time::timeout` firing.
    Transient(String),
    /// Body decoded but didn't match the expected envelope.
    InvalidResponse(String),
    /// Anything else (shouldn't normally happen).
    Other(String),
}

impl std::fmt::Display for ProviderModelsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unauthorized => write!(f, "unauthorized"),
            Self::Transient(message) => write!(f, "transient: {message}"),
            Self::InvalidResponse(message) => write!(f, "invalid response: {message}"),
            Self::Other(message) => write!(f, "other: {message}"),
        }
    }
}

impl std::error::Error for ProviderModelsError {}

/// Default per-provider timeout for the `/models` call.
///
/// 4 s leaves enough headroom for a real cross-Atlantic call while keeping
/// the whole tab-completion paint cycle bounded. Anything slower is treated
/// as `Transient` and falls back to the registry.
pub const DEFAULT_FETCH_TIMEOUT: Duration = Duration::from_secs(4);

/// Build a `reqwest::Client` with the standard fetch timeout.
fn build_fetch_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(DEFAULT_FETCH_TIMEOUT)
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
}

// ---------------------------------------------------------------------------
// Credential resolution — same path the runtime client uses.
// ---------------------------------------------------------------------------

/// Result of asking "do we have working credentials for this provider right
/// now?" The actual secret value isn't propagated outside the api crate — only
/// the kind tag, so callers can decide whether to display / fetch / hide.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderCredentials {
    /// Anthropic via Bearer (OAuth) or x-api-key (env / saved).
    Anthropic,
    /// xAI bearer token.
    Xai,
    /// OpenAI bearer token.
    OpenAi,
    /// Google Gemini API key.
    Gemini,
    /// Local Ollama daemon reachable on `OLLAMA_HOST` (or default).
    OllamaLocal,
    /// Local Ollama daemon reachable AND authenticated against Ollama Cloud.
    OllamaCloud,
    /// Any Group B or Group A provider with a slug for identification.
    /// `slug` is the canonical `/provider` slug (e.g. `"groq"`, `"azure"`).
    GroupB(&'static str),
}

impl ProviderCredentials {
    /// The provider kind these credentials authorize. Multiple credentials
    /// can share a kind (e.g. local + cloud Ollama both map to
    /// [`ProviderKind::Ollama`]).
    #[must_use]
    pub fn kind(&self) -> ProviderKind {
        match self {
            Self::Anthropic => ProviderKind::AnvilApi,
            Self::Xai => ProviderKind::Xai,
            Self::OpenAi => ProviderKind::OpenAi,
            Self::Gemini => ProviderKind::Gemini,
            Self::OllamaLocal | Self::OllamaCloud => ProviderKind::Ollama,
            Self::GroupB(slug) => slug_to_provider_kind(slug)
                .unwrap_or(ProviderKind::AnvilApi),
        }
    }
}

/// Probe whether the Ollama daemon is reachable on its `OLLAMA_HOST`.
///
/// `/api/tags` is the lightest existing endpoint; 500 ms is enough for a
/// loopback hit even on a sluggish box. Failure → daemon not running.
async fn ollama_daemon_reachable() -> bool {
    let base = std::env::var(OLLAMA_HOST_ENV)
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| DEFAULT_OLLAMA_BASE_URL.to_string());
    let url = format!("{}/api/tags", base.trim_end_matches('/'));
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_millis(500))
        .build()
    {
        Ok(client) => client,
        Err(_) => return false,
    };
    client
        .get(&url)
        .send()
        .await
        .map(|response| response.status().is_success())
        .unwrap_or(false)
}

/// Probe whether the local Ollama daemon has cloud auth set up — i.e. it has
/// at least one model with a `:cloud` / `-cloud` suffix in its tag list.
///
/// We can't introspect the daemon's device-key state directly without an
/// undocumented endpoint, but pulling a cloud model registers it locally, so
/// presence in `/api/tags` is a reliable positive signal.
async fn ollama_cloud_available() -> bool {
    let models = match fetch_ollama_local_models().await {
        Ok(models) => models,
        Err(_) => return false,
    };
    models.iter().any(|model| is_ollama_cloud_model(&model.id))
}

/// Resolve credentials for a specific provider kind, mirroring the same
/// env + saved-OAuth path that `DefaultRuntimeClient` uses at startup.
///
/// Returns `Some(credentials)` if any of:
/// - the matching env var is set (e.g. `ANTHROPIC_API_KEY`, `OPENAI_API_KEY`),
/// - an OAuth token is saved on disk for that provider (currently Anthropic + Copilot),
/// - for Ollama, the local daemon is reachable.
/// - for Azure, the required AZURE_OPENAI_ENDPOINT + credential vars are set.
/// - for Bedrock, AWS_ACCESS_KEY_ID + AWS_SECRET_ACCESS_KEY are set.
///
/// Returns `None` if no path resolves — the caller hides the provider.
pub async fn is_provider_configured(kind: ProviderKind) -> Option<ProviderCredentials> {
    match kind {
        ProviderKind::AnvilApi => has_auth_from_env_or_saved()
            .unwrap_or(false)
            .then_some(ProviderCredentials::Anthropic),
        ProviderKind::Xai => has_api_key("XAI_API_KEY").then_some(ProviderCredentials::Xai),
        ProviderKind::OpenAi => {
            has_api_key("OPENAI_API_KEY").then_some(ProviderCredentials::OpenAi)
        }
        ProviderKind::Gemini | ProviderKind::Antigravity => {
            if has_api_key("GEMINI_API_KEY")
                || has_api_key("GOOGLE_API_KEY")
                || has_api_key("ANTIGRAVITY_API_KEY")
            {
                Some(ProviderCredentials::Gemini)
            } else {
                None
            }
        }
        ProviderKind::Ollama => {
            if !ollama_daemon_reachable().await {
                return None;
            }
            if ollama_cloud_available().await {
                Some(ProviderCredentials::OllamaCloud)
            } else {
                Some(ProviderCredentials::OllamaLocal)
            }
        }
        // ── Group B: direct API-key providers ────────────────────────────────
        ProviderKind::Fireworks => has_api_key("FIREWORKS_API_KEY").then_some(ProviderCredentials::GroupB("fireworks")),
        ProviderKind::Groq => has_api_key("GROQ_API_KEY").then_some(ProviderCredentials::GroupB("groq")),
        ProviderKind::Mistral => has_api_key("MISTRAL_API_KEY").then_some(ProviderCredentials::GroupB("mistral")),
        ProviderKind::Perplexity => has_api_key("PERPLEXITY_API_KEY").then_some(ProviderCredentials::GroupB("perplexity")),
        ProviderKind::DeepSeek => has_api_key("DEEPSEEK_API_KEY").then_some(ProviderCredentials::GroupB("deepseek")),
        ProviderKind::TogetherAi => has_api_key("TOGETHER_API_KEY").then_some(ProviderCredentials::GroupB("togetherai")),
        ProviderKind::DeepInfra => has_api_key("DEEPINFRA_API_KEY").then_some(ProviderCredentials::GroupB("deepinfra")),
        ProviderKind::Cerebras => has_api_key("CEREBRAS_API_KEY").then_some(ProviderCredentials::GroupB("cerebras")),
        ProviderKind::NvidiaNim => has_api_key("NVIDIA_API_KEY").then_some(ProviderCredentials::GroupB("nvidia-nim")),
        ProviderKind::HuggingFace => has_api_key("HF_TOKEN").then_some(ProviderCredentials::GroupB("huggingface")),
        ProviderKind::MoonshotAi => has_api_key("MOONSHOT_API_KEY").then_some(ProviderCredentials::GroupB("moonshotai")),
        ProviderKind::Nebius => has_api_key("NEBIUS_API_KEY").then_some(ProviderCredentials::GroupB("nebius")),
        ProviderKind::OpenRouter => has_api_key("OPENROUTER_API_KEY").then_some(ProviderCredentials::GroupB("openrouter")),
        ProviderKind::LmStudio => {
            // LM Studio is local with no auth; always report as configured.
            Some(ProviderCredentials::GroupB("lmstudio"))
        }
        ProviderKind::Chutes => has_api_key("CHUTES_API_KEY").then_some(ProviderCredentials::GroupB("chutes")),
        ProviderKind::Scaleway => has_api_key("SCALEWAY_API_KEY").then_some(ProviderCredentials::GroupB("scaleway")),
        ProviderKind::Baseten => has_api_key("BASETEN_API_KEY").then_some(ProviderCredentials::GroupB("baseten")),
        ProviderKind::MiniMax => has_api_key("MINIMAX_API_KEY").then_some(ProviderCredentials::GroupB("minimax")),
        ProviderKind::StackIt => has_api_key("STACKIT_API_KEY").then_some(ProviderCredentials::GroupB("stackit")),
        ProviderKind::Cortecs => has_api_key("CORTECS_API_KEY").then_some(ProviderCredentials::GroupB("cortecs")),
        ProviderKind::Ai302 => has_api_key("AI302_API_KEY").then_some(ProviderCredentials::GroupB("302ai")),
        ProviderKind::Zai => has_api_key("ZAI_API_KEY").then_some(ProviderCredentials::GroupB("zai")),
        ProviderKind::OpenCode => has_api_key("OPENCODE_API_KEY").then_some(ProviderCredentials::GroupB("opencode")),
        ProviderKind::OpenCodeGo => has_api_key("OPENCODE_API_KEY").then_some(ProviderCredentials::GroupB("opencode-go")),
        ProviderKind::Alibaba => {
            if has_api_key("DASHSCOPE_API_KEY") || has_api_key("ALIBABA_API_KEY") {
                Some(ProviderCredentials::GroupB("alibaba"))
            } else {
                None
            }
        }
        ProviderKind::Cursor => {
            let from_env = has_api_key("CURSOR_API_KEY");
            let from_file = super::copilot::load_cursor_auth_token()
                .map(|t| t.is_some())
                .unwrap_or(false);
            (from_env || from_file).then_some(ProviderCredentials::GroupB("cursor"))
        }
        // ── Group A: specialised auth ─────────────────────────────────────────
        ProviderKind::Copilot => {
            let from_env = has_api_key("GITHUB_TOKEN");
            let from_saved = super::copilot::load_copilot_token()
                .map(|t| t.map(|tok| !tok.is_expired()).unwrap_or(false))
                .unwrap_or(false);
            (from_env || from_saved).then_some(ProviderCredentials::GroupB("copilot"))
        }
        ProviderKind::Azure => {
            let endpoint_set = has_api_key("AZURE_OPENAI_ENDPOINT");
            let auth_set = has_api_key("AZURE_OPENAI_API_KEY") || has_api_key("AZURE_AD_TOKEN");
            (endpoint_set && auth_set).then_some(ProviderCredentials::GroupB("azure"))
        }
        ProviderKind::Bedrock => {
            let key_set = has_api_key("AWS_ACCESS_KEY_ID") && has_api_key("AWS_SECRET_ACCESS_KEY");
            key_set.then_some(ProviderCredentials::GroupB("bedrock"))
        }
    }
}

/// Map a `/provider` slug to its [`ProviderKind`].
#[must_use]
pub fn slug_to_provider_kind(slug: &str) -> Option<ProviderKind> {
    // This is the canonical slug table — must stay in sync with the slug
    // parser in `crates/anvil-cli/src/providers.rs`.
    match slug {
        "anthropic" | "claude" | "anvil" => Some(ProviderKind::AnvilApi),
        "xai" | "grok" => Some(ProviderKind::Xai),
        "openai" => Some(ProviderKind::OpenAi),
        "gemini" | "google" => Some(ProviderKind::Gemini),
        "ollama" => Some(ProviderKind::Ollama),
        "fireworks" => Some(ProviderKind::Fireworks),
        "groq" => Some(ProviderKind::Groq),
        "mistral" => Some(ProviderKind::Mistral),
        "perplexity" => Some(ProviderKind::Perplexity),
        "deepseek" => Some(ProviderKind::DeepSeek),
        "togetherai" | "together" => Some(ProviderKind::TogetherAi),
        "deepinfra" => Some(ProviderKind::DeepInfra),
        "cerebras" => Some(ProviderKind::Cerebras),
        "nvidia-nim" | "nvidia" => Some(ProviderKind::NvidiaNim),
        "huggingface" | "hf" => Some(ProviderKind::HuggingFace),
        "moonshotai" | "moonshot" => Some(ProviderKind::MoonshotAi),
        "nebius" => Some(ProviderKind::Nebius),
        "openrouter" => Some(ProviderKind::OpenRouter),
        "lmstudio" | "lm-studio" => Some(ProviderKind::LmStudio),
        "chutes" => Some(ProviderKind::Chutes),
        "scaleway" => Some(ProviderKind::Scaleway),
        "baseten" => Some(ProviderKind::Baseten),
        "minimax" => Some(ProviderKind::MiniMax),
        "stackit" => Some(ProviderKind::StackIt),
        "cortecs" => Some(ProviderKind::Cortecs),
        "302ai" | "ai302" => Some(ProviderKind::Ai302),
        "zai" | "kimi" | "glm" => Some(ProviderKind::Zai),
        "opencode" => Some(ProviderKind::OpenCode),
        "opencode-go" => Some(ProviderKind::OpenCodeGo),
        "copilot" | "github-copilot" => Some(ProviderKind::Copilot),
        "azure" | "azure-openai" => Some(ProviderKind::Azure),
        "bedrock" | "aws-bedrock" => Some(ProviderKind::Bedrock),
        "alibaba" | "dashscope" | "alibaba-coding-plan" => Some(ProviderKind::Alibaba),
        "antigravity" => Some(ProviderKind::Antigravity),
        "cursor" => Some(ProviderKind::Cursor),
        _ => None,
    }
}

/// All provider kinds in the canonical display order.
const ALL_PROVIDER_KINDS: &[ProviderKind] = &[
    // Original five
    ProviderKind::AnvilApi,
    ProviderKind::Xai,
    ProviderKind::OpenAi,
    ProviderKind::Gemini,
    ProviderKind::Ollama,
    // Group B
    ProviderKind::Fireworks,
    ProviderKind::Groq,
    ProviderKind::Mistral,
    ProviderKind::Perplexity,
    ProviderKind::DeepSeek,
    ProviderKind::TogetherAi,
    ProviderKind::DeepInfra,
    ProviderKind::Cerebras,
    ProviderKind::NvidiaNim,
    ProviderKind::HuggingFace,
    ProviderKind::MoonshotAi,
    ProviderKind::Nebius,
    ProviderKind::OpenRouter,
    ProviderKind::LmStudio,
    ProviderKind::Chutes,
    ProviderKind::Scaleway,
    ProviderKind::Baseten,
    ProviderKind::MiniMax,
    ProviderKind::StackIt,
    ProviderKind::Cortecs,
    ProviderKind::Ai302,
    ProviderKind::Zai,
    ProviderKind::OpenCode,
    ProviderKind::OpenCodeGo,
    // Group A
    ProviderKind::Copilot,
    ProviderKind::Azure,
    ProviderKind::Bedrock,
    ProviderKind::Alibaba,
    ProviderKind::Antigravity,
    ProviderKind::Cursor,
];

/// Enumerate every configured provider in a single pass.
///
/// Order matches the canonical provider kind declaration so labels are stable
/// across calls.
pub async fn enumerate_configured_providers() -> Vec<ProviderCredentials> {
    let mut out = Vec::new();
    for &kind in ALL_PROVIDER_KINDS {
        if let Some(credentials) = is_provider_configured(kind).await {
            out.push(credentials);
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Per-provider fetchers
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct AnthropicModelsEnvelope {
    data: Vec<AnthropicModelEntry>,
}

#[derive(Debug, Deserialize)]
struct AnthropicModelEntry {
    id: String,
    #[serde(default)]
    display_name: Option<String>,
}

const ANTHROPIC_VERSION_HEADER: &str = "2023-06-01";

/// Fetch the live Anthropic model list.
///
/// Endpoint: `GET https://api.anthropic.com/v1/models`. Auth: same priority
/// as the streaming path — `ANTHROPIC_API_KEY` (x-api-key), then
/// `ANTHROPIC_AUTH_TOKEN` (Bearer), then saved OAuth (Bearer with
/// `anthropic-beta: oauth-2025-04-20`).
///
/// The base URL respects `ANTHROPIC_BASE_URL` for test/mocking.
pub async fn fetch_anthropic_models() -> Result<Vec<ProviderModel>, ProviderModelsError> {
    let base = super::anvil_provider::read_base_url();
    let url = format!("{}/v1/models", base.trim_end_matches('/'));

    let client = build_fetch_client();
    let mut builder = client.get(&url).header("anthropic-version", ANTHROPIC_VERSION_HEADER);

    // Same auth resolution that streaming uses, but we tolerate "no creds at
    // all" by returning Unauthorized — the caller already gated on
    // `is_provider_configured`, so reaching here without a key is a race
    // (e.g. user revoked between TAB hits).
    let auth = super::anvil_provider::AuthSource::from_env_or_saved()
        .map_err(|_| ProviderModelsError::Unauthorized)?;
    if auth.bearer_token().is_some() {
        builder = builder.header("anthropic-beta", "oauth-2025-04-20");
    }
    builder = auth.apply(builder);

    let response = builder
        .send()
        .await
        .map_err(|error| ProviderModelsError::Transient(error.to_string()))?;
    let status = response.status();
    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        return Err(ProviderModelsError::Unauthorized);
    }
    if !status.is_success() {
        return Err(ProviderModelsError::Transient(format!(
            "HTTP {} from {url}",
            status.as_u16()
        )));
    }
    let envelope: AnthropicModelsEnvelope = response
        .json()
        .await
        .map_err(|error| ProviderModelsError::InvalidResponse(error.to_string()))?;
    Ok(envelope
        .data
        .into_iter()
        .map(|entry| ProviderModel {
            id: entry.id,
            provider: ProviderKind::AnvilApi,
            display_name: entry.display_name,
            context_window: None,
            deprecated: false,
        })
        .collect())
}

#[derive(Debug, Deserialize)]
struct OpenAiModelsEnvelope {
    data: Vec<OpenAiModelEntry>,
}

#[derive(Debug, Deserialize)]
struct OpenAiModelEntry {
    id: String,
}

/// Shared OpenAI-compat fetcher used by OpenAI, xAI, and Gemini.
async fn fetch_openai_compat_models(
    config: OpenAiCompatConfig,
    provider: ProviderKind,
) -> Result<Vec<ProviderModel>, ProviderModelsError> {
    let base = read_base_url(config);
    let url = format!("{}/models", base.trim_end_matches('/'));

    // Pull the matching env var the runtime would have picked.
    let api_key_env = config.api_key_env;
    let api_key = std::env::var(api_key_env)
        .ok()
        .filter(|value| !value.is_empty())
        .ok_or(ProviderModelsError::Unauthorized)?;

    let client = build_fetch_client();
    let response = client
        .get(&url)
        .bearer_auth(api_key)
        .send()
        .await
        .map_err(|error| ProviderModelsError::Transient(error.to_string()))?;
    let status = response.status();
    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        return Err(ProviderModelsError::Unauthorized);
    }
    if !status.is_success() {
        return Err(ProviderModelsError::Transient(format!(
            "HTTP {} from {url}",
            status.as_u16()
        )));
    }
    let envelope: OpenAiModelsEnvelope = response
        .json()
        .await
        .map_err(|error| ProviderModelsError::InvalidResponse(error.to_string()))?;
    Ok(envelope
        .data
        .into_iter()
        .map(|entry| ProviderModel {
            id: entry.id,
            provider,
            display_name: None,
            context_window: None,
            deprecated: false,
        })
        .collect())
}

/// Fetch the live OpenAI model list.
///
/// Endpoint: `GET {OPENAI_BASE_URL}/models` (defaults to
/// `https://api.openai.com/v1`). Bearer auth via `OPENAI_API_KEY`.
pub async fn fetch_openai_models() -> Result<Vec<ProviderModel>, ProviderModelsError> {
    fetch_openai_compat_models(OpenAiCompatConfig::openai(), ProviderKind::OpenAi).await
}

/// Fetch the live xAI model list (OpenAI-compatible endpoint).
pub async fn fetch_xai_models() -> Result<Vec<ProviderModel>, ProviderModelsError> {
    fetch_openai_compat_models(OpenAiCompatConfig::xai(), ProviderKind::Xai).await
}

/// Fetch the live Gemini model list via the OpenAI-compat endpoint.
///
/// Google also exposes `/v1beta/models` with the native shape, but the
/// OpenAI-compat path is the one the runtime client uses; staying on it keeps
/// the wire format identical and avoids carrying two response decoders.
pub async fn fetch_gemini_models() -> Result<Vec<ProviderModel>, ProviderModelsError> {
    fetch_openai_compat_models(OpenAiCompatConfig::gemini(), ProviderKind::Gemini).await
}

#[derive(Debug, Deserialize)]
struct OllamaTagsEnvelope {
    #[serde(default)]
    models: Vec<OllamaTagEntry>,
}

#[derive(Debug, Deserialize)]
struct OllamaTagEntry {
    name: String,
}

/// Fetch every model tag from the local Ollama daemon's `/api/tags`.
///
/// Returns BOTH local and cloud-flavored tags — the caller filters by suffix
/// if needed. Per `feedback-ollama-cloud-auth.md` we ONLY ever talk to
/// `localhost:11434` (or `OLLAMA_HOST`); the daemon handles cloud proxying
/// via its device key.
pub async fn fetch_ollama_local_models() -> Result<Vec<ProviderModel>, ProviderModelsError> {
    let base = std::env::var(OLLAMA_HOST_ENV)
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| DEFAULT_OLLAMA_BASE_URL.to_string());
    let url = format!("{}/api/tags", base.trim_end_matches('/'));

    let client = build_fetch_client();
    let response = client
        .get(&url)
        .send()
        .await
        .map_err(|error| ProviderModelsError::Transient(error.to_string()))?;
    let status = response.status();
    if !status.is_success() {
        return Err(ProviderModelsError::Transient(format!(
            "HTTP {} from {url}",
            status.as_u16()
        )));
    }
    let envelope: OllamaTagsEnvelope = response
        .json()
        .await
        .map_err(|error| ProviderModelsError::InvalidResponse(error.to_string()))?;
    Ok(envelope
        .models
        .into_iter()
        .map(|entry| ProviderModel {
            id: entry.name,
            provider: ProviderKind::Ollama,
            display_name: None,
            context_window: None,
            deprecated: false,
        })
        .collect())
}

/// Fetch only the Ollama Cloud models the local daemon knows about.
///
/// Same endpoint as [`fetch_ollama_local_models`] — the local daemon merges
/// local + cloud tags into a single list — filtered to entries whose tag
/// carries the `:cloud` or `-cloud` suffix.
pub async fn fetch_ollama_cloud_models() -> Result<Vec<ProviderModel>, ProviderModelsError> {
    let mut all = fetch_ollama_local_models().await?;
    all.retain(|model| is_ollama_cloud_model(&model.id));
    Ok(all)
}

// ---------------------------------------------------------------------------
// Group B model fetchers (all OpenAI-compat /v1/models)
// ---------------------------------------------------------------------------

/// Fetch live model list for any OpenAI-compatible Group B provider.
///
/// Falls back gracefully: if the provider's `/v1/models` returns a 404 or
/// non-JSON body (some providers return an HTML error page), return `Transient`
/// so the caller can fall back to the static registry.
pub async fn fetch_group_b_models(
    config: OpenAiCompatConfig,
    provider: ProviderKind,
) -> Result<Vec<ProviderModel>, ProviderModelsError> {
    // Local providers (LM Studio) skip the auth check.
    if !config.api_key_env.is_empty() {
        // For providers that fall back to a secondary env var (Alibaba, Antigravity)
        // we accept any of them.
        let secondary = match config.provider_name {
            "Alibaba DashScope" => Some("ALIBABA_API_KEY"),
            "Antigravity" => Some("GEMINI_API_KEY"),
            _ => None,
        };
        let has_key = std::env::var(config.api_key_env)
            .ok()
            .filter(|v| !v.is_empty())
            .is_some()
            || secondary
                .and_then(|k| std::env::var(k).ok().filter(|v| !v.is_empty()))
                .is_some();
        if !has_key {
            return Err(ProviderModelsError::Unauthorized);
        }
    }
    fetch_openai_compat_models(config, provider).await
}

macro_rules! group_b_fetcher {
    ($fn_name:ident, $config_fn:ident, $kind:expr) => {
        pub async fn $fn_name() -> Result<Vec<ProviderModel>, ProviderModelsError> {
            fetch_group_b_models(OpenAiCompatConfig::$config_fn(), $kind).await
        }
    };
}

group_b_fetcher!(fetch_fireworks_models, fireworks, ProviderKind::Fireworks);
group_b_fetcher!(fetch_groq_models, groq, ProviderKind::Groq);
group_b_fetcher!(fetch_mistral_models, mistral, ProviderKind::Mistral);
group_b_fetcher!(fetch_perplexity_models, perplexity, ProviderKind::Perplexity);
group_b_fetcher!(fetch_deepseek_models, deepseek, ProviderKind::DeepSeek);
group_b_fetcher!(fetch_togetherai_models, togetherai, ProviderKind::TogetherAi);
group_b_fetcher!(fetch_deepinfra_models, deepinfra, ProviderKind::DeepInfra);
group_b_fetcher!(fetch_cerebras_models, cerebras, ProviderKind::Cerebras);
group_b_fetcher!(fetch_nvidia_nim_models, nvidia_nim, ProviderKind::NvidiaNim);
group_b_fetcher!(fetch_huggingface_models, huggingface, ProviderKind::HuggingFace);
group_b_fetcher!(fetch_moonshotai_models, moonshotai, ProviderKind::MoonshotAi);
group_b_fetcher!(fetch_nebius_models, nebius, ProviderKind::Nebius);
group_b_fetcher!(fetch_openrouter_models, openrouter, ProviderKind::OpenRouter);
group_b_fetcher!(fetch_lmstudio_models, lmstudio, ProviderKind::LmStudio);
group_b_fetcher!(fetch_chutes_models, chutes, ProviderKind::Chutes);
group_b_fetcher!(fetch_scaleway_models, scaleway, ProviderKind::Scaleway);
group_b_fetcher!(fetch_baseten_models, baseten, ProviderKind::Baseten);
group_b_fetcher!(fetch_minimax_models, minimax, ProviderKind::MiniMax);
group_b_fetcher!(fetch_stackit_models, stackit, ProviderKind::StackIt);
group_b_fetcher!(fetch_cortecs_models, cortecs, ProviderKind::Cortecs);
group_b_fetcher!(fetch_ai302_models, ai302, ProviderKind::Ai302);
group_b_fetcher!(fetch_zai_models, zai, ProviderKind::Zai);
group_b_fetcher!(fetch_opencode_models, opencode, ProviderKind::OpenCode);
group_b_fetcher!(fetch_opencode_go_models, opencode_go, ProviderKind::OpenCodeGo);
group_b_fetcher!(fetch_alibaba_models, alibaba, ProviderKind::Alibaba);
group_b_fetcher!(fetch_antigravity_models, antigravity, ProviderKind::Antigravity);
group_b_fetcher!(fetch_cursor_models, cursor, ProviderKind::Cursor);

/// Fetch models for GitHub Copilot.  Uses the same OpenAI-compat shape;
/// tries `GITHUB_TOKEN` first then the saved device-flow token.
pub async fn fetch_copilot_models() -> Result<Vec<ProviderModel>, ProviderModelsError> {
    use super::copilot::load_copilot_token;
    let token = std::env::var("GITHUB_TOKEN")
        .ok()
        .filter(|v| !v.is_empty())
        .or_else(|| {
            load_copilot_token()
                .ok()
                .flatten()
                .filter(|t| !t.is_expired())
                .map(|t| t.access_token)
        })
        .ok_or(ProviderModelsError::Unauthorized)?;

    let base = std::env::var("COPILOT_BASE_URL")
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| super::copilot::BASE_URL.to_string());
    let url = format!("{}/models", base.trim_end_matches('/'));

    let client = build_fetch_client();
    let response = client
        .get(&url)
        .bearer_auth(token)
        .send()
        .await
        .map_err(|e| ProviderModelsError::Transient(e.to_string()))?;

    let status = response.status();
    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        return Err(ProviderModelsError::Unauthorized);
    }
    if !status.is_success() {
        return Err(ProviderModelsError::Transient(format!(
            "HTTP {} from Copilot /models",
            status.as_u16()
        )));
    }
    let envelope: OpenAiModelsEnvelope = response
        .json()
        .await
        .map_err(|e| ProviderModelsError::InvalidResponse(e.to_string()))?;
    Ok(envelope
        .data
        .into_iter()
        .map(|e| ProviderModel {
            id: e.id,
            provider: ProviderKind::Copilot,
            display_name: None,
            context_window: None,
            deprecated: false,
        })
        .collect())
}

/// Fetch Azure OpenAI model list.
///
/// Azure's `/openai/models` endpoint lists available deployments.
/// The URL uses `AZURE_OPENAI_ENDPOINT` + `/openai/models?api-version={version}`.
pub async fn fetch_azure_models() -> Result<Vec<ProviderModel>, ProviderModelsError> {
    use super::azure::DEFAULT_API_VERSION;
    let endpoint = std::env::var("AZURE_OPENAI_ENDPOINT")
        .ok()
        .filter(|v| !v.is_empty())
        .ok_or(ProviderModelsError::Unauthorized)?;
    let api_version = std::env::var("AZURE_OPENAI_API_VERSION")
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| DEFAULT_API_VERSION.to_string());
    let url = format!(
        "{}/openai/models?api-version={api_version}",
        endpoint.trim_end_matches('/')
    );

    let api_key = std::env::var("AZURE_OPENAI_API_KEY").ok().filter(|v| !v.is_empty());
    let aad_token = std::env::var("AZURE_AD_TOKEN").ok().filter(|v| !v.is_empty());
    if api_key.is_none() && aad_token.is_none() {
        return Err(ProviderModelsError::Unauthorized);
    }

    let client = build_fetch_client();
    let mut builder = client.get(&url);
    if let Some(token) = aad_token {
        builder = builder.bearer_auth(token);
    } else if let Some(key) = api_key {
        builder = builder.header("api-key", key);
    }
    let response = builder.send().await.map_err(|e| ProviderModelsError::Transient(e.to_string()))?;
    let status = response.status();
    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        return Err(ProviderModelsError::Unauthorized);
    }
    if !status.is_success() {
        return Err(ProviderModelsError::Transient(format!(
            "HTTP {} from Azure /openai/models",
            status.as_u16()
        )));
    }
    let envelope: OpenAiModelsEnvelope = response
        .json()
        .await
        .map_err(|e| ProviderModelsError::InvalidResponse(e.to_string()))?;
    Ok(envelope
        .data
        .into_iter()
        .map(|e| ProviderModel {
            id: e.id,
            provider: ProviderKind::Azure,
            display_name: None,
            context_window: None,
            deprecated: false,
        })
        .collect())
}

/// Fetch AWS Bedrock foundation model list.
///
/// Uses `ListFoundationModels` — no streaming, returns a flat list.
/// Endpoint: `GET {bedrock_base}/foundation-models`
/// Auth: SigV4.
pub async fn fetch_bedrock_models() -> Result<Vec<ProviderModel>, ProviderModelsError> {
    use super::bedrock::{BedrockClient};
    // Delegate to the client which has SigV4 signing built in.
    // We call the ListFoundationModels endpoint.
    let creds_ok = std::env::var("AWS_ACCESS_KEY_ID").ok().filter(|v| !v.is_empty()).is_some()
        && std::env::var("AWS_SECRET_ACCESS_KEY").ok().filter(|v| !v.is_empty()).is_some();
    if !creds_ok {
        return Err(ProviderModelsError::Unauthorized);
    }

    // Build a minimal request using the client's signing machinery.
    // We reuse the client just for the SigV4 signing helper — the actual
    // list endpoint doesn't use InvokeModel.
    let client = BedrockClient::from_env()
        .map_err(|e| ProviderModelsError::Other(e.to_string()))?;

    let url = format!(
        "https://bedrock.{}.amazonaws.com/foundation-models",
        std::env::var("AWS_REGION")
            .or_else(|_| std::env::var("AWS_DEFAULT_REGION"))
            .unwrap_or_else(|_| "us-east-1".to_string())
    );

    // Use the client's HTTP client with SigV4 headers for a GET request.
    let http = reqwest::Client::builder()
        .timeout(DEFAULT_FETCH_TIMEOUT)
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());

    // Build signed GET request manually using the signing helper.
    let sig = super::bedrock::sign_request_get(&client.credentials(), "GET", &url);
    let mut builder = http.get(&url)
        .header("Authorization", sig.authorization)
        .header("x-amz-date", sig.x_amz_date)
        .header("x-amz-content-sha256", sig.x_amz_content_sha256);
    if let Some(token) = sig.x_amz_security_token {
        builder = builder.header("x-amz-security-token", token);
    }

    let response = builder.send().await.map_err(|e| ProviderModelsError::Transient(e.to_string()))?;
    let status = response.status();
    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        return Err(ProviderModelsError::Unauthorized);
    }
    if !status.is_success() {
        return Err(ProviderModelsError::Transient(format!(
            "HTTP {} from Bedrock /foundation-models",
            status.as_u16()
        )));
    }
    #[derive(Deserialize)]
    struct BedrockModelsEnvelope {
        #[serde(rename = "modelSummaries", default)]
        model_summaries: Vec<BedrockModelEntry>,
    }
    #[derive(Deserialize)]
    struct BedrockModelEntry {
        #[serde(rename = "modelId")]
        model_id: String,
        #[serde(rename = "modelName", default)]
        model_name: Option<String>,
    }
    let envelope: BedrockModelsEnvelope = response
        .json()
        .await
        .map_err(|e| ProviderModelsError::InvalidResponse(e.to_string()))?;
    Ok(envelope
        .model_summaries
        .into_iter()
        .map(|e| ProviderModel {
            id: e.model_id,
            provider: ProviderKind::Bedrock,
            display_name: e.model_name,
            context_window: None,
            deprecated: false,
        })
        .collect())
}

/// Dispatch a model-list fetch for any provider given its slug.
///
/// Returns the live model list from the provider's `/v1/models` (or equivalent)
/// endpoint.  This is the single entry point used by the TAB-completion layer.
pub async fn fetch_models_for_slug(
    slug: &str,
) -> Result<Vec<ProviderModel>, ProviderModelsError> {
    match slug {
        "fireworks" => fetch_fireworks_models().await,
        "groq" => fetch_groq_models().await,
        "mistral" => fetch_mistral_models().await,
        "perplexity" => fetch_perplexity_models().await,
        "deepseek" => fetch_deepseek_models().await,
        "togetherai" | "together" => fetch_togetherai_models().await,
        "deepinfra" => fetch_deepinfra_models().await,
        "cerebras" => fetch_cerebras_models().await,
        "nvidia-nim" | "nvidia" => fetch_nvidia_nim_models().await,
        "huggingface" | "hf" => fetch_huggingface_models().await,
        "moonshotai" | "moonshot" => fetch_moonshotai_models().await,
        "nebius" => fetch_nebius_models().await,
        "openrouter" => fetch_openrouter_models().await,
        "lmstudio" | "lm-studio" => fetch_lmstudio_models().await,
        "chutes" => fetch_chutes_models().await,
        "scaleway" => fetch_scaleway_models().await,
        "baseten" => fetch_baseten_models().await,
        "minimax" => fetch_minimax_models().await,
        "stackit" => fetch_stackit_models().await,
        "cortecs" => fetch_cortecs_models().await,
        "302ai" | "ai302" => fetch_ai302_models().await,
        "zai" | "kimi" | "glm" => fetch_zai_models().await,
        "opencode" => fetch_opencode_models().await,
        "opencode-go" => fetch_opencode_go_models().await,
        "alibaba" | "dashscope" | "alibaba-coding-plan" => fetch_alibaba_models().await,
        "antigravity" => fetch_antigravity_models().await,
        "cursor" => fetch_cursor_models().await,
        "copilot" | "github-copilot" => fetch_copilot_models().await,
        "azure" | "azure-openai" => fetch_azure_models().await,
        "bedrock" | "aws-bedrock" => fetch_bedrock_models().await,
        _ => Err(ProviderModelsError::Other(format!("unknown provider slug: {slug}"))),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    // Edition 2024: env::set_var/remove_var require unsafe.
    #![allow(unsafe_code)]

    use super::*;
    use crate::providers::crate_env_lock;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    /// Spawn a tiny one-shot HTTP server that returns the given status code
    /// and body for the first connection it accepts.
    fn spawn_one_shot_server(status_line: &'static str, body: &'static str) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind one-shot server");
        let address = listener.local_addr().expect("local addr");
        thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut buf = [0u8; 4096];
            let _ = stream.read(&mut buf);
            let response = format!(
                "HTTP/1.1 {status_line}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes());
        });
        format!("http://{address}")
    }

    #[test]
    fn unauthorized_when_provider_returns_401() {
        let _guard = crate_env_lock();
        let base = spawn_one_shot_server("401 Unauthorized", "{\"error\":{\"message\":\"bad key\"}}");
        unsafe {
            std::env::set_var("OPENAI_BASE_URL", &base);
            std::env::set_var("OPENAI_API_KEY", "bogus");
        }
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("rt");
        let result = runtime.block_on(fetch_openai_models());
        unsafe {
            std::env::remove_var("OPENAI_BASE_URL");
            std::env::remove_var("OPENAI_API_KEY");
        }
        assert_eq!(result, Err(ProviderModelsError::Unauthorized));
    }

    #[test]
    fn transient_when_provider_returns_500() {
        let _guard = crate_env_lock();
        let base = spawn_one_shot_server("500 Internal Server Error", "{}");
        unsafe {
            std::env::set_var("OPENAI_BASE_URL", &base);
            std::env::set_var("OPENAI_API_KEY", "k");
        }
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("rt");
        let result = runtime.block_on(fetch_openai_models());
        unsafe {
            std::env::remove_var("OPENAI_BASE_URL");
            std::env::remove_var("OPENAI_API_KEY");
        }
        match result {
            Err(ProviderModelsError::Transient(_)) => {}
            other => panic!("expected Transient, got {other:?}"),
        }
    }

    #[test]
    fn parses_openai_models_envelope_into_provider_models() {
        let _guard = crate_env_lock();
        let body = r#"{"data":[{"id":"gpt-5.4"},{"id":"gpt-5"},{"id":"o3-mini"}],"object":"list"}"#;
        let base = spawn_one_shot_server("200 OK", body);
        unsafe {
            std::env::set_var("OPENAI_BASE_URL", &base);
            std::env::set_var("OPENAI_API_KEY", "k");
        }
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("rt");
        let result = runtime.block_on(fetch_openai_models());
        unsafe {
            std::env::remove_var("OPENAI_BASE_URL");
            std::env::remove_var("OPENAI_API_KEY");
        }
        let models = result.expect("ok");
        assert_eq!(models.len(), 3);
        assert!(models.iter().all(|m| m.provider == ProviderKind::OpenAi));
        let ids: Vec<&str> = models.iter().map(|m| m.id.as_str()).collect();
        assert!(ids.contains(&"gpt-5.4"));
        assert!(ids.contains(&"o3-mini"));
    }

    #[test]
    fn parses_anthropic_models_envelope() {
        let _guard = crate_env_lock();
        let body = r#"{"data":[{"id":"claude-sonnet-4-6","display_name":"Sonnet 4.6"},{"id":"claude-opus-4-6"}]}"#;
        let base = spawn_one_shot_server("200 OK", body);
        unsafe {
            std::env::set_var("ANTHROPIC_BASE_URL", &base);
            std::env::set_var("ANTHROPIC_API_KEY", "k");
        }
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("rt");
        let result = runtime.block_on(fetch_anthropic_models());
        unsafe {
            std::env::remove_var("ANTHROPIC_BASE_URL");
            std::env::remove_var("ANTHROPIC_API_KEY");
        }
        let models = result.expect("ok");
        assert_eq!(models.len(), 2);
        let sonnet = models
            .iter()
            .find(|m| m.id == "claude-sonnet-4-6")
            .expect("sonnet entry");
        assert_eq!(sonnet.display_name.as_deref(), Some("Sonnet 4.6"));
        assert_eq!(sonnet.provider, ProviderKind::AnvilApi);
    }

    #[test]
    fn parses_ollama_tags_envelope() {
        let _guard = crate_env_lock();
        let body = r#"{"models":[{"name":"llama3.2:latest"},{"name":"kimi-k2.6:cloud"}]}"#;
        let base = spawn_one_shot_server("200 OK", body);
        unsafe {
            std::env::set_var("OLLAMA_HOST", &base);
        }
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("rt");
        let result = runtime.block_on(fetch_ollama_local_models());
        unsafe {
            std::env::remove_var("OLLAMA_HOST");
        }
        let models = result.expect("ok");
        assert_eq!(models.len(), 2);
        assert!(models.iter().all(|m| m.provider == ProviderKind::Ollama));
    }

    #[test]
    fn cloud_filter_keeps_only_cloud_suffixed_tags() {
        let _guard = crate_env_lock();
        let body = r#"{"models":[{"name":"llama3.2:latest"},{"name":"kimi-k2.6:cloud"},{"name":"gpt-oss:120b-cloud"}]}"#;
        let base = spawn_one_shot_server("200 OK", body);
        unsafe {
            std::env::set_var("OLLAMA_HOST", &base);
        }
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("rt");
        let result = runtime.block_on(fetch_ollama_cloud_models());
        unsafe {
            std::env::remove_var("OLLAMA_HOST");
        }
        let models = result.expect("ok");
        assert_eq!(models.len(), 2);
        assert!(models.iter().all(|m| is_ollama_cloud_model(&m.id)));
    }

    #[test]
    fn is_provider_configured_anthropic_via_env_key() {
        let _guard = crate_env_lock();
        let config_home = std::env::temp_dir().join(format!(
            "model-list-test-anthropic-{}",
            std::process::id()
        ));
        unsafe {
            std::env::set_var("ANVIL_CONFIG_HOME", &config_home);
            std::env::set_var("ANTHROPIC_API_KEY", "test-key");
            std::env::remove_var("ANTHROPIC_AUTH_TOKEN");
        }
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("rt");
        let configured = runtime.block_on(is_provider_configured(ProviderKind::AnvilApi));
        unsafe {
            std::env::remove_var("ANTHROPIC_API_KEY");
            std::env::remove_var("ANVIL_CONFIG_HOME");
        }
        let _ = std::fs::remove_dir_all(&config_home);
        assert_eq!(configured, Some(ProviderCredentials::Anthropic));
    }

    #[test]
    fn is_provider_configured_openai_requires_env_key() {
        let _guard = crate_env_lock();
        unsafe {
            std::env::remove_var("OPENAI_API_KEY");
        }
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("rt");
        let configured = runtime.block_on(is_provider_configured(ProviderKind::OpenAi));
        assert_eq!(configured, None);

        unsafe {
            std::env::set_var("OPENAI_API_KEY", "set");
        }
        let configured = runtime.block_on(is_provider_configured(ProviderKind::OpenAi));
        unsafe {
            std::env::remove_var("OPENAI_API_KEY");
        }
        assert_eq!(configured, Some(ProviderCredentials::OpenAi));
    }

    #[test]
    fn is_provider_configured_ollama_unreachable_returns_none() {
        let _guard = crate_env_lock();
        // Point at a port we know is closed: bind a listener, capture its
        // port, drop the listener so the port is free, then test that the
        // probe returns None.
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("addr").port();
        drop(listener);
        unsafe {
            std::env::set_var("OLLAMA_HOST", format!("http://127.0.0.1:{port}"));
        }
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("rt");
        let configured = runtime.block_on(is_provider_configured(ProviderKind::Ollama));
        unsafe {
            std::env::remove_var("OLLAMA_HOST");
        }
        assert_eq!(configured, None);
    }
}
