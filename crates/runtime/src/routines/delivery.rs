//! Delivery targets for routine output.
//!
//! Every routine writes an archive entry locally (the `Local` target, always
//! enabled — see [`super::definition::DeliveryTarget`]).  Beyond the local
//! archive a routine may forward its output to one or more **webhooks**.
//!
//! ## Webhook contract
//!
//! - Method: `POST` by default; `GET`/`PUT`/`PATCH` accepted.
//! - Headers: `Content-Type: application/json`, `User-Agent:
//!   anvil-routines/<version>`.
//! - Body: a JSON serialisation of [`WebhookPayload`] — the same shape across
//!   every routine so consumers only have to learn one schema.
//! - URLs starting with `vault://path/to/secret` are resolved by the caller
//!   via the unlocked vault before [`deliver_webhook`] runs.  The delivery
//!   layer itself never touches the vault; it expects a resolved URL.
//!
//! ## Timeouts and retries
//!
//! - 15-second hard timeout per request.
//! - One retry on a connection-refused or 5xx response, after a 1-second
//!   back-off.  We deliberately keep this small — routines fire on a
//!   schedule, so a transient outage just delays delivery to the next tick.
//!
//! ## `[SILENT]` suppression
//!
//! When the routine's output contains [`super::SILENT_MARKER`], the local
//! archive is still written but **every** webhook is skipped (the call site
//! decides whether to even invoke this module).
//!
//! ## Why blocking, not async
//!
//! The executor is itself synchronous — it spawns `anvil -p` as a subprocess
//! and waits for it to exit before invoking delivery.  Reusing a tokio
//! runtime here just to fire one HTTPS request would add complexity without
//! benefit.  We use `reqwest::blocking`.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::routines::archive::RunStatus;
use crate::routines::definition::DeliveryTarget;
use crate::routines::packet::{compute_input_hash, PacketStatus, RoutinePacket};

/// Common JSON envelope POSTed to every webhook.  Schema is intentionally
/// flat and stable — third-party automations (n8n, Zapier, etc.) parse this
/// without a custom integration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebhookPayload {
    /// Routine name, e.g. `"release-watch"`.
    pub routine: String,
    /// Unix seconds when the run started.
    pub started_at: u64,
    /// Unix seconds when the run finished.
    pub finished_at: u64,
    /// Final status of the run.
    pub status: WebhookStatus,
    /// One-paragraph summary lifted from the packet (or first line if the
    /// model didn't emit a structured summary).
    pub summary: String,
    /// Full LLM output body — without packet delimiters.  May be large; the
    /// receiver decides whether to truncate.
    pub body: String,
    /// SHA-256 of the input fingerprint (`prompt + schedule + context_from`).
    /// Stable across re-runs with the same inputs.  Useful for downstream
    /// deduplication.
    pub input_hash: String,
    /// Anvil version of the routines daemon that produced this delivery.
    pub anvil_version: String,
}

/// Wire form of the status field — mirrors [`RunStatus`] / [`PacketStatus`]
/// but renames the success variant to read naturally in JSON
/// (`"success"` vs the internal `"clean"`).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WebhookStatus {
    Success,
    /// Exited 0 but output contained `[SILENT]`; archive was still written.
    Silent,
    Failed,
}

impl From<RunStatus> for WebhookStatus {
    fn from(s: RunStatus) -> Self {
        match s {
            RunStatus::Clean => Self::Success,
            RunStatus::Silent => Self::Silent,
            RunStatus::Failed => Self::Failed,
        }
    }
}

impl From<PacketStatus> for WebhookStatus {
    fn from(s: PacketStatus) -> Self {
        match s {
            PacketStatus::Clean => Self::Success,
            PacketStatus::Silent => Self::Silent,
            PacketStatus::Failed => Self::Failed,
        }
    }
}

/// Errors surfaced by [`deliver_webhook`].  Local archive failures are
/// surfaced separately by the executor (it owns archive writes).
#[derive(Debug)]
pub enum DeliveryError {
    Build(String),
    Http(String),
    Status(u16, String),
    Timeout,
}

impl std::fmt::Display for DeliveryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Build(m) => write!(f, "request build failed: {m}"),
            Self::Http(m) => write!(f, "transport error: {m}"),
            Self::Status(s, b) => {
                let preview: String = b.chars().take(120).collect();
                write!(f, "webhook returned HTTP {s}: {preview}")
            }
            Self::Timeout => write!(f, "webhook timed out after 15s"),
        }
    }
}

impl std::error::Error for DeliveryError {}

/// Build a [`WebhookPayload`] from the parts the executor already has.
///
/// `body` is the raw LLM output (without packet delimiters).  `summary` is
/// the first-paragraph summary extracted by
/// [`super::packet::extract_summary`] when present, else a truncated copy of
/// the body's first line.
///
/// The `input_hash` is computed from `(system_prompt, user_prompt,
/// script_output)` — matching [`compute_input_hash`] — so two runs with the
/// same inputs always produce the same hash, regardless of when they fired
/// or what time-of-day scheduling pushed them.  `script_output` is `None`
/// for plain prompt routines; routines that pipe a shell script's stdout
/// into the prompt pass that stdout here.
#[must_use]
pub fn build_payload(
    routine: &str,
    started_at: u64,
    finished_at: u64,
    status: WebhookStatus,
    summary: &str,
    body: &str,
    system_prompt: &str,
    user_prompt: &str,
    script_output: Option<&str>,
    anvil_version: &str,
) -> WebhookPayload {
    let input_hash = compute_input_hash(system_prompt, user_prompt, script_output);
    WebhookPayload {
        routine: routine.to_string(),
        started_at,
        finished_at,
        status,
        summary: summary.to_string(),
        body: body.to_string(),
        input_hash,
        anvil_version: anvil_version.to_string(),
    }
}

/// Resolve `url` (which may be a `vault://` reference) into a plain
/// `https://…` / `http://localhost…` URL.  This helper is intentionally
/// agnostic about *how* vault resolution works — the caller passes a closure
/// that maps a `vault://path` string to the underlying secret.  Returning
/// `Err` here short-circuits the delivery so we never accidentally POST a
/// literal `vault://…` URL to a real server.
pub fn resolve_url<F>(url: &str, resolver: F) -> Result<String, DeliveryError>
where
    F: FnOnce(&str) -> Option<String>,
{
    if let Some(path) = url.strip_prefix("vault://") {
        let resolved = resolver(path).ok_or_else(|| {
            DeliveryError::Build(format!(
                "vault key `{path}` not found (is the vault unlocked?)"
            ))
        })?;
        if resolved.starts_with("vault://") {
            return Err(DeliveryError::Build(
                "vault key resolved to another vault:// URL — refusing to follow".to_string(),
            ));
        }
        Ok(resolved)
    } else {
        Ok(url.to_string())
    }
}

/// POST (or GET/PUT/PATCH) `payload` to `resolved_url`.  Pure transport — the
/// caller has already resolved any `vault://` indirection.
///
/// `method` is uppercase.  Anything other than `GET`/`POST`/`PUT`/`PATCH`
/// is rejected at definition load time so it can't reach this function.
pub fn deliver_webhook(
    resolved_url: &str,
    method: &str,
    payload: &WebhookPayload,
) -> Result<(), DeliveryError> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(15))
        .user_agent(format!(
            "anvil-routines/{}",
            payload.anvil_version
        ))
        .build()
        .map_err(|e| DeliveryError::Build(e.to_string()))?;

    let body = serde_json::to_vec(payload)
        .map_err(|e| DeliveryError::Build(format!("serialize payload: {e}")))?;

    let send_once = || -> Result<(), DeliveryError> {
        let req = match method {
            "GET" => client.get(resolved_url),
            "PUT" => client.put(resolved_url).body(body.clone()),
            "PATCH" => client.patch(resolved_url).body(body.clone()),
            _ => client.post(resolved_url).body(body.clone()),
        }
        .header("Content-Type", "application/json");

        match req.send() {
            Ok(resp) => {
                let status = resp.status();
                if status.is_success() {
                    Ok(())
                } else {
                    let preview = resp.text().unwrap_or_default();
                    Err(DeliveryError::Status(status.as_u16(), preview))
                }
            }
            Err(e) if e.is_timeout() => Err(DeliveryError::Timeout),
            Err(e) => Err(DeliveryError::Http(e.to_string())),
        }
    };

    match send_once() {
        Ok(()) => Ok(()),
        // 4xx is a contract problem, not transient — surface it immediately.
        Err(e @ DeliveryError::Status(s, _)) if s < 500 => Err(e),
        Err(_) => {
            std::thread::sleep(Duration::from_secs(1));
            send_once()
        }
    }
}

/// Dispatch a finished routine run to every delivery target on the routine.
/// Returns a vector of per-target outcomes so the executor can log which
/// webhooks failed without aborting the whole run.
///
/// `Local` targets are a no-op here — the executor writes the archive
/// directly before calling this function so the archive exists even if every
/// webhook fails.
pub fn dispatch<F>(
    targets: &[DeliveryTarget],
    payload: &WebhookPayload,
    silent: bool,
    vault_resolver: &F,
) -> Vec<TargetOutcome>
where
    F: Fn(&str) -> Option<String>,
{
    let mut outcomes = Vec::with_capacity(targets.len());
    for target in targets {
        match target {
            DeliveryTarget::Local => {
                outcomes.push(TargetOutcome {
                    kind: "local".into(),
                    ok: true,
                    error: None,
                });
            }
            DeliveryTarget::Webhook { url, method } => {
                if silent {
                    outcomes.push(TargetOutcome {
                        kind: "webhook".into(),
                        ok: true,
                        error: Some("skipped: [SILENT]".to_string()),
                    });
                    continue;
                }
                let resolved = match resolve_url(url, |p| vault_resolver(p)) {
                    Ok(u) => u,
                    Err(e) => {
                        outcomes.push(TargetOutcome {
                            kind: "webhook".into(),
                            ok: false,
                            error: Some(e.to_string()),
                        });
                        continue;
                    }
                };
                match deliver_webhook(&resolved, method, payload) {
                    Ok(()) => outcomes.push(TargetOutcome {
                        kind: "webhook".into(),
                        ok: true,
                        error: None,
                    }),
                    Err(e) => outcomes.push(TargetOutcome {
                        kind: "webhook".into(),
                        ok: false,
                        error: Some(e.to_string()),
                    }),
                }
            }
        }
    }
    outcomes
}

/// Per-target dispatch result.  The executor logs every entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TargetOutcome {
    pub kind: String,
    pub ok: bool,
    pub error: Option<String>,
}

/// Convert a [`RoutinePacket`] into a [`WebhookPayload`].  Convenience for
/// call sites that already have a packet on hand (e.g. `/schedule run-now`
/// re-dispatch).
#[must_use]
pub fn payload_from_packet(packet: &RoutinePacket, anvil_version: &str) -> WebhookPayload {
    WebhookPayload {
        routine: packet.routine_id.clone(),
        started_at: packet.started_at,
        finished_at: packet.ended_at,
        status: packet.status.into(),
        summary: packet.summary.clone(),
        body: packet.body.clone(),
        input_hash: packet.input_hash.clone(),
        anvil_version: anvil_version.to_string(),
    }
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_url_passthrough_https() {
        let out = resolve_url("https://example.com/x", |_| Some("nope".into())).unwrap();
        assert_eq!(out, "https://example.com/x");
    }

    #[test]
    fn resolve_url_passthrough_localhost() {
        let out = resolve_url("http://localhost:9000/x", |_| None).unwrap();
        assert_eq!(out, "http://localhost:9000/x");
    }

    #[test]
    fn resolve_url_vault_resolves() {
        let out = resolve_url("vault://routines/x/webhook", |path| {
            assert_eq!(path, "routines/x/webhook");
            Some("https://hooks.example.com/abc".to_string())
        })
        .unwrap();
        assert_eq!(out, "https://hooks.example.com/abc");
    }

    #[test]
    fn resolve_url_vault_miss_errors() {
        let err = resolve_url("vault://routines/x/webhook", |_| None).unwrap_err();
        match err {
            DeliveryError::Build(m) => assert!(m.contains("vault key")),
            other => panic!("expected Build, got {other:?}"),
        }
    }

    #[test]
    fn resolve_url_vault_nested_rejected() {
        let err = resolve_url("vault://x", |_| Some("vault://y".to_string())).unwrap_err();
        match err {
            DeliveryError::Build(m) => assert!(m.contains("another vault://")),
            other => panic!("expected Build, got {other:?}"),
        }
    }

    #[test]
    fn dispatch_silent_skips_webhook_but_records_local() {
        let targets = vec![
            DeliveryTarget::Local,
            DeliveryTarget::Webhook {
                url: "https://example.com/h".to_string(),
                method: "POST".to_string(),
            },
        ];
        let payload = WebhookPayload {
            routine: "x".into(),
            started_at: 0,
            finished_at: 0,
            status: WebhookStatus::Silent,
            summary: String::new(),
            body: String::new(),
            input_hash: String::new(),
            anvil_version: "0.0.0".into(),
        };
        let out = dispatch(&targets, &payload, true, &|_| None);
        assert_eq!(out.len(), 2);
        assert!(out.iter().any(|o| o.kind == "local" && o.ok));
        assert!(out.iter().any(|o| o.kind == "webhook"
            && o.ok
            && o.error.as_deref() == Some("skipped: [SILENT]")));
    }

    #[test]
    fn build_payload_includes_input_hash() {
        let p = build_payload(
            "r",
            100,
            200,
            WebhookStatus::Success,
            "sum",
            "body",
            "you are a routine runner",
            "do the thing",
            None,
            "1.2.3",
        );
        assert_eq!(p.routine, "r");
        assert!(!p.input_hash.is_empty());
        // Same inputs → same hash even though timestamps/status/body differ.
        let p2 = build_payload(
            "r",
            999,
            1000,
            WebhookStatus::Failed,
            "x",
            "y",
            "you are a routine runner",
            "do the thing",
            None,
            "9.9.9",
        );
        assert_eq!(p.input_hash, p2.input_hash);
    }

    #[test]
    fn webhook_status_serializes_snake_case() {
        let json = serde_json::to_string(&WebhookStatus::Success).unwrap();
        assert_eq!(json, "\"success\"");
        let json = serde_json::to_string(&WebhookStatus::Silent).unwrap();
        assert_eq!(json, "\"silent\"");
        let json = serde_json::to_string(&WebhookStatus::Failed).unwrap();
        assert_eq!(json, "\"failed\"");
    }
}
