/// UI state types: events, tab state, log entries, completion popup.
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::SyncSender;
use std::time::Instant;

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use runtime::{Rgb, Theme};

use super::helpers::strip_ansi;
use super::scrollback::{ScrollbackBuffer, ScrollbackState};

// ─── Public event type ────────────────────────────────────────────────────────

/// Events pushed from the streaming/tool path into the TUI.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub enum TuiEvent {
    /// Incremental text delta from the assistant.
    TextDelta(String),
    /// The current streaming text block is complete (flush pending buffer).
    TextDone,
    /// A tool call started (still accumulating input).
    ToolCallStart { name: String },
    /// A tool call is fully known and being executed.
    ToolCallActive {
        name: String,
        detail: String,
        /// Raw JSON input the model emitted — stored for Ctrl+O expansion.
        full_input: String,
    },
    /// A tool call returned (success).
    ToolResult {
        name: String,
        summary: String,
        is_error: bool,
    },
    /// The thinking / pending indicator label changed.
    ThinkLabel(String),
    /// Token usage update.
    Tokens { input: u32, output: u32 },
    /// A system-level notice (errors, notifications).
    System(String),
    /// The turn is complete — clear the thinking indicator.
    TurnDone,
    /// T4-N: `/clear` finished resetting the runtime — wipe the visible
    /// display state for the active tab (or every tab when `all_tabs` is
    /// true), so the user no longer sees messages from the discarded
    /// session.
    WorkspaceClear { all_tabs: bool },
    /// Bug-3 Commit 4: a permission decision is required from the user.
    ///
    /// The worker that emitted this event blocks on `response_tx.recv()`
    /// until the user approves or denies via the TUI modal.  The TUI
    /// renders the modal when the user is on that tab; for background tabs
    /// the request is queued and the tab bar shows a ⚠ marker.
    ///
    /// Pattern: std::sync::mpsc oneshot — `SyncSender<PermissionReply>` with
    /// capacity 1 so the TUI can send exactly one reply.
    PermissionRequired {
        tool_name: String,
        required_mode: String,
        current_mode: String,
        input_summary: String,
        /// Worker blocks on the paired `Receiver` until this is sent.
        response_tx: std::sync::mpsc::SyncSender<PermissionReply>,
    },
}

/// The TUI's response to a `PermissionRequired` event.
///
/// Sent through the `response_tx` channel the worker is blocking on.
#[derive(Debug, Clone)]
pub enum PermissionReply {
    Allow,
    AllowAlways,
    Deny,
}

/// A permission request that has been received from a worker thread and is
/// awaiting the user's decision.  Stored in `AnvilTui::pending_permissions`
/// keyed by `tab_id`.
#[derive(Debug)]
pub(crate) struct PendingPermission {
    pub tool_name: String,
    pub required_mode: String,
    pub current_mode: String,
    pub input_summary: String,
    /// Send one `PermissionReply` here to unblock the worker.
    pub response_tx: std::sync::mpsc::SyncSender<PermissionReply>,
}

/// A TUI event tagged with the tab it belongs to.  The streaming/tool path
/// constructs these via `TuiSender::send` (which stamps the sender's
/// `tab_id`); the TUI's `apply_tagged_event` reads `tab_id` to route to the
/// correct tab.
#[derive(Debug, Clone)]
pub struct TaggedTuiEvent {
    pub tab_id: usize,
    pub event: TuiEvent,
}

/// A cloneable sender that model/tool code uses to push `TuiEvent`s.
///
/// Each clone carries the `tab_id` of the runtime that owns it; sends stamp
/// that `tab_id` onto every event automatically.  The underlying
/// `SyncSender<TaggedTuiEvent>` is shared across all senders for the same
/// TUI instance; only the stamp differs.
#[derive(Debug, Clone)]
pub struct TuiSender {
    inner: SyncSender<TaggedTuiEvent>,
    tab_id: usize,
}

impl TuiSender {
    /// Construct a sender bound to a specific `tab_id`.
    pub fn new(inner: SyncSender<TaggedTuiEvent>, tab_id: usize) -> Self {
        Self { inner, tab_id }
    }

    /// The `tab_id` this sender stamps onto every event.
    pub fn tab_id(&self) -> usize {
        self.tab_id
    }

    /// Send an event tagged with this sender's `tab_id`.  Errors are dropped
    /// silently (the TUI may have closed).
    pub fn send(&self, event: TuiEvent) {
        let _ = self.inner.send(TaggedTuiEvent { tab_id: self.tab_id, event });
    }

    /// Rebind this sender to a different `tab_id`.  The underlying channel is
    /// unchanged.  Used when re-routing a runtime (e.g. on `/fork`).
    pub fn with_tab_id(&self, tab_id: usize) -> Self {
        Self { inner: self.inner.clone(), tab_id }
    }
}

// ─── InFlightInterruption ────────────────────────────────────────────────────

/// The reason `wait_for_turn_end_for_tab` returned to the caller.
///
/// Without this type the wait function was a modal "you can only type plain
/// characters and Backspace" gate.  With it the main loop can react to user
/// actions that arrived while a turn was streaming — tab switches, new-tab
/// requests, slash commands typed mid-stream, and chat submits on idle tabs.
///
/// The main loop dispatches on the variant, performs whatever side-effect is
/// needed, then re-enters the wait if any turn is still in flight.
#[derive(Debug)]
pub enum InFlightInterruption {
    /// The target tab's turn finished naturally.  Caller reaps the worker.
    TurnDone,

    /// User switched to a different tab (F2, F3, Ctrl+Left/Right,
    /// Ctrl+1-9, Alt+1-9, or a mouse click on a tab label).
    /// `tui.active_tab_index()` now reflects the new focus.
    /// If any turn is still in flight the caller should re-enter the wait on
    /// whichever tab is currently focused (or the first still-running tab).
    TabSwitched,

    /// User pressed Ctrl+T (new tab).  The wait function only signals the
    /// request; it does NOT call `tui.new_tab` or `cli.push_tab_runtime`.
    /// The caller must create the tab, install its runtime, and then
    /// re-enter the wait if other turns are still running.
    OpenNewTab,

    /// User pressed Ctrl+W (close active tab).  The wait function only
    /// signals the request; the caller closes the tab, updates cli state,
    /// and re-enters the wait if other turns are still running.
    CloseActiveTab,

    /// User typed `/<command>` and pressed Enter on the active tab.
    /// The returned `String` is the full line including the leading `/`.
    /// Caller routes it through the normal slash-command handler
    /// (`handle_repl_command_tui` / the `/tab`/`/fork` pre-checks).
    /// After dispatching, the caller re-enters the wait if turns remain.
    SlashCommand(String),

    /// User typed a non-slash chat message and pressed Enter while focused
    /// on a tab whose own turn is NOT in flight (i.e. they have switched to
    /// an idle tab and submitted there).
    /// Caller calls `cli.spawn_turn_for_tab(active_idx, prompt, ...)` for
    /// the now-active idle tab, then re-enters the wait.
    SubmitChatPrompt(String),

    /// The TUI event channel disconnected unexpectedly.  Treat as TurnDone.
    ChannelClosed,
}

// ─── Internal message log ─────────────────────────────────────────────────────

/// One entry in the scrollable message log.
#[derive(Debug, Clone)]
pub(crate) enum LogEntry {
    /// User prompt.
    User(String),
    /// Completed assistant message (plain ANSI-stripped text for ratatui).
    Assistant(String),
    /// Tool call block.
    ToolCall {
        name: String,
        detail: String,
        done: bool,
        is_error: bool,
        /// Whether the card is expanded (Ctrl+O).
        expanded: bool,
        /// Raw JSON input the model emitted — shown when expanded.
        full_input: Option<String>,
        /// Raw result body — shown when expanded and done.
        full_result: Option<String>,
    },
    /// System message / error.
    System(String),
    #[allow(dead_code)]
    /// Inline image (rendered via iTerm2/Kitty protocol if supported).
    Image {
        path: String,
        label: String,
    },
}

/// Convert a runtime `Rgb` triple into a ratatui `Color`.
#[inline]
pub(super) const fn rgb(c: Rgb) -> Color {
    Color::Rgb(c.0, c.1, c.2)
}

impl LogEntry {
    /// Render this entry as a list of ratatui `Line`s for display.
    ///
    /// Convenience wrapper around [`Self::to_lines_with`] with
    /// `force_expand = false`. Kept for tests and any future call site that
    /// doesn't need the verbose override.
    #[allow(dead_code)]
    pub(super) fn to_lines(&self, max_width: u16, theme: &Theme) -> Vec<Line<'static>> {
        self.to_lines_with(max_width, theme, false)
    }

    /// Render with an optional `force_expand` override that treats every
    /// `ToolCall` card as expanded regardless of its per-card `expanded`
    /// flag. Used by CC-139-F5 transcript verbose mode (`v` in HISTORICAL
    /// VIEW) so tool input/output renders without truncation across the
    /// whole scrollback. `to_lines` is now a thin wrapper that passes
    /// `false`, so non-verbose callers behave exactly as before.
    pub(super) fn to_lines_with(
        &self,
        max_width: u16,
        theme: &Theme,
        force_expand: bool,
    ) -> Vec<Line<'static>> {
        let width = max_width.saturating_sub(4) as usize;
        match self {
            LogEntry::User(text) => {
                let mut lines = vec![Line::from(vec![
                    Span::styled("You  ", Style::default().fg(rgb(theme.accent)).add_modifier(Modifier::BOLD)),
                    Span::styled(
                        text.lines().next().unwrap_or("").to_string(),
                        Style::default().fg(rgb(theme.text_primary)).add_modifier(Modifier::BOLD),
                    ),
                ])];
                for extra in text.lines().skip(1) {
                    lines.push(Line::from(Span::styled(
                        format!("     {extra}"),
                        Style::default().fg(rgb(theme.text_primary)),
                    )));
                }
                lines.push(Line::from(""));
                lines
            }
            LogEntry::Assistant(text) => {
                let clean = strip_ansi(text);
                let mut lines: Vec<Line<'static>> = clean
                    .lines()
                    .map(|line| Line::from(Span::raw(line.to_string())))
                    .collect();
                lines.push(Line::from(""));
                lines
            }
            LogEntry::ToolCall {
                name,
                detail,
                done,
                is_error,
                expanded,
                full_input,
                full_result,
            } => {
                // CC-139-F5: transcript verbose mode forces every ToolCall
                // card to render as expanded, even when the per-card flag
                // is off. The local `expanded` shadow keeps the rest of
                // this arm's logic untouched.
                let expanded_local = *expanded || force_expand;
                let expanded = &expanded_local;
                let (border_color, icon, label) = if *is_error {
                    (rgb(theme.error), "✗", format!("{name} (error)"))
                } else if *done {
                    (rgb(theme.success), "✓", name.clone())
                } else {
                    (rgb(theme.accent), "●", name.clone())
                };

                let dash_count = (width.saturating_sub(label.len() + 6)).min(width);
                let dashes = "─".repeat(dash_count);
                let top = format!("╭─ {icon} {label} {dashes}╮");

                let mut lines = vec![Line::from(Span::styled(
                    top,
                    Style::default().fg(border_color),
                ))];

                let inner_width = width.saturating_sub(2);
                let muted = Color::DarkGray;

                // Detail lines: uncapped when expanded, capped at 12 otherwise.
                let detail_cap = if *expanded { usize::MAX } else { 12 };
                for dl in detail.lines().take(detail_cap) {
                    let truncated = if !*expanded && dl.chars().count() > inner_width {
                        format!("{}…", dl.chars().take(inner_width.saturating_sub(1)).collect::<String>())
                    } else {
                        dl.to_string()
                    };
                    lines.push(Line::from(vec![
                        Span::styled("│ ", Style::default().fg(border_color)),
                        Span::raw(truncated),
                    ]));
                }

                // When done: show one-line result summary in muted color.
                if *done {
                    let result_text = if let Some(raw) = full_result {
                        crate::format_tool::tool_result_summary(name, raw, *is_error)
                    } else {
                        String::new()
                    };
                    if !result_text.is_empty() {
                        if *expanded {
                            // Expanded: show full result body, up to 200 lines.
                            let raw_body = full_result.as_deref().unwrap_or("");
                            lines.push(Line::from(vec![
                                Span::styled("│ ", Style::default().fg(border_color)),
                                Span::styled("── result ──", Style::default().fg(muted)),
                            ]));
                            for rl in raw_body.lines().take(200) {
                                let truncated = if rl.chars().count() > inner_width {
                                    format!("{}…", rl.chars().take(inner_width.saturating_sub(1)).collect::<String>())
                                } else {
                                    rl.to_string()
                                };
                                lines.push(Line::from(vec![
                                    Span::styled("│ ", Style::default().fg(border_color)),
                                    Span::styled(truncated, Style::default().fg(muted)),
                                ]));
                            }
                        } else {
                            lines.push(Line::from(vec![
                                Span::styled("│ ", Style::default().fg(border_color)),
                                Span::styled(result_text, Style::default().fg(muted)),
                            ]));
                        }
                    }
                }

                // Footer with Ctrl+O hint when done.
                let bot = if *done {
                    if *expanded {
                        format!("╰─ Ctrl+O to collapse {:─<w$}╯", "", w = width.saturating_sub(22))
                    } else {
                        format!("╰─ Ctrl+O to expand {:─<w$}╯", "", w = width.saturating_sub(20))
                    }
                } else {
                    format!("╰{:─<width$}╯", "", width = width + 2)
                };

                lines.push(Line::from(Span::styled(
                    bot,
                    Style::default().fg(border_color),
                )));
                lines.push(Line::from(""));
                lines
            }
            LogEntry::System(text) => {
                let sys_style = Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::ITALIC);
                let link_style = Style::default()
                    .fg(rgb(theme.accent))
                    .add_modifier(Modifier::UNDERLINED);

                let mut lines = Vec::new();
                for (i, raw_line) in text.lines().enumerate() {
                    let prefix = if i == 0 { "◆  " } else { "   " };
                    let mut spans = vec![Span::styled(prefix.to_string(), sys_style)];
                    // Highlight URLs with accent color (no OSC 8 — ratatui doesn't support it)
                    let mut rest = raw_line;
                    while let Some(start) = rest.find("https://").or_else(|| rest.find("http://")) {
                        if start > 0 {
                            spans.push(Span::styled(rest[..start].to_string(), sys_style));
                        }
                        let url_end = rest[start..].find(|c: char| c.is_whitespace() || c == '>' || c == ')' || c == ']')
                            .map_or(rest.len(), |e| start + e);
                        let url = &rest[start..url_end];
                        spans.push(Span::styled(url.to_string(), link_style));
                        rest = &rest[url_end..];
                    }
                    if !rest.is_empty() {
                        spans.push(Span::styled(rest.to_string(), sys_style));
                    }
                    lines.push(Line::from(spans));
                }
                lines.push(Line::from(""));
                lines
            }
            LogEntry::Image { path, label } => {
                // For terminals that support inline images, we'd emit the protocol escape.
                // Ratatui doesn't directly support image protocols, so we render a styled
                // placeholder that the raw terminal writer can intercept.
                let display = if label.is_empty() {
                    format!("[Image: {path}]")
                } else {
                    format!("[Image: {label} — {path}]")
                };
                vec![
                    Line::from(Span::styled(
                        display,
                        Style::default().fg(rgb(theme.accent)).add_modifier(Modifier::ITALIC),
                    )),
                    Line::from(""),
                ]
            }
        }
    }
}

// ─── Tab ──────────────────────────────────────────────────────────────────────

/// All per-tab mutable state.
pub(crate) struct Tab {
    pub id: usize,
    pub name: String,
    pub log: Vec<LogEntry>,
    pub pending_text: String,
    pub scroll: usize,
    pub input: String,
    pub cursor: usize,
    pub history: Vec<String>,
    pub history_idx: Option<usize>,
    pub history_backup: Option<String>,
    pub think_label: String,
    pub think_start: Option<Instant>,
    pub think_frame: usize,
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub session_start: Instant,
    pub model: String,
    pub session_id: String,
    pub completion: CompletionPopup,
    pub has_unread: bool,
    /// Conversation branches — each is an Arc-shared snapshot of log at branch
    /// point. Using Arc lets `/fork` create a branch in O(1) (refcount bump,
    /// no element clone). The actual log only diverges on the next push, at
    /// which point the Arc gets cloned-on-write into the live `log` Vec.
    /// (T3-I — see #344/#411.)
    pub branches: Vec<(String, Arc<Vec<LogEntry>>)>,
    /// Active branch index (0 = main, 1+ = branches).
    pub active_branch: usize,
    /// T3-I: most recent Arc snapshot of `log` for structural-sharing on
    /// repeated `/fork` and `/fork switch`. Reused when `log_len_at_snapshot`
    /// equals `log.len()` (i.e. no pushes have happened since capture).
    pub last_snapshot: Option<Arc<Vec<LogEntry>>>,
    /// `log.len()` at the time `last_snapshot` was taken.
    pub log_len_at_snapshot: Option<usize>,
    /// Ring buffer holding the last N rendered text lines for in-TUI scrollback.
    pub scrollback: ScrollbackBuffer,
    /// How many lines at the END of `scrollback` came from `pending_text`
    /// rather than from finalized `log` entries. Used by the draw path to
    /// pop the (mutable) pending tail before re-pushing it from the current
    /// pending_text — fixes the bug where mid-stream deltas left an early
    /// truncated prefix cached in scrollback forever.
    pub scrollback_pending_lines: usize,
    /// Current scrollback navigation state (None = live view).
    pub scrollback_state: ScrollbackState,
    /// CC-139-F5: transcript verbose mode. Toggled by `v` in HISTORICAL
    /// VIEW. When `true`, every `LogEntry::ToolCall` renders as if its
    /// per-card `expanded` flag were set — i.e. tool input/output is
    /// shown in full instead of the usual truncated detail + one-line
    /// result summary. Defaults to `false` and is per-tab so a verbose
    /// transcript on one tab doesn't bleed into another.
    pub transcript_verbose: bool,
    /// T5-Ssh-D: when present, this tab is in SSH mode and renders the
    /// vt100 virtual screen instead of the chat log. All chat-related
    /// fields above are unused when `ssh.is_some()`.
    pub ssh: Option<crate::tui::ssh_tab::SshTabState>,
    /// Per-tab inference runtime ownership marker (bug 3).
    ///
    /// Each tab owns its own runtime so multiple tabs can run independent
    /// turns concurrently (bug 3). The runtime itself lives in
    /// `LiveCli.tab_runtimes[i]` (parallel to `AnvilTui.tabs[i]`); this
    /// flag tracks whether that slot is populated. For the bootstrap tab
    /// this is set to `true` by `LiveCli::new`; for subsequent tabs it is
    /// set by the `/tab new` handler when the runtime is installed.
    pub has_runtime: bool,
    /// v2.2.14 TUI-1: shared cancel flag wired into this tab's
    /// `ConversationRuntime`. The TUI flips this from its Ctrl+C handler
    /// while a turn is streaming; the runtime polls between SSE frames and
    /// bails with `RuntimeError::cancelled()`.
    pub cancel_token: Arc<AtomicBool>,
}

impl Tab {
    pub fn new(id: usize, name: impl Into<String>, model: impl Into<String>, session_id: impl Into<String>) -> Self {
        Self {
            id,
            name: name.into(),
            log: Vec::new(),
            pending_text: String::new(),
            scroll: 0,
            input: String::new(),
            cursor: 0,
            history: Vec::new(),
            history_idx: None,
            history_backup: None,
            think_label: String::new(),
            think_start: None,
            think_frame: 0,
            input_tokens: 0,
            output_tokens: 0,
            session_start: Instant::now(),
            model: model.into(),
            session_id: session_id.into(),
            completion: CompletionPopup::default(),
            has_unread: false,
            branches: Vec::new(),
            active_branch: 0,
            last_snapshot: None,
            log_len_at_snapshot: None,
            scrollback: ScrollbackBuffer::new(),
            scrollback_pending_lines: 0,
            scrollback_state: ScrollbackState::live(),
            transcript_verbose: false,
            ssh: None,
            has_runtime: false,
            cancel_token: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Create a new conversation branch from the current log state.
    ///
    /// Branches share their snapshot via `Arc<Vec<LogEntry>>`. We track the
    /// most recent snapshot and the live log's len at the time of capture;
    /// if the user `/fork`s again without having pushed to log in between,
    /// the branch reuses the existing Arc — true O(1) refcount-only fork.
    /// If log has been mutated, we capture a fresh snapshot (one Vec clone).
    /// (T3-I — see #344/#411.)
    pub fn create_branch(&mut self, name: &str) -> usize {
        let snapshot: Arc<Vec<LogEntry>> =
            match (&self.last_snapshot, self.log_len_at_snapshot) {
                (Some(arc), Some(len)) if len == self.log.len() => Arc::clone(arc),
                _ => {
                    let fresh = Arc::new(self.log.clone());
                    self.last_snapshot = Some(Arc::clone(&fresh));
                    self.log_len_at_snapshot = Some(self.log.len());
                    fresh
                }
            };
        self.branches.push((name.to_string(), snapshot));
        self.branches.len() // 1-indexed for display
    }

    /// Switch to a branch by index (1-indexed). 0 = stay on current.
    ///
    /// Saving the current live log into the previously-active branch slot
    /// reuses the cached `last_snapshot` Arc when log hasn't grown since
    /// the last capture; otherwise pays one Vec clone. Restoring is one
    /// clone-on-read since `log` must be a Vec.
    pub fn switch_branch(&mut self, idx: usize) -> bool {
        if idx == 0 || idx > self.branches.len() {
            return false;
        }
        // Save current log into the previously active branch slot.
        if self.active_branch > 0 && self.active_branch <= self.branches.len() {
            let slot_idx = self.active_branch - 1;
            let saved: Arc<Vec<LogEntry>> =
                match (&self.last_snapshot, self.log_len_at_snapshot) {
                    (Some(arc), Some(len)) if len == self.log.len() => Arc::clone(arc),
                    _ => {
                        let fresh = Arc::new(self.log.clone());
                        self.last_snapshot = Some(Arc::clone(&fresh));
                        self.log_len_at_snapshot = Some(self.log.len());
                        fresh
                    }
                };
            self.branches[slot_idx].1 = saved;
        }
        // Restore the target branch — clone-on-read since `log` must be mut.
        let target = Arc::clone(&self.branches[idx - 1].1);
        self.log = (*target).clone();
        self.last_snapshot = Some(target);
        self.log_len_at_snapshot = Some(self.log.len());
        self.active_branch = idx;
        true
    }

    /// List all branches with names.
    #[allow(dead_code)]
    pub fn list_branches(&self) -> Vec<(usize, &str, bool)> {
        self.branches
            .iter()
            .enumerate()
            .map(|(i, (name, _))| (i + 1, name.as_str(), i + 1 == self.active_branch))
            .collect()
    }
}

// ─── Per-tab runtime flag tests ───────────────────────────────────────────────

#[cfg(test)]
mod tab_runtime_tests {
    use super::*;

    /// A freshly constructed `Tab` must have `has_runtime = false`; the flag is
    /// set externally by `run_repl_tui` after `push_tab_runtime` succeeds.
    #[test]
    fn tab_holds_optional_runtime() {
        let tab = Tab::new(1, "test".to_string(), "model".to_string(), "sess".to_string());
        assert!(!tab.has_runtime, "new Tab should start with has_runtime = false");
    }

    /// `TuiSender::with_tab_id` rebinds the tab stamp and delivers events to
    /// the original channel — verifying the sender stamping used in
    /// `push_tab_runtime`.
    #[test]
    fn tab_install_runtime_stamps_correct_sender() {
        use std::sync::mpsc;
        let (tx, rx) = mpsc::sync_channel::<TaggedTuiEvent>(4);
        let prototype = TuiSender::new(tx, 1);
        // Simulate what push_tab_runtime does: stamp the sender with the new tab's id.
        let tab_id = 42usize;
        let stamped = prototype.with_tab_id(tab_id);
        stamped.send(TuiEvent::TurnDone);
        let tagged = rx.try_recv().expect("expected event on channel");
        assert_eq!(tagged.tab_id, tab_id, "sender must be stamped with the new tab's id");
    }
}

// ─── Sender tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod sender_tests {
    use super::*;
    use std::sync::mpsc;

    /// `TuiSender::send` must stamp the sender's `tab_id` onto every event.
    #[test]
    fn tui_sender_stamps_tab_id() {
        let (tx, rx) = mpsc::sync_channel::<TaggedTuiEvent>(4);
        let sender = TuiSender::new(tx, 7);
        sender.send(TuiEvent::TurnDone);
        let tagged = rx.try_recv().expect("expected a message");
        assert_eq!(tagged.tab_id, 7);
        assert!(matches!(tagged.event, TuiEvent::TurnDone));
    }

    /// `TuiSender::with_tab_id` must rebind the stamp without changing the
    /// underlying channel.
    #[test]
    fn tui_sender_with_tab_id_rebinds() {
        let (tx, rx) = mpsc::sync_channel::<TaggedTuiEvent>(4);
        let sender = TuiSender::new(tx, 1);
        let rebound = sender.with_tab_id(99);
        rebound.send(TuiEvent::System("hello".to_string()));
        let tagged = rx.try_recv().expect("expected a message");
        assert_eq!(tagged.tab_id, 99);
        assert!(matches!(tagged.event, TuiEvent::System(_)));
    }
}

#[cfg(test)]
mod fork_tests {
    use super::*;

    fn fresh_tab() -> Tab {
        Tab::new(0, "test", "model", "session")
    }

    #[test]
    fn back_to_back_forks_share_snapshot_arc() {
        // T3-I: two /forks against an unchanged log should share the same Arc
        // (refcount-only snapshot reuse).
        let mut tab = fresh_tab();
        tab.log.push(LogEntry::User("hello".to_string()));
        tab.create_branch("a");
        tab.create_branch("b");
        let arc_a = Arc::clone(&tab.branches[0].1);
        let arc_b = Arc::clone(&tab.branches[1].1);
        assert!(
            Arc::ptr_eq(&arc_a, &arc_b),
            "back-to-back forks should reuse the same Arc snapshot"
        );
    }

    #[test]
    fn fork_after_log_push_takes_fresh_snapshot() {
        // After a log mutation, the next /fork must NOT reuse the prior Arc.
        let mut tab = fresh_tab();
        tab.log.push(LogEntry::User("hello".to_string()));
        tab.create_branch("a");
        tab.log.push(LogEntry::User("more".to_string()));
        tab.create_branch("b");
        assert!(
            !Arc::ptr_eq(&tab.branches[0].1, &tab.branches[1].1),
            "post-mutation fork should produce a divergent snapshot"
        );
        assert_eq!(tab.branches[0].1.len(), 1);
        assert_eq!(tab.branches[1].1.len(), 2);
    }

    #[test]
    fn switch_branch_restores_log_state() {
        let mut tab = fresh_tab();
        tab.log.push(LogEntry::User("alpha".to_string()));
        tab.create_branch("a"); // branch 1 captures [alpha]
        tab.log.push(LogEntry::User("beta".to_string())); // live now [alpha, beta]
        tab.create_branch("b"); // branch 2 captures [alpha, beta]
        // switch back to branch 1
        assert!(tab.switch_branch(1));
        assert_eq!(tab.log.len(), 1);
        // switch to branch 2
        assert!(tab.switch_branch(2));
        assert_eq!(tab.log.len(), 2);
    }

    #[test]
    fn switch_branch_rejects_invalid_index() {
        let mut tab = fresh_tab();
        tab.create_branch("only");
        assert!(!tab.switch_branch(0));
        assert!(!tab.switch_branch(99));
    }
}

// ─── Tool-call verbosity tests ────────────────────────────────────────────────

#[cfg(test)]
mod tool_call_verbosity_tests {
    use super::*;
    use runtime::Theme;

    fn default_theme() -> Theme {
        Theme::default_theme()
    }

    /// Helper: simulate the ToolCallActive handler logic against a Tab.log.
    fn apply_tool_call_active(log: &mut Vec<LogEntry>, name: &str, detail: &str, full_input: &str) {
        let mut found = false;
        for entry in log.iter_mut().rev() {
            if let LogEntry::ToolCall {
                name: n,
                detail: d,
                full_input: fi,
                done,
                ..
            } = entry
                && *n == name && !*done
            {
                *d = detail.to_string();
                *fi = Some(full_input.to_string());
                found = true;
                break;
            }
        }
        if !found {
            log.push(LogEntry::ToolCall {
                name: name.to_string(),
                detail: detail.to_string(),
                done: false,
                is_error: false,
                expanded: false,
                full_input: Some(full_input.to_string()),
                full_result: None,
            });
        }
    }

    /// Helper: simulate the ToolResult handler logic. Returns true if a card
    /// was matched (no System fallback needed), false otherwise.
    fn apply_tool_result(log: &mut Vec<LogEntry>, name: &str, summary: &str, is_error: bool) -> bool {
        let mut matched = false;
        for entry in log.iter_mut().rev() {
            if let LogEntry::ToolCall {
                name: n,
                done,
                is_error: err,
                full_result,
                ..
            } = entry
                && *n == name && !*done
            {
                *done = true;
                *err = is_error;
                *full_result = Some(summary.to_string());
                matched = true;
                break;
            }
        }
        matched
    }

    // ── Test 1: ToolCallActive without prior ToolCallStart pushes a card ───────

    #[test]
    fn tool_call_active_without_prior_start_pushes_card() {
        let mut log: Vec<LogEntry> = Vec::new();
        apply_tool_call_active(&mut log, "Glob", "**/*.rs\nin crates/", r#"{"pattern":"**/*.rs","path":"crates/"}"#);
        assert_eq!(log.len(), 1, "should push exactly one entry");
        match &log[0] {
            LogEntry::ToolCall { name, detail, done, full_input, .. } => {
                assert_eq!(name, "Glob");
                assert!(detail.contains("**/*.rs"), "detail should contain the pattern");
                assert!(!done, "should not be done yet");
                assert!(full_input.is_some(), "full_input should be stashed");
            }
            other => panic!("expected ToolCall, got {other:?}"),
        }
    }

    // ── Test 2: ToolResult updates card and does NOT add a duplicate System ───

    #[test]
    fn tool_result_updates_card_no_duplicate_system() {
        let mut log: Vec<LogEntry> = Vec::new();
        // Push a card via ToolCallActive (simulates the push-if-missing path).
        apply_tool_call_active(&mut log, "Bash", "cargo build", r#"{"command":"cargo build"}"#);
        assert_eq!(log.len(), 1);

        // Now receive the result.
        let matched = apply_tool_result(&mut log, "Bash", "exit 0", false);
        assert!(matched, "should have matched the existing card");

        // Log must still be exactly 1 entry — no extra System line.
        assert_eq!(log.len(), 1, "no System fallback should be pushed when card matched");

        // The card must be done with full_result set.
        match &log[0] {
            LogEntry::ToolCall { name, done, full_result, .. } => {
                assert_eq!(name, "Bash");
                assert!(done, "card should be marked done");
                assert_eq!(full_result.as_deref(), Some("exit 0"));
            }
            other => panic!("expected ToolCall, got {other:?}"),
        }
    }

    // ── Test 3: ToolResult without prior card falls back to System ─────────────

    #[test]
    fn tool_result_without_prior_card_falls_back_to_system() {
        let mut log: Vec<LogEntry> = Vec::new();
        // No prior ToolCallActive — result arrives out of order.
        let matched = apply_tool_result(&mut log, "Read", "5 lines", false);
        assert!(!matched, "no card to match, should return false");
        // Caller would push a System entry; verify it doesn't go into the log
        // from apply_tool_result itself (the caller pushes System, not the helper).
        assert_eq!(log.len(), 0, "apply_tool_result itself never pushes System");
        // The real code in mod.rs checks `!matched` and then pushes System.
        // Verify the flag is correct so the caller can act.
    }

    // ── Test 4: expanded toggle changes to_lines output ───────────────────────

    #[test]
    fn expanded_toggle_changes_to_lines_output() {
        let long_detail: String = (0..25)
            .map(|i| format!("line {i}: some detail content here"))
            .collect::<Vec<_>>()
            .join("\n");

        let mut entry = LogEntry::ToolCall {
            name: "Glob".to_string(),
            detail: long_detail.clone(),
            done: false,
            is_error: false,
            expanded: false,
            full_input: None,
            full_result: None,
        };

        let theme = default_theme();
        let unexpanded_lines = entry.to_lines(80, &theme);
        // 1 top + up to 12 detail + 1 bottom + 1 blank = ≤15 lines
        assert!(
            unexpanded_lines.len() <= 15,
            "unexpanded card should be ≤15 lines, got {}",
            unexpanded_lines.len()
        );

        // Now expand.
        if let LogEntry::ToolCall { expanded, .. } = &mut entry {
            *expanded = true;
        }
        let expanded_lines = entry.to_lines(80, &theme);
        // 25 detail lines + top + bottom + blank = 28 minimum
        assert!(
            expanded_lines.len() > unexpanded_lines.len(),
            "expanded card should have more lines than unexpanded ({} vs {})",
            expanded_lines.len(),
            unexpanded_lines.len()
        );
    }

    // ── Test 5: Glob/Grep/Read/Write detail formats ───────────────────────────

    #[test]
    fn glob_detail_renders_correctly() {
        use crate::format_tool::tool_call_detail;
        let input = r#"{"pattern":"**/*.rs","path":"crates/anvil-cli/src"}"#;
        let detail = tool_call_detail("Glob", input);
        assert!(detail.contains("**/*.rs"), "detail should contain pattern: {detail}");
        assert!(detail.contains("crates/anvil-cli/src"), "detail should contain path: {detail}");
    }

    #[test]
    fn grep_detail_renders_correctly() {
        use crate::format_tool::tool_call_detail;
        let input = r#"{"pattern":"ToolCallActive","path":"crates/"}"#;
        let detail = tool_call_detail("Grep", input);
        assert!(detail.contains("ToolCallActive"), "detail should contain pattern: {detail}");
    }

    #[test]
    fn read_detail_renders_correctly() {
        use crate::format_tool::tool_call_detail;
        let input = r#"{"file_path":"/some/file.rs"}"#;
        let detail = tool_call_detail("Read", input);
        assert!(detail.contains("file.rs"), "detail should reference the path: {detail}");
    }

    #[test]
    fn write_detail_renders_correctly() {
        use crate::format_tool::tool_call_detail;
        let input = r#"{"file_path":"/out.txt","content":"line1\nline2\nline3"}"#;
        let detail = tool_call_detail("Write", input);
        assert!(detail.contains("out.txt"), "detail should contain path: {detail}");
        assert!(detail.contains("line"), "detail should reference content: {detail}");
    }
}

// ─── Completion popup ─────────────────────────────────────────────────────────

/// Tracks the state of the slash-command completion popup.
#[derive(Debug, Default)]
pub(crate) struct CompletionPopup {
    pub visible: bool,
    pub matches: Vec<CompletionItem>,
    pub selected: usize,
    /// Top-of-viewport offset for the popup list.  Updated by Up/Down so the
    /// `selected` row always stays visible (FEAT-36 — long completion lists
    /// like `/vault store ` (21 entries) used to clip rows beyond the
    /// 12-row cap).
    pub view_offset: usize,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct CompletionItem {
    /// The text to insert when this item is accepted (empty for header items).
    pub insert: String,
    /// Short description shown to the right of the insert text.
    pub hint: String,
    /// When `true` this is a non-selectable category header row.
    /// The popup renderer should skip these during selection navigation.
    pub is_header: bool,
    /// When `true` the insert text is a free-text placeholder (`<hint>`)
    /// that should be rendered with DIM styling instead of inserted verbatim.
    pub is_free_text: bool,
}

// ─── Spinner frames ───────────────────────────────────────────────────────────

pub(super) const THINK_FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::thread;

    /// Spin up two concurrent workers, each sending TextDelta events and a
    /// TurnDone via the shared channel.  Verify:
    ///   - No cross-contamination: every event carries the tab_id of the
    ///     worker that produced it.
    ///   - TurnDone arrives for both tab_ids.
    ///   - Within each tab's event sequence, TextDeltas precede TurnDone.
    #[test]
    fn concurrent_workers_emit_correct_tab_ids() {
        let (tx, rx) = mpsc::sync_channel::<TaggedTuiEvent>(64);

        let sender1 = TuiSender::new(tx.clone(), 1);
        let sender2 = TuiSender::new(tx.clone(), 2);
        drop(tx);

        let h1 = thread::spawn(move || {
            for chunk in &["hello", " world"] {
                sender1.send(TuiEvent::TextDelta(chunk.to_string()));
            }
            sender1.send(TuiEvent::TurnDone);
        });
        let h2 = thread::spawn(move || {
            for chunk in &["foo", " bar", " baz"] {
                sender2.send(TuiEvent::TextDelta(chunk.to_string()));
            }
            sender2.send(TuiEvent::TurnDone);
        });

        h1.join().expect("worker 1 panicked");
        h2.join().expect("worker 2 panicked");

        let events: Vec<TaggedTuiEvent> =
            std::iter::from_fn(|| rx.try_recv().ok()).collect();

        // No text from tab 2 should appear under tab 1, and vice versa.
        let tab1_texts: Vec<String> = events.iter()
            .filter(|e| e.tab_id == 1)
            .filter_map(|e| if let TuiEvent::TextDelta(ref s) = e.event { Some(s.clone()) } else { None })
            .collect();
        let tab2_texts: Vec<String> = events.iter()
            .filter(|e| e.tab_id == 2)
            .filter_map(|e| if let TuiEvent::TextDelta(ref s) = e.event { Some(s.clone()) } else { None })
            .collect();

        for t in &tab1_texts {
            assert!(["hello", " world"].contains(&t.as_str()),
                "unexpected text in tab 1: {t:?}");
        }
        for t in &tab2_texts {
            assert!(["foo", " bar", " baz"].contains(&t.as_str()),
                "unexpected text in tab 2: {t:?}");
        }
        assert_eq!(tab1_texts.len(), 2);
        assert_eq!(tab2_texts.len(), 3);

        // TurnDone must arrive for both tabs.
        let done_tabs: Vec<usize> = events.iter()
            .filter(|e| matches!(e.event, TuiEvent::TurnDone))
            .map(|e| e.tab_id)
            .collect();
        assert!(done_tabs.contains(&1), "no TurnDone for tab 1: {done_tabs:?}");
        assert!(done_tabs.contains(&2), "no TurnDone for tab 2: {done_tabs:?}");

        // Per-tab ordering: last TextDelta must precede TurnDone.
        let tab1_ev: Vec<_> = events.iter().filter(|e| e.tab_id == 1).collect();
        let t1_done_pos = tab1_ev.iter().rposition(|e| matches!(e.event, TuiEvent::TurnDone)).unwrap();
        let t1_text_pos = tab1_ev.iter().rposition(|e| matches!(e.event, TuiEvent::TextDelta(_))).unwrap();
        assert!(t1_text_pos < t1_done_pos, "TurnDone before last TextDelta in tab 1");

        let tab2_ev: Vec<_> = events.iter().filter(|e| e.tab_id == 2).collect();
        let t2_done_pos = tab2_ev.iter().rposition(|e| matches!(e.event, TuiEvent::TurnDone)).unwrap();
        let t2_text_pos = tab2_ev.iter().rposition(|e| matches!(e.event, TuiEvent::TextDelta(_))).unwrap();
        assert!(t2_text_pos < t2_done_pos, "TurnDone before last TextDelta in tab 2");
    }

    // ─── Bug-3 Commit 4 tests ─────────────────────────────────────────────────

    /// `PermissionRequired` events from two different tab workers must land in
    /// the channel with the correct `tab_id` stamps and must NOT cross-route.
    ///
    /// This mirrors what `apply_tagged_event` does: it reads `tagged.tab_id`
    /// and inserts into `pending_permissions[tab_id]`.  We test only the
    /// channel/tagging layer here (no live TUI needed).
    #[test]
    fn permission_request_routes_to_correct_tab() {
        use std::collections::HashMap;
        use std::sync::mpsc;

        let (tx, rx) = mpsc::sync_channel::<TaggedTuiEvent>(16);
        let sender_tab1 = TuiSender::new(tx.clone(), 1);
        let sender_tab2 = TuiSender::new(tx.clone(), 2);
        drop(tx);

        // Simulate two workers emitting PermissionRequired with distinct tab IDs.
        let (reply_tx1, _reply_rx1) = mpsc::sync_channel::<PermissionReply>(1);
        let (reply_tx2, _reply_rx2) = mpsc::sync_channel::<PermissionReply>(1);

        sender_tab1.send(TuiEvent::PermissionRequired {
            tool_name: "bash".to_string(),
            required_mode: "bypassPermissions".to_string(),
            current_mode: "default".to_string(),
            input_summary: "rm -rf /tmp/test".to_string(),
            response_tx: reply_tx1,
        });
        sender_tab2.send(TuiEvent::PermissionRequired {
            tool_name: "write_file".to_string(),
            required_mode: "default".to_string(),
            current_mode: "default".to_string(),
            input_summary: "path: /etc/hosts".to_string(),
            response_tx: reply_tx2,
        });

        // Simulate what apply_tagged_event does: insert into a map keyed by tab_id.
        let mut pending: HashMap<usize, (String, String)> = HashMap::new();
        while let Ok(tagged) = rx.try_recv() {
            if let TuiEvent::PermissionRequired { tool_name, current_mode, .. } = tagged.event {
                pending.insert(tagged.tab_id, (tool_name, current_mode));
            }
        }

        assert_eq!(pending.len(), 2, "expected 2 pending entries, one per tab");
        let (name1, _) = pending.get(&1).expect("no entry for tab 1");
        assert_eq!(name1, "bash", "tab 1 should have bash request");
        let (name2, _) = pending.get(&2).expect("no entry for tab 2");
        assert_eq!(name2, "write_file", "tab 2 should have write_file request");
    }

    /// A worker that sends `PermissionRequired` and blocks on the reply channel
    /// must unblock when the TUI side sends `Allow` through `response_tx`.
    #[test]
    fn permission_reply_unblocks_worker() {
        use std::sync::mpsc;
        use std::thread;

        let (tx, rx) = mpsc::sync_channel::<TaggedTuiEvent>(4);
        let sender = TuiSender::new(tx, 5);

        // Spawn a worker that mimics CliPermissionPrompter's TUI path.
        let worker = thread::spawn(move || {
            let (reply_tx, reply_rx) = mpsc::sync_channel::<PermissionReply>(1);
            sender.send(TuiEvent::PermissionRequired {
                tool_name: "test_tool".to_string(),
                required_mode: "default".to_string(),
                current_mode: "default".to_string(),
                input_summary: "some input".to_string(),
                response_tx: reply_tx,
            });
            // Block until a reply arrives — this is what the worker does.
            reply_rx.recv().expect("reply channel closed unexpectedly")
        });

        // "TUI side": receive the event, extract response_tx, send Allow.
        let tagged = rx.recv().expect("expected a tagged event from the worker");
        assert_eq!(tagged.tab_id, 5, "event must carry the sender's tab_id");
        if let TuiEvent::PermissionRequired { response_tx, .. } = tagged.event {
            response_tx.send(PermissionReply::Allow).expect("failed to send reply");
        } else {
            panic!("expected PermissionRequired event");
        }

        let decision = worker.join().expect("worker panicked");
        assert!(
            matches!(decision, PermissionReply::Allow),
            "worker should have received Allow reply"
        );
    }

    /// Verify that a try_recv drain loop (as used by pump_events_nonblocking)
    /// collects every event from two concurrent workers without blocking.
    #[test]
    fn drain_loop_collects_all_events() {
        let (tx, rx) = mpsc::sync_channel::<TaggedTuiEvent>(128);
        let s1 = TuiSender::new(tx.clone(), 10);
        let s2 = TuiSender::new(tx.clone(), 20);
        drop(tx);

        let h1 = thread::spawn(move || {
            for c in &["a", "b", "c", "d", "e"] { s1.send(TuiEvent::TextDelta(c.to_string())); }
            s1.send(TuiEvent::TurnDone);
        });
        let h2 = thread::spawn(move || {
            for c in &["x", "y", "z"] { s2.send(TuiEvent::TextDelta(c.to_string())); }
            s2.send(TuiEvent::TurnDone);
        });
        h1.join().unwrap();
        h2.join().unwrap();

        let count = std::iter::from_fn(|| rx.try_recv().ok()).count();
        // 5 TextDeltas + 1 TurnDone from worker 1 = 6
        // 3 TextDeltas + 1 TurnDone from worker 2 = 4
        assert_eq!(count, 10, "expected 10 events, got {count}");
    }
}
