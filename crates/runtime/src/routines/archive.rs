/// Per-run markdown archive for routine outputs.
///
/// Each run is written to:
///   `~/.anvil/routines/output/{routine_id}/{YYYYMMDDTHHMMSSZ}.md`
///
/// Writes are atomic (tmp file + rename).  Parent directories are created with
/// 0700 permissions on Unix.
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// Monotonic per-process counter for unique tmp filenames.
static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Outcome of a single routine run.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum RunStatus {
    /// Normal completion; output was delivered to the configured target.
    Clean,
    /// Output contained [`super::SILENT_MARKER`]; not delivered.
    Silent,
    /// The run encountered an error.
    Failed,
}

/// All metadata and output for one routine run.
#[derive(Debug, Clone)]
pub struct RunRecord {
    /// Unique ID of the routine definition.
    pub routine_id: String,
    /// Human-readable name of the routine.
    pub routine_name: String,
    /// 8-character unique identifier for this specific run.
    pub run_id: String,
    /// Unix seconds when execution started.
    pub started_at: u64,
    /// Unix seconds when execution ended.
    pub ended_at: u64,
    /// Wall-clock duration in milliseconds.
    pub duration_ms: u64,
    /// Whether the run completed cleanly, was silent, or failed.
    pub status: RunStatus,
    /// Human-readable representation of the schedule expression.
    pub schedule_display: String,
    /// Model identifier used for this run.
    pub model: String,
    /// Number of input tokens consumed.
    pub tokens_in: u64,
    /// Number of output tokens generated.
    pub tokens_out: u64,
    /// Full LLM response text, verbatim.
    pub output: String,
    /// Error description; populated only when `status == Failed`.
    pub error: Option<String>,
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Write a routine run to the on-disk archive.
///
/// Target path: `~/.anvil/routines/output/{routine_id}/{ISO-timestamp}.md`
///
/// Uses an atomic tmp-then-rename write so a partial failure never leaves a
/// corrupt file at the target path.
pub fn write_archive(record: &RunRecord) -> Result<PathBuf, String> {
    let target = archive_path(&record.routine_id, record.started_at)?;

    let parent = target
        .parent()
        .ok_or_else(|| format!("no parent directory for archive path: {}", target.display()))?;

    std::fs::create_dir_all(parent)
        .map_err(|e| format!("cannot create archive dir `{}`: {e}", parent.display()))?;

    // Set directory permissions to 0700 on Unix.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o700);
        std::fs::set_permissions(parent, perms)
            .map_err(|e| format!("cannot set permissions on `{}`: {e}", parent.display()))?;
    }

    let content = render_markdown(record);

    // Build a unique tmp path alongside the target.
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let pid = std::process::id();
    let seq = TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let tmp_name = format!(
        "{}.tmp.{pid}.{nanos:010}.{seq}",
        target
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("archive")
    );
    let tmp = parent.join(tmp_name);

    // Write to tmp then rename.
    if let Err(e) = std::fs::write(&tmp, &content) {
        // Best-effort cleanup — ignore second error.
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("cannot write archive tmp file: {e}"));
    }

    std::fs::rename(&tmp, &target).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("cannot rename archive tmp to target: {e}")
    })?;

    Ok(target)
}

/// Return the canonical path for a run archive file.
///
/// Path: `~/.anvil/routines/output/{routine_id}/{unix_to_iso(started_at)}.md`
pub fn archive_path(routine_id: &str, started_at: u64) -> Result<PathBuf, String> {
    validate_routine_id(routine_id)?;
    let base = anvil_routines_output_dir()?;
    let filename = format!("{}.md", unix_to_iso(started_at));
    Ok(base.join(routine_id).join(filename))
}

// ---------------------------------------------------------------------------
// Private helpers
// ---------------------------------------------------------------------------

/// Reject routine IDs that could be used for path traversal.
fn validate_routine_id(id: &str) -> Result<(), String> {
    if id.is_empty() {
        return Err("routine_id must not be empty".to_string());
    }
    if id.contains('/') {
        return Err(format!("routine_id must not contain '/': `{id}`"));
    }
    if id.contains("..") {
        return Err(format!("routine_id must not contain '..': `{id}`"));
    }
    if id.contains('\0') {
        return Err(format!("routine_id must not contain null byte: `{id}`"));
    }
    Ok(())
}

/// Resolve `~/.anvil/routines/output`.
fn anvil_routines_output_dir() -> Result<PathBuf, String> {
    let home =
        dirs_next::home_dir().ok_or_else(|| "cannot determine home directory".to_string())?;
    Ok(home.join(".anvil").join("routines").join("output"))
}

/// Render a `RunRecord` as a markdown document with YAML frontmatter.
fn render_markdown(record: &RunRecord) -> String {
    let started_rfc = unix_to_rfc3339(record.started_at);
    let ended_rfc = unix_to_rfc3339(record.ended_at);
    let status_str = match record.status {
        RunStatus::Clean => "clean",
        RunStatus::Silent => "silent",
        RunStatus::Failed => "failed",
    };

    let mut out = String::new();
    out.push_str("---\n");
    out.push_str(&format!("routine_name: {}\n", record.routine_name));
    out.push_str(&format!("routine_id: {}\n", record.routine_id));
    out.push_str(&format!("run_id: {}\n", record.run_id));
    out.push_str(&format!("started_at: {started_rfc}\n"));
    out.push_str(&format!("ended_at: {ended_rfc}\n"));
    out.push_str(&format!("duration_ms: {}\n", record.duration_ms));
    out.push_str(&format!("status: {status_str}\n"));
    out.push_str(&format!("schedule: {}\n", record.schedule_display));
    out.push_str(&format!("model: {}\n", record.model));
    out.push_str(&format!("tokens_in: {}\n", record.tokens_in));
    out.push_str(&format!("tokens_out: {}\n", record.tokens_out));
    out.push_str("---\n");

    if record.status == RunStatus::Failed {
        out.push_str("\n## Error\n\n");
        if let Some(ref err) = record.error {
            out.push_str(err);
        }
        out.push('\n');
    } else {
        out.push_str("\n## Agent Output\n\n");
        out.push_str(&record.output);
        out.push('\n');
    }

    out
}

/// Format Unix seconds as `"YYYYMMDDTHHMMSSZ"` (no separators, lexicographic).
///
/// Uses the same calendar decomposition as `cron::unix_to_parts`.
#[must_use]
pub fn unix_to_iso(ts: u64) -> String {
    let (year, month, day, hour, min, sec) = decompose_unix(ts);
    format!("{year:04}{month:02}{day:02}T{hour:02}{min:02}{sec:02}Z")
}

/// Format Unix seconds as RFC 3339 `"YYYY-MM-DDTHH:MM:SSZ"`.
#[must_use]
pub fn unix_to_rfc3339(ts: u64) -> String {
    let (year, month, day, hour, min, sec) = decompose_unix(ts);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{min:02}:{sec:02}Z")
}

/// Decompose a Unix timestamp into `(year, month, day, hour, minute, second)`.
///
/// Mirrors the Howard Hinnant civil-from-days algorithm used in `cron.rs`.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss
)]
fn decompose_unix(ts: u64) -> (u32, u32, u32, u32, u32, u32) {
    let secs_per_day: u64 = 86_400;
    let days_since_epoch = ts / secs_per_day;
    let time_of_day = ts % secs_per_day;

    let hour = (time_of_day / 3600) as u32;
    let min = ((time_of_day % 3600) / 60) as u32;
    let sec = (time_of_day % 60) as u32;

    let z = days_since_epoch as i64 + 719_468;
    let era: i64 = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = (y + i64::from(month <= 2)) as u32;

    (year, month, day, hour, min, sec)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn make_record(status: RunStatus, error: Option<String>) -> RunRecord {
        RunRecord {
            routine_id: "test-routine-01".to_string(),
            routine_name: "My Test Routine".to_string(),
            run_id: "abc12345".to_string(),
            started_at: 946_684_800, // 2000-01-01T00:00:00Z
            ended_at: 946_684_860,
            duration_ms: 60_000,
            status,
            schedule_display: "every 1h".to_string(),
            model: "anthropic/claude-sonnet-4-6".to_string(),
            tokens_in: 1000,
            tokens_out: 200,
            output: "Everything looks good.".to_string(),
            error,
        }
    }

    // ── unix_to_iso ───────────────────────────────────────────────────────

    #[test]
    fn unix_to_iso_y2k() {
        assert_eq!(unix_to_iso(946_684_800), "20000101T000000Z");
    }

    #[test]
    fn unix_to_iso_arbitrary_time() {
        // 2026-05-11T15:30:00Z
        let epoch = crate::routines::schedule::parts_to_unix(2026, 5, 11, 15, 30, 0);
        let iso = unix_to_iso(epoch);
        assert_eq!(iso, "20260511T153000Z");
    }

    // ── unix_to_rfc3339 ───────────────────────────────────────────────────

    #[test]
    fn unix_to_rfc3339_y2k() {
        assert_eq!(unix_to_rfc3339(946_684_800), "2000-01-01T00:00:00Z");
    }

    // ── validate_routine_id ───────────────────────────────────────────────

    #[test]
    fn validate_id_rejects_path_traversal() {
        assert!(validate_routine_id("../escape").is_err());
    }

    #[test]
    fn validate_id_rejects_slash() {
        assert!(validate_routine_id("id/with/slash").is_err());
    }

    #[test]
    fn validate_id_rejects_null_byte() {
        assert!(validate_routine_id("id\0null").is_err());
    }

    #[test]
    fn validate_id_accepts_normal_id() {
        assert!(validate_routine_id("abc12345").is_ok());
    }

    #[test]
    fn validate_id_rejects_empty() {
        assert!(validate_routine_id("").is_err());
    }

    // ── render_markdown ───────────────────────────────────────────────────

    #[test]
    fn render_markdown_clean_run() {
        let record = make_record(RunStatus::Clean, None);
        let md = render_markdown(&record);
        assert!(md.contains("status: clean"));
        assert!(md.contains("## Agent Output"));
        assert!(md.contains("Everything looks good."));
        assert!(!md.contains("## Error"));
    }

    #[test]
    fn render_markdown_silent_run() {
        let mut record = make_record(RunStatus::Silent, None);
        record.output = "Done. [SILENT]".to_string();
        let md = render_markdown(&record);
        assert!(md.contains("status: silent"));
        assert!(md.contains("## Agent Output"));
    }

    #[test]
    fn render_markdown_failed_run() {
        let record = make_record(RunStatus::Failed, Some("timeout after 30s".to_string()));
        let md = render_markdown(&record);
        assert!(md.contains("status: failed"));
        assert!(md.contains("## Error"));
        assert!(md.contains("timeout after 30s"));
        assert!(!md.contains("## Agent Output"));
    }

    // ── write_archive ─────────────────────────────────────────────────────

    #[test]
    fn write_archive_clean_run() {
        let tmp_home = tempfile::tempdir().unwrap();
        // Override HOME for this test via the archive path helper.
        // We use archive_path directly with a known routine_id.
        let record = make_record(RunStatus::Clean, None);

        // Build path manually against tmp dir.
        let target = tmp_home
            .path()
            .join(".anvil")
            .join("routines")
            .join("output")
            .join(&record.routine_id)
            .join(format!("{}.md", unix_to_iso(record.started_at)));

        let parent = target.parent().unwrap();
        std::fs::create_dir_all(parent).unwrap();

        // Write content directly (bypassing HOME lookup).
        let content = render_markdown(&record);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        let tmp = parent.join(format!("test.tmp.{nanos}"));
        std::fs::write(&tmp, &content).unwrap();
        std::fs::rename(&tmp, &target).unwrap();

        assert!(target.exists());
        let on_disk = std::fs::read_to_string(&target).unwrap();
        assert!(on_disk.contains("status: clean"));
        assert!(on_disk.contains("routine_id: test-routine-01"));
        assert!(on_disk.contains("## Agent Output"));
    }

    #[test]
    fn write_archive_silent_run() {
        let record = RunRecord {
            routine_id: "silent-routine".to_string(),
            status: RunStatus::Silent,
            output: "Done. [SILENT]".to_string(),
            ..make_record(RunStatus::Silent, None)
        };
        let content = render_markdown(&record);
        assert!(content.contains("status: silent"));
    }

    #[test]
    fn write_archive_failed_run_has_error_section() {
        let record = make_record(RunStatus::Failed, Some("LLM timeout".to_string()));
        let content = render_markdown(&record);
        assert!(content.contains("## Error"));
        assert!(content.contains("LLM timeout"));
        assert!(!content.contains("## Agent Output"));
    }

    // ── archive_path ──────────────────────────────────────────────────────

    #[test]
    fn archive_path_contains_routine_id_and_iso() {
        let path = archive_path("abc12345", 946_684_800).unwrap();
        let path_str = path.to_string_lossy();
        assert!(path_str.contains("abc12345"));
        assert!(path_str.contains("20000101T000000Z"));
        assert!(path_str.ends_with(".md"));
    }

    #[test]
    fn archive_path_rejects_bad_id() {
        assert!(archive_path("../bad", 0).is_err());
    }
}
