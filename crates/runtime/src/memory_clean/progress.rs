/// memory_clean::progress — Phase 6.5e (progress persistence)
///
/// Resumable progress tracking for `anvil memory clean`.
///
/// Progress is stored at `~/.anvil/.memory-clean-progress.json` and records
/// the content hashes of entries that have been fully cleaned.  Re-running
/// the command skips entries whose hash appears in the completed set.
///
/// If the user manually edits an imported entry after it was cleaned, the new
/// hash differs and the entry becomes eligible for clean again.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::import::{now_rfc3339, staging::anvil_config_home};

// ── CleanProgress ─────────────────────────────────────────────────────────────

/// Resumable progress state for `anvil memory clean`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CleanProgress {
    /// Content hashes of entries that were fully cleaned.
    pub completed: Vec<String>,
    /// RFC 3339 timestamp when the first run started.
    pub started_at: String,
    /// RFC 3339 timestamp of the most recent successful write.
    pub last_cleaned_at: Option<String>,
    /// Path of the last successfully cleaned file (for informational display).
    pub last_path: Option<String>,
}

impl CleanProgress {
    /// Load from `path`, or return a fresh default if the file does not exist.
    pub fn load(path: &Path) -> Self {
        let mut progress: Self = std::fs::read_to_string(path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        if progress.started_at.is_empty() {
            progress.started_at = now_rfc3339();
        }
        progress
    }

    /// Save to `path` atomically (temp file + rename).
    pub fn save(&self, path: &Path) -> Result<(), String> {
        if let Some(p) = path.parent() {
            std::fs::create_dir_all(p).map_err(|e| format!("create dir: {e}"))?;
        }
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| format!("serialize: {e}"))?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, &json).map_err(|e| format!("write tmp: {e}"))?;
        std::fs::rename(&tmp, path).map_err(|e| format!("rename: {e}"))?;
        Ok(())
    }

    /// Return `true` if `hash` is already in the completed set.
    pub fn is_done(&self, hash: &str) -> bool {
        self.completed.iter().any(|h| h == hash)
    }

    /// Mark `hash` as completed.
    pub fn mark_done(&mut self, hash: String) {
        if !self.completed.contains(&hash) {
            self.completed.push(hash);
            self.last_cleaned_at = Some(now_rfc3339());
        }
    }

    /// Return the default path: `~/.anvil/.memory-clean-progress.json`.
    pub fn default_path() -> PathBuf {
        anvil_config_home().join(".memory-clean-progress.json")
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn progress_save_load_round_trip() {
        let dir = TempDir::new().expect("tmpdir");
        let path = dir.path().join("progress.json");
        let mut p = CleanProgress {
            started_at: "2026-05-15T00:00:00Z".to_string(),
            ..Default::default()
        };
        p.mark_done("hash-abc".to_string());
        p.mark_done("hash-def".to_string());
        p.save(&path).expect("save");

        let loaded = CleanProgress::load(&path);
        assert_eq!(loaded.completed.len(), 2);
        assert!(loaded.is_done("hash-abc"));
        assert!(loaded.is_done("hash-def"));
        assert!(!loaded.is_done("hash-xyz"));
    }

    #[test]
    fn progress_is_done_returns_false_for_unknown() {
        let p = CleanProgress::default();
        assert!(!p.is_done("anything"));
    }

    #[test]
    fn progress_mark_done_is_idempotent() {
        let mut p = CleanProgress::default();
        p.mark_done("hash-abc".to_string());
        p.mark_done("hash-abc".to_string());
        assert_eq!(p.completed.len(), 1, "duplicate marks must be collapsed");
    }

    #[test]
    fn progress_load_nonexistent_returns_default() {
        let dir = TempDir::new().expect("tmpdir");
        let path = dir.path().join("nonexistent.json");
        let p = CleanProgress::load(&path);
        assert!(!p.started_at.is_empty(), "started_at must be set even on fresh load");
    }
}
