/// UI state types: events, tab state, log entries, completion popup.
use std::sync::Arc;
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
    ToolCallActive { name: String, detail: String },
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
}

/// A cloneable sender that model/tool code uses to push `TuiEvent`s.
#[derive(Debug, Clone)]
pub struct TuiSender(pub SyncSender<TuiEvent>);

impl TuiSender {
    /// Send an event, discarding errors silently (TUI may have been closed).
    pub fn send(&self, event: TuiEvent) {
        let _ = self.0.send(event);
    }
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
    pub(super) fn to_lines(&self, max_width: u16, theme: &Theme) -> Vec<Line<'static>> {
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
            } => {
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
                let bot = format!("╰{:─<width$}╯", "", width = width + 2);

                let mut lines = vec![Line::from(Span::styled(
                    top,
                    Style::default().fg(border_color),
                ))];

                let inner_width = width.saturating_sub(2);
                for dl in detail.lines().take(12) {
                    let truncated = if dl.chars().count() > inner_width {
                        format!("{}…", dl.chars().take(inner_width.saturating_sub(1)).collect::<String>())
                    } else {
                        dl.to_string()
                    };
                    lines.push(Line::from(vec![
                        Span::styled("│ ", Style::default().fg(border_color)),
                        Span::raw(truncated),
                    ]));
                }

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
    /// Current scrollback navigation state (None = live view).
    pub scrollback_state: ScrollbackState,
    /// T5-Ssh-D: when present, this tab is in SSH mode and renders the
    /// vt100 virtual screen instead of the chat log. All chat-related
    /// fields above are unused when `ssh.is_some()`.
    pub ssh: Option<crate::tui::ssh_tab::SshTabState>,
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
            scrollback_state: ScrollbackState::live(),
            ssh: None,
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
