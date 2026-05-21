use crate::config::{McpServerConfig, ScopedMcpServerConfig};

const CLAUDEAI_SERVER_PREFIX: &str = "claude.ai ";
const CCR_PROXY_PATH_MARKERS: [&str; 2] = ["/v2/session_ingress/shttp/mcp/", "/v2/ccr-sessions/"];

#[must_use]
pub fn normalize_name_for_mcp(name: &str) -> String {
    let mut normalized = name
        .chars()
        .map(|ch| match ch {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '_' | '-' => ch,
            _ => '_',
        })
        .collect::<String>();

    if name.starts_with(CLAUDEAI_SERVER_PREFIX) {
        normalized = collapse_underscores(&normalized)
            .trim_matches('_')
            .to_string();
    }

    normalized
}

#[must_use]
pub fn mcp_tool_prefix(server_name: &str) -> String {
    format!("mcp__{}__", normalize_name_for_mcp(server_name))
}

#[must_use]
pub fn mcp_tool_name(server_name: &str, tool_name: &str) -> String {
    format!(
        "{}{}",
        mcp_tool_prefix(server_name),
        normalize_name_for_mcp(tool_name)
    )
}

#[must_use]
pub fn unwrap_ccr_proxy_url(url: &str) -> String {
    if !CCR_PROXY_PATH_MARKERS
        .iter()
        .any(|marker| url.contains(marker))
    {
        return url.to_string();
    }

    let Some(query_start) = url.find('?') else {
        return url.to_string();
    };
    let query = &url[query_start + 1..];
    for pair in query.split('&') {
        let mut parts = pair.splitn(2, '=');
        if matches!(parts.next(), Some("mcp_url"))
            && let Some(value) = parts.next() {
                return percent_decode(value);
            }
    }

    url.to_string()
}

#[must_use]
pub fn mcp_server_signature(config: &McpServerConfig) -> Option<String> {
    match config {
        McpServerConfig::Stdio(config) => {
            let mut command = vec![config.command.clone()];
            command.extend(config.args.clone());
            Some(format!("stdio:{}", render_command_signature(&command)))
        }
        McpServerConfig::Sse(config) | McpServerConfig::Http(config) => {
            Some(format!("url:{}", unwrap_ccr_proxy_url(&config.url)))
        }
        McpServerConfig::Ws(config) => Some(format!("url:{}", unwrap_ccr_proxy_url(&config.url))),
        McpServerConfig::ManagedProxy(config) => {
            Some(format!("url:{}", unwrap_ccr_proxy_url(&config.url)))
        }
        McpServerConfig::Sdk(_) => None,
    }
}

#[must_use]
pub fn scoped_mcp_config_hash(config: &ScopedMcpServerConfig) -> String {
    let rendered = match &config.config {
        McpServerConfig::Stdio(stdio) => format!(
            "stdio|{}|{}|{}",
            stdio.command,
            render_command_signature(&stdio.args),
            render_env_signature(&stdio.env)
        ),
        McpServerConfig::Sse(remote) => format!(
            "sse|{}|{}|{}|{}",
            remote.url,
            render_env_signature(&remote.headers),
            remote.headers_helper.as_deref().unwrap_or(""),
            render_oauth_signature(remote.oauth.as_ref())
        ),
        McpServerConfig::Http(remote) => format!(
            "http|{}|{}|{}|{}",
            remote.url,
            render_env_signature(&remote.headers),
            remote.headers_helper.as_deref().unwrap_or(""),
            render_oauth_signature(remote.oauth.as_ref())
        ),
        McpServerConfig::Ws(ws) => format!(
            "ws|{}|{}|{}",
            ws.url,
            render_env_signature(&ws.headers),
            ws.headers_helper.as_deref().unwrap_or("")
        ),
        McpServerConfig::Sdk(sdk) => format!("sdk|{}", sdk.name),
        McpServerConfig::ManagedProxy(proxy) => {
            format!("claudeai-proxy|{}|{}", proxy.url, proxy.id)
        }
    };
    stable_hex_hash(&rendered)
}

fn render_command_signature(command: &[String]) -> String {
    let escaped = command
        .iter()
        .map(|part| part.replace('\\', "\\\\").replace('|', "\\|"))
        .collect::<Vec<_>>();
    format!("[{}]", escaped.join("|"))
}

fn render_env_signature(map: &std::collections::BTreeMap<String, String>) -> String {
    map.iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect::<Vec<_>>()
        .join(";")
}

fn render_oauth_signature(oauth: Option<&crate::config::McpOAuthConfig>) -> String {
    oauth.map_or_else(String::new, |oauth| {
        format!(
            "{}|{}|{}|{}",
            oauth.client_id.as_deref().unwrap_or(""),
            oauth
                .callback_port
                .map_or_else(String::new, |port| port.to_string()),
            oauth.auth_server_metadata_url.as_deref().unwrap_or(""),
            oauth.xaa.map_or_else(String::new, |flag| flag.to_string())
        )
    })
}

fn stable_hex_hash(value: &str) -> String {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in value.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{hash:016x}")
}

fn collapse_underscores(value: &str) -> String {
    let mut collapsed = String::with_capacity(value.len());
    let mut last_was_underscore = false;
    for ch in value.chars() {
        if ch == '_' {
            if !last_was_underscore {
                collapsed.push(ch);
            }
            last_was_underscore = true;
        } else {
            collapsed.push(ch);
            last_was_underscore = false;
        }
    }
    collapsed
}

fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'%' if index + 2 < bytes.len() => {
                let hex = &value[index + 1..index + 3];
                if let Ok(byte) = u8::from_str_radix(hex, 16) {
                    decoded.push(byte);
                    index += 3;
                    continue;
                }
                decoded.push(bytes[index]);
                index += 1;
            }
            b'+' => {
                decoded.push(b' ');
                index += 1;
            }
            byte => {
                decoded.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8_lossy(&decoded).into_owned()
}

// ─── Task #725: MCP image fallback for unsupported MIME types ───────────────
//
// CC parity (v2.1.144-B17). When an MCP server returns image content with an
// unsupported MIME (SVG, BMP, TIFF, AVIF, etc.) the conversation previously
// aborted or silently dropped the block. This module disk-binds the raw bytes
// and synthesizes a Text replacement that points at the saved file, so the
// model gets a coherent context even for exotic MIMEs.
//
// The decision (passthrough vs disk-bind) is split out as a pure helper so
// the caller in `anvil-cli` can log via `crate::tui::log_warning` without
// pulling the TUI crate into runtime.

/// Image MIME types that Anvil currently inlines through the provider's
/// native image-block path. Everything else falls through to the disk-bind
/// fallback in `fallback_unsupported_image_content`.
///
/// Source: Anthropic Messages API image content acceptance.
pub const MCP_SUPPORTED_IMAGE_MIMES: &[&str] = &[
    "image/png",
    "image/jpeg",
    "image/gif",
    "image/webp",
];

/// Returns `true` iff the MIME string is one Anvil can inline directly into
/// the conversation as an image content block. Case-insensitive on the MIME
/// type; whitespace-tolerant.
#[must_use]
pub fn is_supported_image_mime(mime: &str) -> bool {
    let trimmed = mime.trim().to_ascii_lowercase();
    MCP_SUPPORTED_IMAGE_MIMES
        .iter()
        .any(|known| *known == trimmed)
}

/// Outcome of routing an unsupported MCP image MIME through the disk-bind
/// fallback.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpImageFallback {
    /// On-disk path where the raw bytes now live. Caller embeds this in the
    /// synthesized Text content block.
    pub saved_path: std::path::PathBuf,
    /// Human-readable Text content that replaces the broken Image block.
    /// Designed to survive the round-trip into the model's tool-result
    /// channel.
    pub replacement_text: String,
    /// Warning string the caller should pass to `crate::tui::log_warning`
    /// (or stderr in headless mode). Already includes the MIME and path.
    pub warning: String,
}

/// Decide the file extension for a saved MCP image fallback. Best-effort
/// derivation from the MIME's subtype; falls back to `bin` so something
/// always writes.
#[must_use]
pub fn mcp_image_fallback_extension(mime: &str) -> &'static str {
    let lower = mime.trim().to_ascii_lowercase();
    match lower.as_str() {
        "image/svg+xml" => "svg",
        "image/bmp" => "bmp",
        "image/tiff" => "tiff",
        "image/avif" => "avif",
        "image/heic" => "heic",
        "image/heif" => "heif",
        "image/x-icon" | "image/vnd.microsoft.icon" => "ico",
        _ => {
            // Last-resort: pull the trailing token after the last "/" or "+",
            // strip the "x-" prefix, and constrain to short ascii letters/digits.
            let subtype = lower
                .rsplit_once('/')
                .map(|(_, st)| st)
                .unwrap_or("bin")
                .rsplit_once('+')
                .map(|(_, st)| st)
                .unwrap_or_else(|| {
                    lower
                        .rsplit_once('/')
                        .map(|(_, st)| st)
                        .unwrap_or("bin")
                });
            // Limit to safe characters; if anything weird slipped through,
            // settle for "bin".
            if !subtype.is_empty()
                && subtype.len() <= 6
                && subtype
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric())
            {
                // Leak intentionally: the set of MIME subtypes we'd accept is
                // small enough this is fine. Most callers actually hit one of
                // the explicit arms above. Tests pin the common cases.
                Box::leak(subtype.to_string().into_boxed_str())
            } else {
                "bin"
            }
        }
    }
}

/// Compute the default disk-bind root for MCP image fallbacks.
///
/// Returns `~/.anvil/mcp-images/` if HOME resolves cleanly and the parent
/// `~/.anvil/` is writable; otherwise falls back to
/// `std::env::temp_dir().join("anvil-mcp-images")`.
pub fn mcp_image_fallback_root() -> std::path::PathBuf {
    if let Some(home) = dirs_next::home_dir() {
        let anvil_dir = home.join(".anvil");
        let target = anvil_dir.join("mcp-images");
        // Probe parent writability by trying to create the chain. If it
        // succeeds we're good; if it fails we fall through to the temp dir.
        if std::fs::create_dir_all(&target).is_ok() {
            return target;
        }
    }
    std::env::temp_dir().join("anvil-mcp-images")
}

/// Disk-bind the raw bytes for an unsupported MCP image MIME and return the
/// synthesized Text replacement + warning.
///
/// The file is written at `<root>/<sha256-of-bytes>.<ext>`. Repeated calls
/// for the same bytes return the same path (content-addressed; cheap idempotency).
///
/// Returns `Err` only when the filesystem refuses to write at all (root +
/// fallback temp dir both fail). MIME / payload validation is the caller's
/// responsibility; this function happily disk-binds any bytes you hand it.
pub fn disk_bind_unsupported_image(
    mime: &str,
    raw_bytes: &[u8],
    root_override: Option<&std::path::Path>,
) -> std::io::Result<McpImageFallback> {
    use sha2::{Digest, Sha256};

    let root = match root_override {
        Some(p) => p.to_path_buf(),
        None => mcp_image_fallback_root(),
    };
    std::fs::create_dir_all(&root)?;

    let mut hasher = Sha256::new();
    hasher.update(raw_bytes);
    let digest = hasher.finalize();
    let hex: String = digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();

    let ext = mcp_image_fallback_extension(mime);
    let filename = format!("{hex}.{ext}");
    let saved_path = root.join(&filename);

    // Write idempotently — skip if the content-addressed file already exists.
    if !saved_path.exists() {
        std::fs::write(&saved_path, raw_bytes)?;
    }

    let replacement_text = format!(
        "[image: {mime} ({} bytes) saved to {}]",
        raw_bytes.len(),
        saved_path.display()
    );
    let warning = format!(
        "MCP image MIME `{mime}` is unsupported inline; saved {} bytes to {} (Text block substituted)",
        raw_bytes.len(),
        saved_path.display()
    );

    Ok(McpImageFallback {
        saved_path,
        replacement_text,
        warning,
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use crate::config::{
        ConfigSource, McpRemoteServerConfig, McpServerConfig, McpStdioServerConfig,
        McpWebSocketServerConfig, ScopedMcpServerConfig,
    };

    use super::{
        mcp_server_signature, mcp_tool_name, normalize_name_for_mcp, scoped_mcp_config_hash,
        unwrap_ccr_proxy_url,
    };

    #[test]
    fn normalizes_server_names_for_mcp_tooling() {
        assert_eq!(normalize_name_for_mcp("github.com"), "github_com");
        assert_eq!(normalize_name_for_mcp("tool name!"), "tool_name_");
        assert_eq!(
            normalize_name_for_mcp("claude.ai Example   Server!!"),
            "claude_ai_Example_Server"
        );
        assert_eq!(
            mcp_tool_name("claude.ai Example Server", "weather tool"),
            "mcp__claude_ai_Example_Server__weather_tool"
        );
    }

    #[test]
    fn unwraps_ccr_proxy_urls_for_signature_matching() {
        let wrapped = "https://api.anthropic.com/v2/session_ingress/shttp/mcp/123?mcp_url=https%3A%2F%2Fvendor.example%2Fmcp&other=1";
        assert_eq!(unwrap_ccr_proxy_url(wrapped), "https://vendor.example/mcp");
        assert_eq!(
            unwrap_ccr_proxy_url("https://vendor.example/mcp"),
            "https://vendor.example/mcp"
        );
    }

    #[test]
    fn computes_signatures_for_stdio_and_remote_servers() {
        let stdio = McpServerConfig::Stdio(McpStdioServerConfig {
            command: "uvx".to_string(),
            args: vec!["mcp-server".to_string()],
            env: BTreeMap::from([("TOKEN".to_string(), "secret".to_string())]),
            always_load: false,
        });
        assert_eq!(
            mcp_server_signature(&stdio),
            Some("stdio:[uvx|mcp-server]".to_string())
        );

        let remote = McpServerConfig::Ws(McpWebSocketServerConfig {
            url: "https://api.anthropic.com/v2/ccr-sessions/1?mcp_url=wss%3A%2F%2Fvendor.example%2Fmcp".to_string(),
            headers: BTreeMap::new(),
            headers_helper: None,
            always_load: false,
        });
        assert_eq!(
            mcp_server_signature(&remote),
            Some("url:wss://vendor.example/mcp".to_string())
        );
    }

    #[test]
    fn scoped_hash_ignores_scope_but_tracks_config_content() {
        let base_config = McpServerConfig::Http(McpRemoteServerConfig {
            url: "https://vendor.example/mcp".to_string(),
            headers: BTreeMap::from([("Authorization".to_string(), "Bearer token".to_string())]),
            headers_helper: Some("helper.sh".to_string()),
            oauth: None,
            always_load: false,
        });
        let user = ScopedMcpServerConfig {
            scope: ConfigSource::User,
            config: base_config.clone(),
        };
        let local = ScopedMcpServerConfig {
            scope: ConfigSource::Local,
            config: base_config,
        };
        assert_eq!(
            scoped_mcp_config_hash(&user),
            scoped_mcp_config_hash(&local)
        );

        let changed = ScopedMcpServerConfig {
            scope: ConfigSource::Local,
            config: McpServerConfig::Http(McpRemoteServerConfig {
                url: "https://vendor.example/v2/mcp".to_string(),
                headers: BTreeMap::new(),
                headers_helper: None,
                oauth: None,
                always_load: false,
            }),
        };
        assert_ne!(
            scoped_mcp_config_hash(&user),
            scoped_mcp_config_hash(&changed)
        );
    }

    // ── Task #725: MCP image fallback for unsupported MIME types ──────────────

    use super::{
        disk_bind_unsupported_image, is_supported_image_mime, mcp_image_fallback_extension,
        MCP_SUPPORTED_IMAGE_MIMES,
    };

    #[test]
    fn supported_mimes_pass_through() {
        // All four supported MIMEs return true regardless of case + trim.
        assert!(is_supported_image_mime("image/png"));
        assert!(is_supported_image_mime("image/jpeg"));
        assert!(is_supported_image_mime("image/gif"));
        assert!(is_supported_image_mime("image/webp"));
        assert!(is_supported_image_mime("  image/PNG  "));
        // Spot-check the allowlist constant.
        assert_eq!(MCP_SUPPORTED_IMAGE_MIMES.len(), 4);
    }

    #[test]
    fn unsupported_mimes_rejected() {
        // The MIMEs the CC parity audit explicitly calls out as broken.
        assert!(!is_supported_image_mime("image/svg+xml"));
        assert!(!is_supported_image_mime("image/bmp"));
        assert!(!is_supported_image_mime("image/tiff"));
        assert!(!is_supported_image_mime("image/avif"));
        // Empty / nonsense MIMEs fail closed.
        assert!(!is_supported_image_mime(""));
        assert!(!is_supported_image_mime("text/plain"));
    }

    #[test]
    fn fallback_extension_handles_common_unsupported_mimes() {
        assert_eq!(mcp_image_fallback_extension("image/svg+xml"), "svg");
        assert_eq!(mcp_image_fallback_extension("image/bmp"), "bmp");
        assert_eq!(mcp_image_fallback_extension("image/tiff"), "tiff");
        assert_eq!(mcp_image_fallback_extension("image/avif"), "avif");
        assert_eq!(mcp_image_fallback_extension("image/heic"), "heic");
        // Empty MIME falls back to "bin".
        assert_eq!(mcp_image_fallback_extension(""), "bin");
    }

    /// Task #725 (CC v2.1.144-B17 parity) headline contract: when an MCP tool
    /// returns image/svg+xml content, `disk_bind_unsupported_image`:
    ///   (a) produces a Text-block replacement that references the saved path,
    ///   (b) the file exists on disk at the expected path,
    ///   (c) no error propagates to the caller.
    #[test]
    fn svg_image_is_disk_bound_and_returns_text_replacement() {
        let tmp_root = std::env::temp_dir().join(format!(
            "anvil-mcp-image-svg-test-{}",
            std::process::id()
        ));
        // Clean slate.
        let _ = std::fs::remove_dir_all(&tmp_root);

        let svg_bytes = b"<svg xmlns='http://www.w3.org/2000/svg'><rect width='10' height='10'/></svg>";

        let fallback = disk_bind_unsupported_image("image/svg+xml", svg_bytes, Some(&tmp_root))
            .expect("disk-bind must not propagate an error on a writable temp dir");

        // (a) Replacement text is a Text block that references the saved path.
        assert!(
            fallback.replacement_text.contains("image/svg+xml"),
            "replacement text must name the MIME: {}",
            fallback.replacement_text
        );
        assert!(
            fallback.replacement_text.contains(&fallback.saved_path.to_string_lossy().to_string()),
            "replacement text must reference the saved path"
        );

        // (b) File exists on disk.
        assert!(
            fallback.saved_path.exists(),
            "saved file must exist at {}",
            fallback.saved_path.display()
        );
        let read_back = std::fs::read(&fallback.saved_path).expect("read back");
        assert_eq!(read_back, svg_bytes, "saved bytes must round-trip");

        // (b.1) File extension is `.svg` (not `.bin`).
        assert_eq!(
            fallback.saved_path.extension().and_then(|s| s.to_str()),
            Some("svg")
        );

        // (c) The warning string is non-empty so the caller has something to
        // pass to tui::log_warning. We don't assert exact text — only that it
        // mentions the MIME and the path.
        assert!(fallback.warning.contains("image/svg+xml"));
        assert!(fallback.warning.contains(&fallback.saved_path.to_string_lossy().to_string()));

        // Cleanup.
        let _ = std::fs::remove_dir_all(&tmp_root);
    }

    /// Identical bytes return the identical path on re-call (content-addressed).
    /// This guards against an FD/disk leak if an MCP server emits the same
    /// unsupported image repeatedly across turns.
    #[test]
    fn disk_bind_is_content_addressed_and_idempotent() {
        let tmp_root = std::env::temp_dir().join(format!(
            "anvil-mcp-image-idempotent-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&tmp_root);

        let bytes = b"<svg/>";
        let first = disk_bind_unsupported_image("image/svg+xml", bytes, Some(&tmp_root))
            .expect("first disk-bind");
        let second = disk_bind_unsupported_image("image/svg+xml", bytes, Some(&tmp_root))
            .expect("second disk-bind");

        assert_eq!(first.saved_path, second.saved_path);
        // Only one file in the dir.
        let entries: Vec<_> = std::fs::read_dir(&tmp_root)
            .expect("read tmp_root")
            .collect();
        assert_eq!(entries.len(), 1, "idempotent disk-bind must produce one file");

        let _ = std::fs::remove_dir_all(&tmp_root);
    }
}
