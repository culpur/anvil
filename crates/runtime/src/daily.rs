//! Daily summary system — records session activity to `~/.anvil/daily/YYYY-MM-DD.json`.
//!
//! At session exit, a `SessionSummary` is appended to today's `DailySummary`.
//! The `/daily` command reads and formats these records, running reconciliation
//! to surface tasks that were requested but never completed.

use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::config::default_config_home;

// ─── Data Types ─────────────────────────────────────────────────────────────

/// Top-level daily record.  One file per calendar day.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DailySummary {
    /// Calendar date in `YYYY-MM-DD` format.
    pub date: String,
    /// All sessions recorded on this day (appended as each session exits).
    pub sessions: Vec<SessionSummary>,
    /// Open items carried forward from reconciliation.
    pub open_items: Vec<String>,
    /// Aggregate token count across all sessions.
    pub total_tokens: u32,
    /// Aggregate estimated cost (USD) across all sessions.
    pub total_cost_usd: f64,
}

/// One REPL session's contribution to the daily record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSummary {
    pub session_id: String,
    pub model: String,
    /// Wall-clock duration of the session in seconds.
    pub duration_secs: u64,
    pub tokens_used: u32,
    pub messages_count: usize,
    /// Tasks that were recognised as completed in this session.
    pub tasks_completed: Vec<String>,
    /// Tasks that were requested but not confirmed complete.
    pub tasks_open: Vec<String>,
    /// Paths that appeared in tool-result output as modified files.
    pub files_modified: Vec<String>,
    /// How many knowledge nominations were generated.
    pub nominations_generated: usize,
    /// How many credentials were auto-vaulted.
    pub credentials_auto_vaulted: usize,
}

// ─── Store ───────────────────────────────────────────────────────────────────

/// Thin wrapper around the `~/.anvil/daily/` directory.
pub struct DailyStore {
    dir: PathBuf,
}

impl DailyStore {
    /// Create a store pointing at the default `~/.anvil/daily/` directory.
    #[must_use]
    pub fn new() -> Self {
        Self::with_dir(default_config_home().join("daily"))
    }

    /// Create a store pointing at an arbitrary directory (useful for tests).
    #[must_use]
    pub fn with_dir(dir: PathBuf) -> Self {
        Self { dir }
    }

    // ── Persistence ──────────────────────────────────────────────────────────

    /// Load today's `DailySummary`, creating an empty one if none exists.
    #[must_use]
    pub fn today(&self) -> DailySummary {
        self.get(&today_date()).unwrap_or_else(|| DailySummary {
            date: today_date(),
            ..Default::default()
        })
    }

    /// Load the `DailySummary` for a specific date (`YYYY-MM-DD`).
    /// Returns `None` when no record exists for that date.
    #[must_use]
    pub fn get(&self, date: &str) -> Option<DailySummary> {
        let path = self.dir.join(format!("{date}.json"));
        let raw = fs::read_to_string(&path).ok()?;
        serde_json::from_str(&raw).ok()
    }

    /// Append a `SessionSummary` to today's daily record, updating aggregates.
    pub fn record_session(&self, session: SessionSummary) -> std::io::Result<()> {
        fs::create_dir_all(&self.dir)?;
        let mut summary = self.today();

        // Update aggregates.
        summary.total_tokens = summary.total_tokens.saturating_add(session.tokens_used);
        summary.total_cost_usd += estimate_cost_usd(session.tokens_used, &session.model);

        // Reconcile open items before pushing the new session.
        summary.sessions.push(session);
        summary.open_items = self.reconcile(&summary);

        let path = self.dir.join(format!("{}.json", summary.date));
        let json = serde_json::to_string_pretty(&summary)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        fs::write(&path, json)
    }

    // ── Formatting ───────────────────────────────────────────────────────────

    /// Render a `DailySummary` as a human-readable multi-line string.
    #[must_use]
    pub fn format_summary(&self, summary: &DailySummary) -> String {
        let mut lines: Vec<String> = Vec::new();

        lines.push(format!("Daily Summary - {}", summary.date));
        lines.push("─".repeat(45));

        if summary.sessions.is_empty() {
            lines.push("No sessions recorded today.".to_string());
            return lines.join("\n");
        }

        lines.push(format!(
            "Sessions: {} | Tokens: {} | Cost: ${:.2}",
            summary.sessions.len(),
            format_tokens(summary.total_tokens),
            summary.total_cost_usd,
        ));
        lines.push(String::new());

        for (i, s) in summary.sessions.iter().enumerate() {
            let dur_min = s.duration_secs / 60;
            let dur_s = s.duration_secs % 60;
            let duration_str = if dur_min > 0 {
                format!("{dur_min}min {dur_s}s")
            } else {
                format!("{dur_s}s")
            };

            lines.push(format!(
                "Session {} ({}, {}):",
                i + 1,
                s.model,
                duration_str,
            ));

            for task in &s.tasks_completed {
                lines.push(format!("  v {task}"));
            }
            for task in &s.tasks_open {
                lines.push(format!("  ! {task} -- NOT COMPLETED"));
            }

            if s.tasks_completed.is_empty() && s.tasks_open.is_empty() {
                lines.push(format!(
                    "  {} messages, {} tokens",
                    s.messages_count,
                    format_tokens(s.tokens_used)
                ));
            }
            lines.push(String::new());
        }

        let open = self.reconcile(summary);
        if !open.is_empty() {
            lines.push("! Open Items (carry forward):".to_string());
            for (i, item) in open.iter().enumerate() {
                lines.push(format!("  {}. {item}", i + 1));
            }
            lines.push("─".repeat(45));
        } else {
            lines.push("All tasks completed today.".to_string());
            lines.push("─".repeat(45));
        }

        lines.join("\n")
    }

    /// Return deduplicated open items across all sessions in a summary.
    #[must_use]
    pub fn reconcile(&self, summary: &DailySummary) -> Vec<String> {
        // Collect everything that was ever marked open; remove anything that
        // subsequently appeared as completed in a later session.
        let mut open: Vec<String> = Vec::new();
        let mut completed: std::collections::HashSet<String> = std::collections::HashSet::new();

        for s in &summary.sessions {
            for task in &s.tasks_completed {
                completed.insert(normalise_task(task));
            }
        }

        for s in &summary.sessions {
            for task in &s.tasks_open {
                let key = normalise_task(task);
                if !completed.contains(&key) && !open.iter().any(|o| normalise_task(o) == key) {
                    open.push(task.clone());
                }
            }
        }
        open
    }

    /// Return the last `days` daily summaries (most recent first).
    #[must_use]
    pub fn recent(&self, days: usize) -> Vec<DailySummary> {
        let entries = match fs::read_dir(&self.dir) {
            Ok(e) => e,
            Err(_) => return Vec::new(),
        };

        let mut files: Vec<String> = entries
            .flatten()
            .filter_map(|e| {
                let name = e.file_name();
                let s = name.to_string_lossy();
                if s.ends_with(".json") {
                    Some(s.trim_end_matches(".json").to_owned())
                } else {
                    None
                }
            })
            .filter(|s| looks_like_date(s))
            .collect();

        // Sort descending so most recent comes first.
        files.sort_unstable_by(|a, b| b.cmp(a));
        files.truncate(days);

        files
            .into_iter()
            .filter_map(|date| self.get(&date))
            .collect()
    }
}

impl Default for DailyStore {
    fn default() -> Self {
        Self::new()
    }
}

// ─── Task extraction ─────────────────────────────────────────────────────────

/// Imperative verbs that indicate a user is requesting a task.
const TASK_VERBS: &[&str] = &[
    "fix", "add", "update", "create", "build", "deploy", "write",
    "remove", "refactor", "delete", "implement", "migrate", "install",
    "configure", "set up", "run", "generate", "publish", "review",
    "analyse", "analyze", "check", "test", "send", "push", "pull",
    "rename", "move", "copy", "upgrade", "downgrade", "enable", "disable",
    "start", "stop", "restart", "connect", "open", "close",
];

/// Response phrases that indicate the AI completed a task.
const COMPLETION_INDICATORS: &[&str] = &[
    "done", "complete", "completed", "fixed", "deployed", "created",
    "updated", "added", "removed", "deleted", "implemented", "installed",
    "migrated", "configured", "generated", "published", "renamed",
    "moved", "upgraded", "downgraded", "enabled", "disabled", "started",
    "stopped", "restarted", "connected", "written", "built", "refactored",
    "success", "successfully", "all tests pass", "0 errors",
];

/// Extract user-requested tasks from a slice of conversation messages.
///
/// Each user message whose first word (lowercased) is a recognised imperative
/// verb is treated as a task request.  If the immediately-following assistant
/// message contains a completion indicator the task is marked complete;
/// otherwise it is marked open.
#[must_use]
pub fn extract_tasks(
    messages: &[crate::session::ConversationMessage],
) -> (Vec<String>, Vec<String>) {
    use crate::session::{ContentBlock, MessageRole};

    let mut completed: Vec<String> = Vec::new();
    let mut open: Vec<String> = Vec::new();

    let mut i = 0;
    while i < messages.len() {
        let msg = &messages[i];
        if msg.role == MessageRole::User {
            if let Some(text) = first_text_block(&msg.blocks) {
                let trimmed = text.trim();
                if is_task_request(trimmed) {
                    let task = first_sentence(trimmed);

                    // Peek at the next assistant reply.
                    let next_assistant = messages[i + 1..]
                        .iter()
                        .find(|m| m.role == MessageRole::Assistant);

                    let done = next_assistant.is_some_and(|a| {
                        first_text_block(&a.blocks)
                            .is_some_and(|t| has_completion_indicator(t))
                    });

                    if done {
                        completed.push(task);
                    } else {
                        open.push(task);
                    }
                }
            }
        }
        i += 1;
    }

    (completed, open)
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

/// Return today's date in `YYYY-MM-DD` format using UTC.
#[must_use]
pub fn today_date() -> String {
    // We deliberately avoid pulling in a date-time crate.  The epoch offset
    // from UTC midnight gives us the day number; from that we compute Y/M/D
    // via the proleptic Gregorian algorithm.
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    epoch_secs_to_date(secs)
}

/// Convert Unix epoch seconds to a `YYYY-MM-DD` string (UTC, Gregorian).
#[must_use]
pub fn epoch_secs_to_date(secs: u64) -> String {
    // Days since 1970-01-01.
    let days = secs / 86_400;

    // Gregorian calendar algorithm (civil date from Julian Day Number).
    let z = days + 719_468;
    let era = z / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };

    format!("{y:04}-{m:02}-{d:02}")
}

/// Rough cost estimate for display (uses default sonnet-tier pricing when
/// the model is unknown, just as `UsageTracker` does).
fn estimate_cost_usd(tokens: u32, model: &str) -> f64 {
    use crate::usage::pricing_for_model;
    let pricing = pricing_for_model(model).unwrap_or(crate::usage::ModelPricing::default_sonnet_tier());
    // Simple total based on input-cost-per-million as a proxy (accurate enough
    // for summary display).
    f64::from(tokens) / 1_000_000.0 * pricing.input_cost_per_million
}

/// Format a token count with thousands separators.
fn format_tokens(n: u32) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, ch) in s.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out.chars().rev().collect()
}

/// Return the first `ContentBlock::Text` body from a block list.
fn first_text_block(blocks: &[crate::session::ContentBlock]) -> Option<&str> {
    use crate::session::ContentBlock;
    for b in blocks {
        if let ContentBlock::Text { text } = b {
            return Some(text.as_str());
        }
    }
    None
}

/// Return true if the first word of `text` is a recognised task verb.
fn is_task_request(text: &str) -> bool {
    let first = text
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    // Strip trailing punctuation so "Fix." still matches "fix".
    let first = first.trim_end_matches(|c: char| !c.is_alphanumeric());
    TASK_VERBS.contains(&first)
}

/// Return true when `text` contains any completion indicator phrase.
fn has_completion_indicator(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    COMPLETION_INDICATORS.iter().any(|&ind| lower.contains(ind))
}

/// Truncate `text` at the first sentence boundary (`. `, `.\n`, or 80 chars).
fn first_sentence(text: &str) -> String {
    let limit = text
        .char_indices()
        .find(|&(i, c)| {
            c == '.' && {
                let rest = &text[i + 1..];
                rest.starts_with(' ') || rest.starts_with('\n') || rest.is_empty()
            }
        })
        .map(|(i, _)| i + 1)
        .unwrap_or(text.len());

    let s = &text[..limit];
    // Cap at 80 characters.
    if s.len() > 80 {
        format!("{}…", &s[..79])
    } else {
        s.to_owned()
    }
}

/// Normalise a task string for deduplication: lowercase and strip punctuation.
fn normalise_task(s: &str) -> String {
    s.to_ascii_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric() || c.is_whitespace())
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Return true when a string looks like `YYYY-MM-DD`.
fn looks_like_date(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 10
        && b[4] == b'-'
        && b[7] == b'-'
        && b[..4].iter().all(|c| c.is_ascii_digit())
        && b[5..7].iter().all(|c| c.is_ascii_digit())
        && b[8..].iter().all(|c| c.is_ascii_digit())
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    // ── Helpers ──────────────────────────────────────────────────────────────

    fn temp_store() -> (DailyStore, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = DailyStore::with_dir(dir.path().to_path_buf());
        (store, dir)
    }

    fn make_session(id: &str, completed: &[&str], open: &[&str]) -> SessionSummary {
        SessionSummary {
            session_id: id.to_owned(),
            model: "claude-sonnet-4-6".to_owned(),
            duration_secs: 300,
            tokens_used: 1_000,
            messages_count: 4,
            tasks_completed: completed.iter().map(|s| (*s).to_owned()).collect(),
            tasks_open: open.iter().map(|s| (*s).to_owned()).collect(),
            files_modified: vec![],
            nominations_generated: 0,
            credentials_auto_vaulted: 0,
        }
    }

    // ── Basic store operations ────────────────────────────────────────────────

    #[test]
    fn record_session_writes_json() {
        let (store, _dir) = temp_store();
        let session = make_session("s1", &["Fixed the bug."], &[]);
        store.record_session(session).expect("record_session should succeed");

        let summary = store.today();
        assert_eq!(summary.sessions.len(), 1);
        assert_eq!(summary.sessions[0].session_id, "s1");
        assert_eq!(summary.total_tokens, 1_000);
    }

    #[test]
    fn multiple_sessions_aggregate_tokens() {
        let (store, _dir) = temp_store();
        store
            .record_session(make_session("s1", &["Fixed A."], &[]))
            .unwrap();
        store
            .record_session(make_session("s2", &["Fixed B."], &[]))
            .unwrap();

        let summary = store.today();
        assert_eq!(summary.sessions.len(), 2);
        assert_eq!(summary.total_tokens, 2_000);
    }

    #[test]
    fn get_returns_none_for_missing_date() {
        let (store, _dir) = temp_store();
        assert!(store.get("1970-01-01").is_none());
    }

    // ── Reconciliation ───────────────────────────────────────────────────────

    #[test]
    fn reconcile_flags_open_tasks() {
        let (store, _dir) = temp_store();
        let session = make_session(
            "s1",
            &["Fix widget disconnect bug."],
            &["Session persistence design."],
        );
        store.record_session(session).unwrap();
        let summary = store.today();
        let open = store.reconcile(&summary);
        assert_eq!(open.len(), 1);
        assert!(open[0].contains("persistence"));
    }

    #[test]
    fn reconcile_clears_when_later_session_completes_task() {
        let (store, _dir) = temp_store();

        // Session 1: opened a task.
        store
            .record_session(make_session("s1", &[], &["Session persistence design."]))
            .unwrap();

        // Session 2: completed it.
        store
            .record_session(make_session("s2", &["Session persistence design."], &[]))
            .unwrap();

        let summary = store.today();
        let open = store.reconcile(&summary);
        assert!(
            open.is_empty(),
            "reconcile should see the task as completed: {open:?}"
        );
    }

    #[test]
    fn reconcile_deduplicates_open_items() {
        let (store, _dir) = temp_store();
        store
            .record_session(make_session("s1", &[], &["Draw loop opt."]))
            .unwrap();
        store
            .record_session(make_session("s2", &[], &["Draw loop opt."]))
            .unwrap();

        let summary = store.today();
        assert_eq!(store.reconcile(&summary).len(), 1);
    }

    // ── Formatting ───────────────────────────────────────────────────────────

    #[test]
    fn format_summary_includes_open_items() {
        let (store, _dir) = temp_store();
        store
            .record_session(make_session(
                "s1",
                &["Fixed RC widget."],
                &["Draw loop opt."],
            ))
            .unwrap();
        let summary = store.today();
        let out = store.format_summary(&summary);
        assert!(out.contains("Draw loop opt."));
        assert!(out.contains("Open Items"));
    }

    #[test]
    fn format_summary_shows_session_count() {
        let (store, _dir) = temp_store();
        store.record_session(make_session("s1", &[], &[])).unwrap();
        store.record_session(make_session("s2", &[], &[])).unwrap();
        let summary = store.today();
        let out = store.format_summary(&summary);
        assert!(out.contains("Sessions: 2"));
    }

    // ── Date helpers ─────────────────────────────────────────────────────────

    #[test]
    fn epoch_secs_known_date() {
        // 2024-01-15 00:00:00 UTC = 1705276800
        assert_eq!(epoch_secs_to_date(1_705_276_800), "2024-01-15");
    }

    #[test]
    fn today_date_looks_like_date() {
        let d = today_date();
        assert!(looks_like_date(&d), "today_date() should be YYYY-MM-DD: {d}");
    }

    #[test]
    fn today_date_is_current_year() {
        let d = today_date();
        let year: u32 = d[..4].parse().unwrap();
        // The tests will be running somewhere between 2024 and 2100.
        assert!(year >= 2024, "year should be >= 2024: {year}");
    }

    // ── Task extraction ───────────────────────────────────────────────────────

    #[test]
    fn extract_tasks_marks_completed() {
        use crate::session::{ContentBlock, ConversationMessage, MessageRole};

        let messages = vec![
            ConversationMessage {
                role: MessageRole::User,
                blocks: vec![ContentBlock::Text {
                    text: "Fix the broken test.".to_owned(),
                }],
                usage: None,
            },
            ConversationMessage {
                role: MessageRole::Assistant,
                blocks: vec![ContentBlock::Text {
                    text: "Done — I fixed the broken test.".to_owned(),
                }],
                usage: None,
            },
        ];

        let (completed, open) = extract_tasks(&messages);
        assert_eq!(completed.len(), 1, "should have 1 completed task");
        assert!(open.is_empty(), "open should be empty");
    }

    #[test]
    fn extract_tasks_marks_open_when_no_completion() {
        use crate::session::{ContentBlock, ConversationMessage, MessageRole};

        let messages = vec![
            ConversationMessage {
                role: MessageRole::User,
                blocks: vec![ContentBlock::Text {
                    text: "Build the new module.".to_owned(),
                }],
                usage: None,
            },
            ConversationMessage {
                role: MessageRole::Assistant,
                blocks: vec![ContentBlock::Text {
                    text: "I can see two approaches for this problem...".to_owned(),
                }],
                usage: None,
            },
        ];

        let (_completed, open) = extract_tasks(&messages);
        assert_eq!(open.len(), 1, "should have 1 open task");
    }

    // ── Recent list ───────────────────────────────────────────────────────────

    #[test]
    fn recent_returns_n_most_recent() {
        let dir = tempfile::tempdir().unwrap();

        // Write three fake daily files.
        for date in &["2026-01-01", "2026-01-02", "2026-01-03"] {
            let summary = DailySummary {
                date: (*date).to_owned(),
                sessions: vec![make_session("s", &[], &[])],
                ..Default::default()
            };
            let path = dir.path().join(format!("{date}.json"));
            fs::write(&path, serde_json::to_string(&summary).unwrap()).unwrap();
        }

        let store = DailyStore::with_dir(dir.path().to_path_buf());
        let recent = store.recent(2);
        assert_eq!(recent.len(), 2);
        // Most recent first.
        assert_eq!(recent[0].date, "2026-01-03");
        assert_eq!(recent[1].date, "2026-01-02");
    }
}
