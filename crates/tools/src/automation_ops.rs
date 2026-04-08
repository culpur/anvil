use reqwest::blocking::Client;
use runtime::CronManager;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::to_pretty_json;

// =============================================================================
// RemoteTrigger
// =============================================================================

#[derive(Debug, Deserialize)]
pub(crate) struct RemoteTriggerInput {
    pub url: String,
    pub prompt: String,
    pub model: Option<String>,
    pub session_id: Option<String>,
    pub api_key: Option<String>,
}

#[derive(Debug, Serialize)]
struct RemoteTriggerOutput {
    session_id: String,
    url: String,
    prompt_sent: String,
    status: u16,
    response: Value,
}

#[allow(clippy::needless_pass_by_value)]
pub(crate) fn run_remote_trigger(input: RemoteTriggerInput) -> Result<String, String> {
    // Validate that the target URL uses HTTPS to prevent SSRF to internal
    // endpoints or cleartext HTTP downgrade attacks.
    if !input.url.starts_with("https://") {
        return Err(format!(
            "RemoteTrigger: URL must use https (got: {})",
            input.url
        ));
    }

    let base = input.url.trim_end_matches('/');

    let client = Client::new();

    // Step 1: resolve or create a session.
    let session_id: String = if let Some(sid) = input.session_id {
        sid
    } else {
        // POST /sessions → { session_id }
        let mut req = client.post(format!("{base}/sessions"));
        if let Some(ref key) = input.api_key {
            req = req.bearer_auth(key);
        }
        // Optionally forward model hint as a query param if the server supports it.
        if let Some(ref model) = input.model {
            req = req.query(&[("model", model.as_str())]);
        }
        let resp = req
            .send()
            .map_err(|e| format!("RemoteTrigger: failed to create session: {e}"))?;
        let status = resp.status().as_u16();
        let body: Value = resp
            .json()
            .map_err(|e| format!("RemoteTrigger: invalid JSON from /sessions: {e}"))?;
        if status != 201 {
            return Err(format!(
                "RemoteTrigger: /sessions returned HTTP {status}: {body}"
            ));
        }
        body.get("session_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "RemoteTrigger: no session_id in response".to_string())?
            .to_string()
    };

    // Step 2: POST /sessions/{id}/message
    let msg_url = format!("{base}/sessions/{session_id}/message");
    let mut msg_req = client
        .post(&msg_url)
        .json(&serde_json::json!({ "message": input.prompt }));
    if let Some(ref key) = input.api_key {
        msg_req = msg_req.bearer_auth(key);
    }
    let msg_resp = msg_req
        .send()
        .map_err(|e| format!("RemoteTrigger: failed to send message: {e}"))?;
    let msg_status = msg_resp.status().as_u16();

    // The send-message endpoint returns 204 No Content on success.
    let response_body: Value = if msg_status == 204 {
        serde_json::json!({ "sent": true })
    } else {
        msg_resp
            .json()
            .unwrap_or_else(|_| serde_json::json!({ "raw": "non-JSON body" }))
    };

    to_pretty_json(RemoteTriggerOutput {
        session_id,
        url: input.url,
        prompt_sent: input.prompt,
        status: msg_status,
        response: response_body,
    })
}

// =============================================================================
// Cron tools
// =============================================================================

#[derive(Debug, Deserialize)]
pub(crate) struct CronCreateInput {
    pub cron_expression: String,
    pub prompt: String,
    pub name: Option<String>,
    pub target_url: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct CronDeleteInput {
    pub cron_id: String,
}

#[allow(clippy::needless_pass_by_value)]
pub(crate) fn run_cron_create(input: CronCreateInput) -> Result<String, String> {
    let mut mgr = CronManager::global()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let id = mgr.create(
        input.cron_expression,
        input.prompt,
        input.name,
        input.target_url,
    )?;
    let entry = mgr
        .get(&id)
        .ok_or_else(|| String::from("cron entry vanished after creation"))?
        .clone();
    to_pretty_json(entry)
}

pub(crate) fn run_cron_list() -> Result<String, String> {
    let mgr = CronManager::global()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    to_pretty_json(mgr.list())
}

#[allow(clippy::needless_pass_by_value)]
pub(crate) fn run_cron_delete(input: CronDeleteInput) -> Result<String, String> {
    let mut mgr = CronManager::global()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    mgr.delete(&input.cron_id)?;
    to_pretty_json(serde_json::json!({ "deleted": input.cron_id }))
}
