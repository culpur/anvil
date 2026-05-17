// Edition 2024: env::set_var/remove_var require unsafe
#![allow(unsafe_code)]

use std::fmt::Write as _;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::session::{ContentBlock, MessageRole, Session};

/// Minimum messages a session must have before it is worth archiving.
pub const MIN_ARCHIVE_MESSAGES: usize = 10;

/// Environment variable that overrides the default 85 % auto-compact threshold.
/// Value is an integer 1-99 representing the percentage.
pub const COMPACT_THRESHOLD_ENV: &str = "ANVIL_COMPACT_THRESHOLD";

/// Default auto-compact threshold as a percentage of the model's context window.
pub const DEFAULT_COMPACT_THRESHOLD_PCT: u8 = 85;

/// Phase 4.1 (L2 §4): env override for primary history retention in days.
/// Default 90. Set to `0` to disable pruning entirely.
pub const HISTORY_RETENTION_DAYS_ENV: &str = "ANVIL_HISTORY_RETENTION_DAYS";

/// Phase 4.1: default primary retention window — 90 days.
pub const DEFAULT_HISTORY_RETENTION_DAYS: u64 = 90;

/// Phase 4.1: secondary retention — anything moved to `.trash/` older than
/// this is permanently deleted on the next prune run.
pub const DEFAULT_TRASH_RETENTION_DAYS: u64 = 30;

/// Phase 4.1: hard cap on the number of file moves a single auto-prune run
/// will perform. Protects against runaway sessions on an old fleet of
/// archives.
pub const MAX_AUTO_PRUNE_MOVES: usize = 100;

/// Phase 4.1: outcome of a single retention-prune action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrunedAction {
    /// File was (or would be) moved from `history/` into `.trash/<date>/`.
    MovedToTrash,
    /// File was already in `.trash/` and past the secondary retention; it
    /// was (or would be) permanently deleted.
    PermanentlyDeleted,
}

/// Phase 4.1: one row of `prune_expired`'s return.
///
/// `from` is the original path under `history/` or `.trash/`. For
/// `MovedToTrash` entries `to` is the destination path under
/// `.trash/<YYYY-MM-DD>/<file>`. For `PermanentlyDeleted` entries `to` is
/// `None` (no destination — it's gone).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrunedEntry {
    pub from: PathBuf,
    pub to: Option<PathBuf>,
    pub action: PrunedAction,
    /// Unix-secs mtime of the file that drove the decision.
    pub mtime: u64,
}

/// Phase 4.1: summary of an auto-prune-on-start run, suitable for one-line
/// TUI scrollback. `moved` is the count of `MovedToTrash` actions; `deleted`
/// is the count of `PermanentlyDeleted` actions. `bucket` is the
/// `.trash/<YYYY-MM-DD>/` shard used for newly-moved files this run (empty
/// when no moves happened).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PruneSummary {
    pub moved: usize,
    pub deleted: usize,
    pub bucket: String,
}

impl PruneSummary {
    /// Render the one-line summary printed to TUI scrollback. Returns
    /// `None` when the run was a no-op so callers can suppress the line
    /// entirely.
    #[must_use]
    pub fn format_one_line(&self) -> Option<String> {
        if self.moved == 0 && self.deleted == 0 {
            return None;
        }
        let mut out = String::from("[info] History retention: ");
        if self.moved > 0 {
            let _ = write!(
                out,
                "moved {} file{} to .trash/{}/",
                self.moved,
                if self.moved == 1 { "" } else { "s" },
                self.bucket,
            );
        }
        if self.deleted > 0 {
            if self.moved > 0 {
                out.push_str("; ");
            }
            let _ = write!(
                out,
                "permanently deleted {} file{} past trash retention",
                self.deleted,
                if self.deleted == 1 { "" } else { "s" },
            );
        }
        Some(out)
    }
}

/// Metadata entry for a single archived session file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchiveEntry {
    pub path: PathBuf,
    pub session_id: String,
    pub timestamp: u64,
    pub model: String,
    pub message_count: usize,
}

/// Archives full conversations to `~/.anvil/history/` before compaction so
/// that context is never lost and remains searchable via QMD.
pub struct HistoryArchiver {
    history_dir: PathBuf,
}

impl HistoryArchiver {
    /// Create a new archiver.  The history directory is created on demand.
    #[must_use]
    pub fn new() -> Self {
        let dir = home_dir()
            .unwrap_or_default()
            .join(".anvil")
            .join("history");
        // Best-effort — if the directory cannot be created we will fail at
        // write time and return a descriptive io::Error.
        let _ = std::fs::create_dir_all(&dir);
        Self { history_dir: dir }
    }

    /// Return the history directory path (used by QMD for indexing).
    #[must_use]
    pub fn history_dir(&self) -> &Path {
        &self.history_dir
    }

    /// Write the full conversation to a dated markdown file.
    ///
    /// Returns the path to the written file.
    ///
    /// The file is skipped (returns `Ok(None)`) when the session contains
    /// fewer than [`MIN_ARCHIVE_MESSAGES`] messages — small sessions are not
    /// useful enough to archive.
    pub fn archive_session(
        &self,
        session_id: &str,
        session: &Session,
        model: &str,
        summary: &str,
    ) -> io::Result<Option<PathBuf>> {
        if session.messages.len() < MIN_ARCHIVE_MESSAGES {
            return Ok(None);
        }

        let timestamp = unix_secs();
        let filename = format!("{session_id}-{timestamp}.md");
        let path = self.history_dir.join(&filename);

        let content = build_archive_markdown(session_id, session, model, summary, timestamp);
        std::fs::write(&path, content)?;
        Ok(Some(path))
    }

    /// List all archived sessions, newest first.
    #[must_use]
    pub fn list_archives(&self) -> Vec<ArchiveEntry> {
        let Ok(read_dir) = std::fs::read_dir(&self.history_dir) else {
            return Vec::new();
        };

        let mut entries: Vec<ArchiveEntry> = read_dir
            .filter_map(std::result::Result::ok)
            .filter(|entry| {
                entry
                    .path()
                    .extension()
                    .and_then(|e| e.to_str())
                    .is_some_and(|e| e.eq_ignore_ascii_case("md"))
            })
            .filter_map(|entry| parse_archive_entry(&entry.path()))
            .collect();

        entries.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
        entries
    }

    /// Return the auto-compact threshold percentage, reading
    /// `ANVIL_COMPACT_THRESHOLD` from the environment when present.
    #[must_use]
    pub fn compact_threshold_pct() -> u8 {
        std::env::var(COMPACT_THRESHOLD_ENV)
            .ok()
            .and_then(|v| v.parse::<u8>().ok())
            .filter(|&pct| (1..=99).contains(&pct))
            .unwrap_or(DEFAULT_COMPACT_THRESHOLD_PCT)
    }

    /// Phase 4.1: return the effective primary retention window in days.
    ///
    /// Reads `ANVIL_HISTORY_RETENTION_DAYS` from the env; defaults to 90.
    /// A value of `0` disables pruning (returned as 0; callers MUST short-
    /// circuit when this is 0).
    #[must_use]
    pub fn retention_days() -> u64 {
        std::env::var(HISTORY_RETENTION_DAYS_ENV)
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(DEFAULT_HISTORY_RETENTION_DAYS)
    }

    /// Phase 4.1: return the trash dir (`<config-home>/.trash` — sibling to
    /// `history/` under `~/.anvil/`). Lazy-created on first move.
    #[must_use]
    pub fn trash_dir(&self) -> PathBuf {
        // The history dir is `<home>/.anvil/history/` — its parent is the
        // anvil config home, where `.trash/` lives as a sibling.
        let anvil_home = self
            .history_dir
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| home_dir().unwrap_or_default().join(".anvil"));
        anvil_home.join(".trash")
    }

    /// Phase 4.1: prune the history archive.
    ///
    /// Behavior:
    ///   1. Files under `history/` with mtime older than
    ///      `ANVIL_HISTORY_RETENTION_DAYS` (default 90) are MOVED into
    ///      `<anvil-home>/.trash/<YYYY-MM-DD>/`, where `<YYYY-MM-DD>` is
    ///      today's date in UTC. The move is rename-or-copy-then-delete
    ///      across the same volume, so it should be cheap.
    ///   2. Files already under `.trash/` with mtime older than
    ///      `DEFAULT_TRASH_RETENTION_DAYS` (30) are permanently deleted on
    ///      this run.
    ///
    /// `dry_run = true` skips all filesystem mutations and returns the
    /// same `PrunedEntry` list a real run would have produced. Callers
    /// surface this list (e.g. `/memory prune --dry-run`).
    ///
    /// When [`HistoryArchiver::retention_days`] is `0`, returns
    /// `Ok(Vec::new())` immediately — pruning is disabled.
    pub fn prune_expired(&self, dry_run: bool) -> io::Result<Vec<PrunedEntry>> {
        self.prune_expired_with_cap(dry_run, usize::MAX)
    }

    /// Phase 4.1: like [`prune_expired`] but caps the number of newly-moved
    /// files at `max_moves`. Trash deletions are uncapped (cheap, and the
    /// trash already grew past its window). Used by auto-prune-on-start to
    /// avoid massive churn on old fleets.
    pub fn prune_expired_with_cap(
        &self,
        dry_run: bool,
        max_moves: usize,
    ) -> io::Result<Vec<PrunedEntry>> {
        self.prune_expired_with_clock(dry_run, max_moves, unix_secs())
    }

    /// Phase 4.1: same as [`prune_expired_with_cap`] but accepts an
    /// injectable `now`. Used by tests to control time without backdating
    /// file mtimes (which would require a libc dep or filetime crate). The
    /// public API always passes the real clock.
    pub fn prune_expired_with_clock(
        &self,
        dry_run: bool,
        max_moves: usize,
        now: u64,
    ) -> io::Result<Vec<PrunedEntry>> {
        let retention = Self::retention_days();
        if retention == 0 {
            return Ok(Vec::new());
        }
        let primary_cutoff = now.saturating_sub(retention.saturating_mul(86_400));
        let trash_cutoff = now.saturating_sub(
            DEFAULT_TRASH_RETENTION_DAYS.saturating_mul(86_400),
        );

        let mut results: Vec<PrunedEntry> = Vec::new();

        // ── Pass 1: trash-side permanent deletions ────────────────────────
        let trash_dir = self.trash_dir();
        if trash_dir.exists() {
            self.collect_trash_expirations(&trash_dir, trash_cutoff, dry_run, &mut results)?;
        }

        // ── Pass 2: history-side moves into .trash/<today>/ ───────────────
        let bucket_name = date_bucket_for_secs(now);
        let trash_bucket = trash_dir.join(&bucket_name);
        let mut moves_remaining = max_moves;
        if self.history_dir.exists() && moves_remaining > 0 {
            let read = std::fs::read_dir(&self.history_dir)?;
            for entry in read.filter_map(std::result::Result::ok) {
                if moves_remaining == 0 {
                    break;
                }
                let path = entry.path();
                if !path.is_file() {
                    continue;
                }
                if path
                    .extension()
                    .and_then(|e| e.to_str())
                    .is_none_or(|e| !e.eq_ignore_ascii_case("md"))
                {
                    continue;
                }
                let mtime = match entry.metadata().and_then(|m| m.modified()) {
                    Ok(t) => t
                        .duration_since(UNIX_EPOCH)
                        .map(|d| d.as_secs())
                        .unwrap_or(0),
                    Err(_) => continue,
                };
                if mtime >= primary_cutoff {
                    // Still inside the retention window — keep.
                    continue;
                }
                let dest = trash_bucket.join(path.file_name().unwrap_or_else(|| {
                    std::ffi::OsStr::new("orphan.md")
                }));
                if !dry_run {
                    if let Some(parent) = dest.parent() {
                        std::fs::create_dir_all(parent)?;
                    }
                    move_file(&path, &dest)?;
                }
                results.push(PrunedEntry {
                    from: path,
                    to: Some(dest),
                    action: PrunedAction::MovedToTrash,
                    mtime,
                });
                moves_remaining -= 1;
            }
        }

        Ok(results)
    }

    /// Phase 4.1: convenience wrapper called at session start. Runs prune
    /// with the auto-prune cap and returns a [`PruneSummary`] suitable for
    /// the TUI scrollback line. Errors are swallowed (silent on read
    /// failures) so a busted history dir cannot prevent startup.
    pub fn auto_prune_on_session_start(&self) -> PruneSummary {
        // Retention disabled? No-op.
        if Self::retention_days() == 0 {
            return PruneSummary {
                moved: 0,
                deleted: 0,
                bucket: String::new(),
            };
        }
        let results = self
            .prune_expired_with_cap(false, MAX_AUTO_PRUNE_MOVES)
            .unwrap_or_default();
        let bucket = date_bucket_for_secs(unix_secs());
        let moved = results
            .iter()
            .filter(|p| p.action == PrunedAction::MovedToTrash)
            .count();
        let deleted = results
            .iter()
            .filter(|p| p.action == PrunedAction::PermanentlyDeleted)
            .count();
        PruneSummary {
            moved,
            deleted,
            bucket: if moved > 0 { bucket } else { String::new() },
        }
    }

    /// Phase 4.1: recursive walk of `<trash_dir>/<bucket>/*` to find files
    /// past the secondary retention cutoff.
    fn collect_trash_expirations(
        &self,
        trash_dir: &Path,
        cutoff_secs: u64,
        dry_run: bool,
        out: &mut Vec<PrunedEntry>,
    ) -> io::Result<()> {
        let read = match std::fs::read_dir(trash_dir) {
            Ok(r) => r,
            Err(_) => return Ok(()),
        };
        for shard in read.filter_map(std::result::Result::ok) {
            let shard_path = shard.path();
            // The trash layout is `.trash/<YYYY-MM-DD>/<file.md>`; if the
            // entry is a file (not a date bucket) we still inspect it so a
            // legacy layout doesn't leak.
            if shard_path.is_file() {
                self.maybe_delete_trash_file(&shard_path, cutoff_secs, dry_run, out)?;
                continue;
            }
            if !shard_path.is_dir() {
                continue;
            }
            let inner = match std::fs::read_dir(&shard_path) {
                Ok(r) => r,
                Err(_) => continue,
            };
            for f in inner.filter_map(std::result::Result::ok) {
                let p = f.path();
                if p.is_file() {
                    self.maybe_delete_trash_file(&p, cutoff_secs, dry_run, out)?;
                }
            }
            // Best-effort: remove the bucket if now empty (post-deletions).
            if !dry_run {
                let _ = std::fs::read_dir(&shard_path).and_then(|mut r| {
                    if r.next().is_none() {
                        std::fs::remove_dir(&shard_path)
                    } else {
                        Ok(())
                    }
                });
            }
        }
        Ok(())
    }

    fn maybe_delete_trash_file(
        &self,
        path: &Path,
        cutoff_secs: u64,
        dry_run: bool,
        out: &mut Vec<PrunedEntry>,
    ) -> io::Result<()> {
        let meta = match std::fs::metadata(path) {
            Ok(m) => m,
            Err(_) => return Ok(()),
        };
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);
        if mtime >= cutoff_secs {
            return Ok(());
        }
        if !dry_run {
            // Failures here are non-fatal but propagated so callers see them
            // in non-dry-run mode.
            std::fs::remove_file(path)?;
        }
        out.push(PrunedEntry {
            from: path.to_path_buf(),
            to: None,
            action: PrunedAction::PermanentlyDeleted,
            mtime,
        });
        Ok(())
    }
}

impl Default for HistoryArchiver {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Markdown archive builder
// ---------------------------------------------------------------------------

fn build_archive_markdown(
    session_id: &str,
    session: &Session,
    model: &str,
    summary: &str,
    timestamp: u64,
) -> String {
    let mut out = String::with_capacity(4096);

    // YAML front-matter so QMD can index metadata without parsing the body.
    out.push_str("---\n");
    let _ = writeln!(out, "session_id: {session_id}");
    let _ = writeln!(out, "archived_at: {timestamp}");
    let _ = writeln!(out, "model: {model}");
    let _ = writeln!(out, "message_count: {}", session.messages.len());
    out.push_str("---\n\n");

    let _ = writeln!(out, "# Session Archive: {session_id}\n");

    if !summary.is_empty() {
        out.push_str("## Summary\n\n");
        out.push_str(summary);
        out.push_str("\n\n");
    }

    out.push_str("## Conversation\n\n");

    for (i, msg) in session.messages.iter().enumerate() {
        let role_label = match msg.role {
            MessageRole::System => "System",
            MessageRole::User => "User",
            MessageRole::Assistant => "Assistant",
            MessageRole::Tool => "Tool",
        };
        let _ = write!(out, "### [{i}] {role_label}\n\n");

        for block in &msg.blocks {
            match block {
                ContentBlock::Text { text } => {
                    out.push_str(text);
                    out.push_str("\n\n");
                }
                ContentBlock::ToolUse { name, input, .. } => {
                    let _ = write!(out, "**Tool:** `{name}`\n\n");
                    // Truncate large tool inputs for archive readability.
                    let truncated = truncate_chars(input, 500);
                    let _ = write!(out, "```json\n{truncated}\n```\n\n");
                }
                ContentBlock::ToolResult {
                    tool_name, output, is_error, ..
                } => {
                    let label = if *is_error { "Error result" } else { "Result" };
                    let _ = write!(out, "**{label} ({tool_name}):** ");
                    out.push_str(&truncate_chars(output, 500));
                    out.push_str("\n\n");
                }
                ContentBlock::Image { media_type, .. } => {
                    let _ = write!(out, "*[image: {media_type}]*\n\n");
                }
                ContentBlock::Document {
                    media_type, title, ..
                } => {
                    let name = title.as_deref().unwrap_or("document");
                    let _ = write!(out, "*[document: {name} ({media_type})]*\n\n");
                }
            }
        }
    }

    out
}

// ---------------------------------------------------------------------------
// Front-matter parser (used when listing archives)
// ---------------------------------------------------------------------------

fn parse_archive_entry(path: &Path) -> Option<ArchiveEntry> {
    let text = std::fs::read_to_string(path).ok()?;

    // Extract YAML front-matter between the first pair of `---` lines.
    let after_first = text.strip_prefix("---\n")?;
    let end = after_first.find("\n---")?;
    let frontmatter = &after_first[..end];

    let mut session_id = String::new();
    let mut timestamp: u64 = 0;
    let mut model = String::new();
    let mut message_count: usize = 0;

    for line in frontmatter.lines() {
        if let Some(val) = line.strip_prefix("session_id: ") {
            session_id = val.trim().to_string();
        } else if let Some(val) = line.strip_prefix("archived_at: ") {
            timestamp = val.trim().parse().unwrap_or(0);
        } else if let Some(val) = line.strip_prefix("model: ") {
            model = val.trim().to_string();
        } else if let Some(val) = line.strip_prefix("message_count: ") {
            message_count = val.trim().parse().unwrap_or(0);
        }
    }

    if session_id.is_empty() {
        return None;
    }

    Some(ArchiveEntry {
        path: path.to_path_buf(),
        session_id,
        timestamp,
        model,
        message_count,
    })
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn home_dir() -> Option<PathBuf> {
    dirs_next::home_dir()
}

fn truncate_chars(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    let mut truncated: String = s.chars().take(max_chars).collect();
    truncated.push_str("...[truncated]");
    truncated
}

/// Phase 4.1: render a `YYYY-MM-DD` UTC date for a Unix-secs value.
///
/// Used as the `.trash/<bucket>/` shard name so a single prune run groups
/// all of its moves together. Pure conversion — does not call any time
/// APIs (so it's testable with arbitrary clocks).
#[must_use]
pub fn date_bucket_for_secs(secs: u64) -> String {
    // Civil date computation from Unix-secs. This is the standard
    // Howard Hinnant algorithm — exact + branch-light. Avoids pulling
    // chrono into runtime.
    let days = (secs / 86_400) as i64;
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = (yoe as i64) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    let year = y + if m <= 2 { 1 } else { 0 };
    format!("{year:04}-{m:02}-{d:02}")
}

/// Phase 4.1: cross-volume-safe move. Prefer `rename`; fall back to
/// copy-then-delete when rename fails (e.g. `EXDEV`).
fn move_file(src: &Path, dest: &Path) -> io::Result<()> {
    match std::fs::rename(src, dest) {
        Ok(()) => Ok(()),
        Err(_) => {
            std::fs::copy(src, dest)?;
            std::fs::remove_file(src)?;
            Ok(())
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{ContentBlock, ConversationMessage, Session};

    fn make_session(n: usize) -> Session {
        Session {
            version: 1,
            messages: (0..n)
                .map(|i| {
                    if i % 2 == 0 {
                        ConversationMessage::user_text(format!("message {i}"))
                    } else {
                        ConversationMessage::assistant(vec![ContentBlock::Text {
                            text: format!("reply {i}"),
                        }])
                    }
                })
                .collect(),
        }
    }

    #[test]
    fn skips_small_sessions() {
        let archiver = HistoryArchiver {
            history_dir: std::env::temp_dir().join("anvil-test-skips-small"),
        };
        let session = make_session(5);
        let result = archiver.archive_session("sid", &session, "claude", "summary");
        // Should succeed but return None (skipped).
        assert!(result.is_ok());
        assert!(result.unwrap().is_none());
    }

    #[test]
    fn archives_large_sessions() {
        let dir = std::env::temp_dir().join(format!(
            "anvil-test-archive-{}",
            std::time::SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let archiver = HistoryArchiver { history_dir: dir.clone() };
        let session = make_session(15);
        let path = archiver
            .archive_session("test-session", &session, "claude-opus", "Summary text")
            .unwrap()
            .expect("should have been archived");

        assert!(path.exists());
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.starts_with("---\n"));
        assert!(content.contains("session_id: test-session"));
        assert!(content.contains("model: claude-opus"));
        assert!(content.contains("message_count: 15"));
        assert!(content.contains("# Session Archive: test-session"));
        assert!(content.contains("## Summary"));

        // Clean up.
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn list_archives_parses_frontmatter() {
        let dir = std::env::temp_dir().join(format!(
            "anvil-test-list-{}",
            std::time::SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let archiver = HistoryArchiver { history_dir: dir.clone() };
        let session = make_session(12);
        archiver
            .archive_session("list-test", &session, "my-model", "")
            .unwrap();

        let entries = archiver.list_archives();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].session_id, "list-test");
        assert_eq!(entries[0].model, "my-model");
        assert_eq!(entries[0].message_count, 12);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn compact_threshold_default_and_env_override() {
        // Combined into one test to avoid env var race between parallel tests.
        // Test 1: default when env var is unset
        unsafe { std::env::remove_var(COMPACT_THRESHOLD_ENV); }
        assert_eq!(HistoryArchiver::compact_threshold_pct(), 85);

        // Test 2: reads from env var when set
        unsafe { std::env::set_var(COMPACT_THRESHOLD_ENV, "70"); }
        assert_eq!(HistoryArchiver::compact_threshold_pct(), 70);

        // Cleanup
        unsafe { std::env::remove_var(COMPACT_THRESHOLD_ENV); }
    }

    #[test]
    fn truncate_chars_works() {
        let s = "a".repeat(600);
        let truncated = truncate_chars(&s, 500);
        assert!(truncated.ends_with("...[truncated]"));
        assert_eq!(truncated.chars().count(), 514);

        let short = "hello";
        assert_eq!(truncate_chars(short, 500), "hello");
    }

    // ── Phase 4.1: retention pruner tests ─────────────────────────────────
    //
    // Tests inject a `now` clock value rather than backdating file mtimes,
    // so they're portable to every target the workspace builds for (no
    // libc / utimensat). The file's real mtime is "right now" — we just
    // pretend `now` is far in the future when we want the file to look
    // expired.

    /// Lay out a fake anvil home with `history/` and (optionally) an
    /// existing `.trash/` and return the archiver pointed at history/.
    fn fake_archiver(tag: &str) -> (tempfile::TempDir, HistoryArchiver) {
        let dir = tempfile::TempDir::new().expect("tmp");
        let anvil_home = dir.path().join(tag);
        std::fs::create_dir_all(anvil_home.join("history")).unwrap();
        let archiver = HistoryArchiver {
            history_dir: anvil_home.join("history"),
        };
        (dir, archiver)
    }

    /// Write a fake archive .md file with default (real) mtime.
    fn write_archive(dir: &Path, name: &str) -> PathBuf {
        let path = dir.join(name);
        let body = format!("---\nsession_id: {name}\n---\n");
        std::fs::write(&path, body).unwrap();
        path
    }

    /// Pick a `now` that's a year ahead of real-now so anything written
    /// with the real clock will look "very old" (>90 days) to the pruner.
    fn far_future_now() -> u64 {
        unix_secs() + 365 * 86_400
    }

    #[test]
    fn retention_days_defaults_to_90() {
        // Single test to avoid env-var race with other tests in the file.
        unsafe { std::env::remove_var(HISTORY_RETENTION_DAYS_ENV); }
        assert_eq!(HistoryArchiver::retention_days(), 90);
        unsafe { std::env::set_var(HISTORY_RETENTION_DAYS_ENV, "30"); }
        assert_eq!(HistoryArchiver::retention_days(), 30);
        unsafe { std::env::set_var(HISTORY_RETENTION_DAYS_ENV, "0"); }
        assert_eq!(HistoryArchiver::retention_days(), 0);
        unsafe { std::env::remove_var(HISTORY_RETENTION_DAYS_ENV); }
    }

    #[test]
    fn prune_disabled_when_retention_is_zero() {
        let (_g, archiver) = fake_archiver("disabled");
        write_archive(archiver.history_dir(), "old.md");

        unsafe { std::env::set_var(HISTORY_RETENTION_DAYS_ENV, "0"); }
        let pruned = archiver
            .prune_expired_with_clock(false, usize::MAX, far_future_now())
            .unwrap();
        unsafe { std::env::remove_var(HISTORY_RETENTION_DAYS_ENV); }

        assert!(pruned.is_empty(), "retention=0 must be a no-op");
        assert!(
            archiver.history_dir().join("old.md").exists(),
            "file must survive when pruning is disabled",
        );
    }

    #[test]
    fn prune_moves_old_files_to_trash_dated_bucket() {
        let (_g, archiver) = fake_archiver("moves");
        write_archive(archiver.history_dir(), "old-a.md");
        write_archive(archiver.history_dir(), "old-b.md");
        let recent_path = archiver.history_dir().join("recent.md");
        // recent_path is what a "kept" file looks like — we'll pass a now
        // that's only 1 day in the past relative to creation, so the 90-day
        // cutoff sees it as recent.
        // To accomplish the keep, we'll do the prune twice with different
        // nows: first prune at "now = real-now" — the cutoff is real-now -
        // 90d, all files are newer, NO moves. Then prune at far-future —
        // all 3 should look old. But "recent.md" needs to be kept, so the
        // mtime trick doesn't help here without backdating.
        // Simpler: just do the far-future prune and expect all 3 files
        // moved, then a separate test asserts the keep behavior using a
        // close-to-real now where nothing's expired.
        let _ = recent_path;

        unsafe { std::env::set_var(HISTORY_RETENTION_DAYS_ENV, "90"); }
        let pruned = archiver
            .prune_expired_with_clock(false, usize::MAX, far_future_now())
            .unwrap();
        unsafe { std::env::remove_var(HISTORY_RETENTION_DAYS_ENV); }

        let move_count = pruned
            .iter()
            .filter(|p| p.action == PrunedAction::MovedToTrash)
            .count();
        assert_eq!(move_count, 2, "both old files should have moved");
        assert!(
            !archiver.history_dir().join("old-a.md").exists(),
            "old-a moved out of history/",
        );

        let trash = archiver.trash_dir();
        let bucket = date_bucket_for_secs(far_future_now());
        assert!(trash.join(&bucket).join("old-a.md").exists());
        assert!(trash.join(&bucket).join("old-b.md").exists());
    }

    #[test]
    fn prune_keeps_recent_files() {
        let (_g, archiver) = fake_archiver("keepsrecent");
        let p = write_archive(archiver.history_dir(), "fresh.md");
        unsafe { std::env::set_var(HISTORY_RETENTION_DAYS_ENV, "90"); }
        // "now" = real-now so the just-written file is well within the 90d
        // window.
        let pruned = archiver
            .prune_expired_with_clock(false, usize::MAX, unix_secs())
            .unwrap();
        unsafe { std::env::remove_var(HISTORY_RETENTION_DAYS_ENV); }
        assert!(
            pruned.is_empty(),
            "fresh file should not be pruned: {pruned:?}",
        );
        assert!(p.exists(), "fresh file must remain in history/");
    }

    #[test]
    fn prune_dry_run_leaves_filesystem_unchanged() {
        let (_g, archiver) = fake_archiver("dryrun");
        let p = write_archive(archiver.history_dir(), "ancient.md");

        unsafe { std::env::set_var(HISTORY_RETENTION_DAYS_ENV, "90"); }
        let pruned = archiver
            .prune_expired_with_clock(true, usize::MAX, far_future_now())
            .unwrap();
        unsafe { std::env::remove_var(HISTORY_RETENTION_DAYS_ENV); }

        // Dry-run reports the move would happen…
        assert_eq!(pruned.len(), 1);
        assert_eq!(pruned[0].action, PrunedAction::MovedToTrash);
        // …but the file is still in the original location.
        assert!(p.exists(), "dry-run must not move the file");
        assert!(
            !archiver.trash_dir().exists()
                || std::fs::read_dir(archiver.trash_dir())
                    .map(|mut r| r.next().is_none())
                    .unwrap_or(true),
            "dry-run must not create .trash/",
        );
    }

    #[test]
    fn prune_permanently_deletes_trash_older_than_30_days() {
        let (_g, archiver) = fake_archiver("trashexp");
        let trash_bucket = archiver.trash_dir().join("2000-01-01");
        std::fs::create_dir_all(&trash_bucket).unwrap();
        let stale = trash_bucket.join("ancient.md");
        std::fs::write(&stale, "old").unwrap();
        // far-future now means the 30-day trash window has expired for the
        // just-written stale file.
        unsafe { std::env::set_var(HISTORY_RETENTION_DAYS_ENV, "90"); }
        let pruned = archiver
            .prune_expired_with_clock(false, usize::MAX, far_future_now())
            .unwrap();
        unsafe { std::env::remove_var(HISTORY_RETENTION_DAYS_ENV); }

        let deletions: Vec<_> = pruned
            .iter()
            .filter(|p| p.action == PrunedAction::PermanentlyDeleted)
            .collect();
        assert_eq!(deletions.len(), 1);
        assert!(!stale.exists(), "trash file past 30d window must be deleted");
    }

    #[test]
    fn auto_prune_on_start_caps_at_max_moves() {
        let (_g, archiver) = fake_archiver("cap");
        // Way more than the cap.
        for i in 0..(MAX_AUTO_PRUNE_MOVES + 50) {
            write_archive(archiver.history_dir(), &format!("old-{i}.md"));
        }
        // auto_prune_on_session_start uses real now; in this test the
        // files have current mtime so none should move. We exercise the
        // cap path via prune_expired_with_clock directly.
        unsafe { std::env::set_var(HISTORY_RETENTION_DAYS_ENV, "90"); }
        let pruned = archiver
            .prune_expired_with_clock(false, MAX_AUTO_PRUNE_MOVES, far_future_now())
            .unwrap();
        unsafe { std::env::remove_var(HISTORY_RETENTION_DAYS_ENV); }

        let moves = pruned
            .iter()
            .filter(|p| p.action == PrunedAction::MovedToTrash)
            .count();
        assert_eq!(moves, MAX_AUTO_PRUNE_MOVES, "must respect the move cap");
    }

    #[test]
    fn auto_prune_on_start_real_clock_is_noop_for_fresh_files() {
        let (_g, archiver) = fake_archiver("freshauto");
        write_archive(archiver.history_dir(), "fresh.md");
        let summary = archiver.auto_prune_on_session_start();
        assert_eq!(summary.moved, 0);
        assert_eq!(summary.deleted, 0);
        assert!(summary.format_one_line().is_none());
    }

    #[test]
    fn prune_summary_format_includes_singular_and_plural() {
        let one = PruneSummary {
            moved: 1,
            deleted: 0,
            bucket: "2026-05-13".into(),
        };
        let one_line = one.format_one_line().unwrap();
        assert!(one_line.contains("moved 1 file to .trash/2026-05-13/"), "{one_line}");

        let many = PruneSummary {
            moved: 12,
            deleted: 0,
            bucket: "2026-05-13".into(),
        };
        let many_line = many.format_one_line().unwrap();
        assert!(many_line.contains("moved 12 files to .trash/2026-05-13/"), "{many_line}");
    }

    #[test]
    fn date_bucket_for_secs_is_iso_for_known_epochs() {
        // 0 → 1970-01-01
        assert_eq!(date_bucket_for_secs(0), "1970-01-01");
        // 86_400 → 1970-01-02
        assert_eq!(date_bucket_for_secs(86_400), "1970-01-02");
        // 2026-05-13 00:00 UTC = unix 1_778_630_400.
        // (sanity: month transitions, leap years, etc.)
        let ts_2026_05_13: u64 = 1_778_630_400;
        assert_eq!(date_bucket_for_secs(ts_2026_05_13), "2026-05-13");
        // 2000-01-01 = unix 946684800
        assert_eq!(date_bucket_for_secs(946_684_800), "2000-01-01");
    }

    #[test]
    fn prune_summary_format_omits_line_when_empty() {
        let s = PruneSummary {
            moved: 0,
            deleted: 0,
            bucket: String::new(),
        };
        assert!(s.format_one_line().is_none());
    }
}
