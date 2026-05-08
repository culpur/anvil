use std::collections::BTreeMap;

use crate::json::JsonValue;

use super::{
    helpers::{optional_string, optional_string_array, optional_string_map},
    output_style::{output_style_from_str_builtin_only, OutputStyle},
    ConfigError, ResolvedPermissionMode,
};
use super::helpers::expect_object;

/// All fields that a named profile can override.  Every field is `Option<T>`;
/// `None` means "fall through to the base config".
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ProfileOverride {
    /// Override the model (e.g. `"claude-opus-4-7"` or `"ollama:llama3.3:70b"`).
    pub model: Option<String>,
    /// Override the provider (e.g. `"anvilApi"`, `"ollama"`, `"openai"`).
    pub provider: Option<String>,
    /// Override the effort level (W2 field; stored as raw string).
    pub effort_level: Option<String>,
    /// Override the output style axis.
    pub output_style: Option<OutputStyle>,
    /// Override the permission mode.
    pub permission_mode: Option<ResolvedPermissionMode>,
    /// User-installed plugin names to enable for this profile.
    /// Additive: bundled plugins are never disabled through this list.
    pub enabled_plugins: Vec<String>,
    /// MCP server names to activate for this profile.
    pub enabled_mcp_servers: Vec<String>,
    /// Extra environment variable remappings for this profile.
    /// The map value is the *name* of the env var whose value should be used.
    /// e.g. `{"GITHUB_TOKEN_ENV_VAR": "WORK_GITHUB_TOKEN"}` means: when this
    /// profile is active, read `$WORK_GITHUB_TOKEN` and expose it as
    /// `$GITHUB_TOKEN_ENV_VAR`.
    pub env: BTreeMap<String, String>,
}

impl ProfileOverride {
    /// Apply this override on top of `base_model`, returning the effective model.
    #[must_use]
    pub fn effective_model<'a>(&'a self, base: Option<&'a str>) -> Option<&'a str> {
        self.model.as_deref().or(base)
    }

    /// Apply this override on top of `base_output_style`.
    #[must_use]
    pub fn effective_output_style(&self, base: OutputStyle) -> OutputStyle {
        self.output_style.clone().unwrap_or(base)
    }

    /// Apply this override on top of `base_permission_mode`.
    #[must_use]
    pub fn effective_permission_mode(
        &self,
        base: Option<ResolvedPermissionMode>,
    ) -> Option<ResolvedPermissionMode> {
        self.permission_mode.or(base)
    }
}

/// Parse the `profiles` section of the merged config.
///
/// Returns an empty map when the key is absent (no-profiles is valid).
/// A malformed profile is skipped with a warning rather than aborting the load.
pub fn parse_profiles(root: &JsonValue) -> BTreeMap<String, ProfileOverride> {
    let Some(object) = root.as_object() else {
        return BTreeMap::new();
    };
    let Some(profiles_value) = object.get("profiles") else {
        return BTreeMap::new();
    };
    let Some(profiles_map) = profiles_value.as_object() else {
        eprintln!("anvil: ignoring malformed `profiles` section: expected JSON object");
        return BTreeMap::new();
    };

    let mut result = BTreeMap::new();
    for (name, profile_value) in profiles_map {
        match parse_single_profile(name, profile_value) {
            Ok(profile) => {
                result.insert(name.clone(), profile);
            }
            Err(error) => {
                eprintln!("anvil: ignoring malformed profile `{name}`: {error}");
            }
        }
    }
    result
}

/// Parse `active_profile` from the root config object.
///
/// Returns `None` when the key is absent.
pub fn parse_active_profile(root: &JsonValue) -> Option<String> {
    root.as_object()
        .and_then(|obj| obj.get("active_profile"))
        .and_then(JsonValue::as_str)
        .map(ToOwned::to_owned)
}

fn parse_single_profile(
    name: &str,
    value: &JsonValue,
) -> Result<ProfileOverride, ConfigError> {
    let ctx = format!("profiles.{name}");
    let object = expect_object(value, &ctx)?;

    let model = optional_string(object, "model", &ctx)?.map(ToOwned::to_owned);
    let provider = optional_string(object, "provider", &ctx)?.map(ToOwned::to_owned);
    let effort_level = optional_string(object, "effort_level", &ctx)?.map(ToOwned::to_owned);

    let output_style = optional_string(object, "output_style", &ctx)?
        .map(|s| output_style_from_str_builtin_only(s));

    let permission_mode = if let Some(mode_str) = optional_string(object, "permission_mode", &ctx)? {
        Some(parse_permission_mode_label(mode_str, &format!("{ctx}.permission_mode"))?)
    } else {
        None
    };

    let enabled_plugins = optional_string_array(object, "enabled_plugins", &ctx)?
        .unwrap_or_default();

    let enabled_mcp_servers =
        optional_string_array(object, "enabled_mcp_servers", &ctx)?.unwrap_or_default();

    let env = optional_string_map(object, "env", &ctx)?.unwrap_or_default();

    Ok(ProfileOverride {
        model,
        provider,
        effort_level,
        output_style,
        permission_mode,
        enabled_plugins,
        enabled_mcp_servers,
        env,
    })
}

fn parse_permission_mode_label(
    mode: &str,
    context: &str,
) -> Result<ResolvedPermissionMode, ConfigError> {
    match mode {
        "default" | "plan" | "read-only" => Ok(ResolvedPermissionMode::ReadOnly),
        "acceptEdits" | "auto" | "workspace-write" => Ok(ResolvedPermissionMode::WorkspaceWrite),
        "dontAsk" | "danger-full-access" => Ok(ResolvedPermissionMode::DangerFullAccess),
        other => Err(ConfigError::Parse(format!(
            "{context}: unsupported permission mode {other}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::json::JsonValue;

    fn json(s: &str) -> JsonValue {
        JsonValue::parse(s).expect("test json")
    }

    // ── parse_profiles ────────────────────────────────────────────────────────

    #[test]
    fn profiles_absent_returns_empty_map() {
        let root = json(r#"{"model":"opus"}"#);
        let profiles = parse_profiles(&root);
        assert!(profiles.is_empty());
    }

    #[test]
    fn profiles_not_object_is_skipped_with_warning() {
        let root = json(r#"{"profiles":"oops"}"#);
        let profiles = parse_profiles(&root);
        assert!(profiles.is_empty());
    }

    #[test]
    fn parses_full_work_profile() {
        let root = json(r#"{
            "profiles": {
                "work": {
                    "model": "claude-opus-4-7",
                    "provider": "anvilApi",
                    "effort_level": "high",
                    "output_style": "precise",
                    "permission_mode": "default",
                    "enabled_plugins": ["security-audit"],
                    "enabled_mcp_servers": ["culpur-infra", "qmd"],
                    "env": { "GITHUB_TOKEN_ENV_VAR": "WORK_GITHUB_TOKEN" }
                }
            }
        }"#);

        let profiles = parse_profiles(&root);
        let work = profiles.get("work").expect("work profile");

        assert_eq!(work.model.as_deref(), Some("claude-opus-4-7"));
        assert_eq!(work.provider.as_deref(), Some("anvilApi"));
        assert_eq!(work.effort_level.as_deref(), Some("high"));
        assert_eq!(work.output_style, Some(OutputStyle::BuiltIn(crate::config::output_style::BuiltInStyle::Precise)));
        assert_eq!(work.permission_mode, Some(ResolvedPermissionMode::ReadOnly));
        assert_eq!(work.enabled_plugins, vec!["security-audit"]);
        assert_eq!(
            work.enabled_mcp_servers,
            vec!["culpur-infra", "qmd"]
        );
        assert_eq!(
            work.env.get("GITHUB_TOKEN_ENV_VAR").map(String::as_str),
            Some("WORK_GITHUB_TOKEN")
        );
    }

    #[test]
    fn parses_personal_profile_with_condensed_style() {
        let root = json(r#"{
            "profiles": {
                "personal": {
                    "model": "ollama:llama3.3:70b",
                    "provider": "ollama",
                    "effort_level": "medium",
                    "permission_mode": "acceptEdits",
                    "enabled_plugins": [],
                    "enabled_mcp_servers": []
                }
            }
        }"#);

        let profiles = parse_profiles(&root);
        let personal = profiles.get("personal").expect("personal profile");

        assert_eq!(personal.model.as_deref(), Some("ollama:llama3.3:70b"));
        assert_eq!(
            personal.permission_mode,
            Some(ResolvedPermissionMode::WorkspaceWrite)
        );
        assert!(personal.enabled_plugins.is_empty());
        assert!(personal.enabled_mcp_servers.is_empty());
        assert!(personal.output_style.is_none(), "unset output_style must fall through");
    }

    #[test]
    fn malformed_profile_skipped_but_sibling_survives() {
        let root = json(r#"{
            "profiles": {
                "broken": { "permission_mode": 999 },
                "good": { "model": "opus" }
            }
        }"#);
        let profiles = parse_profiles(&root);
        // "broken" should be skipped (integer where string expected)
        // but "good" must still parse
        assert!(profiles.get("broken").is_none(), "broken profile should be skipped");
        let good = profiles.get("good").expect("good profile must survive");
        assert_eq!(good.model.as_deref(), Some("opus"));
    }

    // ── parse_active_profile ──────────────────────────────────────────────────

    #[test]
    fn active_profile_absent_returns_none() {
        let root = json(r#"{"model":"opus"}"#);
        assert_eq!(parse_active_profile(&root), None);
    }

    #[test]
    fn active_profile_parsed() {
        let root = json(r#"{"active_profile":"work"}"#);
        assert_eq!(parse_active_profile(&root), Some("work".to_string()));
    }

    // ── ProfileOverride composition ───────────────────────────────────────────

    #[test]
    fn profile_model_overrides_base() {
        let profile = ProfileOverride {
            model: Some("claude-opus-4-7".to_string()),
            ..Default::default()
        };
        assert_eq!(profile.effective_model(Some("sonnet")), Some("claude-opus-4-7"));
    }

    #[test]
    fn profile_model_falls_through_when_unset() {
        let profile = ProfileOverride::default();
        assert_eq!(profile.effective_model(Some("sonnet")), Some("sonnet"));
    }

    #[test]
    fn profile_output_style_overrides_base() {
        let profile = ProfileOverride {
            output_style: Some(OutputStyle::BuiltIn(crate::config::output_style::BuiltInStyle::Condensed)),
            ..Default::default()
        };
        assert_eq!(
            profile.effective_output_style(OutputStyle::BuiltIn(crate::config::output_style::BuiltInStyle::Precise)),
            OutputStyle::BuiltIn(crate::config::output_style::BuiltInStyle::Condensed)
        );
    }

    #[test]
    fn profile_output_style_falls_through_to_base() {
        let profile = ProfileOverride::default();
        assert_eq!(
            profile.effective_output_style(OutputStyle::BuiltIn(crate::config::output_style::BuiltInStyle::Condensed)),
            OutputStyle::BuiltIn(crate::config::output_style::BuiltInStyle::Condensed)
        );
    }

    #[test]
    fn profile_permission_mode_overrides_base() {
        let profile = ProfileOverride {
            permission_mode: Some(ResolvedPermissionMode::WorkspaceWrite),
            ..Default::default()
        };
        assert_eq!(
            profile.effective_permission_mode(Some(ResolvedPermissionMode::ReadOnly)),
            Some(ResolvedPermissionMode::WorkspaceWrite)
        );
    }

    #[test]
    fn profile_permission_mode_falls_through_to_base() {
        let profile = ProfileOverride::default();
        assert_eq!(
            profile.effective_permission_mode(Some(ResolvedPermissionMode::ReadOnly)),
            Some(ResolvedPermissionMode::ReadOnly)
        );
    }

    /// Cross-workstream composition test: effort_level + output_style +
    /// permission_mode all override in the same profile.
    #[test]
    fn profile_composition_covers_all_override_axes() {
        let root = json(r#"{
            "model": "sonnet",
            "output_style": "precise",
            "profiles": {
                "ci": {
                    "model": "haiku",
                    "effort_level": "low",
                    "output_style": "condensed",
                    "permission_mode": "acceptEdits"
                }
            }
        }"#);
        let profiles = parse_profiles(&root);
        let ci = profiles.get("ci").expect("ci profile");

        // effort_level (W2)
        assert_eq!(ci.effort_level.as_deref(), Some("low"));
        // output_style (W7)
        assert_eq!(ci.output_style, Some(OutputStyle::BuiltIn(crate::config::output_style::BuiltInStyle::Condensed)));
        // permission_mode
        assert_eq!(ci.permission_mode, Some(ResolvedPermissionMode::WorkspaceWrite));

        // Base-config fallthrough when profile field is unset
        let base_output_style = OutputStyle::BuiltIn(crate::config::output_style::BuiltInStyle::Precise);
        let ci_no_style = ProfileOverride {
            effort_level: Some("low".to_string()),
            permission_mode: Some(ResolvedPermissionMode::WorkspaceWrite),
            ..Default::default()
        };
        assert_eq!(
            ci_no_style.effective_output_style(base_output_style),
            OutputStyle::BuiltIn(crate::config::output_style::BuiltInStyle::Precise),
            "unset output_style in profile must fall through to base"
        );
    }
}
