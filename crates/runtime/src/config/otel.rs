//! OTel configuration section parsed from `settings.json`.
//!
//! Example:
//! ```json
//! {
//!   "otel": {
//!     "enabled": false,
//!     "exporter": "otlp-http",
//!     "endpoint": "http://localhost:4318",
//!     "headers": { "Authorization": "Bearer ${OTEL_TOKEN}" },
//!     "service_name": "anvil",
//!     "redact_user_prompts": true
//!   }
//! }
//! ```

use std::collections::BTreeMap;

use crate::json::JsonValue;

use super::helpers::{expect_object, optional_bool, optional_string, optional_string_map};
use super::ConfigError;

/// Parsed representation of the `otel` settings block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OtelConfig {
    /// Whether OTel event emission is enabled.  Default: `false`.
    pub enabled: bool,
    /// Exporter type.  Currently only `"otlp-http"` is supported.
    pub exporter: String,
    /// OTLP endpoint URL.
    pub endpoint: String,
    /// HTTP headers forwarded to the exporter (e.g. bearer tokens).
    /// Values may contain `${VAR_NAME}` env-var references.
    pub headers: BTreeMap<String, String>,
    /// `service.name` attribute in every span.
    pub service_name: String,
    /// When `true` (default), strip user-prompt content from all events.
    pub redact_user_prompts: bool,
}

impl Default for OtelConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            exporter: "otlp-http".to_string(),
            endpoint: "http://localhost:4318".to_string(),
            headers: BTreeMap::new(),
            service_name: "anvil".to_string(),
            redact_user_prompts: true,
        }
    }
}

/// Parse the optional `"otel"` block from the merged settings JSON.
///
/// Missing block → `OtelConfig::default()`.
/// Malformed block → `Err` (caller uses `tolerate_section` to fall back).
pub fn parse_optional_otel_config(root: &JsonValue) -> Result<OtelConfig, ConfigError> {
    let Some(object) = root.as_object() else {
        return Ok(OtelConfig::default());
    };
    let Some(otel_value) = object.get("otel") else {
        return Ok(OtelConfig::default());
    };
    let otel = expect_object(otel_value, "merged settings.otel")?;

    let enabled = optional_bool(otel, "enabled", "merged settings.otel")?
        .unwrap_or(false);
    let exporter = optional_string(otel, "exporter", "merged settings.otel")?
        .unwrap_or("otlp-http")
        .to_string();
    let endpoint = optional_string(otel, "endpoint", "merged settings.otel")?
        .unwrap_or("http://localhost:4318")
        .to_string();
    let headers = optional_string_map(otel, "headers", "merged settings.otel")?
        .unwrap_or_default();
    let service_name = optional_string(otel, "service_name", "merged settings.otel")?
        .unwrap_or("anvil")
        .to_string();
    let redact_user_prompts =
        optional_bool(otel, "redact_user_prompts", "merged settings.otel")?
            .unwrap_or(true);

    Ok(OtelConfig {
        enabled,
        exporter,
        endpoint,
        headers,
        service_name,
        redact_user_prompts,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::json::JsonValue;

    #[test]
    fn defaults_when_otel_block_absent() {
        let root = JsonValue::parse(r#"{"model": "opus"}"#).expect("parse");
        let config = parse_optional_otel_config(&root).expect("should parse");
        assert!(!config.enabled);
        assert_eq!(config.exporter, "otlp-http");
        assert_eq!(config.endpoint, "http://localhost:4318");
        assert!(config.redact_user_prompts);
    }

    #[test]
    fn parses_full_otel_block() {
        let root = JsonValue::parse(r#"{
            "otel": {
                "enabled": true,
                "exporter": "otlp-http",
                "endpoint": "http://collector:4318",
                "headers": {"Authorization": "Bearer ${TOKEN}"},
                "service_name": "my-anvil",
                "redact_user_prompts": false
            }
        }"#).expect("parse");

        let config = parse_optional_otel_config(&root).expect("should parse");
        assert!(config.enabled);
        assert_eq!(config.endpoint, "http://collector:4318");
        assert_eq!(
            config.headers.get("Authorization").map(String::as_str),
            Some("Bearer ${TOKEN}")
        );
        assert_eq!(config.service_name, "my-anvil");
        assert!(!config.redact_user_prompts);
    }

    #[test]
    fn partial_otel_block_uses_defaults_for_missing_fields() {
        let root = JsonValue::parse(r#"{"otel": {"enabled": true}}"#).expect("parse");
        let config = parse_optional_otel_config(&root).expect("should parse");
        assert!(config.enabled);
        assert_eq!(config.endpoint, "http://localhost:4318");
        assert!(config.redact_user_prompts);
    }
}
