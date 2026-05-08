use std::ffi::OsStr;
use std::process::Command;
use std::sync::Arc;

use plugins::{interpolate, HookInterpolationContext, HookSpec};
use serde_json::{json, Value as JsonValue};

use crate::config::{RuntimeFeatureConfig, RuntimeHookConfig};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookEvent {
    PreToolUse,
    PostToolUse,
    /// Fires after config + MCP servers are loaded, before the first user prompt.
    SessionStart,
    /// Fires on clean exit (Ctrl+C/Ctrl+D, /exit, normal shutdown).
    SessionEnd,
    /// Fires after Edit/Write/MultiEdit tool succeeds.
    /// Payload: `{ path, action: "edit"|"write"|"create"|"delete" }`.
    FileChanged,
    /// Fires when cwd changes mid-session.
    /// Payload: `{ old_cwd, new_cwd }`.
    CwdChanged,
    /// Fires when a permission prompt is about to be shown to the user.
    /// Payload: `{ tool, input, requested_mode }`.
    /// Hook may return `{ "permissionDecision": "allow|deny|ask|defer" }` to
    /// short-circuit the prompt.
    PermissionRequest,
    /// Fires after a tool call is denied (by hook, user, or sandbox).
    /// Payload: `{ tool, input, reason, source: "hook"|"user"|"sandbox" }`.
    PermissionDenied,
    /// Fires once per parallel tool batch after all tools complete.
    /// Payload: `{ tool_count, durations_ms, success_count, failure_count }`.
    PostToolBatch,
    /// Fires when Anvil displays a notification to the user.
    /// Payload: `{ kind: "permission_prompt"|"error"|"completion"|"info", message }`.
    Notification,
}

// ---------------------------------------------------------------------------
// Payload structs for the new events.
// ---------------------------------------------------------------------------

/// Payload for `HookEvent::FileChanged`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileChangedPayload {
    pub path: String,
    pub action: FileChangeAction,
}

/// The kind of file operation that produced a `FileChanged` event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileChangeAction {
    Edit,
    Write,
    Create,
    Delete,
}

impl FileChangeAction {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Edit => "edit",
            Self::Write => "write",
            Self::Create => "create",
            Self::Delete => "delete",
        }
    }
}

/// Payload for `HookEvent::CwdChanged`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CwdChangedPayload {
    pub old_cwd: String,
    pub new_cwd: String,
}

/// Payload for `HookEvent::PermissionRequest`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionRequestPayload {
    pub tool: String,
    pub input: String,
    pub requested_mode: String,
}

/// Decision injected by a `PermissionRequest` hook.
/// Only `PermissionRequest` supports decision injection; all other new
/// events are strictly observe-only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HookPermissionDecision {
    Allow,
    Deny,
    Ask,
    Defer,
}

impl HookPermissionDecision {
    /// Parse the `permissionDecision` field from hook stdout JSON.
    #[must_use]
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "allow" => Some(Self::Allow),
            "deny" => Some(Self::Deny),
            "ask" => Some(Self::Ask),
            "defer" => Some(Self::Defer),
            _ => None,
        }
    }
}

/// Result of running `PermissionRequest` hooks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionRequestHookResult {
    /// Injected decision, if any hook returned one.  `None` means fall
    /// through to the normal interactive prompt.
    pub decision: Option<HookPermissionDecision>,
    pub messages: Vec<String>,
}

/// Payload for `HookEvent::PermissionDenied`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionDeniedPayload {
    pub tool: String,
    pub input: String,
    pub reason: String,
    pub source: PermissionDeniedSource,
}

/// Who or what denied the tool call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionDeniedSource {
    Hook,
    User,
    Sandbox,
}

impl PermissionDeniedSource {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Hook => "hook",
            Self::User => "user",
            Self::Sandbox => "sandbox",
        }
    }
}

/// Payload for `HookEvent::PostToolBatch`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PostToolBatchPayload {
    pub tool_count: usize,
    /// Wall-clock duration in ms.  For parallel batches this is the total
    /// elapsed time (not per-tool); for single-tool batches it is the
    /// single tool's duration.
    pub durations_ms: Vec<u64>,
    pub success_count: usize,
    pub failure_count: usize,
}

/// The kind of notification being shown to the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotificationKind {
    PermissionPrompt,
    Error,
    Completion,
    Info,
}

impl NotificationKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PermissionPrompt => "permission_prompt",
            Self::Error => "error",
            Self::Completion => "completion",
            Self::Info => "info",
        }
    }
}

/// Payload for `HookEvent::Notification`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotificationPayload {
    pub kind: NotificationKind,
    pub message: String,
}

// ---------------------------------------------------------------------------
// RuntimeHookSpec — runtime-side superset of plugins::HookSpec
// ---------------------------------------------------------------------------
//
// This enum is the runtime-side hook descriptor used by HookRunner.  It is a
// superset of `plugins::HookSpec` (Command + Tagged{Command|Prompt}) and adds
// the `McpTool` variant introduced for Claude Code v2.1.118 parity (FEAT-30):
// hooks may dispatch directly to an MCP tool instead of forking a shell.
//
// JSON shape (Claude Code parity):
//   { "type": "mcp_tool", "server": "myserver", "tool": "redact",
//     "input": { "key": "value" } }
//
// TODO(stream-b): config/hooks.rs::parse_hook_spec_array currently deserializes
// each entry as `plugins::HookSpec` (Command|Tagged).  To wire the parser side
// for `type: "mcp_tool"`, Stream B's partial-tolerance rewrite needs to:
//   1. Switch the per-element parse target to `RuntimeHookSpec` (this enum).
//   2. Migrate `RuntimeHookConfig` to hold `Vec<RuntimeHookSpec>` instead of
//      `Vec<plugins::HookSpec>`.
// Until that lands, `RuntimeHookSpec::McpTool` is reachable only via direct
// construction (e.g. tests / programmatic callers via
// `HookRunner::from_runtime_specs`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeHookSpec {
    /// Pass-through for the existing plugins-side variants (Command / Prompt).
    Plugin(HookSpec),
    /// Invoke an MCP tool directly instead of running a shell command.
    McpTool {
        server: String,
        tool: String,
        input: JsonValue,
    },
}

impl RuntimeHookSpec {
    /// Borrow-conversion from a plugins-side `HookSpec` (the legacy shape).
    #[must_use]
    pub fn from_plugin(spec: &HookSpec) -> Self {
        Self::Plugin(spec.clone())
    }
}

impl From<HookSpec> for RuntimeHookSpec {
    fn from(spec: HookSpec) -> Self {
        Self::Plugin(spec)
    }
}

/// Result returned by an MCP tool invocation triggered from a hook.
///
/// The hook runner treats the textual `output` analogously to a shell hook's
/// stdout: non-empty content is appended to the message stream so callers
/// (e.g. PostToolUse → updatedToolOutput) can use it.  An `is_error: true`
/// result is mapped to a warning, never a hard deny — hook-driven MCP calls
/// are explicitly best-effort, matching the constraint that an unavailable
/// MCP server must not crash the turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpHookInvocationResult {
    pub output: String,
    pub is_error: bool,
}

impl McpHookInvocationResult {
    #[must_use]
    pub const fn ok(output: String) -> Self {
        Self {
            output,
            is_error: false,
        }
    }

    #[must_use]
    pub const fn error(output: String) -> Self {
        Self {
            output,
            is_error: true,
        }
    }
}

/// Sync invoker trait so a tokio-driven MCP server manager can be adapted
/// (block_on / channel) without dragging async into HookRunner.  Tests provide
/// a tiny in-process implementation.  Production callers wire this to the
/// real MCP server registry.
///
/// Contract:
/// - `Ok(Some(result))` — server + tool resolved, call returned a result.
/// - `Ok(None)` — server unknown, tool unknown, or call elected to no-op.
///   The runner treats this as a silent skip (no message).
/// - `Err(message)` — transport / protocol failure.  Mapped to a warning,
///   never a deny.
pub trait McpHookInvoker: Send + Sync {
    fn invoke(
        &self,
        server: &str,
        tool: &str,
        input: &JsonValue,
    ) -> Result<Option<McpHookInvocationResult>, String>;
}

impl HookEvent {
    const fn as_str(self) -> &'static str {
        match self {
            Self::PreToolUse => "PreToolUse",
            Self::PostToolUse => "PostToolUse",
            Self::SessionStart => "SessionStart",
            Self::SessionEnd => "SessionEnd",
            Self::FileChanged => "FileChanged",
            Self::CwdChanged => "CwdChanged",
            Self::PermissionRequest => "PermissionRequest",
            Self::PermissionDenied => "PermissionDenied",
            Self::PostToolBatch => "PostToolBatch",
            Self::Notification => "Notification",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookRunResult {
    denied: bool,
    messages: Vec<String>,
}

impl HookRunResult {
    #[must_use]
    pub const fn allow(messages: Vec<String>) -> Self {
        Self {
            denied: false,
            messages,
        }
    }

    #[must_use]
    pub const fn is_denied(&self) -> bool {
        self.denied
    }

    #[must_use]
    pub fn messages(&self) -> &[String] {
        &self.messages
    }
}

#[derive(Clone, Default)]
pub struct HookRunner {
    config: RuntimeHookConfig,
    /// Extra runtime-only hook specs (e.g. `McpTool`) appended after the
    /// config-derived ones.  Stream B's parser rewrite will fold this back
    /// into `RuntimeHookConfig` once it migrates to `Vec<RuntimeHookSpec>`.
    pre_tool_use_extra: Vec<RuntimeHookSpec>,
    post_tool_use_extra: Vec<RuntimeHookSpec>,
    // v2.2.11 new event extras.
    session_start_extra: Vec<RuntimeHookSpec>,
    session_end_extra: Vec<RuntimeHookSpec>,
    file_changed_extra: Vec<RuntimeHookSpec>,
    cwd_changed_extra: Vec<RuntimeHookSpec>,
    permission_request_extra: Vec<RuntimeHookSpec>,
    permission_denied_extra: Vec<RuntimeHookSpec>,
    post_tool_batch_extra: Vec<RuntimeHookSpec>,
    notification_extra: Vec<RuntimeHookSpec>,
    mcp_invoker: Option<Arc<dyn McpHookInvoker>>,
}

impl std::fmt::Debug for HookRunner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HookRunner")
            .field("config", &self.config)
            .field("pre_tool_use_extra", &self.pre_tool_use_extra)
            .field("post_tool_use_extra", &self.post_tool_use_extra)
            .field("session_start_extra", &self.session_start_extra)
            .field("session_end_extra", &self.session_end_extra)
            .field("file_changed_extra", &self.file_changed_extra)
            .field("cwd_changed_extra", &self.cwd_changed_extra)
            .field("permission_request_extra", &self.permission_request_extra)
            .field("permission_denied_extra", &self.permission_denied_extra)
            .field("post_tool_batch_extra", &self.post_tool_batch_extra)
            .field("notification_extra", &self.notification_extra)
            .field("mcp_invoker", &self.mcp_invoker.as_ref().map(|_| "<dyn McpHookInvoker>"))
            .finish()
    }
}

impl PartialEq for HookRunner {
    fn eq(&self, other: &Self) -> bool {
        self.config == other.config
            && self.pre_tool_use_extra == other.pre_tool_use_extra
            && self.post_tool_use_extra == other.post_tool_use_extra
            && self.session_start_extra == other.session_start_extra
            && self.session_end_extra == other.session_end_extra
            && self.file_changed_extra == other.file_changed_extra
            && self.cwd_changed_extra == other.cwd_changed_extra
            && self.permission_request_extra == other.permission_request_extra
            && self.permission_denied_extra == other.permission_denied_extra
            && self.post_tool_batch_extra == other.post_tool_batch_extra
            && self.notification_extra == other.notification_extra
    }
}
impl Eq for HookRunner {}

#[derive(Debug, Clone, Copy)]
struct HookCommandRequest<'a> {
    event: HookEvent,
    tool_name: &'a str,
    tool_input: &'a str,
    tool_output: Option<&'a str>,
    is_error: bool,
    payload: &'a str,
}

impl HookRunner {
    #[must_use]
    pub const fn new(config: RuntimeHookConfig) -> Self {
        Self {
            config,
            pre_tool_use_extra: Vec::new(),
            post_tool_use_extra: Vec::new(),
            session_start_extra: Vec::new(),
            session_end_extra: Vec::new(),
            file_changed_extra: Vec::new(),
            cwd_changed_extra: Vec::new(),
            permission_request_extra: Vec::new(),
            permission_denied_extra: Vec::new(),
            post_tool_batch_extra: Vec::new(),
            notification_extra: Vec::new(),
            mcp_invoker: None,
        }
    }

    #[must_use]
    pub fn from_feature_config(feature_config: &RuntimeFeatureConfig) -> Self {
        Self::new(feature_config.hooks().clone())
    }

    /// Construct a runner directly from runtime-side specs.  Used by tests and
    /// programmatic callers that want to inject `RuntimeHookSpec::McpTool`
    /// entries before Stream B's parser rewrite plumbs them through config.
    #[must_use]
    pub fn from_runtime_specs(
        pre_tool_use: Vec<RuntimeHookSpec>,
        post_tool_use: Vec<RuntimeHookSpec>,
    ) -> Self {
        Self {
            config: RuntimeHookConfig::default(),
            pre_tool_use_extra: pre_tool_use,
            post_tool_use_extra: post_tool_use,
            session_start_extra: Vec::new(),
            session_end_extra: Vec::new(),
            file_changed_extra: Vec::new(),
            cwd_changed_extra: Vec::new(),
            permission_request_extra: Vec::new(),
            permission_denied_extra: Vec::new(),
            post_tool_batch_extra: Vec::new(),
            notification_extra: Vec::new(),
            mcp_invoker: None,
        }
    }

    /// Attach an MCP invoker so `RuntimeHookSpec::McpTool` entries dispatch
    /// to a live MCP server registry.  Without an invoker, MCP-tool hooks
    /// log a warning and are treated as a no-op (per FEAT-30 constraint).
    #[must_use]
    pub fn with_mcp_invoker(mut self, invoker: Arc<dyn McpHookInvoker>) -> Self {
        self.mcp_invoker = Some(invoker);
        self
    }

    #[must_use]
    pub fn run_pre_tool_use(&self, tool_name: &str, tool_input: &str) -> HookRunResult {
        let specs = self.collect_specs(HookEvent::PreToolUse);
        self.run_commands(
            HookEvent::PreToolUse,
            &specs,
            tool_name,
            tool_input,
            None,
            false,
        )
    }

    #[must_use]
    pub fn run_post_tool_use(
        &self,
        tool_name: &str,
        tool_input: &str,
        tool_output: &str,
        is_error: bool,
    ) -> HookRunResult {
        let specs = self.collect_specs(HookEvent::PostToolUse);
        self.run_commands(
            HookEvent::PostToolUse,
            &specs,
            tool_name,
            tool_input,
            Some(tool_output),
            is_error,
        )
    }

    // ─── New v2.2.11 dispatch methods ───────────────────────────────────────

    /// Fire all SessionStart hooks (observe-only).
    pub fn run_session_start(&self) -> HookRunResult {
        let specs = self.collect_specs(HookEvent::SessionStart);
        if specs.is_empty() {
            return HookRunResult::allow(Vec::new());
        }
        let payload = json!({ "hook_event_name": HookEvent::SessionStart.as_str() }).to_string();
        self.run_observe_only(HookEvent::SessionStart, &specs, &payload)
    }

    /// Fire all SessionEnd hooks (observe-only).
    pub fn run_session_end(&self) -> HookRunResult {
        let specs = self.collect_specs(HookEvent::SessionEnd);
        if specs.is_empty() {
            return HookRunResult::allow(Vec::new());
        }
        let payload = json!({ "hook_event_name": HookEvent::SessionEnd.as_str() }).to_string();
        self.run_observe_only(HookEvent::SessionEnd, &specs, &payload)
    }

    /// Fire all FileChanged hooks (observe-only).
    pub fn run_file_changed(&self, p: &FileChangedPayload) -> HookRunResult {
        let specs = self.collect_specs(HookEvent::FileChanged);
        if specs.is_empty() {
            return HookRunResult::allow(Vec::new());
        }
        let payload = json!({
            "hook_event_name": HookEvent::FileChanged.as_str(),
            "path": p.path,
            "action": p.action.as_str(),
        })
        .to_string();
        self.run_observe_only(HookEvent::FileChanged, &specs, &payload)
    }

    /// Fire all CwdChanged hooks (observe-only).
    pub fn run_cwd_changed(&self, p: &CwdChangedPayload) -> HookRunResult {
        let specs = self.collect_specs(HookEvent::CwdChanged);
        if specs.is_empty() {
            return HookRunResult::allow(Vec::new());
        }
        let payload = json!({
            "hook_event_name": HookEvent::CwdChanged.as_str(),
            "old_cwd": p.old_cwd,
            "new_cwd": p.new_cwd,
        })
        .to_string();
        self.run_observe_only(HookEvent::CwdChanged, &specs, &payload)
    }

    /// Fire all PermissionRequest hooks.  The first hook that returns a valid
    /// `permissionDecision` in its JSON stdout wins; remaining hooks still run
    /// (observe semantics for them).
    pub fn run_permission_request(
        &self,
        p: &PermissionRequestPayload,
    ) -> PermissionRequestHookResult {
        let specs = self.collect_specs(HookEvent::PermissionRequest);
        if specs.is_empty() {
            return PermissionRequestHookResult {
                decision: None,
                messages: Vec::new(),
            };
        }
        let payload = json!({
            "hook_event_name": HookEvent::PermissionRequest.as_str(),
            "tool": p.tool,
            "input": p.input,
            "requested_mode": p.requested_mode,
        })
        .to_string();

        let ctx = HookInterpolationContext {
            tool_name: Some(p.tool.clone()),
            tool_input: Some(p.input.clone()),
            cwd: std::env::current_dir().ok().map(|path| path.display().to_string()),
            date: Some(current_date_iso()),
            model: None,
        };

        let mut messages = Vec::new();
        let mut decision: Option<HookPermissionDecision> = None;

        for spec in &specs {
            let outcome = match spec {
                RuntimeHookSpec::Plugin(plugin) if plugin.is_prompt() => {
                    run_prompt_spec(plugin, HookEvent::PermissionRequest, &p.tool, &ctx)
                }
                RuntimeHookSpec::Plugin(plugin) => Self::run_command(
                    plugin.body(),
                    HookCommandRequest {
                        event: HookEvent::PermissionRequest,
                        tool_name: &p.tool,
                        tool_input: &p.input,
                        tool_output: None,
                        is_error: false,
                        payload: &payload,
                    },
                ),
                RuntimeHookSpec::McpTool { server, tool, input } => self.run_mcp_tool_spec(
                    HookEvent::PermissionRequest,
                    &p.tool,
                    server,
                    tool,
                    input,
                ),
            };
            match outcome {
                HookCommandOutcome::Allow { message } => {
                    if decision.is_none() {
                        if let Some(ref stdout) = message {
                            if let Ok(val) = serde_json::from_str::<serde_json::Value>(stdout) {
                                if let Some(d) = val
                                    .get("permissionDecision")
                                    .and_then(|v| v.as_str())
                                    .and_then(HookPermissionDecision::from_str)
                                {
                                    decision = Some(d);
                                }
                            }
                        }
                    }
                    if let Some(msg) = message {
                        messages.push(msg);
                    }
                }
                // Exit code 2 on PermissionRequest = inject deny decision.
                HookCommandOutcome::Deny { message } => {
                    if decision.is_none() {
                        decision = Some(HookPermissionDecision::Deny);
                    }
                    if let Some(msg) = message {
                        messages.push(msg);
                    }
                }
                HookCommandOutcome::Warn { message } => messages.push(message),
            }
        }

        PermissionRequestHookResult { decision, messages }
    }

    /// Fire all PermissionDenied hooks (observe-only).
    pub fn run_permission_denied(&self, p: &PermissionDeniedPayload) -> HookRunResult {
        let specs = self.collect_specs(HookEvent::PermissionDenied);
        if specs.is_empty() {
            return HookRunResult::allow(Vec::new());
        }
        let payload = json!({
            "hook_event_name": HookEvent::PermissionDenied.as_str(),
            "tool": p.tool,
            "input": p.input,
            "reason": p.reason,
            "source": p.source.as_str(),
        })
        .to_string();
        self.run_observe_only(HookEvent::PermissionDenied, &specs, &payload)
    }

    /// Fire all PostToolBatch hooks (observe-only).
    pub fn run_post_tool_batch(&self, p: &PostToolBatchPayload) -> HookRunResult {
        let specs = self.collect_specs(HookEvent::PostToolBatch);
        if specs.is_empty() {
            return HookRunResult::allow(Vec::new());
        }
        let payload = json!({
            "hook_event_name": HookEvent::PostToolBatch.as_str(),
            "tool_count": p.tool_count,
            "durations_ms": p.durations_ms,
            "success_count": p.success_count,
            "failure_count": p.failure_count,
        })
        .to_string();
        self.run_observe_only(HookEvent::PostToolBatch, &specs, &payload)
    }

    /// Fire all Notification hooks (observe-only).
    pub fn run_notification(&self, p: &NotificationPayload) -> HookRunResult {
        let specs = self.collect_specs(HookEvent::Notification);
        if specs.is_empty() {
            return HookRunResult::allow(Vec::new());
        }
        let payload = json!({
            "hook_event_name": HookEvent::Notification.as_str(),
            "kind": p.kind.as_str(),
            "message": p.message,
        })
        .to_string();
        self.run_observe_only(HookEvent::Notification, &specs, &payload)
    }

    /// Shared observe-only dispatcher: exit code 2 is demoted to a warning
    /// because none of the new events support deny semantics.
    fn run_observe_only(
        &self,
        event: HookEvent,
        specs: &[RuntimeHookSpec],
        payload: &str,
    ) -> HookRunResult {
        let ctx = HookInterpolationContext {
            tool_name: None,
            tool_input: None,
            cwd: std::env::current_dir().ok().map(|p| p.display().to_string()),
            date: Some(current_date_iso()),
            model: None,
        };

        let mut messages = Vec::new();

        for spec in specs {
            let outcome = match spec {
                RuntimeHookSpec::Plugin(plugin) if plugin.is_prompt() => {
                    run_prompt_spec(plugin, event, event.as_str(), &ctx)
                }
                RuntimeHookSpec::Plugin(plugin) => Self::run_command(
                    plugin.body(),
                    HookCommandRequest {
                        event,
                        tool_name: event.as_str(),
                        tool_input: "{}",
                        tool_output: None,
                        is_error: false,
                        payload,
                    },
                ),
                RuntimeHookSpec::McpTool { server, tool, input } => {
                    self.run_mcp_tool_spec(event, event.as_str(), server, tool, input)
                }
            };
            match outcome {
                HookCommandOutcome::Allow { message } => {
                    if let Some(msg) = message {
                        messages.push(msg);
                    }
                }
                // Observe-only: exit 2 is a warning, not a deny.
                HookCommandOutcome::Deny { message } => {
                    let msg = message.unwrap_or_else(|| {
                        format!(
                            "{} hook exited 2 but this event is observe-only; treating as warning",
                            event.as_str()
                        )
                    });
                    messages.push(msg);
                }
                HookCommandOutcome::Warn { message } => messages.push(message),
            }
        }

        HookRunResult::allow(messages)
    }

    /// Merge the config-derived plugin-side specs with runtime-only extras
    /// (e.g. `McpTool`) into a single dispatch list.  Plugin specs come first
    /// to preserve existing ordering semantics for callers.
    fn collect_specs(&self, event: HookEvent) -> Vec<RuntimeHookSpec> {
        let (config_specs, extras): (&[HookSpec], &Vec<RuntimeHookSpec>) = match event {
            HookEvent::PreToolUse => (self.config.pre_tool_use(), &self.pre_tool_use_extra),
            HookEvent::PostToolUse => (self.config.post_tool_use(), &self.post_tool_use_extra),
            HookEvent::SessionStart => (self.config.session_start(), &self.session_start_extra),
            HookEvent::SessionEnd => (self.config.session_end(), &self.session_end_extra),
            HookEvent::FileChanged => (self.config.file_changed(), &self.file_changed_extra),
            HookEvent::CwdChanged => (self.config.cwd_changed(), &self.cwd_changed_extra),
            HookEvent::PermissionRequest => {
                (self.config.permission_request(), &self.permission_request_extra)
            }
            HookEvent::PermissionDenied => {
                (self.config.permission_denied(), &self.permission_denied_extra)
            }
            HookEvent::PostToolBatch => {
                (self.config.post_tool_batch(), &self.post_tool_batch_extra)
            }
            HookEvent::Notification => (self.config.notification(), &self.notification_extra),
        };
        let mut out = Vec::with_capacity(config_specs.len() + extras.len());
        out.extend(config_specs.iter().map(RuntimeHookSpec::from_plugin));
        out.extend(extras.iter().cloned());
        out
    }

    fn run_commands(
        &self,
        event: HookEvent,
        specs: &[RuntimeHookSpec],
        tool_name: &str,
        tool_input: &str,
        tool_output: Option<&str>,
        is_error: bool,
    ) -> HookRunResult {
        if specs.is_empty() {
            return HookRunResult::allow(Vec::new());
        }

        let payload = json!({
            "hook_event_name": event.as_str(),
            "tool_name": tool_name,
            "tool_input": parse_tool_input(tool_input),
            "tool_input_json": tool_input,
            "tool_output": tool_output,
            "tool_result_is_error": is_error,
        })
        .to_string();

        let ctx = HookInterpolationContext {
            tool_name: Some(tool_name.to_string()),
            tool_input: Some(tool_input.to_string()),
            cwd: std::env::current_dir()
                .ok()
                .map(|p| p.display().to_string()),
            date: Some(current_date_iso()),
            model: None,
        };

        let mut messages = Vec::new();

        for spec in specs {
            let outcome = match spec {
                RuntimeHookSpec::Plugin(plugin) if plugin.is_prompt() => {
                    run_prompt_spec(plugin, event, tool_name, &ctx)
                }
                RuntimeHookSpec::Plugin(plugin) => Self::run_command(
                    plugin.body(),
                    HookCommandRequest {
                        event,
                        tool_name,
                        tool_input,
                        tool_output,
                        is_error,
                        payload: &payload,
                    },
                ),
                RuntimeHookSpec::McpTool {
                    server,
                    tool,
                    input,
                } => self.run_mcp_tool_spec(event, tool_name, server, tool, input),
            };
            match outcome {
                HookCommandOutcome::Allow { message } => {
                    if let Some(message) = message {
                        messages.push(message);
                    }
                }
                HookCommandOutcome::Deny { message } => {
                    let message = message.unwrap_or_else(|| {
                        format!("{} hook denied tool `{tool_name}`", event.as_str())
                    });
                    messages.push(message);
                    return HookRunResult {
                        denied: true,
                        messages,
                    };
                }
                HookCommandOutcome::Warn { message } => messages.push(message),
            }
        }

        HookRunResult::allow(messages)
    }

    /// Dispatch a `RuntimeHookSpec::McpTool` entry through the registered
    /// invoker.  Per FEAT-30 contract: any failure (no invoker, unknown
    /// server/tool, transport error) is a warning, never a deny — MCP-driven
    /// hooks must not crash the turn.
    fn run_mcp_tool_spec(
        &self,
        event: HookEvent,
        tool_name: &str,
        server: &str,
        tool: &str,
        input: &JsonValue,
    ) -> HookCommandOutcome {
        let Some(invoker) = self.mcp_invoker.as_ref() else {
            return HookCommandOutcome::Warn {
                message: format!(
                    "{} mcp_tool hook `{server}:{tool}` skipped for `{tool_name}`: no MCP invoker registered",
                    event.as_str()
                ),
            };
        };

        match invoker.invoke(server, tool, input) {
            Ok(Some(result)) => {
                let trimmed = result.output.trim().to_string();
                if result.is_error {
                    HookCommandOutcome::Warn {
                        message: format!(
                            "{} mcp_tool hook `{server}:{tool}` reported error for `{tool_name}`: {}",
                            event.as_str(),
                            if trimmed.is_empty() { "<no output>" } else { trimmed.as_str() }
                        ),
                    }
                } else {
                    HookCommandOutcome::Allow {
                        message: (!trimmed.is_empty()).then_some(trimmed),
                    }
                }
            }
            Ok(None) => HookCommandOutcome::Warn {
                message: format!(
                    "{} mcp_tool hook `{server}:{tool}` for `{tool_name}` resolved to no-op (server or tool unavailable)",
                    event.as_str()
                ),
            },
            Err(error) => HookCommandOutcome::Warn {
                message: format!(
                    "{} mcp_tool hook `{server}:{tool}` failed for `{tool_name}`: {error}",
                    event.as_str()
                ),
            },
        }
    }

    fn run_command(command: &str, request: HookCommandRequest<'_>) -> HookCommandOutcome {
        let mut child = shell_command(command);
        child.stdin(std::process::Stdio::piped());
        child.stdout(std::process::Stdio::piped());
        child.stderr(std::process::Stdio::piped());
        child.env("HOOK_EVENT", request.event.as_str());
        child.env("HOOK_TOOL_NAME", request.tool_name);
        child.env("HOOK_TOOL_INPUT", request.tool_input);
        child.env(
            "HOOK_TOOL_IS_ERROR",
            if request.is_error { "1" } else { "0" },
        );
        if let Some(tool_output) = request.tool_output {
            child.env("HOOK_TOOL_OUTPUT", tool_output);
        }

        match child.output_with_stdin(request.payload.as_bytes()) {
            Ok(output) => {
                let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
                let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
                let message = (!stdout.is_empty()).then_some(stdout);
                match output.status.code() {
                    Some(0) => HookCommandOutcome::Allow { message },
                    Some(2) => HookCommandOutcome::Deny { message },
                    Some(code) => HookCommandOutcome::Warn {
                        message: format_hook_warning(
                            command,
                            code,
                            message.as_deref(),
                            stderr.as_str(),
                        ),
                    },
                    None => HookCommandOutcome::Warn {
                        message: format!(
                            "{} hook `{command}` terminated by signal while handling `{}`",
                            request.event.as_str(),
                            request.tool_name
                        ),
                    },
                }
            }
            Err(error) => HookCommandOutcome::Warn {
                message: format!(
                    "{} hook `{command}` failed to start for `{}`: {error}",
                    request.event.as_str(),
                    request.tool_name
                ),
            },
        }
    }
}

enum HookCommandOutcome {
    Allow { message: Option<String> },
    Deny { message: Option<String> },
    Warn { message: String },
}

/// Run a prompt-type hook spec: interpolate variables and wrap with a label.
fn run_prompt_spec(
    spec: &HookSpec,
    event: HookEvent,
    tool_name: &str,
    ctx: &HookInterpolationContext,
) -> HookCommandOutcome {
    let body = spec.body();
    if body.trim().is_empty() {
        return HookCommandOutcome::Warn {
            message: format!(
                "{} prompt hook for `{tool_name}` has an empty body; skipping",
                event.as_str()
            ),
        };
    }
    let interpolated = interpolate(body, ctx);
    let labeled = format!("[hook: {} → '{}']\n{interpolated}", event.as_str(), body);
    HookCommandOutcome::Allow {
        message: Some(labeled),
    }
}

/// Return today's date in ISO 8601 (YYYY-MM-DD) from the system clock.
fn current_date_iso() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let days = secs / 86_400;
    let mut year = 1970u32;
    let mut remaining = days;
    loop {
        let days_in_year = if is_leap_year(year) { 366 } else { 365 };
        if remaining < days_in_year {
            break;
        }
        remaining -= days_in_year;
        year += 1;
    }
    let leap = is_leap_year(year);
    let month_days: [u32; 12] = [
        31,
        if leap { 29 } else { 28 },
        31, 30, 31, 30, 31, 31, 30, 31, 30, 31,
    ];
    let mut month = 1u32;
    for &md in &month_days {
        if remaining < u64::from(md) {
            break;
        }
        remaining -= u64::from(md);
        month += 1;
    }
    let day = remaining + 1;
    format!("{year:04}-{month:02}-{day:02}")
}

const fn is_leap_year(year: u32) -> bool {
    (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0)
}

fn parse_tool_input(tool_input: &str) -> serde_json::Value {
    serde_json::from_str(tool_input).unwrap_or_else(|_| json!({ "raw": tool_input }))
}

fn format_hook_warning(command: &str, code: i32, stdout: Option<&str>, stderr: &str) -> String {
    let mut message =
        format!("Hook `{command}` exited with status {code}; allowing tool execution to continue");
    if let Some(stdout) = stdout.filter(|stdout| !stdout.is_empty()) {
        message.push_str(": ");
        message.push_str(stdout);
    } else if !stderr.is_empty() {
        message.push_str(": ");
        message.push_str(stderr);
    }
    message
}

fn shell_command(command: &str) -> CommandWithStdin {
    #[cfg(windows)]
    let mut command_builder = {
        let mut command_builder = Command::new("cmd");
        command_builder.arg("/C").arg(command);
        CommandWithStdin::new(command_builder)
    };

    #[cfg(not(windows))]
    let command_builder = {
        let mut command_builder = Command::new("sh");
        command_builder.arg("-lc").arg(command);
        CommandWithStdin::new(command_builder)
    };

    command_builder
}

struct CommandWithStdin {
    command: Command,
}

impl CommandWithStdin {
    const fn new(command: Command) -> Self {
        Self { command }
    }

    fn stdin(&mut self, cfg: std::process::Stdio) -> &mut Self {
        self.command.stdin(cfg);
        self
    }

    fn stdout(&mut self, cfg: std::process::Stdio) -> &mut Self {
        self.command.stdout(cfg);
        self
    }

    fn stderr(&mut self, cfg: std::process::Stdio) -> &mut Self {
        self.command.stderr(cfg);
        self
    }

    fn env<K, V>(&mut self, key: K, value: V) -> &mut Self
    where
        K: AsRef<OsStr>,
        V: AsRef<OsStr>,
    {
        self.command.env(key, value);
        self
    }

    fn output_with_stdin(&mut self, stdin: &[u8]) -> std::io::Result<std::process::Output> {
        let mut child = self.command.spawn()?;
        if let Some(mut child_stdin) = child.stdin.take() {
            use std::io::Write;
            child_stdin.write_all(stdin)?;
        }
        child.wait_with_output()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::Mutex;

    use serde_json::{json, Value as JsonValue};

    use super::{
        HookRunResult, HookRunner, McpHookInvocationResult, McpHookInvoker, RuntimeHookSpec,
    };
    use crate::config::{RuntimeFeatureConfig, RuntimeHookConfig};
    use plugins::{HookKind, HookSpec};

    #[test]
    fn allows_exit_code_zero_and_captures_stdout() {
        let runner = HookRunner::new(RuntimeHookConfig::new(
            vec![HookSpec::Command(shell_snippet("printf 'pre ok'"))],
            Vec::new(),
        ));

        let result = runner.run_pre_tool_use("Read", r#"{"path":"README.md"}"#);

        assert_eq!(result, HookRunResult::allow(vec!["pre ok".to_string()]));
    }

    #[test]
    fn denies_exit_code_two() {
        let runner = HookRunner::new(RuntimeHookConfig::new(
            vec![HookSpec::Command(shell_snippet(
                "printf 'blocked by hook'; exit 2",
            ))],
            Vec::new(),
        ));

        let result = runner.run_pre_tool_use("Bash", r#"{"command":"pwd"}"#);

        assert!(result.is_denied());
        assert_eq!(result.messages(), &["blocked by hook".to_string()]);
    }

    #[test]
    fn warns_for_other_non_zero_statuses() {
        let runner = HookRunner::from_feature_config(&RuntimeFeatureConfig::default().with_hooks(
            RuntimeHookConfig::new(
                vec![HookSpec::Command(shell_snippet(
                    "printf 'warning hook'; exit 1",
                ))],
                Vec::new(),
            ),
        ));

        let result = runner.run_pre_tool_use("Edit", r#"{"file":"src/lib.rs"}"#);

        assert!(!result.is_denied());
        assert!(result
            .messages()
            .iter()
            .any(|message| message.contains("allowing tool execution to continue")));
    }

    #[test]
    fn prompt_hook_injects_message_without_denying() {
        let runner = HookRunner::new(RuntimeHookConfig::new(
            vec![HookSpec::Tagged {
                kind: HookKind::Prompt,
                body: "verify {tool_name} still compiles".to_string(),
            }],
            Vec::new(),
        ));

        let result = runner.run_pre_tool_use("Write", r#"{"file":"src/main.rs"}"#);

        assert!(!result.is_denied(), "prompt hook must not deny");
        let msgs = result.messages();
        assert_eq!(msgs.len(), 1);
        assert!(
            msgs[0].contains("[hook: PreToolUse →"),
            "label missing: {}",
            msgs[0]
        );
        assert!(
            msgs[0].contains("verify Write still compiles"),
            "interpolation failed: {}",
            msgs[0]
        );
    }

    #[test]
    fn prompt_hook_from_commands_constructor() {
        // Ensure RuntimeHookConfig::from_commands keeps working for callers
        // that build configs from plain string lists.
        let config = RuntimeHookConfig::from_commands(
            vec!["printf 'ok'".to_string()],
            Vec::new(),
        );
        assert!(!config.pre_tool_use()[0].is_prompt());
    }

    /// Captures every (server, tool, input) invocation and replays a scripted
    /// response.  Used by the FEAT-30 mcp_tool dispatch tests.
    struct MockMcpInvoker {
        calls: Mutex<Vec<(String, String, JsonValue)>>,
        response: Mutex<Result<Option<McpHookInvocationResult>, String>>,
    }

    impl MockMcpInvoker {
        fn with_ok(output: &str) -> Self {
            Self {
                calls: Mutex::new(Vec::new()),
                response: Mutex::new(Ok(Some(McpHookInvocationResult::ok(output.to_string())))),
            }
        }

        fn with_unavailable() -> Self {
            Self {
                calls: Mutex::new(Vec::new()),
                response: Mutex::new(Ok(None)),
            }
        }

        fn with_error(message: &str) -> Self {
            Self {
                calls: Mutex::new(Vec::new()),
                response: Mutex::new(Err(message.to_string())),
            }
        }

        fn calls(&self) -> Vec<(String, String, JsonValue)> {
            self.calls.lock().expect("calls lock").clone()
        }
    }

    impl McpHookInvoker for MockMcpInvoker {
        fn invoke(
            &self,
            server: &str,
            tool: &str,
            input: &JsonValue,
        ) -> Result<Option<McpHookInvocationResult>, String> {
            self.calls
                .lock()
                .expect("calls lock")
                .push((server.to_string(), tool.to_string(), input.clone()));
            self.response.lock().expect("response lock").clone()
        }
    }

    #[test]
    fn mcp_tool_hook_dispatches_to_invoker_and_captures_output() {
        let mock = Arc::new(MockMcpInvoker::with_ok("redacted: <token>"));
        let runner = HookRunner::from_runtime_specs(
            Vec::new(),
            vec![RuntimeHookSpec::McpTool {
                server: "vault-scrubber".to_string(),
                tool: "redact".to_string(),
                input: json!({ "field": "stdout" }),
            }],
        )
        .with_mcp_invoker(mock.clone() as Arc<dyn McpHookInvoker>);

        let result = runner.run_post_tool_use(
            "Bash",
            r#"{"command":"echo secret"}"#,
            "secret=AKIA...",
            false,
        );

        assert!(!result.is_denied(), "mcp_tool hook must not deny");
        assert_eq!(
            result.messages(),
            &["redacted: <token>".to_string()],
            "stdout-equivalent output should flow into messages"
        );
        let calls = mock.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "vault-scrubber");
        assert_eq!(calls[0].1, "redact");
        assert_eq!(calls[0].2, json!({ "field": "stdout" }));
    }

    #[test]
    fn mcp_tool_hook_warns_when_no_invoker_registered() {
        let runner = HookRunner::from_runtime_specs(
            vec![RuntimeHookSpec::McpTool {
                server: "noop-server".to_string(),
                tool: "anything".to_string(),
                input: json!({}),
            }],
            Vec::new(),
        );

        let result = runner.run_pre_tool_use("Read", r#"{"path":"README.md"}"#);

        assert!(!result.is_denied(), "missing invoker must not deny");
        assert!(
            result
                .messages()
                .iter()
                .any(|m| m.contains("no MCP invoker registered")),
            "expected warning about missing invoker, got {:?}",
            result.messages()
        );
    }

    #[test]
    fn mcp_tool_hook_warns_on_unavailable_server() {
        let mock = Arc::new(MockMcpInvoker::with_unavailable());
        let runner = HookRunner::from_runtime_specs(
            vec![RuntimeHookSpec::McpTool {
                server: "missing".to_string(),
                tool: "redact".to_string(),
                input: json!({}),
            }],
            Vec::new(),
        )
        .with_mcp_invoker(mock as Arc<dyn McpHookInvoker>);

        let result = runner.run_pre_tool_use("Read", r#"{"path":"a.txt"}"#);

        assert!(!result.is_denied(), "unavailable server must not deny turn");
        assert!(
            result
                .messages()
                .iter()
                .any(|m| m.contains("resolved to no-op")),
            "expected no-op warning, got {:?}",
            result.messages()
        );
    }

    #[test]
    fn mcp_tool_hook_warns_on_invoker_error_and_does_not_deny() {
        let mock = Arc::new(MockMcpInvoker::with_error("transport closed"));
        let runner = HookRunner::from_runtime_specs(
            vec![RuntimeHookSpec::McpTool {
                server: "flaky".to_string(),
                tool: "redact".to_string(),
                input: json!({}),
            }],
            Vec::new(),
        )
        .with_mcp_invoker(mock as Arc<dyn McpHookInvoker>);

        let result = runner.run_pre_tool_use("Read", r#"{"path":"a.txt"}"#);

        assert!(!result.is_denied(), "invoker error must not deny turn");
        assert!(
            result
                .messages()
                .iter()
                .any(|m| m.contains("transport closed")),
            "expected transport error in warning, got {:?}",
            result.messages()
        );
    }

    #[test]
    fn mcp_tool_hook_runs_alongside_command_hooks_in_order() {
        let mock = Arc::new(MockMcpInvoker::with_ok("mcp-ran"));
        let mut runner = HookRunner::new(RuntimeHookConfig::new(
            vec![HookSpec::Command(shell_snippet("printf 'cmd-ran'"))],
            Vec::new(),
        ))
        .with_mcp_invoker(mock.clone() as Arc<dyn McpHookInvoker>);
        // Append the mcp_tool entry through the runtime-side extras path
        // (Stream B will eventually fold this into RuntimeHookConfig).
        runner.pre_tool_use_extra.push(RuntimeHookSpec::McpTool {
            server: "scrub".to_string(),
            tool: "redact".to_string(),
            input: json!({ "k": "v" }),
        });

        let result = runner.run_pre_tool_use("Edit", r#"{"file":"x.rs"}"#);

        assert!(!result.is_denied());
        assert_eq!(
            result.messages(),
            &["cmd-ran".to_string(), "mcp-ran".to_string()],
            "command hook should fire first, mcp_tool second"
        );
        assert_eq!(mock.calls().len(), 1);
    }

    // ─── v2.2.11 new-event tests ─────────────────────────────────────────────

    #[test]
    fn session_start_dispatches_to_registered_hooks() {
        let mut runner = HookRunner::default();
        runner.session_start_extra.push(RuntimeHookSpec::Plugin(
            HookSpec::Command(shell_snippet("printf 'session-start'")),
        ));
        let result = runner.run_session_start();
        assert!(!result.is_denied(), "SessionStart must not deny");
        assert_eq!(result.messages(), &["session-start".to_string()]);
    }

    #[test]
    fn session_end_dispatches_to_registered_hooks() {
        let mut runner = HookRunner::default();
        runner.session_end_extra.push(RuntimeHookSpec::Plugin(
            HookSpec::Command(shell_snippet("printf 'session-end'")),
        ));
        let result = runner.run_session_end();
        assert!(!result.is_denied(), "SessionEnd must not deny");
        assert_eq!(result.messages(), &["session-end".to_string()]);
    }

    #[test]
    fn file_changed_payload_carries_path_and_action() {
        use super::{FileChangeAction, FileChangedPayload};
        let mut runner = HookRunner::default();
        runner.file_changed_extra.push(RuntimeHookSpec::Plugin(
            HookSpec::Command(shell_snippet("printf 'file-ok'")),
        ));
        let result = runner.run_file_changed(&FileChangedPayload {
            path: "/tmp/foo.rs".to_string(),
            action: FileChangeAction::Edit,
        });
        assert!(!result.is_denied());
        assert_eq!(result.messages(), &["file-ok".to_string()]);
    }

    #[test]
    fn file_changed_action_serializes_correctly() {
        use super::FileChangeAction;
        assert_eq!(FileChangeAction::Edit.as_str(), "edit");
        assert_eq!(FileChangeAction::Write.as_str(), "write");
        assert_eq!(FileChangeAction::Create.as_str(), "create");
        assert_eq!(FileChangeAction::Delete.as_str(), "delete");
    }

    #[test]
    fn cwd_changed_payload_carries_old_and_new() {
        use super::CwdChangedPayload;
        let mut runner = HookRunner::default();
        runner.cwd_changed_extra.push(RuntimeHookSpec::Plugin(
            HookSpec::Command(shell_snippet("printf 'cwd-ok'")),
        ));
        let result = runner.run_cwd_changed(&CwdChangedPayload {
            old_cwd: "/old/path".to_string(),
            new_cwd: "/new/path".to_string(),
        });
        assert!(!result.is_denied(), "CwdChanged must not deny");
        assert_eq!(result.messages(), &["cwd-ok".to_string()]);
    }

    #[test]
    fn permission_request_hook_can_inject_decision() {
        use super::{HookPermissionDecision, PermissionRequestPayload};
        let mut runner = HookRunner::default();
        // Hook that returns a JSON decision via stdout (exit 0 = allow).
        runner.permission_request_extra.push(RuntimeHookSpec::Plugin(
            HookSpec::Command(shell_snippet(
                r#"printf '{"permissionDecision":"allow"}'"#,
            )),
        ));
        let result = runner.run_permission_request(&PermissionRequestPayload {
            tool: "bash".to_string(),
            input: r#"{"command":"ls"}"#.to_string(),
            requested_mode: "plan".to_string(),
        });
        assert_eq!(
            result.decision,
            Some(HookPermissionDecision::Allow),
            "hook should inject Allow decision"
        );
    }

    #[test]
    fn permission_request_hook_decision_short_circuits_prompt() {
        use super::{HookPermissionDecision, PermissionRequestPayload};
        let mut runner = HookRunner::default();
        // exit 2 on PermissionRequest = inject Deny decision.
        runner.permission_request_extra.push(RuntimeHookSpec::Plugin(
            HookSpec::Command(shell_snippet("printf 'denied'; exit 2")),
        ));
        let result = runner.run_permission_request(&PermissionRequestPayload {
            tool: "write_file".to_string(),
            input: r#"{"path":"/etc/passwd"}"#.to_string(),
            requested_mode: "workspace_write".to_string(),
        });
        assert_eq!(
            result.decision,
            Some(HookPermissionDecision::Deny),
            "exit 2 on PermissionRequest should inject Deny"
        );
        assert!(result.messages.contains(&"denied".to_string()));
    }

    #[test]
    fn permission_denied_payload_carries_source_and_reason() {
        use super::{PermissionDeniedPayload, PermissionDeniedSource};
        let mut runner = HookRunner::default();
        runner.permission_denied_extra.push(RuntimeHookSpec::Plugin(
            HookSpec::Command(shell_snippet("printf 'denied-event'")),
        ));
        let result = runner.run_permission_denied(&PermissionDeniedPayload {
            tool: "bash".to_string(),
            input: r#"{"command":"rm -rf /"}"#.to_string(),
            reason: "sandbox blocked".to_string(),
            source: PermissionDeniedSource::Sandbox,
        });
        assert!(!result.is_denied(), "PermissionDenied is observe-only");
        assert_eq!(result.messages(), &["denied-event".to_string()]);
    }

    #[test]
    fn permission_denied_source_serializes() {
        use super::PermissionDeniedSource;
        assert_eq!(PermissionDeniedSource::Hook.as_str(), "hook");
        assert_eq!(PermissionDeniedSource::User.as_str(), "user");
        assert_eq!(PermissionDeniedSource::Sandbox.as_str(), "sandbox");
    }

    #[test]
    fn post_tool_batch_payload_aggregates_durations_and_counts() {
        use super::PostToolBatchPayload;
        let mut runner = HookRunner::default();
        runner.post_tool_batch_extra.push(RuntimeHookSpec::Plugin(
            HookSpec::Command(shell_snippet("printf 'batch-ok'")),
        ));
        let result = runner.run_post_tool_batch(&PostToolBatchPayload {
            tool_count: 3,
            durations_ms: vec![10, 20, 30],
            success_count: 2,
            failure_count: 1,
        });
        assert!(!result.is_denied(), "PostToolBatch must not deny");
        assert_eq!(result.messages(), &["batch-ok".to_string()]);
    }

    #[test]
    fn notification_payload_dispatches_with_kind_and_message() {
        use super::{NotificationKind, NotificationPayload};
        let mut runner = HookRunner::default();
        runner.notification_extra.push(RuntimeHookSpec::Plugin(
            HookSpec::Command(shell_snippet("printf 'notify-ok'")),
        ));
        let result = runner.run_notification(&NotificationPayload {
            kind: NotificationKind::Completion,
            message: "Turn complete".to_string(),
        });
        assert!(!result.is_denied(), "Notification must not deny");
        assert_eq!(result.messages(), &["notify-ok".to_string()]);
    }

    // ─────────────────────────────────────────────────────────────────────────

    #[cfg(windows)]
    fn shell_snippet(script: &str) -> String {
        script.replace('\'', "\"")
    }

    #[cfg(not(windows))]
    fn shell_snippet(script: &str) -> String {
        script.to_string()
    }
}
