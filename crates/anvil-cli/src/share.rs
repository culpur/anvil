//! `/share` command — ephemeral read-only conversation URL manager.
//!
//! `ShareManager` is the per-`LiveCli` component that owns the active share
//! map and dispatches `/share`, `/share stop`, and `/share list` commands.
//!
//! It is NOT a replacement for `/remote-control`.  Remote-control exposes full
//! bidirectional control of the whole Anvil instance.  Share creates a
//! lightweight, read-only, per-tab URL backed by the passage-culpur relay
//! under the `/v1/share/` namespace.
//!
//! ## Vault gate
//! All `/share` sub-commands require an unlocked vault.  The caller
//! (`run_command_for_tui` / `handle_repl_command`) must check
//! `runtime::vault_is_session_unlocked()` before calling any method here.
//! The methods themselves do NOT re-check — this keeps the pattern consistent
//! with how every other vault-gated command is handled in `main.rs`.
//!
//! ## TTL
//! Default TTL is 24 h (86 400 s).
//! // TODO(v2.3): --ttl flag  e.g. `/share --ttl 1h`

use std::collections::HashMap;
use runtime::{
    ActiveShare, BlockingShareClient, ShareError, ShareMessage, ShareSnapshot,
};
use crate::tui::state::{LogEntry, Tab};

/// Default share lifetime: 24 hours.
const DEFAULT_TTL_SECS: u64 = 86_400;

/// Per-`LiveCli` share manager.
///
/// Holds the map of active shares (keyed by `tab_id` as a string) and wraps
/// the blocking relay client.
pub(crate) struct ShareManager {
    /// tab_id (as string) → active share
    active_shares: HashMap<String, ActiveShare>,
}

impl ShareManager {
    /// Create a new, empty `ShareManager`.
    #[must_use]
    pub fn new() -> Self {
        Self {
            active_shares: HashMap::new(),
        }
    }

    /// Directly insert a share record (used by REPL code paths that build shares
    /// outside of `share_tab`'s log-driven flow).
    pub fn insert_active_share(&mut self, tab_id: String, share: ActiveShare) {
        self.active_shares.insert(tab_id, share);
    }

    /// Share the given tab.
    ///
    /// Serialises the tab's log into a sanitised `ShareSnapshot`, calls the
    /// relay, and stores the result in the active-shares map.
    ///
    /// Returns a human-readable output string suitable for both TUI scrollback
    /// and CLI REPL.
    pub fn share_tab(&mut self, tab: &Tab) -> String {
        let tab_id = tab.id.to_string();

        // If the tab is already shared, report the existing URL.
        if let Some(existing) = self.active_shares.get(&tab_id) {
            return format!(
                "Tab \"{}\" is already shared.\n  URL     {}\n  Expires in {}\n\nUse /share stop to revoke.",
                tab.name,
                existing.url,
                existing.expires_in_display(),
            );
        }

        // Build snapshot from tab log, skipping tool results and system messages.
        let messages: Vec<ShareMessage> = tab
            .log
            .iter()
            .filter_map(|entry| match entry {
                LogEntry::User(text) => Some(ShareMessage {
                    role: "user".to_string(),
                    content: text.clone(),
                }),
                LogEntry::Assistant(text) => Some(ShareMessage {
                    role: "assistant".to_string(),
                    content: text.clone(),
                }),
                // Tool calls, system messages, and images are excluded from
                // the read-only share — no credentials, no noise.
                LogEntry::ToolCall { .. }
                | LogEntry::System(_)
                | LogEntry::Image { .. } => None,
            })
            .collect();

        let snapshot = ShareSnapshot::build(tab.name.clone(), tab.model.clone(), messages);

        // Obtain a tokio runtime handle for the blocking client.
        let client = match self.make_blocking_client() {
            Ok(c) => c,
            Err(msg) => return msg,
        };

        match client.create_share(&tab_id, &tab.name, snapshot, DEFAULT_TTL_SECS) {
            Ok(share) => {
                let url = share.url.clone();
                let expires_display = share.expires_in_display();
                self.active_shares.insert(tab_id, share);
                format!("Shared at {url} (expires in {expires_display})")
            }
            Err(ShareError::RateLimitExceeded) => {
                "Rate limit: 10 shares/hour. Try again later.".to_string()
            }
            Err(ShareError::RelayNotFound) => {
                "Share is temporarily unavailable (relay endpoint not yet deployed).".to_string()
            }
            Err(e) => format!("Share unavailable (relay unreachable): {e}"),
        }
    }

    /// Revoke the share for the given tab, if one exists.
    ///
    /// Returns a human-readable output string.
    pub fn stop_share(&mut self, tab: &Tab) -> String {
        let tab_id = tab.id.to_string();
        let Some(share) = self.active_shares.remove(&tab_id) else {
            return "Current tab is not being shared.".to_string();
        };

        // Best-effort DELETE to the relay — if it fails, the share will expire
        // naturally.  We don't re-insert on failure because the local state is
        // the authoritative view.
        let delete_msg = match self.make_blocking_client() {
            Ok(client) => match client.delete_share(&share.share_id) {
                Ok(()) => String::new(),
                Err(e) => format!("\n  (relay delete failed: {e} — share will expire naturally)"),
            },
            Err(msg) => format!("\n  ({msg} — share will expire naturally)"),
        };

        format!("Share for \"{}\" stopped.{delete_msg}", tab.name)
    }

    /// List all active shares across all tabs.
    ///
    /// Returns a human-readable output string.
    #[must_use]
    pub fn list_shares(&self) -> String {
        if self.active_shares.is_empty() {
            return "No active shares.".to_string();
        }

        let mut lines = vec!["Active shares:".to_string()];
        let mut entries: Vec<&ActiveShare> = self.active_shares.values().collect();
        // Stable output order: sort by creation time.
        entries.sort_by_key(|s| s.created_at_secs);
        for share in entries {
            lines.push(format!(
                "  {}: {} (expires {})",
                share.tab_name,
                share.url,
                share.expires_in_display(),
            ));
        }
        lines.join("\n")
    }

    /// Return the number of active shares (used in tests).
    #[cfg(test)]
    pub fn active_share_count(&self) -> usize {
        self.active_shares.len()
    }

    /// Return a reference to the active share for a tab, if any (used in tests).
    #[cfg(test)]
    pub fn get_active_share(&self, tab_id: &str) -> Option<&ActiveShare> {
        self.active_shares.get(tab_id)
    }

    // ── Internal helpers ─────────────────────────────────────────────────────

    /// Build a `BlockingShareClient` using the best available tokio handle.
    fn make_blocking_client(&self) -> Result<BlockingShareClient, String> {
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => Ok(BlockingShareClient::default_client(handle)),
            Err(_) => match tokio::runtime::Runtime::new() {
                Ok(rt) => Ok(BlockingShareClient::default_client(rt.handle().clone())),
                Err(e) => Err(format!("Share: could not start async runtime: {e}")),
            },
        }
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use runtime::ActiveShare;

    /// Build a minimal `Tab` for testing.
    fn make_tab(id: usize, name: &str, log: Vec<LogEntry>) -> Tab {
        Tab {
            id,
            name: name.to_string(),
            log,
            model: "claude-sonnet-4-6".to_string(),
            session_id: format!("test-session-{id}"),
            // Remaining fields default to their zero values.
            pending_text: String::new(),
            scroll: 0,
            input: String::new(),
            input_placeholders: Vec::new(),
            pending_paste_blocks: Vec::new(),
            cursor: 0,
            history: Vec::new(),
            history_idx: None,
            history_backup: None,
            think_label: String::new(),
            think_start: None,
            think_frame: 0,
            input_tokens: 0,
            output_tokens: 0,
            session_start: std::time::Instant::now(),
            completion: Default::default(),
            has_unread: false,
            branches: Vec::new(),
            active_branch: 0,
            last_snapshot: None,
            log_len_at_snapshot: None,
            scrollback: crate::tui::scrollback::ScrollbackBuffer::new(),
            scrollback_pending_lines: 0,
            scrollback_state: crate::tui::scrollback::ScrollbackState::live(),
            transcript_verbose: false,
            ssh: None,
            has_runtime: false,
            cancel_token: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            message_queue: std::collections::VecDeque::new(),
            in_flight: false,
            tui_layout: crate::tui::state::Tab::load_default_layout(),
            layout_local: crate::tui::layouts::LayoutLocalState::Classic,
        }
    }

    // ── list_shares — empty ───────────────────────────────────────────────────

    #[test]
    fn list_shares_empty_when_no_active_shares() {
        let mgr = ShareManager::new();
        assert_eq!(mgr.list_shares(), "No active shares.");
    }

    // ── list_shares — with entries ────────────────────────────────────────────

    #[test]
    fn list_shares_shows_active_entries() {
        use std::time::{SystemTime, UNIX_EPOCH, Duration};
        let mut mgr = ShareManager::new();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or(Duration::ZERO)
            .as_secs();
        // Manually insert a share (bypasses network).
        mgr.active_shares.insert(
            "42".to_string(),
            ActiveShare {
                share_id: "test-share-id".to_string(),
                url: "https://share.anvilhub.culpur.net/test-share-id".to_string(),
                tab_id: "42".to_string(),
                tab_name: "Main".to_string(),
                created_at_secs: now,
                expires_at_secs: now + DEFAULT_TTL_SECS,
                expires_at: "2026-04-21T00:00:00Z".to_string(),
            },
        );
        let output = mgr.list_shares();
        assert!(output.contains("Main"), "should list tab name");
        assert!(output.contains("share.anvilhub.culpur.net"), "should list URL");
        assert!(!output.contains("No active shares"), "should not be empty message");
    }

    // ── stop_share — no active share ──────────────────────────────────────────

    #[test]
    fn stop_share_when_no_active_share_returns_message() {
        let mut mgr = ShareManager::new();
        let tab = make_tab(1, "Main", vec![]);
        let result = mgr.stop_share(&tab);
        assert_eq!(result, "Current tab is not being shared.");
    }

    // ── share_tab — already shared ────────────────────────────────────────────

    #[test]
    fn share_tab_reports_existing_share_without_new_relay_call() {
        use std::time::{SystemTime, UNIX_EPOCH, Duration};
        let mut mgr = ShareManager::new();
        let tab = make_tab(7, "Research", vec![]);
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or(Duration::ZERO)
            .as_secs();
        // Pre-populate with an existing share.
        mgr.active_shares.insert(
            "7".to_string(),
            ActiveShare {
                share_id: "existing-id".to_string(),
                url: "https://share.anvilhub.culpur.net/existing-id".to_string(),
                tab_id: "7".to_string(),
                tab_name: "Research".to_string(),
                created_at_secs: now,
                expires_at_secs: now + DEFAULT_TTL_SECS,
                expires_at: "2026-04-21T00:00:00Z".to_string(),
            },
        );
        let result = mgr.share_tab(&tab);
        assert!(result.contains("already shared"), "should report existing share");
        assert!(result.contains("existing-id"), "should include existing URL");
        // The share count should still be 1 — no duplicate was added.
        assert_eq!(mgr.active_share_count(), 1);
    }

    // ── snapshot extraction ───────────────────────────────────────────────────

    #[test]
    fn snapshot_excludes_tool_calls_and_system_messages() {
        // Build a log with mixed entry types and verify only user/assistant
        // entries make it into the snapshot messages list.
        let log = vec![
            LogEntry::User("hello".to_string()),
            LogEntry::ToolCall {
                name: "bash".to_string(),
                detail: "ls -la".to_string(),
                done: true,
                is_error: false,
                expanded: false,
                full_input: None,
                full_result: None,
            },
            LogEntry::System("session started".to_string()),
            LogEntry::Assistant("Hi there".to_string()),
            LogEntry::Image {
                path: "/tmp/img.png".to_string(),
                label: "screenshot".to_string(),
            },
        ];

        let messages: Vec<ShareMessage> = log
            .iter()
            .filter_map(|entry| match entry {
                LogEntry::User(text) => Some(ShareMessage {
                    role: "user".to_string(),
                    content: text.clone(),
                }),
                LogEntry::Assistant(text) => Some(ShareMessage {
                    role: "assistant".to_string(),
                    content: text.clone(),
                }),
                LogEntry::ToolCall { .. }
                | LogEntry::System(_)
                | LogEntry::Image { .. } => None,
            })
            .collect();

        assert_eq!(messages.len(), 2, "only user+assistant should be included");
        assert_eq!(messages[0].role, "user");
        assert_eq!(messages[1].role, "assistant");
    }
}
