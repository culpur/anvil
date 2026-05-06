use std::collections::BTreeMap;

use crate::config::{McpOAuthConfig, McpServerConfig, ScopedMcpServerConfig};
use crate::mcp::{mcp_server_signature, mcp_tool_prefix, normalize_name_for_mcp};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpClientTransport {
    Stdio(McpStdioTransport),
    Sse(McpRemoteTransport),
    Http(McpRemoteTransport),
    WebSocket(McpRemoteTransport),
    Sdk(McpSdkTransport),
    ManagedProxy(McpManagedProxyTransport),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpStdioTransport {
    pub command: String,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpRemoteTransport {
    pub url: String,
    pub headers: BTreeMap<String, String>,
    pub headers_helper: Option<String>,
    pub auth: McpClientAuth,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpSdkTransport {
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpManagedProxyTransport {
    pub url: String,
    pub id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpClientAuth {
    None,
    OAuth(McpOAuthConfig),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpClientBootstrap {
    pub server_name: String,
    pub normalized_name: String,
    pub tool_prefix: String,
    pub signature: Option<String>,
    pub transport: McpClientTransport,
    /// FEAT-41 (Claude Code v2.1.121 parity): when `true`, all tools
    /// discovered from this server skip tool-search deferral and are
    /// exposed to the model immediately at session start.  Sourced
    /// directly from the server config's `alwaysLoad` JSON field.
    pub always_load: bool,
}

impl McpClientBootstrap {
    #[must_use]
    pub fn from_scoped_config(server_name: &str, config: &ScopedMcpServerConfig) -> Self {
        Self {
            server_name: server_name.to_string(),
            normalized_name: normalize_name_for_mcp(server_name),
            tool_prefix: mcp_tool_prefix(server_name),
            signature: mcp_server_signature(&config.config),
            transport: McpClientTransport::from_config(&config.config),
            always_load: config.always_load(),
        }
    }
}

impl McpClientTransport {
    #[must_use]
    pub fn from_config(config: &McpServerConfig) -> Self {
        match config {
            McpServerConfig::Stdio(config) => Self::Stdio(McpStdioTransport {
                command: config.command.clone(),
                args: config.args.clone(),
                env: config.env.clone(),
            }),
            McpServerConfig::Sse(config) => Self::Sse(McpRemoteTransport {
                url: config.url.clone(),
                headers: config.headers.clone(),
                headers_helper: config.headers_helper.clone(),
                auth: McpClientAuth::from_oauth(config.oauth.clone()),
            }),
            McpServerConfig::Http(config) => Self::Http(McpRemoteTransport {
                url: config.url.clone(),
                headers: config.headers.clone(),
                headers_helper: config.headers_helper.clone(),
                auth: McpClientAuth::from_oauth(config.oauth.clone()),
            }),
            McpServerConfig::Ws(config) => Self::WebSocket(McpRemoteTransport {
                url: config.url.clone(),
                headers: config.headers.clone(),
                headers_helper: config.headers_helper.clone(),
                auth: McpClientAuth::None,
            }),
            McpServerConfig::Sdk(config) => Self::Sdk(McpSdkTransport {
                name: config.name.clone(),
            }),
            McpServerConfig::ManagedProxy(config) => Self::ManagedProxy(McpManagedProxyTransport {
                url: config.url.clone(),
                id: config.id.clone(),
            }),
        }
    }
}

impl McpClientAuth {
    #[must_use]
    pub fn from_oauth(oauth: Option<McpOAuthConfig>) -> Self {
        oauth.map_or(Self::None, Self::OAuth)
    }

    #[must_use]
    pub const fn requires_user_auth(&self) -> bool {
        matches!(self, Self::OAuth(_))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use crate::config::{
        ConfigSource, McpOAuthConfig, McpRemoteServerConfig, McpSdkServerConfig, McpServerConfig,
        McpStdioServerConfig, McpWebSocketServerConfig, ScopedMcpServerConfig,
    };

    use super::{McpClientAuth, McpClientBootstrap, McpClientTransport};

    #[test]
    fn bootstraps_stdio_servers_into_transport_targets() {
        let config = ScopedMcpServerConfig {
            scope: ConfigSource::User,
            config: McpServerConfig::Stdio(McpStdioServerConfig {
                command: "uvx".to_string(),
                args: vec!["mcp-server".to_string()],
                env: BTreeMap::from([("TOKEN".to_string(), "secret".to_string())]),
                always_load: false,
            }),
        };

        let bootstrap = McpClientBootstrap::from_scoped_config("stdio-server", &config);
        assert_eq!(bootstrap.normalized_name, "stdio-server");
        assert_eq!(bootstrap.tool_prefix, "mcp__stdio-server__");
        assert_eq!(
            bootstrap.signature.as_deref(),
            Some("stdio:[uvx|mcp-server]")
        );
        match bootstrap.transport {
            McpClientTransport::Stdio(transport) => {
                assert_eq!(transport.command, "uvx");
                assert_eq!(transport.args, vec!["mcp-server"]);
                assert_eq!(
                    transport.env.get("TOKEN").map(String::as_str),
                    Some("secret")
                );
            }
            other => panic!("expected stdio transport, got {other:?}"),
        }
    }

    #[test]
    fn bootstraps_remote_servers_with_oauth_auth() {
        let config = ScopedMcpServerConfig {
            scope: ConfigSource::Project,
            config: McpServerConfig::Http(McpRemoteServerConfig {
                url: "https://vendor.example/mcp".to_string(),
                headers: BTreeMap::from([("X-Test".to_string(), "1".to_string())]),
                headers_helper: Some("helper.sh".to_string()),
                oauth: Some(McpOAuthConfig {
                    client_id: Some("client-id".to_string()),
                    callback_port: Some(7777),
                    auth_server_metadata_url: Some(
                        "https://issuer.example/.well-known/oauth-authorization-server".to_string(),
                    ),
                    xaa: Some(true),
                }),
                always_load: false,
            }),
        };

        let bootstrap = McpClientBootstrap::from_scoped_config("remote server", &config);
        assert_eq!(bootstrap.normalized_name, "remote_server");
        match bootstrap.transport {
            McpClientTransport::Http(transport) => {
                assert_eq!(transport.url, "https://vendor.example/mcp");
                assert_eq!(transport.headers_helper.as_deref(), Some("helper.sh"));
                assert!(transport.auth.requires_user_auth());
                match transport.auth {
                    McpClientAuth::OAuth(oauth) => {
                        assert_eq!(oauth.client_id.as_deref(), Some("client-id"));
                    }
                    other @ McpClientAuth::None => panic!("expected oauth auth, got {other:?}"),
                }
            }
            other => panic!("expected http transport, got {other:?}"),
        }
    }

    #[test]
    fn bootstraps_websocket_and_sdk_transports_without_oauth() {
        let ws = ScopedMcpServerConfig {
            scope: ConfigSource::Local,
            config: McpServerConfig::Ws(McpWebSocketServerConfig {
                url: "wss://vendor.example/mcp".to_string(),
                headers: BTreeMap::new(),
                headers_helper: None,
                always_load: false,
            }),
        };
        let sdk = ScopedMcpServerConfig {
            scope: ConfigSource::Local,
            config: McpServerConfig::Sdk(McpSdkServerConfig {
                name: "sdk-server".to_string(),
                always_load: false,
            }),
        };

        let ws_bootstrap = McpClientBootstrap::from_scoped_config("ws server", &ws);
        match ws_bootstrap.transport {
            McpClientTransport::WebSocket(transport) => {
                assert_eq!(transport.url, "wss://vendor.example/mcp");
                assert!(!transport.auth.requires_user_auth());
            }
            other => panic!("expected websocket transport, got {other:?}"),
        }

        let sdk_bootstrap = McpClientBootstrap::from_scoped_config("sdk server", &sdk);
        assert_eq!(sdk_bootstrap.signature, None);
        match sdk_bootstrap.transport {
            McpClientTransport::Sdk(transport) => {
                assert_eq!(transport.name, "sdk-server");
            }
            other => panic!("expected sdk transport, got {other:?}"),
        }
    }

    // FEAT-41: bootstraps must surface the `alwaysLoad` flag from
    // config so the tool-registration path can split discovered
    // tools between the deferred and always-available toolsets.
    #[test]
    fn bootstrap_propagates_always_load_from_config() {
        let opt_in = ScopedMcpServerConfig {
            scope: ConfigSource::User,
            config: McpServerConfig::Stdio(McpStdioServerConfig {
                command: "uvx".to_string(),
                args: vec!["mcp-server".to_string()],
                env: BTreeMap::new(),
                always_load: true,
            }),
        };
        let opt_out = ScopedMcpServerConfig {
            scope: ConfigSource::User,
            config: McpServerConfig::Stdio(McpStdioServerConfig {
                command: "uvx".to_string(),
                args: vec!["mcp-server".to_string()],
                env: BTreeMap::new(),
                always_load: false,
            }),
        };

        let opt_in_bootstrap = McpClientBootstrap::from_scoped_config("hot", &opt_in);
        let opt_out_bootstrap = McpClientBootstrap::from_scoped_config("cold", &opt_out);

        assert!(
            opt_in_bootstrap.always_load,
            "alwaysLoad: true must propagate to bootstrap"
        );
        assert!(
            !opt_out_bootstrap.always_load,
            "default (alwaysLoad omitted/false) must keep bootstrap deferred"
        );
    }
}
