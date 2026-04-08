use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::json::JsonValue;

use super::helpers::{
    expect_object, expect_string, optional_string, optional_string_array, optional_string_map,
};
use super::ConfigError;

/// Mirrors `LspServerConfig` from the `lsp` crate but lives in the runtime config layer
/// so the config module does not take a hard dependency on lsp types.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LspServerEntry {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub workspace_root: PathBuf,
    /// Maps file extension (e.g. `.rs`) to LSP language id (e.g. `rust`).
    pub extension_to_language: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LspConfig {
    pub servers: Vec<LspServerEntry>,
}

/// Parse the optional `lspServers` array from the merged settings JSON.
///
/// Expected shape:
/// ```json
/// {
///   "lspServers": [
///     {
///       "name": "rust-analyzer",
///       "command": "rust-analyzer",
///       "args": [],
///       "env": {},
///       "workspaceRoot": "/path/to/project",
///       "extensionToLanguage": { ".rs": "rust" }
///     }
///   ]
/// }
/// ```
///
/// `workspace_root` defaults to `cwd` when the key is absent so callers do not
/// need to repeat the project root for the common single-workspace case.
pub fn parse_optional_lsp_config(root: &JsonValue, cwd: &Path) -> Result<LspConfig, ConfigError> {
    let Some(servers_value) = root.as_object().and_then(|o| o.get("lspServers")) else {
        return Ok(LspConfig::default());
    };
    let array = servers_value
        .as_array()
        .ok_or_else(|| ConfigError::Parse("merged settings.lspServers: must be an array".to_string()))?;

    let mut servers = Vec::with_capacity(array.len());
    for (index, item) in array.iter().enumerate() {
        let ctx = format!("merged settings.lspServers[{index}]");
        let object = expect_object(item, &ctx)?;
        let name = expect_string(object, "name", &ctx)?.to_string();
        let command = expect_string(object, "command", &ctx)?.to_string();
        let args = optional_string_array(object, "args", &ctx)?.unwrap_or_default();
        let env = optional_string_map(object, "env", &ctx)?.unwrap_or_default();
        let workspace_root = optional_string(object, "workspaceRoot", &ctx)?.map_or_else(|| cwd.to_path_buf(), PathBuf::from);
        let extension_to_language =
            optional_string_map(object, "extensionToLanguage", &ctx)?.unwrap_or_default();
        servers.push(LspServerEntry {
            name,
            command,
            args,
            env,
            workspace_root,
            extension_to_language,
        });
    }

    Ok(LspConfig { servers })
}
