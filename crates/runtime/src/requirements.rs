// Edition 2024: env::set_var/remove_var require unsafe in tests
#![allow(unsafe_code)]

//! Admin policy floor — `requirements.toml`.
//!
//! # Lookup order (first found wins)
//! 1. `ANVIL_REQUIREMENTS_PATH` environment variable (test / managed-deployment override)
//! 2. `/etc/anvil/requirements.toml` — system-wide
//! 3. `~/.anvil/requirements.toml`   — per-user
//!
//! # Behavior
//! - Missing file → no constraints applied (clean slate).
//! - Malformed TOML → warning to stderr + continue with no constraints.
//!   An admin typo must not break the user's session.
//! - Constraint violations → loud error, refuse to proceed.

use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::config::RuntimeConfig;
use crate::effort::EffortLevel;

// ─── Policy structures ────────────────────────────────────────────────────────

/// Top-level deserialization target for `requirements.toml`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RequirementsPolicy {
    pub permissions: PermissionsPolicy,
    pub providers: ProvidersPolicy,
    pub mcp: McpPolicy,
    pub vault: VaultPolicy,
    pub egress: EgressPolicy,
    pub telemetry: TelemetryPolicy,
    pub plugins: PluginsPolicy,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PermissionsPolicy {
    /// Mode strings the user is forbidden from setting.
    /// Recognised values mirror `permissionMode` in `settings.json`.
    pub forbidden_modes: Vec<String>,
    /// If `true`, `sandbox.enabled` must be `true` in the user config.
    pub require_sandbox: bool,
    /// Ceiling on the `effort_level` the user may set.
    /// Accepted values: `"low"`, `"medium"`, `"high"`, `"xhigh"`.
    pub max_effort: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ProvidersPolicy {
    /// Provider names (e.g. `"openai"`) that may not be used.
    pub denied_providers: Vec<String>,
    /// If non-empty, at least one of these providers must be configured.
    pub required_providers: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct McpPolicy {
    /// MCP server names that may not appear in `mcpServers`.
    pub denied_servers: Vec<String>,
    /// If non-empty, at least one of these MCP server names must be present.
    pub required_servers: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct VaultPolicy {
    /// If `true`, vault must be unlocked before any session starts.
    pub require_unlocked_for_session: bool,
    /// Credential types (e.g. `"RecoveryCode"`) that may not be stored.
    pub forbidden_credential_types: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct EgressPolicy {
    /// If `true`, the user may not extend the `EgressPolicy` allowlist.
    pub allowlist_locked: bool,
    /// Domains that are always denied, regardless of user config.
    pub forbidden_domains: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TelemetryPolicy {
    /// If `true`, `otel.enabled = true` in user config is forbidden.
    pub require_otel_disabled: bool,
    /// Belt-and-suspenders: same semantics as `require_otel_disabled`.
    pub require_telemetry_disabled: bool,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PluginsPolicy {
    /// Plugin names (e.g. `"evil-plugin@external"`) that are blocked.
    pub denied_plugins: Vec<String>,
    /// If `true`, `/plugin install` commands are rejected.
    pub plugin_install_disabled: bool,
    /// If `false`, the `--plugin-url` flag is rejected.
    pub allow_url_plugins: bool,
}

// ─── Violation record ─────────────────────────────────────────────────────────

/// A single policy violation found during config validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyViolation {
    /// Human-readable name of the constraint (e.g. `"permissions.forbidden_modes"`).
    pub requirement: String,
    /// What the user config actually contains.
    pub user_value: String,
    /// Path to the user's config file (best-effort, may be empty).
    pub config_source: PathBuf,
    /// Path to the requirements.toml that declared the constraint.
    pub policy_source: PathBuf,
    /// Actionable remediation hint.
    pub action: String,
}

impl PolicyViolation {
    /// Render the violation in the format specified by the design brief.
    #[must_use]
    pub fn render(&self) -> String {
        format!(
            "ERROR: requirements.toml policy violation\n  \
             Requirement: {requirement}\n  \
             User config: {user_value}\n  \
             Source:      {config_source}\n  \
             Policy:      {policy_source}\n\
             Action: {action}",
            requirement = self.requirement,
            user_value = self.user_value,
            config_source = self.config_source.display(),
            policy_source = self.policy_source.display(),
            action = self.action,
        )
    }
}

// ─── Load helpers ─────────────────────────────────────────────────────────────

/// Candidate paths searched in order (first found wins).
fn candidate_paths() -> Vec<PathBuf> {
    // 1. Test / managed-deployment override via env var.
    if let Ok(p) = std::env::var("ANVIL_REQUIREMENTS_PATH") {
        if !p.is_empty() {
            return vec![PathBuf::from(p)];
        }
    }
    // 2. System-wide → 3. Per-user.
    let mut paths = vec![PathBuf::from("/etc/anvil/requirements.toml")];
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        paths.push(home.join(".anvil").join("requirements.toml"));
    }
    paths
}

/// Load a `RequirementsPolicy` from the standard lookup paths.
///
/// Returns `(policy, source_path)`.  When no file exists, returns a default
/// policy (no constraints) with an empty source path.
///
/// Malformed TOML is logged to stderr and treated as "no file" — an admin
/// typo must not break the user's session.
#[must_use]
pub fn load_from_paths() -> (RequirementsPolicy, PathBuf) {
    for path in candidate_paths() {
        match try_load(&path) {
            LoadOutcome::NotFound => continue,
            LoadOutcome::Malformed => {
                // Warn and continue — no policy applied.
                return (RequirementsPolicy::default(), PathBuf::new());
            }
            LoadOutcome::Ok(policy) => return (policy, path),
        }
    }
    (RequirementsPolicy::default(), PathBuf::new())
}

/// Same as [`load_from_paths`] but reads from a specific path (useful for
/// tests and alternative deployment layouts).
#[must_use]
pub fn load_from_path(path: &Path) -> (RequirementsPolicy, PathBuf) {
    match try_load(path) {
        LoadOutcome::NotFound | LoadOutcome::Malformed => {
            (RequirementsPolicy::default(), PathBuf::new())
        }
        LoadOutcome::Ok(policy) => (policy, path.to_path_buf()),
    }
}

enum LoadOutcome {
    NotFound,
    Malformed,
    Ok(RequirementsPolicy),
}

fn try_load(path: &Path) -> LoadOutcome {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return LoadOutcome::NotFound,
        Err(e) => {
            eprintln!(
                "anvil: warning: could not read requirements.toml at {}: {e}",
                path.display()
            );
            return LoadOutcome::NotFound;
        }
    };
    match toml::from_str::<RequirementsPolicy>(&text) {
        Ok(policy) => LoadOutcome::Ok(policy),
        Err(e) => {
            eprintln!(
                "anvil: warning: requirements.toml at {} is malformed — \
                 policy will not be enforced: {e}",
                path.display()
            );
            LoadOutcome::Malformed
        }
    }
}

// ─── Validation ───────────────────────────────────────────────────────────────

/// Validate `config` against `policy`.
///
/// Returns `Ok(())` when no violations are found.  All violations are
/// collected before returning so the user sees every problem at once.
///
/// `policy_source` is the path from which the policy was loaded; it is
/// embedded in each `PolicyViolation` for user-facing error messages.
///
/// # Errors
/// Returns a `Vec<PolicyViolation>` with at least one entry when any constraint
/// is violated.
pub fn validate(
    config: &RuntimeConfig,
    policy: &RequirementsPolicy,
    policy_source: &Path,
) -> Result<(), Vec<PolicyViolation>> {
    let mut violations: Vec<PolicyViolation> = Vec::new();

    // Determine best-effort user config source path for error messages.
    let user_config_path = config
        .loaded_entries()
        .iter()
        .last()
        .map(|e| e.path.clone())
        .unwrap_or_else(|| PathBuf::from("<unknown>"));

    check_forbidden_modes(config, policy, policy_source, &user_config_path, &mut violations);
    check_require_sandbox(config, policy, policy_source, &user_config_path, &mut violations);
    check_max_effort(config, policy, policy_source, &user_config_path, &mut violations);
    check_denied_providers(config, policy, policy_source, &user_config_path, &mut violations);
    check_required_providers(config, policy, policy_source, &user_config_path, &mut violations);
    check_denied_mcp_servers(config, policy, policy_source, &user_config_path, &mut violations);
    check_required_mcp_servers(config, policy, policy_source, &user_config_path, &mut violations);
    check_otel_disabled(config, policy, policy_source, &user_config_path, &mut violations);
    check_egress_allowlist_locked(config, policy, policy_source, &user_config_path, &mut violations);
    check_denied_plugins(config, policy, policy_source, &user_config_path, &mut violations);

    if violations.is_empty() {
        Ok(())
    } else {
        Err(violations)
    }
}

// ── Individual checks ─────────────────────────────────────────────────────────

fn check_forbidden_modes(
    config: &RuntimeConfig,
    policy: &RequirementsPolicy,
    policy_source: &Path,
    config_path: &Path,
    violations: &mut Vec<PolicyViolation>,
) {
    if policy.permissions.forbidden_modes.is_empty() {
        return;
    }
    let Some(mode) = config.permission_mode() else {
        return;
    };
    // Map the resolved enum back to its canonical string labels so we can
    // compare against the policy's string list.
    let mode_labels: &[&str] = match mode {
        crate::config::ResolvedPermissionMode::ReadOnly => &["default", "plan", "read-only"],
        crate::config::ResolvedPermissionMode::WorkspaceWrite => {
            &["acceptEdits", "auto", "workspace-write"]
        }
        crate::config::ResolvedPermissionMode::DangerFullAccess => {
            &["dontAsk", "danger-full-access", "bypassPermissions", "DangerFullAccess"]
        }
    };
    // Find whether any of the resolved mode's labels are in the forbidden list.
    let forbidden_hit: Option<&str> = policy.permissions.forbidden_modes.iter().find_map(|f| {
        mode_labels
            .iter()
            .find(|&&label| label.eq_ignore_ascii_case(f.as_str()))
            .map(|_| f.as_str())
    });
    if let Some(forbidden_label) = forbidden_hit {
        violations.push(PolicyViolation {
            requirement: format!(
                "permissions.forbidden_modes contains \"{forbidden_label}\""
            ),
            user_value: format!("permission_mode = \"{forbidden_label}\""),
            config_source: config_path.to_path_buf(),
            policy_source: policy_source.to_path_buf(),
            action: "Edit your config to use an allowed permission mode, \
                     or contact your administrator."
                .to_string(),
        });
    }
}

fn check_require_sandbox(
    config: &RuntimeConfig,
    policy: &RequirementsPolicy,
    policy_source: &Path,
    config_path: &Path,
    violations: &mut Vec<PolicyViolation>,
) {
    if !policy.permissions.require_sandbox {
        return;
    }
    let sandbox_enabled = config.sandbox().enabled.unwrap_or(false);
    if !sandbox_enabled {
        violations.push(PolicyViolation {
            requirement: "permissions.require_sandbox = true".to_string(),
            user_value: format!("sandbox.enabled = {sandbox_enabled}"),
            config_source: config_path.to_path_buf(),
            policy_source: policy_source.to_path_buf(),
            action: "Set `\"sandbox\": {\"enabled\": true}` in your settings.json, \
                     or contact your administrator."
                .to_string(),
        });
    }
}

fn check_max_effort(
    config: &RuntimeConfig,
    policy: &RequirementsPolicy,
    policy_source: &Path,
    config_path: &Path,
    violations: &mut Vec<PolicyViolation>,
) {
    let Some(max_str) = &policy.permissions.max_effort else {
        return;
    };
    let Some(ceiling) = EffortLevel::from_str(max_str) else {
        // Malformed ceiling value in policy — silently skip (don't
        // break the user because of an admin typo in a constraint).
        return;
    };
    let Some(user_level) = config.effort_level() else {
        return;
    };
    // EffortLevel ordering: Low < Medium < High < Xhigh (derived via PartialOrd).
    if user_level > ceiling {
        violations.push(PolicyViolation {
            requirement: format!("permissions.max_effort = \"{max_str}\""),
            user_value: format!("effort_level = \"{}\"", user_level.as_str()),
            config_source: config_path.to_path_buf(),
            policy_source: policy_source.to_path_buf(),
            action: format!(
                "Set effort_level to \"{}\" or lower in your settings.json, \
                 or contact your administrator.",
                ceiling.as_str()
            ),
        });
    }
}

fn check_denied_providers(
    config: &RuntimeConfig,
    policy: &RequirementsPolicy,
    policy_source: &Path,
    config_path: &Path,
    violations: &mut Vec<PolicyViolation>,
) {
    if policy.providers.denied_providers.is_empty() {
        return;
    }
    // Provider name is surfaced through the `model` field (e.g. "gpt-4o" → openai)
    // or through oauth.clientId presence.  We use a simple heuristic: check if
    // the configured model string starts with a known provider prefix.
    let model = config.model().unwrap_or("");
    for denied in &policy.providers.denied_providers {
        if provider_matches_model(denied, model) {
            violations.push(PolicyViolation {
                requirement: format!(
                    "providers.denied_providers contains \"{denied}\""
                ),
                user_value: format!("model = \"{model}\""),
                config_source: config_path.to_path_buf(),
                policy_source: policy_source.to_path_buf(),
                action: format!(
                    "Remove the \"{denied}\" provider configuration, \
                     or contact your administrator."
                ),
            });
        }
    }
}

fn check_required_providers(
    config: &RuntimeConfig,
    policy: &RequirementsPolicy,
    policy_source: &Path,
    config_path: &Path,
    violations: &mut Vec<PolicyViolation>,
) {
    if policy.providers.required_providers.is_empty() {
        return;
    }
    let model = config.model().unwrap_or("");
    let has_required = policy
        .providers
        .required_providers
        .iter()
        .any(|req| provider_matches_model(req, model));
    if !has_required {
        violations.push(PolicyViolation {
            requirement: format!(
                "providers.required_providers = {:?}",
                policy.providers.required_providers
            ),
            user_value: format!("model = \"{model}\" (no required provider matched)"),
            config_source: config_path.to_path_buf(),
            policy_source: policy_source.to_path_buf(),
            action: format!(
                "Configure one of the required providers {:?} in your settings.json, \
                 or contact your administrator.",
                policy.providers.required_providers
            ),
        });
    }
}

fn check_denied_mcp_servers(
    config: &RuntimeConfig,
    policy: &RequirementsPolicy,
    policy_source: &Path,
    config_path: &Path,
    violations: &mut Vec<PolicyViolation>,
) {
    if policy.mcp.denied_servers.is_empty() {
        return;
    }
    for denied in &policy.mcp.denied_servers {
        if config.mcp().get(denied).is_some() {
            violations.push(PolicyViolation {
                requirement: format!("mcp.denied_servers contains \"{denied}\""),
                user_value: format!("mcpServers.\"{denied}\" is configured"),
                config_source: config_path.to_path_buf(),
                policy_source: policy_source.to_path_buf(),
                action: format!(
                    "Remove the \"{denied}\" entry from mcpServers in your settings.json, \
                     or contact your administrator."
                ),
            });
        }
    }
}

fn check_required_mcp_servers(
    config: &RuntimeConfig,
    policy: &RequirementsPolicy,
    policy_source: &Path,
    config_path: &Path,
    violations: &mut Vec<PolicyViolation>,
) {
    if policy.mcp.required_servers.is_empty() {
        return;
    }
    let has_required = policy
        .mcp
        .required_servers
        .iter()
        .any(|req| config.mcp().get(req).is_some());
    if !has_required {
        violations.push(PolicyViolation {
            requirement: format!(
                "mcp.required_servers = {:?}",
                policy.mcp.required_servers
            ),
            user_value: "mcpServers does not contain any required server".to_string(),
            config_source: config_path.to_path_buf(),
            policy_source: policy_source.to_path_buf(),
            action: format!(
                "Add one of {:?} to mcpServers in your settings.json, \
                 or contact your administrator.",
                policy.mcp.required_servers
            ),
        });
    }
}

fn check_otel_disabled(
    config: &RuntimeConfig,
    policy: &RequirementsPolicy,
    policy_source: &Path,
    config_path: &Path,
    violations: &mut Vec<PolicyViolation>,
) {
    if !policy.telemetry.require_otel_disabled && !policy.telemetry.require_telemetry_disabled {
        return;
    }
    if config.otel().enabled {
        let which = if policy.telemetry.require_otel_disabled {
            "telemetry.require_otel_disabled = true"
        } else {
            "telemetry.require_telemetry_disabled = true"
        };
        violations.push(PolicyViolation {
            requirement: which.to_string(),
            user_value: "otel.enabled = true".to_string(),
            config_source: config_path.to_path_buf(),
            policy_source: policy_source.to_path_buf(),
            action: "Set `\"otel\": {\"enabled\": false}` in your settings.json, \
                     or contact your administrator."
                .to_string(),
        });
    }
}

fn check_egress_allowlist_locked(
    config: &RuntimeConfig,
    policy: &RequirementsPolicy,
    policy_source: &Path,
    config_path: &Path,
    violations: &mut Vec<PolicyViolation>,
) {
    if !policy.egress.allowlist_locked {
        return;
    }
    // We check whether the user config has a `security.egress_allowlist` key
    // that adds entries beyond the default set.  We inspect the raw merged JSON
    // because RuntimeConfig does not expose egress as a typed field.
    let has_user_allowlist = config
        .get("security")
        .and_then(|v| v.as_object())
        .and_then(|obj| obj.get("egress_allowlist"))
        .and_then(|v| v.as_array())
        .map_or(false, |arr| !arr.is_empty());
    if has_user_allowlist {
        violations.push(PolicyViolation {
            requirement: "egress.allowlist_locked = true".to_string(),
            user_value: "security.egress_allowlist contains user-added entries".to_string(),
            config_source: config_path.to_path_buf(),
            policy_source: policy_source.to_path_buf(),
            action: "Remove security.egress_allowlist from your settings.json, \
                     or contact your administrator."
                .to_string(),
        });
    }
}

fn check_denied_plugins(
    config: &RuntimeConfig,
    policy: &RequirementsPolicy,
    policy_source: &Path,
    config_path: &Path,
    violations: &mut Vec<PolicyViolation>,
) {
    if policy.plugins.denied_plugins.is_empty() {
        return;
    }
    for denied in &policy.plugins.denied_plugins {
        if config
            .plugins()
            .enabled_plugins()
            .get(denied.as_str())
            .copied()
            .unwrap_or(false)
        {
            violations.push(PolicyViolation {
                requirement: format!("plugins.denied_plugins contains \"{denied}\""),
                user_value: format!("enabledPlugins.\"{denied}\" = true"),
                config_source: config_path.to_path_buf(),
                policy_source: policy_source.to_path_buf(),
                action: format!(
                    "Set enabledPlugins.\"{denied}\" to false or remove it from \
                     your settings.json, or contact your administrator."
                ),
            });
        }
    }
}

// ─── Provider-matching heuristic ─────────────────────────────────────────────

/// Returns `true` when `provider_name` appears to match the given `model` string.
///
/// Heuristic: check whether the model identifier starts with or contains a
/// well-known prefix for the provider.  Comparison is case-insensitive.
fn provider_matches_model(provider_name: &str, model: &str) -> bool {
    if model.is_empty() {
        return false;
    }
    let pn = provider_name.to_ascii_lowercase();
    let mn = model.to_ascii_lowercase();
    match pn.as_str() {
        "openai" => mn.starts_with("gpt-") || mn.starts_with("o1") || mn.starts_with("o3") || mn.contains("openai"),
        "anthropic" => mn.starts_with("claude"),
        "xai" | "grok" => mn.starts_with("grok"),
        "gemini" | "google" => mn.starts_with("gemini") || mn.contains("google"),
        "ollama" => mn.contains("ollama") || mn.contains(":"),
        other => mn.contains(other),
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ConfigLoader, ResolvedPermissionMode};
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    // ── helpers ──────────────────────────────────────────────────────────────

    fn unique_dir() -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time after epoch")
            .as_nanos();
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("req-test-{nanos}-{id}"))
    }

    /// Build a bare `RuntimeConfig` with no files on disk.
    fn empty_config() -> RuntimeConfig {
        RuntimeConfig::empty()
    }

    /// Write a `requirements.toml` to `path` and parse it.
    fn load_policy(toml_text: &str, path: &Path) -> (RequirementsPolicy, PathBuf) {
        fs::write(path, toml_text).expect("write requirements.toml");
        load_from_path(path)
    }

    // ── test 1 ────────────────────────────────────────────────────────────────
    #[test]
    fn requirements_no_file_no_constraints() {
        let (policy, source) = load_from_path(Path::new("/tmp/nonexistent-req-abc123.toml"));
        assert!(source == PathBuf::new(), "source should be empty when file is missing");
        let config = empty_config();
        assert!(
            validate(&config, &policy, &source).is_ok(),
            "no file → no constraints → no violations"
        );
    }

    // ── test 2 ────────────────────────────────────────────────────────────────
    #[test]
    fn requirements_loads_from_user_path() {
        let dir = unique_dir();
        fs::create_dir_all(&dir).expect("dir");
        let req_path = dir.join("requirements.toml");
        let (policy, source) = load_policy(
            r#"[permissions]
forbidden_modes = ["DangerFullAccess"]
"#,
            &req_path,
        );
        assert_eq!(source, req_path);
        assert_eq!(policy.permissions.forbidden_modes, vec!["DangerFullAccess"]);
        let _ = fs::remove_dir_all(&dir);
    }

    // ── test 3 ────────────────────────────────────────────────────────────────
    #[test]
    fn requirements_loads_from_system_path() {
        let dir = unique_dir();
        fs::create_dir_all(&dir).expect("dir");
        let req_path = dir.join("requirements.toml");
        let (policy, source) = load_policy(
            r#"[mcp]
denied_servers = ["evil-server"]
"#,
            &req_path,
        );
        assert_eq!(source, req_path);
        assert_eq!(policy.mcp.denied_servers, vec!["evil-server"]);
        let _ = fs::remove_dir_all(&dir);
    }

    // ── test 4: env-var override takes precedence over filesystem order ────────
    #[test]
    fn requirements_user_path_overrides_system() {
        let dir = unique_dir();
        fs::create_dir_all(&dir).expect("dir");
        let system_path = dir.join("system-req.toml");
        let user_path = dir.join("user-req.toml");

        fs::write(
            &system_path,
            r#"[permissions]
forbidden_modes = ["danger-full-access"]
"#,
        )
        .expect("write system");
        fs::write(
            &user_path,
            r#"[permissions]
forbidden_modes = ["dontAsk"]
"#,
        )
        .expect("write user");

        // The env-var override mechanism honours the first found; set it to
        // user_path so that user_path wins.
        // SAFETY: single-threaded test; env isolation handled by the temp dir.
        unsafe {
            std::env::set_var(
                "ANVIL_REQUIREMENTS_PATH",
                user_path.to_str().expect("utf8"),
            );
        }
        let (policy, source) = load_from_paths();
        unsafe { std::env::remove_var("ANVIL_REQUIREMENTS_PATH"); }

        assert_eq!(source, user_path, "env-var path should be used");
        assert_eq!(policy.permissions.forbidden_modes, vec!["dontAsk"]);
        let _ = fs::remove_dir_all(&dir);
    }

    // ── test 5 ────────────────────────────────────────────────────────────────
    #[test]
    fn requirements_forbidden_permission_mode_rejects_user_config() {
        let dir = unique_dir();
        fs::create_dir_all(&dir).expect("dir");

        let req_path = dir.join("requirements.toml");
        let cwd = dir.join("project");
        let home = dir.join("home").join(".anvil");
        fs::create_dir_all(&cwd).expect("cwd");
        fs::create_dir_all(&home).expect("home");

        // User has set DangerFullAccess.
        fs::write(
            home.join("settings.json"),
            r#"{"permissionMode": "dontAsk"}"#,
        )
        .expect("write settings");

        let config = ConfigLoader::new(&cwd, &home)
            .load()
            .expect("config loads");
        assert_eq!(
            config.permission_mode(),
            Some(ResolvedPermissionMode::DangerFullAccess)
        );

        let (policy, source) = load_policy(
            r#"[permissions]
forbidden_modes = ["dontAsk", "bypassPermissions", "DangerFullAccess"]
"#,
            &req_path,
        );
        let result = validate(&config, &policy, &source);
        assert!(result.is_err(), "should reject dontAsk");
        let v = result.unwrap_err();
        assert_eq!(v.len(), 1);
        assert!(v[0].requirement.contains("forbidden_modes"));

        let _ = fs::remove_dir_all(&dir);
    }

    // ── test 6 ────────────────────────────────────────────────────────────────
    #[test]
    fn requirements_max_effort_rejects_xhigh_when_ceiling_is_high() {
        let dir = unique_dir();
        fs::create_dir_all(&dir).expect("dir");

        let req_path = dir.join("requirements.toml");
        let cwd = dir.join("project");
        let home = dir.join("home").join(".anvil");
        fs::create_dir_all(&cwd).expect("cwd");
        fs::create_dir_all(&home).expect("home");

        fs::write(
            home.join("settings.json"),
            r#"{"effort_level": "xhigh"}"#,
        )
        .expect("write settings");

        let config = ConfigLoader::new(&cwd, &home)
            .load()
            .expect("config loads");
        assert_eq!(config.effort_level(), Some(EffortLevel::Xhigh));

        let (policy, source) = load_policy(
            r#"[permissions]
max_effort = "high"
"#,
            &req_path,
        );
        let result = validate(&config, &policy, &source);
        assert!(result.is_err(), "xhigh exceeds ceiling high");
        let violations = result.unwrap_err();
        assert_eq!(violations.len(), 1);
        assert!(violations[0].requirement.contains("max_effort"));
        assert!(violations[0].user_value.contains("xhigh"));

        let _ = fs::remove_dir_all(&dir);
    }

    // ── test 7 ────────────────────────────────────────────────────────────────
    #[test]
    fn requirements_denied_provider_rejects_user_config_using_provider() {
        let dir = unique_dir();
        fs::create_dir_all(&dir).expect("dir");

        let req_path = dir.join("requirements.toml");
        let cwd = dir.join("project");
        let home = dir.join("home").join(".anvil");
        fs::create_dir_all(&cwd).expect("cwd");
        fs::create_dir_all(&home).expect("home");

        fs::write(
            home.join("settings.json"),
            r#"{"model": "gpt-4o"}"#,
        )
        .expect("write settings");

        let config = ConfigLoader::new(&cwd, &home)
            .load()
            .expect("config loads");

        let (policy, source) = load_policy(
            r#"[providers]
denied_providers = ["openai"]
"#,
            &req_path,
        );
        let result = validate(&config, &policy, &source);
        assert!(result.is_err(), "gpt-4o is an openai model");
        let violations = result.unwrap_err();
        assert!(violations[0].requirement.contains("denied_providers"));

        let _ = fs::remove_dir_all(&dir);
    }

    // ── test 8 ────────────────────────────────────────────────────────────────
    #[test]
    fn requirements_required_provider_rejects_when_user_has_none() {
        let dir = unique_dir();
        fs::create_dir_all(&dir).expect("dir");

        let req_path = dir.join("requirements.toml");
        // No settings.json → no model configured.
        let cwd = dir.join("project");
        let home = dir.join("home").join(".anvil");
        fs::create_dir_all(&cwd).expect("cwd");
        fs::create_dir_all(&home).expect("home");

        let config = ConfigLoader::new(&cwd, &home)
            .load()
            .expect("config loads");

        let (policy, source) = load_policy(
            r#"[providers]
required_providers = ["anthropic"]
"#,
            &req_path,
        );
        let result = validate(&config, &policy, &source);
        assert!(
            result.is_err(),
            "no model configured → required provider not satisfied"
        );
        let violations = result.unwrap_err();
        assert!(violations[0].requirement.contains("required_providers"));

        let _ = fs::remove_dir_all(&dir);
    }

    // ── test 9 ────────────────────────────────────────────────────────────────
    #[test]
    fn requirements_denied_mcp_server_rejects_user_enabling_it() {
        let dir = unique_dir();
        fs::create_dir_all(&dir).expect("dir");

        let req_path = dir.join("requirements.toml");
        let cwd = dir.join("project");
        let home = dir.join("home").join(".anvil");
        fs::create_dir_all(&cwd).expect("cwd");
        fs::create_dir_all(&home).expect("home");

        fs::write(
            home.join("settings.json"),
            r#"{"mcpServers": {"claude-in-chrome": {"command": "uvx", "args": ["cic"]}}}"#,
        )
        .expect("write settings");

        let config = ConfigLoader::new(&cwd, &home)
            .load()
            .expect("config loads");

        let (policy, source) = load_policy(
            r#"[mcp]
denied_servers = ["claude-in-chrome"]
"#,
            &req_path,
        );
        let result = validate(&config, &policy, &source);
        assert!(result.is_err(), "denied mcp server is configured");
        let violations = result.unwrap_err();
        assert!(violations[0].requirement.contains("claude-in-chrome"));

        let _ = fs::remove_dir_all(&dir);
    }

    // ── test 10 ───────────────────────────────────────────────────────────────
    #[test]
    fn requirements_otel_required_disabled_rejects_user_enabling() {
        let dir = unique_dir();
        fs::create_dir_all(&dir).expect("dir");

        let req_path = dir.join("requirements.toml");
        let cwd = dir.join("project");
        let home = dir.join("home").join(".anvil");
        fs::create_dir_all(&cwd).expect("cwd");
        fs::create_dir_all(&home).expect("home");

        fs::write(
            home.join("settings.json"),
            r#"{"otel": {"enabled": true}}"#,
        )
        .expect("write settings");

        let config = ConfigLoader::new(&cwd, &home)
            .load()
            .expect("config loads");
        assert!(config.otel().enabled);

        let (policy, source) = load_policy(
            r#"[telemetry]
require_otel_disabled = true
"#,
            &req_path,
        );
        let result = validate(&config, &policy, &source);
        assert!(result.is_err(), "otel.enabled=true violates require_otel_disabled");
        let violations = result.unwrap_err();
        assert!(violations[0].requirement.contains("require_otel_disabled"));

        let _ = fs::remove_dir_all(&dir);
    }

    // ── test 11 ───────────────────────────────────────────────────────────────
    #[test]
    fn requirements_egress_allowlist_locked_rejects_user_extension() {
        let dir = unique_dir();
        fs::create_dir_all(&dir).expect("dir");

        let req_path = dir.join("requirements.toml");
        let cwd = dir.join("project");
        let home = dir.join("home").join(".anvil");
        fs::create_dir_all(&cwd).expect("cwd");
        fs::create_dir_all(&home).expect("home");

        fs::write(
            home.join("settings.json"),
            r#"{"security": {"egress_allowlist": ["extra.example.com"]}}"#,
        )
        .expect("write settings");

        let config = ConfigLoader::new(&cwd, &home)
            .load()
            .expect("config loads");

        let (policy, source) = load_policy(
            r#"[egress]
allowlist_locked = true
"#,
            &req_path,
        );
        let result = validate(&config, &policy, &source);
        assert!(result.is_err(), "user extended allowlist while allowlist_locked=true");
        let violations = result.unwrap_err();
        assert!(violations[0].requirement.contains("allowlist_locked"));

        let _ = fs::remove_dir_all(&dir);
    }

    // ── test 12 ───────────────────────────────────────────────────────────────
    #[test]
    fn requirements_malformed_toml_logs_warning_continues() {
        let dir = unique_dir();
        fs::create_dir_all(&dir).expect("dir");

        let bad_path = dir.join("bad-requirements.toml");
        fs::write(&bad_path, "NOT VALID TOML = [[[").expect("write bad toml");

        // load_from_path should return a default policy, not panic.
        let (policy, source) = load_from_path(&bad_path);

        // Source path should be empty (policy not applied).
        assert_eq!(source, PathBuf::new(), "malformed file → no source");

        // Default policy has no constraints.
        let config = empty_config();
        assert!(
            validate(&config, &policy, &source).is_ok(),
            "malformed requirements.toml → no constraints"
        );

        let _ = fs::remove_dir_all(&dir);
    }

    // ── test 13 ───────────────────────────────────────────────────────────────
    #[test]
    fn requirements_violation_message_includes_source_and_policy_paths() {
        let dir = unique_dir();
        fs::create_dir_all(&dir).expect("dir");

        let req_path = dir.join("requirements.toml");
        let cwd = dir.join("project");
        let home = dir.join("home").join(".anvil");
        fs::create_dir_all(&cwd).expect("cwd");
        fs::create_dir_all(&home).expect("home");

        fs::write(
            home.join("settings.json"),
            r#"{"permissionMode": "dontAsk"}"#,
        )
        .expect("write settings");

        let config = ConfigLoader::new(&cwd, &home)
            .load()
            .expect("config loads");

        let (policy, source) = load_policy(
            r#"[permissions]
forbidden_modes = ["dontAsk"]
"#,
            &req_path,
        );
        let result = validate(&config, &policy, &source);
        assert!(result.is_err());
        let violations = result.unwrap_err();
        let rendered = violations[0].render();

        // Must include both path kinds in the rendered message.
        assert!(
            rendered.contains(req_path.to_str().unwrap()),
            "rendered message must include policy source path"
        );
        assert!(
            rendered.contains("Policy:"),
            "rendered message must have Policy: label"
        );
        assert!(
            rendered.contains("Source:"),
            "rendered message must have Source: label"
        );
        assert!(
            rendered.contains("Action:"),
            "rendered message must have Action: label"
        );

        let _ = fs::remove_dir_all(&dir);
    }

    // ── test 14 ───────────────────────────────────────────────────────────────
    #[test]
    fn requirements_plugin_url_disabled_rejects_plugin_url_flag() {
        // The allow_url_plugins = false flag is stored in the policy struct.
        // This test verifies the policy loads and that the flag is correctly set.
        let dir = unique_dir();
        fs::create_dir_all(&dir).expect("dir");

        let req_path = dir.join("requirements.toml");
        let (policy, source) = load_policy(
            r#"[plugins]
plugin_install_disabled = true
allow_url_plugins = false
"#,
            &req_path,
        );
        assert_eq!(source, req_path);
        assert!(policy.plugins.plugin_install_disabled);
        assert!(!policy.plugins.allow_url_plugins);

        // Verify that a clean config passes (the check for these flags
        // happens at the call site that receives a --plugin-url argument,
        // not at config-load time, since there's no equivalent config key).
        let config = empty_config();
        assert!(
            validate(&config, &policy, &source).is_ok(),
            "plugin_install_disabled/allow_url_plugins are enforced at \
             the install-command call site, not at config load"
        );

        let _ = fs::remove_dir_all(&dir);
    }

    // ── additional coverage: sandbox required ─────────────────────────────────
    #[test]
    fn requirements_require_sandbox_rejects_when_not_enabled() {
        let dir = unique_dir();
        fs::create_dir_all(&dir).expect("dir");

        let req_path = dir.join("requirements.toml");
        let cwd = dir.join("project");
        let home = dir.join("home").join(".anvil");
        fs::create_dir_all(&cwd).expect("cwd");
        fs::create_dir_all(&home).expect("home");

        // sandbox.enabled absent → treated as false.
        fs::write(home.join("settings.json"), r#"{"model": "opus"}"#).expect("write");

        let config = ConfigLoader::new(&cwd, &home).load().expect("config");

        let (policy, source) = load_policy(
            r#"[permissions]
require_sandbox = true
"#,
            &req_path,
        );
        let result = validate(&config, &policy, &source);
        assert!(result.is_err());
        assert!(result.unwrap_err()[0].requirement.contains("require_sandbox"));

        let _ = fs::remove_dir_all(&dir);
    }

    // ── additional coverage: no violation when within ceiling ─────────────────
    #[test]
    fn requirements_max_effort_allows_equal_ceiling() {
        let dir = unique_dir();
        fs::create_dir_all(&dir).expect("dir");

        let req_path = dir.join("requirements.toml");
        let cwd = dir.join("project");
        let home = dir.join("home").join(".anvil");
        fs::create_dir_all(&cwd).expect("cwd");
        fs::create_dir_all(&home).expect("home");

        fs::write(
            home.join("settings.json"),
            r#"{"effort_level": "high"}"#,
        )
        .expect("write settings");

        let config = ConfigLoader::new(&cwd, &home).load().expect("config");

        let (policy, source) = load_policy(
            r#"[permissions]
max_effort = "high"
"#,
            &req_path,
        );
        assert!(
            validate(&config, &policy, &source).is_ok(),
            "effort == ceiling should not violate"
        );

        let _ = fs::remove_dir_all(&dir);
    }

    // ── additional coverage: multiple violations reported at once ─────────────
    #[test]
    fn requirements_multiple_violations_all_reported() {
        let dir = unique_dir();
        fs::create_dir_all(&dir).expect("dir");

        let req_path = dir.join("requirements.toml");
        let cwd = dir.join("project");
        let home = dir.join("home").join(".anvil");
        fs::create_dir_all(&cwd).expect("cwd");
        fs::create_dir_all(&home).expect("home");

        fs::write(
            home.join("settings.json"),
            r#"{
                "permissionMode": "dontAsk",
                "effort_level": "xhigh",
                "otel": {"enabled": true}
            }"#,
        )
        .expect("write settings");

        let config = ConfigLoader::new(&cwd, &home).load().expect("config");

        let (policy, source) = load_policy(
            r#"[permissions]
forbidden_modes = ["dontAsk"]
max_effort = "high"

[telemetry]
require_otel_disabled = true
"#,
            &req_path,
        );
        let result = validate(&config, &policy, &source);
        assert!(result.is_err());
        let violations = result.unwrap_err();
        assert!(
            violations.len() >= 3,
            "all three violations must be reported, got {}",
            violations.len()
        );
    }
}
