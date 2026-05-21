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

// ─── Title heuristic ─────────────────────────────────────────────────────────

/// Derive a short session title from the user's first message.
///
/// Rules (#563, CC-142-B; #580 auto-title wiring):
/// 1. If the first message is a bare URL (all tokens start with `http://`
///    or `https://`, no surrounding text) → return `None` (caller should use
///    a generic fallback like `"Session <date>"`).
/// 2. If the message contains a URL but also surrounding text → strip the URL
///    tokens and use the remaining text.
/// 3. Strip a leading slash-command token (e.g. `/compact`, `/model gpt-5`)
///    so titles reflect the user's actual ask rather than the bare command.
/// 4. Truncate to ~40 chars (whole characters, not bytes).
/// 5. Sanitize for sidecar-name compatibility: only `[A-Za-z0-9_-]` chars
///    survive, others become `-`; collapse runs of `-`, trim leading/trailing
///    `-`/`_`. Empty result → `None`.
///
/// Returns `None` when no usable title can be extracted (bare URL or empty).
#[must_use]
pub(crate) fn derive_title_from_first_message(message: &str) -> Option<String> {
    let trimmed = message.trim();
    if trimmed.is_empty() {
        return None;
    }

    // Detect bare URL: entire message is a single http/https URL token.
    let is_bare_url = trimmed
        .split_ascii_whitespace()
        .all(|token| token.starts_with("http://") || token.starts_with("https://"));

    if is_bare_url {
        return None;
    }

    // Strip URL tokens; collect the rest.
    let without_urls: String = trimmed
        .split_ascii_whitespace()
        .filter(|token| !token.starts_with("http://") && !token.starts_with("https://"))
        .collect::<Vec<_>>()
        .join(" ");

    // Strip a leading slash-command token (`/foo` or `/foo arg`). The slash
    // command itself is rarely a useful title — what follows it is. If the
    // message is ONLY a slash command (`/help`), fall back to the command
    // name without the slash.
    let candidate_pre_slash = without_urls.trim();
    let candidate = if let Some(rest) = candidate_pre_slash.strip_prefix('/') {
        let mut parts = rest.splitn(2, char::is_whitespace);
        let cmd = parts.next().unwrap_or("");
        let args = parts.next().unwrap_or("").trim();
        if args.is_empty() { cmd.to_string() } else { args.to_string() }
    } else {
        candidate_pre_slash.to_string()
    };

    if candidate.trim().is_empty() {
        return None;
    }

    // Truncate to 40 chars before sanitization so we sanitize the relevant
    // prefix rather than throwing away leading whitespace surprises.
    const MAX_CHARS: usize = 40;
    let truncated: String = candidate.chars().take(MAX_CHARS).collect();

    // Sanitize to sidecar-name alphabet: [A-Za-z0-9_-]. Other chars → `-`.
    // Collapse repeated `-`. Trim leading/trailing `-`/`_`.
    let mut out = String::with_capacity(truncated.len());
    let mut last_dash = false;
    for ch in truncated.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
            out.push(ch);
            last_dash = ch == '-';
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    let title = out.trim_matches(|c| c == '-' || c == '_').to_string();
    if title.is_empty() { None } else { Some(title) }
}

/// Auto-set a session name from its first user message — but only if the
/// session has no existing name (manual /name or prior auto-name).
///
/// Called from `LiveCli::persist_session` after the first turn completes.
/// Failures are swallowed: a missing title or duplicate name must not
/// interrupt the persist path.
///
/// Triggers ONLY when:
///   * the session has at least one user AND one assistant message
///     (i.e. a complete turn — not just a queued send), AND
///   * no `<id>.meta.json` name field is set.
///
/// `first_user_message_text` is the plain-text first user message; if `None`
/// or the heuristic returns `None`, no name is written.
pub(crate) fn auto_set_title_if_missing(
    id: &str,
    first_user_message_text: Option<&str>,
    has_assistant_message: bool,
) {
    if !has_assistant_message {
        return;
    }
    if get_session_name(id).is_some() {
        return;
    }
    let Some(text) = first_user_message_text else { return };
    let Some(title) = derive_title_from_first_message(text) else { return };
    // set_session_name validates and enforces uniqueness; if a collision
    // exists (rare), silently skip rather than fail the persist path.
    let _ = set_session_name(id, &title);
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

    // ─── Task #721 — Resume session model preservation ───────────────────────
    //
    // CC parity, community issue anthropics/claude-code#61068. Audit doc:
    // docs/cc-parity-audit-2026-05-21.md TASK-N.
    //
    // The sidecar is the source of truth for the model identifier across
    // resume. The provider's /models endpoint MUST NOT override the
    // persisted value (it can change between releases — e.g. by dropping
    // the [1m] context window suffix on resume). These tests pin the
    // contract: whatever string is written via set_session_model comes
    // back from get_session_model byte-for-byte.
    //
    // Two spec tests:
    //   1. claude-opus-4-7-1m (the 1M context window variant) round-trips
    //      with its -1m suffix intact.
    //   2. An exotic provider:tag model name (anvil-mock:test) is
    //      preserved verbatim, including the colon.

    /// Switch into a fresh temp cwd, run `f`, then restore the original cwd.
    /// Serialised across tests via #[serial(cwd_session_meta)] because
    /// `std::env::set_current_dir` mutates process-global state.
    fn with_temp_workspace<F: FnOnce()>(f: F) {
        let orig = std::env::current_dir().expect("orig cwd");
        let tmp = tempfile::tempdir().expect("tempdir");
        std::env::set_current_dir(tmp.path()).expect("set_current_dir to temp");
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
        // Restore cwd before re-raising any panic so the next test starts clean.
        std::env::set_current_dir(&orig).expect("restore cwd");
        if let Err(p) = result {
            std::panic::resume_unwind(p);
        }
    }

    #[test]
    #[serial_test::serial(cwd_session_meta)]
    fn model_round_trip_preserves_1m_context_suffix() {
        // Spec test 1: the 1M context window variant must round-trip
        // exactly. A session created on claude-opus-4-7-1m must NOT
        // come back as plain claude-opus-4-7 (the bug observed in
        // anthropics/claude-code#61068).
        with_temp_workspace(|| {
            let id = "session-1m-test";
            set_session_model(id, "claude-opus-4-7-1m").expect("set model");
            let got = get_session_model(id);
            assert_eq!(
                got.as_deref(),
                Some("claude-opus-4-7-1m"),
                "model[1m] suffix MUST round-trip without being normalised"
            );
        });
    }

    #[test]
    #[serial_test::serial(cwd_session_meta)]
    fn model_round_trip_preserves_exotic_provider_tag() {
        // Spec test 2: an exotic provider:tag model name (with a colon,
        // unusual prefix) must come back byte-for-byte. The persisted
        // sidecar is the source of truth; no provider lookup may rewrite it.
        with_temp_workspace(|| {
            let id = "session-exotic-test";
            set_session_model(id, "anvil-mock:test").expect("set model");
            let got = get_session_model(id);
            assert_eq!(
                got.as_deref(),
                Some("anvil-mock:test"),
                "exotic provider:tag model must preserve verbatim"
            );
        });
    }

    #[test]
    #[serial_test::serial(cwd_session_meta)]
    fn model_round_trip_survives_name_then_model_update() {
        // Real-world ordering: a session is renamed first, then a model
        // is recorded. The name must not be lost when the model is
        // written, and the model must come back exactly.
        with_temp_workspace(|| {
            let id = "session-combined-test";
            set_session_name(id, "auth-refactor").expect("set name");
            set_session_model(id, "claude-opus-4-7-1m").expect("set model");
            assert_eq!(
                get_session_name(id).as_deref(),
                Some("auth-refactor"),
                "name must survive a subsequent model write"
            );
            assert_eq!(
                get_session_model(id).as_deref(),
                Some("claude-opus-4-7-1m"),
                "model must round-trip after the name write"
            );
        });
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

    // ── derive_title_from_first_message tests (#563, CC-142-B) ─────────────

    use super::derive_title_from_first_message;

    #[test]
    fn title_skips_bare_url() {
        assert_eq!(
            derive_title_from_first_message("https://example.com/some/path"),
            None
        );
        assert_eq!(
            derive_title_from_first_message("http://localhost:3000"),
            None
        );
    }

    #[test]
    fn title_uses_text_around_url() {
        let msg = "Please review this page https://example.com/pr/123 and tell me what's wrong";
        let title = derive_title_from_first_message(msg);
        assert!(
            title.is_some(),
            "expected a title from text + URL, got None"
        );
        let t = title.unwrap();
        assert!(
            !t.contains("https://"),
            "title should not contain the URL: {t}"
        );
        assert!(
            t.contains("review") || t.contains("page"),
            "title should contain surrounding text: {t}"
        );
    }

    #[test]
    fn title_falls_back_to_generic_when_only_url() {
        // A message that is only a URL (possibly with query params) → None
        assert_eq!(
            derive_title_from_first_message(
                "https://github.com/org/repo/pull/42?tab=files"
            ),
            None
        );
    }

    #[test]
    fn title_works_normally_for_regular_text() {
        // After #580: spaces become `-`, ~40 char cap, sanitized.
        let msg = "Refactor the database connection pool to use async/await";
        let title = derive_title_from_first_message(msg).expect("title");
        assert!(title.starts_with("Refactor-the-database-connection"), "got {title}");
        // Length cap (40 chars max, post-truncation; sanitization may shorten further).
        assert!(title.chars().count() <= 40, "title too long: {title}");
        // Sidecar-name alphabet only.
        assert!(
            title.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
            "non-sidecar-safe chars in title: {title}"
        );
    }

    // ── #580: slash-command stripping ─────────────────────────────────────

    #[test]
    fn title_strips_leading_slash_command_with_args() {
        // `/compact summarize the auth refactor` → title from the args.
        let title = derive_title_from_first_message(
            "/compact summarize the auth refactor",
        )
        .expect("title");
        assert!(!title.starts_with('/'), "title should not start with /: {title}");
        assert!(
            title.contains("summarize") || title.contains("auth"),
            "expected slash args in title: {title}"
        );
    }

    #[test]
    fn title_strips_bare_slash_command_keeps_name() {
        // `/help` alone → keep `help` (not None).
        let title = derive_title_from_first_message("/help").expect("title");
        assert_eq!(title, "help");
    }

    #[test]
    fn title_truncates_to_40_chars() {
        let msg = "a".repeat(200);
        let title = derive_title_from_first_message(&msg).expect("title");
        assert!(title.chars().count() <= 40, "title too long: {title}");
    }

    #[test]
    fn title_sanitizes_non_alphabet_chars() {
        // Question marks, colons, parens all become `-`.
        let title = derive_title_from_first_message(
            "How do I fix the SSO redirect? (auth0 + Okta)",
        )
        .expect("title");
        assert!(
            title.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
            "non-sidecar-safe chars: {title}"
        );
        // No leading/trailing dashes.
        assert!(!title.starts_with('-'));
        assert!(!title.ends_with('-'));
    }

    // ── #580: auto_set_title_if_missing gating ────────────────────────────

    #[test]
    fn auto_title_skips_when_no_assistant_message() {
        // Pure unit logic: with has_assistant_message=false we should
        // never touch the sidecar. Use a non-existent session id —
        // even if we tried to write, set_session_name would create
        // a sidecar in the workspace, but the gate fires FIRST.
        // Verify by post-condition: get_session_name still None.
        let id = "test-session-no-assistant-message";
        // Make sure we start clean.
        if let Ok(p) = meta_path_for_id(id)
            && p.exists()
        {
            let _ = fs::remove_file(p);
        }
        auto_set_title_if_missing(id, Some("hello world"), false);
        assert_eq!(get_session_name(id), None);
    }
}
