//! Pending-approval proposals for routines whose `permission_mode` tier
//! is [`RoutineTier::Ask`].
//!
//! When the daemon's loop ([`crate::routines::executor::run_once`])
//! decides a due routine cannot fire on its own (because its permission
//! mode is `accept` or `danger`), it writes a JSON proposal to
//! `~/.anvil/routines/pending/<name>-<scheduled_at>.json` and *skips*
//! execution.  The TUI (`/schedule pending`) and the remote control
//! viewer surface the queue; the user runs `/schedule approve <name>`
//! to fire it once, or `/schedule reject <name>` to drop it.
//!
//! ## File naming
//!
//! Filenames are `<name>-<scheduled_at>.json`, padded so they sort
//! lexicographically by time when listed via `read_dir()`.  Approving
//! a routine deletes *all* pending proposals for that name (the user
//! is approving the routine, not a specific occurrence; running it
//! once flushes the queue).  Rejecting does the same.
//!
//! ## Expiry
//!
//! Proposals carry a `proposed_at` timestamp; [`list_pending`] filters
//! out anything older than [`PROPOSAL_TTL_SECS`] (24 h) and best-effort
//! deletes the stale file from disk.  This keeps an absent user from
//! returning to a wall of week-old proposals.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::routines::definition::{RoutineDef, RoutinePermissionMode, RoutineTier};

/// 24 hours.  Proposals older than this are silently dropped on the
/// next `list_pending()` call.
pub const PROPOSAL_TTL_SECS: u64 = 24 * 60 * 60;

/// JSON shape written to `~/.anvil/routines/pending/<name>-<ts>.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoutineProposal {
    /// Routine name (matches `RoutineDef::name`).
    pub routine: String,
    /// Unix epoch seconds — when the routine *would have* fired had it
    /// been auto-tier.  Distinct from `proposed_at` only when the
    /// daemon was sleeping past the scheduled time.
    pub scheduled_at: u64,
    /// Unix epoch seconds — when this proposal was written to disk.
    pub proposed_at: u64,
    /// Permission mode that triggered the ask-tier decision.  Recorded
    /// so the UI can say "danger mode — review carefully" without
    /// re-reading the definition.
    pub permission_mode: RoutinePermissionMode,
    /// Schedule string ("every 30m", "cron 0 9 * * *", etc.) so the
    /// user knows what cadence they're approving.
    pub schedule_raw: String,
    /// First ~200 chars of the routine prompt for quick context.
    pub prompt_preview: String,
    /// Absolute path to the routine TOML on disk.
    pub source_path: PathBuf,
}

impl RoutineProposal {
    /// Build a proposal from a [`RoutineDef`] + the timestamp the
    /// daemon decided not to fire on.
    #[must_use]
    pub fn from_def(def: &RoutineDef, scheduled_at: u64, proposed_at: u64) -> Self {
        let preview = preview_prompt(&def.prompt);
        Self {
            routine: def.name.clone(),
            scheduled_at,
            proposed_at,
            permission_mode: def.permission_mode,
            schedule_raw: def.schedule_raw.clone(),
            prompt_preview: preview,
            source_path: def.source_path.clone(),
        }
    }

    /// Tier shorthand for UI rendering.
    #[must_use]
    pub fn tier(&self) -> RoutineTier {
        self.permission_mode.tier()
    }

    /// Has this proposal aged past [`PROPOSAL_TTL_SECS`]?
    #[must_use]
    pub fn is_expired(&self, now: u64) -> bool {
        now.saturating_sub(self.proposed_at) > PROPOSAL_TTL_SECS
    }
}

/// `~/.anvil/routines/pending/` directory used by every entry point.
#[must_use]
pub fn pending_dir(home: &Path) -> PathBuf {
    home.join("routines").join("pending")
}

/// Write a proposal to disk.  Creates the pending dir on first use.
///
/// Returns the path the proposal was written to.  Idempotent: if a
/// proposal for this routine + scheduled_at already exists it is
/// overwritten (same content), so the daemon may call this safely on
/// every tick.
pub fn write_proposal(home: &Path, p: &RoutineProposal) -> Result<PathBuf, String> {
    let dir = pending_dir(home);
    fs::create_dir_all(&dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    let path = dir.join(filename(&p.routine, p.scheduled_at));
    let json = serde_json::to_string_pretty(p)
        .map_err(|e| format!("serialize proposal: {e}"))?;
    fs::write(&path, json).map_err(|e| format!("write {}: {e}", path.display()))?;
    Ok(path)
}

/// True when any pending proposal exists on disk for this routine,
/// regardless of timestamp.  Used by the daemon to skip writing a
/// duplicate when an earlier proposal hasn't been approved/rejected.
pub fn has_pending_for(home: &Path, routine: &str) -> bool {
    let dir = pending_dir(home);
    let Ok(entries) = fs::read_dir(&dir) else {
        return false;
    };
    let prefix = format!("{routine}-");
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(s) = name.to_str() else {
            continue;
        };
        if s.starts_with(&prefix) && s.ends_with(".json") {
            return true;
        }
    }
    false
}

/// List all pending proposals.  Expired entries are removed from disk
/// best-effort and excluded from the returned list.
///
/// Sorted by `proposed_at` ascending (oldest first).  Bad/unparseable
/// files are silently skipped — one rogue file never locks the queue.
#[must_use]
pub fn list_pending(home: &Path, now: u64) -> Vec<RoutineProposal> {
    let dir = pending_dir(home);
    let Ok(entries) = fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut out: Vec<RoutineProposal> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Ok(raw) = fs::read_to_string(&path) else {
            continue;
        };
        let Ok(p) = serde_json::from_str::<RoutineProposal>(&raw) else {
            continue;
        };
        if p.is_expired(now) {
            let _ = fs::remove_file(&path);
            continue;
        }
        out.push(p);
    }
    out.sort_by_key(|p| p.proposed_at);
    out
}

/// Delete every pending proposal for `routine`.  Used by both
/// `/schedule approve` and `/schedule reject` — approval runs the
/// routine once and clears the queue; rejection just clears.
///
/// Returns the number of files removed.
pub fn drop_pending_for(home: &Path, routine: &str) -> Result<usize, io::Error> {
    let dir = pending_dir(home);
    if !dir.exists() {
        return Ok(0);
    }
    let prefix = format!("{routine}-");
    let mut removed = 0usize;
    for entry in fs::read_dir(&dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(s) = name.to_str() else {
            continue;
        };
        if s.starts_with(&prefix) && s.ends_with(".json") {
            fs::remove_file(entry.path())?;
            removed += 1;
        }
    }
    Ok(removed)
}

/// `<routine>-<scheduled_at>.json`.  Padded width keeps lexicographic
/// order matching chronological order for the next ~317 years.
fn filename(routine: &str, scheduled_at: u64) -> String {
    format!("{routine}-{scheduled_at:010}.json")
}

/// Truncate the prompt to a single line of ~200 chars for the proposal
/// preview field.  Newlines become spaces so the UI can render it on
/// one row.
fn preview_prompt(prompt: &str) -> String {
    let flat: String = prompt
        .chars()
        .map(|c| if c.is_control() || c == '\n' { ' ' } else { c })
        .collect();
    let trimmed = flat.trim();
    if trimmed.chars().count() <= 200 {
        return trimmed.to_string();
    }
    let mut out: String = trimmed.chars().take(197).collect();
    out.push('…');
    out
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routines::definition::{DeliveryTarget, RoutineDef};
    use crate::routines::schedule::Schedule;
    use tempfile::TempDir;

    fn mk_def(name: &str, perm: RoutinePermissionMode) -> RoutineDef {
        RoutineDef {
            name: name.to_string(),
            schedule: Schedule::Interval(1800),
            schedule_raw: "every 30m".to_string(),
            prompt: "do the thing".to_string(),
            enabled: true,
            model: None,
            permission_mode: perm,
            cwd: None,
            context_from: Vec::new(),
            delivery: vec![DeliveryTarget::Local],
            source_path: PathBuf::from(format!("/tmp/{name}.toml")),
        }
    }

    /// Allocate a per-test home directory backed by `tempfile::TempDir`.
    ///
    /// Earlier revisions of this module rolled their own name from
    /// `process::id()` + `SystemTime::now().as_nanos()`. Under
    /// `cargo test`'s parallel runner every test in the module shares
    /// the same PID, and `SystemTime::now()` is not guaranteed to be
    /// monotonically distinct across threads — two threads entering
    /// `tmp_home()` at the same instant could collide on the same
    /// path. When that happened, `drop_pending_only_targets_named_routine`
    /// would see foreign proposals in the pending dir and the
    /// `list_pending(..).len() == 1` assertion would fail (task #741).
    ///
    /// `TempDir` uses the OS RNG to build the suffix and the handle
    /// auto-cleans on drop, eliminating both the collision risk and
    /// the manual `fs::remove_dir_all` book-keeping.
    fn tmp_home() -> TempDir {
        tempfile::Builder::new()
            .prefix("anvil-proposal-test-")
            .tempdir()
            .expect("create tempdir for proposal test")
    }

    #[test]
    fn proposal_round_trip_to_disk() {
        let home = tmp_home();
        let def = mk_def("nightly-deploy", RoutinePermissionMode::Danger);
        let p = RoutineProposal::from_def(&def, 1_000_000, 1_000_005);
        let path = write_proposal(home.path(), &p).expect("write");
        assert!(path.exists());
        let listed = list_pending(home.path(), 1_000_005);
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0], p);
    }

    #[test]
    fn expired_proposals_drop_silently() {
        let home = tmp_home();
        let def = mk_def("old-job", RoutinePermissionMode::Accept);
        let p = RoutineProposal::from_def(&def, 1_000_000, 1_000_000);
        write_proposal(home.path(), &p).unwrap();
        // 25h later
        let now = 1_000_000 + 25 * 60 * 60;
        let listed = list_pending(home.path(), now);
        assert!(listed.is_empty());
        // Files removed from disk too.
        let leftover: Vec<_> = fs::read_dir(pending_dir(home.path()))
            .map(|d| d.flatten().collect())
            .unwrap_or_default();
        assert!(leftover.is_empty());
    }

    #[test]
    fn drop_pending_only_targets_named_routine() {
        let home = tmp_home();
        let a = RoutineProposal::from_def(
            &mk_def("a", RoutinePermissionMode::Danger),
            1_000_000,
            1_000_000,
        );
        let b = RoutineProposal::from_def(
            &mk_def("b", RoutinePermissionMode::Danger),
            1_000_000,
            1_000_000,
        );
        write_proposal(home.path(), &a).unwrap();
        write_proposal(home.path(), &b).unwrap();
        let removed = drop_pending_for(home.path(), "a").unwrap();
        assert_eq!(removed, 1);
        let listed = list_pending(home.path(), 1_000_001);
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].routine, "b");
    }

    #[test]
    fn drop_pending_removes_all_occurrences_for_routine() {
        let home = tmp_home();
        let def = mk_def("recurring", RoutinePermissionMode::Danger);
        write_proposal(
            home.path(),
            &RoutineProposal::from_def(&def, 1_000_000, 1_000_000),
        )
        .unwrap();
        write_proposal(
            home.path(),
            &RoutineProposal::from_def(&def, 1_001_800, 1_001_800),
        )
        .unwrap();
        let removed = drop_pending_for(home.path(), "recurring").unwrap();
        assert_eq!(removed, 2);
    }

    #[test]
    fn has_pending_for_finds_existing_proposal() {
        let home = tmp_home();
        let def = mk_def("watch", RoutinePermissionMode::Accept);
        write_proposal(
            home.path(),
            &RoutineProposal::from_def(&def, 1_000_000, 1_000_000),
        )
        .unwrap();
        assert!(has_pending_for(home.path(), "watch"));
        assert!(!has_pending_for(home.path(), "other"));
    }

    #[test]
    fn proposal_preview_truncates_at_200_chars() {
        let long = "x".repeat(500);
        let preview = preview_prompt(&long);
        assert_eq!(preview.chars().count(), 198); // 197 + ellipsis
        assert!(preview.ends_with('…'));
    }

    #[test]
    fn proposal_preview_strips_newlines() {
        let multiline = "line one\nline two\nline three";
        assert_eq!(preview_prompt(multiline), "line one line two line three");
    }

    #[test]
    fn list_pending_skips_malformed_files() {
        let home = tmp_home();
        let dir = pending_dir(home.path());
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("garbage-0000001000.json"), "{ not valid json").unwrap();
        let def = mk_def("valid", RoutinePermissionMode::Danger);
        write_proposal(
            home.path(),
            &RoutineProposal::from_def(&def, 1_000_000, 1_000_000),
        )
        .unwrap();
        let listed = list_pending(home.path(), 1_000_005);
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].routine, "valid");
    }

    #[test]
    fn filename_pads_for_lexicographic_sort() {
        let early = filename("x", 100);
        let late = filename("x", 100_000);
        assert!(early < late, "lex order: {early} < {late}");
    }
}
