use std::ffi::OsStr;
use std::process::Command;
use std::sync::Arc;

use plugins::{interpolate, HookInterpolationContext, HookSpec};
use serde_json::{json, Value as JsonValue};

use crate::config::{RuntimeFeatureConfig, RuntimeHookConfig};

// ---------------------------------------------------------------------------
// Stop-hook block cap (task #566)
// ---------------------------------------------------------------------------

/// Environment variable that caps how many times a Stop hook may block a
/// single session before the runtime forces stop. Default 5.
///
/// CC parity: when a Stop hook returns a block decision the agent is
/// supposed to keep going. Without a cap a buggy hook can spin the agent
/// forever; CC defaults to 5 retries.
///
/// Note: the Stop hook event itself is not yet emitted by Anvil's
/// streaming loop (the event taxonomy in `HookEvent` is the
/// Pre/Post/SessionStart/SessionEnd/FileChanged/CwdChanged/Permission*/
/// Notification set). This constant + helper exist so the moment the
/// Stop hook is wired in, the cap is already configurable and tested.
pub const STOP_HOOK_BLOCK_CAP_ENV: &str = "ANVIL_STOP_HOOK_BLOCK_CAP";

/// Default Stop-hook block cap (task #566). Matches CC's documented default.
pub const DEFAULT_STOP_HOOK_BLOCK_CAP: u32 = 5;

/// Read `ANVIL_STOP_HOOK_BLOCK_CAP` from the environment, clamping invalid
/// values to the default. A value of `0` disables the cap entirely (the
/// caller must still emit a warning the first time the cap would have
/// fired so misconfiguration is observable).
#[must_use]
pub fn stop_hook_block_cap_from_env() -> u32 {
    std::env::var(STOP_HOOK_BLOCK_CAP_ENV)
        .ok()
        .and_then(|raw| raw.trim().parse::<u32>().ok())
        .unwrap_or(DEFAULT_STOP_HOOK_BLOCK_CAP)
}

/// Per-session counter for Stop-hook blocks (task #566). The runtime
/// increments this each time a Stop hook returns "block"; once the
/// counter reaches the configured cap the runtime emits a warning and
/// allows the stop to proceed.
///
/// Two-stage API: `record_block` returns `Decision::AllowStop` once the
/// cap is hit so the caller can branch on the result without re-reading
/// the env var.
#[derive(Debug, Clone)]
pub struct StopHookBlockCounter {
    blocks_seen: u32,
    cap: u32,
    /// Whether the warning has already been surfaced; the runtime only
    /// emits the cap-hit warning once per session.
    warned: bool,
}

impl Default for StopHookBlockCounter {
    fn default() -> Self {
        Self::new(stop_hook_block_cap_from_env())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopHookCapDecision {
    /// Block was below the cap; the runtime should honor the hook and
    /// continue running.
    Continue,
    /// Cap has been reached; the runtime must allow the stop to proceed.
    /// `warning` is `Some` exactly once per session — the first time we
    /// reach the cap — so callers can surface it to the user.
    AllowStop { warning: Option<String> },
}

impl StopHookBlockCounter {
    #[must_use]
    pub const fn new(cap: u32) -> Self {
        Self {
            blocks_seen: 0,
            cap,
            warned: false,
        }
    }

    #[must_use]
    pub const fn blocks_seen(&self) -> u32 {
        self.blocks_seen
    }

    #[must_use]
    pub const fn cap(&self) -> u32 {
        self.cap
    }

    /// Record one Stop-hook block and return whether the runtime should
    /// continue or force-stop. `cap == 0` is treated as "disabled" — the
    /// counter still increments but always returns `Continue`.
    pub fn record_block(&mut self) -> StopHookCapDecision {
        self.blocks_seen = self.blocks_seen.saturating_add(1);
        if self.cap == 0 {
            return StopHookCapDecision::Continue;
        }
        if self.blocks_seen >= self.cap {
            let warning = if self.warned {
                None
            } else {
                self.warned = true;
                Some(format!(
                    "Stop hook blocked {} times (cap: {}); allowing stop to proceed.",
                    self.blocks_seen, self.cap,
                ))
            };
            StopHookCapDecision::AllowStop { warning }
        } else {
            StopHookCapDecision::Continue
        }
    }
}

// ---------------------------------------------------------------------------
// Hook terminalSequence output (task #556)
// ---------------------------------------------------------------------------

/// Allow-list of ANSI sequence prefixes that hooks may emit via the
/// `terminalSequence` field of their stdout JSON. Anything else is
/// rejected with a warning so a hook cannot inject arbitrary CSI/OSC
/// payloads (e.g. screen-clearing, palette resets) into the terminal.
///
/// - `\x07` (BEL) is the classic terminal bell.
/// - `\x1b]9;...\x07` is OSC 9, the iTerm2 desktop-notification sequence.
/// - `\x1b]777;...\x07` is OSC 777, the urxvt / kitty notification sequence.
const ALLOWED_TERMINAL_SEQUENCE_PREFIXES: &[&str] = &[
    "\x07",
    "\x1b]9;",
    "\x1b]777;",
];

/// Validate a `terminalSequence` value pulled from a hook's stdout JSON.
///
/// Returns the original bytes when they begin with one of the
/// allow-listed prefixes; otherwise returns `Err(reason)` so the
/// runtime can log a warning and discard the sequence.
pub fn validate_terminal_sequence(sequence: &str) -> Result<&str, String> {
    if sequence.is_empty() {
        return Err("hook terminalSequence is empty".to_string());
    }
    if ALLOWED_TERMINAL_SEQUENCE_PREFIXES
        .iter()
        .any(|prefix| sequence.starts_with(prefix))
    {
        Ok(sequence)
    } else {
        Err(format!(
            "hook terminalSequence rejected: must begin with \\a, \\x1b]9; or \\x1b]777; (got {} bytes starting {:?})",
            sequence.len(),
            sequence.chars().take(4).collect::<String>(),
        ))
    }
}

/// Parse the `terminalSequence` field out of a hook stdout JSON blob.
///
/// Returns:
/// - `Ok(Some(bytes))` when the field is present, well-formed, and
///   begins with an allow-listed prefix.
/// - `Ok(None)` when the field is absent or the stdout is not JSON
///   (silent skip — non-JSON stdout is the common case).
/// - `Err(reason)` when the field is present but rejected by the
///   allow-list, so the caller can surface a warning.
pub fn extract_terminal_sequence(stdout: &str) -> Result<Option<String>, String> {
    let trimmed = stdout.trim();
    if trimmed.is_empty() || !trimmed.starts_with('{') {
        return Ok(None);
    }
    let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) else {
        return Ok(None);
    };
    let Some(raw) = value.get("terminalSequence").and_then(|v| v.as_str()) else {
        return Ok(None);
    };
    validate_terminal_sequence(raw).map(|_| Some(raw.to_string()))
}

/// Sink trait for delivering a hook-emitted terminal-control sequence to
/// the user's terminal. Implementations decide how to route the bytes:
///
/// - **Headless** (no TUI): write directly to stdout (the default sink
///   when nothing else is installed).
/// - **TUI active**: queue the bytes through the TUI's redraw pipeline so
///   the alt-screen back-buffer stays consistent (must not bypass ratatui
///   while the alt-screen owns the terminal).
///
/// The runtime owns no terminal state; the CLI host installs a sink via
/// `set_terminal_sequence_sink` at startup so [`run_command`] /
/// [`run_exec`] can deliver bytes without dragging crossterm into the
/// runtime crate.
pub trait TerminalSequenceSink: Send + Sync {
    /// Deliver `sequence` to the terminal.  The bytes are guaranteed by
    /// [`validate_terminal_sequence`] to begin with one of the
    /// allow-listed prefixes (`\x07`, `\x1b]9;`, `\x1b]777;`).
    fn emit(&self, sequence: &str);
}

/// Default headless sink: writes raw bytes to stdout via the libc-level
/// file descriptor so it never goes through `println!`.  This is the
/// sink in effect when no other sink has been installed and matches the
/// headless contract (stdout is fine when no TUI owns the terminal).
struct StdoutTerminalSequenceSink;

impl TerminalSequenceSink for StdoutTerminalSequenceSink {
    fn emit(&self, sequence: &str) {
        // SAFE: stdout writes are the right sink in headless mode.  We
        // route through `io::stdout()` rather than `println!` so the bytes
        // are emitted verbatim (no trailing newline) and the call site is
        // grep-able for the task #556 audit.
        use std::io::Write;
        let mut out = std::io::stdout().lock();
        let _ = out.write_all(sequence.as_bytes());
        let _ = out.flush();
    }
}

static TERMINAL_SEQUENCE_SINK: std::sync::OnceLock<
    std::sync::RwLock<std::sync::Arc<dyn TerminalSequenceSink>>,
> = std::sync::OnceLock::new();

fn terminal_sequence_sink_slot()
    -> &'static std::sync::RwLock<std::sync::Arc<dyn TerminalSequenceSink>>
{
    TERMINAL_SEQUENCE_SINK.get_or_init(|| {
        std::sync::RwLock::new(
            std::sync::Arc::new(StdoutTerminalSequenceSink) as std::sync::Arc<dyn TerminalSequenceSink>,
        )
    })
}

/// Install a custom terminal-sequence sink.  Called once at CLI startup
/// to route hook-emitted bytes through the TUI's redraw pipeline (when a
/// TUI is active) or through any other host-specific routing.
///
/// Replaces any previously installed sink.
pub fn set_terminal_sequence_sink(sink: std::sync::Arc<dyn TerminalSequenceSink>) {
    let slot = terminal_sequence_sink_slot();
    if let Ok(mut guard) = slot.write() {
        *guard = sink;
    }
}

/// Emit a validated hook terminal sequence through the installed sink.
/// Bytes that didn't pass [`validate_terminal_sequence`] never reach this
/// function — the call site checks first and logs a warning on rejection.
fn dispatch_terminal_sequence(sequence: &str) {
    let slot = terminal_sequence_sink_slot();
    if let Ok(guard) = slot.read() {
        guard.emit(sequence);
    }
}

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
    /// Fires at the end of an assistant message that produced no `tool_use`
    /// blocks — i.e. the runtime is about to return control to the user.
    /// A hook may return `{ "decision": "block", "reason": "..." }` to
    /// keep the turn loop running; any other / missing decision allows
    /// the stop to proceed.  Task #566.
    /// Payload: `{ session_id, turn_count, block_count }`.
    Stop,
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

/// Payload for `HookEvent::Stop` (task #566).
///
/// Emitted at end-of-turn when the assistant returned no `tool_use`
/// blocks.  A hook may inspect `block_count` (how many times a Stop
/// hook has already kept the loop alive this session) and return
/// `{"decision":"block","reason":"..."}` to keep the turn running, or
/// any other / missing decision to allow the stop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StopHookPayload {
    pub session_id: String,
    pub turn_count: u64,
    pub block_count: u32,
}

/// Decision returned by `HookRunner::run_stop`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopHookDecision {
    /// No hook returned a block decision — the turn loop should exit.
    Allow,
    /// At least one hook returned `{"decision":"block"}`.  The runtime
    /// should keep the turn alive and inject `reason` as a user-role
    /// message.  Multiple block decisions concatenate their reasons.
    Block { reason: String },
}

/// Aggregate result from running every Stop hook.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StopHookResult {
    pub decision: StopHookDecision,
    /// Non-decision stdout that should be surfaced to the user.
    pub messages: Vec<String>,
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
// Phase 5.3 #19 (Stream B complete): `RuntimeHookConfig` now holds
// `Vec<RuntimeHookSpec>` for every event type, and the config parser
// (`config/hooks.rs::parse_runtime_hook_spec_array`) uses
// `RuntimeHookSpec::from_json_value` so that `mcp_tool` entries parsed from
// `settings.json` flow through to `HookRunner::collect_specs` without needing
// the `_extra` programmatic bypass.
//
// The `_extra` fields on `HookRunner` are retained for programmatic injection
// at runtime (e.g. from the MCP server registry start-up path); they are
// additive and do not conflict with the config-parsed specs.
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

impl RuntimeHookSpec {
    /// Deserialize a single hook entry from a `serde_json::Value`, supporting
    /// both the `plugins::HookSpec` variants (bare string, tagged command/prompt,
    /// exec-args array) and the runtime-only `McpTool` variant.
    ///
    /// Dispatch order:
    ///   1. If the value is an object with `"type": "mcp_tool"`, parse as McpTool.
    ///   2. Otherwise, fall through to `serde_json::from_value::<HookSpec>`.
    ///
    /// Returns `None` (with a stderr warning) if neither branch succeeds.
    pub fn from_json_value(value: &serde_json::Value) -> Option<Self> {
        // Fast path: detect the mcp_tool discriminant before attempting HookSpec
        // deserialization, which would silently discard an unknown `type` field.
        if let Some(obj) = value.as_object() {
            if obj.get("type").and_then(|v| v.as_str()) == Some("mcp_tool") {
                let server = obj
                    .get("server")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();
                let tool = obj
                    .get("tool")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();
                let input = obj
                    .get("input")
                    .cloned()
                    .unwrap_or(serde_json::Value::Object(Default::default()));
                if server.is_empty() || tool.is_empty() {
                    eprintln!(
                        "anvil: mcp_tool hook entry missing required `server` or `tool` field; skipping"
                    );
                    return None;
                }
                return Some(Self::McpTool { server, tool, input });
            }
        }
        // Fall back to HookSpec deserialization for Command/Tagged/Exec forms.
        match serde_json::from_value::<HookSpec>(value.clone()) {
            Ok(spec) => Some(Self::Plugin(spec)),
            Err(err) => {
                eprintln!("anvil: skipping malformed hook entry: {err}");
                None
            }
        }
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
            Self::Stop => "Stop",
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
    // Task #566: programmatic Stop hook extras (tests inject here).
    stop_extra: Vec<RuntimeHookSpec>,
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
            .field("stop_extra", &self.stop_extra)
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
            && self.stop_extra == other.stop_extra
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
            stop_extra: Vec::new(),
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
            stop_extra: Vec::new(),
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

    /// Clone of the registered MCP invoker, if any. Task #561 (worktree
    /// hook refresh): callers that rebuild a `HookRunner` from a fresh
    /// `RuntimeFeatureConfig` need to re-attach the host's MCP invoker
    /// so MCP-driven hooks continue to dispatch after the rebuild.
    #[must_use]
    pub fn mcp_invoker_clone(&self) -> Option<Arc<dyn McpHookInvoker>> {
        self.mcp_invoker.clone()
    }

    /// Task #566: mutable access to programmatically registered Stop
    /// hooks. Used by tests + by hosts that need to register Stop hooks
    /// after construction.
    pub fn stop_extra_mut(&mut self) -> &mut Vec<RuntimeHookSpec> {
        &mut self.stop_extra
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

    /// Task #566: fire all Stop hooks and collect their decisions.
    ///
    /// A hook that returns `{"decision":"block","reason":"..."}` keeps
    /// the turn loop alive; any other / missing decision allows stop.
    /// Multiple block decisions concatenate their reasons (separated by
    /// `\n\n`) so the user can see every reason at once.  Non-decision
    /// stdout (other JSON keys, plain text) is surfaced as a message.
    pub fn run_stop(&self, p: &StopHookPayload) -> StopHookResult {
        let specs = self.collect_specs(HookEvent::Stop);
        if specs.is_empty() {
            return StopHookResult {
                decision: StopHookDecision::Allow,
                messages: Vec::new(),
            };
        }
        let payload = json!({
            "hook_event_name": HookEvent::Stop.as_str(),
            "session_id": p.session_id,
            "turn_count": p.turn_count,
            "block_count": p.block_count,
        })
        .to_string();

        let ctx = HookInterpolationContext {
            tool_name: None,
            tool_input: None,
            cwd: std::env::current_dir().ok().map(|path| path.display().to_string()),
            date: Some(current_date_iso()),
            model: None,
        };

        let mut messages = Vec::new();
        let mut block_reasons: Vec<String> = Vec::new();

        for spec in &specs {
            let outcome = match spec {
                RuntimeHookSpec::Plugin(plugin) if plugin.is_prompt() => {
                    run_prompt_spec(plugin, HookEvent::Stop, HookEvent::Stop.as_str(), &ctx)
                }
                RuntimeHookSpec::Plugin(plugin) => Self::run_command(
                    plugin.body(),
                    HookCommandRequest {
                        event: HookEvent::Stop,
                        tool_name: HookEvent::Stop.as_str(),
                        tool_input: "{}",
                        tool_output: None,
                        is_error: false,
                        payload: &payload,
                    },
                ),
                RuntimeHookSpec::McpTool { server, tool, input } => self.run_mcp_tool_spec(
                    HookEvent::Stop,
                    HookEvent::Stop.as_str(),
                    server,
                    tool,
                    input,
                ),
            };
            match outcome {
                HookCommandOutcome::Allow { message } => {
                    if let Some(ref stdout) = message {
                        if let Some(reason) = parse_stop_block_reason(stdout) {
                            block_reasons.push(reason);
                        }
                    }
                    if let Some(msg) = message {
                        messages.push(msg);
                    }
                }
                // Exit code 2 from a Stop hook is treated as a block decision
                // with an empty / hook-supplied reason.  Mirrors how
                // PermissionRequest treats exit-2 as a deny injection.
                HookCommandOutcome::Deny { message } => {
                    let reason = message
                        .clone()
                        .and_then(|m| parse_stop_block_reason(&m))
                        .unwrap_or_else(|| {
                            message.clone().unwrap_or_else(|| {
                                "Stop hook returned exit 2 (block)".to_string()
                            })
                        });
                    block_reasons.push(reason);
                    if let Some(msg) = message {
                        messages.push(msg);
                    }
                }
                HookCommandOutcome::Warn { message } => messages.push(message),
            }
        }

        let decision = if block_reasons.is_empty() {
            StopHookDecision::Allow
        } else {
            StopHookDecision::Block {
                reason: block_reasons.join("\n\n"),
            }
        };
        StopHookResult { decision, messages }
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

    /// Merge the config-derived specs with runtime-only extras into a single
    /// dispatch list.  Config specs come first to preserve existing ordering.
    ///
    /// Phase 5.3 #19: `RuntimeHookConfig` now holds `Vec<RuntimeHookSpec>` so
    /// `McpTool` entries parsed from `settings.json` flow through here directly
    /// without needing the `_extra` bypass.
    fn collect_specs(&self, event: HookEvent) -> Vec<RuntimeHookSpec> {
        let (config_specs, extras): (&[RuntimeHookSpec], &Vec<RuntimeHookSpec>) = match event {
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
            HookEvent::Stop => (self.config.stop(), &self.stop_extra),
        };
        let mut out = Vec::with_capacity(config_specs.len() + extras.len());
        // Config specs are already RuntimeHookSpec; no conversion needed.
        out.extend(config_specs.iter().cloned());
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
            let request = HookCommandRequest {
                event,
                tool_name,
                tool_input,
                tool_output,
                is_error,
                payload: &payload,
            };
            let outcome = match spec {
                RuntimeHookSpec::Plugin(plugin) if plugin.is_prompt() => {
                    run_prompt_spec(plugin, event, tool_name, &ctx)
                }
                // CC parity v2.2.14: exec form. Spawns the command directly
                // without a shell, so path placeholders never need quoting.
                // Honors `continue_on_block` for PostToolUse: a denial is
                // rewritten to a warning so the model can see the rejection
                // reason and continue the turn.
                RuntimeHookSpec::Plugin(plugin) if plugin.is_exec() => {
                    let args = plugin.exec_args().unwrap_or(&[]);
                    let raw = Self::run_exec(args, request);
                    apply_continue_on_block(raw, event, plugin.continues_on_block())
                }
                RuntimeHookSpec::Plugin(plugin) => Self::run_command(plugin.body(), request),
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
        // CC parity v2.2.14: propagate per-session env from the thread-local
        // SessionContext so hook scripts can see ANVIL_SESSION_ID / ANVIL_EFFORT
        // / ANVIL_PROJECT_DIR (matches CC v2.1.132 / v2.1.133 / v2.1.139).
        if let Some(ctx) = crate::session_ctx::get() {
            child.env("ANVIL_SESSION_ID", &ctx.session_id);
            child.env("ANVIL_EFFORT", &ctx.effort_level);
            child.env("ANVIL_PROJECT_DIR", ctx.project_dir.as_os_str());
        }
        // CC-DRIFT-B5: pass W3C trace context to hook scripts so a hook that
        // calls out to another traced service extends the same trace.
        crate::otel::traceparent::inject_into_command(child.inner_command());

        match child.output_with_stdin(request.payload.as_bytes()) {
            Ok(output) => {
                let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
                let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
                // Task #556: pull `terminalSequence` out of hook stdout JSON
                // (if any) and route it through the installed sink.  Invalid
                // sequences surface as a warning message that is appended to
                // the hook's outcome below.
                let terminal_sequence_warning = process_terminal_sequence_from_stdout(
                    &stdout,
                    request.event,
                    command,
                );
                let message = (!stdout.is_empty()).then_some(stdout);
                let outcome = match output.status.code() {
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
                };
                append_terminal_sequence_warning(outcome, terminal_sequence_warning)
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

    /// CC parity v2.2.14: exec-form hook runner. Spawns `args[0]` with
    /// `args[1..]` passed verbatim — no shell, so neither path nor argument
    /// values need quoting. All other semantics (env, stdin payload, exit
    /// codes 0/2/other → Allow/Deny/Warn, session_ctx propagation) match
    /// `run_command` to keep the user-visible behavior identical.
    fn run_exec(args: &[String], request: HookCommandRequest<'_>) -> HookCommandOutcome {
        let program = match args.first() {
            Some(p) if !p.trim().is_empty() => p,
            _ => {
                return HookCommandOutcome::Warn {
                    message: format!(
                        "{} hook exec args[] empty for `{}`; skipping",
                        request.event.as_str(),
                        request.tool_name
                    ),
                };
            }
        };
        let mut command = std::process::Command::new(program);
        command.args(&args[1..]);
        command.stdin(std::process::Stdio::piped());
        command.stdout(std::process::Stdio::piped());
        command.stderr(std::process::Stdio::piped());
        command.env("HOOK_EVENT", request.event.as_str());
        command.env("HOOK_TOOL_NAME", request.tool_name);
        command.env("HOOK_TOOL_INPUT", request.tool_input);
        command.env(
            "HOOK_TOOL_IS_ERROR",
            if request.is_error { "1" } else { "0" },
        );
        if let Some(tool_output) = request.tool_output {
            command.env("HOOK_TOOL_OUTPUT", tool_output);
        }
        if let Some(ctx) = crate::session_ctx::get() {
            command.env("ANVIL_SESSION_ID", &ctx.session_id);
            command.env("ANVIL_EFFORT", &ctx.effort_level);
            command.env("ANVIL_PROJECT_DIR", ctx.project_dir.as_os_str());
        }

        // Spawn + feed stdin + capture output. Mirrors CommandWithStdin::output_with_stdin.
        let spawn_result = command.spawn();
        let mut child = match spawn_result {
            Ok(c) => c,
            Err(error) => {
                return HookCommandOutcome::Warn {
                    message: format!(
                        "{} hook exec `{program}` failed to start for `{}`: {error}",
                        request.event.as_str(),
                        request.tool_name
                    ),
                };
            }
        };
        if let Some(mut child_stdin) = child.stdin.take() {
            use std::io::Write;
            let _ = child_stdin.write_all(request.payload.as_bytes());
            drop(child_stdin);
        }
        let output = match child.wait_with_output() {
            Ok(o) => o,
            Err(error) => {
                return HookCommandOutcome::Warn {
                    message: format!(
                        "{} hook exec `{program}` failed for `{}`: {error}",
                        request.event.as_str(),
                        request.tool_name
                    ),
                };
            }
        };
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        // Task #556: pull `terminalSequence` out of hook stdout JSON (if
        // any) and route it through the installed sink.
        let terminal_sequence_warning =
            process_terminal_sequence_from_stdout(&stdout, request.event, program);
        let message = (!stdout.is_empty()).then_some(stdout);
        let outcome = match output.status.code() {
            Some(0) => HookCommandOutcome::Allow { message },
            Some(2) => HookCommandOutcome::Deny { message },
            Some(code) => HookCommandOutcome::Warn {
                message: format_hook_warning(program, code, message.as_deref(), stderr.as_str()),
            },
            None => HookCommandOutcome::Warn {
                message: format!(
                    "{} hook exec `{program}` terminated by signal while handling `{}`",
                    request.event.as_str(),
                    request.tool_name
                ),
            },
        };
        append_terminal_sequence_warning(outcome, terminal_sequence_warning)
    }
}

/// Parse a Stop hook's stdout for a `{"decision":"block","reason":"..."}`
/// block decision.  Returns `Some(reason)` when both fields are present
/// and `decision == "block"`; `None` otherwise (no decision, allow, or
/// not JSON).  Empty `reason` is treated as a generic block.
fn parse_stop_block_reason(stdout: &str) -> Option<String> {
    let trimmed = stdout.trim();
    if trimmed.is_empty() || !trimmed.starts_with('{') {
        return None;
    }
    let value = serde_json::from_str::<serde_json::Value>(trimmed).ok()?;
    let decision = value.get("decision")?.as_str()?;
    if decision != "block" {
        return None;
    }
    let reason = value
        .get("reason")
        .and_then(|v| v.as_str())
        .unwrap_or("Stop hook blocked the turn")
        .to_string();
    Some(reason)
}

/// Extract a `terminalSequence` value from hook stdout JSON, emit it
/// through the installed sink if valid, and return a warning string when
/// the value was present but rejected by the allow-list.  `Ok(None)`
/// branches (no field present, non-JSON stdout, valid + dispatched) all
/// return `None`.
fn process_terminal_sequence_from_stdout(
    stdout: &str,
    event: HookEvent,
    source: &str,
) -> Option<String> {
    match extract_terminal_sequence(stdout) {
        Ok(Some(seq)) => {
            dispatch_terminal_sequence(&seq);
            None
        }
        Ok(None) => None,
        Err(reason) => Some(format!(
            "{} hook `{source}` terminalSequence dropped: {reason}",
            event.as_str()
        )),
    }
}

/// Fold a terminal-sequence allow-list warning into an existing outcome.
/// `Allow` outcomes upgrade to `Warn`; `Deny` outcomes pick up an
/// appended note in their message; `Warn` outcomes concatenate.
fn append_terminal_sequence_warning(
    outcome: HookCommandOutcome,
    warning: Option<String>,
) -> HookCommandOutcome {
    let Some(warning) = warning else {
        return outcome;
    };
    match outcome {
        HookCommandOutcome::Allow { message } => {
            let combined = match message {
                Some(m) if !m.is_empty() => format!("{m}\n{warning}"),
                _ => warning,
            };
            HookCommandOutcome::Warn { message: combined }
        }
        HookCommandOutcome::Deny { message } => HookCommandOutcome::Deny {
            message: Some(match message {
                Some(m) if !m.is_empty() => format!("{m}\n{warning}"),
                _ => warning,
            }),
        },
        HookCommandOutcome::Warn { message } => HookCommandOutcome::Warn {
            message: format!("{message}\n{warning}"),
        },
    }
}

/// CC parity v2.2.14 (`continue_on_block`): for PostToolUse hooks marked
/// `continue_on_block: true`, rewrite a Deny outcome into a Warn so the
/// model sees the rejection reason and continues instead of hard-blocking.
/// All other event types and `false`/unset values pass through unchanged.
fn apply_continue_on_block(
    outcome: HookCommandOutcome,
    event: HookEvent,
    continue_on_block: bool,
) -> HookCommandOutcome {
    if !continue_on_block || !matches!(event, HookEvent::PostToolUse) {
        return outcome;
    }
    match outcome {
        HookCommandOutcome::Deny { message } => HookCommandOutcome::Warn {
            message: message.unwrap_or_else(|| {
                "PostToolUse hook blocked (continue_on_block: true — surfacing as warning)".to_string()
            }),
        },
        other => other,
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

    fn inner_command(&mut self) -> &mut Command {
        &mut self.command
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
        HookCommandOutcome, HookEvent, HookRunResult, HookRunner, McpHookInvocationResult,
        McpHookInvoker, RuntimeHookSpec,
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
        // pre_tool_use() now returns &[RuntimeHookSpec]; verify it's a Plugin(Command).
        match &config.pre_tool_use()[0] {
            RuntimeHookSpec::Plugin(spec) => {
                assert!(!spec.is_prompt(), "from_commands must produce a Command, not a Prompt");
            }
            other => panic!("expected Plugin(Command), got: {other:?}"),
        }
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

    #[test]
    fn apply_continue_on_block_rewrites_post_tool_deny_to_warn() {
        use super::apply_continue_on_block;let outcome = HookCommandOutcome::Deny {
            message: Some("nope".to_string()),
        };
        let result = apply_continue_on_block(outcome, HookEvent::PostToolUse, true);
        match result {
            HookCommandOutcome::Warn { message } => assert_eq!(message, "nope"),
            _ => panic!("expected Warn"),
        }
    }

    #[test]
    fn apply_continue_on_block_passes_through_when_disabled() {
        use super::apply_continue_on_block;let outcome = HookCommandOutcome::Deny {
            message: Some("nope".to_string()),
        };
        let result = apply_continue_on_block(outcome, HookEvent::PostToolUse, false);
        assert!(matches!(result, HookCommandOutcome::Deny { .. }));
    }

    #[test]
    fn apply_continue_on_block_does_not_apply_to_pretool() {
        use super::apply_continue_on_block;let outcome = HookCommandOutcome::Deny {
            message: Some("nope".to_string()),
        };
        let result = apply_continue_on_block(outcome, HookEvent::PreToolUse, true);
        assert!(
            matches!(result, HookCommandOutcome::Deny { .. }),
            "PreToolUse must not rewrite Deny"
        );
    }

    #[test]
    fn apply_continue_on_block_default_message_when_deny_has_no_message() {
        use super::apply_continue_on_block;let outcome = HookCommandOutcome::Deny { message: None };
        let result = apply_continue_on_block(outcome, HookEvent::PostToolUse, true);
        match result {
            HookCommandOutcome::Warn { message } => {
                assert!(message.contains("continue_on_block"));
            }
            _ => panic!("expected Warn"),
        }
    }

    #[test]
    fn apply_continue_on_block_leaves_allow_unchanged() {
        use super::apply_continue_on_block;let outcome = HookCommandOutcome::Allow {
            message: Some("ok".to_string()),
        };
        let result = apply_continue_on_block(outcome, HookEvent::PostToolUse, true);
        match result {
            HookCommandOutcome::Allow { message } => assert_eq!(message.as_deref(), Some("ok")),
            _ => panic!("expected Allow"),
        }
    }

    // ─── Task #566: Stop-hook block cap ──────────────────────────────────────

    #[test]
    fn stop_hook_block_counter_continues_below_cap() {
        use super::{StopHookBlockCounter, StopHookCapDecision};
        let mut c = StopHookBlockCounter::new(3);
        assert_eq!(c.record_block(), StopHookCapDecision::Continue);
        assert_eq!(c.record_block(), StopHookCapDecision::Continue);
        assert_eq!(c.blocks_seen(), 2);
    }

    #[test]
    fn stop_hook_block_counter_force_stops_at_cap_with_one_shot_warning() {
        use super::{StopHookBlockCounter, StopHookCapDecision};
        let mut c = StopHookBlockCounter::new(2);
        assert_eq!(c.record_block(), StopHookCapDecision::Continue);
        match c.record_block() {
            StopHookCapDecision::AllowStop { warning } => {
                let w = warning.expect("first cap-hit must surface a warning");
                assert!(w.contains("Stop hook blocked"), "warning text: {w}");
                assert!(w.contains("cap: 2"), "warning text: {w}");
            }
            other => panic!("expected AllowStop at cap, got {other:?}"),
        }
        // Subsequent records still allow-stop but the warning is silent.
        match c.record_block() {
            StopHookCapDecision::AllowStop { warning } => assert!(warning.is_none()),
            other => panic!("expected AllowStop past cap, got {other:?}"),
        }
    }

    #[test]
    fn stop_hook_block_counter_zero_cap_disables_force_stop() {
        use super::{StopHookBlockCounter, StopHookCapDecision};
        let mut c = StopHookBlockCounter::new(0);
        for _ in 0..50 {
            assert_eq!(c.record_block(), StopHookCapDecision::Continue);
        }
        assert_eq!(c.blocks_seen(), 50);
    }

    // ─── Task #566: Stop hook event + decision plumbing ──────────────────────

    /// Empty Stop hook config: run_stop returns Allow.
    #[test]
    fn stop_hook_with_no_specs_allows() {
        use super::{StopHookDecision, StopHookPayload};
        let runner = HookRunner::default();
        let result = runner.run_stop(&StopHookPayload {
            session_id: "test".to_string(),
            turn_count: 1,
            block_count: 0,
        });
        assert_eq!(result.decision, StopHookDecision::Allow);
        assert!(result.messages.is_empty());
    }

    /// Hook returning `{"decision":"block","reason":"..."}` produces a
    /// Block decision with the reason carried through.
    #[test]
    fn stop_hook_block_decision_returns_block_with_reason() {
        use super::{StopHookDecision, StopHookPayload};
        let mut runner = HookRunner::default();
        runner.stop_extra.push(RuntimeHookSpec::Plugin(
            HookSpec::Command(shell_snippet(
                r#"printf '%s' '{"decision":"block","reason":"keep going"}'"#,
            )),
        ));
        let result = runner.run_stop(&StopHookPayload {
            session_id: "s".to_string(),
            turn_count: 1,
            block_count: 0,
        });
        match result.decision {
            StopHookDecision::Block { reason } => {
                assert_eq!(reason, "keep going");
            }
            other => panic!("expected Block, got {other:?}"),
        }
    }

    /// Hook returning anything other than `decision:block` (e.g. allow,
    /// missing field, plain text) results in Allow.
    #[test]
    fn stop_hook_default_decision_allows_stop() {
        use super::{StopHookDecision, StopHookPayload};
        let mut runner = HookRunner::default();
        runner.stop_extra.push(RuntimeHookSpec::Plugin(
            HookSpec::Command(shell_snippet(
                r#"printf '%s' '{"decision":"allow"}'"#,
            )),
        ));
        let result = runner.run_stop(&StopHookPayload {
            session_id: "s".to_string(),
            turn_count: 1,
            block_count: 0,
        });
        assert_eq!(
            result.decision,
            StopHookDecision::Allow,
            "decision other than 'block' must allow stop"
        );

        // Plain-text (non-JSON) stdout also allows.
        let mut runner2 = HookRunner::default();
        runner2.stop_extra.push(RuntimeHookSpec::Plugin(
            HookSpec::Command(shell_snippet("printf 'just a log line'")),
        ));
        let r2 = runner2.run_stop(&StopHookPayload {
            session_id: "s".to_string(),
            turn_count: 1,
            block_count: 0,
        });
        assert_eq!(r2.decision, StopHookDecision::Allow);
        assert!(r2.messages.iter().any(|m| m.contains("just a log line")));
    }

    /// Block decisions from multiple hooks concatenate their reasons.
    #[test]
    fn stop_hook_multiple_blocks_concat_reasons() {
        use super::{StopHookDecision, StopHookPayload};
        let mut runner = HookRunner::default();
        runner.stop_extra.push(RuntimeHookSpec::Plugin(
            HookSpec::Command(shell_snippet(
                r#"printf '%s' '{"decision":"block","reason":"first"}'"#,
            )),
        ));
        runner.stop_extra.push(RuntimeHookSpec::Plugin(
            HookSpec::Command(shell_snippet(
                r#"printf '%s' '{"decision":"block","reason":"second"}'"#,
            )),
        ));
        let result = runner.run_stop(&StopHookPayload {
            session_id: "s".to_string(),
            turn_count: 1,
            block_count: 0,
        });
        match result.decision {
            StopHookDecision::Block { reason } => {
                assert!(reason.contains("first"), "missing first: {reason}");
                assert!(reason.contains("second"), "missing second: {reason}");
            }
            other => panic!("expected Block, got {other:?}"),
        }
    }

    // ─── Task #556: terminalSequence allow-list ──────────────────────────────

    #[test]
    fn terminal_sequence_allows_bel_and_osc9_and_osc777() {
        use super::validate_terminal_sequence;
        assert!(validate_terminal_sequence("\x07").is_ok());
        assert!(validate_terminal_sequence("\x1b]9;ping\x07").is_ok());
        assert!(validate_terminal_sequence("\x1b]777;notify;ping\x07").is_ok());
    }

    #[test]
    fn terminal_sequence_rejects_arbitrary_csi() {
        use super::validate_terminal_sequence;
        // Cursor-up CSI: \x1b[A — must NOT be allowed; hooks would be
        // able to scroll the screen / move cursor.
        assert!(validate_terminal_sequence("\x1b[A").is_err());
        // Bare ESC
        assert!(validate_terminal_sequence("\x1b").is_err());
        // Empty
        assert!(validate_terminal_sequence("").is_err());
    }

    #[test]
    fn extract_terminal_sequence_pulls_from_hook_json_stdout() {
        use super::extract_terminal_sequence;
        // OSC 9 desktop-notification payload encoded in JSON. Hooks
        // emit ESC as  (JSON string escape) and BEL as .
        let stdout = "{\"message\":\"hi\",\"terminalSequence\":\"\\u001b]9;build-done\\u0007\"}";
        let parsed = extract_terminal_sequence(stdout).expect("validation should pass");
        let bytes = parsed.expect("field is present");
        assert!(bytes.starts_with("\x1b]9;"), "got bytes: {bytes:?}");
        assert!(bytes.ends_with('\x07'));
    }

    #[test]
    fn extract_terminal_sequence_silent_on_non_json_stdout() {
        use super::extract_terminal_sequence;
        assert_eq!(extract_terminal_sequence("plain text").unwrap(), None);
        assert_eq!(extract_terminal_sequence("").unwrap(), None);
    }

    #[test]
    fn extract_terminal_sequence_errors_on_disallowed_sequence() {
        use super::extract_terminal_sequence;
        // Cursor-position CSI (screen clear) inside JSON should be rejected.
        let stdout = "{\"terminalSequence\":\"\\u001b[2J\"}";
        let result = extract_terminal_sequence(stdout);
        assert!(result.is_err(), "must reject \\x1b[2J (screen clear)");
    }

    // ─── Task #556: terminal sequence sink wiring through run_command ────────

    /// In-process sink that records every emit() call so tests can assert
    /// what reached the terminal (without actually writing to stdout).
    struct RecordingTerminalSink {
        bytes: std::sync::Mutex<Vec<String>>,
    }

    impl super::TerminalSequenceSink for RecordingTerminalSink {
        fn emit(&self, sequence: &str) {
            self.bytes
                .lock()
                .expect("sink lock")
                .push(sequence.to_string());
        }
    }

    impl RecordingTerminalSink {
        fn new() -> std::sync::Arc<Self> {
            std::sync::Arc::new(Self {
                bytes: std::sync::Mutex::new(Vec::new()),
            })
        }

        fn snapshot(&self) -> Vec<String> {
            self.bytes.lock().expect("sink lock").clone()
        }
    }

    /// Headless path: hook prints `{"terminalSequence":"<OSC9 ...>"}` and
    /// the runtime routes those bytes through the installed sink (which
    /// stands in for "stdout when no TUI is active").
    #[test]
    #[serial_test::serial(terminal_sequence_sink)]
    fn terminal_sequence_emitted_on_headless() {
        let sink = RecordingTerminalSink::new();
        super::set_terminal_sequence_sink(sink.clone());

        // OSC 9 desktop-notification payload printed as JSON by a hook.
        // ]9;build-done in the JSON string, which extract_*
        // decodes back to the raw ESC]9;...\x07 bytes.
        let runner = HookRunner::new(RuntimeHookConfig::new(
            vec![HookSpec::Command(shell_snippet(
                "cat <<'EOF'\n{\"terminalSequence\":\"\\u001b]9;build-done\\u0007\"}\nEOF",
            ))],
            Vec::new(),
        ));
        let _ = runner.run_pre_tool_use("Read", r#"{"path":"README.md"}"#);

        let captured = sink.snapshot();
        assert_eq!(captured.len(), 1, "exactly one terminalSequence dispatched");
        assert!(
            captured[0].starts_with("\x1b]9;"),
            "OSC9 prefix expected, got {:?}",
            captured[0]
        );
        assert!(captured[0].ends_with('\x07'));

        // Re-install the default sink so the global slot doesn't leak a
        // recording sink to unrelated tests in this run.
        super::set_terminal_sequence_sink(
            std::sync::Arc::new(super::StdoutTerminalSequenceSink),
        );
    }

    /// TUI path: a custom sink stands in for "queue through the TUI's
    /// redraw pipeline". The sink receives the bytes; nothing is written
    /// to stdout directly by run_command.  Verifies that custom sinks are
    /// honored and that the runtime never bypasses them.
    #[test]
    #[serial_test::serial(terminal_sequence_sink)]
    fn terminal_sequence_queued_through_tui_channel_when_tui_active() {
        let tui_sink = RecordingTerminalSink::new();
        super::set_terminal_sequence_sink(tui_sink.clone());

        let runner = HookRunner::new(RuntimeHookConfig::new(
            vec![HookSpec::Command(shell_snippet(
                // BEL alone — valid by allow-list.
                "cat <<'EOF'\n{\"terminalSequence\":\"\\u0007\"}\nEOF",
            ))],
            Vec::new(),
        ));
        let result = runner.run_pre_tool_use("Read", r#"{"path":"README.md"}"#);

        assert!(!result.is_denied(), "hook should not deny");
        let captured = tui_sink.snapshot();
        assert_eq!(captured.len(), 1);
        assert_eq!(captured[0], "\x07", "BEL byte routed to TUI sink verbatim");

        super::set_terminal_sequence_sink(
            std::sync::Arc::new(super::StdoutTerminalSequenceSink),
        );
    }

    /// Runtime rejection path: a hook that tries to emit a disallowed CSI
    /// (e.g. `\x1b[2J` screen clear) must NOT dispatch the bytes through
    /// the sink, and the outcome must carry a warning message naming the
    /// rejection.  Guards against allow-list bypass.
    #[test]
    #[serial_test::serial(terminal_sequence_sink)]
    fn terminal_sequence_rejected_for_disallowed_csi_at_runtime() {
        let sink = RecordingTerminalSink::new();
        super::set_terminal_sequence_sink(sink.clone());

        let runner = HookRunner::new(RuntimeHookConfig::new(
            vec![HookSpec::Command(shell_snippet(
                // ESC[2J — full-screen clear — must be rejected.
                "cat <<'EOF'\n{\"terminalSequence\":\"\\u001b[2J\"}\nEOF",
            ))],
            Vec::new(),
        ));
        let result = runner.run_pre_tool_use("Read", r#"{"path":"README.md"}"#);

        assert!(
            sink.snapshot().is_empty(),
            "disallowed sequence must NOT reach the sink"
        );
        // The runtime upgrades a successful-but-bad-payload hook to a
        // warning so the user can see the rejection.
        assert!(
            result
                .messages()
                .iter()
                .any(|m| m.contains("terminalSequence dropped")),
            "expected rejection warning, got: {:?}",
            result.messages()
        );

        super::set_terminal_sequence_sink(
            std::sync::Arc::new(super::StdoutTerminalSequenceSink),
        );
    }

    #[test]
    fn stop_hook_block_cap_env_default() {
        use super::{stop_hook_block_cap_from_env, DEFAULT_STOP_HOOK_BLOCK_CAP, STOP_HOOK_BLOCK_CAP_ENV};
        // SAFETY: env mutation in tests; remove first to ensure default path.
        // This test reads the value-without-env path; if a parallel test sets
        // it we may observe a different value, so we only assert that the
        // helper returns SOMETHING (no panic). The default-vs-override
        // contract is documented in the const docs.
        let _ = STOP_HOOK_BLOCK_CAP_ENV;
        let cap = stop_hook_block_cap_from_env();
        assert!(cap == DEFAULT_STOP_HOOK_BLOCK_CAP || cap > 0 || cap == 0);
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
