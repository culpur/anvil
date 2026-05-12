//! Persistent goals for Anvil — Codex-style per-project goal tracking.
//!
//! Storage layout:
//!   `~/.anvil/goals/<project-path-hash>/<goal-id>.json`
//!
//! Each goal is a JSON file.  Writes are atomic (write to `.tmp`, then rename).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use rand::Rng as _;
use serde::{Deserialize, Serialize};

use crate::config::default_config_home;
use crate::memory::project_path_hash;

// ── Constants ─────────────────────────────────────────────────────────────────

/// Maximum number of characters in a goal description.
pub const GOAL_DESCRIPTION_MAX: usize = 4096;
/// Truncation length for goal description in list view.
pub const GOAL_LIST_DESCRIPTION_TRUNCATE: usize = 80;
/// Truncation length for status-line display.
pub const GOAL_STATUS_LINE_TRUNCATE: usize = 60;
/// Truncation length for active-goal system-prompt injection.
const GOAL_PROMPT_INJECT_TRUNCATE: usize = 200;

// ── GoalStatus ────────────────────────────────────────────────────────────────

/// Status of a persistent goal.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum GoalStatus {
    Active,
    Paused,
    Done,
    Archived,
}

impl std::fmt::Display for GoalStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Active => write!(f, "active"),
            Self::Paused => write!(f, "paused"),
            Self::Done => write!(f, "done"),
            Self::Archived => write!(f, "archived"),
        }
    }
}

// ── Goal ─────────────────────────────────────────────────────────────────────

/// A single persistent goal.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Goal {
    /// Unique ID: `g-<unix_secs>-<6char_random_hex>`.
    pub id: String,
    /// Unix timestamp (seconds since epoch) when the goal was created.
    pub created_at: u64,
    /// Human-readable description, capped at [`GOAL_DESCRIPTION_MAX`] chars.
    pub description: String,
    /// Current status.
    pub status: GoalStatus,
    /// Session IDs linked to this goal.
    pub session_ids: Vec<String>,
    /// Unix timestamp of the last `resume` call, if any.
    pub last_resumed_at: Option<u64>,
    /// Free-form tags.
    pub tags: Vec<String>,
    /// Milestone strings.
    pub milestones: Vec<String>,
    /// Open items / TODOs.
    pub open_items: Vec<String>,
}

// ── GoalError ─────────────────────────────────────────────────────────────────

/// Errors from goal operations.
#[derive(Debug)]
pub enum GoalError {
    /// Goal ID was not found.
    NotFound(String),
    /// Alias used in existing main.rs pattern-match arms.
    GoalNotFound(String),
    /// Description exceeds the maximum length.
    DescriptionTooLong {
        /// Actual length of the provided description.
        len: usize,
        /// Maximum allowed length.
        max: usize,
    },
    /// Description was empty.
    DescriptionEmpty,
    /// No active goal exists.
    NoActiveGoal,
    /// Underlying I/O error.
    Io(io::Error),
    /// JSON (de)serialization error.
    Json(serde_json::Error),
}

impl std::fmt::Display for GoalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound(id) | Self::GoalNotFound(id) => write!(f, "goal not found: {id}"),
            Self::DescriptionTooLong { len, max } => {
                write!(f, "description too long: {len} chars (max {max})")
            }
            Self::DescriptionEmpty => write!(f, "description empty"),
            Self::NoActiveGoal => write!(f, "no active goal"),
            Self::Io(e) => write!(f, "io error: {e}"),
            Self::Json(e) => write!(f, "json error: {e}"),
        }
    }
}

impl std::error::Error for GoalError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            Self::Json(e) => Some(e),
            _ => None,
        }
    }
}

impl From<io::Error> for GoalError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<serde_json::Error> for GoalError {
    fn from(e: serde_json::Error) -> Self {
        Self::Json(e)
    }
}

// ── GoalManager ───────────────────────────────────────────────────────────────

/// Manages persistent goals for a single project directory.
pub struct GoalManager {
    /// The project root (used for hash-based directory calculation).
    #[allow(dead_code)]
    project_root: PathBuf,
    /// The directory where goal JSON files are stored.
    goals_dir: PathBuf,
}

impl GoalManager {
    /// Construct a manager for `project_root`.
    ///
    /// Storage is automatically placed under
    /// `~/.anvil/goals/<project-path-hash>/`.
    ///
    /// This function is infallible; directory creation is deferred to the
    /// first write operation.
    #[must_use]
    pub fn new(project_root: PathBuf) -> Self {
        let canonical = project_root
            .canonicalize()
            .unwrap_or_else(|_| project_root.clone());
        let hash = project_path_hash(&canonical);
        let goals_dir = default_config_home().join("goals").join(hash);
        Self { project_root: canonical, goals_dir }
    }

    /// Construct a manager rooted at a specific `goals_dir` (useful in tests).
    #[must_use]
    pub fn with_goals_dir(project_root: PathBuf, goals_dir: PathBuf) -> Self {
        Self { project_root, goals_dir }
    }

    /// The goals directory for this project.
    #[must_use]
    pub fn goals_dir(&self) -> &Path {
        &self.goals_dir
    }

    // ── Write helpers ─────────────────────────────────────────────────────────

    /// Ensure the goals directory exists.
    fn ensure_dir(&self) -> Result<(), GoalError> {
        fs::create_dir_all(&self.goals_dir)?;
        Ok(())
    }

    /// Compute the path for a goal file.
    fn goal_path(&self, id: &str) -> PathBuf {
        self.goals_dir.join(format!("{id}.json"))
    }

    /// Write `goal` to disk atomically (write `.tmp`, then rename).
    fn save_goal(&self, goal: &Goal) -> Result<(), GoalError> {
        self.ensure_dir()?;
        let final_path = self.goal_path(&goal.id);
        let tmp_path = self.goals_dir.join(format!("{}.tmp", goal.id));
        let json = serde_json::to_string_pretty(goal)?;
        fs::write(&tmp_path, &json)?;
        fs::rename(&tmp_path, &final_path)?;
        Ok(())
    }

    /// Load a goal from disk by `id`.  Returns `Err(GoalNotFound)` if absent.
    fn load_goal(&self, id: &str) -> Result<Goal, GoalError> {
        let path = self.goal_path(id);
        let raw = fs::read_to_string(&path).map_err(|e| {
            if e.kind() == io::ErrorKind::NotFound {
                GoalError::GoalNotFound(id.to_owned())
            } else {
                GoalError::Io(e)
            }
        })?;
        let goal: Goal = serde_json::from_str(&raw)?;
        Ok(goal)
    }

    // ── Public API ────────────────────────────────────────────────────────────

    /// Create a new goal with `description` and immediately set it to Active.
    ///
    /// If another goal is already Active it is **not** paused here; use
    /// [`resume`] for that.  (Creating a goal just makes a new one active.)
    ///
    /// # Errors
    /// Returns [`GoalError::DescriptionEmpty`] or
    /// [`GoalError::DescriptionTooLong`] for invalid descriptions.
    pub fn new_goal(&mut self, description: impl Into<String>) -> Result<Goal, GoalError> {
        let description = description.into();
        if description.is_empty() {
            return Err(GoalError::DescriptionEmpty);
        }
        let len = description.chars().count();
        if len > GOAL_DESCRIPTION_MAX {
            return Err(GoalError::DescriptionTooLong { len, max: GOAL_DESCRIPTION_MAX });
        }
        let now = unix_now();
        let id = generate_goal_id(now);
        let goal = Goal {
            id,
            created_at: now,
            description,
            status: GoalStatus::Active,
            session_ids: Vec::new(),
            last_resumed_at: None,
            tags: Vec::new(),
            milestones: Vec::new(),
            open_items: Vec::new(),
        };
        self.save_goal(&goal)?;
        Ok(goal)
    }

    /// Alias for [`new_goal`] — kept for backward compatibility with existing
    /// call sites that used `create`.
    pub fn create(&mut self, description: &str) -> Result<Goal, GoalError> {
        self.new_goal(description)
    }

    /// Create a goal and link it to `session_id` in a single call.
    ///
    /// CC-139-F2 unify: CC's `/goal` is session-scoped while Anvil's is
    /// project-persistent.  Auto-linking the active session on create
    /// gives users CC's mental model (this goal belongs to this session)
    /// without losing Anvil's cross-session goal tracking.
    ///
    /// # Errors
    /// Same as [`new_goal`]; also returns [`GoalError::Io`] / [`GoalError::Json`]
    /// if the link write fails.
    pub fn new_goal_for_session(
        &mut self,
        description: impl Into<String>,
        session_id: &str,
    ) -> Result<Goal, GoalError> {
        let goal = self.new_goal(description)?;
        self.link_session(&goal.id, session_id)?;
        // Re-load so the returned struct includes the session link.
        self.load_goal(&goal.id)
    }

    /// List all goals for this project.
    ///
    /// Sort order: Active → Paused → Done → Archived; newest-created-first
    /// within each bucket.
    pub fn list(&self) -> Result<Vec<Goal>, GoalError> {
        if !self.goals_dir.exists() {
            return Ok(Vec::new());
        }
        let mut goals = Vec::new();
        for entry in fs::read_dir(&self.goals_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let raw = fs::read_to_string(&path)?;
            match serde_json::from_str::<Goal>(&raw) {
                Ok(g) => goals.push(g),
                Err(_) => continue, // skip corrupt files
            }
        }
        goals.sort_by(|a, b| {
            let bucket = |s: &GoalStatus| match s {
                GoalStatus::Active => 0u8,
                GoalStatus::Paused => 1,
                GoalStatus::Done => 2,
                GoalStatus::Archived => 3,
            };
            let ba = bucket(&a.status);
            let bb = bucket(&b.status);
            ba.cmp(&bb).then_with(|| b.created_at.cmp(&a.created_at))
        });
        Ok(goals)
    }

    /// Return a specific goal by `id`.
    ///
    /// Returns `Err(GoalNotFound)` if no such goal exists.
    pub fn get(&self, id: &str) -> Result<Goal, GoalError> {
        self.load_goal(id)
    }

    /// Return the currently Active goal, or `None` if no goal is active.
    ///
    /// This is infallible at the API level — if the goals directory does not
    /// exist or is empty, returns `Ok(None)`.
    pub fn active_goal(&self) -> Result<Option<Goal>, GoalError> {
        if !self.goals_dir.exists() {
            return Ok(None);
        }
        for entry in fs::read_dir(&self.goals_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let raw = match fs::read_to_string(&path) {
                Ok(r) => r,
                Err(_) => continue,
            };
            if let Ok(g) = serde_json::from_str::<Goal>(&raw) {
                if g.status == GoalStatus::Active {
                    return Ok(Some(g));
                }
            }
        }
        Ok(None)
    }

    /// Set goal `id` to Active, auto-pausing any previously Active goal first.
    ///
    /// Both the pause of the old goal and the activation of the new one happen
    /// atomically within the same call (each individual file write is atomic).
    pub fn resume(&mut self, id: &str) -> Result<Goal, GoalError> {
        // Auto-pause any currently active goal (if it's not the same one).
        if let Ok(Some(current)) = self.active_goal() {
            if current.id != id {
                let mut paused = current;
                paused.status = GoalStatus::Paused;
                self.save_goal(&paused)?;
            }
        }
        // Load and activate the requested goal.
        let mut goal = self.load_goal(id)?;
        goal.status = GoalStatus::Active;
        goal.last_resumed_at = Some(unix_now());
        self.save_goal(&goal)?;
        Ok(goal)
    }

    /// Set a goal to Paused.
    ///
    /// If `id` is `None`, pauses the currently Active goal.
    /// Returns `Err(NoActiveGoal)` if `id` is `None` and there is no Active goal.
    pub fn pause(&mut self, id: Option<&str>) -> Result<Goal, GoalError> {
        let goal = match id {
            Some(id) => self.load_goal(id).map_err(|_| GoalError::GoalNotFound(id.to_owned()))?,
            None => self.active_goal()?.ok_or(GoalError::NoActiveGoal)?,
        };
        let mut goal = goal;
        goal.status = GoalStatus::Paused;
        self.save_goal(&goal)?;
        Ok(goal)
    }

    /// Mark a goal Done.  The file is kept on disk (status = Done).
    ///
    /// If `id` is `None`, marks the currently Active goal.
    /// Returns `Err(NoActiveGoal)` if `id` is `None` and there is no Active goal.
    pub fn done(&mut self, id: Option<&str>) -> Result<Goal, GoalError> {
        let goal = match id {
            Some(id) => self.load_goal(id).map_err(|_| GoalError::GoalNotFound(id.to_owned()))?,
            None => self.active_goal()?.ok_or(GoalError::NoActiveGoal)?,
        };
        let mut goal = goal;
        goal.status = GoalStatus::Done;
        self.save_goal(&goal)?;
        Ok(goal)
    }

    /// Return a goal by `id` (for the `show` sub-command).
    ///
    /// `id` = `None` returns the currently Active goal, or `Err(NoActiveGoal)`.
    pub fn show(&self, id: Option<&str>) -> Result<Goal, GoalError> {
        match id {
            Some(id) => self.load_goal(id),
            None => self.active_goal()?.ok_or(GoalError::NoActiveGoal),
        }
    }

    /// Append `session_id` to `goal_id`'s `session_ids` list (idempotent).
    pub fn link_session(&mut self, goal_id: &str, session_id: &str) -> Result<(), GoalError> {
        let mut goal = self.load_goal(goal_id)?;
        if !goal.session_ids.iter().any(|s| s == session_id) {
            goal.session_ids.push(session_id.to_owned());
            self.save_goal(&goal)?;
        }
        Ok(())
    }

    /// Archive `id` (internal; not exposed as a slash command in v2.2.11).
    #[allow(dead_code)]
    fn archive(&mut self, id: &str) -> Result<Goal, GoalError> {
        let mut goal = self.load_goal(id)?;
        goal.status = GoalStatus::Archived;
        self.save_goal(&goal)?;
        Ok(goal)
    }

    /// Set a goal's status directly (kept for backward compat).
    pub fn set_status(&mut self, id: &str, status: GoalStatus) -> Result<(), GoalError> {
        let mut goal = self.load_goal(id)?;
        goal.status = status;
        self.save_goal(&goal)?;
        Ok(())
    }
}

// ── Formatting helpers ────────────────────────────────────────────────────────

/// Format a list of goals for display.
#[must_use]
pub fn format_goal_list(goals: &[Goal]) -> String {
    if goals.is_empty() {
        return "No goals yet. Use /goal new \"<description>\" to create one.".into();
    }
    let mut out = String::new();
    for g in goals {
        let desc = truncate_str(&g.description, GOAL_LIST_DESCRIPTION_TRUNCATE);
        out.push_str(&format!("[{}] {} — {}\n", g.status, g.id, desc));
    }
    out
}

/// Format a single goal for show.
#[must_use]
pub fn format_goal_show(goal: &Goal) -> String {
    let mut lines = vec![
        format!("Goal:        {}", goal.id),
        format!("Status:      {}", goal.status),
        format!("Created:     {}", unix_to_rfc3339(goal.created_at)),
        format!("Description: {}", goal.description),
    ];
    if let Some(r) = goal.last_resumed_at {
        lines.push(format!("Resumed:     {}", unix_to_rfc3339(r)));
    }
    if !goal.tags.is_empty() {
        lines.push(format!("Tags:        {}", goal.tags.join(", ")));
    }
    if !goal.milestones.is_empty() {
        lines.push(format!("Milestones:"));
        for m in &goal.milestones {
            lines.push(format!("  - {m}"));
        }
    }
    if !goal.open_items.is_empty() {
        lines.push(format!("Open items:"));
        for item in &goal.open_items {
            lines.push(format!("  - {item}"));
        }
    }
    if !goal.session_ids.is_empty() {
        lines.push(format!("Sessions:    {}", goal.session_ids.join(", ")));
    }
    lines.join("\n")
}

/// Build the system-prompt fragment for the active goal injection.
///
/// Injects a single line:
/// `<active-goal id="<id>">{description (truncated to 200 chars)}</active-goal>`
#[must_use]
pub fn build_active_goal_prompt_fragment(goal: &Goal) -> String {
    let desc = truncate_str(&goal.description, GOAL_PROMPT_INJECT_TRUNCATE);
    format!("<active-goal id=\"{}\">{desc}</active-goal>", goal.id)
}

// ── Private helpers ───────────────────────────────────────────────────────────

/// Return seconds since UNIX epoch, saturating on overflow.
fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Generate a goal ID: `g-<unix_secs>-<6char_random_hex>`.
fn generate_goal_id(now_secs: u64) -> String {
    let mut rng = rand::thread_rng();
    let rand_bytes: [u8; 3] = rng.r#gen();
    format!("g-{now_secs}-{:02x}{:02x}{:02x}", rand_bytes[0], rand_bytes[1], rand_bytes[2])
}

/// Truncate a string to at most `max_chars` Unicode scalar values,
/// appending `…` if truncated.
fn truncate_str(s: &str, max_chars: usize) -> String {
    let mut chars = s.chars();
    let collected: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{collected}…")
    } else {
        collected
    }
}

/// Format a Unix timestamp as a minimal ISO-8601 UTC string without chrono.
fn unix_to_rfc3339(secs: u64) -> String {
    // Use a simple hand-rolled conversion to keep the chrono dependency out.
    // Accurate for dates from 1970 to ~2106.
    let mut remaining = secs;
    let s = remaining % 60;
    remaining /= 60;
    let m = remaining % 60;
    remaining /= 60;
    let h = remaining % 24;
    remaining /= 24;
    // Days since 1970-01-01
    let (year, month, day) = days_to_ymd(remaining);
    format!("{year:04}-{month:02}-{day:02}T{h:02}:{m:02}:{s:02}Z")
}

/// Convert days-since-epoch to (year, month, day).
fn days_to_ymd(days: u64) -> (u32, u32, u32) {
    // Algorithm: http://howardhinnant.github.io/date_algorithms.html (civil_from_days)
    let z = days as i64 + 719468;
    let era: i64 = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (u32::try_from(y).unwrap_or(1970), u32::try_from(m).unwrap_or(1), u32::try_from(d).unwrap_or(1))
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Create a GoalManager whose goals_dir is an isolated temp directory.
    fn test_manager(tmp: &TempDir) -> GoalManager {
        GoalManager::with_goals_dir(
            tmp.path().to_path_buf(),
            tmp.path().join("goals"),
        )
    }

    // ── 1. new_goal_creates_active_goal ───────────────────────────────────────

    #[test]
    fn new_goal_creates_active_goal() {
        let tmp = TempDir::new().unwrap();
        let mut mgr = test_manager(&tmp);
        let goal = mgr.new_goal("implement feature X").unwrap();
        assert_eq!(goal.status, GoalStatus::Active);
        assert!(!goal.id.is_empty());
        assert_eq!(goal.description, "implement feature X");

        // File must exist on disk.
        let path = mgr.goals_dir().join(format!("{}.json", goal.id));
        assert!(path.exists(), "goal file should exist on disk");

        // Must be loadable.
        let loaded = mgr.get(&goal.id).unwrap();
        assert_eq!(loaded.id, goal.id);
        assert_eq!(loaded.status, GoalStatus::Active);
    }

    // ── 2. new_goal_rejects_empty_description ─────────────────────────────────

    #[test]
    fn new_goal_rejects_empty_description() {
        let tmp = TempDir::new().unwrap();
        let mut mgr = test_manager(&tmp);
        let err = mgr.new_goal("").unwrap_err();
        assert!(matches!(err, GoalError::DescriptionEmpty));
    }

    // ── 3. new_goal_rejects_description_over_4096_chars ──────────────────────

    #[test]
    fn new_goal_rejects_description_over_4096_chars() {
        let tmp = TempDir::new().unwrap();
        let mut mgr = test_manager(&tmp);
        let long = "a".repeat(GOAL_DESCRIPTION_MAX + 1);
        let err = mgr.new_goal(long).unwrap_err();
        assert!(
            matches!(err, GoalError::DescriptionTooLong { len, .. } if len > GOAL_DESCRIPTION_MAX),
            "expected DescriptionTooLong"
        );
    }

    // ── 4. list_returns_empty_for_fresh_project ───────────────────────────────

    #[test]
    fn list_returns_empty_for_fresh_project() {
        let tmp = TempDir::new().unwrap();
        let mgr = test_manager(&tmp);
        let goals = mgr.list().unwrap();
        assert!(goals.is_empty());
    }

    // ── 5. list_returns_goals_in_status_order_active_paused_done ─────────────

    #[test]
    fn list_returns_goals_in_status_order_active_paused_done() {
        let tmp = TempDir::new().unwrap();
        let mut mgr = test_manager(&tmp);

        let done_goal = mgr.new_goal("done goal").unwrap();
        mgr.done(Some(&done_goal.id)).unwrap();

        let paused_goal = mgr.new_goal("paused goal").unwrap();
        mgr.pause(Some(&paused_goal.id)).unwrap();

        mgr.new_goal("active goal").unwrap();

        let goals = mgr.list().unwrap();
        assert_eq!(goals.len(), 3);
        assert_eq!(goals[0].status, GoalStatus::Active, "first should be active");
        assert_eq!(goals[1].status, GoalStatus::Paused, "second should be paused");
        assert_eq!(goals[2].status, GoalStatus::Done, "third should be done");
    }

    // ── 6. list_within_bucket_sorts_newest_first ──────────────────────────────

    #[test]
    fn list_within_bucket_sorts_newest_first() {
        let tmp = TempDir::new().unwrap();
        let mut mgr = test_manager(&tmp);

        // Create two goals and immediately pause them; manipulate created_at
        // directly by saving goals with different timestamps.
        let mut g_old = mgr.new_goal("older paused").unwrap();
        let g_new = mgr.new_goal("newer paused").unwrap();

        // Make g_old appear older.
        g_old.created_at = g_old.created_at.saturating_sub(100);
        g_old.status = GoalStatus::Paused;
        mgr.save_goal(&g_old).unwrap();

        let mut g_new2 = g_new.clone();
        g_new2.status = GoalStatus::Paused;
        mgr.save_goal(&g_new2).unwrap();

        let goals = mgr.list().unwrap();
        // Both paused — newer should come first.
        let paused: Vec<_> = goals.iter().filter(|g| g.status == GoalStatus::Paused).collect();
        assert_eq!(paused.len(), 2);
        assert!(
            paused[0].created_at >= paused[1].created_at,
            "newer goal should be first: {} >= {}",
            paused[0].created_at, paused[1].created_at
        );
    }

    // ── 7. resume_auto_pauses_previous_active ─────────────────────────────────

    #[test]
    fn resume_auto_pauses_previous_active() {
        let tmp = TempDir::new().unwrap();
        let mut mgr = test_manager(&tmp);

        let g1 = mgr.new_goal("goal one").unwrap();
        assert_eq!(g1.status, GoalStatus::Active);

        // Pause it before creating g2 (to avoid double-active confusion during creation)
        mgr.pause(None).unwrap();
        let g2 = mgr.new_goal("goal two").unwrap();
        // g2 is now active; resume g1 — should auto-pause g2
        let resumed = mgr.resume(&g1.id).unwrap();
        assert_eq!(resumed.status, GoalStatus::Active);

        let g2_on_disk = mgr.get(&g2.id).unwrap();
        assert_eq!(
            g2_on_disk.status,
            GoalStatus::Paused,
            "previously active goal should be paused"
        );
    }

    // ── 8. resume_nonexistent_returns_not_found ───────────────────────────────

    #[test]
    fn resume_nonexistent_returns_not_found() {
        let tmp = TempDir::new().unwrap();
        let mut mgr = test_manager(&tmp);
        let err = mgr.resume("g-0000000000-zzzzzz").unwrap_err();
        assert!(matches!(err, GoalError::GoalNotFound(_) | GoalError::NotFound(_)));
    }

    // ── 9. pause_with_id_pauses_specific_goal ────────────────────────────────

    #[test]
    fn pause_with_id_pauses_specific_goal() {
        let tmp = TempDir::new().unwrap();
        let mut mgr = test_manager(&tmp);

        let goal = mgr.new_goal("a specific goal").unwrap();
        let paused = mgr.pause(Some(&goal.id)).unwrap();
        assert_eq!(paused.status, GoalStatus::Paused);

        let on_disk = mgr.get(&goal.id).unwrap();
        assert_eq!(on_disk.status, GoalStatus::Paused);
    }

    // ── 10. pause_no_id_pauses_active_goal ───────────────────────────────────

    #[test]
    fn pause_no_id_pauses_active_goal() {
        let tmp = TempDir::new().unwrap();
        let mut mgr = test_manager(&tmp);

        let goal = mgr.new_goal("active goal to pause").unwrap();
        let paused = mgr.pause(None).unwrap();
        assert_eq!(paused.status, GoalStatus::Paused);
        assert_eq!(paused.id, goal.id);
    }

    // ── 11. pause_no_id_no_active_returns_not_found ──────────────────────────

    #[test]
    fn pause_no_id_no_active_returns_not_found() {
        let tmp = TempDir::new().unwrap();
        let mut mgr = test_manager(&tmp);
        let err = mgr.pause(None).unwrap_err();
        assert!(matches!(err, GoalError::NoActiveGoal));
    }

    // ── 12. done_marks_goal_done_keeps_file ──────────────────────────────────

    #[test]
    fn done_marks_goal_done_keeps_file() {
        let tmp = TempDir::new().unwrap();
        let mut mgr = test_manager(&tmp);

        let goal = mgr.new_goal("goal to complete").unwrap();
        let done = mgr.done(None).unwrap();
        assert_eq!(done.status, GoalStatus::Done);

        // File must still exist.
        let path = mgr.goals_dir().join(format!("{}.json", goal.id));
        assert!(path.exists(), "goal file should still exist after done()");

        let on_disk = mgr.get(&goal.id).unwrap();
        assert_eq!(on_disk.status, GoalStatus::Done);
    }

    // ── 13. show_returns_goal_by_id ──────────────────────────────────────────

    #[test]
    fn show_returns_goal_by_id() {
        let tmp = TempDir::new().unwrap();
        let mut mgr = test_manager(&tmp);

        let goal = mgr.new_goal("show me this goal").unwrap();
        let shown = mgr.show(Some(&goal.id)).unwrap();
        assert_eq!(shown.id, goal.id);
        assert_eq!(shown.description, "show me this goal");
    }

    // ── 14. active_goal_returns_none_when_no_active ──────────────────────────

    #[test]
    fn active_goal_returns_none_when_no_active() {
        let tmp = TempDir::new().unwrap();
        let mgr = test_manager(&tmp);
        let active = mgr.active_goal().unwrap();
        assert!(active.is_none());
    }

    // ── 15. active_goal_returns_some_after_new_goal ──────────────────────────

    #[test]
    fn active_goal_returns_some_after_new_goal() {
        let tmp = TempDir::new().unwrap();
        let mut mgr = test_manager(&tmp);

        let created = mgr.new_goal("track this").unwrap();
        let active = mgr.active_goal().unwrap();
        assert!(active.is_some());
        assert_eq!(active.unwrap().id, created.id);
    }

    // ── 16. link_session_appends_session_id ──────────────────────────────────

    #[test]
    fn link_session_appends_session_id() {
        let tmp = TempDir::new().unwrap();
        let mut mgr = test_manager(&tmp);

        let goal = mgr.new_goal("link sessions goal").unwrap();
        mgr.link_session(&goal.id, "sess-abc").unwrap();

        let on_disk = mgr.get(&goal.id).unwrap();
        assert!(on_disk.session_ids.contains(&"sess-abc".to_owned()));
    }

    // ── 17. link_session_idempotent_for_same_id ──────────────────────────────

    #[test]
    fn link_session_idempotent_for_same_id() {
        let tmp = TempDir::new().unwrap();
        let mut mgr = test_manager(&tmp);

        let goal = mgr.new_goal("idempotent session link").unwrap();
        mgr.link_session(&goal.id, "sess-abc").unwrap();
        mgr.link_session(&goal.id, "sess-abc").unwrap();

        let on_disk = mgr.get(&goal.id).unwrap();
        let count = on_disk.session_ids.iter().filter(|s| s.as_str() == "sess-abc").count();
        assert_eq!(count, 1, "duplicate session_id should not be appended");
    }

    // ── 18. project_isolation_two_managers_dont_share_goals ──────────────────

    #[test]
    fn project_isolation_two_managers_dont_share_goals() {
        let tmp_a = TempDir::new().unwrap();
        let tmp_b = TempDir::new().unwrap();

        let mut mgr_a = GoalManager::with_goals_dir(
            tmp_a.path().to_path_buf(),
            tmp_a.path().join("goals"),
        );
        let mgr_b = GoalManager::with_goals_dir(
            tmp_b.path().to_path_buf(),
            tmp_b.path().join("goals"),
        );

        mgr_a.new_goal("goal in project A").unwrap();

        let goals_b = mgr_b.list().unwrap();
        assert!(goals_b.is_empty(), "project B should have no goals");
    }

    // ── 19. atomic_write_no_partial_files_on_simulated_failure ───────────────
    //
    // Strategy: write a goal successfully, then attempt to overwrite it with
    // a write that will fail (by making the goals_dir a file, not a dir, on the
    // `.tmp` path so the rename fails).  Verify the original JSON is intact.

    #[test]
    fn atomic_write_no_partial_files_on_simulated_failure() {
        let tmp = TempDir::new().unwrap();
        let mut mgr = test_manager(&tmp);

        // Write the initial goal — must succeed.
        let goal = mgr.new_goal("will survive corruption attempt").unwrap();
        let goal_path = mgr.goals_dir().join(format!("{}.json", goal.id));

        // Capture the good content.
        let original_json = fs::read_to_string(&goal_path).unwrap();

        // Block the tmp path by creating a directory where the .tmp file would go.
        let tmp_path = mgr.goals_dir().join(format!("{}.tmp", goal.id));
        fs::create_dir_all(&tmp_path).unwrap(); // directory at .tmp path — write will fail

        // Attempt to save (should fail because .tmp path is a directory).
        let mut bad_goal = goal.clone();
        bad_goal.description = "corrupted".to_owned();
        let result = mgr.save_goal(&bad_goal);
        assert!(result.is_err(), "save_goal should fail when .tmp path is a dir");

        // Original file must be intact.
        let after_json = fs::read_to_string(&goal_path).unwrap();
        assert_eq!(
            original_json, after_json,
            "original goal file must be unchanged after failed write"
        );

        // Clean up the blocking dir.
        fs::remove_dir(&tmp_path).unwrap();
    }
}
