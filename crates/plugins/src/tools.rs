use std::path::PathBuf;
use std::process::{Command, Stdio};

use serde_json::Value;

use crate::manifest::{PluginToolDefinition, PluginToolPermission};
use crate::PluginError;

// ---------------------------------------------------------------------------
// PluginTool — executable tool provided by a plugin
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginTool {
    pub(crate) plugin_id: String,
    pub(crate) plugin_name: String,
    pub(crate) definition: PluginToolDefinition,
    pub(crate) command: String,
    pub(crate) args: Vec<String>,
    pub(crate) required_permission: PluginToolPermission,
    pub(crate) root: Option<PathBuf>,
}

impl PluginTool {
    #[must_use]
    pub fn new(
        plugin_id: impl Into<String>,
        plugin_name: impl Into<String>,
        definition: PluginToolDefinition,
        command: impl Into<String>,
        args: Vec<String>,
        required_permission: PluginToolPermission,
        root: Option<PathBuf>,
    ) -> Self {
        Self {
            plugin_id: plugin_id.into(),
            plugin_name: plugin_name.into(),
            definition,
            command: command.into(),
            args,
            required_permission,
            root,
        }
    }

    #[must_use]
    pub fn plugin_id(&self) -> &str {
        &self.plugin_id
    }

    #[must_use]
    pub const fn definition(&self) -> &PluginToolDefinition {
        &self.definition
    }

    #[must_use]
    pub const fn required_permission(&self) -> &str {
        self.required_permission.as_str()
    }

    pub fn execute(&self, input: &Value) -> Result<String, PluginError> {
        let input_json = input.to_string();
        let mut process = Command::new(&self.command);
        process
            .args(&self.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env("ANVIL_PLUGIN_ID", &self.plugin_id)
            .env("ANVIL_PLUGIN_NAME", &self.plugin_name)
            .env("ANVIL_TOOL_NAME", &self.definition.name)
            .env("ANVIL_TOOL_INPUT", &input_json);
        if let Some(root) = &self.root {
            process
                .current_dir(root)
                .env("ANVIL_PLUGIN_ROOT", root.display().to_string());
        }
        // CC-DRIFT-B5 parity: propagate W3C trace context to plugin subprocesses
        // so that plugin tool spans link back to the parent Anvil trace.
        // The plugins crate does not depend on runtime, so we read TRACEPARENT
        // directly from the environment (the same value the runtime initialised
        // via otel::traceparent::init_from_env at startup).  If the env var is
        // absent or empty, we leave the child env unset — no TRACEPARENT beats
        // a malformed one.
        if let Ok(tp) = std::env::var("TRACEPARENT") {
            if !tp.trim().is_empty() {
                process.env("TRACEPARENT", &tp);
            }
        }
        if let Ok(ts) = std::env::var("TRACESTATE") {
            if !ts.trim().is_empty() {
                process.env("TRACESTATE", &ts);
            }
        }

        let mut child = process.spawn()?;
        if let Some(stdin) = child.stdin.as_mut() {
            use std::io::Write as _;
            stdin.write_all(input_json.as_bytes())?;
        }

        let output = child.wait_with_output()?;
        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            Err(PluginError::CommandFailed(format!(
                "plugin tool `{}` from `{}` failed for `{}`: {}",
                self.definition.name,
                self.plugin_id,
                self.command,
                if stderr.is_empty() {
                    format!("exit status {}", output.status)
                } else {
                    stderr
                }
            )))
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

// The TRACEPARENT propagation tests use std::env::set_var/remove_var which
// requires an unsafe block in Rust 2024.  This is the standard pattern for
// env-var tests in this codebase (see runtime/src/permission_memory.rs).
#[cfg_attr(test, allow(unsafe_code))]
#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::{PluginToolDefinition, PluginToolPermission};
    use serde_json::json;
    use serial_test::serial;

    fn make_tool(command: &str) -> PluginTool {
        PluginTool {
            plugin_id: "test-plugin".to_string(),
            plugin_name: "Test Plugin".to_string(),
            definition: PluginToolDefinition {
                name: "test_tool".to_string(),
                description: Some("a test tool".to_string()),
                input_schema: json!({}),
            },
            command: command.to_string(),
            args: Vec::new(),
            required_permission: PluginToolPermission::ReadOnly,
            root: None,
        }
    }

    /// Verify that a valid TRACEPARENT in the process environment is forwarded
    /// to the plugin subprocess.  We set the env var in the test process, run a
    /// shell one-liner that echoes it back, and confirm the value arrived.
    ///
    /// This test is Unix-only because the shell snippet uses `sh -c`.
    #[cfg(unix)]
    #[test]
    #[serial(traceparent_env)]
    fn plugin_tool_forwards_traceparent_to_subprocess() {
        // Use a syntactically valid W3C traceparent.  The exact value is
        // irrelevant — we just need to confirm it passes through.
        let fake_tp = "00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01";

        // Set TRACEPARENT in the test process environment so execute() picks it up.
        // SAFETY: single-threaded section of the test; no other thread modifies
        // TRACEPARENT concurrently.  This is the standard pattern used by
        // runtime/tests/otel_traceparent.rs.
        unsafe { std::env::set_var("TRACEPARENT", fake_tp) };

        // Build a minimal tool that prints the value of $TRACEPARENT to stdout.
        let mut tool_with_arg = make_tool("sh");
        tool_with_arg.args = vec!["-c".to_string(), "printf '%s' \"$TRACEPARENT\"".to_string()];

        let result = tool_with_arg.execute(&json!({}));

        // Clean up before asserting so failures don't leak state.
        unsafe { std::env::remove_var("TRACEPARENT") };

        let output = result.expect("tool should execute successfully");
        assert_eq!(
            output, fake_tp,
            "subprocess TRACEPARENT must match the value from the parent env; got: {output:?}"
        );
    }

    /// When TRACEPARENT is absent, the child must NOT receive a spurious
    /// TRACEPARENT env var (no injection of empty/garbage values).
    #[cfg(unix)]
    #[test]
    #[serial(traceparent_env)]
    fn plugin_tool_does_not_inject_traceparent_when_absent() {
        // Ensure no TRACEPARENT in env for this test.
        unsafe { std::env::remove_var("TRACEPARENT") };

        let tool = PluginTool {
            plugin_id: "test-plugin".to_string(),
            plugin_name: "Test Plugin".to_string(),
            definition: PluginToolDefinition {
                name: "test_tool".to_string(),
                description: Some("a test tool".to_string()),
                input_schema: json!({}),
            },
            command: "sh".to_string(),
            args: vec![
                "-c".to_string(),
                // Print "SET" if the var is set, "UNSET" if it is not.
                "if [ -n \"${TRACEPARENT+x}\" ]; then printf SET; else printf UNSET; fi".to_string(),
            ],
            required_permission: PluginToolPermission::ReadOnly,
            root: None,
        };

        let output = tool.execute(&json!({})).expect("tool should run");
        assert_eq!(
            output, "UNSET",
            "TRACEPARENT must not be injected when absent from parent env; got: {output:?}"
        );
    }
}
