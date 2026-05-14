// Allow `unsafe` blocks in tests only — we need `std::env::set_var` to
// redirect `ANVIL_CONFIG_HOME` at a temp dir for isolated test runs, and
// Rust 2024 gates env mutation behind `unsafe`. The crate-wide
// `#![forbid(unsafe_code)]` lint would otherwise block it.
#![cfg_attr(test, allow(unsafe_code))]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::config::default_config_home;
use crate::memory::project_path_hash;

// ─── Types ─────────────────────────────────────────────────────────────────

/// The lifetime scope of a permission grant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PermissionScope {
    /// Valid only for the current CLI process / session.
    Session,
    /// Persisted for the current project directory.
    Project,
    /// Persisted globally across all projects.
    Global,
}

/// Phase 3.4: the effect a stored grant has when it matches.
///
/// Existing on-disk entries (written before 3.4) deserialise with
/// `effect: Allow` because of the `#[serde(default)]` attribute, so the
/// schema change is forward-compatible.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionEffect {
    /// Allow the matching tool invocation without prompting (legacy default).
    Allow,
    /// Deny the matching tool invocation. Outranks reviewer and prompter.
    /// MUST NOT bypass auto-mode hard-deny — that remains the unbypassable
    /// safety override.
    Deny,
    /// Force the matching tool invocation to the normal prompter path,
    /// even when a less-specific Allow would otherwise short-circuit.
    Prompt,
}

impl Default for PermissionEffect {
    fn default() -> Self {
        Self::Allow
    }
}

/// A single persisted permission grant.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionMemoryEntry {
    pub tool_name: String,
    /// Optional glob/substring pattern for the tool input.  `None` means "any
    /// input for this tool".
    pub input_pattern: Option<String>,
    pub scope: PermissionScope,
    /// Unix timestamp (seconds) when the grant was created.
    pub granted_at: u64,
    /// Phase 3.4: what should happen when this entry matches a call.
    /// `#[serde(default)]` means entries written before 3.4 deserialise
    /// with `Allow`, preserving legacy semantics for stored grants.
    #[serde(default)]
    pub effect: PermissionEffect,
}

/// Persistent store that remembers tool permission grants across sessions.
///
/// Session-scoped entries live only in memory; Project- and Global-scoped
/// entries are serialised to JSON on `save()`.
///
/// Storage paths:
/// - project scope: `~/.anvil/projects/<hash>/permissions.json`
/// - global scope:  `~/.anvil/permissions.json`
#[derive(Debug)]
pub struct PermissionMemory {
    store_path: PathBuf,
    /// Entries keyed by `tool_name`.
    entries: BTreeMap<String, Vec<PermissionMemoryEntry>>,
}

// ─── On-disk schema ────────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize, Default)]
struct PermissionsFile {
    entries: Vec<PermissionMemoryEntry>,
}

// ─── Implementation ────────────────────────────────────────────────────────

impl PermissionMemory {
    /// Load permission memory for the given project directory.
    ///
    /// Both project-scoped (`~/.anvil/projects/<hash>/permissions.json`) and
    /// global-scoped (`~/.anvil/permissions.json`) entries are merged into a
    /// single in-memory store.  Missing files are silently ignored.
    #[must_use]
    pub fn load(project_dir: &Path) -> Self {
        let canonical = project_dir
            .canonicalize()
            .unwrap_or_else(|_| project_dir.to_path_buf());
        let hash = project_path_hash(&canonical);
        let project_path = default_config_home()
            .join("projects")
            .join(&hash)
            .join("permissions.json");

        let global_path = default_config_home().join("permissions.json");

        let mut entries: BTreeMap<String, Vec<PermissionMemoryEntry>> = BTreeMap::new();

        // Load global entries first, then project entries (project wins on
        // conflicts because it is more specific).
        for path in [&global_path, &project_path] {
            if let Ok(raw) = std::fs::read_to_string(path)
                && let Ok(file) = serde_json::from_str::<PermissionsFile>(&raw) {
                    for entry in file.entries {
                        entries
                            .entry(entry.tool_name.clone())
                            .or_default()
                            .push(entry);
                    }
                }
        }

        Self {
            store_path: project_path,
            entries,
        }
    }

    /// Return `true` if `tool_name` + `input` are covered by a stored
    /// `Allow`-effect grant.
    ///
    /// Phase 3.4: this is the legacy bool-return entry point. Callers that
    /// need to know about `Deny` or `Prompt` effects MUST use
    /// [`Self::effect_for`] instead — `is_allowed` returns `false` for
    /// `Deny` and `Prompt` matches, which loses information.
    ///
    /// A grant matches when:
    /// - its `tool_name` equals the requested tool, AND
    /// - its `input_pattern` is `None` (wildcard), OR the input contains the
    ///   pattern as a substring.
    #[must_use]
    pub fn is_allowed(&self, tool_name: &str, input: &str) -> bool {
        matches!(self.effect_for(tool_name, input), Some(PermissionEffect::Allow))
    }

    /// Phase 3.4: return the effect of the most-specific matching grant.
    ///
    /// Selection order when multiple grants match:
    ///   1. `Deny`   — outranks everything else (it's a user veto).
    ///   2. `Prompt` — forces the prompter path.
    ///   3. `Allow`  — shortcircuit.
    ///
    /// Returns `None` if no grant matches.
    #[must_use]
    pub fn effect_for(&self, tool_name: &str, input: &str) -> Option<PermissionEffect> {
        let grants = self.entries.get(tool_name)?;
        let mut best: Option<PermissionEffect> = None;
        for grant in grants {
            let matches = match &grant.input_pattern {
                None => true,
                Some(pattern) => input.contains(pattern.as_str()),
            };
            if !matches {
                continue;
            }
            // Deny is sticky. Once we see a Deny match, nothing else
            // changes the answer.
            match grant.effect {
                PermissionEffect::Deny => return Some(PermissionEffect::Deny),
                PermissionEffect::Prompt => {
                    if best != Some(PermissionEffect::Deny) {
                        best = Some(PermissionEffect::Prompt);
                    }
                }
                PermissionEffect::Allow => {
                    if best.is_none() {
                        best = Some(PermissionEffect::Allow);
                    }
                }
            }
        }
        best
    }

    /// Record a new permission grant with the default Allow effect.
    ///
    /// Session-scoped grants are kept only in memory.  Project- and
    /// Global-scoped grants are written to disk via [`Self::save`]; callers
    /// should call `save()` after `grant()` if they want durability.
    pub fn grant(
        &mut self,
        tool_name: &str,
        pattern: Option<&str>,
        scope: PermissionScope,
    ) {
        self.grant_with_effect(tool_name, pattern, scope, PermissionEffect::Allow);
    }

    /// Phase 3.4: record a permission grant with an explicit effect
    /// (Allow / Deny / Prompt).
    pub fn grant_with_effect(
        &mut self,
        tool_name: &str,
        pattern: Option<&str>,
        scope: PermissionScope,
        effect: PermissionEffect,
    ) {
        let granted_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);

        let entry = PermissionMemoryEntry {
            tool_name: tool_name.to_string(),
            input_pattern: pattern.map(std::string::ToString::to_string),
            scope,
            granted_at,
            effect,
        };

        self.entries
            .entry(tool_name.to_string())
            .or_default()
            .push(entry);
    }

    /// Persist all non-session-scoped entries to
    /// `~/.anvil/projects/<hash>/permissions.json`.
    ///
    /// # Errors
    /// Returns an error if the directory cannot be created or the file cannot
    /// be written.
    pub fn save(&self) -> Result<(), String> {
        let durable_entries: Vec<PermissionMemoryEntry> = self
            .entries
            .values()
            .flatten()
            .filter(|e| e.scope != PermissionScope::Session)
            .cloned()
            .collect();

        if durable_entries.is_empty() {
            // Nothing to persist — skip creating the file.
            return Ok(());
        }

        if let Some(parent) = self.store_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("cannot create permissions dir: {e}"))?;
        }

        let file = PermissionsFile {
            entries: durable_entries,
        };
        let json = serde_json::to_string_pretty(&file)
            .map_err(|e| format!("cannot serialise permissions: {e}"))?;
        std::fs::write(&self.store_path, json)
            .map_err(|e| format!("cannot write permissions file: {e}"))?;
        Ok(())
    }

    /// Iterate over all entries (all scopes).
    pub fn all_entries(&self) -> impl Iterator<Item = &PermissionMemoryEntry> {
        self.entries.values().flatten()
    }
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use tempfile::TempDir;

    fn temp_project() -> TempDir {
        tempfile::tempdir().unwrap()
    }

    /// Scoped guard that points `ANVIL_CONFIG_HOME` at a fresh temp dir for
    /// the duration of a single test, restoring the previous value on drop.
    /// Shares the crate-wide `test_env_lock()` so it serialises with other
    /// tests that also mutate `ANVIL_CONFIG_HOME` (oauth, prompt).
    struct ConfigHomeGuard {
        _lock: std::sync::MutexGuard<'static, ()>,
        _home: TempDir,
        prev: Option<std::ffi::OsString>,
    }

    impl ConfigHomeGuard {
        fn new() -> Self {
            let lock = crate::test_env_lock();
            let prev = std::env::var_os("ANVIL_CONFIG_HOME");
            let home = tempfile::tempdir().unwrap();
            unsafe { std::env::set_var("ANVIL_CONFIG_HOME", home.path()); }
            Self {
                _lock: lock,
                _home: home,
                prev,
            }
        }
    }

    impl Drop for ConfigHomeGuard {
        fn drop(&mut self) {
            match &self.prev {
                Some(value) => unsafe { std::env::set_var("ANVIL_CONFIG_HOME", value); },
                None => unsafe { std::env::remove_var("ANVIL_CONFIG_HOME"); },
            }
        }
    }

    #[test]
    #[serial(anvil_config_home)]
    fn is_allowed_returns_false_when_empty() {
        let _guard = ConfigHomeGuard::new();
        let dir = temp_project();
        let mem = PermissionMemory::load(dir.path());
        assert!(!mem.is_allowed("bash", "echo hi"));
    }

    #[test]
    #[serial(anvil_config_home)]
    fn grant_session_allows_immediately() {
        let _guard = ConfigHomeGuard::new();
        let dir = temp_project();
        let mut mem = PermissionMemory::load(dir.path());
        mem.grant("bash", None, PermissionScope::Session);
        assert!(mem.is_allowed("bash", "echo hi"));
        assert!(mem.is_allowed("bash", "any input at all"));
    }

    #[test]
    #[serial(anvil_config_home)]
    fn grant_with_pattern_matches_substring() {
        let _guard = ConfigHomeGuard::new();
        let dir = temp_project();
        let mut mem = PermissionMemory::load(dir.path());
        mem.grant("bash", Some("echo"), PermissionScope::Session);
        assert!(mem.is_allowed("bash", "echo hello"));
        assert!(!mem.is_allowed("bash", "rm -rf /"));
    }

    #[test]
    #[serial(anvil_config_home)]
    fn save_and_reload_project_scope() {
        let _guard = ConfigHomeGuard::new();
        let dir = temp_project();
        {
            let mut mem = PermissionMemory::load(dir.path());
            mem.grant("read_file", None, PermissionScope::Project);
            mem.save().unwrap();
        }
        let mem2 = PermissionMemory::load(dir.path());
        assert!(mem2.is_allowed("read_file", "/any/path"));
    }

    #[test]
    #[serial(anvil_config_home)]
    fn session_scope_not_persisted() {
        let _guard = ConfigHomeGuard::new();
        let dir = temp_project();
        {
            let mut mem = PermissionMemory::load(dir.path());
            mem.grant("bash", None, PermissionScope::Session);
            mem.save().unwrap();
        }
        let mem2 = PermissionMemory::load(dir.path());
        assert!(!mem2.is_allowed("bash", "echo hi"));
    }

    // ── Phase 3.4: PermissionEffect tests ───────────────────────────

    #[test]
    #[serial(anvil_config_home)]
    fn effect_for_returns_allow_for_legacy_grant() {
        let _guard = ConfigHomeGuard::new();
        let dir = temp_project();
        let mut mem = PermissionMemory::load(dir.path());
        mem.grant("bash", None, PermissionScope::Session);
        assert_eq!(
            mem.effect_for("bash", "ls"),
            Some(PermissionEffect::Allow),
            "default grant() should record Allow effect"
        );
    }

    #[test]
    #[serial(anvil_config_home)]
    fn effect_for_returns_deny_when_grant_is_deny() {
        let _guard = ConfigHomeGuard::new();
        let dir = temp_project();
        let mut mem = PermissionMemory::load(dir.path());
        mem.grant_with_effect("bash", Some("rm"), PermissionScope::Session, PermissionEffect::Deny);
        assert_eq!(
            mem.effect_for("bash", "rm -rf /tmp"),
            Some(PermissionEffect::Deny)
        );
    }

    #[test]
    #[serial(anvil_config_home)]
    fn effect_for_returns_prompt_when_grant_is_prompt() {
        let _guard = ConfigHomeGuard::new();
        let dir = temp_project();
        let mut mem = PermissionMemory::load(dir.path());
        mem.grant_with_effect("bash", None, PermissionScope::Session, PermissionEffect::Prompt);
        assert_eq!(
            mem.effect_for("bash", "anything"),
            Some(PermissionEffect::Prompt)
        );
    }

    #[test]
    #[serial(anvil_config_home)]
    fn deny_outranks_allow_when_both_match() {
        let _guard = ConfigHomeGuard::new();
        let dir = temp_project();
        let mut mem = PermissionMemory::load(dir.path());
        // Wildcard Allow + specific Deny → Deny wins.
        mem.grant_with_effect("bash", None, PermissionScope::Session, PermissionEffect::Allow);
        mem.grant_with_effect("bash", Some("rm"), PermissionScope::Session, PermissionEffect::Deny);
        assert_eq!(
            mem.effect_for("bash", "rm -rf /"),
            Some(PermissionEffect::Deny),
            "Deny must outrank Allow when both match"
        );
        // Calls that don't match the Deny pattern still get Allow.
        assert_eq!(
            mem.effect_for("bash", "ls"),
            Some(PermissionEffect::Allow)
        );
    }

    #[test]
    #[serial(anvil_config_home)]
    fn deny_persists_through_save_and_reload() {
        let _guard = ConfigHomeGuard::new();
        let dir = temp_project();
        {
            let mut mem = PermissionMemory::load(dir.path());
            mem.grant_with_effect(
                "bash",
                Some("dangerous"),
                PermissionScope::Project,
                PermissionEffect::Deny,
            );
            mem.save().unwrap();
        }
        let mem2 = PermissionMemory::load(dir.path());
        assert_eq!(
            mem2.effect_for("bash", "dangerous command"),
            Some(PermissionEffect::Deny),
            "Deny must round-trip through save/load"
        );
    }

    #[test]
    #[serial(anvil_config_home)]
    fn legacy_entries_without_effect_field_deserialise_as_allow() {
        let _guard = ConfigHomeGuard::new();
        let dir = temp_project();
        // Write a pre-3.4 entry by hand (no effect field).
        let hash = crate::memory::project_path_hash(
            &dir.path().canonicalize().unwrap_or_else(|_| dir.path().to_path_buf()),
        );
        let store_path = default_config_home()
            .join("projects")
            .join(&hash)
            .join("permissions.json");
        std::fs::create_dir_all(store_path.parent().unwrap()).unwrap();
        let legacy_json = r#"{
            "entries": [
                {
                    "tool_name": "bash",
                    "input_pattern": null,
                    "scope": "Project",
                    "granted_at": 1700000000
                }
            ]
        }"#;
        std::fs::write(&store_path, legacy_json).unwrap();

        let mem = PermissionMemory::load(dir.path());
        // Legacy entry must come back with Allow effect.
        assert_eq!(
            mem.effect_for("bash", "any input"),
            Some(PermissionEffect::Allow),
            "legacy entries without effect must default to Allow"
        );
    }

    #[test]
    #[serial(anvil_config_home)]
    fn is_allowed_is_false_for_deny_grant() {
        // is_allowed is the legacy bool API. After 3.4 it returns true ONLY
        // for Allow matches — Deny and Prompt return false so the gate
        // continues to its policy/prompter path.
        let _guard = ConfigHomeGuard::new();
        let dir = temp_project();
        let mut mem = PermissionMemory::load(dir.path());
        mem.grant_with_effect("bash", None, PermissionScope::Session, PermissionEffect::Deny);
        assert!(
            !mem.is_allowed("bash", "anything"),
            "is_allowed must be false for Deny effect"
        );
    }
}
