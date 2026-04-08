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
            if let Ok(raw) = std::fs::read_to_string(path) {
                if let Ok(file) = serde_json::from_str::<PermissionsFile>(&raw) {
                    for entry in file.entries {
                        entries
                            .entry(entry.tool_name.clone())
                            .or_default()
                            .push(entry);
                    }
                }
            }
        }

        Self {
            store_path: project_path,
            entries,
        }
    }

    /// Return `true` if `tool_name` + `input` are covered by a stored grant.
    ///
    /// A grant matches when:
    /// - its `tool_name` equals the requested tool, AND
    /// - its `input_pattern` is `None` (wildcard), OR the input contains the
    ///   pattern as a substring.
    #[must_use]
    pub fn is_allowed(&self, tool_name: &str, input: &str) -> bool {
        let Some(grants) = self.entries.get(tool_name) else {
            return false;
        };
        grants.iter().any(|grant| match &grant.input_pattern {
            None => true,
            Some(pattern) => input.contains(pattern.as_str()),
        })
    }

    /// Record a new permission grant.
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
        let granted_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);

        let entry = PermissionMemoryEntry {
            tool_name: tool_name.to_string(),
            input_pattern: pattern.map(|p| p.to_string()),
            scope,
            granted_at,
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
    use tempfile::TempDir;

    fn temp_project() -> TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn is_allowed_returns_false_when_empty() {
        let dir = temp_project();
        let mem = PermissionMemory::load(dir.path());
        assert!(!mem.is_allowed("bash", "echo hi"));
    }

    #[test]
    fn grant_session_allows_immediately() {
        let dir = temp_project();
        let mut mem = PermissionMemory::load(dir.path());
        mem.grant("bash", None, PermissionScope::Session);
        assert!(mem.is_allowed("bash", "echo hi"));
        assert!(mem.is_allowed("bash", "any input at all"));
    }

    #[test]
    fn grant_with_pattern_matches_substring() {
        let dir = temp_project();
        let mut mem = PermissionMemory::load(dir.path());
        mem.grant("bash", Some("echo"), PermissionScope::Session);
        assert!(mem.is_allowed("bash", "echo hello"));
        assert!(!mem.is_allowed("bash", "rm -rf /"));
    }

    #[test]
    fn save_and_reload_project_scope() {
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
    fn session_scope_not_persisted() {
        let dir = temp_project();
        {
            let mut mem = PermissionMemory::load(dir.path());
            mem.grant("bash", None, PermissionScope::Session);
            mem.save().unwrap();
        }
        let mem2 = PermissionMemory::load(dir.path());
        assert!(!mem2.is_allowed("bash", "echo hi"));
    }
}
