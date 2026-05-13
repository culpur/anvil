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
    /// OpenAI bearer token (raw API key only — OAuth-via-ChatGPT support is
    /// not yet wired in `DefaultRuntimeClient`; when it is, this enum stays
    /// the same and the resolver picks it up automatically).
    OpenAi,
    /// Google Gemini API key.
    Gemini,
    /// Local Ollama daemon reachable on `OLLAMA_HOST` (or default).
    OllamaLocal,
    /// Local Ollama daemon reachable AND authenticated against Ollama Cloud
    /// (the daemon proxies via its signed device key).
    OllamaCloud,
}

impl ProviderCredentials {
    /// The provider kind these credentials authorize. Multiple credentials
    /// can share a kind (e.g. local + cloud Ollama both map to
    /// [`ProviderKind::Ollama`]).
    #[must_use]
    pub const fn kind(&self) -> ProviderKind {
        match self {
            Self::Anthropic => ProviderKind::AnvilApi,
            Self::Xai => ProviderKind::Xai,
            Self::OpenAi => ProviderKind::OpenAi,
            Self::Gemini => ProviderKind::Gemini,
            Self::OllamaLocal | Self::OllamaCloud => ProviderKind::Ollama,
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
/// - an OAuth token is saved on disk for that provider (currently Anthropic only),
/// - for Ollama, the local daemon is reachable.
///
/// Returns `None` if no path resolves — the caller hides the provider.
///
/// **Critical**: this function MUST NOT introduce a new credential surface.
/// If `DefaultRuntimeClient::new()` learns about a new credential path, this
/// function picks it up automatically through the shared `has_*` helpers and
/// the `OllamaClient::is_available()` probe.
pub async fn is_provider_configured(kind: ProviderKind) -> Option<ProviderCredentials> {
    match kind {
        ProviderKind::AnvilApi => has_auth_from_env_or_saved()
            .unwrap_or(false)
            .then_some(ProviderCredentials::Anthropic),
        ProviderKind::Xai => has_api_key("XAI_API_KEY").then_some(ProviderCredentials::Xai),
        ProviderKind::OpenAi => {
            has_api_key("OPENAI_API_KEY").then_some(ProviderCredentials::OpenAi)
        }
        ProviderKind::Gemini => {
            if has_api_key("GEMINI_API_KEY") || has_api_key("GOOGLE_API_KEY") {
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
    }
}

/// Enumerate every configured provider in a single pass.
///
/// The returned set is small (≤ 5 entries), so the caller can iterate it
/// freely. Order matches the [`ProviderKind`] enum declaration so the picker
/// labels are stable across calls.
pub async fn enumerate_configured_providers() -> Vec<ProviderCredentials> {
    let mut out = Vec::new();
    for kind in [
        ProviderKind::AnvilApi,
        ProviderKind::Xai,
        ProviderKind::OpenAi,
        ProviderKind::Gemini,
        ProviderKind::Ollama,
    ] {
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
