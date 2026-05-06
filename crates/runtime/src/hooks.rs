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
    mcp_invoker: Option<Arc<dyn McpHookInvoker>>,
}

impl std::fmt::Debug for HookRunner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HookRunner")
            .field("config", &self.config)
            .field("pre_tool_use_extra", &self.pre_tool_use_extra)
            .field("post_tool_use_extra", &self.post_tool_use_extra)
            .field("mcp_invoker", &self.mcp_invoker.as_ref().map(|_| "<dyn McpHookInvoker>"))
            .finish()
    }
}

impl PartialEq for HookRunner {
    fn eq(&self, other: &Self) -> bool {
        // Two runners with the same config + extras are considered equal,
        // regardless of whether an mcp_invoker is attached (trait objects
        // can't compare).
        self.config == other.config
            && self.pre_tool_use_extra == other.pre_tool_use_extra
            && self.post_tool_use_extra == other.post_tool_use_extra
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

    /// Merge the config-derived plugin-side specs with runtime-only extras
    /// (e.g. `McpTool`) into a single dispatch list.  Plugin specs come first
    /// to preserve existing ordering semantics for callers.
    fn collect_specs(&self, event: HookEvent) -> Vec<RuntimeHookSpec> {
        let (config_specs, extras) = match event {
            HookEvent::PreToolUse => (self.config.pre_tool_use(), &self.pre_tool_use_extra),
            HookEvent::PostToolUse => (self.config.post_tool_use(), &self.post_tool_use_extra),
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

    #[cfg(windows)]
    fn shell_snippet(script: &str) -> String {
        script.replace('\'', "\"")
    }

    #[cfg(not(windows))]
    fn shell_snippet(script: &str) -> String {
        script.to_string()
    }
}
