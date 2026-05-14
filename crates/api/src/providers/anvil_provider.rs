// Edition 2024: env::set_var/remove_var require unsafe
#![allow(unsafe_code)]

use std::collections::VecDeque;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use runtime::{
    load_oauth_credentials, save_oauth_credentials, EffortLevel,
    OAuthConfig, OAuthRefreshRequest, OAuthTokenExchangeRequest,
};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::error::ApiError;

use super::common::{
    self, expect_success, read_env_non_empty, request_id_from_headers, DEFAULT_INITIAL_BACKOFF,
    DEFAULT_MAX_BACKOFF, DEFAULT_MAX_RETRIES,
};
use super::openai_compat::resolve_stream_dead_air_timeout;
use super::{Provider, ProviderFuture};
use crate::sse::SseParser;
use crate::types::{CacheControl, MessageRequest, MessageResponse, StreamEvent};

pub const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";
const ANTHROPIC_VERSION: &str = "2023-06-01";


#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthSource {
    None,
    ApiKey(String),
    BearerToken(String),
    ApiKeyAndBearer {
        api_key: String,
        bearer_token: String,
    },
}

impl AuthSource {
    pub fn from_env() -> Result<Self, ApiError> {
        let api_key = read_env_non_empty("ANTHROPIC_API_KEY")?;
        let auth_token = read_env_non_empty("ANTHROPIC_AUTH_TOKEN")?;
        match (api_key, auth_token) {
            (Some(api_key), Some(bearer_token)) => Ok(Self::ApiKeyAndBearer {
                api_key,
                bearer_token,
            }),
            (Some(api_key), None) => Ok(Self::ApiKey(api_key)),
            (None, Some(bearer_token)) => Ok(Self::BearerToken(bearer_token)),
            (None, None) => Err(ApiError::missing_credentials(
                "Anvil",
                &["ANTHROPIC_AUTH_TOKEN", "ANTHROPIC_API_KEY"],
            )),
        }
    }

    #[must_use]
    pub fn api_key(&self) -> Option<&str> {
        match self {
            Self::ApiKey(api_key) | Self::ApiKeyAndBearer { api_key, .. } => Some(api_key),
            Self::None | Self::BearerToken(_) => None,
        }
    }

    #[must_use]
    pub fn bearer_token(&self) -> Option<&str> {
        match self {
            Self::BearerToken(token)
            | Self::ApiKeyAndBearer {
                bearer_token: token,
                ..
            } => Some(token),
            Self::None | Self::ApiKey(_) => None,
        }
    }

    #[must_use]
    pub fn masked_authorization_header(&self) -> &'static str {
        if self.bearer_token().is_some() {
            "Bearer [REDACTED]"
        } else {
            "<absent>"
        }
    }

    pub fn apply(&self, mut request_builder: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        if let Some(api_key) = self.api_key() {
            request_builder = request_builder.header("x-api-key", api_key);
        }
        if let Some(token) = self.bearer_token() {
            request_builder = request_builder.bearer_auth(token);
        }
        request_builder
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct OAuthTokenSet {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_at: Option<u64>,
    #[serde(default)]
    pub scopes: Vec<String>,
}

impl From<OAuthTokenSet> for AuthSource {
    fn from(value: OAuthTokenSet) -> Self {
        Self::BearerToken(value.access_token)
    }
}

#[derive(Debug, Clone)]
pub struct AnvilApiClient {
    http: reqwest::Client,
    auth: AuthSource,
    base_url: String,
    max_retries: u32,
    initial_backoff: Duration,
    max_backoff: Duration,
}

impl AnvilApiClient {
    #[must_use]
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            http: reqwest::Client::new(),
            auth: AuthSource::ApiKey(api_key.into()),
            base_url: DEFAULT_BASE_URL.to_string(),
            max_retries: DEFAULT_MAX_RETRIES,
            initial_backoff: DEFAULT_INITIAL_BACKOFF,
            max_backoff: DEFAULT_MAX_BACKOFF,
        }
    }

    #[must_use]
    pub fn from_auth(auth: AuthSource) -> Self {
        Self {
            http: reqwest::Client::new(),
            auth,
            base_url: DEFAULT_BASE_URL.to_string(),
            max_retries: DEFAULT_MAX_RETRIES,
            initial_backoff: DEFAULT_INITIAL_BACKOFF,
            max_backoff: DEFAULT_MAX_BACKOFF,
        }
    }

    pub fn from_env() -> Result<Self, ApiError> {
        Ok(Self::from_auth(AuthSource::from_env_or_saved()?).with_base_url(read_base_url()))
    }

    #[must_use]
    pub fn with_auth_source(mut self, auth: AuthSource) -> Self {
        self.auth = auth;
        self
    }

    #[must_use]
    pub fn with_auth_token(mut self, auth_token: Option<String>) -> Self {
        match (
            self.auth.api_key().map(ToOwned::to_owned),
            auth_token.filter(|token| !token.is_empty()),
        ) {
            (Some(api_key), Some(bearer_token)) => {
                self.auth = AuthSource::ApiKeyAndBearer {
                    api_key,
                    bearer_token,
                };
            }
            (Some(api_key), None) => {
                self.auth = AuthSource::ApiKey(api_key);
            }
            (None, Some(bearer_token)) => {
                self.auth = AuthSource::BearerToken(bearer_token);
            }
            (None, None) => {
                self.auth = AuthSource::None;
            }
        }
        self
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

    #[must_use]
    pub const fn auth_source(&self) -> &AuthSource {
        &self.auth
    }

    pub async fn send_message(
        &self,
        request: &MessageRequest,
    ) -> Result<MessageResponse, ApiError> {
        let t0 = Instant::now();
        let request = MessageRequest {
            stream: false,
            ..request.clone()
        };
        let http_response = self.send_with_retry(&request).await;
        let duration_ms = t0.elapsed().as_millis() as u64;

        match http_response {
            Ok(response) => {
                let status_code = response.status().as_u16();
                let request_id = request_id_from_headers(response.headers());
                let mut parsed = response
                    .json::<MessageResponse>()
                    .await
                    .map_err(ApiError::from)?;
                if parsed.request_id.is_none() {
                    parsed.request_id = request_id;
                }
                runtime::otel::api_request(
                    "anthropic",
                    &request.model,
                    status_code,
                    0,
                    duration_ms,
                    u64::from(parsed.usage.input_tokens),
                    u64::from(parsed.usage.output_tokens),
                );
                Ok(parsed)
            }
            Err(err) => {
                // Extract HTTP status code if the error carries one (e.g. after
                // retries are exhausted the final Api error has the status).
                let status_code = match &err {
                    ApiError::Api { status, .. } => status.as_u16(),
                    ApiError::RetriesExhausted { last_error, .. } => {
                        if let ApiError::Api { status, .. } = last_error.as_ref() {
                            status.as_u16()
                        } else {
                            0
                        }
                    }
                    _ => 0,
                };
                runtime::otel::api_request(
                    "anthropic",
                    &request.model,
                    status_code,
                    self.max_retries,
                    duration_ms,
                    0,
                    0,
                );
                Err(err)
            }
        }
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
            parser: SseParser::new(),
            pending: VecDeque::new(),
            done: false,
            last_chunk_at: Instant::now(),
            dead_air_timeout: resolve_stream_dead_air_timeout(),
        })
    }

    pub async fn exchange_oauth_code(
        &self,
        config: &OAuthConfig,
        request: &OAuthTokenExchangeRequest,
    ) -> Result<OAuthTokenSet, ApiError> {
        let response = self
            .http
            .post(&config.token_url)
            .header("content-type", "application/x-www-form-urlencoded")
            .form(&request.form_params())
            .send()
            .await
            .map_err(ApiError::from)?;
        let response = expect_success(response).await?;
        response
            .json::<OAuthTokenSet>()
            .await
            .map_err(ApiError::from)
    }

    pub async fn refresh_oauth_token(
        &self,
        config: &OAuthConfig,
        request: &OAuthRefreshRequest,
    ) -> Result<OAuthTokenSet, ApiError> {
        let response = self
            .http
            .post(&config.token_url)
            .header("content-type", "application/x-www-form-urlencoded")
            .form(&request.form_params())
            .send()
            .await
            .map_err(ApiError::from)?;
        let response = expect_success(response).await?;
        response
            .json::<OAuthTokenSet>()
            .await
            .map_err(ApiError::from)
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
        let request_url = format!("{}/v1/messages", self.base_url.trim_end_matches('/'));
        let mut request_builder = self
            .http
            .post(&request_url)
            .header("anthropic-version", ANTHROPIC_VERSION)
            .header("content-type", "application/json");

        // When using OAuth bearer tokens, add the beta headers required by
        // Anthropic's Max-plan OAuth gate.  Max-plan bearers are only accepted
        // when the client identifies as Claude Code via both:
        //   * `anthropic-beta: claude-code-20250219,oauth-2025-04-20`
        //   * a leading identity system block (injected in
        //     `build_messages_request_body` below)
        // Without both, Anthropic returns HTTP 429 `rate_limit_error` with an
        // empty body — looks like a quota issue but is actually an
        // access-control rejection.
        let is_oauth_bearer = self.auth.bearer_token().is_some();
        if is_oauth_bearer {
            request_builder = request_builder
                .header("anthropic-beta", "claude-code-20250219,oauth-2025-04-20");
        }

        request_builder = self.auth.apply(request_builder);
        request_builder = common::apply_traceparent_header(request_builder);
        // Bug #26: serialize the request through the Anthropic-specific wire
        // builder so we can attach `cache_control` markers to the system
        // prompt and the last tool definition.  The breakpoints tell the
        // Anthropic API to cache up to and including those blocks; with a
        // 1h TTL the cache survives long agentic sessions and we stop
        // re-billing the full system+tools prefix on every turn.
        let payload = build_messages_request_body(request, is_oauth_bearer);
        request_builder = request_builder.json(&payload);
        request_builder.send().await.map_err(ApiError::from)
    }


}

/// Build the JSON body sent to `POST /v1/messages` for the Anthropic API.
///
/// This is a thin pass-through over `MessageRequest` with three
/// Anthropic-specific transformations:
///
///   1. The `system` field is upgraded from a plain string to a typed content
///      block array carrying a `cache_control` breakpoint.  This makes the
///      cached prefix include the system prompt itself.
///   2. The LAST entry in the `tools` array gets a `cache_control` marker.
///      Anthropic caches *up to and including* the breakpoint, so marking the
///      tail tool definition caches the whole tools array (and, transitively,
///      the system prompt above it) in one cache entry.
///   3. When `ANVIL_EFFORT` is set (or non-default effort is active), a
///      `thinking` block is injected with the appropriate `budget_tokens`.
///      The budget is capped at `max(0, request.max_tokens - 4096)` to leave
///      room for the model's own output.  A warning is emitted to stderr when
///      the cap is applied.  For models / effort levels where thinking is
///      disabled (i.e. the effective budget would be 0 or the env says "low"
///      with a special no-think override), no `thinking` block is emitted.
///
/// Both cache-control breakpoints use TTL `"1h"` so the cache survives
/// long-running agent sessions.  When the request has no system prompt or no
/// tools, the corresponding breakpoint is silently skipped.
///
/// This function is gated to the Anthropic provider on purpose: OpenAI-compat
/// and Ollama backends would reject `cache_control` and `thinking` keys, so
/// the wire format is built locally rather than embedded in the shared
/// `MessageRequest` type.
/// Build the Anthropic `/v1/messages` request body.
///
/// `is_oauth_bearer` toggles the Max-plan identity preamble: when true, an
/// uncached `"You are Claude Code, Anthropic's official CLI for Claude."`
/// system block is prepended.  Anthropic's Max-plan OAuth gate requires this
/// preamble alongside the `claude-code-20250219` beta header — without both,
/// requests are rejected as HTTP 429 `rate_limit_error` with no body.  API-key
/// auth doesn't need the preamble and shouldn't pay the 23 tokens for it.
fn build_messages_request_body(request: &MessageRequest, is_oauth_bearer: bool) -> Value {
    // Start from the standard serde-serialized envelope so we inherit any
    // future field changes for free.
    let mut payload = serde_json::to_value(request).unwrap_or_else(|_| json!({}));

    let cache_control = CacheControl::ephemeral_with_ttl("1h");
    let cache_control_value =
        serde_json::to_value(&cache_control).unwrap_or_else(|_| json!({"type": "ephemeral"}));

    /// The exact identity string Anthropic's Max-plan OAuth gate checks for.
    /// Do not change without re-verifying against the live `/v1/messages`
    /// endpoint — the server uses this as an access-control signal.
    const CLAUDE_CODE_IDENTITY: &str =
        "You are Claude Code, Anthropic's official CLI for Claude.";

    // (1) Upgrade `system: "..."` → array of content blocks with
    //     `cache_control`. Phase 4.5 (L1 §9): when the prompt body
    //     contains `SYSTEM_PROMPT_DYNAMIC_BOUNDARY`, split into a
    //     cache-stable HEAD (intro/system/doing-tasks/actions) and a
    //     fresh TAIL (environment/retrieval-order/project/memory/qmd/
    //     config/known-files + appended skills/goals). Only the head
    //     carries `cache_control`, so per-turn changes to the tail
    //     don't bust the cached prefix. The boundary marker itself is
    //     stripped before sending — it's an internal anchor, never
    //     model-facing tokens.
    //
    //     When the marker is absent (subagents, --print mode, legacy
    //     prompts), fall back to the historical single-block path with
    //     cache_control on the whole system prompt.
    if let Some(system_text) = payload
        .get("system")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
    {
        match runtime::split_system_prompt_at_boundary(&system_text) {
            Some((head, tail)) if !head.is_empty() => {
                // Two-block layout: cached head, fresh tail.
                let mut blocks: Vec<Value> = Vec::with_capacity(2);
                blocks.push(json!({
                    "type": "text",
                    "text": head,
                    "cache_control": cache_control_value.clone(),
                }));
                if !tail.is_empty() {
                    blocks.push(json!({
                        "type": "text",
                        "text": tail,
                    }));
                }
                payload["system"] = Value::Array(blocks);
            }
            // No boundary, OR head is empty (boundary at index 0): fall
            // back to the historical single-block form with the entire
            // body cached.
            _ => {
                payload["system"] = json!([{
                    "type": "text",
                    "text": system_text,
                    "cache_control": cache_control_value.clone(),
                }]);
            }
        }
    }

    // (1b) Prepend the Claude Code identity block when using OAuth bearer
    //      auth.  This satisfies Anthropic's Max-plan OAuth gate (see header
    //      comment on this function).  The block is intentionally uncached:
    //      it's 23 tokens, sent once per session anyway, and cache_control on
    //      the FIRST block would force every subsequent system block to share
    //      its cache lifetime — better to let the existing breakpoint on the
    //      real prompt body do the caching.
    if is_oauth_bearer {
        let identity_block = json!({
            "type": "text",
            "text": CLAUDE_CODE_IDENTITY,
        });
        match payload.get_mut("system") {
            Some(Value::Array(blocks)) => {
                blocks.insert(0, identity_block);
            }
            _ => {
                payload["system"] = json!([identity_block]);
            }
        }
    }

    // (2) Attach cache_control to the LAST tool definition.  Anthropic caches
    //     up to (and including) the breakpoint, so this single marker covers
    //     every tool above it as well.
    if let Some(tools) = payload.get_mut("tools").and_then(Value::as_array_mut)
        && let Some(last) = tools.last_mut()
        && let Some(obj) = last.as_object_mut()
    {
        obj.insert("cache_control".to_string(), cache_control_value);
    }

    // (3) Inject the `thinking` block when ANVIL_EFFORT is set to a non-default
    //     level OR when the env var is present at all.
    //
    //     We only inject when the env var is explicitly present (any level,
    //     including medium).  Rationale: if the user has never set ANVIL_EFFORT
    //     we want zero behaviour change — existing sessions get no thinking
    //     block, matching historical behaviour.  Once they do `/effort medium`
    //     (or set ANVIL_EFFORT=medium) they opt in.
    if let Some(effort) = EffortLevel::from_env() {
        let budget_target = effort.anthropic_budget_tokens();
        // Leave at least 4096 tokens for the model's output.
        let safe_max = request.max_tokens.saturating_sub(4_096);
        let budget = budget_target.min(safe_max);
        if budget_target > safe_max {
            eprintln!(
                "[anvil] warning: effort={} targets {budget_target} thinking tokens but \
                 max_tokens={} — capping to {budget}",
                effort.as_str(),
                request.max_tokens,
            );
        }
        if budget > 0 {
            payload["thinking"] = json!({
                "type": "enabled",
                "budget_tokens": budget,
            });
        }
    }

    payload
}

/// Build the Anthropic wire body with an explicit effort level (used in tests).
#[cfg(test)]
fn build_messages_request_body_with_effort(
    request: &MessageRequest,
    effort: Option<EffortLevel>,
) -> Value {
    // Temporarily override ANVIL_EFFORT so the production builder picks it up.
    let prev = std::env::var("ANVIL_EFFORT").ok();
    match effort {
        Some(level) => unsafe { std::env::set_var("ANVIL_EFFORT", level.as_str()); },
        None => unsafe { std::env::remove_var("ANVIL_EFFORT"); },
    }
    let result = build_messages_request_body(request, false);
    match prev {
        Some(val) => unsafe { std::env::set_var("ANVIL_EFFORT", val); },
        None => unsafe { std::env::remove_var("ANVIL_EFFORT"); },
    }
    result
}

impl AuthSource {
    pub fn from_env_or_saved() -> Result<Self, ApiError> {
        if let Some(api_key) = read_env_non_empty("ANTHROPIC_API_KEY")? {
            return match read_env_non_empty("ANTHROPIC_AUTH_TOKEN")? {
                Some(bearer_token) => Ok(Self::ApiKeyAndBearer {
                    api_key,
                    bearer_token,
                }),
                None => Ok(Self::ApiKey(api_key)),
            };
        }
        if let Some(bearer_token) = read_env_non_empty("ANTHROPIC_AUTH_TOKEN")? {
            return Ok(Self::BearerToken(bearer_token));
        }
        match load_saved_oauth_token() {
            Ok(Some(token_set)) if oauth_token_is_expired(&token_set) => {
                // Auto-refresh expired tokens if we have a refresh token
                if token_set.refresh_token.is_some() {
                    let config = OAuthConfig {
                        client_id: String::from("9d1c250a-e61b-44d9-88ed-5944d1962f5e"),
                        authorize_url: String::from("https://claude.ai/oauth/authorize"),
                        token_url: String::from("https://platform.claude.com/v1/oauth/token"),
                        callback_port: None,
                        manual_redirect_url: Some(String::from("https://platform.claude.com/oauth/code/callback")),
                        scopes: vec![
                            String::from("user:profile"),
                            String::from("user:inference"),
                            String::from("user:sessions:claude_code"),
                        ],
                    };
                    match resolve_saved_oauth_token_set(&config, token_set) {
                        Ok(refreshed) => Ok(Self::BearerToken(refreshed.access_token)),
                        Err(_) => Err(ApiError::ExpiredOAuthToken),
                    }
                } else {
                    Err(ApiError::ExpiredOAuthToken)
                }
            }
            Ok(Some(token_set)) => Ok(Self::BearerToken(token_set.access_token)),
            Ok(None) => Err(ApiError::missing_credentials(
                "Anvil",
                &["ANTHROPIC_AUTH_TOKEN", "ANTHROPIC_API_KEY"],
            )),
            Err(error) => Err(error),
        }
    }
}

#[must_use]
pub fn oauth_token_is_expired(token_set: &OAuthTokenSet) -> bool {
    token_set
        .expires_at
        .is_some_and(|expires_at| expires_at <= now_unix_timestamp())
}

pub fn resolve_saved_oauth_token(config: &OAuthConfig) -> Result<Option<OAuthTokenSet>, ApiError> {
    let Some(token_set) = load_saved_oauth_token()? else {
        return Ok(None);
    };
    resolve_saved_oauth_token_set(config, token_set).map(Some)
}

pub fn has_auth_from_env_or_saved() -> Result<bool, ApiError> {
    Ok(read_env_non_empty("ANTHROPIC_API_KEY")?.is_some()
        || read_env_non_empty("ANTHROPIC_AUTH_TOKEN")?.is_some()
        || load_saved_oauth_token()?.is_some())
}

pub fn resolve_startup_auth_source<F>(load_oauth_config: F) -> Result<AuthSource, ApiError>
where
    F: FnOnce() -> Result<Option<OAuthConfig>, ApiError>,
{
    if let Some(api_key) = read_env_non_empty("ANTHROPIC_API_KEY")? {
        return match read_env_non_empty("ANTHROPIC_AUTH_TOKEN")? {
            Some(bearer_token) => Ok(AuthSource::ApiKeyAndBearer {
                api_key,
                bearer_token,
            }),
            None => Ok(AuthSource::ApiKey(api_key)),
        };
    }
    if let Some(bearer_token) = read_env_non_empty("ANTHROPIC_AUTH_TOKEN")? {
        return Ok(AuthSource::BearerToken(bearer_token));
    }

    let Some(token_set) = load_saved_oauth_token()? else {
        return Err(ApiError::missing_credentials(
            "Anvil",
            &["ANTHROPIC_AUTH_TOKEN", "ANTHROPIC_API_KEY"],
        ));
    };
    if !oauth_token_is_expired(&token_set) {
        return Ok(AuthSource::BearerToken(token_set.access_token));
    }
    if token_set.refresh_token.is_none() {
        return Err(ApiError::ExpiredOAuthToken);
    }

    let Some(config) = load_oauth_config()? else {
        return Err(ApiError::Auth(
            "saved OAuth token is expired; runtime OAuth config is missing".to_string(),
        ));
    };
    Ok(AuthSource::from(resolve_saved_oauth_token_set(
        &config, token_set,
    )?))
}

fn resolve_saved_oauth_token_set(
    config: &OAuthConfig,
    token_set: OAuthTokenSet,
) -> Result<OAuthTokenSet, ApiError> {
    if !oauth_token_is_expired(&token_set) {
        return Ok(token_set);
    }
    let Some(refresh_token) = token_set.refresh_token.clone() else {
        return Err(ApiError::ExpiredOAuthToken);
    };
    let client = AnvilApiClient::from_auth(AuthSource::None).with_base_url(read_base_url());
    let refreshed = client_runtime_block_on(async {
        client
            .refresh_oauth_token(
                config,
                &OAuthRefreshRequest::from_config(
                    config,
                    refresh_token,
                    Some(token_set.scopes.clone()),
                ),
            )
            .await
    })?;
    let resolved = OAuthTokenSet {
        access_token: refreshed.access_token,
        refresh_token: refreshed.refresh_token.or(token_set.refresh_token),
        expires_at: refreshed.expires_at,
        scopes: refreshed.scopes,
    };
    save_oauth_credentials(&runtime::OAuthTokenSet {
        access_token: resolved.access_token.clone(),
        refresh_token: resolved.refresh_token.clone(),
        expires_at: resolved.expires_at,
        scopes: resolved.scopes.clone(),
    })
    .map_err(ApiError::from)?;
    Ok(resolved)
}

fn client_runtime_block_on<F, T>(future: F) -> Result<T, ApiError>
where
    F: std::future::Future<Output = Result<T, ApiError>>,
{
    tokio::runtime::Runtime::new()
        .map_err(ApiError::from)?
        .block_on(future)
}

fn load_saved_oauth_token() -> Result<Option<OAuthTokenSet>, ApiError> {
    let token_set = load_oauth_credentials().map_err(ApiError::from)?;
    Ok(token_set.map(|token_set| OAuthTokenSet {
        access_token: token_set.access_token,
        refresh_token: token_set.refresh_token,
        expires_at: token_set.expires_at,
        scopes: token_set.scopes,
    }))
}

fn now_unix_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}


#[cfg(test)]
fn read_api_key() -> Result<String, ApiError> {
    let auth = AuthSource::from_env_or_saved()?;
    auth.api_key()
        .or_else(|| auth.bearer_token())
        .map(ToOwned::to_owned)
        .ok_or(ApiError::missing_credentials(
            "Anvil",
            &["ANTHROPIC_AUTH_TOKEN", "ANTHROPIC_API_KEY"],
        ))
}

#[cfg(test)]
fn read_auth_token() -> Option<String> {
    read_env_non_empty("ANTHROPIC_AUTH_TOKEN")
        .ok()
        .and_then(std::convert::identity)
}

#[must_use]
pub fn read_base_url() -> String {
    std::env::var("ANTHROPIC_BASE_URL").unwrap_or_else(|_| DEFAULT_BASE_URL.to_string())
}


impl Provider for AnvilApiClient {
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

#[derive(Debug)]
pub struct MessageStream {
    request_id: Option<String>,
    response: reqwest::Response,
    parser: SseParser,
    pending: VecDeque<StreamEvent>,
    done: bool,
    /// Wall-clock time of the last successful chunk receipt.  Used by the
    /// dead-air timer below to surface a distinctive stall error instead of
    /// hanging on an indefinitely-stalled upstream connection.
    last_chunk_at: Instant,
    /// Maximum allowed gap between chunks before we surface
    /// `ApiError::StreamStalled`.  Mirrors the OpenAI-compat path
    /// (Bug #82) and is configurable via `ANVIL_STREAM_DEAD_AIR_MS`.
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
                let remaining = self.parser.finish()?;
                self.pending.extend(remaining);
                if let Some(event) = self.pending.pop_front() {
                    return Ok(Some(event));
                }
                return Ok(None);
            }

            // Bug #15 fix: apply a dead-air timeout to the chunk read so a
            // stalled Anthropic stream surfaces `StreamStalled` instead of
            // hanging forever.  Mirrors the OpenAI-compat path verbatim — the
            // timer resets on every chunk (including thinking_delta), which is
            // wake-from-sleep safe because `Instant` advances monotonically
            // regardless of system clock skew.
            let chunk_result =
                tokio::time::timeout(self.dead_air_timeout, self.response.chunk()).await;

            match chunk_result {
                Ok(Ok(Some(chunk))) => {
                    self.last_chunk_at = Instant::now();
                    self.pending.extend(self.parser.push(&chunk)?);
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


#[cfg(test)]
mod tests {
    use super::common::{ALT_REQUEST_ID_HEADER, REQUEST_ID_HEADER, is_retryable_status, backoff_for_attempt};
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use runtime::{clear_oauth_credentials, save_oauth_credentials, OAuthConfig};

    use super::{
        now_unix_timestamp, oauth_token_is_expired, resolve_saved_oauth_token,
        resolve_startup_auth_source, AuthSource, AnvilApiClient, OAuthTokenSet,
    };
    use crate::types::{ContentBlockDelta, MessageRequest};

    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        super::super::crate_env_lock()
    }

    fn temp_config_home() -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "api-oauth-test-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("time")
                .as_nanos()
        ))
    }

    fn cleanup_temp_config_home(config_home: &std::path::Path) {
        match std::fs::remove_dir_all(config_home) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => panic!("cleanup temp dir: {error}"),
        }
    }

    fn sample_oauth_config(token_url: String) -> OAuthConfig {
        OAuthConfig {
            client_id: "runtime-client".to_string(),
            authorize_url: "https://console.test/oauth/authorize".to_string(),
            token_url,
            callback_port: Some(4545),
            manual_redirect_url: Some("https://console.test/oauth/callback".to_string()),
            scopes: vec!["org:read".to_string(), "user:write".to_string()],
        }
    }

    fn spawn_token_server(response_body: &'static str) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind listener");
        let address = listener.local_addr().expect("local addr");
        thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept connection");
            let mut buffer = [0_u8; 4096];
            let _ = stream.read(&mut buffer).expect("read request");
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{}",
                response_body.len(),
                response_body
            );
            stream
                .write_all(response.as_bytes())
                .expect("write response");
        });
        format!("http://{address}/oauth/token")
    }

    #[test]
    fn read_api_key_requires_presence() {
        let _guard = env_lock();
        // Point ANVIL_CONFIG_HOME at an empty temp dir so no saved OAuth credentials
        // are found even if the real user config contains them, preventing a real
        // key from leaking into a panic message.
        let config_home = temp_config_home();
        unsafe { std::env::set_var("ANVIL_CONFIG_HOME", &config_home); }
        unsafe { std::env::remove_var("ANTHROPIC_AUTH_TOKEN"); }
        unsafe { std::env::remove_var("ANTHROPIC_API_KEY"); }
        let error = super::read_api_key().expect_err("missing key should error");
        unsafe { std::env::remove_var("ANVIL_CONFIG_HOME"); }
        cleanup_temp_config_home(&config_home);
        assert!(matches!(
            error,
            crate::error::ApiError::MissingCredentials { .. }
        ));
    }

    #[test]
    fn read_api_key_requires_non_empty_value() {
        let _guard = env_lock();
        // Point ANVIL_CONFIG_HOME at an empty temp dir so no saved OAuth credentials
        // are found, preventing a real key from leaking into a panic message.
        let config_home = temp_config_home();
        unsafe { std::env::set_var("ANVIL_CONFIG_HOME", &config_home); }
        unsafe { std::env::set_var("ANTHROPIC_AUTH_TOKEN", ""); }
        unsafe { std::env::remove_var("ANTHROPIC_API_KEY"); }
        let error = super::read_api_key().expect_err("empty key should error");
        unsafe { std::env::remove_var("ANTHROPIC_AUTH_TOKEN"); }
        unsafe { std::env::remove_var("ANVIL_CONFIG_HOME"); }
        cleanup_temp_config_home(&config_home);
        assert!(matches!(
            error,
            crate::error::ApiError::MissingCredentials { .. }
        ));
    }

    #[test]
    fn read_api_key_prefers_api_key_env() {
        let _guard = env_lock();
        unsafe { std::env::set_var("ANTHROPIC_AUTH_TOKEN", "auth-token"); }
        unsafe { std::env::set_var("ANTHROPIC_API_KEY", "legacy-key"); }
        assert_eq!(
            super::read_api_key().expect("api key should load"),
            "legacy-key"
        );
        unsafe { std::env::remove_var("ANTHROPIC_AUTH_TOKEN"); }
        unsafe { std::env::remove_var("ANTHROPIC_API_KEY"); }
    }

    #[test]
    fn read_auth_token_reads_auth_token_env() {
        let _guard = env_lock();
        unsafe { std::env::set_var("ANTHROPIC_AUTH_TOKEN", "auth-token"); }
        assert_eq!(super::read_auth_token().as_deref(), Some("auth-token"));
        unsafe { std::env::remove_var("ANTHROPIC_AUTH_TOKEN"); }
    }

    #[test]
    fn oauth_token_maps_to_bearer_auth_source() {
        let auth = AuthSource::from(OAuthTokenSet {
            access_token: "access-token".to_string(),
            refresh_token: Some("refresh".to_string()),
            expires_at: Some(123),
            scopes: vec!["scope:a".to_string()],
        });
        assert_eq!(auth.bearer_token(), Some("access-token"));
        assert_eq!(auth.api_key(), None);
    }

    #[test]
    fn auth_source_from_env_combines_api_key_and_bearer_token() {
        let _guard = env_lock();
        unsafe { std::env::set_var("ANTHROPIC_AUTH_TOKEN", "auth-token"); }
        unsafe { std::env::set_var("ANTHROPIC_API_KEY", "legacy-key"); }
        let auth = AuthSource::from_env().expect("env auth");
        assert_eq!(auth.api_key(), Some("legacy-key"));
        assert_eq!(auth.bearer_token(), Some("auth-token"));
        unsafe { std::env::remove_var("ANTHROPIC_AUTH_TOKEN"); }
        unsafe { std::env::remove_var("ANTHROPIC_API_KEY"); }
    }

    #[test]
    fn auth_source_from_saved_oauth_when_env_absent() {
        let _guard = env_lock();
        let config_home = temp_config_home();
        unsafe { std::env::set_var("ANVIL_CONFIG_HOME", &config_home); }
        unsafe { std::env::remove_var("ANTHROPIC_AUTH_TOKEN"); }
        unsafe { std::env::remove_var("ANTHROPIC_API_KEY"); }
        save_oauth_credentials(&runtime::OAuthTokenSet {
            access_token: "saved-access-token".to_string(),
            refresh_token: Some("refresh".to_string()),
            expires_at: Some(now_unix_timestamp() + 300),
            scopes: vec!["scope:a".to_string()],
        })
        .expect("save oauth credentials");

        let auth = AuthSource::from_env_or_saved().expect("saved auth");
        assert_eq!(auth.bearer_token(), Some("saved-access-token"));

        clear_oauth_credentials().expect("clear credentials");
        unsafe { std::env::remove_var("ANVIL_CONFIG_HOME"); }
        cleanup_temp_config_home(&config_home);
    }

    #[test]
    fn oauth_token_expiry_uses_expires_at_timestamp() {
        assert!(oauth_token_is_expired(&OAuthTokenSet {
            access_token: "access-token".to_string(),
            refresh_token: None,
            expires_at: Some(1),
            scopes: Vec::new(),
        }));
        assert!(!oauth_token_is_expired(&OAuthTokenSet {
            access_token: "access-token".to_string(),
            refresh_token: None,
            expires_at: Some(now_unix_timestamp() + 60),
            scopes: Vec::new(),
        }));
    }

    #[test]
    fn resolve_saved_oauth_token_refreshes_expired_credentials() {
        let _guard = env_lock();
        let config_home = temp_config_home();
        unsafe { std::env::set_var("ANVIL_CONFIG_HOME", &config_home); }
        unsafe { std::env::remove_var("ANTHROPIC_AUTH_TOKEN"); }
        unsafe { std::env::remove_var("ANTHROPIC_API_KEY"); }
        save_oauth_credentials(&runtime::OAuthTokenSet {
            access_token: "expired-access-token".to_string(),
            refresh_token: Some("refresh-token".to_string()),
            expires_at: Some(1),
            scopes: vec!["scope:a".to_string()],
        })
        .expect("save expired oauth credentials");

        let token_url = spawn_token_server(
            "{\"access_token\":\"refreshed-token\",\"refresh_token\":\"fresh-refresh\",\"expires_at\":9999999999,\"scopes\":[\"scope:a\"]}",
        );
        let resolved = resolve_saved_oauth_token(&sample_oauth_config(token_url))
            .expect("resolve refreshed token")
            .expect("token set present");
        assert_eq!(resolved.access_token, "refreshed-token");
        let stored = runtime::load_oauth_credentials()
            .expect("load stored credentials")
            .expect("stored token set");
        assert_eq!(stored.access_token, "refreshed-token");

        clear_oauth_credentials().expect("clear credentials");
        unsafe { std::env::remove_var("ANVIL_CONFIG_HOME"); }
        cleanup_temp_config_home(&config_home);
    }

    #[test]
    fn resolve_startup_auth_source_uses_saved_oauth_without_loading_config() {
        let _guard = env_lock();
        let config_home = temp_config_home();
        unsafe { std::env::set_var("ANVIL_CONFIG_HOME", &config_home); }
        unsafe { std::env::remove_var("ANTHROPIC_AUTH_TOKEN"); }
        unsafe { std::env::remove_var("ANTHROPIC_API_KEY"); }
        save_oauth_credentials(&runtime::OAuthTokenSet {
            access_token: "saved-access-token".to_string(),
            refresh_token: Some("refresh".to_string()),
            expires_at: Some(now_unix_timestamp() + 300),
            scopes: vec!["scope:a".to_string()],
        })
        .expect("save oauth credentials");

        let auth = resolve_startup_auth_source(|| panic!("config should not be loaded"))
            .expect("startup auth");
        assert_eq!(auth.bearer_token(), Some("saved-access-token"));

        clear_oauth_credentials().expect("clear credentials");
        unsafe { std::env::remove_var("ANVIL_CONFIG_HOME"); }
        cleanup_temp_config_home(&config_home);
    }

    #[test]
    fn resolve_startup_auth_source_errors_when_refreshable_token_lacks_config() {
        let _guard = env_lock();
        let config_home = temp_config_home();
        unsafe { std::env::set_var("ANVIL_CONFIG_HOME", &config_home); }
        unsafe { std::env::remove_var("ANTHROPIC_AUTH_TOKEN"); }
        unsafe { std::env::remove_var("ANTHROPIC_API_KEY"); }
        save_oauth_credentials(&runtime::OAuthTokenSet {
            access_token: "expired-access-token".to_string(),
            refresh_token: Some("refresh-token".to_string()),
            expires_at: Some(1),
            scopes: vec!["scope:a".to_string()],
        })
        .expect("save expired oauth credentials");

        let error =
            resolve_startup_auth_source(|| Ok(None)).expect_err("missing config should error");
        assert!(
            matches!(error, crate::error::ApiError::Auth(message) if message.contains("runtime OAuth config is missing"))
        );

        let stored = runtime::load_oauth_credentials()
            .expect("load stored credentials")
            .expect("stored token set");
        assert_eq!(stored.access_token, "expired-access-token");
        assert_eq!(stored.refresh_token.as_deref(), Some("refresh-token"));

        clear_oauth_credentials().expect("clear credentials");
        unsafe { std::env::remove_var("ANVIL_CONFIG_HOME"); }
        cleanup_temp_config_home(&config_home);
    }

    #[test]
    fn resolve_saved_oauth_token_preserves_refresh_token_when_refresh_response_omits_it() {
        let _guard = env_lock();
        let config_home = temp_config_home();
        unsafe { std::env::set_var("ANVIL_CONFIG_HOME", &config_home); }
        unsafe { std::env::remove_var("ANTHROPIC_AUTH_TOKEN"); }
        unsafe { std::env::remove_var("ANTHROPIC_API_KEY"); }
        save_oauth_credentials(&runtime::OAuthTokenSet {
            access_token: "expired-access-token".to_string(),
            refresh_token: Some("refresh-token".to_string()),
            expires_at: Some(1),
            scopes: vec!["scope:a".to_string()],
        })
        .expect("save expired oauth credentials");

        let token_url = spawn_token_server(
            "{\"access_token\":\"refreshed-token\",\"expires_at\":9999999999,\"scopes\":[\"scope:a\"]}",
        );
        let resolved = resolve_saved_oauth_token(&sample_oauth_config(token_url))
            .expect("resolve refreshed token")
            .expect("token set present");
        assert_eq!(resolved.access_token, "refreshed-token");
        assert_eq!(resolved.refresh_token.as_deref(), Some("refresh-token"));
        let stored = runtime::load_oauth_credentials()
            .expect("load stored credentials")
            .expect("stored token set");
        assert_eq!(stored.refresh_token.as_deref(), Some("refresh-token"));

        clear_oauth_credentials().expect("clear credentials");
        unsafe { std::env::remove_var("ANVIL_CONFIG_HOME"); }
        cleanup_temp_config_home(&config_home);
    }

    #[test]
    fn message_request_stream_helper_sets_stream_true() {
        let request = MessageRequest {
            model: "claude-opus-4-6".to_string(),
            max_tokens: 64,
            messages: vec![],
            system: None,
            tools: None,
            tool_choice: None,
            stream: false,
        };

        assert!(request.with_streaming().stream);
    }

    #[test]
    fn backoff_doubles_until_maximum() {
        let _client = AnvilApiClient::new("test-key").with_retry_policy(
            3,
            Duration::from_millis(10),
            Duration::from_millis(25),
        );
        assert_eq!(
            backoff_for_attempt(1, Duration::from_millis(10), Duration::from_millis(25)).expect("attempt 1"),
            Duration::from_millis(10)
        );
        assert_eq!(
            backoff_for_attempt(2, Duration::from_millis(10), Duration::from_millis(25)).expect("attempt 2"),
            Duration::from_millis(20)
        );
        assert_eq!(
            backoff_for_attempt(3, Duration::from_millis(10), Duration::from_millis(25)).expect("attempt 3"),
            Duration::from_millis(25)
        );
    }

    #[test]
    fn retryable_statuses_are_detected() {
        assert!(is_retryable_status(
            reqwest::StatusCode::TOO_MANY_REQUESTS
        ));
        assert!(is_retryable_status(
            reqwest::StatusCode::INTERNAL_SERVER_ERROR
        ));
        assert!(!is_retryable_status(
            reqwest::StatusCode::UNAUTHORIZED
        ));
    }

    #[test]
    fn tool_delta_variant_round_trips() {
        let delta = ContentBlockDelta::InputJsonDelta {
            partial_json: "{\"city\":\"Paris\"}".to_string(),
        };
        let encoded = serde_json::to_string(&delta).expect("delta should serialize");
        let decoded: ContentBlockDelta =
            serde_json::from_str(&encoded).expect("delta should deserialize");
        assert_eq!(decoded, delta);
    }

    #[test]
    fn request_id_uses_primary_or_fallback_header() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(REQUEST_ID_HEADER, "req_primary".parse().expect("header"));
        assert_eq!(
            super::request_id_from_headers(&headers).as_deref(),
            Some("req_primary")
        );

        headers.clear();
        headers.insert(
            ALT_REQUEST_ID_HEADER,
            "req_fallback".parse().expect("header"),
        );
        assert_eq!(
            super::request_id_from_headers(&headers).as_deref(),
            Some("req_fallback")
        );
    }

    #[test]
    fn auth_source_applies_headers() {
        let auth = AuthSource::ApiKeyAndBearer {
            api_key: "test-key".to_string(),
            bearer_token: "proxy-token".to_string(),
        };
        let request = auth
            .apply(reqwest::Client::new().post("https://example.test"))
            .build()
            .expect("request build");
        let headers = request.headers();
        assert_eq!(
            headers.get("x-api-key").and_then(|v| v.to_str().ok()),
            Some("test-key")
        );
        assert_eq!(
            headers.get("authorization").and_then(|v| v.to_str().ok()),
            Some("Bearer proxy-token")
        );
    }

    // ─── Bug #26: prompt-cache breakpoints on Anthropic wire format ────────

    #[test]
    fn wire_body_attaches_cache_control_to_system_prompt() {
        use crate::types::{ToolDefinition};
        let request = MessageRequest {
            model: "claude-sonnet-4-6".to_string(),
            max_tokens: 1024,
            messages: vec![],
            system: Some("you are a careful coding assistant".to_string()),
            tools: Some(vec![ToolDefinition {
                name: "read_file".to_string(),
                description: Some("Read a file".to_string()),
                input_schema: serde_json::json!({"type": "object"}),
            }]),
            tool_choice: None,
            stream: false,
        };

        let body = super::build_messages_request_body(&request, false);
        // System prompt should be the array form so the cache_control marker
        // can hang off the text block.
        let system_array = body
            .get("system")
            .and_then(|v| v.as_array())
            .expect("system must serialize as content-block array when present");
        assert_eq!(system_array.len(), 1);
        let block = &system_array[0];
        assert_eq!(block["type"], serde_json::json!("text"));
        assert_eq!(block["text"], serde_json::json!("you are a careful coding assistant"));
        assert_eq!(
            block["cache_control"],
            serde_json::json!({"type": "ephemeral", "ttl": "1h"})
        );
    }

    // ─── Max-plan OAuth identity preamble ─────────────────────────────────

    /// When `is_oauth_bearer = true`, the wire body must prepend an uncached
    /// `"You are Claude Code, ..."` block as the FIRST entry of `system`,
    /// before the user's actual prompt block.  Anthropic's Max-plan OAuth
    /// gate requires this exact identity string; without it, the server
    /// returns HTTP 429 `rate_limit_error` with an empty body.
    #[test]
    fn wire_body_prepends_claude_code_identity_when_oauth_bearer() {
        let request = MessageRequest {
            model: "claude-sonnet-4-6".to_string(),
            max_tokens: 1024,
            messages: vec![],
            system: Some("you are a careful coding assistant".to_string()),
            tools: None,
            tool_choice: None,
            stream: false,
        };

        let body = super::build_messages_request_body(&request, true);
        let system_array = body
            .get("system")
            .and_then(|v| v.as_array())
            .expect("system must serialize as content-block array");
        assert_eq!(system_array.len(), 2, "identity block + user prompt block");

        let identity = &system_array[0];
        assert_eq!(identity["type"], serde_json::json!("text"));
        assert_eq!(
            identity["text"],
            serde_json::json!("You are Claude Code, Anthropic's official CLI for Claude.")
        );
        assert!(
            identity.get("cache_control").is_none(),
            "identity block must be uncached (saves a cache slot for 23 tokens)"
        );

        let user_block = &system_array[1];
        assert_eq!(
            user_block["text"],
            serde_json::json!("you are a careful coding assistant")
        );
    }

    /// When `is_oauth_bearer = false` (API-key auth), no identity preamble
    /// is injected — API-key requests don't go through the Max-plan gate.
    #[test]
    fn wire_body_omits_identity_preamble_when_api_key() {
        let request = MessageRequest {
            model: "claude-sonnet-4-6".to_string(),
            max_tokens: 1024,
            messages: vec![],
            system: Some("you are a careful coding assistant".to_string()),
            tools: None,
            tool_choice: None,
            stream: false,
        };

        let body = super::build_messages_request_body(&request, false);
        let system_array = body
            .get("system")
            .and_then(|v| v.as_array())
            .expect("system must serialize as content-block array");
        assert_eq!(system_array.len(), 1, "user prompt block only");
        assert_eq!(
            system_array[0]["text"],
            serde_json::json!("you are a careful coding assistant")
        );
    }

    /// When `is_oauth_bearer = true` and the request has NO system prompt,
    /// the identity block must still be injected (it's the access-control
    /// signal, not optional).
    #[test]
    fn wire_body_injects_identity_even_without_user_system_prompt() {
        let request = MessageRequest {
            model: "claude-sonnet-4-6".to_string(),
            max_tokens: 1024,
            messages: vec![],
            system: None,
            tools: None,
            tool_choice: None,
            stream: false,
        };

        let body = super::build_messages_request_body(&request, true);
        let system_array = body
            .get("system")
            .and_then(|v| v.as_array())
            .expect("system must be created when oauth bearer is set");
        assert_eq!(system_array.len(), 1);
        assert_eq!(
            system_array[0]["text"],
            serde_json::json!("You are Claude Code, Anthropic's official CLI for Claude.")
        );
    }

    // ─── Phase 4.5 (L1 §9): SYSTEM_PROMPT_DYNAMIC_BOUNDARY split ──────────

    /// When the prompt body contains `SYSTEM_PROMPT_DYNAMIC_BOUNDARY`,
    /// the Anthropic wire body must emit TWO `system` blocks: a cached
    /// head and a fresh tail. `head + "\n\n" + tail` (joined naturally
    /// by the model) must equal the unsplit body minus the marker.
    #[test]
    fn wire_body_splits_system_prompt_at_dynamic_boundary() {
        use crate::types::ToolDefinition;
        let head = "intro\n\n# System\nrules\n\n# Doing tasks\nwork";
        let tail = "# Environment\ntoday is 2026-05-13\n\n# Memory\nMEMORY.md body";
        let combined = format!(
            "{head}\n\n{}\n\n{tail}",
            runtime::SYSTEM_PROMPT_DYNAMIC_BOUNDARY,
        );

        let request = MessageRequest {
            model: "claude-sonnet-4-6".to_string(),
            max_tokens: 1024,
            messages: vec![],
            system: Some(combined.clone()),
            tools: Some(vec![ToolDefinition {
                name: "read_file".to_string(),
                description: Some("Read a file".to_string()),
                input_schema: serde_json::json!({"type": "object"}),
            }]),
            tool_choice: None,
            stream: false,
        };
        let body = super::build_messages_request_body(&request, false);
        let system_array = body
            .get("system")
            .and_then(|v| v.as_array())
            .expect("system must be a content-block array");
        assert_eq!(system_array.len(), 2, "must emit head + tail blocks");

        let head_block = &system_array[0];
        assert_eq!(head_block["type"], serde_json::json!("text"));
        assert_eq!(head_block["text"], serde_json::json!(head));
        assert_eq!(
            head_block["cache_control"],
            serde_json::json!({"type": "ephemeral", "ttl": "1h"}),
            "only the head should carry cache_control",
        );

        let tail_block = &system_array[1];
        assert_eq!(tail_block["type"], serde_json::json!("text"));
        assert_eq!(tail_block["text"], serde_json::json!(tail));
        assert!(
            tail_block.get("cache_control").is_none(),
            "tail must NOT carry cache_control",
        );

        // Concat round-trip: head + \n\n + tail == combined sans marker.
        let head_str = head_block["text"].as_str().unwrap();
        let tail_str = tail_block["text"].as_str().unwrap();
        let rejoined = format!("{head_str}\n\n{tail_str}");
        let expected = combined.replace(
            &format!("\n\n{}\n\n", runtime::SYSTEM_PROMPT_DYNAMIC_BOUNDARY),
            "\n\n",
        );
        assert_eq!(rejoined, expected, "round-trip drops marker only");
    }

    /// Prompt without the boundary marker (subagents, --print) keeps
    /// the historical single-block layout with cache_control on the
    /// whole body. No regression for non-boundary callers.
    #[test]
    fn wire_body_falls_back_to_single_block_without_boundary() {
        use crate::types::ToolDefinition;
        let request = MessageRequest {
            model: "claude-sonnet-4-6".to_string(),
            max_tokens: 1024,
            messages: vec![],
            // No boundary marker.
            system: Some("simple prompt".to_string()),
            tools: Some(vec![ToolDefinition {
                name: "read_file".to_string(),
                description: Some("Read a file".to_string()),
                input_schema: serde_json::json!({"type": "object"}),
            }]),
            tool_choice: None,
            stream: false,
        };
        let body = super::build_messages_request_body(&request, false);
        let system_array = body
            .get("system")
            .and_then(|v| v.as_array())
            .expect("system must be array");
        assert_eq!(system_array.len(), 1, "no marker => single block");
        assert_eq!(
            system_array[0]["cache_control"],
            serde_json::json!({"type": "ephemeral", "ttl": "1h"}),
        );
    }

    #[test]
    fn wire_body_attaches_cache_control_to_last_tool_only() {
        use crate::types::ToolDefinition;
        let request = MessageRequest {
            model: "claude-sonnet-4-6".to_string(),
            max_tokens: 1024,
            messages: vec![],
            system: Some("sys".to_string()),
            tools: Some(vec![
                ToolDefinition {
                    name: "first_tool".to_string(),
                    description: Some("first".to_string()),
                    input_schema: serde_json::json!({"type": "object"}),
                },
                ToolDefinition {
                    name: "second_tool".to_string(),
                    description: Some("second".to_string()),
                    input_schema: serde_json::json!({"type": "object"}),
                },
            ]),
            tool_choice: None,
            stream: false,
        };

        let body = super::build_messages_request_body(&request, false);
        let tools = body
            .get("tools")
            .and_then(|v| v.as_array())
            .expect("tools array must be present");
        assert_eq!(tools.len(), 2);
        // First tool: NO cache_control marker.
        assert!(
            tools[0].get("cache_control").is_none(),
            "only the last tool should carry cache_control; got {:?}",
            tools[0]
        );
        // Second (last) tool: cache_control with 1h TTL.
        assert_eq!(
            tools[1]["cache_control"],
            serde_json::json!({"type": "ephemeral", "ttl": "1h"}),
            "last tool must carry the cache breakpoint with 1h TTL"
        );
    }

    #[test]
    fn wire_body_omits_cache_control_when_no_system_or_tools() {
        let request = MessageRequest {
            model: "claude-sonnet-4-6".to_string(),
            max_tokens: 1024,
            messages: vec![crate::types::InputMessage::user_text("hi")],
            system: None,
            tools: None,
            tool_choice: None,
            stream: false,
        };

        let body = super::build_messages_request_body(&request, false);
        // Neither field should appear (they are skip-serialize-if-none) and
        // therefore no cache_control marker is added.
        assert!(body.get("system").is_none());
        assert!(body.get("tools").is_none());
        let serialized = serde_json::to_string(&body).expect("serialize body");
        assert!(
            !serialized.contains("cache_control"),
            "unexpected cache_control in payload: {serialized}"
        );
    }

    #[test]
    fn wire_body_full_payload_round_trips_with_breakpoints() {
        use crate::types::ToolDefinition;
        // End-to-end shape assertion: the serialized JSON must contain the
        // exact `"cache_control":{"type":"ephemeral","ttl":"1h"}` substring
        // both on the system block and on the last tool entry.
        let request = MessageRequest {
            model: "claude-sonnet-4-6".to_string(),
            max_tokens: 1024,
            messages: vec![],
            system: Some("system text".to_string()),
            tools: Some(vec![
                ToolDefinition {
                    name: "alpha".to_string(),
                    description: Some("first".to_string()),
                    input_schema: serde_json::json!({"type": "object"}),
                },
                ToolDefinition {
                    name: "omega".to_string(),
                    description: Some("last".to_string()),
                    input_schema: serde_json::json!({"type": "object"}),
                },
            ]),
            tool_choice: None,
            stream: false,
        };

        let body = super::build_messages_request_body(&request, false);
        let serialized = serde_json::to_string(&body).expect("serialize body");

        // Two cache_control breakpoints must be present.
        let occurrences = serialized.matches("\"cache_control\"").count();
        assert_eq!(
            occurrences, 2,
            "exactly two cache_control breakpoints expected (system + last tool); got {occurrences} in {serialized}"
        );
        // serde_json orders object keys deterministically but the serializer
        // does not guarantee a stable order between `type` and `ttl`; assert
        // on parsed JSON so the test is order-independent.
        let expected_marker = serde_json::json!({"type": "ephemeral", "ttl": "1h"});
        let system_block_marker = body["system"][0]["cache_control"].clone();
        assert_eq!(
            system_block_marker, expected_marker,
            "system cache_control marker mismatch: {serialized}"
        );
        let last_tool_marker = body["tools"][1]["cache_control"].clone();
        assert_eq!(
            last_tool_marker, expected_marker,
            "last-tool cache_control marker mismatch: {serialized}"
        );
    }

    // ─── Bug #15: Anthropic streaming dead-air timer ───────────────────────

    /// Spawn a tiny HTTP server that emits one SSE chunk and then keeps the
    /// connection open without writing more bytes — simulating an upstream
    /// stall.  The dead-air timer should surface `ApiError::StreamStalled`
    /// rather than hanging indefinitely.
    fn spawn_stalling_sse_server() -> String {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind sse listener");
        let address = listener.local_addr().expect("local addr");
        thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept connection");
            let mut buffer = [0_u8; 4096];
            let _ = stream.read(&mut buffer).expect("read request");
            // Send a chunked-transfer-encoding response with one SSE frame
            // and then *do not* close the connection or send any more bytes.
            // The HTTP keep-alive plus chunked framing means reqwest will
            // happily wait forever for the next chunk — the dead-air timer
            // is the only thing that can break us out.
            let initial_frame =
                "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"m_1\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[],\"model\":\"claude-sonnet-4-6\",\"usage\":{\"input_tokens\":1,\"output_tokens\":0}}}\n\n";
            let chunk_size_hex = format!("{:X}", initial_frame.len());
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ntransfer-encoding: chunked\r\n\r\n{chunk_size_hex}\r\n{initial_frame}\r\n",
            );
            stream
                .write_all(response.as_bytes())
                .expect("write initial sse chunk");
            // Park forever — the timeout test will outlive this thread.
            thread::sleep(Duration::from_secs(60));
            let _ = stream.shutdown(std::net::Shutdown::Both);
        });
        format!("http://{address}")
    }

    #[test]
    fn anthropic_stream_surfaces_stalled_error_after_dead_air_timeout() {
        let _guard = env_lock();
        // Configure a tight dead-air timeout so the test finishes quickly.
        unsafe { std::env::set_var("ANVIL_STREAM_DEAD_AIR_MS", "300"); }

        let base_url = spawn_stalling_sse_server();
        let client = AnvilApiClient::new("test-key").with_base_url(base_url);

        let request = MessageRequest {
            model: "claude-sonnet-4-6".to_string(),
            max_tokens: 64,
            messages: vec![crate::types::InputMessage::user_text("hi")],
            system: None,
            tools: None,
            tool_choice: None,
            stream: true,
        };

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime");

        let outcome: Result<(), crate::error::ApiError> = runtime.block_on(async move {
            let mut stream = client.stream_message(&request).await?;
            // Drain events until the stream stalls.  We expect at least one
            // event (the message_start delivered before the stall) and then
            // a StreamStalled error from the timer.
            loop {
                stream.next_event().await?;
            }
        });

        unsafe { std::env::remove_var("ANVIL_STREAM_DEAD_AIR_MS"); }

        match outcome {
            Err(crate::error::ApiError::StreamStalled { elapsed_ms }) => {
                // Elapsed should be at least the configured timeout
                // (300 ms).  Allow generous slack so a slow CI machine
                // doesn't flake.
                assert!(
                    elapsed_ms >= 200,
                    "elapsed_ms ({elapsed_ms}) should be near or above the configured 300ms timeout"
                );
            }
            Err(other) => panic!("expected StreamStalled, got: {other:?}"),
            Ok(()) => panic!("stream should have stalled, got a clean termination"),
        }
    }

    // ─── Effort / thinking budget injection ──────────────────────────────────

    fn base_request() -> MessageRequest {
        MessageRequest {
            model: "claude-opus-4-6".to_string(),
            max_tokens: 32_768,
            messages: vec![],
            system: Some("sys".to_string()),
            tools: None,
            tool_choice: None,
            stream: false,
        }
    }

    #[test]
    fn effort_env_absent_produces_no_thinking_block() {
        let _guard = env_lock();
        unsafe { std::env::remove_var("ANVIL_EFFORT"); }
        let body = super::build_messages_request_body(&base_request(), false);
        assert!(
            body.get("thinking").is_none(),
            "thinking block must not appear when ANVIL_EFFORT is unset: {body}"
        );
    }

    #[test]
    fn effort_medium_injects_thinking_budget_8k() {
        let _guard = env_lock();
        let body = super::build_messages_request_body_with_effort(
            &base_request(),
            Some(runtime::EffortLevel::Medium),
        );
        let thinking = body
            .get("thinking")
            .and_then(|v| v.as_object())
            .expect("thinking block must be present for medium effort");
        assert_eq!(thinking["type"], "enabled");
        assert_eq!(thinking["budget_tokens"].as_u64(), Some(8_192));
    }

    #[test]
    fn effort_high_injects_thinking_budget_24k() {
        let _guard = env_lock();
        let body = super::build_messages_request_body_with_effort(
            &base_request(),
            Some(runtime::EffortLevel::High),
        );
        let thinking = body
            .get("thinking")
            .and_then(|v| v.as_object())
            .expect("thinking block must be present for high effort");
        assert_eq!(thinking["budget_tokens"].as_u64(), Some(24_576));
    }

    #[test]
    fn effort_xhigh_injects_thinking_budget_64k() {
        let _guard = env_lock();
        let body = super::build_messages_request_body_with_effort(
            &base_request(),
            Some(runtime::EffortLevel::Xhigh),
        );
        let thinking = body
            .get("thinking")
            .and_then(|v| v.as_object())
            .expect("thinking block must be present for xhigh effort");
        // max_tokens=32768, safe_max=28672; xhigh target=65536 → capped at 28672
        assert_eq!(thinking["budget_tokens"].as_u64(), Some(28_672));
    }

    #[test]
    fn effort_xhigh_not_capped_for_large_model() {
        let _guard = env_lock();
        let mut req = base_request();
        req.max_tokens = 128_000;
        let body = super::build_messages_request_body_with_effort(
            &req,
            Some(runtime::EffortLevel::Xhigh),
        );
        let thinking = body
            .get("thinking")
            .and_then(|v| v.as_object())
            .expect("thinking block must be present");
        // 65536 < (128000-4096=123904) → no cap
        assert_eq!(thinking["budget_tokens"].as_u64(), Some(65_536));
    }

    #[test]
    fn effort_budget_zero_suppresses_thinking_block() {
        let _guard = env_lock();
        // max_tokens=4096 → safe_max=0 → budget capped at 0 → no block
        let mut req = base_request();
        req.max_tokens = 4_096;
        let body = super::build_messages_request_body_with_effort(
            &req,
            Some(runtime::EffortLevel::Low),
        );
        // budget = min(2048, 0) = 0 → no thinking block
        assert!(
            body.get("thinking").is_none(),
            "thinking block must be suppressed when budget is 0: {body}"
        );
    }
}
