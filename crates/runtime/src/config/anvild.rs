//! Configuration schema for the `anvild` background service.
//!
//! Written to / read from `~/.anvil/config.json` under the `"anvild"` key.

use crate::json::JsonValue;

/// How the TUI should treat the `anvild` background service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AnvildAutostart {
    /// Start/ensure the daemon is running on every TUI launch.
    Yes,
    /// Never start the daemon; use the in-TUI keepalive fallback.
    No,
    /// Ask on the next TUI launch (prompt not yet answered).
    Ask,
}

impl Default for AnvildAutostart {
    fn default() -> Self {
        Self::Ask
    }
}

/// Top-level config block written under `"anvild"` in `config.json`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnvildConfig {
    /// Whether to autostart the daemon.  Default: `Ask`.
    pub autostart: AnvildAutostart,
    /// Whether the platform service unit (LaunchAgent / systemd) has been installed.
    pub install_service: bool,
}

impl Default for AnvildConfig {
    fn default() -> Self {
        Self {
            autostart: AnvildAutostart::Ask,
            install_service: false,
        }
    }
}

/// Parse the `"anvild"` block from the merged config JSON.
///
/// Lenient on read: missing fields fall back to their defaults.
/// An entirely absent `"anvild"` key returns `AnvildConfig::default()`.
pub fn parse_optional_anvild_config(root: &JsonValue) -> AnvildConfig {
    let Some(obj) = root
        .as_object()
        .and_then(|o| o.get("anvild"))
        .and_then(JsonValue::as_object)
    else {
        return AnvildConfig::default();
    };

    let autostart = obj
        .get("autostart")
        .and_then(JsonValue::as_str)
        .map(|s| match s {
            "yes" => AnvildAutostart::Yes,
            "no" => AnvildAutostart::No,
            _ => AnvildAutostart::Ask,
        })
        .unwrap_or_default();

    let install_service = obj
        .get("install_service")
        .or_else(|| obj.get("installService"))
        .and_then(JsonValue::as_bool)
        .unwrap_or(false);

    AnvildConfig {
        autostart,
        install_service,
    }
}

/// Serialize `AnvildConfig` back to a `serde_json::Value` suitable for
/// merging into `config.json`.
#[must_use]
pub fn anvild_config_to_json(config: &AnvildConfig) -> serde_json::Value {
    serde_json::json!({
        "anvild": {
            "autostart": match config.autostart {
                AnvildAutostart::Yes => "yes",
                AnvildAutostart::No  => "no",
                AnvildAutostart::Ask => "ask",
            },
            "install_service": config.install_service,
        }
    })
}

/// Load the `AnvildConfig` directly from `config.json` on disk.
///
/// Returns `AnvildConfig::default()` when the file is absent or malformed.
pub fn load_anvild_config(config_path: &std::path::Path) -> AnvildConfig {
    let Ok(text) = std::fs::read_to_string(config_path) else {
        return AnvildConfig::default();
    };
    let Ok(val) = JsonValue::parse(&text) else {
        return AnvildConfig::default();
    };
    parse_optional_anvild_config(&val)
}

/// Persist updated `AnvildConfig` fields into `config.json`.
///
/// Only the `"anvild"` key is updated; all other keys are preserved.
/// A missing file is created from scratch.
pub fn save_anvild_config(
    config_path: &std::path::Path,
    cfg: &AnvildConfig,
) -> std::io::Result<()> {
    let mut root: serde_json::Map<String, serde_json::Value> = if config_path.exists() {
        let text = std::fs::read_to_string(config_path)?;
        serde_json::from_str(&text)
            .ok()
            .and_then(|v: serde_json::Value| v.into_object())
            .unwrap_or_default()
    } else {
        serde_json::Map::new()
    };

    let anvild_val = serde_json::json!({
        "autostart": match cfg.autostart {
            AnvildAutostart::Yes => "yes",
            AnvildAutostart::No  => "no",
            AnvildAutostart::Ask => "ask",
        },
        "install_service": cfg.install_service,
    });

    root.insert("anvild".to_string(), anvild_val);

    if let Some(parent) = config_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let rendered = serde_json::to_string_pretty(&serde_json::Value::Object(root))
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let tmp = config_path.with_extension("json.tmp");
    std::fs::write(&tmp, format!("{rendered}\n"))?;
    std::fs::rename(tmp, config_path)?;
    Ok(())
}

// ── Extension trait to convert Value → Object ──────────────────────────────

trait IntoObject {
    fn into_object(self) -> Option<serde_json::Map<String, serde_json::Value>>;
}

impl IntoObject for serde_json::Value {
    fn into_object(self) -> Option<serde_json::Map<String, serde_json::Value>> {
        match self {
            serde_json::Value::Object(m) => Some(m),
            _ => None,
        }
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn parse(json: &str) -> AnvildConfig {
        let val = JsonValue::parse(json).expect("valid json");
        parse_optional_anvild_config(&val)
    }

    #[test]
    fn defaults_when_key_absent() {
        let cfg = parse("{}");
        assert_eq!(cfg.autostart, AnvildAutostart::Ask);
        assert!(!cfg.install_service);
    }

    #[test]
    fn parses_yes_autostart() {
        let cfg = parse(r#"{"anvild": {"autostart": "yes"}}"#);
        assert_eq!(cfg.autostart, AnvildAutostart::Yes);
    }

    #[test]
    fn parses_no_autostart() {
        let cfg = parse(r#"{"anvild": {"autostart": "no"}}"#);
        assert_eq!(cfg.autostart, AnvildAutostart::No);
    }

    #[test]
    fn parses_ask_autostart() {
        let cfg = parse(r#"{"anvild": {"autostart": "ask"}}"#);
        assert_eq!(cfg.autostart, AnvildAutostart::Ask);
    }

    #[test]
    fn unknown_autostart_value_defaults_to_ask() {
        let cfg = parse(r#"{"anvild": {"autostart": "maybe"}}"#);
        assert_eq!(cfg.autostart, AnvildAutostart::Ask);
    }

    #[test]
    fn parses_install_service_true() {
        let cfg = parse(r#"{"anvild": {"autostart": "yes", "install_service": true}}"#);
        assert!(cfg.install_service);
    }

    #[test]
    fn round_trip_save_and_load() {
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().join("config.json");

        let original = AnvildConfig {
            autostart: AnvildAutostart::Yes,
            install_service: true,
        };
        save_anvild_config(&path, &original).expect("save");

        let loaded = load_anvild_config(&path);
        assert_eq!(loaded, original);
    }

    #[test]
    fn save_preserves_other_keys() {
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().join("config.json");
        fs::write(&path, r#"{"default_model":"claude-sonnet-4-5"}"#)
            .expect("write base config");

        let cfg = AnvildConfig {
            autostart: AnvildAutostart::No,
            install_service: false,
        };
        save_anvild_config(&path, &cfg).expect("save");

        let text = fs::read_to_string(&path).expect("read back");
        assert!(
            text.contains("default_model"),
            "other keys should be preserved"
        );
        assert!(text.contains("anvild"));
    }

    #[test]
    fn load_returns_default_when_file_absent() {
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().join("nonexistent.json");
        let cfg = load_anvild_config(&path);
        assert_eq!(cfg, AnvildConfig::default());
    }
}
