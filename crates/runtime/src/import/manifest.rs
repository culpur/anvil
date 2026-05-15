/// ImportManifest — on-disk record of every artifact that has been imported.
///
/// Lives at `~/.anvil/.import-manifest.json`.  Written after each successful
/// Commit phase and read at the start of every import run to enforce
/// idempotency: if an artifact's `(source_path, content_hash)` pair already
/// appears in the manifest with status `Committed`, it is skipped.
///
/// Schema versioning:
///   `manifest_version: 1` — Phase 6.0 initial schema.
///   Bump the version field (not the filename) when fields are added or the
///   semantics of an existing field change in a breaking way.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

// ── Status enum ─────────────────────────────────────────────────────────────

/// Lifecycle state of a single import entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImportEntryStatus {
    /// Staged but not yet committed to its final destination.
    Pending,
    /// Written to `~/.anvil/.import-staging/` but not yet moved.
    Staged,
    /// Atomically moved from staging to its final destination.
    Committed,
    /// Deliberately excluded from import (see `skip_reason`).
    Skipped,
    /// Import attempt failed (see `error`).
    Failed,
}

impl ImportEntryStatus {
    /// Return `true` when the entry was successfully committed.
    #[must_use]
    pub fn is_committed(&self) -> bool {
        matches!(self, Self::Committed)
    }
}

// ── Entry ────────────────────────────────────────────────────────────────────

/// One record in the import manifest, representing a single artifact that
/// has been (or attempted to be) imported.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportEntry {
    /// String discriminator matching `ImportArtifact::kind_tag()`.
    pub artifact: String,
    /// Original path on disk (under `~/.claude/` for CC imports).
    pub source_path: PathBuf,
    /// Final destination path (under `~/.anvil/`).
    pub destination_path: PathBuf,
    /// SHA-256 hex digest of the source bytes at import time.
    ///
    /// Idempotency gate: if a re-run discovers an artifact whose
    /// `(source_path, content_hash)` already appears in a `Committed` entry,
    /// the artifact is skipped without re-importing.
    pub content_hash: String,
    /// RFC 3339 timestamp when the entry reached its current status.
    pub imported_at: String,
    /// Current lifecycle status.
    pub status: ImportEntryStatus,
    /// Human-readable skip reason, present only when `status == Skipped`.
    pub skip_reason: Option<String>,
    /// Error message, present only when `status == Failed`.
    pub error: Option<String>,
}

impl ImportEntry {
    /// Construct a new entry in `Pending` state.
    #[must_use]
    pub fn pending(
        artifact: impl Into<String>,
        source_path: PathBuf,
        destination_path: PathBuf,
        content_hash: impl Into<String>,
        imported_at: impl Into<String>,
    ) -> Self {
        Self {
            artifact: artifact.into(),
            source_path,
            destination_path,
            content_hash: content_hash.into(),
            imported_at: imported_at.into(),
            status: ImportEntryStatus::Pending,
            skip_reason: None,
            error: None,
        }
    }
}

// ── Manifest ─────────────────────────────────────────────────────────────────

/// Root manifest document stored at `~/.anvil/.import-manifest.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportManifest {
    /// Schema version.  Must be `1` for Phase 6.0 readers to accept the file.
    pub manifest_version: u32,
    /// The Anvil version string that last wrote this manifest.
    pub pipeline_version: String,
    /// All import entries, in insertion order.
    pub entries: Vec<ImportEntry>,
}

impl ImportManifest {
    /// Construct an empty manifest for the current Anvil version.
    #[must_use]
    pub fn new(pipeline_version: impl Into<String>) -> Self {
        Self {
            manifest_version: 1,
            pipeline_version: pipeline_version.into(),
            entries: Vec::new(),
        }
    }

    /// Return the canonical path for the manifest file.
    ///
    /// Respects `ANVIL_CONFIG_HOME`; defaults to `~/.anvil/`.
    #[must_use]
    pub fn default_path() -> PathBuf {
        crate::import::staging::anvil_config_home().join(".import-manifest.json")
    }

    /// Read the manifest from `path`, returning a fresh empty manifest if the
    /// file does not exist.
    ///
    /// # Errors
    ///
    /// Returns an error if the file exists but cannot be parsed as valid JSON.
    pub fn load_or_new(path: &Path, pipeline_version: &str) -> Result<Self, ManifestError> {
        if !path.exists() {
            return Ok(Self::new(pipeline_version));
        }
        let bytes = std::fs::read(path).map_err(|e| ManifestError::Io(e.to_string()))?;
        serde_json::from_slice(&bytes).map_err(|e| ManifestError::Parse(e.to_string()))
    }

    /// Persist the manifest to `path`, creating parent directories as needed.
    ///
    /// Writes atomically: serializes to a temp file beside the target, then
    /// renames.  On platforms where rename is non-atomic this is best-effort.
    ///
    /// # Errors
    ///
    /// Returns an error on I/O failure or JSON serialization failure.
    pub fn save(&self, path: &Path) -> Result<(), ManifestError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| ManifestError::Io(e.to_string()))?;
        }

        let json = serde_json::to_string_pretty(self)
            .map_err(|e| ManifestError::Serialize(e.to_string()))?;

        // Write to a temp file alongside the target, then rename.
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, json.as_bytes())
            .map_err(|e| ManifestError::Io(e.to_string()))?;
        std::fs::rename(&tmp, path)
            .map_err(|e| ManifestError::Io(e.to_string()))?;

        Ok(())
    }

    /// Return `true` if an entry with the given `source_path` and
    /// `content_hash` already exists with status `Committed`.
    ///
    /// This is the idempotency gate: a re-run on unchanged source produces
    /// a no-op.
    #[must_use]
    pub fn is_already_committed(&self, source_path: &Path, content_hash: &str) -> bool {
        self.entries.iter().any(|e| {
            e.source_path == source_path
                && e.content_hash == content_hash
                && e.status.is_committed()
        })
    }

    /// Append an entry to the manifest (does not persist to disk).
    pub fn push(&mut self, entry: ImportEntry) {
        self.entries.push(entry);
    }

    /// Return a view of all entries with the given status.
    #[must_use]
    pub fn entries_with_status(&self, status: &ImportEntryStatus) -> Vec<&ImportEntry> {
        self.entries.iter().filter(|e| &e.status == status).collect()
    }

    /// Count entries by status.
    #[must_use]
    pub fn count_by_status(&self, status: &ImportEntryStatus) -> usize {
        self.entries.iter().filter(|e| &e.status == status).count()
    }
}

// ── ManifestError ────────────────────────────────────────────────────────────

/// Error type for manifest I/O operations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManifestError {
    Io(String),
    Parse(String),
    Serialize(String),
}

impl std::fmt::Display for ManifestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(msg) => write!(f, "manifest I/O error: {msg}"),
            Self::Parse(msg) => write!(f, "manifest parse error: {msg}"),
            Self::Serialize(msg) => write!(f, "manifest serialize error: {msg}"),
        }
    }
}

impl std::error::Error for ManifestError {}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tempfile::TempDir;

    fn sample_entry(status: ImportEntryStatus) -> ImportEntry {
        ImportEntry {
            artifact: "memory".into(),
            source_path: PathBuf::from("/home/user/.claude/projects/abc/memory/rule.md"),
            destination_path: PathBuf::from("/home/user/.anvil/memory/rule.md"),
            content_hash: "deadbeef1234".into(),
            imported_at: "2026-05-15T10:00:00Z".into(),
            status,
            skip_reason: None,
            error: None,
        }
    }

    #[test]
    fn manifest_round_trip_json() {
        let mut m = ImportManifest::new("2.2.14-test");
        m.push(sample_entry(ImportEntryStatus::Committed));
        m.push(sample_entry(ImportEntryStatus::Skipped));

        let json = serde_json::to_string(&m).expect("serialize");
        let back: ImportManifest = serde_json::from_str(&json).expect("deserialize");

        assert_eq!(back.manifest_version, 1);
        assert_eq!(back.pipeline_version, "2.2.14-test");
        assert_eq!(back.entries.len(), 2);
        assert!(back.entries[0].status.is_committed());
    }

    #[test]
    fn idempotency_gate_detects_committed_entry() {
        let mut m = ImportManifest::new("2.2.14-test");
        let src = PathBuf::from("/home/user/.claude/projects/abc/memory/rule.md");

        // Not yet committed — should return false.
        assert!(!m.is_already_committed(&src, "deadbeef1234"));

        m.push(sample_entry(ImportEntryStatus::Committed));

        // Now committed — should return true.
        assert!(m.is_already_committed(&src, "deadbeef1234"));

        // Different hash — should still return false.
        assert!(!m.is_already_committed(&src, "otherhash"));
    }

    #[test]
    fn idempotency_gate_ignores_non_committed_entries() {
        let mut m = ImportManifest::new("2.2.14-test");
        let src = PathBuf::from("/home/user/.claude/projects/abc/memory/rule.md");

        // A Pending entry does NOT satisfy the idempotency gate.
        m.push(sample_entry(ImportEntryStatus::Pending));
        assert!(!m.is_already_committed(&src, "deadbeef1234"));

        // A Failed entry does NOT satisfy the idempotency gate.
        m.push({
            let mut e = sample_entry(ImportEntryStatus::Failed);
            e.error = Some("disk full".into());
            e
        });
        assert!(!m.is_already_committed(&src, "deadbeef1234"));
    }

    #[test]
    fn save_and_load_round_trip() {
        let dir = TempDir::new().expect("tmpdir");
        let path = dir.path().join(".import-manifest.json");

        let mut m = ImportManifest::new("2.2.14-test");
        m.push(sample_entry(ImportEntryStatus::Committed));
        m.save(&path).expect("save");

        let loaded = ImportManifest::load_or_new(&path, "2.2.14-test").expect("load");
        assert_eq!(loaded.entries.len(), 1);
        assert!(loaded.entries[0].status.is_committed());
    }

    #[test]
    fn load_or_new_returns_empty_when_file_missing() {
        let dir = TempDir::new().expect("tmpdir");
        let path = dir.path().join("nonexistent.json");

        let m = ImportManifest::load_or_new(&path, "2.2.14-test").expect("load");
        assert_eq!(m.entries.len(), 0);
        assert_eq!(m.manifest_version, 1);
    }

    #[test]
    fn count_by_status_works() {
        let mut m = ImportManifest::new("2.2.14-test");
        m.push(sample_entry(ImportEntryStatus::Committed));
        m.push(sample_entry(ImportEntryStatus::Committed));
        m.push(sample_entry(ImportEntryStatus::Skipped));

        assert_eq!(m.count_by_status(&ImportEntryStatus::Committed), 2);
        assert_eq!(m.count_by_status(&ImportEntryStatus::Skipped), 1);
        assert_eq!(m.count_by_status(&ImportEntryStatus::Pending), 0);
    }
}
