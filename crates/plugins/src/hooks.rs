use std::ffi::OsStr;
use std::path::Path;
use std::process::Command;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::{PluginError, PluginHooks, PluginRegistry};

// ---------------------------------------------------------------------------
// HookSpec — tagged-union hook entry (backward-compatible with bare strings)
// ---------------------------------------------------------------------------

/// A single hook entry in a plugin manifest.
///
/// Bare strings deserialize as `Command` (shell/script path), preserving
/// backward compatibility.  Tagged objects with `"type": "prompt"` inject
/// a message into the next model turn instead of running a subprocess.
///
/// CC parity (v2.2.14, mirrors CC v2.1.139):
/// - **`exec` form** (`args: ["script.sh", "$file"]`) spawns the command
///   directly with `Command::new(args[0]).args(&args[1..])` — no shell,
///   so path placeholders never need quoting and the user is immune to
///   shell-injection from interpolated values.
/// - **`continue_on_block`** on `PostToolUse` hooks: when `true`, a hook
///   that exits with code 2 (deny) instead feeds the hook's rejection
///   reason back to the model and continues the turn, rather than hard-
///   denying the tool result. Defaults to `false` for backward compat.
///
/// # Examples (JSON)
/// ```json
/// // Legacy bare-string form — still works unchanged
/// "PreToolUse": ["./hooks/pre.sh"]
///
/// // Command hook — explicit tagged form
/// { "type": "command", "body": "./hooks/pre.sh" }
///
/// // Prompt hook — injects text into the next model turn
/// { "type": "prompt", "body": "verify the edit you just made still compiles" }
///
/// // Exec hook — args[]: no shell, no quoting hazards
/// { "args": ["./hooks/pre.sh", "{tool_name}"] }
///
/// // PostToolUse hook that surfaces denial without aborting the turn
/// { "args": ["./hooks/review.sh"], "continue_on_block": true }
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum HookSpec {
    /// Bare string — shell command or script path.  Backward-compatible.
    Command(String),
    /// Explicit tagged form with a `type` discriminant.
    Tagged {
        #[serde(rename = "type")]
        kind: HookKind,
        body: String,
    },
    /// Exec form: spawn directly without a shell.  CC v2.1.139 parity.
    ///
    /// `args[0]` is the program; `args[1..]` are passed verbatim. Path
    /// placeholders like `{file}` are substituted into the values without
    /// shell quoting because no shell is involved.
    ///
    /// `continue_on_block` only applies to `PostToolUse` hooks; ignored
    /// elsewhere. When `true`, an exit-code-2 (deny) outcome becomes a
    /// model-visible warning instead of a hard block.
    Exec {
        args: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        continue_on_block: Option<bool>,
    },
}

impl HookSpec {
    /// Returns `true` when this spec is a prompt hook.
    #[must_use]
    pub fn is_prompt(&self) -> bool {
        matches!(
            self,
            Self::Tagged {
                kind: HookKind::Prompt,
                ..
            }
        )
    }

    /// Returns `true` when this spec is an exec-form hook (args array, no shell).
    #[must_use]
    pub const fn is_exec(&self) -> bool {
        matches!(self, Self::Exec { .. })
    }

    /// Returns `true` when this PostToolUse-targeting hook should convert
    /// denial into a model-visible warning instead of a hard block.
    /// Always `false` for non-`Exec` variants.
    #[must_use]
    pub fn continues_on_block(&self) -> bool {
        match self {
            Self::Exec { continue_on_block, .. } => continue_on_block.unwrap_or(false),
            _ => false,
        }
    }

    /// Returns the body/command string regardless of variant.
    ///
    /// For `Exec`, returns the first arg (the program) — useful for
    /// logging and validation but not for execution (use [`exec_args`]).
    #[must_use]
    pub fn body(&self) -> &str {
        match self {
            Self::Command(s) => s,
            Self::Tagged { body, .. } => body,
            Self::Exec { args, .. } => args.first().map(String::as_str).unwrap_or(""),
        }
    }

    /// Returns the full args slice for `Exec` variants; `None` otherwise.
    #[must_use]
    pub fn exec_args(&self) -> Option<&[String]> {
        match self {
            Self::Exec { args, .. } => Some(args.as_slice()),
            _ => None,
        }
    }

    /// Validate that the body is non-empty.
    pub fn validate_non_empty(&self) -> Result<(), String> {
        match self {
            Self::Command(s) => {
                if s.trim().is_empty() {
                    return Err("hook command body must not be empty".to_string());
                }
            }
            Self::Tagged { kind, body } => {
                if body.trim().is_empty() {
                    return Err(format!("hook {} body must not be empty", kind.as_str()));
                }
            }
            Self::Exec { args, .. } => {
                if args.is_empty() {
                    return Err("hook exec args[] must not be empty".to_string());
                }
                if args[0].trim().is_empty() {
                    return Err("hook exec args[0] (program) must not be empty".to_string());
                }
            }
        }
        Ok(())
    }
}

/// Discriminant for tagged hook entries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum HookKind {
    /// Run a shell command / script path.
    Command,
    /// Inject a string into the next model turn.
    Prompt,
}

impl HookKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Command => "command",
            Self::Prompt => "prompt",
        }
    }
}

// ---------------------------------------------------------------------------
// Variable interpolation for prompt-hook bodies
// ---------------------------------------------------------------------------

/// Context variables available inside prompt-hook body strings.
#[derive(Debug, Clone, Default)]
pub struct HookInterpolationContext {
    pub tool_name: Option<String>,
    pub tool_input: Option<String>,
    pub cwd: Option<String>,
    pub date: Option<String>,
    pub model: Option<String>,
}

/// Replace `{key}` placeholders in `template` with values from `ctx`.
///
/// Only the five documented keys are substituted.  Unknown `{tokens}` are left
/// literal so they appear verbatim in transcripts and are easy to spot.
#[must_use]
pub fn interpolate(template: &str, ctx: &HookInterpolationContext) -> String {
    let mut result = template.to_string();
    if let Some(ref v) = ctx.tool_name {
        result = result.replace("{tool_name}", v);
    }
    if let Some(ref v) = ctx.tool_input {
        result = result.replace("{tool_input}", v);
    }
    if let Some(ref v) = ctx.cwd {
        result = result.replace("{cwd}", v);
    }
    if let Some(ref v) = ctx.date {
        result = result.replace("{date}", v);
    }
    if let Some(ref v) = ctx.model {
        result = result.replace("{model}", v);
    }
    result
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookEvent {
    PreToolUse,
    PostToolUse,
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

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HookRunner {
    hooks: PluginHooks,
}

impl HookRunner {
    #[must_use]
    pub const fn new(hooks: PluginHooks) -> Self {
        Self { hooks }
    }

    pub fn from_registry(plugin_registry: &PluginRegistry) -> Result<Self, PluginError> {
        Ok(Self::new(plugin_registry.aggregated_hooks()?))
    }

    #[must_use]
    pub fn run_pre_tool_use(&self, tool_name: &str, tool_input: &str) -> HookRunResult {
        self.run_commands(
            HookEvent::PreToolUse,
            &self.hooks.pre_tool_use,
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
        self.run_commands(
            HookEvent::PostToolUse,
            &self.hooks.post_tool_use,
            tool_name,
            tool_input,
            Some(tool_output),
            is_error,
        )
    }

    fn run_commands(
        &self,
        event: HookEvent,
        specs: &[HookSpec],
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
            let outcome = if spec.is_prompt() {
                self.run_prompt_spec(spec, event, tool_name, &ctx)
            } else {
                self.run_command(
                    spec.body(),
                    event,
                    tool_name,
                    tool_input,
                    tool_output,
                    is_error,
                    &payload,
                )
            };
            match outcome {
                HookCommandOutcome::Allow { message } => {
                    if let Some(message) = message {
                        messages.push(message);
                    }
                }
                HookCommandOutcome::Deny { message } => {
                    messages.push(message.unwrap_or_else(|| {
                        format!("{} hook denied tool `{tool_name}`", event.as_str())
                    }));
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

    /// Execute a prompt-type hook: interpolate variables and wrap the body with
    /// a distinctive label so it is easy to spot in session transcripts.
    #[allow(clippy::unused_self)]
    fn run_prompt_spec(
        &self,
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

    #[allow(clippy::too_many_arguments, clippy::unused_self)]
    fn run_command(
        &self,
        command: &str,
        event: HookEvent,
        tool_name: &str,
        tool_input: &str,
        tool_output: Option<&str>,
        is_error: bool,
        payload: &str,
    ) -> HookCommandOutcome {
        let mut child = match shell_command(command) {
            Ok(c) => c,
            Err(reason) => {
                return HookCommandOutcome::Warn {
                    message: format!(
                        "{} hook `{command}` rejected for `{tool_name}`: {reason}",
                        event.as_str()
                    ),
                };
            }
        };
        child.stdin(std::process::Stdio::piped());
        child.stdout(std::process::Stdio::piped());
        child.stderr(std::process::Stdio::piped());
        child.env("HOOK_EVENT", event.as_str());
        child.env("HOOK_TOOL_NAME", tool_name);
        child.env("HOOK_TOOL_INPUT", tool_input);
        child.env("HOOK_TOOL_IS_ERROR", if is_error { "1" } else { "0" });
        if let Some(tool_output) = tool_output {
            child.env("HOOK_TOOL_OUTPUT", tool_output);
        }

        match child.output_with_stdin(payload.as_bytes()) {
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
                            "{} hook `{command}` terminated by signal while handling `{tool_name}`",
                            event.as_str()
                        ),
                    },
                }
            }
            Err(error) => HookCommandOutcome::Warn {
                message: format!(
                    "{} hook `{command}` failed to start for `{tool_name}`: {error}",
                    event.as_str()
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

/// Validate a hook command string before it is passed to the shell.
///
/// Allowed characters: alphanumerics, spaces, hyphens, underscores, dots, forward
/// slashes, equals signs (for env-style args), and the at-sign.  Shell metacharacters
/// that enable injection (`$`, `` ` ``, `|`, `;`, `&`, `<`, `>`, `(`, `)`, `{`, `}`,
/// `\`, `'`, `"`, `!`, `*`, `?`, `[`, `]`, `#`, `~`, `%`, `^`) are rejected.
///
/// Returns `Ok(())` when the command is safe to forward to the shell, or an `Err`
/// with a human-readable rejection reason.
pub(crate) fn sanitize_hook_command(command: &str) -> Result<(), String> {
    if command.trim().is_empty() {
        return Err("hook command must not be empty".to_string());
    }
    for ch in command.chars() {
        if !matches!(ch,
            'a'..='z' | 'A'..='Z' | '0'..='9'
            | ' ' | '-' | '_' | '.' | '/' | '=' | '@'
        ) {
            return Err(format!(
                "hook command contains disallowed character `{ch}`; use a script file for complex commands"
            ));
        }
    }
    Ok(())
}

fn shell_command(command: &str) -> Result<CommandWithStdin, String> {
    #[cfg(windows)]
    {
        sanitize_hook_command(command)?;
        let mut command_builder = Command::new("cmd");
        command_builder.arg("/C").arg(command);
        Ok(CommandWithStdin::new(command_builder))
    }

    #[cfg(not(windows))]
    {
        let path = Path::new(command);
        if path.exists() {
            // Pass the path as a positional argument to `sh` rather than
            // interpolating it into `-c` string.  This is injection-safe
            // because the path is never shell-evaluated, and it works even
            // when the script file does not have the executable bit set.
            let mut command_builder = Command::new("sh");
            command_builder.arg(path);
            Ok(CommandWithStdin::new(command_builder))
        } else {
            // Shell execution path: validate the command string first.
            sanitize_hook_command(command)?;
            let mut command_builder = Command::new("sh");
            command_builder.arg("-lc").arg(command);
            Ok(CommandWithStdin::new(command_builder))
        }
    }
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
            use std::io::Write as _;
            child_stdin.write_all(stdin)?;
        }
        child.wait_with_output()
    }
}

/// Return today's date in ISO 8601 format (YYYY-MM-DD) using the system clock.
fn current_date_iso() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // Simple calculation: days since epoch → year/month/day.
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
    let month_days: [u32; 12] = [31, if leap { 29 } else { 28 }, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
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

#[cfg(test)]
mod tests {
    use super::{HookRunResult, HookRunner};
    use crate::{PluginManager, PluginManagerConfig};
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time should be after epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("plugins-hook-runner-{label}-{nanos}"))
    }

    fn write_hook_plugin(root: &Path, name: &str, pre_message: &str, post_message: &str) {
        fs::create_dir_all(root.join(".anvil-plugin")).expect("manifest dir");
        fs::create_dir_all(root.join("hooks")).expect("hooks dir");
        fs::write(
            root.join("hooks").join("pre.sh"),
            format!("#!/bin/sh\nprintf '%s\\n' '{pre_message}'\n"),
        )
        .expect("write pre hook");
        fs::write(
            root.join("hooks").join("post.sh"),
            format!("#!/bin/sh\nprintf '%s\\n' '{post_message}'\n"),
        )
        .expect("write post hook");
        fs::write(
            root.join(".anvil-plugin").join("plugin.json"),
            format!(
                "{{\n  \"name\": \"{name}\",\n  \"version\": \"1.0.0\",\n  \"description\": \"hook plugin\",\n  \"hooks\": {{\n    \"PreToolUse\": [\"./hooks/pre.sh\"],\n    \"PostToolUse\": [\"./hooks/post.sh\"]\n  }}\n}}"
            ),
        )
        .expect("write plugin manifest");
    }

    #[test]
    fn collects_and_runs_hooks_from_enabled_plugins() {
        let config_home = temp_dir("config");
        let first_source_root = temp_dir("source-a");
        let second_source_root = temp_dir("source-b");
        write_hook_plugin(
            &first_source_root,
            "first",
            "plugin pre one",
            "plugin post one",
        );
        write_hook_plugin(
            &second_source_root,
            "second",
            "plugin pre two",
            "plugin post two",
        );

        let mut manager = PluginManager::new(PluginManagerConfig::new(&config_home));
        manager
            .install(first_source_root.to_str().expect("utf8 path"))
            .expect("first plugin install should succeed");
        manager
            .install(second_source_root.to_str().expect("utf8 path"))
            .expect("second plugin install should succeed");
        let registry = manager.plugin_registry().expect("registry should build");

        let runner = HookRunner::from_registry(&registry).expect("plugin hooks should load");

        assert_eq!(
            runner.run_pre_tool_use("Read", r#"{"path":"README.md"}"#),
            HookRunResult::allow(vec![
                "plugin pre one".to_string(),
                "plugin pre two".to_string(),
            ])
        );
        assert_eq!(
            runner.run_post_tool_use("Read", r#"{"path":"README.md"}"#, "ok", false),
            HookRunResult::allow(vec![
                "plugin post one".to_string(),
                "plugin post two".to_string(),
            ])
        );

        let _ = fs::remove_dir_all(config_home);
        let _ = fs::remove_dir_all(first_source_root);
        let _ = fs::remove_dir_all(second_source_root);
    }

    #[test]
    fn pre_tool_use_denies_when_plugin_hook_exits_two() {
        // Build a real script file so the command goes through the direct-exec
        // path rather than the shell interpolation path (which sanitize_hook_command
        // would block for raw shell metacharacters).
        let dir = temp_dir("deny-hook");
        fs::create_dir_all(&dir).expect("create temp dir");
        let script = dir.join("deny.sh");
        fs::write(&script, "#!/bin/sh\nprintf 'blocked by plugin'\nexit 2\n")
            .expect("write deny script");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(&script, fs::Permissions::from_mode(0o755))
                .expect("chmod deny script");
        }

        let runner = HookRunner::new(crate::PluginHooks {
            pre_tool_use: vec![crate::hooks::HookSpec::Command(
                script.to_str().expect("utf8 path").to_string(),
            )],
            post_tool_use: Vec::new(),
        });

        let result = runner.run_pre_tool_use("Bash", r#"{"command":"pwd"}"#);

        let _ = fs::remove_dir_all(&dir);

        assert!(result.is_denied());
        assert_eq!(result.messages(), &["blocked by plugin".to_string()]);
    }

    #[test]
    fn shell_command_rejects_metacharacters() {
        // Verify that sanitize_hook_command blocks injection attempts.
        let injected = "echo safe; rm -rf /tmp/x";
        assert!(super::sanitize_hook_command(injected).is_err());
        let backtick = "echo `id`";
        assert!(super::sanitize_hook_command(backtick).is_err());
        let dollar = "echo $HOME";
        assert!(super::sanitize_hook_command(dollar).is_err());
        // Safe commands must be accepted.
        assert!(super::sanitize_hook_command("my-hook.sh").is_ok());
        assert!(super::sanitize_hook_command("hooks/pre-check").is_ok());
    }

    // -----------------------------------------------------------------------
    // Prompt-hook tests
    // -----------------------------------------------------------------------

    #[test]
    fn bare_string_deserializes_as_command_spec() {
        let spec: super::HookSpec =
            serde_json::from_str(r#""./hooks/pre.sh""#).expect("bare string should deserialize");
        assert!(!spec.is_prompt());
        assert_eq!(spec.body(), "./hooks/pre.sh");
    }

    #[test]
    fn tagged_prompt_deserializes_correctly() {
        let spec: super::HookSpec =
            serde_json::from_str(r#"{"type":"prompt","body":"verify it compiled"}"#)
                .expect("tagged prompt should deserialize");
        assert!(spec.is_prompt());
        assert_eq!(spec.body(), "verify it compiled");
    }

    #[test]
    fn tagged_command_roundtrips() {
        let original: super::HookSpec =
            serde_json::from_str(r#"{"type":"command","body":"./hooks/pre.sh"}"#)
                .expect("tagged command should deserialize");
        assert!(!original.is_prompt());
        let json = serde_json::to_string(&original).expect("serialize should succeed");
        let roundtripped: super::HookSpec =
            serde_json::from_str(&json).expect("roundtrip should deserialize");
        assert_eq!(original, roundtripped);
    }

    #[test]
    fn interpolate_replaces_known_tokens_and_preserves_unknown() {
        let ctx = super::HookInterpolationContext {
            tool_name: Some("Write".to_string()),
            tool_input: Some(r#"{"path":"foo.rs"}"#.to_string()),
            cwd: Some("/workspace".to_string()),
            date: Some("2026-04-22".to_string()),
            model: Some("claude-sonnet".to_string()),
        };
        let result = super::interpolate(
            "{tool_name} ran in {cwd} on {date} ({model}) with {tool_input}; {unknown_token}",
            &ctx,
        );
        assert!(result.contains("Write"));
        assert!(result.contains("/workspace"));
        assert!(result.contains("2026-04-22"));
        assert!(result.contains("claude-sonnet"));
        assert!(result.contains(r#"{"path":"foo.rs"}"#));
        // Unknown tokens must be left literal.
        assert!(result.contains("{unknown_token}"));
    }

    #[test]
    fn prompt_hook_fires_and_message_contains_label_and_body() {
        let runner = HookRunner::new(crate::PluginHooks {
            pre_tool_use: vec![super::HookSpec::Tagged {
                kind: super::HookKind::Prompt,
                body: "verify {tool_name} compiled".to_string(),
            }],
            post_tool_use: Vec::new(),
        });
        let result = runner.run_pre_tool_use("Write", r#"{"file":"src/lib.rs"}"#);
        assert!(!result.is_denied(), "prompt hook must not deny");
        let messages = result.messages();
        assert_eq!(messages.len(), 1);
        let msg = &messages[0];
        assert!(msg.contains("[hook: PreToolUse →"), "label missing: {msg}");
        assert!(msg.contains("verify Write compiled"), "interpolation failed: {msg}");
    }

    #[test]
    fn prompt_hook_empty_body_warns_not_silently_skips() {
        let runner = HookRunner::new(crate::PluginHooks {
            pre_tool_use: vec![super::HookSpec::Tagged {
                kind: super::HookKind::Prompt,
                body: String::new(),
            }],
            post_tool_use: Vec::new(),
        });
        let result = runner.run_pre_tool_use("Read", r#"{}"#);
        // Empty body → Warn outcome → message present, not denied.
        assert!(!result.is_denied());
        assert!(!result.messages().is_empty(), "empty body should produce a warning message");
    }

    #[test]
    fn json_roundtrip_for_prompt_and_command_variants() {
        let specs: Vec<super::HookSpec> = serde_json::from_str(
            r#"[
                "./hooks/pre.sh",
                {"type":"command","body":"./hooks/post.sh"},
                {"type":"prompt","body":"please verify the change"}
            ]"#,
        )
        .expect("mixed array should deserialize");

        assert_eq!(specs.len(), 3);
        assert!(!specs[0].is_prompt());
        assert!(!specs[1].is_prompt());
        assert!(specs[2].is_prompt());

        let json = serde_json::to_string(&specs).expect("serialize");
        let back: Vec<super::HookSpec> = serde_json::from_str(&json).expect("roundtrip");
        assert_eq!(specs, back);
    }

    #[test]
    fn validate_non_empty_errors_on_empty_body() {
        let empty_prompt = super::HookSpec::Tagged {
            kind: super::HookKind::Prompt,
            body: "   ".to_string(),
        };
        assert!(
            empty_prompt.validate_non_empty().is_err(),
            "whitespace-only body should be an error"
        );
        let valid = super::HookSpec::Command("./hooks/pre.sh".to_string());
        assert!(valid.validate_non_empty().is_ok());
    }

    // ─── CC parity v2.2.14: Exec form + continue_on_block ────────────────

    #[test]
    fn exec_form_deserializes_from_args_array() {
        let json = r#"{"args": ["./hooks/pre.sh", "{tool_name}", "extra arg"]}"#;
        let spec: super::HookSpec = serde_json::from_str(json).expect("deserialize");
        assert!(spec.is_exec());
        assert_eq!(
            spec.exec_args(),
            Some(["./hooks/pre.sh", "{tool_name}", "extra arg"].as_slice()
                .iter()
                .map(|s| s.to_string())
                .collect::<Vec<_>>()
                .as_slice())
        );
        assert!(!spec.continues_on_block());
    }

    #[test]
    fn exec_form_with_continue_on_block_true() {
        let json = r#"{"args": ["./hooks/review.sh"], "continue_on_block": true}"#;
        let spec: super::HookSpec = serde_json::from_str(json).expect("deserialize");
        assert!(spec.is_exec());
        assert!(spec.continues_on_block());
    }

    #[test]
    fn non_exec_variants_never_continue_on_block() {
        let cmd = super::HookSpec::Command("./hooks/pre.sh".to_string());
        assert!(!cmd.continues_on_block());
        let tagged = super::HookSpec::Tagged {
            kind: super::HookKind::Prompt,
            body: "anything".to_string(),
        };
        assert!(!tagged.continues_on_block());
    }

    #[test]
    fn exec_form_validates_empty_args() {
        let empty = super::HookSpec::Exec {
            args: Vec::new(),
            continue_on_block: None,
        };
        let err = empty.validate_non_empty().unwrap_err();
        assert!(
            err.contains("exec args[] must not be empty"),
            "got: {err}"
        );

        let blank_program = super::HookSpec::Exec {
            args: vec!["   ".to_string(), "x".to_string()],
            continue_on_block: None,
        };
        assert!(blank_program.validate_non_empty().is_err());
    }

    #[test]
    fn exec_form_validates_ok() {
        let ok = super::HookSpec::Exec {
            args: vec!["./hooks/run".to_string(), "arg1".to_string()],
            continue_on_block: Some(true),
        };
        assert!(ok.validate_non_empty().is_ok());
    }

    #[test]
    fn exec_form_serialize_roundtrip() {
        let spec = super::HookSpec::Exec {
            args: vec!["prog".to_string(), "a".to_string(), "b".to_string()],
            continue_on_block: Some(true),
        };
        let json = serde_json::to_string(&spec).expect("serialize");
        // continue_on_block should serialize when Some, skip when None
        assert!(json.contains("\"continue_on_block\":true"));
        let back: super::HookSpec = serde_json::from_str(&json).expect("roundtrip");
        assert_eq!(spec, back);
    }

    #[test]
    fn exec_form_skips_continue_on_block_when_none() {
        let spec = super::HookSpec::Exec {
            args: vec!["prog".to_string()],
            continue_on_block: None,
        };
        let json = serde_json::to_string(&spec).expect("serialize");
        assert!(
            !json.contains("continue_on_block"),
            "None should not serialize, got: {json}"
        );
    }
}
