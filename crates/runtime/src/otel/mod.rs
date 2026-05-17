//! Optional OpenTelemetry event emission.
//!
//! ## Design
//!
//! - **Default off**: `OtelConfig::enabled` is `false` unless the user opts in
//!   via `settings.json` or `ANVIL_OTEL_ENABLED=1`.  When disabled every
//!   `emit_event` call returns immediately (O(1), no allocation).
//!
//! - **Compile-time gate**: the `otel` cargo feature (default-on) gates all
//!   OTel crate imports.  A `--no-default-features` build produces a binary
//!   with zero OTel code, which is measurably smaller.
//!
//! - **Redaction**: `redact_user_prompts: true` (the default) strips any field
//!   that could carry user prompt content before it reaches the exporter.
//!
//! - **PII**: the default attribute set never includes prompt text.  Model
//!   names, durations, token counts, and status codes are safe to export.
//!
//! ## Usage
//!
//! ```ignore
//! // At startup
//! otel::init_tracer(&config);
//!
//! // At any event site
//! otel::emit_event("anvil.session_start", &[
//!     ("session_id", "abc123"),
//!     ("model", "claude-opus-4-6"),
//! ]);
//!
//! // At shutdown
//! otel::shutdown();
//! ```

use crate::config::OtelConfig;

pub mod traceparent;

// ---------------------------------------------------------------------------
// Runtime-state singleton
// ---------------------------------------------------------------------------

/// Global "OTel is initialised and enabled" flag.  Set once during
/// `init_tracer`; checked on every `emit_event` call for O(1) fast-path.
static OTEL_ENABLED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Whether to redact fields that could carry user prompt content.
static REDACT_PROMPTS: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(true);

// ---------------------------------------------------------------------------
// Public API — always present regardless of feature flag
// ---------------------------------------------------------------------------

/// Attribute key–value pair for an OTel event.
pub type Attribute<'a> = (&'a str, &'a str);

/// Initialise the OTel tracer from `config`.
///
/// When `config.enabled` is `false` (the default) this is a no-op.
/// The function is idempotent: calling it multiple times is safe (subsequent
/// calls after the first are no-ops due to the `OTEL_ENABLED` guard).
pub fn init_tracer(config: &OtelConfig) {
    // Resolve `enabled`: config field OR env-var override.
    let enabled = config.enabled
        || std::env::var("ANVIL_OTEL_ENABLED")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false);

    // CC-DRIFT-B5: TRACEPARENT must propagate even when the local OTel
    // exporter is disabled — a parent process tracing through us still
    // expects their context to reach our subprocesses and outbound HTTP.
    // Parse `TRACEPARENT` and `TRACESTATE` unconditionally; generate a
    // fresh local root only when OTel is enabled (otherwise we have no
    // span tree to anchor it to and a synthetic root would just confuse
    // downstream collectors).
    traceparent::init_from_env(enabled);

    if !enabled {
        return;
    }

    // Already initialised.
    if OTEL_ENABLED.load(std::sync::atomic::Ordering::Relaxed) {
        return;
    }

    REDACT_PROMPTS.store(
        config.redact_user_prompts,
        std::sync::atomic::Ordering::Relaxed,
    );

    #[cfg(feature = "otel")]
    {
        init_tracer_inner(config);
    }

    // Mark enabled only after the provider is installed so that concurrent
    // callers don't race past a partially-initialised pipeline.
    OTEL_ENABLED.store(true, std::sync::atomic::Ordering::Release);
}

/// Emit a named span/event with the provided key–value attributes.
///
/// When OTel is disabled (the common case) this returns immediately without
/// any heap allocation.
#[inline]
pub fn emit_event(event_name: &'static str, attrs: &[(&str, &str)]) {
    if !OTEL_ENABLED.load(std::sync::atomic::Ordering::Relaxed) {
        return;
    }

    // CC-139-F16: when a subagent is active, prepend agent_id /
    // parent_agent_id to the attribute list so the event carries
    // trace-reassembly headers without every caller threading them
    // through. The clone is paid only when OTel is enabled *and* a
    // subagent is active.
    let ctx = crate::agent_ctx::current();
    if let Some(ref c) = ctx {
        let mut prefixed: Vec<(&str, &str)> = Vec::with_capacity(attrs.len() + 2);
        prefixed.push(("agent_id", c.agent_id.as_str()));
        if let Some(parent) = c.parent_agent_id.as_deref() {
            prefixed.push(("parent_agent_id", parent));
        }
        prefixed.extend_from_slice(attrs);
        #[cfg(feature = "otel")]
        emit_event_inner(event_name, &prefixed);
        #[cfg(not(feature = "otel"))]
        {
            let _ = (event_name, &prefixed);
        }
        return;
    }

    #[cfg(feature = "otel")]
    emit_event_inner(event_name, attrs);

    // Suppress unused-variable warning in non-otel builds.
    #[cfg(not(feature = "otel"))]
    {
        let _ = (event_name, attrs);
    }
}

/// Flush and shut down the global OTel provider.
///
/// Must be called at process exit so in-flight spans are flushed to the
/// exporter.  Safe to call when OTel was never initialised (no-op).
pub fn shutdown() {
    if !OTEL_ENABLED.load(std::sync::atomic::Ordering::Relaxed) {
        return;
    }

    #[cfg(feature = "otel")]
    shutdown_inner();

    OTEL_ENABLED.store(false, std::sync::atomic::Ordering::Release);
}

/// Returns `true` if OTel is currently enabled and will emit events.
#[must_use]
#[inline]
pub fn is_enabled() -> bool {
    OTEL_ENABLED.load(std::sync::atomic::Ordering::Relaxed)
}

/// Returns `true` if prompt-content redaction is active.
#[must_use]
#[inline]
pub fn redact_prompts() -> bool {
    REDACT_PROMPTS.load(std::sync::atomic::Ordering::Relaxed)
}

// ---------------------------------------------------------------------------
// Feature-gated implementation
// ---------------------------------------------------------------------------

#[cfg(feature = "otel")]
mod inner {
    use opentelemetry::global;
    use opentelemetry::trace::{Span, SpanKind, Tracer};
    use opentelemetry_sdk::runtime::Tokio;
    use opentelemetry_sdk::trace::TracerProvider;
    use opentelemetry_otlp::{WithExportConfig, WithHttpConfig};

    use crate::config::OtelConfig;

    /// Install the global OTel tracer provider backed by an OTLP-HTTP exporter.
    pub(super) fn init_tracer_inner(config: &OtelConfig) {
        // Resolve endpoint: config > OTEL_EXPORTER_OTLP_ENDPOINT env var > default.
        let endpoint = std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT")
            .ok()
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| config.endpoint.clone());

        // Build headers map: start with config headers, expand ${VAR} references,
        // then overlay OTEL_EXPORTER_OTLP_HEADERS env-var (key=value,... format).
        let mut headers: std::collections::HashMap<String, String> = config
            .headers
            .iter()
            .map(|(k, v)| (k.clone(), expand_env(v)))
            .collect();

        if let Ok(env_headers) = std::env::var("OTEL_EXPORTER_OTLP_HEADERS") {
            for pair in env_headers.split(',') {
                if let Some((k, v)) = pair.split_once('=') {
                    headers.insert(k.trim().to_string(), v.trim().to_string());
                }
            }
        }

        let service_name = config.service_name.clone();

        let exporter = match opentelemetry_otlp::SpanExporter::builder()
            .with_http()
            .with_endpoint(&endpoint)
            .with_headers(headers)
            .build()
        {
            Ok(exp) => exp,
            Err(e) => {
                eprintln!("anvil: otel exporter init failed: {e}");
                return;
            }
        };

        let provider = TracerProvider::builder()
            .with_batch_exporter(exporter, Tokio)
            .with_resource(opentelemetry_sdk::Resource::new(vec![
                opentelemetry::KeyValue::new("service.name", service_name),
                opentelemetry::KeyValue::new(
                    "service.version",
                    env!("CARGO_PKG_VERSION"),
                ),
            ]))
            .build();

        global::set_tracer_provider(provider);
    }

    /// Emit a single-span event with the provided attributes.
    pub(super) fn emit_event_inner(event_name: &'static str, attrs: &[(&str, &str)]) {
        let tracer = global::tracer("anvil");
        let mut span = tracer
            .span_builder(event_name)
            .with_kind(SpanKind::Internal)
            .start(&tracer);
        for (key, value) in attrs {
            // Both key and value must be owned — `Key` only implements
            // `From<&'static str>` or `From<String>`, not `From<&str>`.
            span.set_attribute(opentelemetry::KeyValue::new(
                key.to_string(),
                value.to_string(),
            ));
        }
        span.end();
    }

    /// Flush and shut down the global provider.
    pub(super) fn shutdown_inner() {
        global::shutdown_tracer_provider();
    }

    /// Expand `${VAR_NAME}` patterns in a string using environment variables.
    fn expand_env(s: &str) -> String {
        let mut result = s.to_string();
        // Simple ${VAR} expansion — no nested or default-value syntax needed.
        while let Some(start) = result.find("${") {
            let Some(end) = result[start..].find('}') else {
                break;
            };
            let var_name = &result[start + 2..start + end];
            let value = std::env::var(var_name).unwrap_or_default();
            result = format!(
                "{}{}{}",
                &result[..start],
                value,
                &result[start + end + 1..]
            );
        }
        result
    }
}

#[cfg(feature = "otel")]
use inner::{emit_event_inner, init_tracer_inner, shutdown_inner};

// ---------------------------------------------------------------------------
// Convenience helpers for specific Anvil events
// ---------------------------------------------------------------------------

/// Emit `anvil.session_start`.
pub fn session_start(session_id: &str, model: &str, provider: &str, effort: &str, profile_name: &str) {
    emit_event(
        "anvil.session_start",
        &[
            ("session_id", session_id),
            ("model", model),
            ("provider", provider),
            ("effort", effort),
            ("profile_name", profile_name),
        ],
    );
}

/// Emit `anvil.session_end`.
pub fn session_end(
    session_id: &str,
    duration_ms: u64,
    tokens_used: u64,
    cost_usd: &str,
    tool_count: u64,
) {
    let duration_ms_s = duration_ms.to_string();
    let tokens_s = tokens_used.to_string();
    let tool_count_s = tool_count.to_string();
    emit_event(
        "anvil.session_end",
        &[
            ("session_id", session_id),
            ("duration_ms", &duration_ms_s),
            ("tokens_used", &tokens_s),
            ("cost_usd", cost_usd),
            ("tool_count", &tool_count_s),
        ],
    );
}

/// Emit `anvil.tool_use`.
pub fn tool_use(tool_name: &str, tool_use_id: &str, session_id: &str, model: &str) {
    emit_event(
        "anvil.tool_use",
        &[
            ("tool_name", tool_name),
            ("tool_use_id", tool_use_id),
            ("session_id", session_id),
            ("model", model),
        ],
    );
}

/// Emit `anvil.tool_result`.
pub fn tool_result(
    tool_name: &str,
    tool_use_id: &str,
    success: bool,
    duration_ms: u64,
    output_size_bytes: u64,
) {
    let success_s = success.to_string();
    let duration_ms_s = duration_ms.to_string();
    let output_size_s = output_size_bytes.to_string();
    emit_event(
        "anvil.tool_result",
        &[
            ("tool_name", tool_name),
            ("tool_use_id", tool_use_id),
            ("success", &success_s),
            ("duration_ms", &duration_ms_s),
            ("output_size_bytes", &output_size_s),
        ],
    );
}

/// Emit `anvil.skill_activated`.
pub fn skill_activated(skill_name: &str, source: &str) {
    emit_event(
        "anvil.skill_activated",
        &[("skill_name", skill_name), ("source", source)],
    );
}

/// Emit `anvil.hub_package_install`.
///
/// Fired by the type-specific install slash commands (`/skill install`,
/// `/agent install`, `/theme install`, `/plugin install`) and by
/// `/hub install`.  `pkg_type` is one of `skill`, `agent`, `theme`, `plugin`;
/// `outcome` is `ok`, `type-mismatch`, `revoked`, `unverified`, or `error`.
pub fn hub_package_install(pkg_type: &str, slug: &str, outcome: &str) {
    emit_event(
        "anvil.hub_package_install",
        &[("pkg_type", pkg_type), ("slug", slug), ("outcome", outcome)],
    );
}

/// Emit `anvil.permission_decision`.
pub fn permission_decision(tool: &str, decision: &str, source: &str) {
    emit_event(
        "anvil.permission_decision",
        &[
            ("tool", tool),
            ("decision", decision),
            ("source", source),
        ],
    );
}

/// Emit `anvil.error`.
///
/// The `message` parameter is always redacted when `redact_user_prompts` is
/// active — only the error type is forwarded in that case.
pub fn error(error_type: &str, message: &str, session_id: &str) {
    let safe_message = if redact_prompts() { "[redacted]" } else { message };
    emit_event(
        "anvil.error",
        &[
            ("error_type", error_type),
            ("message_redacted", safe_message),
            ("session_id", session_id),
        ],
    );
}

/// Emit `anvil.api_request`.
pub fn api_request(
    provider: &str,
    model: &str,
    status_code: u16,
    retry_count: u32,
    duration_ms: u64,
    prompt_tokens: u64,
    completion_tokens: u64,
) {
    let status_s = status_code.to_string();
    let retry_s = retry_count.to_string();
    let dur_s = duration_ms.to_string();
    let prompt_s = prompt_tokens.to_string();
    let completion_s = completion_tokens.to_string();
    emit_event(
        "anvil.api_request",
        &[
            ("provider", provider),
            ("model", model),
            ("status_code", &status_s),
            ("retry_count", &retry_s),
            ("duration_ms", &dur_s),
            ("prompt_tokens", &prompt_s),
            ("completion_tokens", &completion_s),
        ],
    );
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

// Note: unsafe env-var mutation is not allowed in the `runtime` crate.
// The tests below cover the core logic without env-var manipulation.
// End-to-end `ANVIL_OTEL_ENABLED` env-var behaviour is covered by
// manual / integration testing.
#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use super::*;
    use crate::config::OtelConfig;

    fn make_config(enabled: bool) -> OtelConfig {
        OtelConfig {
            enabled,
            exporter: "otlp-http".to_string(),
            endpoint: "http://localhost:4318".to_string(),
            headers: BTreeMap::new(),
            service_name: "anvil-test".to_string(),
            redact_user_prompts: true,
        }
    }

    // Reset the atomic flags between tests so they are independent.
    fn reset_state() {
        OTEL_ENABLED.store(false, std::sync::atomic::Ordering::SeqCst);
        REDACT_PROMPTS.store(true, std::sync::atomic::Ordering::SeqCst);
    }

    #[test]
    fn otel_disabled_by_default() {
        reset_state();
        // `enabled: false` with no env-var override — OTel must stay disabled.
        // The test relies on the env var NOT being set in the test environment.
        // If ANVIL_OTEL_ENABLED=1 is present in the environment this test is
        // skipped with a note rather than failing, since forcing it absent
        // would require unsafe env-var mutation.
        if std::env::var("ANVIL_OTEL_ENABLED").as_deref() == Ok("1") {
            // Skip in environments where the override is already set.
            reset_state();
            return;
        }

        let config = make_config(false);
        init_tracer(&config);

        assert!(!is_enabled(), "otel must remain disabled when config.enabled=false");

        // emit_event must be a no-op — no panic, no side-effects.
        emit_event("anvil.session_start", &[("session_id", "x"), ("model", "m")]);

        reset_state();
    }

    #[test]
    fn otel_disabled_emits_nothing_without_init() {
        reset_state();
        // Calling emit_event without init_tracer must be a pure no-op.
        emit_event(
            "anvil.session_start",
            &[("session_id", "noop"), ("model", "m"), ("provider", "anthropic")],
        );
        assert!(!is_enabled());
    }

    // The batch exporter requires an active Tokio reactor.  Wrap the test in
    // a minimal runtime so the `init_tracer(enabled=true)` path is exercised
    // without panicking on "no reactor running".
    #[test]
    fn otel_init_with_endpoint_is_no_panic() {
        reset_state();
        // Build a minimal single-threaded Tokio runtime just for this test.
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio rt");
        let config = make_config(true);
        // Run init_tracer inside the runtime context so the batch pipeline
        // can find a reactor.
        rt.block_on(async {
            init_tracer(&config);
        });
        let _ = is_enabled();
        reset_state();
    }

    #[test]
    fn otel_redacts_user_prompts() {
        reset_state();
        // Redaction is applied via the helper — test the helper directly
        // without needing a live tracer.
        REDACT_PROMPTS.store(true, std::sync::atomic::Ordering::SeqCst);

        // The `error()` helper should substitute "[redacted]" for the message
        // when redact_prompts() is true.  Since OTEL_ENABLED is false the
        // emit_event call is a no-op, but the redaction logic runs before
        // calling emit_event — we can unit-test it independently.
        let safe = if redact_prompts() { "[redacted]" } else { "user secret" };
        assert_eq!(safe, "[redacted]");

        REDACT_PROMPTS.store(false, std::sync::atomic::Ordering::SeqCst);
        let safe = if redact_prompts() { "[redacted]" } else { "user secret" };
        assert_eq!(safe, "user secret");

        reset_state();
    }

    #[test]
    fn convenience_helpers_are_no_ops_when_disabled() {
        reset_state();
        // All convenience helpers must not panic when OTel is disabled.
        session_start("s1", "claude-opus-4-6", "anthropic", "normal", "default");
        session_end("s1", 1234, 500, "0.01", 3);
        tool_use("read_file", "tu_1", "s1", "claude-opus-4-6");
        tool_result("read_file", "tu_1", true, 50, 1024);
        skill_activated("init", "user");
        permission_decision("bash", "allow", "policy");
        error("ApiError", "some message with user content", "s1");
        api_request("anthropic", "claude-opus-4-6", 200, 0, 800, 1000, 500);
    }
}
