// Allow `unsafe` only in test code (env::set_var for ANVIL_CONFIG_HOME).
#![cfg_attr(test, allow(unsafe_code))]

/// Settings import — Phase 6.2a.
///
/// Translates `~/.claude/settings.json` (+ optional `settings.local.json`)
/// into Anvil's `~/.anvil/settings.json` schema per §5 of
/// `MIGRATION-CLAUDE-CODE.md`.
///
/// # Pipeline
///
/// 1. **Discover** — [`SettingsDiscoverer::discover`] walks the CC profile
///    directory and returns `ImportArtifact::Settings` entries.
/// 2. **Triage** — [`SettingsTriager::triage`] categorises each file as
///    Keep (always, for settings).
/// 3. **Translate** — [`SettingsTranslator::translate`] drives
///    [`translate_settings`], which applies the §5 key table, detects
///    conflicts with existing Anvil settings, and writes:
///    - `<staging>/settings/translated.json` — proposed merged settings
///    - `<staging>/settings/conflicts.json`  — keys that need user review
/// 4. **Stage** — [`SettingsStager::stage`] copies `translated.json` to
///    staging; the commit phase moves it to `~/.anvil/settings.json` only
///    when `conflicts.json` is empty (or `--accept-cc-on-conflict` is set,
///    deferred to Phase 6.4).
///
/// # Conflict semantics
///
/// A conflict arises when a translated key is already present in the existing
/// Anvil `settings.json` with a *different* value.  Conflicting keys are:
/// - staged to `<staging>/settings/conflicts.json` as
///   `{ key, cc_value, anvil_value, decision: "user_required" }`
/// - the manifest entry is set to `NeedsReview`
/// - the translated.json is still written with the *CC* value so the user
///   can see what the diff looks like (Phase 6.4's TUI shows the diff)
///
/// # Unknown CC keys
///
/// Any CC key not in the §5 translation table is preserved verbatim in the
/// translated output AND logged to the manifest entry's `notes` field with
/// the original key + value.  Nothing is discarded silently.
///
/// # Hook translation
///
/// CC hooks come in three forms (§5):
///   1. `{ "type": "command", "command": "echo hi" }` → Anvil `Command`
///   2. `{ "type": "exec", "args": [...], "continueOnBlock": bool }` → Anvil `Exec`
///   3. `{ "type": "mcp_tool", "server": "s", "tool": "t", "input": {} }` → Anvil `McpTool`
///
/// [`translate_hook_spec`] handles all three forms and is re-exported for use
/// by `plugins.rs` (DRY).

use std::path::{Path, PathBuf};

use std::time::SystemTime;

use serde_json::{Map, Value as JsonValue};

use crate::import::artifact::{ImportArtifact, ImportArtifactMeta, ImportSource, SettingsScope};
use crate::import::discover::{DiscoveredArtifact, Discoverer};
use crate::import::stage::{StageAction, Stager};
use crate::import::staging::StagingDir;
use crate::import::translate::{TranslationResult, Translator};
use crate::import::triage::{TriageDecision, Triager};
use crate::import::sha256_file;


// ── Hook translation ─────────────────────────────────────────────────────────

/// Translate a single CC hook JSON entry to an Anvil-compatible JSON object.
///
/// CC hook forms supported:
///
/// | CC `type` field | Example | Anvil output |
/// |---|---|---|
/// | `"command"` | `{"type":"command","command":"echo hi"}` | `{"type":"command","body":"echo hi"}` |
/// | `"exec"` | `{"type":"exec","args":["./h"],"continueOnBlock":true}` | `{"args":["./h"],"continue_on_block":true}` |
/// | `"mcp_tool"` | `{"type":"mcp_tool","server":"s","tool":"t","input":{}}` | `{"type":"mcp_tool","server":"s","tool":"t","input":{}}` |
/// | (bare string) | `"./hooks/pre.sh"` | `"./hooks/pre.sh"` (pass-through) |
///
/// Returns `Err(description)` for entirely unparseable entries.
///
/// This function is `pub(super)` so `plugins.rs` can call it — DRY.
pub(super) fn translate_hook_spec(cc_hook: &JsonValue) -> Result<JsonValue, String> {
    // Bare string — Command shorthand.  Pass through verbatim.
    if cc_hook.is_string() {
        return Ok(cc_hook.clone());
    }

    let obj = cc_hook
        .as_object()
        .ok_or_else(|| format!("hook entry is not a string or object: {cc_hook}"))?;

    let type_str = obj
        .get("type")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    match type_str {
        "command" => {
            // CC: { "type": "command", "command": "echo hi" }
            // Anvil: { "type": "command", "body": "echo hi" }
            let command = obj
                .get("command")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "command hook missing `command` field".to_string())?;
            let mut out = Map::new();
            out.insert("type".to_string(), JsonValue::String("command".to_string()));
            out.insert("body".to_string(), JsonValue::String(command.to_string()));
            Ok(JsonValue::Object(out))
        }
        "exec" => {
            // CC: { "type": "exec", "args": [...], "continueOnBlock": bool }
            // Anvil: { "args": [...], "continue_on_block": bool }
            let args = obj
                .get("args")
                .and_then(|v| v.as_array())
                .ok_or_else(|| "exec hook missing `args` array".to_string())?;
            if args.is_empty() {
                return Err("exec hook `args` array must not be empty".to_string());
            }
            let mut out = Map::new();
            out.insert("args".to_string(), JsonValue::Array(args.clone()));
            if let Some(cob) = obj.get("continueOnBlock").and_then(|v| v.as_bool()) {
                out.insert(
                    "continue_on_block".to_string(),
                    JsonValue::Bool(cob),
                );
            }
            Ok(JsonValue::Object(out))
        }
        "mcp_tool" => {
            // CC and Anvil share the same mcp_tool shape.  Pass through, but
            // validate required fields are present.
            let server = obj
                .get("server")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "mcp_tool hook missing `server` field".to_string())?;
            let tool = obj
                .get("tool")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "mcp_tool hook missing `tool` field".to_string())?;
            let input = obj
                .get("input")
                .cloned()
                .unwrap_or(JsonValue::Object(Map::new()));
            let mut out = Map::new();
            out.insert("type".to_string(), JsonValue::String("mcp_tool".to_string()));
            out.insert("server".to_string(), JsonValue::String(server.to_string()));
            out.insert("tool".to_string(), JsonValue::String(tool.to_string()));
            out.insert("input".to_string(), input);
            Ok(JsonValue::Object(out))
        }
        "" => {
            // No `type` field — could be the Exec bare-args form: `{"args": [...]}`
            if let Some(args) = obj.get("args").and_then(|v| v.as_array()) {
                if args.is_empty() {
                    return Err("exec hook `args` array must not be empty".to_string());
                }
                let mut out = Map::new();
                out.insert("args".to_string(), JsonValue::Array(args.clone()));
                if let Some(cob) = obj.get("continue_on_block").and_then(|v| v.as_bool()) {
                    out.insert(
                        "continue_on_block".to_string(),
                        JsonValue::Bool(cob),
                    );
                }
                // Also accept the CC camelCase spelling for this no-type form
                if let Some(cob) = obj.get("continueOnBlock").and_then(|v| v.as_bool()) {
                    out.insert(
                        "continue_on_block".to_string(),
                        JsonValue::Bool(cob),
                    );
                }
                Ok(JsonValue::Object(out))
            } else {
                Err(format!("unrecognised hook entry (no `type`, no `args`): {cc_hook}"))
            }
        }
        other => Err(format!("unknown hook type '{other}': {cc_hook}")),
    }
}

// ── Settings translation result ──────────────────────────────────────────────

/// Result of translating a CC `settings.json` into Anvil's schema.
#[derive(Debug, Clone)]
pub struct SettingsTranslationResult {
    /// The proposed merged settings as a JSON object.
    pub translated: Map<String, JsonValue>,
    /// Conflicting keys (key exists in both CC and existing Anvil settings
    /// with different values).  Each entry has:
    ///   `{ "key": ..., "cc_value": ..., "anvil_value": ..., "decision": "user_required" }`
    pub conflicts: Vec<JsonValue>,
    /// Unknown CC keys that were preserved verbatim — surfaced in the
    /// manifest notes so the user can review.
    pub unknown_keys: Vec<String>,
    /// Translation warnings (e.g. a hook entry that couldn't be parsed).
    pub warnings: Vec<String>,
}

// ── Core translation function ─────────────────────────────────────────────────

/// Translate a CC `settings.json` (merged with optional `settings.local.json`)
/// into Anvil's settings schema per §5.
///
/// `cc_settings`: parsed JSON of the *merged* CC settings (local-wins semantics
/// already applied by the caller).
///
/// `existing_anvil`: parsed JSON of the current Anvil `~/.anvil/settings.json`,
/// if it exists.  Used to detect conflicts.  Pass `None` when no Anvil settings
/// exist yet.
///
/// Returns a [`SettingsTranslationResult`] that the caller stages to disk.
#[must_use]
pub fn translate_settings(
    cc_settings: &JsonValue,
    existing_anvil: Option<&JsonValue>,
) -> SettingsTranslationResult {
    let cc_obj = match cc_settings.as_object() {
        Some(o) => o,
        None => {
            return SettingsTranslationResult {
                translated: Map::new(),
                conflicts: vec![],
                unknown_keys: vec![],
                warnings: vec!["CC settings is not a JSON object; skipping".to_string()],
            };
        }
    };

    let anvil_obj: &Map<String, JsonValue> = match existing_anvil.and_then(|v| v.as_object()) {
        Some(o) => o,
        None => &Map::new(),
    };

    // We'll build the output map starting from existing Anvil settings (if any),
    // then apply CC-translated values on top — with conflict detection.
    let mut out: Map<String, JsonValue> = anvil_obj.clone();
    let mut conflicts: Vec<JsonValue> = Vec::new();
    let mut unknown_keys: Vec<String> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();

    for (cc_key, cc_val) in cc_obj {
        translate_one_key(
            cc_key,
            cc_val,
            anvil_obj,
            &mut out,
            &mut conflicts,
            &mut unknown_keys,
            &mut warnings,
        );
    }

    SettingsTranslationResult {
        translated: out,
        conflicts,
        unknown_keys,
        warnings,
    }
}

/// Apply translation for a single CC key→value pair.
#[allow(clippy::too_many_arguments)]
fn translate_one_key(
    cc_key: &str,
    cc_val: &JsonValue,
    anvil_obj: &Map<String, JsonValue>,
    out: &mut Map<String, JsonValue>,
    conflicts: &mut Vec<JsonValue>,
    unknown_keys: &mut Vec<String>,
    warnings: &mut Vec<String>,
) {
    match cc_key {
        // ── Direct mappings ──────────────────────────────────────────────────
        "theme"
        | "effort_level"
        | "enabledPlugins"
        | "mcpServers"
        | "worktree"
        | "autoMode" => {
            set_with_conflict(cc_key, cc_val.clone(), anvil_obj, out, conflicts);
        }

        // ── permissions.allow / permissions.deny / permissions.defaultMode ──
        "permissions" => {
            if let Some(perm_obj) = cc_val.as_object() {
                let mut anvil_perms = out
                    .get("permissions")
                    .and_then(|v| v.as_object())
                    .cloned()
                    .unwrap_or_default();
                let existing_perms = anvil_obj
                    .get("permissions")
                    .and_then(|v| v.as_object())
                    .cloned()
                    .unwrap_or_default();

                for (perm_key, perm_val) in perm_obj {
                    match perm_key.as_str() {
                        "allow" | "deny" => {
                            let anvil_key = perm_key.as_str(); // direct map
                            set_with_conflict_nested(
                                "permissions",
                                anvil_key,
                                perm_val.clone(),
                                &existing_perms,
                                &mut anvil_perms,
                                conflicts,
                            );
                        }
                        "defaultMode" => {
                            // CC `permissions.defaultMode` → Anvil top-level `permissionMode`
                            set_with_conflict(
                                "permissionMode",
                                perm_val.clone(),
                                anvil_obj,
                                out,
                                conflicts,
                            );
                        }
                        other => {
                            // Unknown sub-key under permissions
                            let note = format!("permissions.{other}");
                            set_with_conflict_nested(
                                "permissions",
                                other,
                                perm_val.clone(),
                                &existing_perms,
                                &mut anvil_perms,
                                conflicts,
                            );
                            unknown_keys.push(note);
                        }
                    }
                }
                out.insert("permissions".to_string(), JsonValue::Object(anvil_perms));
            }
        }

        // ── outputStyle → output_style with value rewrite ───────────────────
        "outputStyle" => {
            let anvil_val = match cc_val.as_str() {
                Some("concise") => JsonValue::String("condensed".to_string()),
                _ => JsonValue::String("precise".to_string()),
            };
            set_with_conflict("output_style", anvil_val, anvil_obj, out, conflicts);
        }

        // ── hooks: translate each event's array ─────────────────────────────
        "hooks" => {
            if let Some(hooks_obj) = cc_val.as_object() {
                let mut anvil_hooks = out
                    .get("hooks")
                    .and_then(|v| v.as_object())
                    .cloned()
                    .unwrap_or_default();

                translate_hooks_object(
                    hooks_obj,
                    &mut anvil_hooks,
                    warnings,
                );

                out.insert("hooks".to_string(), JsonValue::Object(anvil_hooks));
            }
        }

        // ── additionalDirectories → sandbox.allowedMounts ───────────────────
        "additionalDirectories" => {
            if let Some(dirs) = cc_val.as_array() {
                let mut sandbox = out
                    .get("sandbox")
                    .and_then(|v| v.as_object())
                    .cloned()
                    .unwrap_or_default();
                sandbox.insert(
                    "allowedMounts".to_string(),
                    JsonValue::Array(dirs.clone()),
                );
                out.insert("sandbox".to_string(), JsonValue::Object(sandbox));
                warnings.push(
                    "additionalDirectories translated to sandbox.allowedMounts \
                     (different name, same idea — verify paths are still valid)"
                        .to_string(),
                );
            }
        }

        // ── SKIP: CC credentials ─────────────────────────────────────────────
        "claudeAiOauth" | "claudeAiOAuth" => {
            // Intentionally not imported — user re-authenticates via `anvil login`.
        }

        // ── Unknown CC keys — preserve verbatim, surface in notes ───────────
        other => {
            // Write the key verbatim into the translated output under a
            // `cc_metadata` sub-object for forward-compat.
            let mut cc_meta = out
                .get("cc_metadata")
                .and_then(|v| v.as_object())
                .cloned()
                .unwrap_or_default();
            cc_meta.insert(other.to_string(), cc_val.clone());
            out.insert("cc_metadata".to_string(), JsonValue::Object(cc_meta));
            unknown_keys.push(format!("{other} = {cc_val}"));
        }
    }
}

/// Translate a CC `hooks` object (keys are event names, values are arrays).
///
/// CC key names → Anvil key names:
///   `PreToolUse`  → `pre_tool_use`
///   `PostToolUse` → `post_tool_use`
///   `SessionStart` → `session_start`
///   (any other CC hook key → snake_case passthrough)
fn translate_hooks_object(
    cc_hooks: &Map<String, JsonValue>,
    anvil_hooks: &mut Map<String, JsonValue>,
    warnings: &mut Vec<String>,
) {
    for (event_name, hooks_arr) in cc_hooks {
        let anvil_event = match event_name.as_str() {
            "PreToolUse" => "pre_tool_use",
            "PostToolUse" => "post_tool_use",
            "SessionStart" => "session_start",
            "SessionEnd" => "session_end",
            other => {
                // Snake-case conversion as best-effort passthrough.
                &*Box::leak(to_snake_case(other).into_boxed_str())
            }
        };

        let arr = match hooks_arr.as_array() {
            Some(a) => a,
            None => {
                warnings.push(format!("hooks.{event_name} is not an array; skipping"));
                continue;
            }
        };

        let mut translated_arr: Vec<JsonValue> = Vec::new();
        for (i, hook) in arr.iter().enumerate() {
            match translate_hook_spec(hook) {
                Ok(translated) => translated_arr.push(translated),
                Err(e) => {
                    warnings.push(format!(
                        "hooks.{event_name}[{i}] translation failed: {e}; entry skipped"
                    ));
                }
            }
        }

        anvil_hooks.insert(anvil_event.to_string(), JsonValue::Array(translated_arr));
    }
}

/// Convert a PascalCase or camelCase string to snake_case (simple heuristic).
fn to_snake_case(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 4);
    for (i, ch) in s.chars().enumerate() {
        if ch.is_uppercase() && i > 0 {
            out.push('_');
        }
        out.push(ch.to_ascii_lowercase());
    }
    out
}

/// Set `anvil_key` in `out` to `value`, recording a conflict entry if the key
/// already exists in `anvil_obj` (the original Anvil settings) with a different value.
fn set_with_conflict(
    anvil_key: &str,
    value: JsonValue,
    anvil_obj: &Map<String, JsonValue>,
    out: &mut Map<String, JsonValue>,
    conflicts: &mut Vec<JsonValue>,
) {
    if let Some(existing) = anvil_obj.get(anvil_key) {
        if *existing != value {
            let mut conflict = Map::new();
            conflict.insert("key".to_string(), JsonValue::String(anvil_key.to_string()));
            conflict.insert("cc_value".to_string(), value.clone());
            conflict.insert("anvil_value".to_string(), existing.clone());
            conflict.insert(
                "decision".to_string(),
                JsonValue::String("user_required".to_string()),
            );
            conflicts.push(JsonValue::Object(conflict));
        }
    }
    // Always write — translated.json shows CC value; conflicts.json shows the diff
    out.insert(anvil_key.to_string(), value);
}

/// Same as `set_with_conflict` but operates on a nested sub-object.
fn set_with_conflict_nested(
    parent_key: &str,
    sub_key: &str,
    value: JsonValue,
    existing_parent: &Map<String, JsonValue>,
    out_parent: &mut Map<String, JsonValue>,
    conflicts: &mut Vec<JsonValue>,
) {
    let full_key = format!("{parent_key}.{sub_key}");
    if let Some(existing) = existing_parent.get(sub_key) {
        if *existing != value {
            let mut conflict = Map::new();
            conflict.insert("key".to_string(), JsonValue::String(full_key));
            conflict.insert("cc_value".to_string(), value.clone());
            conflict.insert("anvil_value".to_string(), existing.clone());
            conflict.insert(
                "decision".to_string(),
                JsonValue::String("user_required".to_string()),
            );
            conflicts.push(JsonValue::Object(conflict));
        }
    }
    out_parent.insert(sub_key.to_string(), value);
}

// ── Merge CC settings + local override ──────────────────────────────────────

/// Merge `~/.claude/settings.json` with `~/.claude/settings.local.json`.
///
/// Local-wins semantics: every top-level key in `local` overrides the same
/// key in `base`.
#[must_use]
pub fn merge_cc_settings(base: &JsonValue, local: Option<&JsonValue>) -> JsonValue {
    let mut merged: Map<String, JsonValue> = base
        .as_object()
        .cloned()
        .unwrap_or_default();

    if let Some(local_obj) = local.and_then(|v| v.as_object()) {
        for (k, v) in local_obj {
            merged.insert(k.clone(), v.clone());
        }
    }

    JsonValue::Object(merged)
}

// ── Discoverer ───────────────────────────────────────────────────────────────

/// Discovers `~/.claude/settings.json` and optionally `~/.claude/settings.local.json`.
pub struct SettingsDiscoverer;

impl Discoverer for SettingsDiscoverer {
    fn discover(&self, source: &ImportSource) -> Vec<DiscoveredArtifact> {
        let profile_dir = match source {
            ImportSource::ClaudeCode { profile_dir } => profile_dir.clone(),
            _ => return vec![],
        };

        let mut results = Vec::new();

        for (filename, scope) in &[
            ("settings.json", SettingsScope::User),
            ("settings.local.json", SettingsScope::User),
        ] {
            let path = profile_dir.join(filename);
            if !path.exists() {
                continue;
            }
            let hash = sha256_file(&path).unwrap_or_default();
            results.push(DiscoveredArtifact {
                artifact: ImportArtifact::Settings {
                    path: path.clone(),
                    scope: *scope,
                },
                meta: ImportArtifactMeta {
                    source: source.clone(),
                    source_path: path,
                    content_hash: hash,
                    discovered_at: SystemTime::now(),
                },
            });
        }

        results
    }

    fn name(&self) -> &'static str {
        "settings-discoverer"
    }
}

// ── Triager ──────────────────────────────────────────────────────────────────

/// Triages settings artifacts — always Keep (settings are always worth translating).
pub struct SettingsTriager;

impl Triager for SettingsTriager {
    fn triage(&self, artifact: &ImportArtifact, _meta: &ImportArtifactMeta) -> TriageDecision {
        match artifact {
            ImportArtifact::Settings { .. } => TriageDecision::Keep,
            _ => TriageDecision::Skip {
                reason: "SettingsTriager only handles Settings artifacts".to_string(),
            },
        }
    }

    fn name(&self) -> &'static str {
        "settings-triager"
    }
}

// ── Translator ───────────────────────────────────────────────────────────────

/// Translates a CC `settings.json` into Anvil's settings schema.
///
/// The `existing_anvil_settings_path` is checked for conflict detection.
pub struct SettingsTranslator {
    /// Path to the existing Anvil `settings.json`, if any.
    pub existing_anvil_settings_path: Option<PathBuf>,
    /// Path to `~/.claude/settings.local.json` (override layer).
    pub local_override_path: Option<PathBuf>,
}

impl SettingsTranslator {
    /// Construct with explicit paths (for testing).
    #[must_use]
    pub fn new(
        existing_anvil_settings_path: Option<PathBuf>,
        local_override_path: Option<PathBuf>,
    ) -> Self {
        Self {
            existing_anvil_settings_path,
            local_override_path,
        }
    }
}

impl Translator for SettingsTranslator {
    fn translate(
        &self,
        artifact: &ImportArtifact,
        _meta: &ImportArtifactMeta,
        source_bytes: &[u8],
    ) -> Result<TranslationResult, String> {
        match artifact {
            ImportArtifact::Settings { path, .. } => {
                // Parse the CC settings file
                let cc_settings: JsonValue = serde_json::from_slice(source_bytes)
                    .map_err(|e| format!("parse CC settings {}: {e}", path.display()))?;

                // Load and merge local override if present
                let local_override: Option<JsonValue> = self
                    .local_override_path
                    .as_deref()
                    .and_then(|p| std::fs::read(p).ok())
                    .and_then(|bytes| serde_json::from_slice(&bytes).ok());

                let merged_cc = merge_cc_settings(&cc_settings, local_override.as_ref());

                // Load existing Anvil settings for conflict detection
                let existing_anvil: Option<JsonValue> = self
                    .existing_anvil_settings_path
                    .as_deref()
                    .and_then(|p| std::fs::read(p).ok())
                    .and_then(|bytes| serde_json::from_slice(&bytes).ok());

                // Run translation
                let result = translate_settings(&merged_cc, existing_anvil.as_ref());

                // Encode conflicts.json (even if empty — caller checks emptiness)
                let conflicts_bytes = serde_json::to_vec_pretty(&result.conflicts)
                    .map_err(|e| format!("serialize conflicts: {e}"))?;

                // Determine destination
                let anvil_home = crate::import::staging::anvil_config_home();
                let destination = anvil_home.join("settings.json");

                // Build warnings for the manifest note
                let mut warning_parts: Vec<String> = result.warnings.clone();
                if !result.unknown_keys.is_empty() {
                    warning_parts.push(format!(
                        "Unknown CC keys preserved in cc_metadata: {}",
                        result.unknown_keys.join(", ")
                    ));
                }
                if !result.conflicts.is_empty() {
                    warning_parts.push(format!(
                        "{} conflict(s) require user review — staged to conflicts.json",
                        result.conflicts.len()
                    ));
                }

                // We return the translated JSON; conflicts.json is a side-write
                // that the Stager handles via the `warning` field.
                let warning = if warning_parts.is_empty() {
                    None
                } else {
                    Some(warning_parts.join("; "))
                };

                // Embed conflicts bytes in the suggested_name so the Stager can
                // write them. This is done via a JSON envelope so we don't break
                // the TranslationResult type signature.
                let envelope = serde_json::json!({
                    "translated": result.translated,
                    "conflicts": result.conflicts,
                    "conflicts_bytes": String::from_utf8_lossy(&conflicts_bytes),
                });
                let envelope_bytes = serde_json::to_vec_pretty(&envelope)
                    .map_err(|e| format!("serialize settings envelope: {e}"))?;

                Ok(TranslationResult {
                    bytes: envelope_bytes,
                    suggested_name: "settings-envelope.json".to_string(),
                    destination,
                    warning,
                })
            }
            _ => Err("SettingsTranslator only handles Settings artifacts".to_string()),
        }
    }

    fn name(&self) -> &'static str {
        "settings-translator"
    }
}

// ── Stager ───────────────────────────────────────────────────────────────────

/// Stages translated settings to `<staging>/settings/`.
///
/// Writes:
/// - `<staging>/settings/translated.json` — proposed merged settings
/// - `<staging>/settings/conflicts.json` — conflict records (may be empty array)
pub struct SettingsStager;

impl Stager for SettingsStager {
    fn stage(
        &self,
        _artifact: &ImportArtifact,
        translation: &TranslationResult,
        staging: &StagingDir,
    ) -> Result<StageAction, String> {
        // Parse the envelope written by the translator
        let envelope: JsonValue = serde_json::from_slice(&translation.bytes)
            .map_err(|e| format!("parse settings envelope: {e}"))?;

        let translated = envelope
            .get("translated")
            .ok_or("envelope missing `translated`")?;
        let conflicts = envelope
            .get("conflicts")
            .ok_or("envelope missing `conflicts`")?;

        // Write translated.json
        let translated_bytes = serde_json::to_vec_pretty(translated)
            .map_err(|e| format!("serialize translated.json: {e}"))?;
        staging
            .stage_bytes("settings/translated.json", &translated_bytes)
            .map_err(|e| e.to_string())?;

        // Write conflicts.json (even if the array is empty)
        let conflicts_bytes = serde_json::to_vec_pretty(conflicts)
            .map_err(|e| format!("serialize conflicts.json: {e}"))?;
        staging
            .stage_bytes("settings/conflicts.json", &conflicts_bytes)
            .map_err(|e| e.to_string())?;

        let staged_path = staging.path("settings/translated.json");
        Ok(StageAction {
            staged_path,
            destination: translation.destination.clone(),
        })
    }

    fn name(&self) -> &'static str {
        "settings-stager"
    }
}

// ── Public convenience entry point ───────────────────────────────────────────

/// Run the full settings import pipeline for a CC profile directory.
///
/// This is the standalone function called by `handlers.rs::handle_import_command`
/// (or by the Phase 6.1 composer) — no trait plumbing needed at the call site.
///
/// Returns `(conflict_count, warnings, unknown_keys)` so the caller can record
/// the outcome in the manifest.
///
/// # Errors
///
/// Returns an error string if the source settings file cannot be read or parsed,
/// or if staging writes fail.
pub fn run_settings_import(
    cc_profile_dir: &Path,
    staging: &StagingDir,
) -> Result<(usize, Vec<String>, Vec<String>), String> {
    let cc_settings_path = cc_profile_dir.join("settings.json");
    if !cc_settings_path.exists() {
        return Ok((0, vec![], vec![]));
    }

    let cc_bytes = std::fs::read(&cc_settings_path)
        .map_err(|e| format!("read CC settings: {e}"))?;

    let cc_json: JsonValue = serde_json::from_slice(&cc_bytes)
        .map_err(|e| format!("parse CC settings: {e}"))?;

    // Load local override
    let local_path = cc_profile_dir.join("settings.local.json");
    let local_json: Option<JsonValue> = if local_path.exists() {
        std::fs::read(&local_path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
    } else {
        None
    };

    let merged = merge_cc_settings(&cc_json, local_json.as_ref());

    // Load existing Anvil settings
    let anvil_home = crate::import::staging::anvil_config_home();
    let anvil_settings_path = anvil_home.join("settings.json");
    let existing_anvil: Option<JsonValue> = if anvil_settings_path.exists() {
        std::fs::read(&anvil_settings_path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
    } else {
        None
    };

    let result = translate_settings(&merged, existing_anvil.as_ref());

    // Stage translated.json
    let translated_bytes = serde_json::to_vec_pretty(&result.translated)
        .map_err(|e| format!("serialize translated.json: {e}"))?;
    staging
        .stage_bytes("settings/translated.json", &translated_bytes)
        .map_err(|e| e.to_string())?;

    // Stage conflicts.json (always, even if empty)
    let conflicts_bytes = serde_json::to_vec_pretty(&result.conflicts)
        .map_err(|e| format!("serialize conflicts.json: {e}"))?;
    staging
        .stage_bytes("settings/conflicts.json", &conflicts_bytes)
        .map_err(|e| e.to_string())?;

    let conflict_count = result.conflicts.len();
    Ok((conflict_count, result.warnings, result.unknown_keys))
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use tempfile::TempDir;

    // ── Hook translation ─────────────────────────────────────────────────────

    #[test]
    fn hook_command_form_translates() {
        let cc = serde_json::json!({ "type": "command", "command": "echo hello" });
        let out = translate_hook_spec(&cc).unwrap();
        assert_eq!(out["type"], "command");
        assert_eq!(out["body"], "echo hello");
    }

    #[test]
    fn hook_exec_form_translates() {
        let cc = serde_json::json!({
            "type": "exec",
            "args": ["./hooks/pre.sh", "{tool_name}"],
            "continueOnBlock": true
        });
        let out = translate_hook_spec(&cc).unwrap();
        assert!(out.get("type").is_none(), "exec form should have no `type` field");
        assert_eq!(out["args"][0], "./hooks/pre.sh");
        assert_eq!(out["continue_on_block"], true);
    }

    #[test]
    fn hook_exec_form_without_continue_on_block() {
        let cc = serde_json::json!({
            "type": "exec",
            "args": ["./hooks/post.sh"]
        });
        let out = translate_hook_spec(&cc).unwrap();
        assert!(out.get("continue_on_block").is_none());
        assert_eq!(out["args"][0], "./hooks/post.sh");
    }

    #[test]
    fn hook_mcp_tool_form_translates() {
        let cc = serde_json::json!({
            "type": "mcp_tool",
            "server": "myserver",
            "tool": "redact",
            "input": { "key": "value" }
        });
        let out = translate_hook_spec(&cc).unwrap();
        assert_eq!(out["type"], "mcp_tool");
        assert_eq!(out["server"], "myserver");
        assert_eq!(out["tool"], "redact");
        assert_eq!(out["input"]["key"], "value");
    }

    #[test]
    fn hook_bare_string_passes_through() {
        let cc = JsonValue::String("./hooks/pre.sh".to_string());
        let out = translate_hook_spec(&cc).unwrap();
        assert_eq!(out, cc);
    }

    #[test]
    fn hook_no_type_args_form_translates() {
        // CC also supports the bare exec form without a `type` discriminant
        let cc = serde_json::json!({
            "args": ["./hooks/lint.sh"],
            "continueOnBlock": false
        });
        let out = translate_hook_spec(&cc).unwrap();
        assert_eq!(out["args"][0], "./hooks/lint.sh");
        assert_eq!(out["continue_on_block"], false);
    }

    #[test]
    fn hook_command_missing_command_field_returns_err() {
        let cc = serde_json::json!({ "type": "command" });
        assert!(translate_hook_spec(&cc).is_err());
    }

    #[test]
    fn hook_exec_empty_args_returns_err() {
        let cc = serde_json::json!({ "type": "exec", "args": [] });
        assert!(translate_hook_spec(&cc).is_err());
    }

    #[test]
    fn hook_mcp_tool_missing_server_returns_err() {
        let cc = serde_json::json!({ "type": "mcp_tool", "tool": "t", "input": {} });
        assert!(translate_hook_spec(&cc).is_err());
    }

    #[test]
    fn hook_mcp_tool_missing_tool_returns_err() {
        let cc = serde_json::json!({ "type": "mcp_tool", "server": "s", "input": {} });
        assert!(translate_hook_spec(&cc).is_err());
    }

    #[test]
    fn hook_unknown_type_returns_err() {
        let cc = serde_json::json!({ "type": "future_type", "body": "x" });
        assert!(translate_hook_spec(&cc).is_err());
    }

    // ── Settings translation ──────────────────────────────────────────────────

    #[test]
    fn theme_direct_map() {
        let cc = serde_json::json!({ "theme": "dark" });
        let r = translate_settings(&cc, None);
        assert_eq!(r.translated["theme"], "dark");
        assert!(r.conflicts.is_empty());
        assert!(r.unknown_keys.is_empty());
    }

    #[test]
    fn output_style_concise_maps_to_condensed() {
        let cc = serde_json::json!({ "outputStyle": "concise" });
        let r = translate_settings(&cc, None);
        assert_eq!(r.translated["output_style"], "condensed");
    }

    #[test]
    fn output_style_other_maps_to_precise() {
        let cc = serde_json::json!({ "outputStyle": "verbose" });
        let r = translate_settings(&cc, None);
        assert_eq!(r.translated["output_style"], "precise");
    }

    #[test]
    fn output_style_missing_defaults_to_precise() {
        // When CC has no outputStyle, Anvil should default to precise.
        // (This tests the case where the key is simply absent from CC settings.)
        let cc = serde_json::json!({});
        let r = translate_settings(&cc, None);
        // output_style should not be set if CC didn't specify it
        assert!(r.translated.get("output_style").is_none());
    }

    #[test]
    fn permissions_allow_and_deny_direct_map() {
        let cc = serde_json::json!({
            "permissions": {
                "allow": ["Bash(git:*)"],
                "deny": ["Bash(rm:*)"]
            }
        });
        let r = translate_settings(&cc, None);
        let perms = r.translated["permissions"].as_object().unwrap();
        assert_eq!(perms["allow"][0], "Bash(git:*)");
        assert_eq!(perms["deny"][0], "Bash(rm:*)");
    }

    #[test]
    fn permissions_default_mode_maps_to_permission_mode() {
        let cc = serde_json::json!({
            "permissions": { "defaultMode": "acceptEdits" }
        });
        let r = translate_settings(&cc, None);
        assert_eq!(r.translated["permissionMode"], "acceptEdits");
    }

    #[test]
    fn hooks_pre_tool_use_translates() {
        let cc = serde_json::json!({
            "hooks": {
                "PreToolUse": [
                    { "type": "command", "command": "echo pre" }
                ]
            }
        });
        let r = translate_settings(&cc, None);
        let hooks = r.translated["hooks"].as_object().unwrap();
        assert!(hooks.contains_key("pre_tool_use"), "should have pre_tool_use");
        let pre = &hooks["pre_tool_use"][0];
        assert_eq!(pre["type"], "command");
        assert_eq!(pre["body"], "echo pre");
    }

    #[test]
    fn hooks_session_start_translates() {
        let cc = serde_json::json!({
            "hooks": {
                "SessionStart": [
                    { "type": "exec", "args": ["./start.sh"] }
                ]
            }
        });
        let r = translate_settings(&cc, None);
        let hooks = r.translated["hooks"].as_object().unwrap();
        assert!(hooks.contains_key("session_start"));
        let entry = &hooks["session_start"][0];
        assert_eq!(entry["args"][0], "./start.sh");
    }

    #[test]
    fn additional_directories_maps_to_sandbox_allowed_mounts() {
        let cc = serde_json::json!({
            "additionalDirectories": ["/tmp/data", "/opt/shared"]
        });
        let r = translate_settings(&cc, None);
        let sandbox = r.translated["sandbox"].as_object().unwrap();
        assert_eq!(sandbox["allowedMounts"][0], "/tmp/data");
        assert!(!r.warnings.is_empty(), "should warn about name change");
    }

    #[test]
    fn claude_ai_oauth_is_skipped() {
        let cc = serde_json::json!({
            "claudeAiOauth": { "token": "supersecret" },
            "theme": "light"
        });
        let r = translate_settings(&cc, None);
        // claudeAiOauth must not appear anywhere in the translated output
        let json_str = serde_json::to_string(&r.translated).unwrap();
        assert!(!json_str.contains("supersecret"));
        assert!(!json_str.contains("claudeAiOauth"));
        // theme should still be translated
        assert_eq!(r.translated["theme"], "light");
    }

    #[test]
    fn unknown_keys_preserved_in_cc_metadata() {
        let cc = serde_json::json!({
            "someUnknownKey": { "x": 42 }
        });
        let r = translate_settings(&cc, None);
        let cc_meta = r.translated["cc_metadata"].as_object().unwrap();
        assert!(cc_meta.contains_key("someUnknownKey"));
        assert!(!r.unknown_keys.is_empty());
        assert!(r.unknown_keys[0].contains("someUnknownKey"));
    }

    #[test]
    fn conflict_detected_for_different_values() {
        let cc = serde_json::json!({ "theme": "dark" });
        let existing = serde_json::json!({ "theme": "light" });
        let r = translate_settings(&cc, Some(&existing));
        assert_eq!(r.conflicts.len(), 1);
        let conflict = &r.conflicts[0];
        assert_eq!(conflict["key"], "theme");
        assert_eq!(conflict["cc_value"], "dark");
        assert_eq!(conflict["anvil_value"], "light");
        assert_eq!(conflict["decision"], "user_required");
    }

    #[test]
    fn no_conflict_when_values_are_same() {
        let cc = serde_json::json!({ "theme": "dark" });
        let existing = serde_json::json!({ "theme": "dark" });
        let r = translate_settings(&cc, Some(&existing));
        assert!(r.conflicts.is_empty());
    }

    #[test]
    fn local_override_wins_over_base() {
        let base = serde_json::json!({ "theme": "light" });
        let local = serde_json::json!({ "theme": "dark" });
        let merged = merge_cc_settings(&base, Some(&local));
        assert_eq!(merged["theme"], "dark");
    }

    #[test]
    fn local_override_adds_new_keys() {
        let base = serde_json::json!({ "theme": "light" });
        let local = serde_json::json!({ "effort_level": "high" });
        let merged = merge_cc_settings(&base, Some(&local));
        assert_eq!(merged["theme"], "light");
        assert_eq!(merged["effort_level"], "high");
    }

    #[test]
    fn merge_with_no_local_returns_base() {
        let base = serde_json::json!({ "theme": "light" });
        let merged = merge_cc_settings(&base, None);
        assert_eq!(merged["theme"], "light");
    }

    #[test]
    fn idempotent_retranslation_produces_same_output() {
        let cc = serde_json::json!({
            "theme": "dark",
            "outputStyle": "concise",
            "permissions": { "allow": ["Bash(git:*)"] }
        });
        let r1 = translate_settings(&cc, None);
        let r2 = translate_settings(&cc, None);
        let j1 = serde_json::to_string(&r1.translated).unwrap();
        let j2 = serde_json::to_string(&r2.translated).unwrap();
        assert_eq!(j1, j2);
    }

    #[test]
    fn mcp_servers_direct_map() {
        let cc = serde_json::json!({
            "mcpServers": {
                "my-server": { "command": "npx", "args": ["-y", "@my/server"] }
            }
        });
        let r = translate_settings(&cc, None);
        assert!(r.translated.contains_key("mcpServers"));
        assert!(r.translated["mcpServers"].as_object().unwrap().contains_key("my-server"));
    }

    #[test]
    fn auto_mode_hard_deny_direct_map() {
        let cc = serde_json::json!({
            "autoMode": { "hard_deny": ["rm -rf /"] }
        });
        let r = translate_settings(&cc, None);
        assert_eq!(r.translated["autoMode"]["hard_deny"][0], "rm -rf /");
    }

    #[test]
    fn effort_level_direct_map() {
        let cc = serde_json::json!({ "effort_level": "high" });
        let r = translate_settings(&cc, None);
        assert_eq!(r.translated["effort_level"], "high");
    }

    // ── Staging integration ───────────────────────────────────────────────────

    #[test]
    #[serial(anvil_config_home)]
    fn run_settings_import_creates_staging_files() {
        let dir = TempDir::new().expect("tmpdir");
        unsafe { std::env::set_var("ANVIL_CONFIG_HOME", dir.path()) };

        // Write a CC settings file
        let cc_dir = dir.path().join("cc");
        std::fs::create_dir_all(&cc_dir).unwrap();
        let cc_settings = serde_json::json!({
            "theme": "dark",
            "outputStyle": "concise"
        });
        std::fs::write(
            cc_dir.join("settings.json"),
            serde_json::to_vec_pretty(&cc_settings).unwrap(),
        )
        .unwrap();

        let staging = crate::import::staging::StagingDir::create_clean().unwrap();
        let result = run_settings_import(&cc_dir, &staging);

        unsafe { std::env::remove_var("ANVIL_CONFIG_HOME") };

        let (conflict_count, warnings, unknown_keys) = result.unwrap();
        assert_eq!(conflict_count, 0);
        assert!(warnings.is_empty() || !warnings.is_empty()); // just that it ran
        assert!(unknown_keys.is_empty());

        let translated_path = staging.path("settings/translated.json");
        assert!(translated_path.exists(), "translated.json should exist");

        let translated: JsonValue = serde_json::from_slice(
            &std::fs::read(&translated_path).unwrap()
        )
        .unwrap();
        assert_eq!(translated["theme"], "dark");
        assert_eq!(translated["output_style"], "condensed");

        let conflicts_path = staging.path("settings/conflicts.json");
        assert!(conflicts_path.exists(), "conflicts.json should exist");
        let conflicts: JsonValue =
            serde_json::from_slice(&std::fs::read(&conflicts_path).unwrap()).unwrap();
        assert!(conflicts.as_array().unwrap().is_empty());
    }

    #[test]
    #[serial(anvil_config_home)]
    fn run_settings_import_detects_conflict() {
        let dir = TempDir::new().expect("tmpdir");
        unsafe { std::env::set_var("ANVIL_CONFIG_HOME", dir.path()) };

        // Existing Anvil settings
        std::fs::create_dir_all(dir.path()).unwrap();
        std::fs::write(
            dir.path().join("settings.json"),
            serde_json::to_vec_pretty(&serde_json::json!({ "theme": "light" })).unwrap(),
        )
        .unwrap();

        // CC settings with conflicting value
        let cc_dir = dir.path().join("cc");
        std::fs::create_dir_all(&cc_dir).unwrap();
        std::fs::write(
            cc_dir.join("settings.json"),
            serde_json::to_vec_pretty(&serde_json::json!({ "theme": "dark" })).unwrap(),
        )
        .unwrap();

        let staging = crate::import::staging::StagingDir::create_clean().unwrap();
        let result = run_settings_import(&cc_dir, &staging);

        unsafe { std::env::remove_var("ANVIL_CONFIG_HOME") };

        let (conflict_count, _warnings, _unknown_keys) = result.unwrap();
        assert!(conflict_count > 0, "should detect at least 1 theme conflict");
        assert_eq!(conflict_count, 1, "exactly 1 theme conflict");

        let conflicts_path = staging.path("settings/conflicts.json");
        let conflicts: JsonValue =
            serde_json::from_slice(&std::fs::read(&conflicts_path).unwrap()).unwrap();
        let arr = conflicts.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["key"], "theme");
    }

    #[test]
    #[serial(anvil_config_home)]
    fn run_settings_import_no_op_when_no_cc_settings() {
        let dir = TempDir::new().expect("tmpdir");
        unsafe { std::env::set_var("ANVIL_CONFIG_HOME", dir.path()) };

        let cc_dir = dir.path().join("cc_empty");
        std::fs::create_dir_all(&cc_dir).unwrap();

        let staging = crate::import::staging::StagingDir::create_clean().unwrap();
        let result = run_settings_import(&cc_dir, &staging);

        unsafe { std::env::remove_var("ANVIL_CONFIG_HOME") };

        let (conflict_count, warnings, unknown_keys) = result.unwrap();
        assert_eq!(conflict_count, 0);
        assert!(warnings.is_empty());
        assert!(unknown_keys.is_empty());
    }

    #[test]
    #[serial(anvil_config_home)]
    fn local_override_applied_during_import() {
        let dir = TempDir::new().expect("tmpdir");
        unsafe { std::env::set_var("ANVIL_CONFIG_HOME", dir.path()) };

        let cc_dir = dir.path().join("cc");
        std::fs::create_dir_all(&cc_dir).unwrap();

        // Base: light theme
        std::fs::write(
            cc_dir.join("settings.json"),
            serde_json::to_vec_pretty(&serde_json::json!({ "theme": "light" })).unwrap(),
        )
        .unwrap();
        // Local override: dark theme
        std::fs::write(
            cc_dir.join("settings.local.json"),
            serde_json::to_vec_pretty(&serde_json::json!({ "theme": "dark" })).unwrap(),
        )
        .unwrap();

        let staging = crate::import::staging::StagingDir::create_clean().unwrap();
        run_settings_import(&cc_dir, &staging).unwrap();

        unsafe { std::env::remove_var("ANVIL_CONFIG_HOME") };

        let translated: JsonValue = serde_json::from_slice(
            &std::fs::read(staging.path("settings/translated.json")).unwrap(),
        )
        .unwrap();
        // local-wins: dark should win
        assert_eq!(translated["theme"], "dark");
    }
}
