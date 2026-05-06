use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use crate::json::JsonValue;

use super::ConfigError;

pub fn read_optional_json_object(
    path: &Path,
) -> Result<Option<BTreeMap<String, JsonValue>>, ConfigError> {
    let is_legacy_config = path.file_name().and_then(|name| name.to_str()) == Some(".anvil.json");
    let contents = match fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(ConfigError::Io(error)),
    };

    if contents.trim().is_empty() {
        return Ok(Some(BTreeMap::new()));
    }

    // Match the legacy `.anvil.json` branch: a JSON parse error in a
    // settings file should not nuke every other settings file that
    // loaded successfully.  Warn to stderr and skip the broken file
    // rather than returning an error that aborts the whole load.
    let parsed = match JsonValue::parse(&contents) {
        Ok(parsed) => parsed,
        Err(error) => {
            eprintln!(
                "anvil: ignoring malformed settings file {}: {error}",
                path.display()
            );
            return Ok(None);
        }
    };
    let Some(object) = parsed.as_object() else {
        if is_legacy_config {
            return Ok(None);
        }
        return Err(ConfigError::Parse(format!(
            "{}: top-level settings value must be a JSON object",
            path.display()
        )));
    };
    Ok(Some(object.clone()))
}

pub fn deep_merge_objects(
    target: &mut BTreeMap<String, JsonValue>,
    source: &BTreeMap<String, JsonValue>,
) {
    for (key, value) in source {
        match (target.get_mut(key), value) {
            (Some(JsonValue::Object(existing)), JsonValue::Object(incoming)) => {
                deep_merge_objects(existing, incoming);
            }
            _ => {
                target.insert(key.clone(), value.clone());
            }
        }
    }
}

pub fn expect_object<'a>(
    value: &'a JsonValue,
    context: &str,
) -> Result<&'a BTreeMap<String, JsonValue>, ConfigError> {
    value
        .as_object()
        .ok_or_else(|| ConfigError::Parse(format!("{context}: expected JSON object")))
}

pub fn expect_string<'a>(
    object: &'a BTreeMap<String, JsonValue>,
    key: &str,
    context: &str,
) -> Result<&'a str, ConfigError> {
    object
        .get(key)
        .and_then(JsonValue::as_str)
        .ok_or_else(|| ConfigError::Parse(format!("{context}: missing string field {key}")))
}

pub fn optional_string<'a>(
    object: &'a BTreeMap<String, JsonValue>,
    key: &str,
    context: &str,
) -> Result<Option<&'a str>, ConfigError> {
    match object.get(key) {
        Some(value) => value
            .as_str()
            .map(Some)
            .ok_or_else(|| ConfigError::Parse(format!("{context}: field {key} must be a string"))),
        None => Ok(None),
    }
}

pub fn optional_bool(
    object: &BTreeMap<String, JsonValue>,
    key: &str,
    context: &str,
) -> Result<Option<bool>, ConfigError> {
    match object.get(key) {
        Some(value) => value
            .as_bool()
            .map(Some)
            .ok_or_else(|| ConfigError::Parse(format!("{context}: field {key} must be a boolean"))),
        None => Ok(None),
    }
}

pub fn optional_u16(
    object: &BTreeMap<String, JsonValue>,
    key: &str,
    context: &str,
) -> Result<Option<u16>, ConfigError> {
    match object.get(key) {
        Some(value) => {
            let Some(number) = value.as_i64() else {
                return Err(ConfigError::Parse(format!(
                    "{context}: field {key} must be an integer"
                )));
            };
            let number = u16::try_from(number).map_err(|_| {
                ConfigError::Parse(format!("{context}: field {key} is out of range"))
            })?;
            Ok(Some(number))
        }
        None => Ok(None),
    }
}

pub fn parse_bool_map(value: &JsonValue, context: &str) -> Result<BTreeMap<String, bool>, ConfigError> {
    let Some(map) = value.as_object() else {
        return Err(ConfigError::Parse(format!(
            "{context}: expected JSON object"
        )));
    };
    map.iter()
        .map(|(key, value)| {
            value
                .as_bool()
                .map(|enabled| (key.clone(), enabled))
                .ok_or_else(|| {
                    ConfigError::Parse(format!("{context}: field {key} must be a boolean"))
                })
        })
        .collect()
}

pub fn optional_string_array(
    object: &BTreeMap<String, JsonValue>,
    key: &str,
    context: &str,
) -> Result<Option<Vec<String>>, ConfigError> {
    match object.get(key) {
        Some(value) => {
            let Some(array) = value.as_array() else {
                return Err(ConfigError::Parse(format!(
                    "{context}: field {key} must be an array"
                )));
            };
            array
                .iter()
                .map(|item| {
                    item.as_str().map(ToOwned::to_owned).ok_or_else(|| {
                        ConfigError::Parse(format!(
                            "{context}: field {key} must contain only strings"
                        ))
                    })
                })
                .collect::<Result<Vec<_>, _>>()
                .map(Some)
        }
        None => Ok(None),
    }
}

pub fn optional_string_map(
    object: &BTreeMap<String, JsonValue>,
    key: &str,
    context: &str,
) -> Result<Option<BTreeMap<String, String>>, ConfigError> {
    match object.get(key) {
        Some(value) => {
            let Some(map) = value.as_object() else {
                return Err(ConfigError::Parse(format!(
                    "{context}: field {key} must be an object"
                )));
            };
            map.iter()
                .map(|(entry_key, entry_value)| {
                    entry_value
                        .as_str()
                        .map(|text| (entry_key.clone(), text.to_string()))
                        .ok_or_else(|| {
                            ConfigError::Parse(format!(
                                "{context}: field {key} must contain only string values"
                            ))
                        })
                })
                .collect::<Result<BTreeMap<_, _>, _>>()
                .map(Some)
        }
        None => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    /// BUG-34/35 parity: a malformed `settings.json` (e.g. trailing
    /// comma) must not return Err — that would abort every other
    /// settings file in the load chain.  Match the legacy `.anvil.json`
    /// branch and treat it as "skip with warning".
    #[test]
    fn read_optional_json_object_skips_malformed_settings_file() {
        let dir = tempdir().expect("temp dir");
        let path = dir.path().join("settings.json");
        // Garbage that no JSON parser will accept.
        fs::write(&path, "{\"model\": opus garbage").expect("write malformed settings");

        let result = read_optional_json_object(&path)
            .expect("malformed settings.json must not return Err");
        assert!(
            result.is_none(),
            "malformed settings file should be treated as absent, got {result:?}"
        );
    }

    #[test]
    fn read_optional_json_object_returns_object_for_valid_settings() {
        let dir = tempdir().expect("temp dir");
        let path = dir.path().join("settings.json");
        fs::write(&path, r#"{"model":"opus"}"#).expect("write valid settings");

        let result = read_optional_json_object(&path).expect("valid settings should parse");
        let object = result.expect("valid object should be Some");
        assert_eq!(
            object.get("model"),
            Some(&JsonValue::String("opus".to_string()))
        );
    }
}
