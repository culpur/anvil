//! Nominations — the knowledge discovery pipeline for Anvil's memory tiers.
//!
//! When the AI discovers patterns, conventions, or facts during a session,
//! they are classified by sensitivity and routed:
//!   - Credentials → auto-vault (AES-256-GCM encrypted)
//!   - Infrastructure → private project memory (encrypted)
//!   - Knowledge → nomination (JSON, queued for review)
//!
//! Accepted nominations are promoted to ANVIL.md or QMD for future sessions.

use std::fs;
use std::io;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::config::default_config_home;

// ─── Nomination Schema ──────────────────────────────────────────────────────

/// A single knowledge nomination — a fact discovered during a session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Nomination {
    /// Unique ID: `nom-{timestamp}-{short_hash}`
    pub id: String,
    /// When the nomination was created.
    pub created_at: String,
    /// Session that generated the nomination.
    pub session_id: String,
    /// Category of the learning.
    pub category: NominationCategory,
    /// The actual content (pattern, convention, architecture fact, etc.)
    pub content: String,
    /// AI confidence in the accuracy of this learning (0.0 - 1.0).
    pub confidence: f64,
    /// Current status in the review pipeline.
    pub status: NominationStatus,
    /// Where the nomination was promoted to (if accepted).
    pub promoted_to: Option<String>,
}

/// Categories of knowledge that can be nominated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NominationCategory {
    /// Code pattern or convention (e.g., "uses UUID primary keys")
    Pattern,
    /// Project convention (e.g., "tests go in __tests__/")
    Convention,
    /// Architecture fact (e.g., "microservices communicate via gRPC")
    Architecture,
    /// Workflow preference (e.g., "deploy via git pull, not rsync")
    Workflow,
    /// Tool/library preference (e.g., "uses Prisma over TypeORM")
    ToolPreference,
    /// Configuration fact (e.g., "Vite config uses .js over .ts")
    Configuration,
}

/// Status of a nomination in the review pipeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NominationStatus {
    /// Awaiting review.
    Pending,
    /// Accepted and promoted to project memory or QMD.
    Accepted,
    /// Rejected by the user.
    Rejected,
}

// ─── Nomination Store ───────────────────────────────────────────────────────

/// Manages the nominations directory and CRUD operations.
pub struct NominationStore {
    dir: PathBuf,
}

impl NominationStore {
    /// Create a new store rooted at `~/.anvil/nominations/`.
    #[must_use]
    pub fn new() -> Self {
        let dir = default_config_home().join("nominations");
        Self { dir }
    }

    /// Create a store at a custom path (for testing).
    #[must_use]
    pub fn with_dir(dir: PathBuf) -> Self {
        Self { dir }
    }

    /// Ensure the nominations directory exists.
    pub fn ensure_dir(&self) -> io::Result<()> {
        fs::create_dir_all(&self.dir)
    }

    /// Create a new nomination and persist it to disk.
    pub fn create(
        &self,
        session_id: &str,
        category: NominationCategory,
        content: &str,
        confidence: f64,
    ) -> io::Result<Nomination> {
        self.ensure_dir()?;

        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default();
        let timestamp = now.as_secs();
        let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
        let id = format!("nom-{timestamp}-{seq:04x}");

        let nomination = Nomination {
            id: id.clone(),
            created_at: format_timestamp(timestamp),
            session_id: session_id.to_string(),
            category,
            content: content.to_string(),
            confidence,
            status: NominationStatus::Pending,
            promoted_to: None,
        };

        let path = self.dir.join(format!("{id}.json"));
        let json = serde_json::to_string_pretty(&nomination)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        fs::write(path, json)?;

        Ok(nomination)
    }

    /// List all nominations, optionally filtered by status.
    pub fn list(&self, status_filter: Option<NominationStatus>) -> Vec<Nomination> {
        let mut nominations = Vec::new();
        let Ok(entries) = fs::read_dir(&self.dir) else {
            return nominations;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "json") {
                if let Ok(content) = fs::read_to_string(&path) {
                    if let Ok(nom) = serde_json::from_str::<Nomination>(&content) {
                        if status_filter.is_none() || status_filter == Some(nom.status) {
                            nominations.push(nom);
                        }
                    }
                }
            }
        }
        nominations.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        nominations
    }

    /// Count pending nominations.
    pub fn pending_count(&self) -> usize {
        self.list(Some(NominationStatus::Pending)).len()
    }

    /// Accept a nomination and mark where it was promoted.
    pub fn accept(&self, id: &str, promoted_to: &str) -> io::Result<()> {
        self.update_status(id, NominationStatus::Accepted, Some(promoted_to))
    }

    /// Reject a nomination.
    pub fn reject(&self, id: &str) -> io::Result<()> {
        self.update_status(id, NominationStatus::Rejected, None)
    }

    /// Update the status of a nomination on disk.
    fn update_status(
        &self,
        id: &str,
        status: NominationStatus,
        promoted_to: Option<&str>,
    ) -> io::Result<()> {
        let path = self.dir.join(format!("{id}.json"));
        let content = fs::read_to_string(&path)?;
        let mut nom: Nomination = serde_json::from_str(&content)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        nom.status = status;
        nom.promoted_to = promoted_to.map(String::from);
        let json = serde_json::to_string_pretty(&nom)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        // Atomic write
        let tmp = self.dir.join(format!("{id}.json.tmp"));
        fs::write(&tmp, json)?;
        fs::rename(&tmp, &path)?;
        Ok(())
    }

    /// Get a single nomination by ID.
    pub fn get(&self, id: &str) -> Option<Nomination> {
        let path = self.dir.join(format!("{id}.json"));
        let content = fs::read_to_string(&path).ok()?;
        serde_json::from_str(&content).ok()
    }

    /// Format pending nominations for display.
    pub fn format_pending(&self) -> String {
        let pending = self.list(Some(NominationStatus::Pending));
        if pending.is_empty() {
            return "No pending nominations.\n\nNominations are created when Anvil discovers patterns and conventions during sessions.".to_string();
        }
        let mut lines = vec![format!("📋 Knowledge Nominations ({} pending)\n", pending.len())];
        for (i, nom) in pending.iter().enumerate() {
            lines.push(format!(
                "  {}. [{}] {}\n     Category: {:?} | Confidence: {:.0}% | Session: {}\n",
                i + 1,
                &nom.id[4..16.min(nom.id.len())], // short ID
                nom.content,
                nom.category,
                nom.confidence * 100.0,
                &nom.session_id,
            ));
        }
        lines.push("\nUse /knowledge accept <N> or /knowledge reject <N> to review.".to_string());
        lines.join("\n")
    }
}

impl Default for NominationStore {
    fn default() -> Self {
        Self::new()
    }
}

// ─── Helpers ────────────────────────────────────────────────────────────────

fn format_timestamp(secs: u64) -> String {
    // Simple ISO-ish format without chrono dependency
    let days = secs / 86400;
    let time_of_day = secs % 86400;
    let hours = time_of_day / 3600;
    let minutes = (time_of_day % 3600) / 60;
    let seconds = time_of_day % 60;

    // Rough date calculation (from epoch)
    let mut year = 1970u32;
    let mut remaining_days = days;
    loop {
        let days_in_year = if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) { 366 } else { 365 };
        if remaining_days < days_in_year {
            break;
        }
        remaining_days -= days_in_year;
        year += 1;
    }
    let month_days = [31, if year % 4 == 0 { 29 } else { 28 }, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let mut month = 1u32;
    for &md in &month_days {
        if remaining_days < md {
            break;
        }
        remaining_days -= md;
        month += 1;
    }
    let day = remaining_days + 1;

    format!("{year}-{month:02}-{day:02}T{hours:02}:{minutes:02}:{seconds:02}Z")
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn test_store() -> (NominationStore, tempfile::TempDir) {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = NominationStore::with_dir(tmp.path().to_path_buf());
        store.ensure_dir().unwrap();
        (store, tmp)
    }

    #[test]
    fn create_and_list_nominations() {
        let (store, _tmp) = test_store();
        store.create("session-1", NominationCategory::Pattern, "Uses UUID primary keys", 0.9).unwrap();
        store.create("session-1", NominationCategory::Convention, "Tests in __tests__/", 0.8).unwrap();

        let all = store.list(None);
        assert_eq!(all.len(), 2);

        let pending = store.list(Some(NominationStatus::Pending));
        assert_eq!(pending.len(), 2);
    }

    #[test]
    fn accept_and_reject_nominations() {
        let (store, _tmp) = test_store();
        let nom = store.create("session-1", NominationCategory::Pattern, "Uses Prisma", 0.85).unwrap();

        store.accept(&nom.id, "ANVIL.md").unwrap();
        let updated = store.get(&nom.id).unwrap();
        assert_eq!(updated.status, NominationStatus::Accepted);
        assert_eq!(updated.promoted_to.as_deref(), Some("ANVIL.md"));

        // Pending count should be 0
        assert_eq!(store.pending_count(), 0);
    }

    #[test]
    fn reject_removes_from_pending() {
        let (store, _tmp) = test_store();
        let nom = store.create("session-1", NominationCategory::Workflow, "Deploy via git pull", 0.95).unwrap();

        store.reject(&nom.id).unwrap();
        let updated = store.get(&nom.id).unwrap();
        assert_eq!(updated.status, NominationStatus::Rejected);
        assert_eq!(store.pending_count(), 0);
    }

    #[test]
    fn format_pending_shows_nominations() {
        let (store, _tmp) = test_store();
        store.create("session-1", NominationCategory::Pattern, "Uses UUID primary keys", 0.9).unwrap();
        let output = store.format_pending();
        assert!(output.contains("Knowledge Nominations"));
        assert!(output.contains("UUID primary keys"));
        assert!(output.contains("1 pending"));
    }

    #[test]
    fn empty_store_returns_helpful_message() {
        let (store, _tmp) = test_store();
        let output = store.format_pending();
        assert!(output.contains("No pending nominations"));
    }

    #[test]
    fn nomination_serializes_to_json() {
        let (store, _tmp) = test_store();
        let nom = store.create("session-1", NominationCategory::Architecture, "Microservices via gRPC", 0.75).unwrap();
        let json = serde_json::to_string(&nom).unwrap();
        assert!(json.contains("architecture"));
        assert!(json.contains("gRPC"));
        let deserialized: Nomination = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.id, nom.id);
    }
}
