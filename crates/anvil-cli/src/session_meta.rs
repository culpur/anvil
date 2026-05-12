//! Session metadata sidecar — stores user-set names so `--resume <name>` and
//! the exit-UX message can reference a friendly identifier instead of the
//! timestamp-based session ID.
//!
//! Layout: alongside each `<session-id>.json` we maintain an optional
//! `<session-id>.meta.json` containing:
//!
//! ```json
//! { "name": "auth-refactor", "renamed_at": 1778365293 }
//! ```
//!
//! Sidecar approach (rather than extending the Session struct) keeps the
//! conversation file's schema minimal and lets older anvil versions load
//! sessions transparently. Sessions without a sidecar simply have no name.
//!
//! v2.2.12 T3-J / T3-Exit-UX. See feedback-anvil-exit-resume-ux memory.

use std::fs;
use std::path::{Path, PathBuf};

use crate::session::sessions_dir;

/// Filesystem-safe characters allowed in a session name. Constrained to
/// `[A-Za-z0-9_-]` so a name can also serve as a file-system reference
/// without escape headaches. Length 1..=64 chars.
fn is_valid_name(name: &str) -> bool {
    let len = name.chars().count();
    if !(1..=64).contains(&len) {
        return false;
    }
    name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Path to the metadata sidecar for a given session ID.
fn meta_path_for_id(id: &str) -> Result<PathBuf, Box<dyn std::error::Error>> {
    Ok(sessions_dir()?.join(format!("{id}.meta.json")))
}

/// Set or clear a session's friendly name.
///
/// Writing an empty name removes the sidecar (clears any prior name).
/// The new name MUST be unique among existing sessions in the workspace —
/// returns an error otherwise.
pub(crate) fn set_session_name(
    id: &str,
    new_name: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    if new_name.is_empty() {
        // Clear the sidecar
        let path = meta_path_for_id(id)?;
        if path.exists() {
            fs::remove_file(path)?;
        }
        return Ok(());
    }

    if !is_valid_name(new_name) {
        return Err(format!(
            "session name must be 1..=64 chars of [A-Za-z0-9_-]: {new_name:?}"
        )
        .into());
    }

    // Uniqueness check: refuse if some OTHER session already has this name
    if let Some(existing_id) = resolve_name_to_id(new_name)?
        && existing_id != id
    {
        return Err(format!(
            "session name {new_name:?} already in use by {existing_id}"
        )
        .into());
    }

    let renamed_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let json = format!(
        "{{\"name\":\"{}\",\"renamed_at\":{renamed_at}}}\n",
        new_name.replace('"', "\\\"")
    );
    fs::write(meta_path_for_id(id)?, json)?;
    Ok(())
}

/// Read a session's friendly name (None if no sidecar or no `name` key).
pub(crate) fn get_session_name(id: &str) -> Option<String> {
    let path = meta_path_for_id(id).ok()?;
    if !path.exists() {
        return None;
    }
    let raw = fs::read_to_string(&path).ok()?;
    parse_string_field(&raw, "name")
}

/// Read the saved model identifier (e.g. "ollama:qwen3.5:latest" or
/// "anthropic:claude-sonnet-4-6") that this session was using when last
/// persisted. `--continue` uses this so we don't fall back to DEFAULT_MODEL
/// and accidentally try to talk to Anthropic when the user was on Ollama.
pub(crate) fn get_session_model(id: &str) -> Option<String> {
    let path = meta_path_for_id(id).ok()?;
    if !path.exists() {
        return None;
    }
    let raw = fs::read_to_string(&path).ok()?;
    parse_string_field(&raw, "model")
}

/// Update or set the saved model on the sidecar. Preserves any existing
/// `name` field. No-ops on empty input. Failures are non-fatal — the caller
/// uses this best-effort.
pub(crate) fn set_session_model(
    id: &str,
    model: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    if model.is_empty() {
        return Ok(());
    }
    // Hand-write the JSON so the sidecar stays no-deps. Preserve any existing
    // name + renamed_at so renames aren't blown away when we write the model.
    let path = meta_path_for_id(id)?;
    let existing_name = if path.exists() {
        fs::read_to_string(&path)
            .ok()
            .and_then(|s| parse_string_field(&s, "name"))
    } else {
        None
    };
    let existing_renamed_at = if path.exists() {
        fs::read_to_string(&path)
            .ok()
            .and_then(|s| parse_number_field(&s, "renamed_at"))
    } else {
        None
    };
    let updated_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let mut parts: Vec<String> = Vec::new();
    if let Some(n) = existing_name {
        parts.push(format!("\"name\":\"{}\"", n.replace('"', "\\\"")));
    }
    if let Some(r) = existing_renamed_at {
        parts.push(format!("\"renamed_at\":{r}"));
    }
    parts.push(format!("\"model\":\"{}\"", model.replace('"', "\\\"")));
    parts.push(format!("\"model_updated_at\":{updated_at}"));

    let json = format!("{{{}}}\n", parts.join(","));
    fs::write(&path, json)?;
    Ok(())
}

/// Tiny no-deps JSON sniffer for a single string field. Avoids dragging in
/// runtime's JsonValue here (which would create a cyclic crate dep). Honors
/// a backslash escape for `\"`. Robust enough for the values we ourselves write.
fn parse_string_field(raw: &str, key: &str) -> Option<String> {
    let key_pattern = format!("\"{key}\"");
    let key_idx = raw.find(&key_pattern)?;
    let after_key = &raw[key_idx + key_pattern.len()..];
    let colon_idx = after_key.find(':')?;
    let after_colon = &after_key[colon_idx + 1..];
    let quote_idx = after_colon.find('"')?;
    let val_start = &after_colon[quote_idx + 1..];

    let bytes = val_start.as_bytes();
    let mut i = 0;
    let mut out = String::new();
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'\\' && i + 1 < bytes.len() {
            out.push(bytes[i + 1] as char);
            i += 2;
        } else if b == b'"' {
            return Some(out);
        } else {
            out.push(b as char);
            i += 1;
        }
    }
    None
}

/// Same idea, but for a bare numeric field (`"renamed_at":1234`). Returns
/// None if the key isn't present or its value isn't an unsigned integer.
fn parse_number_field(raw: &str, key: &str) -> Option<u64> {
    let key_pattern = format!("\"{key}\"");
    let key_idx = raw.find(&key_pattern)?;
    let after_key = &raw[key_idx + key_pattern.len()..];
    let colon_idx = after_key.find(':')?;
    let after_colon = &after_key[colon_idx + 1..];
    // Skip whitespace.
    let trimmed = after_colon.trim_start();
    let mut end = 0;
    for ch in trimmed.chars() {
        if ch.is_ascii_digit() {
            end += ch.len_utf8();
        } else {
            break;
        }
    }
    if end == 0 {
        return None;
    }
    trimmed[..end].parse().ok()
}

/// Resolve a name to a session ID by scanning all `*.meta.json` sidecars.
/// Returns `None` if no session in this workspace has the given name.
pub(crate) fn resolve_name_to_id(
    name: &str,
) -> Result<Option<String>, Box<dyn std::error::Error>> {
    let dir = sessions_dir()?;
    if !dir.exists() {
        return Ok(None);
    }
    for entry in fs::read_dir(&dir)? {
        let entry = entry?;
        let path = entry.path();
        let stem = match path.file_name().and_then(|n| n.to_str()) {
            Some(s) if s.ends_with(".meta.json") => s.trim_end_matches(".meta.json").to_string(),
            _ => continue,
        };
        if let Some(meta_name) = get_session_name(&stem)
            && meta_name == name
        {
            return Ok(Some(stem));
        }
    }
    Ok(None)
}

/// Resolve a session reference (path | ID | name) to (id, path).
///
/// Search order:
///   1. Literal filesystem path
///   2. Session ID match (`<sessions_dir>/<reference>.json`)
///   3. Friendly name match (scan sidecars)
pub(crate) fn resolve_reference_extended(
    reference: &str,
) -> Result<(String, PathBuf), Box<dyn std::error::Error>> {
    let direct = Path::new(reference);
    if direct.exists() {
        let id = direct
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or(reference)
            .to_string();
        return Ok((id, direct.to_path_buf()));
    }

    let dir = sessions_dir()?;
    let by_id = dir.join(format!("{reference}.json"));
    if by_id.exists() {
        return Ok((reference.to_string(), by_id));
    }

    if let Some(id) = resolve_name_to_id(reference)? {
        let path = dir.join(format!("{id}.json"));
        if path.exists() {
            return Ok((id, path));
        }
    }

    Err(format!("session not found (tried path, ID, and name): {reference}").into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_name_from_simple_json() {
        let raw = r#"{"name":"auth-refactor","renamed_at":1778365293}"#;
        assert_eq!(
            parse_string_field(raw, "name"),
            Some("auth-refactor".to_string())
        );
    }

    #[test]
    fn parse_name_with_escaped_quote() {
        let raw = r#"{"name":"foo \"bar\"","renamed_at":1}"#;
        assert_eq!(
            parse_string_field(raw, "name"),
            Some("foo \"bar\"".to_string())
        );
    }

    #[test]
    fn parse_name_returns_none_when_missing() {
        let raw = r#"{"renamed_at":1}"#;
        assert_eq!(parse_string_field(raw, "name"), None);
    }

    #[test]
    fn parse_model_from_combined_json() {
        // Sidecar with both name and model (the post-bug-4 shape).
        let raw = r#"{"name":"auth-refactor","renamed_at":100,"model":"ollama:qwen3.5:latest","model_updated_at":200}"#;
        assert_eq!(
            parse_string_field(raw, "model"),
            Some("ollama:qwen3.5:latest".to_string())
        );
        assert_eq!(
            parse_string_field(raw, "name"),
            Some("auth-refactor".to_string())
        );
    }

    #[test]
    fn parse_model_without_name() {
        // Sidecar that has only model (session was never renamed).
        let raw = r#"{"model":"anthropic:claude-sonnet-4-6","model_updated_at":300}"#;
        assert_eq!(
            parse_string_field(raw, "model"),
            Some("anthropic:claude-sonnet-4-6".to_string())
        );
        assert_eq!(parse_string_field(raw, "name"), None);
    }

    #[test]
    fn parse_number_field_works() {
        let raw = r#"{"renamed_at":1778365293,"other":"x"}"#;
        assert_eq!(parse_number_field(raw, "renamed_at"), Some(1778365293));
        assert_eq!(parse_number_field(raw, "missing"), None);
    }

    #[test]
    fn parse_number_field_handles_whitespace() {
        let raw = r#"{"x":   42 }"#;
        assert_eq!(parse_number_field(raw, "x"), Some(42));
    }

    #[test]
    fn name_validation_accepts_normal_names() {
        assert!(is_valid_name("auth-refactor"));
        assert!(is_valid_name("v2_2_12-work"));
        assert!(is_valid_name("a"));
    }

    #[test]
    fn name_validation_rejects_invalid_chars() {
        assert!(!is_valid_name(""));
        assert!(!is_valid_name("auth refactor"));   // space
        assert!(!is_valid_name("path/like"));        // slash
        assert!(!is_valid_name("dotted.name"));      // dot
        assert!(!is_valid_name(&"x".repeat(65)));    // too long
    }

    // CC-136-B5 VERIFY: --resume/--continue must work with names that
    // contain underscores. The resolver path that powers `anvil --resume
    // <name>` is `resolve_name_to_id` → exact `==` match on the stored
    // sidecar "name" field, so any name accepted by is_valid_name is
    // resolvable. is_valid_name explicitly allows ASCII `_`, so the
    // verification is: confirm both `_`-only and `_`-mixed names pass
    // validation. These tests would catch a regression where a future
    // tightening of the regex (e.g. `[A-Za-z0-9-]` without `_`) silently
    // broke saved sessions that already used underscores.

    #[test]
    fn name_validation_accepts_underscore_only() {
        assert!(is_valid_name("snake_case_name"));
        assert!(is_valid_name("_leading"));
        assert!(is_valid_name("trailing_"));
        assert!(is_valid_name("__double__"));
        assert!(is_valid_name("a_b_c_d_e"));
    }

    #[test]
    fn name_validation_accepts_mixed_underscore_and_dash() {
        assert!(is_valid_name("v2_2_14-cc-parity"));
        assert!(is_valid_name("auth_refactor-attempt-3"));
        assert!(is_valid_name("123_abc-XYZ"));
    }

    #[test]
    fn name_validation_accepts_single_char_underscore() {
        // Edge case: the minimum-length name (1 char) must allow `_`
        // since otherwise users would be locked out of the shortest
        // session-name convention.
        assert!(is_valid_name("_"));
    }
}
