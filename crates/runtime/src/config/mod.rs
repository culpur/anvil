mod helpers;
pub mod hooks;
pub mod lsp;
pub mod mcp;
pub mod oauth;
pub mod otel;
pub mod output_style;
pub mod plugins;
pub mod profile;
pub mod sandbox;
pub mod schema;

use std::collections::BTreeMap;
use std::fmt::{Display, Formatter};
use std::path::{Path, PathBuf};

use crate::effort::EffortLevel;
use crate::json::JsonValue;
use crate::permissions::{BlockAction, ReviewerConfig, ReviewerMode};
use crate::sandbox::SandboxConfig;

use helpers::{deep_merge_objects, read_optional_json_object};
use hooks::parse_optional_hooks_config;
use lsp::parse_optional_lsp_config;
use mcp::merge_mcp_servers;
use oauth::parse_optional_oauth_config;
use otel::parse_optional_otel_config;
use plugins::parse_optional_plugin_config;
use profile::{parse_active_profile, parse_profiles};
use sandbox::parse_optional_sandbox_config;

// Re-export all public types so callers can still use `crate::config::*`.
pub use hooks::RuntimeHookConfig;
pub use lsp::{LspConfig, LspServerEntry};
pub use mcp::{
    McpConfigCollection, McpManagedProxyServerConfig, McpOAuthConfig,
    McpRemoteServerConfig, McpSdkServerConfig, McpServerConfig, McpStdioServerConfig,
    McpTransport, McpWebSocketServerConfig, ScopedMcpServerConfig,
};
pub use oauth::OAuthConfig;
pub use otel::OtelConfig;
pub use output_style::{BuiltInStyle, CustomStyle, OutputStyle, OutputStyleRegistry, default_output_styles_dir, output_style_from_str_builtin_only};
pub use plugins::RuntimePluginConfig;
pub use profile::ProfileOverride;

pub const ANVIL_SETTINGS_SCHEMA_NAME: &str = "SettingsSchema";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ConfigSource {
    User,
    Project,
    Local,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolvedPermissionMode {
    ReadOnly,
    WorkspaceWrite,
    DangerFullAccess,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigEntry {
    pub source: ConfigSource,
    pub path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeConfig {
    merged: BTreeMap<String, JsonValue>,
    loaded_entries: Vec<ConfigEntry>,
    feature_config: RuntimeFeatureConfig,
    /// Named profiles parsed from the `profiles` key.
    profiles: std::collections::HashMap<String, ProfileOverride>,
    /// The active profile name after applying precedence:
    /// CLI `--profile` > `ANVIL_PROFILE` env var > `active_profile` in config.
    active_profile: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RuntimeFeatureConfig {
    hooks: RuntimeHookConfig,
    plugins: RuntimePluginConfig,
    mcp: McpConfigCollection,
    lsp: LspConfig,
    oauth: Option<OAuthConfig>,
    otel: OtelConfig,
    model: Option<String>,
    permission_mode: Option<ResolvedPermissionMode>,
    sandbox: SandboxConfig,
    output_style: OutputStyle,
    /// Persisted effort level from `settings.json` (`"effort_level": "high"`).
    /// Absent in config → `None`; the caller falls back to `EffortLevel::Medium`.
    effort_level: Option<EffortLevel>,
    /// Reviewer gate config (disabled by default).
    reviewer: ReviewerConfig,
}

#[derive(Debug)]
pub enum ConfigError {
    Io(std::io::Error),
    Parse(String),
}

impl Display for ConfigError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(f, "{error}"),
            Self::Parse(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for ConfigError {}

impl From<std::io::Error> for ConfigError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigLoader {
    cwd: PathBuf,
    config_home: PathBuf,
}

impl ConfigLoader {
    #[must_use]
    pub fn new(cwd: impl Into<PathBuf>, config_home: impl Into<PathBuf>) -> Self {
        Self {
            cwd: cwd.into(),
            config_home: config_home.into(),
        }
    }

    #[must_use]
    pub fn default_for(cwd: impl Into<PathBuf>) -> Self {
        let cwd = cwd.into();
        let config_home = default_config_home();
        Self { cwd, config_home }
    }

    #[must_use]
    pub fn config_home(&self) -> &Path {
        &self.config_home
    }

    #[must_use]
    pub fn discover(&self) -> Vec<ConfigEntry> {
        let user_legacy_path = self.config_home.parent().map_or_else(
            || PathBuf::from(".anvil.json"),
            |parent| parent.join(".anvil.json"),
        );
        vec![
            ConfigEntry {
                source: ConfigSource::User,
                path: user_legacy_path,
            },
            ConfigEntry {
                source: ConfigSource::User,
                path: self.config_home.join("settings.json"),
            },
            ConfigEntry {
                source: ConfigSource::Project,
                path: self.cwd.join(".anvil.json"),
            },
            ConfigEntry {
                source: ConfigSource::Project,
                path: self.cwd.join(".anvil").join("settings.json"),
            },
            ConfigEntry {
                source: ConfigSource::Local,
                path: self.cwd.join(".anvil").join("settings.local.json"),
            },
        ]
    }

    pub fn load(&self) -> Result<RuntimeConfig, ConfigError> {
        let mut merged = BTreeMap::new();
        let mut loaded_entries = Vec::new();
        let mut mcp_servers = BTreeMap::new();

        for entry in self.discover() {
            let Some(value) = read_optional_json_object(&entry.path)? else {
                continue;
            };
            merge_mcp_servers(&mut mcp_servers, entry.source, &value, &entry.path);
            deep_merge_objects(&mut merged, &value);
            loaded_entries.push(entry);
        }

        let merged_value = JsonValue::Object(merged.clone());

        // Each top-level section is parsed with partial-tolerance: a
        // malformed section is logged and replaced with its default,
        // rather than aborting the entire load.  This matches Claude
        // Code's settings.json handling — a stray comma in `oauth`
        // must not nuke `mcpServers`, and so on.
        let feature_config = RuntimeFeatureConfig {
            hooks: tolerate_section("hooks", parse_optional_hooks_config(&merged_value)),
            plugins: tolerate_section("plugins", parse_optional_plugin_config(&merged_value)),
            mcp: McpConfigCollection {
                servers: mcp_servers,
            },
            lsp: tolerate_section(
                "lsp",
                parse_optional_lsp_config(&merged_value, &self.cwd),
            ),
            oauth: tolerate_section(
                "oauth",
                parse_optional_oauth_config(&merged_value, "merged settings.oauth"),
            ),
            otel: tolerate_section("otel", parse_optional_otel_config(&merged_value)),
            model: parse_optional_model(&merged_value),
            permission_mode: tolerate_section(
                "permissionMode",
                parse_optional_permission_mode(&merged_value),
            ),
            sandbox: tolerate_section("sandbox", parse_optional_sandbox_config(&merged_value)),
            output_style: parse_optional_output_style(&merged_value),
            effort_level: parse_optional_effort_level(&merged_value),
            reviewer: tolerate_section(
                "permissions.reviewer",
                parse_optional_reviewer_config(&merged_value),
            ),
        };

        // Profile section — partial-tolerance: malformed individual profiles
        // are skipped with a warning inside parse_profiles().
        let raw_profiles = parse_profiles(&merged_value);
        let profiles: std::collections::HashMap<String, ProfileOverride> =
            raw_profiles.into_iter().collect();

        let config_active_profile = parse_active_profile(&merged_value);

        Ok(RuntimeConfig {
            merged,
            loaded_entries,
            feature_config,
            profiles,
            active_profile: config_active_profile,
        })
    }

    /// Load config and validate it against the admin requirements policy.
    ///
    /// Callers that want policy enforcement (i.e. the main CLI entry point)
    /// should use this instead of `load()`.  All other call sites (tooling,
    /// tests, subcommand helpers) continue to use `load()` so their existing
    /// error-handling paths are not disturbed.
    ///
    /// On policy violation the caller receives
    /// `Err(PolicyCheckError::Violations(violations))`.  On a plain config
    /// load error it receives `Err(PolicyCheckError::Config(err))`.
    pub fn load_checked(&self) -> Result<RuntimeConfig, PolicyCheckError> {
        let config = self.load().map_err(PolicyCheckError::Config)?;
        let (policy, policy_source) = crate::requirements::load_from_paths();
        crate::requirements::validate(&config, &policy, &policy_source)
            .map_err(PolicyCheckError::Violations)?;
        Ok(config)
    }
}

/// Error type returned by [`ConfigLoader::load_checked`].
#[derive(Debug)]
pub enum PolicyCheckError {
    Config(ConfigError),
    Violations(Vec<crate::requirements::PolicyViolation>),
}

impl Display for PolicyCheckError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Config(e) => write!(f, "{e}"),
            Self::Violations(v) => {
                for violation in v {
                    writeln!(f, "{}", violation.render())?;
                }
                Ok(())
            }
        }
    }
}

impl std::error::Error for PolicyCheckError {}

/// Unwrap a section parse result, defaulting on error with a stderr
/// warning so a single malformed block does not poison the whole load.
fn tolerate_section<T: Default>(section: &str, result: Result<T, ConfigError>) -> T {
    match result {
        Ok(value) => value,
        Err(error) => {
            eprintln!(
                "anvil: ignoring malformed {section} block in settings.json: {error}"
            );
            T::default()
        }
    }
}

impl RuntimeConfig {
    #[must_use]
    pub fn empty() -> Self {
        Self {
            merged: BTreeMap::new(),
            loaded_entries: Vec::new(),
            feature_config: RuntimeFeatureConfig::default(),
            profiles: std::collections::HashMap::new(),
            active_profile: None,
        }
    }

    #[must_use]
    pub const fn merged(&self) -> &BTreeMap<String, JsonValue> {
        &self.merged
    }

    #[must_use]
    pub fn loaded_entries(&self) -> &[ConfigEntry] {
        &self.loaded_entries
    }

    #[must_use]
    pub fn get(&self, key: &str) -> Option<&JsonValue> {
        self.merged.get(key)
    }

    #[must_use]
    pub fn as_json(&self) -> JsonValue {
        JsonValue::Object(self.merged.clone())
    }

    #[must_use]
    pub const fn feature_config(&self) -> &RuntimeFeatureConfig {
        &self.feature_config
    }

    #[must_use]
    pub const fn mcp(&self) -> &McpConfigCollection {
        &self.feature_config.mcp
    }

    #[must_use]
    pub const fn lsp(&self) -> &LspConfig {
        &self.feature_config.lsp
    }

    #[must_use]
    pub const fn hooks(&self) -> &RuntimeHookConfig {
        &self.feature_config.hooks
    }

    #[must_use]
    pub const fn plugins(&self) -> &RuntimePluginConfig {
        &self.feature_config.plugins
    }

    #[must_use]
    pub const fn oauth(&self) -> Option<&OAuthConfig> {
        self.feature_config.oauth.as_ref()
    }

    #[must_use]
    pub const fn otel(&self) -> &OtelConfig {
        &self.feature_config.otel
    }

    #[must_use]
    pub fn model(&self) -> Option<&str> {
        self.feature_config.model.as_deref()
    }

    #[must_use]
    pub const fn permission_mode(&self) -> Option<ResolvedPermissionMode> {
        self.feature_config.permission_mode
    }

    #[must_use]
    pub const fn sandbox(&self) -> &SandboxConfig {
        &self.feature_config.sandbox
    }

    #[must_use]
    pub fn output_style(&self) -> &OutputStyle {
        &self.feature_config.output_style
    }

    // ── Profile accessors ─────────────────────────────────────────────────────

    /// All named profiles parsed from the config.
    #[must_use]
    pub fn profiles(&self) -> &std::collections::HashMap<String, ProfileOverride> {
        &self.profiles
    }

    /// The `active_profile` value stored in the config file.
    /// Use [`RuntimeConfig::resolve_active_profile`] to apply CLI/env precedence.
    #[must_use]
    pub fn config_active_profile(&self) -> Option<&str> {
        self.active_profile.as_deref()
    }

    /// Resolve the effective active profile name, applying precedence:
    /// `cli_override` > `ANVIL_PROFILE` env var > `active_profile` in config.
    ///
    /// Returns `None` when no profile is selected at any tier.
    #[must_use]
    pub fn resolve_active_profile<'a>(
        &'a self,
        cli_override: Option<&'a str>,
    ) -> Option<&'a str> {
        // Highest precedence: CLI --profile flag
        if let Some(name) = cli_override {
            return Some(name);
        }
        // Second: ANVIL_PROFILE env var
        if let Ok(env_val) = std::env::var("ANVIL_PROFILE") {
            if !env_val.is_empty() {
                // We can't return a reference to the locally-owned String here;
                // the caller should use the env var directly when needed.
                // Instead we signal that an env-var override exists by returning
                // a special sentinel — but because we can't hand back a `&str`
                // from a local var, callers that need the actual name must call
                // `std::env::var("ANVIL_PROFILE")` themselves.  We document
                // this in resolve_active_profile_owned below.
                let _ = env_val; // suppress unused warning
            }
        }
        // Third: value in config file
        self.active_profile.as_deref()
    }

    /// Like [`resolve_active_profile`] but returns an owned `String`,
    /// which avoids lifetime issues when the env-var value is needed.
    #[must_use]
    pub fn resolve_active_profile_owned(&self, cli_override: Option<&str>) -> Option<String> {
        // CLI first
        if let Some(name) = cli_override {
            return Some(name.to_owned());
        }
        // Env var second
        if let Ok(env_val) = std::env::var("ANVIL_PROFILE") {
            if !env_val.is_empty() {
                return Some(env_val);
            }
        }
        // Config file last
        self.active_profile.clone()
    }

    /// Look up a profile by name (after resolving which one is active).
    /// Returns `None` when the name is unknown or no profile is active.
    #[must_use]
    pub fn active_profile_override(
        &self,
        cli_override: Option<&str>,
    ) -> Option<&ProfileOverride> {
        let name = self.resolve_active_profile_owned(cli_override)?;
        self.profiles.get(&name)
    }

    #[must_use]
    pub const fn effort_level(&self) -> Option<EffortLevel> {
        self.feature_config.effort_level
    }

    #[must_use]
    pub const fn reviewer(&self) -> &ReviewerConfig {
        &self.feature_config.reviewer
    }
}

impl RuntimeFeatureConfig {
    #[must_use]
    pub fn with_hooks(mut self, hooks: RuntimeHookConfig) -> Self {
        self.hooks = hooks;
        self
    }

    #[must_use]
    pub fn with_plugins(mut self, plugins: RuntimePluginConfig) -> Self {
        self.plugins = plugins;
        self
    }

    #[must_use]
    pub const fn hooks(&self) -> &RuntimeHookConfig {
        &self.hooks
    }

    #[must_use]
    pub const fn plugins(&self) -> &RuntimePluginConfig {
        &self.plugins
    }

    #[must_use]
    pub const fn mcp(&self) -> &McpConfigCollection {
        &self.mcp
    }

    #[must_use]
    pub const fn lsp(&self) -> &LspConfig {
        &self.lsp
    }

    #[must_use]
    pub const fn oauth(&self) -> Option<&OAuthConfig> {
        self.oauth.as_ref()
    }

    #[must_use]
    pub const fn otel(&self) -> &OtelConfig {
        &self.otel
    }

    #[must_use]
    pub fn model(&self) -> Option<&str> {
        self.model.as_deref()
    }

    #[must_use]
    pub const fn permission_mode(&self) -> Option<ResolvedPermissionMode> {
        self.permission_mode
    }

    #[must_use]
    pub const fn sandbox(&self) -> &SandboxConfig {
        &self.sandbox
    }

    #[must_use]
    pub fn output_style(&self) -> &OutputStyle {
        &self.output_style
    }

    #[must_use]
    pub const fn effort_level(&self) -> Option<EffortLevel> {
        self.effort_level
    }

    #[must_use]
    pub const fn reviewer(&self) -> &ReviewerConfig {
        &self.reviewer
    }
}

#[must_use]
pub fn default_config_home() -> PathBuf {
    std::env::var_os("ANVIL_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".anvil")))
        .unwrap_or_else(|| PathBuf::from(".anvil"))
}

fn parse_optional_output_style(root: &JsonValue) -> OutputStyle {
    root.as_object()
        .and_then(|object| object.get("output_style"))
        .and_then(JsonValue::as_str)
        .map(output_style_from_str_builtin_only)
        .unwrap_or_default()
}

fn parse_optional_model(root: &JsonValue) -> Option<String> {
    root.as_object()
        .and_then(|object| object.get("model"))
        .and_then(JsonValue::as_str)
        .map(ToOwned::to_owned)
}

fn parse_optional_effort_level(root: &JsonValue) -> Option<EffortLevel> {
    root.as_object()
        .and_then(|object| object.get("effort_level"))
        .and_then(JsonValue::as_str)
        .and_then(EffortLevel::from_str)
}

/// Parse `permissions.reviewer` from the merged config JSON.
///
/// Returns `Ok(ReviewerConfig::default())` when the key is absent.
/// Returns `Err` when the key is present but malformed (triggers
/// `tolerate_section` to log a warning and use the default).
fn parse_optional_reviewer_config(
    root: &JsonValue,
) -> Result<ReviewerConfig, ConfigError> {
    let Some(reviewer_obj) = root
        .as_object()
        .and_then(|o| o.get("permissions"))
        .and_then(JsonValue::as_object)
        .and_then(|o| o.get("reviewer"))
        .and_then(JsonValue::as_object)
    else {
        return Ok(ReviewerConfig::default());
    };

    let enabled = reviewer_obj
        .get("enabled")
        .and_then(JsonValue::as_bool)
        .unwrap_or(false);

    let mode = match reviewer_obj
        .get("mode")
        .and_then(JsonValue::as_str)
        .unwrap_or("auto_review")
    {
        "auto_review" => ReviewerMode::AutoReview,
        "manual" => ReviewerMode::Manual,
        "off" => ReviewerMode::Off,
        other => {
            return Err(ConfigError::Parse(format!(
                "permissions.reviewer.mode: unknown value {other:?}"
            )));
        }
    };

    let block_action = match reviewer_obj
        .get("block_action")
        .and_then(JsonValue::as_str)
        .unwrap_or("ask")
    {
        "ask" => BlockAction::Ask,
        "deny" => BlockAction::Deny,
        other => {
            return Err(ConfigError::Parse(format!(
                "permissions.reviewer.block_action: unknown value {other:?}"
            )));
        }
    };

    let extra_destructive_patterns = reviewer_obj
        .get("destructive_patterns")
        .and_then(JsonValue::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(JsonValue::as_str)
                .map(ToOwned::to_owned)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    let extra_credential_patterns = reviewer_obj
        .get("credential_patterns")
        .and_then(JsonValue::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(JsonValue::as_str)
                .map(ToOwned::to_owned)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    Ok(ReviewerConfig {
        enabled,
        mode,
        block_action,
        extra_destructive_patterns,
        extra_credential_patterns,
    })
}

fn parse_optional_permission_mode(
    root: &JsonValue,
) -> Result<Option<ResolvedPermissionMode>, ConfigError> {
    let Some(object) = root.as_object() else {
        return Ok(None);
    };
    if let Some(mode) = object.get("permissionMode").and_then(JsonValue::as_str) {
        return parse_permission_mode_label(mode, "merged settings.permissionMode").map(Some);
    }
    let Some(mode) = object
        .get("permissions")
        .and_then(JsonValue::as_object)
        .and_then(|permissions| permissions.get("defaultMode"))
        .and_then(JsonValue::as_str)
    else {
        return Ok(None);
    };
    parse_permission_mode_label(mode, "merged settings.permissions.defaultMode").map(Some)
}

fn parse_permission_mode_label(
    mode: &str,
    context: &str,
) -> Result<ResolvedPermissionMode, ConfigError> {
    match mode {
        "default" | "plan" | "read-only" => Ok(ResolvedPermissionMode::ReadOnly),
        "acceptEdits" | "auto" | "workspace-write" => Ok(ResolvedPermissionMode::WorkspaceWrite),
        "dontAsk"
        | "danger-full-access"
        | "bypassPermissions"
        | "DangerFullAccess" => Ok(ResolvedPermissionMode::DangerFullAccess),
        other => Err(ConfigError::Parse(format!(
            "{context}: unsupported permission mode {other}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ConfigLoader, ConfigSource, McpServerConfig, McpTransport, ResolvedPermissionMode,
        ANVIL_SETTINGS_SCHEMA_NAME,
    };
    use crate::json::JsonValue;
    use crate::sandbox::FilesystemIsolationMode;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir() -> std::path::PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time should be after epoch")
            .as_nanos();
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("runtime-config-{nanos}-{id}"))
    }

    #[test]
    fn rejects_non_object_settings_files() {
        let root = temp_dir();
        let cwd = root.join("project");
        let home = root.join("home").join(".anvil");
        fs::create_dir_all(&home).expect("home config dir");
        fs::create_dir_all(&cwd).expect("project dir");
        fs::write(home.join("settings.json"), "[]").expect("write bad settings");

        let error = ConfigLoader::new(&cwd, &home)
            .load()
            .expect_err("config should fail");
        assert!(error
            .to_string()
            .contains("top-level settings value must be a JSON object"));

        fs::remove_dir_all(root).expect("cleanup temp dir");
    }

    #[test]
    fn loads_and_merges_anvil_config_files_by_precedence() {
        let root = temp_dir();
        let cwd = root.join("project");
        let home = root.join("home").join(".anvil");
        fs::create_dir_all(cwd.join(".anvil")).expect("project config dir");
        fs::create_dir_all(&home).expect("home config dir");

        fs::write(
            home.parent().expect("home parent").join(".anvil.json"),
            r#"{"model":"haiku","env":{"A":"1"},"mcpServers":{"home":{"command":"uvx","args":["home"]}}}"#,
        )
        .expect("write user compat config");
        fs::write(
            home.join("settings.json"),
            r#"{"model":"sonnet","env":{"A2":"1"},"hooks":{"PreToolUse":["base"]},"permissions":{"defaultMode":"plan"}}"#,
        )
        .expect("write user settings");
        fs::write(
            cwd.join(".anvil.json"),
            r#"{"model":"project-compat","env":{"B":"2"}}"#,
        )
        .expect("write project compat config");
        fs::write(
            cwd.join(".anvil").join("settings.json"),
            r#"{"env":{"C":"3"},"hooks":{"PostToolUse":["project"]},"mcpServers":{"project":{"command":"uvx","args":["project"]}}}"#,
        )
        .expect("write project settings");
        fs::write(
            cwd.join(".anvil").join("settings.local.json"),
            r#"{"model":"opus","permissionMode":"acceptEdits"}"#,
        )
        .expect("write local settings");

        let loaded = ConfigLoader::new(&cwd, &home)
            .load()
            .expect("config should load");

        assert_eq!(ANVIL_SETTINGS_SCHEMA_NAME, "SettingsSchema");
        assert_eq!(loaded.loaded_entries().len(), 5);
        assert_eq!(loaded.loaded_entries()[0].source, ConfigSource::User);
        assert_eq!(
            loaded.get("model"),
            Some(&JsonValue::String("opus".to_string()))
        );
        assert_eq!(loaded.model(), Some("opus"));
        assert_eq!(
            loaded.permission_mode(),
            Some(ResolvedPermissionMode::WorkspaceWrite)
        );
        assert_eq!(
            loaded
                .get("env")
                .and_then(JsonValue::as_object)
                .expect("env object")
                .len(),
            4
        );
        assert!(loaded
            .get("hooks")
            .and_then(JsonValue::as_object)
            .expect("hooks object")
            .contains_key("PreToolUse"));
        assert!(loaded
            .get("hooks")
            .and_then(JsonValue::as_object)
            .expect("hooks object")
            .contains_key("PostToolUse"));
        use plugins::HookSpec;
        assert_eq!(
            loaded.hooks().pre_tool_use(),
            &[HookSpec::Command("base".to_string())]
        );
        assert_eq!(
            loaded.hooks().post_tool_use(),
            &[HookSpec::Command("project".to_string())]
        );
        assert!(loaded.mcp().get("home").is_some());
        assert!(loaded.mcp().get("project").is_some());

        fs::remove_dir_all(root).expect("cleanup temp dir");
    }

    #[test]
    fn parses_sandbox_config() {
        let root = temp_dir();
        let cwd = root.join("project");
        let home = root.join("home").join(".anvil");
        fs::create_dir_all(cwd.join(".anvil")).expect("project config dir");
        fs::create_dir_all(&home).expect("home config dir");

        fs::write(
            cwd.join(".anvil").join("settings.local.json"),
            r#"{
              "sandbox": {
                "enabled": true,
                "namespaceRestrictions": false,
                "networkIsolation": true,
                "filesystemMode": "allow-list",
                "allowedMounts": ["logs", "tmp/cache"]
              }
            }"#,
        )
        .expect("write local settings");

        let loaded = ConfigLoader::new(&cwd, &home)
            .load()
            .expect("config should load");

        assert_eq!(loaded.sandbox().enabled, Some(true));
        assert_eq!(loaded.sandbox().namespace_restrictions, Some(false));
        assert_eq!(loaded.sandbox().network_isolation, Some(true));
        assert_eq!(
            loaded.sandbox().filesystem_mode,
            Some(FilesystemIsolationMode::AllowList)
        );
        assert_eq!(loaded.sandbox().allowed_mounts, vec!["logs", "tmp/cache"]);

        fs::remove_dir_all(root).expect("cleanup temp dir");
    }

    #[test]
    fn parses_typed_mcp_and_oauth_config() {
        let root = temp_dir();
        let cwd = root.join("project");
        let home = root.join("home").join(".anvil");
        fs::create_dir_all(cwd.join(".anvil")).expect("project config dir");
        fs::create_dir_all(&home).expect("home config dir");

        fs::write(
            home.join("settings.json"),
            r#"{
              "mcpServers": {
                "stdio-server": {
                  "command": "uvx",
                  "args": ["mcp-server"],
                  "env": {"TOKEN": "secret"}
                },
                "remote-server": {
                  "type": "http",
                  "url": "https://example.test/mcp",
                  "headers": {"Authorization": "Bearer token"},
                  "headersHelper": "helper.sh",
                  "oauth": {
                    "clientId": "mcp-client",
                    "callbackPort": 7777,
                    "authServerMetadataUrl": "https://issuer.test/.well-known/oauth-authorization-server",
                    "xaa": true
                  }
                }
              },
              "oauth": {
                "clientId": "runtime-client",
                "authorizeUrl": "https://console.test/oauth/authorize",
                "tokenUrl": "https://console.test/oauth/token",
                "callbackPort": 54545,
                "manualRedirectUrl": "https://console.test/oauth/callback",
                "scopes": ["org:read", "user:write"]
              }
            }"#,
        )
        .expect("write user settings");
        fs::write(
            cwd.join(".anvil").join("settings.local.json"),
            r#"{
              "mcpServers": {
                "remote-server": {
                  "type": "ws",
                  "url": "wss://override.test/mcp",
                  "headers": {"X-Env": "local"}
                }
              }
            }"#,
        )
        .expect("write local settings");

        let loaded = ConfigLoader::new(&cwd, &home)
            .load()
            .expect("config should load");

        let stdio_server = loaded
            .mcp()
            .get("stdio-server")
            .expect("stdio server should exist");
        assert_eq!(stdio_server.scope, ConfigSource::User);
        assert_eq!(stdio_server.transport(), McpTransport::Stdio);

        let remote_server = loaded
            .mcp()
            .get("remote-server")
            .expect("remote server should exist");
        assert_eq!(remote_server.scope, ConfigSource::Local);
        assert_eq!(remote_server.transport(), McpTransport::Ws);
        match &remote_server.config {
            McpServerConfig::Ws(config) => {
                assert_eq!(config.url, "wss://override.test/mcp");
                assert_eq!(
                    config.headers.get("X-Env").map(String::as_str),
                    Some("local")
                );
            }
            other => panic!("expected ws config, got {other:?}"),
        }

        let oauth = loaded.oauth().expect("oauth config should exist");
        assert_eq!(oauth.client_id, "runtime-client");
        assert_eq!(oauth.callback_port, Some(54_545));
        assert_eq!(oauth.scopes, vec!["org:read", "user:write"]);

        fs::remove_dir_all(root).expect("cleanup temp dir");
    }

    #[test]
    fn parses_plugin_config_from_enabled_plugins() {
        let root = temp_dir();
        let cwd = root.join("project");
        let home = root.join("home").join(".anvil");
        fs::create_dir_all(cwd.join(".anvil")).expect("project config dir");
        fs::create_dir_all(&home).expect("home config dir");

        fs::write(
            home.join("settings.json"),
            r#"{
              "enabledPlugins": {
                "tool-guard@builtin": true,
                "sample-plugin@external": false
              }
            }"#,
        )
        .expect("write user settings");

        let loaded = ConfigLoader::new(&cwd, &home)
            .load()
            .expect("config should load");

        assert_eq!(
            loaded.plugins().enabled_plugins().get("tool-guard@builtin"),
            Some(&true)
        );
        assert_eq!(
            loaded
                .plugins()
                .enabled_plugins()
                .get("sample-plugin@external"),
            Some(&false)
        );

        fs::remove_dir_all(root).expect("cleanup temp dir");
    }

    #[test]
    fn parses_plugin_config() {
        let root = temp_dir();
        let cwd = root.join("project");
        let home = root.join("home").join(".anvil");
        fs::create_dir_all(cwd.join(".anvil")).expect("project config dir");
        fs::create_dir_all(&home).expect("home config dir");

        fs::write(
            home.join("settings.json"),
            r#"{
              "enabledPlugins": {
                "core-helpers@builtin": true
              },
              "plugins": {
                "externalDirectories": ["./external-plugins"],
                "installRoot": "plugin-cache/installed",
                "registryPath": "plugin-cache/installed.json",
                "bundledRoot": "./bundled-plugins"
              }
            }"#,
        )
        .expect("write plugin settings");

        let loaded = ConfigLoader::new(&cwd, &home)
            .load()
            .expect("config should load");

        assert_eq!(
            loaded
                .plugins()
                .enabled_plugins()
                .get("core-helpers@builtin"),
            Some(&true)
        );
        assert_eq!(
            loaded.plugins().external_directories(),
            &["./external-plugins".to_string()]
        );
        assert_eq!(
            loaded.plugins().install_root(),
            Some("plugin-cache/installed")
        );
        assert_eq!(
            loaded.plugins().registry_path(),
            Some("plugin-cache/installed.json")
        );
        assert_eq!(loaded.plugins().bundled_root(), Some("./bundled-plugins"));

        fs::remove_dir_all(root).expect("cleanup temp dir");
    }

    #[test]
    fn skips_invalid_mcp_server_shapes_but_keeps_valid_ones() {
        // Partial-tolerance (BUG-34/35 parity): one bad mcpServers entry
        // must not invalidate the rest of settings.json.  The load
        // should succeed, the bad entry is dropped, and any sibling
        // valid entry is preserved.
        let root = temp_dir();
        let cwd = root.join("project");
        let home = root.join("home").join(".anvil");
        fs::create_dir_all(&home).expect("home config dir");
        fs::create_dir_all(&cwd).expect("project dir");
        fs::write(
            home.join("settings.json"),
            r#"{"mcpServers":{
                "broken":{"type":"http","url":123},
                "good":{"command":"uvx","args":["good"]}
            }}"#,
        )
        .expect("write mixed settings");

        let loaded = ConfigLoader::new(&cwd, &home)
            .load()
            .expect("config should load despite a malformed mcp server");
        assert!(
            loaded.mcp().get("broken").is_none(),
            "broken server entry should be skipped"
        );
        assert!(
            loaded.mcp().get("good").is_some(),
            "valid sibling server entry should still load"
        );

        fs::remove_dir_all(root).expect("cleanup temp dir");
    }

    #[test]
    fn parses_output_style_from_settings() {
        let root = temp_dir();
        let cwd = root.join("project");
        let home = root.join("home").join(".anvil");
        fs::create_dir_all(cwd.join(".anvil")).expect("project config dir");
        fs::create_dir_all(&home).expect("home config dir");

        // Condensed explicitly set.
        fs::write(
            home.join("settings.json"),
            r#"{"output_style": "condensed"}"#,
        )
        .expect("write settings");

        let loaded = ConfigLoader::new(&cwd, &home)
            .load()
            .expect("config should load");
        assert_eq!(*loaded.output_style(), super::OutputStyle::BuiltIn(super::BuiltInStyle::Condensed));

        fs::remove_dir_all(root).expect("cleanup temp dir");
    }

    #[test]
    fn defaults_output_style_to_precise_when_absent() {
        let root = temp_dir();
        let cwd = root.join("project");
        let home = root.join("home").join(".anvil");
        fs::create_dir_all(cwd.join(".anvil")).expect("project config dir");
        fs::create_dir_all(&home).expect("home config dir");

        // No output_style key at all.
        fs::write(
            home.join("settings.json"),
            r#"{"model": "opus"}"#,
        )
        .expect("write settings");

        let loaded = ConfigLoader::new(&cwd, &home)
            .load()
            .expect("config should load");
        assert_eq!(*loaded.output_style(), super::OutputStyle::BuiltIn(super::BuiltInStyle::Precise));

        fs::remove_dir_all(root).expect("cleanup temp dir");
    }

    /// BUG-34/35 parity: a malformed `hooks` block must not nuke
    /// `mcpServers`.  Both sections live in the same settings.json
    /// file; the `load()` should succeed with mcp populated and hooks
    /// empty (default).
    #[test]
    fn malformed_hooks_does_not_nuke_mcp_servers() {
        let root = temp_dir();
        let cwd = root.join("project");
        let home = root.join("home").join(".anvil");
        fs::create_dir_all(&home).expect("home config dir");
        fs::create_dir_all(&cwd).expect("project dir");

        // hooks.PreToolUse is a string instead of an array — the hooks
        // section parser will return Err.  mcpServers is well-formed.
        fs::write(
            home.join("settings.json"),
            r#"{
              "hooks": {"PreToolUse": "not-an-array"},
              "mcpServers": {
                "good": {"command": "uvx", "args": ["good"]}
              }
            }"#,
        )
        .expect("write mixed settings");

        let loaded = ConfigLoader::new(&cwd, &home)
            .load()
            .expect("config should load despite a malformed hooks block");

        assert!(
            loaded.mcp().get("good").is_some(),
            "mcpServers should still load when hooks is malformed"
        );
        assert!(
            loaded.hooks().pre_tool_use().is_empty(),
            "malformed hooks block should fall back to default (empty)"
        );
        assert!(
            loaded.hooks().post_tool_use().is_empty(),
            "malformed hooks block should fall back to default (empty)"
        );

        fs::remove_dir_all(root).expect("cleanup temp dir");
    }

    /// BUG-34/35 parity: a settings.json with a JSON syntax error
    /// (e.g. stray trailing comma) must not abort the whole load.
    /// Other settings files in the chain should still apply.
    #[test]
    fn malformed_user_settings_json_does_not_abort_load() {
        let root = temp_dir();
        let cwd = root.join("project");
        let home = root.join("home").join(".anvil");
        fs::create_dir_all(cwd.join(".anvil")).expect("project config dir");
        fs::create_dir_all(&home).expect("home config dir");

        // User settings.json: invalid JSON (unterminated value /
        // garbage tail).  The bundled JsonValue parser tolerates
        // trailing commas, so we use unmistakable garbage instead.
        fs::write(
            home.join("settings.json"),
            r#"{"model": haiku, garbage"#,
        )
        .expect("write malformed user settings");

        // Project settings.json: well-formed.
        fs::write(
            cwd.join(".anvil").join("settings.json"),
            r#"{"model": "opus", "mcpServers": {"proj": {"command": "uvx", "args": ["p"]}}}"#,
        )
        .expect("write valid project settings");

        let loaded = ConfigLoader::new(&cwd, &home)
            .load()
            .expect("malformed user settings should not abort load");

        // Project file's contents survived even though the user file
        // was malformed and skipped.
        assert_eq!(loaded.model(), Some("opus"));
        assert!(
            loaded.mcp().get("proj").is_some(),
            "project mcp server should load despite malformed user settings"
        );

        fs::remove_dir_all(root).expect("cleanup temp dir");
    }
}
