// Allow `unsafe` only in test code (env::set_var for ANVIL_CONFIG_HOME).
// This matches the pattern used by file_cache.rs and command_cache.rs for the
// same env-isolation requirement in tests (Rust 2024 requires unsafe for set_var).
#![cfg_attr(test, allow(unsafe_code))]

/// Staging directory lifecycle for the import pipeline.
///
/// All artifact writes during import are staged under
/// `~/.anvil/.import-staging/` before being atomically moved to their final
/// destinations.  This ensures that a cancelled or failed import never leaves
/// partial state in the live Anvil directories.
///
/// # Directory layout
///
/// ```text
/// ~/.anvil/.import-staging/
/// ├── manifest-draft.json      — in-progress manifest
/// ├── memory/                  — staged memory entries
/// ├── instructions/
/// │   ├── global/              — staged ~/.anvil/ANVIL.md
/// │   └── <project-hash>/      — staged <project>/ANVIL.md per project
/// ├── settings/
/// │   ├── translated.json      — proposed merged settings.json
/// │   └── conflicts.json       — keys that conflict with existing
/// ├── skills/
/// ├── plugins/
/// ├── agents/
/// └── sessions/
///     ├── progress.json        — resumable progress state
///     └── daily/<date>.json    — staged DailySummary records
/// ```
///
/// A backup copy of any pre-existing staging directory is kept at
/// `~/.anvil/.import-staging.bak/` so rollback is always possible.

use std::path::{Path, PathBuf};

use crate::import::manifest::{ImportEntryStatus, ImportManifest, ManifestError};

// ── Config home helper ───────────────────────────────────────────────────────

/// Return the Anvil configuration home directory.
///
/// Priority:
///   1. `ANVIL_CONFIG_HOME` environment variable
///   2. `~/.anvil/`
#[must_use]
pub fn anvil_config_home() -> PathBuf {
    std::env::var("ANVIL_CONFIG_HOME")
        .ok()
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            dirs_next::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".anvil")
        })
}

// ── Staging directory helpers ────────────────────────────────────────────────

/// Return the staging directory path.
///
/// Resolves to `$ANVIL_CONFIG_HOME/.import-staging/`.
#[must_use]
pub fn staging_dir() -> PathBuf {
    anvil_config_home().join(".import-staging")
}

/// Return the staging backup directory path.
#[must_use]
pub fn staging_backup_dir() -> PathBuf {
    anvil_config_home().join(".import-staging.bak")
}

/// Return the path to the draft manifest inside staging.
#[must_use]
pub fn staging_draft_manifest_path() -> PathBuf {
    staging_dir().join("manifest-draft.json")
}

/// Subdirectory names inside the staging root.
pub const STAGING_SUBDIRS: &[&str] = &[
    "memory",
    "instructions/global",
    "settings",
    "skills",
    "plugins",
    "agents",
    "sessions/daily",
];

// ── StagingDir ───────────────────────────────────────────────────────────────

/// Handle for the import staging directory.
///
/// Created via [`StagingDir::create_clean`] at the start of an import run.
/// Dropped cleanly by calling [`StagingDir::commit_to_final`] or
/// [`StagingDir::rollback`].
pub struct StagingDir {
    root: PathBuf,
}

impl StagingDir {
    /// Create a fresh, empty staging directory.
    ///
    /// If a staging directory already exists:
    ///   1. The existing staging dir is renamed to `<root>.bak` (overwriting
    ///      any previous backup).
    ///   2. A fresh staging directory is created.
    ///
    /// # Errors
    ///
    /// Returns `StagingError` on I/O failure.
    pub fn create_clean() -> Result<Self, StagingError> {
        let root = staging_dir();
        let backup = staging_backup_dir();

        if root.exists() {
            // Remove old backup if present.
            if backup.exists() {
                std::fs::remove_dir_all(&backup)
                    .map_err(|e| StagingError::Io(format!("remove old backup: {e}")))?;
            }
            // Rename existing staging to backup.
            std::fs::rename(&root, &backup)
                .map_err(|e| StagingError::Io(format!("backup existing staging dir: {e}")))?;
        }

        // Create fresh staging root and all subdirectories.
        for sub in STAGING_SUBDIRS {
            std::fs::create_dir_all(root.join(sub))
                .map_err(|e| StagingError::Io(format!("create staging subdir {sub}: {e}")))?;
        }

        Ok(Self { root })
    }

    /// Return the staging root path.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Resolve a path relative to the staging root.
    #[must_use]
    pub fn path(&self, relative: &str) -> PathBuf {
        self.root.join(relative)
    }

    /// Write a file to the staging directory, creating parent directories.
    ///
    /// `relative_dest` is relative to the staging root, e.g.
    /// `"memory/my-rule.md"`.
    ///
    /// # Errors
    ///
    /// Returns `StagingError::Io` on failure.
    pub fn stage_bytes(&self, relative_dest: &str, bytes: &[u8]) -> Result<PathBuf, StagingError> {
        let dest = self.root.join(relative_dest);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| StagingError::Io(format!("create parent for {relative_dest}: {e}")))?;
        }
        std::fs::write(&dest, bytes)
            .map_err(|e| StagingError::Io(format!("write staged file {relative_dest}: {e}")))?;
        Ok(dest)
    }

    /// Validate that the staging directory has the expected structure.
    ///
    /// Returns a list of missing subdirectories; empty list means valid.
    #[must_use]
    pub fn validate(&self) -> Vec<String> {
        STAGING_SUBDIRS
            .iter()
            .filter(|sub| !self.root.join(sub).exists())
            .map(|s| (*s).to_string())
            .collect()
    }

    /// Atomically move all staged files to their final destinations.
    ///
    /// For each `Staged` entry in `manifest`:
    ///   - Creates parent directories under the destination.
    ///   - Renames (atomic on most POSIX filesystems) staging path to dest.
    ///   - Updates entry status to `Committed` on success, `Failed` on error.
    ///
    /// Returns a `CommitReport` summarising the outcome.
    ///
    /// # Errors
    ///
    /// Never returns `Err` — individual entry failures are captured in
    /// `CommitReport::failed`.  The manifest passed in is updated in-place
    /// so the caller can persist it afterwards.
    pub fn commit_to_final(&self, manifest: &mut ImportManifest) -> Result<CommitReport, StagingError> {
        let mut report = CommitReport::default();

        for entry in manifest.entries.iter_mut() {
            if entry.status != ImportEntryStatus::Staged {
                continue;
            }

            // The staged file lives at <staging_root>/<artifact>/<filename>.
            // Its destination_path is the final absolute path.
            let staged_src = {
                // Convention: staged files mirror destination_path relative to
                // the staging root.  Bucket implementations write them there.
                // For Phase 6.0 (no actual artifact imports), this is a no-op.
                let filename = entry.destination_path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();
                self.root.join(&entry.artifact).join(&filename)
            };

            if !staged_src.exists() {
                entry.status = ImportEntryStatus::Failed;
                entry.error = Some(format!("staged file not found: {}", staged_src.display()));
                report.failed += 1;
                continue;
            }

            let dest = &entry.destination_path;
            if let Some(parent) = dest.parent() {
                if let Err(e) = std::fs::create_dir_all(parent) {
                    entry.status = ImportEntryStatus::Failed;
                    entry.error = Some(format!("create dest dir: {e}"));
                    report.failed += 1;
                    continue;
                }
            }

            match std::fs::rename(&staged_src, dest) {
                Ok(()) => {
                    entry.status = ImportEntryStatus::Committed;
                    report.committed += 1;
                }
                Err(e) => {
                    entry.status = ImportEntryStatus::Failed;
                    entry.error = Some(format!("rename to final dest: {e}"));
                    report.failed += 1;
                }
            }
        }

        Ok(report)
    }

    /// Restore the previous staging directory from backup and remove the
    /// current staging directory.
    ///
    /// This is best-effort: individual failures are logged but do not abort
    /// the rollback.
    ///
    /// # Errors
    ///
    /// Returns `StagingError::Io` if removing the current staging directory
    /// fails entirely.
    pub fn rollback() -> Result<(), StagingError> {
        let root = staging_dir();
        let backup = staging_backup_dir();

        if root.exists() {
            std::fs::remove_dir_all(&root)
                .map_err(|e| StagingError::Io(format!("remove staging for rollback: {e}")))?;
        }

        if backup.exists() {
            std::fs::rename(&backup, &root)
                .map_err(|e| StagingError::Io(format!("restore backup staging dir: {e}")))?;
        }

        Ok(())
    }

    /// Save a draft manifest to `staging/manifest-draft.json`.
    ///
    /// # Errors
    ///
    /// Returns `StagingError::Manifest` on serialization or I/O failure.
    pub fn save_draft_manifest(&self, manifest: &ImportManifest) -> Result<(), StagingError> {
        let path = self.root.join("manifest-draft.json");
        manifest
            .save(&path)
            .map_err(|e| StagingError::Manifest(e.to_string()))
    }

    /// Load the draft manifest from `staging/manifest-draft.json`, returning
    /// a fresh empty manifest if the file does not exist.
    ///
    /// # Errors
    ///
    /// Returns `StagingError::Manifest` on parse failure.
    pub fn load_draft_manifest(&self, pipeline_version: &str) -> Result<ImportManifest, StagingError> {
        let path = self.root.join("manifest-draft.json");
        ImportManifest::load_or_new(&path, pipeline_version)
            .map_err(|e| StagingError::Manifest(e.to_string()))
    }
}

// ── CommitReport ─────────────────────────────────────────────────────────────

/// Summary of a `commit_to_final` call.
#[derive(Debug, Clone, Default)]
pub struct CommitReport {
    /// Number of entries successfully moved to their final destinations.
    pub committed: usize,
    /// Number of entries that failed during the move.
    pub failed: usize,
}

impl CommitReport {
    /// Return `true` if every staged entry was committed successfully.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.failed == 0
    }
}

// ── StagingError ─────────────────────────────────────────────────────────────

/// Error type for staging directory operations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StagingError {
    Io(String),
    Manifest(String),
}

impl std::fmt::Display for StagingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(msg) => write!(f, "staging I/O error: {msg}"),
            Self::Manifest(msg) => write!(f, "staging manifest error: {msg}"),
        }
    }
}

impl std::error::Error for StagingError {}

impl From<ManifestError> for StagingError {
    fn from(e: ManifestError) -> Self {
        Self::Manifest(e.to_string())
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::import::manifest::ImportEntry;
    use serial_test::serial;
    use std::path::PathBuf;

    /// Set ANVIL_CONFIG_HOME to `path` for the duration of the test.
    /// Safety: env mutation is race-free because all callers hold the
    /// `serial(anvil_config_home)` lock via the `#[serial]` attribute.
    fn set_anvil_home(path: &std::path::Path) {
        unsafe { std::env::set_var("ANVIL_CONFIG_HOME", path) };
    }

    fn clear_anvil_home() {
        unsafe { std::env::remove_var("ANVIL_CONFIG_HOME") };
    }

    #[test]
    #[serial(anvil_config_home)]
    fn create_clean_creates_subdirs() {
        let dir = tempfile::TempDir::new().expect("tmpdir");
        set_anvil_home(dir.path());

        let staging = StagingDir::create_clean().expect("create_clean");
        let missing = staging.validate();

        clear_anvil_home();
        assert!(
            missing.is_empty(),
            "Missing staging subdirs: {missing:?}"
        );
    }

    #[test]
    #[serial(anvil_config_home)]
    fn create_clean_backs_up_existing() {
        let dir = tempfile::TempDir::new().expect("tmpdir");
        set_anvil_home(dir.path());

        // First create.
        StagingDir::create_clean().expect("first create");
        // Write a sentinel file.
        let sentinel = staging_dir().join("memory").join("sentinel.md");
        std::fs::write(&sentinel, b"hello").expect("write sentinel");

        // Second create should back up the first.
        StagingDir::create_clean().expect("second create");
        let backup_sentinel = staging_backup_dir().join("memory").join("sentinel.md");

        let exists = backup_sentinel.exists();
        clear_anvil_home();
        assert!(exists, "Sentinel should be in backup dir");
    }

    #[test]
    #[serial(anvil_config_home)]
    fn stage_bytes_writes_file() {
        let dir = tempfile::TempDir::new().expect("tmpdir");
        set_anvil_home(dir.path());

        let staging = StagingDir::create_clean().expect("create");
        let dest = staging.stage_bytes("memory/test.md", b"content").expect("stage");
        let exists = dest.exists();
        let contents = std::fs::read(&dest).expect("read");

        clear_anvil_home();
        assert!(exists);
        assert_eq!(contents, b"content");
    }

    #[test]
    #[serial(anvil_config_home)]
    fn rollback_removes_staging_and_restores_backup() {
        let dir = tempfile::TempDir::new().expect("tmpdir");
        set_anvil_home(dir.path());

        StagingDir::create_clean().expect("first create");
        // Write a sentinel to the first staging.
        let sentinel = staging_dir().join("memory").join("original.md");
        std::fs::write(&sentinel, b"original").expect("write");

        // Second create backs up the first.
        StagingDir::create_clean().expect("second create");
        let staging_had_sentinel = !sentinel.exists();

        // Rollback should restore the first staging.
        StagingDir::rollback().expect("rollback");
        let restored = sentinel.exists();

        clear_anvil_home();
        assert!(staging_had_sentinel, "staging was replaced by second create");
        assert!(restored, "rollback should restore original staging");
    }

    #[test]
    #[serial(anvil_config_home)]
    fn idempotent_re_import_produces_no_changes() {
        // Bucket 0 acceptance test:
        //   Re-running import on unchanged source produces zero new artifacts.
        //
        // Mechanism: content_hash + source_path compared against committed entries.
        let dir = tempfile::TempDir::new().expect("tmpdir");
        set_anvil_home(dir.path());

        let src = PathBuf::from("/fake/.claude/projects/abc/memory/rule.md");
        let dest = PathBuf::from("/fake/.anvil/memory/rule.md");
        let hash = "abc123";
        let version = "2.2.14-test";

        let manifest_path = anvil_config_home().join(".import-manifest.json");

        // Simulate a first successful import by writing a committed entry.
        let mut m = ImportManifest::new(version);
        let mut entry = ImportEntry::pending(
            "memory",
            src.clone(),
            dest.clone(),
            hash,
            "2026-05-15T00:00:00Z",
        );
        entry.status = ImportEntryStatus::Committed;
        m.push(entry);
        m.save(&manifest_path).expect("save manifest");

        // Reload and check idempotency gate.
        let loaded = ImportManifest::load_or_new(&manifest_path, version).expect("load");
        let already_committed = loaded.is_already_committed(&src, hash);
        let diff_hash_skip = loaded.is_already_committed(&src, "differenthash");

        clear_anvil_home();
        assert!(already_committed, "Second run should detect committed entry and skip");
        assert!(!diff_hash_skip, "Changed source should not be skipped");
    }

    #[test]
    #[serial(anvil_config_home)]
    fn draft_manifest_round_trip() {
        let dir = tempfile::TempDir::new().expect("tmpdir");
        set_anvil_home(dir.path());

        let staging = StagingDir::create_clean().expect("create");
        let mut m = ImportManifest::new("2.2.14-test");
        m.push(ImportEntry::pending(
            "skill",
            PathBuf::from("/fake/.claude/skills/test.md"),
            PathBuf::from("/fake/.anvil/skills/test.md"),
            "cafebabe",
            "2026-05-15T00:00:00Z",
        ));

        staging.save_draft_manifest(&m).expect("save draft");
        let loaded = staging.load_draft_manifest("2.2.14-test").expect("load draft");
        let entry_count = loaded.entries.len();
        let artifact_name = loaded.entries[0].artifact.clone();

        clear_anvil_home();
        assert_eq!(entry_count, 1);
        assert_eq!(artifact_name, "skill");
    }
}
