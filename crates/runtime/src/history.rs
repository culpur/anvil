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
    std::env::var("HOME")
        .ok()
        .map(PathBuf::from)
        .or_else(|| std::env::var("USERPROFILE").ok().map(PathBuf::from))
}

fn truncate_chars(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    let mut truncated: String = s.chars().take(max_chars).collect();
    truncated.push_str("...[truncated]");
    truncated
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
    fn compact_threshold_defaults_to_85() {
        // Ensure the env var is not set before checking.
        std::env::remove_var(COMPACT_THRESHOLD_ENV);
        assert_eq!(HistoryArchiver::compact_threshold_pct(), 85);
    }

    #[test]
    fn compact_threshold_reads_env_var() {
        std::env::set_var(COMPACT_THRESHOLD_ENV, "70");
        assert_eq!(HistoryArchiver::compact_threshold_pct(), 70);
        std::env::remove_var(COMPACT_THRESHOLD_ENV);
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
}
