use std::path::PathBuf;
use std::process::{Command, Stdio};

use serde_json::Value;

use crate::manifest::{PluginToolDefinition, PluginToolPermission};
use crate::PluginError;

// ---------------------------------------------------------------------------
// PluginTool — executable tool provided by a plugin
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
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
    pub fn definition(&self) -> &PluginToolDefinition {
        &self.definition
    }

    #[must_use]
    pub fn required_permission(&self) -> &str {
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
