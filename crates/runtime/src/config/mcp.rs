use std::collections::BTreeMap;

use crate::json::JsonValue;

use super::helpers::{
    expect_object, expect_string, optional_bool, optional_string, optional_string_array,
    optional_string_map, optional_u16,
};
use super::{ConfigError, ConfigSource};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpTransport {
    Stdio,
    Sse,
    Http,
    Ws,
    Sdk,
    ManagedProxy,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpServerConfig {
    Stdio(McpStdioServerConfig),
    Sse(McpRemoteServerConfig),
    Http(McpRemoteServerConfig),
    Ws(McpWebSocketServerConfig),
    Sdk(McpSdkServerConfig),
    ManagedProxy(McpManagedProxyServerConfig),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpStdioServerConfig {
    pub command: String,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
    /// FEAT-41: when `true`, all tools from this server bypass
    /// tool-search deferral and are exposed to the model from
    /// session start.  JSON key: `alwaysLoad` (defaults to `false`).
    pub always_load: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpRemoteServerConfig {
    pub url: String,
    pub headers: BTreeMap<String, String>,
    pub headers_helper: Option<String>,
    pub oauth: Option<McpOAuthConfig>,
    /// FEAT-41: when `true`, all tools from this server bypass
    /// tool-search deferral and are exposed to the model from
    /// session start.  JSON key: `alwaysLoad` (defaults to `false`).
    pub always_load: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpWebSocketServerConfig {
    pub url: String,
    pub headers: BTreeMap<String, String>,
    pub headers_helper: Option<String>,
    /// FEAT-41: when `true`, all tools from this server bypass
    /// tool-search deferral and are exposed to the model from
    /// session start.  JSON key: `alwaysLoad` (defaults to `false`).
    pub always_load: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpSdkServerConfig {
    pub name: String,
    /// FEAT-41: when `true`, all tools from this server bypass
    /// tool-search deferral and are exposed to the model from
    /// session start.  JSON key: `alwaysLoad` (defaults to `false`).
    pub always_load: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpManagedProxyServerConfig {
    pub url: String,
    pub id: String,
    /// FEAT-41: when `true`, all tools from this server bypass
    /// tool-search deferral and are exposed to the model from
    /// session start.  JSON key: `alwaysLoad` (defaults to `false`).
    pub always_load: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpOAuthConfig {
    pub client_id: Option<String>,
    pub callback_port: Option<u16>,
    pub auth_server_metadata_url: Option<String>,
    pub xaa: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct McpConfigCollection {
    pub(super) servers: BTreeMap<String, ScopedMcpServerConfig>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopedMcpServerConfig {
    pub scope: ConfigSource,
    pub config: McpServerConfig,
}

impl ScopedMcpServerConfig {
    #[must_use]
    pub const fn transport(&self) -> McpTransport {
        self.config.transport()
    }

    /// Convenience pass-through to `McpServerConfig::always_load`
    /// so callers operating on the scoped wrapper don't have to
    /// reach through `.config`.
    #[must_use]
    pub const fn always_load(&self) -> bool {
        self.config.always_load()
    }
}

impl McpServerConfig {
    #[must_use]
    pub const fn transport(&self) -> McpTransport {
        match self {
            Self::Stdio(_) => McpTransport::Stdio,
            Self::Sse(_) => McpTransport::Sse,
            Self::Http(_) => McpTransport::Http,
            Self::Ws(_) => McpTransport::Ws,
            Self::Sdk(_) => McpTransport::Sdk,
            Self::ManagedProxy(_) => McpTransport::ManagedProxy,
        }
    }

    /// FEAT-41 (Claude Code v2.1.121 parity): returns whether the
    /// server has opted out of tool-search deferral via the
    /// `alwaysLoad: true` JSON setting.
    ///
    /// When `true`, tools from this server are exposed to the model
    /// at session start instead of being discoverable only via
    /// `ToolSearch`.
    #[must_use]
    pub const fn always_load(&self) -> bool {
        match self {
            Self::Stdio(config) => config.always_load,
            Self::Sse(config) | Self::Http(config) => config.always_load,
            Self::Ws(config) => config.always_load,
            Self::Sdk(config) => config.always_load,
            Self::ManagedProxy(config) => config.always_load,
        }
    }
}

impl McpConfigCollection {
    #[must_use]
    pub const fn servers(&self) -> &BTreeMap<String, ScopedMcpServerConfig> {
        &self.servers
    }

    #[must_use]
    pub fn get(&self, name: &str) -> Option<&ScopedMcpServerConfig> {
        self.servers.get(name)
    }
}

/// Merge the `mcpServers` block from one settings file into the
/// running collection.
///
/// Partial-tolerance (BUG-34/35 parity): malformed entries are
/// logged to stderr and skipped rather than aborting the load.
/// Because every error path is a warn-and-skip, this function is
/// infallible and never returns an error.
pub fn merge_mcp_servers(
    target: &mut BTreeMap<String, ScopedMcpServerConfig>,
    source: ConfigSource,
    root: &BTreeMap<String, JsonValue>,
    path: &std::path::Path,
) {
    let Some(mcp_servers) = root.get("mcpServers") else {
        return;
    };
    // If `mcpServers` itself is the wrong shape, log and skip the
    // whole block rather than aborting the entire load.
    let Some(servers) = mcp_servers.as_object() else {
        eprintln!(
            "anvil: ignoring malformed mcpServers block in {}: expected JSON object",
            path.display()
        );
        return;
    };
    for (name, value) in servers {
        // Per-entry tolerance: one bad server should not nuke every
        // other server in the same file.
        match parse_mcp_server_config(
            name,
            value,
            &format!("{}: mcpServers.{name}", path.display()),
        ) {
            Ok(parsed) => {
                target.insert(
                    name.clone(),
                    ScopedMcpServerConfig {
                        scope: source,
                        config: parsed,
                    },
                );
            }
            Err(error) => {
                eprintln!(
                    "anvil: skipping malformed mcpServers.{name} in {}: {error}",
                    path.display()
                );
            }
        }
    }
}

pub fn parse_mcp_server_config(
    server_name: &str,
    value: &JsonValue,
    context: &str,
) -> Result<McpServerConfig, ConfigError> {
    let object = expect_object(value, context)?;
    let server_type = optional_string(object, "type", context)?.unwrap_or("stdio");
    // FEAT-41: `alwaysLoad` is shared across every transport variant.
    // A `true` value opts the server's tools out of tool-search
    // deferral.  Default is `false` (deferred).
    let always_load = optional_bool(object, "alwaysLoad", context)?.unwrap_or(false);
    match server_type {
        "stdio" => Ok(McpServerConfig::Stdio(McpStdioServerConfig {
            command: expect_string(object, "command", context)?.to_string(),
            args: optional_string_array(object, "args", context)?.unwrap_or_default(),
            env: optional_string_map(object, "env", context)?.unwrap_or_default(),
            always_load,
        })),
        "sse" => Ok(McpServerConfig::Sse(parse_mcp_remote_server_config(
            object,
            context,
            always_load,
        )?)),
        "http" => Ok(McpServerConfig::Http(parse_mcp_remote_server_config(
            object,
            context,
            always_load,
        )?)),
        "ws" => Ok(McpServerConfig::Ws(McpWebSocketServerConfig {
            url: expect_string(object, "url", context)?.to_string(),
            headers: optional_string_map(object, "headers", context)?.unwrap_or_default(),
            headers_helper: optional_string(object, "headersHelper", context)?.map(str::to_string),
            always_load,
        })),
        "sdk" => Ok(McpServerConfig::Sdk(McpSdkServerConfig {
            name: expect_string(object, "name", context)?.to_string(),
            always_load,
        })),
        "claudeai-proxy" => Ok(McpServerConfig::ManagedProxy(McpManagedProxyServerConfig {
            url: expect_string(object, "url", context)?.to_string(),
            id: expect_string(object, "id", context)?.to_string(),
            always_load,
        })),
        other => Err(ConfigError::Parse(format!(
            "{context}: unsupported MCP server type for {server_name}: {other}"
        ))),
    }
}

fn parse_mcp_remote_server_config(
    object: &BTreeMap<String, JsonValue>,
    context: &str,
    always_load: bool,
) -> Result<McpRemoteServerConfig, ConfigError> {
    Ok(McpRemoteServerConfig {
        url: expect_string(object, "url", context)?.to_string(),
        headers: optional_string_map(object, "headers", context)?.unwrap_or_default(),
        headers_helper: optional_string(object, "headersHelper", context)?.map(str::to_string),
        oauth: parse_optional_mcp_oauth_config(object, context)?,
        always_load,
    })
}

fn parse_optional_mcp_oauth_config(
    object: &BTreeMap<String, JsonValue>,
    context: &str,
) -> Result<Option<McpOAuthConfig>, ConfigError> {
    let Some(value) = object.get("oauth") else {
        return Ok(None);
    };
    let oauth = expect_object(value, &format!("{context}.oauth"))?;
    Ok(Some(McpOAuthConfig {
        client_id: optional_string(oauth, "clientId", context)?.map(str::to_string),
        callback_port: optional_u16(oauth, "callbackPort", context)?,
        auth_server_metadata_url: optional_string(oauth, "authServerMetadataUrl", context)?
            .map(str::to_string),
        xaa: optional_bool(oauth, "xaa", context)?,
    }))
}

#[cfg(test)]
mod tests {
    use super::{parse_mcp_server_config, McpServerConfig};
    use crate::json::JsonValue;

    fn parse(json: &str) -> McpServerConfig {
        let value = JsonValue::parse(json).expect("test fixture should parse as JSON");
        parse_mcp_server_config("test-server", &value, "test-context")
            .expect("test fixture should parse as MCP server config")
    }

    // FEAT-41: `alwaysLoad: true` opts a server's tools out of
    // tool-search deferral so they are immediately available to
    // the model at session start.
    #[test]
    fn always_load_true_is_honored_for_stdio_servers() {
        let config = parse(
            r#"{"command": "uvx", "args": ["mcp-server"], "alwaysLoad": true}"#,
        );
        assert!(
            config.always_load(),
            "alwaysLoad: true should set always_load"
        );
        match config {
            McpServerConfig::Stdio(stdio) => assert!(stdio.always_load),
            other => panic!("expected stdio config, got {other:?}"),
        }
    }

    #[test]
    fn always_load_defaults_to_false_when_field_is_omitted() {
        let config = parse(r#"{"command": "uvx", "args": ["mcp-server"]}"#);
        assert!(
            !config.always_load(),
            "missing alwaysLoad should default to false"
        );
        match config {
            McpServerConfig::Stdio(stdio) => assert!(!stdio.always_load),
            other => panic!("expected stdio config, got {other:?}"),
        }
    }

    #[test]
    fn always_load_propagates_to_remote_http_variant() {
        let config = parse(
            r#"{
              "type": "http",
              "url": "https://example.test/mcp",
              "alwaysLoad": true
            }"#,
        );
        assert!(config.always_load());
        match config {
            McpServerConfig::Http(remote) => assert!(remote.always_load),
            other => panic!("expected http config, got {other:?}"),
        }
    }

    #[test]
    fn always_load_propagates_to_websocket_sse_sdk_and_managed_proxy() {
        let ws = parse(
            r#"{"type": "ws", "url": "wss://example.test/mcp", "alwaysLoad": true}"#,
        );
        let sse = parse(
            r#"{"type": "sse", "url": "https://example.test/mcp", "alwaysLoad": true}"#,
        );
        let sdk = parse(r#"{"type": "sdk", "name": "in-process", "alwaysLoad": true}"#);
        let proxy = parse(
            r#"{"type": "claudeai-proxy", "url": "https://api.anthropic.com/v2/x", "id": "abc", "alwaysLoad": true}"#,
        );

        assert!(ws.always_load());
        assert!(sse.always_load());
        assert!(sdk.always_load());
        assert!(proxy.always_load());
    }
}
