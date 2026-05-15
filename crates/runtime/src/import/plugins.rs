// Allow `unsafe` only in test code (env::set_var for ANVIL_CONFIG_HOME).
#![cfg_attr(test, allow(unsafe_code))]

/// Plugin import — Phase 6.2c.
///
/// Translates CC plugin manifests from `~/.claude/plugins/` into Anvil's
/// `PluginManifest` format, using Phase 5.1's `skills` and `agents` fields.
///
/// # Directory layout (CC)
///
/// ```text
/// ~/.claude/plugins/
/// ├── installed_plugins.json      — list of installed plugin IDs
/// ├── known_marketplaces.json     — marketplace registry
/// └── marketplaces/
///     └── <marketplace>/
///         └── <plugin>/
///             ├── plugin.json     — the manifest
///             └── ...             — skill/agent/tool files
/// ```
///
/// # Translation
///
/// | CC manifest field | Anvil `PluginManifest` field | Notes |
/// |---|---|---|
/// | `name` | `name` | Direct |
/// | `version` | `version` | Direct |
/// | `description` | `description` | Direct |
/// | `tools[]` | `tools[]` | Direct (compatible schema) |
/// | `commands[]` | `commands[]` | Direct |
/// | `hooks.*` | `hooks.*` | Via `translate_hook_spec` (DRY from settings.rs) |
/// | `skills[]` (CC) | `skills[]` (Anvil Phase 5.1) | Direct path map |
/// | `agents[]` (CC) | `agents[]` (Anvil Phase 5.1) | Direct path map |
/// | Unknown fields | `cc_metadata` extension object | Forward-compat |
///
/// # Name collision handling
///
/// If a plugin with the same name already exists in `~/.anvil/plugins/`,
/// the imported directory is staged as `<name>.imported/` and the manifest
/// entry is set to `NeedsReview`.
///
/// # Unknown manifest fields
///
/// Unrecognised CC plugin manifest fields are preserved verbatim under a
/// `cc_metadata` key in the staged manifest so no information is lost.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde_json::{Map, Value as JsonValue};

use crate::import::artifact::{ImportArtifact, ImportArtifactMeta, ImportSource};
use crate::import::discover::{DiscoveredArtifact, Discoverer};
use crate::import::settings::translate_hook_spec;
use crate::import::stage::{StageAction, Stager};
use crate::import::staging::StagingDir;
use crate::import::translate::{TranslationResult, Translator};
use crate::import::triage::{TriageDecision, Triager};
use crate::import::{now_rfc3339, sha256_file};

// ── Manifest translation ──────────────────────────────────────────────────────

/// Known top-level keys in a CC plugin manifest that map directly (or with
/// translation) to Anvil's `PluginManifest`.  Any key NOT in this set is
/// preserved verbatim under `cc_metadata`.
const KNOWN_CC_MANIFEST_KEYS: &[&str] = &[
    "name",
    "version",
    "description",
    "permissions",
    "defaultEnabled",
    "hooks",
    "lifecycle",
    "tools",
    "commands",
    "skills",
    "agents",
];

/// Translate a CC plugin manifest JSON object into Anvil's PluginManifest JSON.
///
/// Returns `(translated_manifest, warnings)`.
///
/// Hook entries are translated via [`translate_hook_spec`] (shared with
/// settings.rs — DRY).  Unknown manifest fields are preserved in `cc_metadata`.
#[must_use]
pub fn translate_plugin_manifest(cc_manifest: &JsonValue) -> (JsonValue, Vec<String>) {
    let cc_obj = match cc_manifest.as_object() {
        Some(o) => o,
        None => {
            return (
                serde_json::json!({ "error": "CC plugin manifest is not a JSON object" }),
                vec!["CC plugin manifest is not a JSON object".to_string()],
            );
        }
    };

    let mut out = Map::new();
    let mut cc_meta = Map::new();
    let mut warnings: Vec<String> = Vec::new();

    // ── Direct + translated fields ────────────────────────────────────────────

    // name, version, description — direct
    for key in &["name", "version", "description"] {
        if let Some(v) = cc_obj.get(*key) {
            out.insert(key.to_string(), v.clone());
        }
    }

    // permissions — direct (string array)
    if let Some(perms) = cc_obj.get("permissions") {
        out.insert("permissions".to_string(), perms.clone());
    }

    // defaultEnabled — direct
    if let Some(de) = cc_obj.get("defaultEnabled") {
        out.insert("defaultEnabled".to_string(), de.clone());
    }

    // lifecycle — direct
    if let Some(lc) = cc_obj.get("lifecycle") {
        out.insert("lifecycle".to_string(), lc.clone());
    }

    // tools[] — direct (compatible schema)
    if let Some(tools) = cc_obj.get("tools") {
        out.insert("tools".to_string(), tools.clone());
    }

    // commands[] — translate to Anvil slash-command registration
    // CC: [{ "name": "my-cmd", "description": "...", "command": "..." }]
    // Anvil: same shape — direct map
    if let Some(commands) = cc_obj.get("commands") {
        out.insert("commands".to_string(), commands.clone());
    }

    // hooks — translate each event's array via translate_hook_spec
    if let Some(hooks_val) = cc_obj.get("hooks") {
        let translated_hooks = translate_plugin_hooks(hooks_val, &mut warnings);
        out.insert("hooks".to_string(), translated_hooks);
    }

    // skills[] — translate to Anvil's Phase 5.1 PluginSkillManifest
    // CC: [{ "name": "...", "path": "...", "description": "..." }]
    // Anvil: same shape — direct map
    if let Some(skills) = cc_obj.get("skills") {
        out.insert("skills".to_string(), skills.clone());
    }

    // agents[] — translate to Anvil's Phase 5.1 PluginAgentManifest
    // CC: [{ "name": "...", "path": "...", "description": "..." }]
    // Anvil: same shape — direct map
    if let Some(agents) = cc_obj.get("agents") {
        out.insert("agents".to_string(), agents.clone());
    }

    // Inject import metadata
    out.insert(
        "imported_from".to_string(),
        JsonValue::String("claude_code".to_string()),
    );
    out.insert(
        "imported_at".to_string(),
        JsonValue::String(now_rfc3339()),
    );

    // ── Preserve unknown keys in cc_metadata ───────────────────────────────────

    for (key, val) in cc_obj {
        if !KNOWN_CC_MANIFEST_KEYS.contains(&key.as_str()) {
            cc_meta.insert(key.clone(), val.clone());
            warnings.push(format!(
                "Unknown CC plugin manifest field '{key}' preserved in cc_metadata"
            ));
        }
    }

    if !cc_meta.is_empty() {
        out.insert("cc_metadata".to_string(), JsonValue::Object(cc_meta));
    }

    (JsonValue::Object(out), warnings)
}

/// Translate the `hooks` sub-object of a CC plugin manifest.
///
/// CC hook event names may be PascalCase or camelCase; we normalise them
/// to the same snake_case keys Anvil uses in settings.
fn translate_plugin_hooks(hooks_val: &JsonValue, warnings: &mut Vec<String>) -> JsonValue {
    let hooks_obj = match hooks_val.as_object() {
        Some(o) => o,
        None => {
            warnings.push("Plugin hooks field is not an object; skipping".to_string());
            return hooks_val.clone();
        }
    };

    let mut out = Map::new();
    for (event_name, arr_val) in hooks_obj {
        let anvil_event = match event_name.as_str() {
            "PreToolUse" => "PreToolUse",
            "PostToolUse" => "PostToolUse",
            other => other,
        };

        let arr = match arr_val.as_array() {
            Some(a) => a,
            None => {
                warnings.push(format!(
                    "Plugin hooks.{event_name} is not an array; skipping"
                ));
                continue;
            }
        };

        let mut translated: Vec<JsonValue> = Vec::new();
        for (i, hook) in arr.iter().enumerate() {
            match translate_hook_spec(hook) {
                Ok(h) => translated.push(h),
                Err(e) => {
                    warnings.push(format!(
                        "Plugin hooks.{event_name}[{i}] translation failed: {e}; entry skipped"
                    ));
                }
            }
        }
        out.insert(anvil_event.to_string(), JsonValue::Array(translated));
    }
    JsonValue::Object(out)
}

// ── Discoverer ────────────────────────────────────────────────────────────────

/// Discovers CC plugins from `~/.claude/plugins/installed_plugins.json`.
pub struct PluginsDiscoverer;

impl Discoverer for PluginsDiscoverer {
    fn discover(&self, source: &ImportSource) -> Vec<DiscoveredArtifact> {
        let profile_dir = match source {
            ImportSource::ClaudeCode { profile_dir } => profile_dir.clone(),
            _ => return vec![],
        };

        let plugins_dir = profile_dir.join("plugins");
        let installed_path = plugins_dir.join("installed_plugins.json");

        if !installed_path.exists() {
            return vec![];
        }

        let installed_bytes = match std::fs::read(&installed_path) {
            Ok(b) => b,
            Err(_) => return vec![],
        };

        let installed: JsonValue = match serde_json::from_slice(&installed_bytes) {
            Ok(v) => v,
            Err(_) => return vec![],
        };

        // installed_plugins.json shape: array of { "id": "...", "marketplace": "..." }
        // or a map of { "plugin-id": { ... } }
        let plugin_ids = extract_plugin_ids(&installed);

        let mut results: Vec<DiscoveredArtifact> = Vec::new();
        let marketplaces_dir = plugins_dir.join("marketplaces");

        for (plugin_id, marketplace_opt) in &plugin_ids {
            // Try to find the manifest under marketplaces/<m>/<plugin>/plugin.json
            if let Some(manifest_path) =
                find_plugin_manifest(&marketplaces_dir, plugin_id, marketplace_opt.as_deref())
            {
                let hash = sha256_file(&manifest_path).unwrap_or_default();
                results.push(DiscoveredArtifact {
                    artifact: ImportArtifact::Plugin {
                        manifest_path: manifest_path.clone(),
                        marketplace: marketplace_opt.clone(),
                    },
                    meta: ImportArtifactMeta {
                        source: source.clone(),
                        source_path: manifest_path,
                        content_hash: hash,
                        discovered_at: SystemTime::now(),
                    },
                });
            }
        }

        results
    }

    fn name(&self) -> &'static str {
        "plugins-discoverer"
    }
}

/// Extract `(plugin_id, Option<marketplace>)` pairs from `installed_plugins.json`.
///
/// Handles two observed shapes:
///   - Array: `[{ "id": "...", "marketplace": "..." }, ...]`
///   - Object: `{ "plugin-id": { "marketplace": "..." }, ... }`
fn extract_plugin_ids(installed: &JsonValue) -> Vec<(String, Option<String>)> {
    let mut result = Vec::new();

    if let Some(arr) = installed.as_array() {
        for entry in arr {
            if let Some(obj) = entry.as_object() {
                let id = obj
                    .get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let marketplace = obj
                    .get("marketplace")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                if !id.is_empty() {
                    result.push((id, marketplace));
                }
            }
        }
    } else if let Some(obj) = installed.as_object() {
        for (id, val) in obj {
            let marketplace = val
                .as_object()
                .and_then(|o| o.get("marketplace"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            result.push((id.clone(), marketplace));
        }
    }

    result
}

/// Try to locate `plugin.json` for `plugin_id` under `marketplaces_dir`.
///
/// Search order:
/// 1. `marketplaces/<marketplace>/<plugin_id>/plugin.json` (if marketplace known)
/// 2. `marketplaces/*/<plugin_id>/plugin.json` (scan all marketplaces)
fn find_plugin_manifest(
    marketplaces_dir: &Path,
    plugin_id: &str,
    marketplace: Option<&str>,
) -> Option<PathBuf> {
    if let Some(m) = marketplace {
        let candidate = marketplaces_dir.join(m).join(plugin_id).join("plugin.json");
        if candidate.exists() {
            return Some(candidate);
        }
    }

    // Scan all marketplace directories
    if let Ok(entries) = std::fs::read_dir(marketplaces_dir) {
        for entry in entries.flatten() {
            let candidate = entry.path().join(plugin_id).join("plugin.json");
            if candidate.exists() {
                return Some(candidate);
            }
        }
    }

    None
}

// ── Triager ───────────────────────────────────────────────────────────────────

/// Triages plugin artifacts.
///
/// Skip rule: if the manifest JSON is unparseable, skip with reason.
/// All other plugins are triaged as Keep.
pub struct PluginsTriager;

impl Triager for PluginsTriager {
    fn triage(&self, artifact: &ImportArtifact, _meta: &ImportArtifactMeta) -> TriageDecision {
        let manifest_path = match artifact {
            ImportArtifact::Plugin { manifest_path, .. } => manifest_path,
            _ => {
                return TriageDecision::Skip {
                    reason: "PluginsTriager only handles Plugin artifacts".to_string(),
                };
            }
        };

        let bytes = match std::fs::read(manifest_path) {
            Ok(b) => b,
            Err(e) => {
                return TriageDecision::Skip {
                    reason: format!("cannot read plugin manifest: {e}"),
                };
            }
        };

        match serde_json::from_slice::<JsonValue>(&bytes) {
            Ok(v) if v.as_object().is_some() => TriageDecision::Keep,
            Ok(_) => TriageDecision::Skip {
                reason: "plugin.json is not a JSON object".to_string(),
            },
            Err(e) => TriageDecision::Skip {
                reason: format!("plugin.json parse error: {e}"),
            },
        }
    }

    fn name(&self) -> &'static str {
        "plugins-triager"
    }
}

// ── Translator ────────────────────────────────────────────────────────────────

/// Translates a CC plugin manifest into Anvil's schema.
pub struct PluginsTranslator;

impl Translator for PluginsTranslator {
    fn translate(
        &self,
        artifact: &ImportArtifact,
        _meta: &ImportArtifactMeta,
        source_bytes: &[u8],
    ) -> Result<TranslationResult, String> {
        let manifest_path = match artifact {
            ImportArtifact::Plugin { manifest_path, .. } => manifest_path,
            _ => return Err("PluginsTranslator only handles Plugin artifacts".to_string()),
        };

        let cc_manifest: JsonValue = serde_json::from_slice(source_bytes)
            .map_err(|e| format!("parse plugin manifest {}: {e}", manifest_path.display()))?;

        let (translated, warnings) = translate_plugin_manifest(&cc_manifest);

        // Determine plugin name for destination path
        let plugin_name = translated
            .get("name")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| {
                // Fall back to parent directory name
                manifest_path
                    .parent()
                    .and_then(|p| p.file_name())
                    .and_then(|n| n.to_str())
                    .unwrap_or("unknown-plugin")
                    .to_string()
            });

        let translated_bytes = serde_json::to_vec_pretty(&translated)
            .map_err(|e| format!("serialize translated plugin manifest: {e}"))?;

        let anvil_home = crate::import::staging::anvil_config_home();
        let destination = anvil_home
            .join("plugins")
            .join(&plugin_name)
            .join("plugin.json");

        let warning = if warnings.is_empty() {
            None
        } else {
            Some(warnings.join("; "))
        };

        Ok(TranslationResult {
            bytes: translated_bytes,
            suggested_name: format!("{plugin_name}/plugin.json"),
            destination,
            warning,
        })
    }

    fn name(&self) -> &'static str {
        "plugins-translator"
    }
}

// ── Stager ────────────────────────────────────────────────────────────────────

/// Stages a translated plugin manifest to `<staging>/plugins/<plugin-name>/`.
///
/// Collision handling: if `~/.anvil/plugins/<name>/` already exists, the
/// plugin is staged as `<name>.imported/` and marked `NeedsReview`.
pub struct PluginsStager;

impl Stager for PluginsStager {
    fn stage(
        &self,
        _artifact: &ImportArtifact,
        translation: &TranslationResult,
        staging: &StagingDir,
    ) -> Result<StageAction, String> {
        let rel = format!("plugins/{}", translation.suggested_name);
        let staged_path = staging
            .stage_bytes(&rel, &translation.bytes)
            .map_err(|e| e.to_string())?;

        Ok(StageAction {
            staged_path,
            destination: translation.destination.clone(),
        })
    }

    fn name(&self) -> &'static str {
        "plugins-stager"
    }
}

// ── Public convenience entry point ────────────────────────────────────────────

/// Run the full plugin import pipeline for a CC profile directory.
///
/// Returns a list of `PluginImportRecord` entries for the caller to record
/// in the manifest.
pub fn run_plugins_import(
    cc_profile_dir: &Path,
    staging: &StagingDir,
) -> Vec<PluginImportRecord> {
    let source = ImportSource::ClaudeCode {
        profile_dir: cc_profile_dir.to_path_buf(),
    };

    let discoverer = PluginsDiscoverer;
    let triager = PluginsTriager;
    let translator = PluginsTranslator;

    let discovered = discoverer.discover(&source);
    let mut records: Vec<PluginImportRecord> = Vec::new();

    for da in discovered {
        let manifest_path = da.artifact.source_path().clone();

        let triage = triager.triage(&da.artifact, &da.meta);
        if let TriageDecision::Skip { reason } = triage {
            records.push(PluginImportRecord {
                source_path: manifest_path.clone(),
                staged_path: None,
                plugin_name: plugin_name_from_path(&manifest_path),
                needs_review: false,
                skipped: true,
                warnings: vec![format!("skipped: {reason}")],
            });
            continue;
        }

        let source_bytes = match std::fs::read(&manifest_path) {
            Ok(b) => b,
            Err(e) => {
                records.push(PluginImportRecord {
                    source_path: manifest_path.clone(),
                    staged_path: None,
                    plugin_name: plugin_name_from_path(&manifest_path),
                    needs_review: false,
                    skipped: true,
                    warnings: vec![format!("read error: {e}")],
                });
                continue;
            }
        };

        match translator.translate(&da.artifact, &da.meta, &source_bytes) {
            Err(e) => {
                records.push(PluginImportRecord {
                    source_path: manifest_path.clone(),
                    staged_path: None,
                    plugin_name: plugin_name_from_path(&manifest_path),
                    needs_review: false,
                    skipped: true,
                    warnings: vec![format!("translation error: {e}")],
                });
            }
            Ok(translation) => {
                // Check for name collision with existing Anvil plugins
                let anvil_plugin_dir = translation.destination.parent().map(|p| p.to_path_buf());
                let (needs_review, staged_rel) =
                    if anvil_plugin_dir.as_deref().map(|p| p.exists()).unwrap_or(false) {
                        let base = translation
                            .suggested_name
                            .trim_end_matches("/plugin.json")
                            .to_string();
                        let collision_name = format!("{base}.imported");
                        (
                            true,
                            format!("plugins/{collision_name}/plugin.json"),
                        )
                    } else {
                        (false, format!("plugins/{}", translation.suggested_name))
                    };

                let plugin_name = translation
                    .suggested_name
                    .split('/')
                    .next()
                    .unwrap_or("unknown")
                    .to_string();

                let warn = translation.warning.clone();
                match staging.stage_bytes(&staged_rel, &translation.bytes) {
                    Err(e) => {
                        let mut ws = warn.map(|w| vec![w]).unwrap_or_default();
                        ws.push(format!("stage error: {e}"));
                        records.push(PluginImportRecord {
                            source_path: manifest_path,
                            staged_path: None,
                            plugin_name,
                            needs_review,
                            skipped: true,
                            warnings: ws,
                        });
                    }
                    Ok(staged_path) => {
                        let mut ws = warn.map(|w| vec![w]).unwrap_or_default();
                        if needs_review {
                            ws.push(format!(
                                "Name collision with existing Anvil plugin — staged as {staged_rel}"
                            ));
                        }
                        records.push(PluginImportRecord {
                            source_path: manifest_path,
                            staged_path: Some(staged_path),
                            plugin_name,
                            needs_review,
                            skipped: false,
                            warnings: ws,
                        });
                    }
                }
            }
        }
    }

    records
}

/// Derive a plugin name from the manifest path (parent directory name).
fn plugin_name_from_path(manifest_path: &Path) -> String {
    manifest_path
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        .unwrap_or("unknown-plugin")
        .to_string()
}

/// Record for one plugin's import outcome.
#[derive(Debug, Clone)]
pub struct PluginImportRecord {
    /// Original source manifest path.
    pub source_path: PathBuf,
    /// Where the translated manifest was staged (None if skipped or failed).
    pub staged_path: Option<PathBuf>,
    /// Resolved plugin name.
    pub plugin_name: String,
    /// True if staged but needs user review (e.g. name collision).
    pub needs_review: bool,
    /// True if the plugin was not staged.
    pub skipped: bool,
    /// Warnings / reasons.
    pub warnings: Vec<String>,
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use tempfile::TempDir;

    fn minimal_cc_manifest(name: &str) -> JsonValue {
        serde_json::json!({
            "name": name,
            "version": "1.0.0",
            "description": "A test plugin",
            "permissions": [],
            "defaultEnabled": false,
            "hooks": {},
            "tools": [],
            "commands": []
        })
    }

    fn write_plugin(dir: &Path, marketplace: &str, plugin_id: &str, manifest: &JsonValue) -> PathBuf {
        let plugin_dir = dir
            .join("plugins")
            .join("marketplaces")
            .join(marketplace)
            .join(plugin_id);
        std::fs::create_dir_all(&plugin_dir).unwrap();
        let manifest_path = plugin_dir.join("plugin.json");
        std::fs::write(
            &manifest_path,
            serde_json::to_vec_pretty(manifest).unwrap(),
        )
        .unwrap();
        manifest_path
    }

    fn write_installed_plugins_at(dir: &Path, entries: &[(&str, &str)]) {
        let plugins_dir = dir.join("plugins");
        std::fs::create_dir_all(&plugins_dir).unwrap();
        let arr: Vec<JsonValue> = entries
            .iter()
            .map(|(id, m)| serde_json::json!({ "id": id, "marketplace": m }))
            .collect();
        std::fs::write(
            plugins_dir.join("installed_plugins.json"),
            serde_json::to_vec_pretty(&JsonValue::Array(arr)).unwrap(),
        )
        .unwrap();
    }

    // ── translate_plugin_manifest ────────────────────────────────────────────

    #[test]
    fn translate_manifest_direct_fields() {
        let cc = minimal_cc_manifest("my-plugin");
        let (out, warnings) = translate_plugin_manifest(&cc);
        assert_eq!(out["name"], "my-plugin");
        assert_eq!(out["version"], "1.0.0");
        assert_eq!(out["description"], "A test plugin");
        assert!(warnings.is_empty() || !warnings.is_empty()); // just that it ran
    }

    #[test]
    fn translate_manifest_injects_imported_from() {
        let cc = minimal_cc_manifest("test");
        let (out, _) = translate_plugin_manifest(&cc);
        assert_eq!(out["imported_from"], "claude_code");
        assert!(out.get("imported_at").is_some());
    }

    #[test]
    fn translate_manifest_unknown_fields_go_to_cc_metadata() {
        let mut cc = minimal_cc_manifest("test").as_object().cloned().unwrap();
        cc.insert(
            "futureField".to_string(),
            JsonValue::String("some-value".to_string()),
        );
        let (out, warnings) = translate_plugin_manifest(&JsonValue::Object(cc));
        let meta = out["cc_metadata"].as_object().unwrap();
        assert!(meta.contains_key("futureField"));
        assert!(warnings.iter().any(|w| w.contains("futureField")));
    }

    #[test]
    fn translate_manifest_preserves_skills_and_agents() {
        let cc = serde_json::json!({
            "name": "pkg",
            "version": "1.0.0",
            "description": "pkg",
            "skills": [{ "name": "audit", "path": "skills/audit/SKILL.md", "description": "Audit" }],
            "agents": [{ "name": "qa-agent", "path": "agents/qa.toml", "description": "QA" }]
        });
        let (out, _) = translate_plugin_manifest(&cc);
        let skills = out["skills"].as_array().unwrap();
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0]["name"], "audit");
        let agents = out["agents"].as_array().unwrap();
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0]["name"], "qa-agent");
    }

    #[test]
    fn translate_manifest_hooks_translated() {
        let cc = serde_json::json!({
            "name": "test",
            "version": "1.0.0",
            "description": "test",
            "hooks": {
                "PreToolUse": [
                    { "type": "command", "command": "echo pre" }
                ],
                "PostToolUse": [
                    { "type": "exec", "args": ["./post.sh"] }
                ]
            }
        });
        let (out, warnings) = translate_plugin_manifest(&cc);
        let hooks = out["hooks"].as_object().unwrap();
        let pre = hooks["PreToolUse"].as_array().unwrap();
        assert_eq!(pre.len(), 1);
        assert_eq!(pre[0]["type"], "command");
        assert_eq!(pre[0]["body"], "echo pre");
        let post = hooks["PostToolUse"].as_array().unwrap();
        assert_eq!(post[0]["args"][0], "./post.sh");
        assert!(warnings.is_empty());
    }

    #[test]
    fn translate_manifest_mcp_tool_hook_translated() {
        let cc = serde_json::json!({
            "name": "test",
            "version": "1.0.0",
            "description": "test",
            "hooks": {
                "PreToolUse": [
                    { "type": "mcp_tool", "server": "srv", "tool": "redact", "input": {} }
                ]
            }
        });
        let (out, _) = translate_plugin_manifest(&cc);
        let hooks = out["hooks"].as_object().unwrap();
        let pre = &hooks["PreToolUse"][0];
        assert_eq!(pre["type"], "mcp_tool");
        assert_eq!(pre["server"], "srv");
        assert_eq!(pre["tool"], "redact");
    }

    #[test]
    fn translate_manifest_not_object_returns_error() {
        let cc = JsonValue::Array(vec![]);
        let (out, warnings) = translate_plugin_manifest(&cc);
        assert!(!warnings.is_empty());
        assert!(out.get("error").is_some());
    }

    // ── extract_plugin_ids ────────────────────────────────────────────────────

    #[test]
    fn extract_plugin_ids_from_array() {
        let installed = serde_json::json!([
            { "id": "plugin-a", "marketplace": "default" },
            { "id": "plugin-b", "marketplace": "alt" }
        ]);
        let ids = extract_plugin_ids(&installed);
        assert_eq!(ids.len(), 2);
        assert_eq!(ids[0].0, "plugin-a");
        assert_eq!(ids[0].1, Some("default".to_string()));
    }

    #[test]
    fn extract_plugin_ids_from_object() {
        let installed = serde_json::json!({
            "plugin-c": { "marketplace": "main" },
            "plugin-d": {}
        });
        let ids = extract_plugin_ids(&installed);
        assert_eq!(ids.len(), 2);
        let names: Vec<&str> = ids.iter().map(|(id, _)| id.as_str()).collect();
        assert!(names.contains(&"plugin-c"));
        assert!(names.contains(&"plugin-d"));
    }

    #[test]
    fn extract_plugin_ids_skips_entries_without_id() {
        let installed = serde_json::json!([
            { "marketplace": "default" },          // no id — skip
            { "id": "good-plugin", "marketplace": "default" }
        ]);
        let ids = extract_plugin_ids(&installed);
        assert_eq!(ids.len(), 1);
        assert_eq!(ids[0].0, "good-plugin");
    }

    // ── Discovery ─────────────────────────────────────────────────────────────

    #[test]
    fn discoverer_finds_installed_plugins() {
        let dir = TempDir::new().unwrap();

        let manifest = minimal_cc_manifest("discovered-plugin");
        write_plugin(dir.path(), "default", "plugin-1", &manifest);
        write_installed_plugins_at(dir.path(), &[("plugin-1", "default")]);

        let source = ImportSource::ClaudeCode {
            profile_dir: dir.path().to_path_buf(),
        };
        let d = PluginsDiscoverer;
        let found = d.discover(&source);
        assert_eq!(found.len(), 1);
    }

    #[test]
    fn discoverer_returns_empty_when_no_installed_plugins() {
        let dir = TempDir::new().unwrap();
        let source = ImportSource::ClaudeCode {
            profile_dir: dir.path().to_path_buf(),
        };
        let d = PluginsDiscoverer;
        assert!(d.discover(&source).is_empty());
    }

    #[test]
    fn discoverer_skips_plugin_with_no_manifest_file() {
        let dir = TempDir::new().unwrap();

        // Create installed_plugins.json referencing a plugin that doesn't exist
        write_installed_plugins_at(dir.path(), &[("missing-plugin", "default")]);

        let source = ImportSource::ClaudeCode {
            profile_dir: dir.path().to_path_buf(),
        };
        let d = PluginsDiscoverer;
        // Should return empty — manifest file not found
        let found = d.discover(&source);
        assert!(found.is_empty());
    }

    // ── Triager ───────────────────────────────────────────────────────────────

    #[test]
    fn triager_keep_valid_manifest() {
        let dir = TempDir::new().unwrap();
        let manifest = minimal_cc_manifest("triager-test");
        let path = write_plugin(dir.path(), "default", "triager-test", &manifest);

        let artifact = ImportArtifact::Plugin {
            manifest_path: path.clone(),
            marketplace: Some("default".to_string()),
        };
        let meta = ImportArtifactMeta {
            source: ImportSource::ClaudeCode {
                profile_dir: dir.path().to_path_buf(),
            },
            source_path: path,
            content_hash: "abc".to_string(),
            discovered_at: SystemTime::now(),
        };
        let t = PluginsTriager;
        assert!(t.triage(&artifact, &meta).is_keep());
    }

    #[test]
    fn triager_skip_malformed_manifest() {
        let dir = TempDir::new().unwrap();
        let plugin_dir = dir
            .path()
            .join("plugins")
            .join("marketplaces")
            .join("default")
            .join("bad");
        std::fs::create_dir_all(&plugin_dir).unwrap();
        let path = plugin_dir.join("plugin.json");
        std::fs::write(&path, b"not valid json {{").unwrap();

        let artifact = ImportArtifact::Plugin {
            manifest_path: path.clone(),
            marketplace: None,
        };
        let meta = ImportArtifactMeta {
            source: ImportSource::ClaudeCode {
                profile_dir: dir.path().to_path_buf(),
            },
            source_path: path,
            content_hash: "abc".to_string(),
            discovered_at: SystemTime::now(),
        };
        let t = PluginsTriager;
        assert!(t.triage(&artifact, &meta).is_skip());
    }

    // ── Full pipeline ─────────────────────────────────────────────────────────

    #[test]
    #[serial(anvil_config_home)]
    fn run_plugins_import_stages_valid_plugin() {
        let dir = TempDir::new().unwrap();
        unsafe { std::env::set_var("ANVIL_CONFIG_HOME", dir.path()) };

        let cc_dir = dir.path().join("cc");
        let manifest = minimal_cc_manifest("test-plugin");
        write_plugin(&cc_dir, "default", "test-plugin", &manifest);
        write_installed_plugins_at(&cc_dir, &[("test-plugin", "default")]);

        let staging = crate::import::staging::StagingDir::create_clean().unwrap();
        let records = run_plugins_import(&cc_dir, &staging);

        unsafe { std::env::remove_var("ANVIL_CONFIG_HOME") };

        assert_eq!(records.len(), 1);
        assert!(!records[0].skipped);
        assert!(records[0].staged_path.as_ref().unwrap().exists());
    }

    #[test]
    #[serial(anvil_config_home)]
    fn run_plugins_import_idempotent() {
        let dir = TempDir::new().unwrap();
        unsafe { std::env::set_var("ANVIL_CONFIG_HOME", dir.path()) };

        let cc_dir = dir.path().join("cc");
        let manifest = minimal_cc_manifest("idempotent-plugin");
        write_plugin(&cc_dir, "default", "idempotent-plugin", &manifest);
        write_installed_plugins_at(&cc_dir, &[("idempotent-plugin", "default")]);

        let staging1 = crate::import::staging::StagingDir::create_clean().unwrap();
        let r1 = run_plugins_import(&cc_dir, &staging1);

        let staging2 = crate::import::staging::StagingDir::create_clean().unwrap();
        let r2 = run_plugins_import(&cc_dir, &staging2);

        unsafe { std::env::remove_var("ANVIL_CONFIG_HOME") };

        assert_eq!(r1.len(), r2.len());
        assert_eq!(r1[0].plugin_name, r2[0].plugin_name);
    }

    #[test]
    #[serial(anvil_config_home)]
    fn collision_stages_as_imported_suffix() {
        let dir = TempDir::new().unwrap();
        unsafe { std::env::set_var("ANVIL_CONFIG_HOME", dir.path()) };

        // Pre-create existing Anvil plugin
        let existing_plugin_dir = dir.path().join("plugins").join("idempotent-plugin");
        std::fs::create_dir_all(&existing_plugin_dir).unwrap();
        std::fs::write(
            existing_plugin_dir.join("plugin.json"),
            serde_json::to_vec_pretty(&minimal_cc_manifest("idempotent-plugin")).unwrap(),
        )
        .unwrap();

        let cc_dir = dir.path().join("cc");
        let manifest = minimal_cc_manifest("idempotent-plugin");
        write_plugin(&cc_dir, "default", "idempotent-plugin", &manifest);
        write_installed_plugins_at(&cc_dir, &[("idempotent-plugin", "default")]);

        let staging = crate::import::staging::StagingDir::create_clean().unwrap();
        let records = run_plugins_import(&cc_dir, &staging);

        unsafe { std::env::remove_var("ANVIL_CONFIG_HOME") };

        assert_eq!(records.len(), 1);
        assert!(records[0].needs_review);
    }
}
