/// UI state types: events, tab state, log entries, completion popup.
use std::sync::mpsc::SyncSender;
use std::time::Instant;

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use runtime::{Rgb, Theme};

use super::helpers::strip_ansi;

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
    /// Conversation branches — each is a snapshot of log at branch point.
    pub branches: Vec<(String, Vec<LogEntry>)>,
    /// Active branch index (0 = main, 1+ = branches).
    pub active_branch: usize,
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
        }
    }

    /// Create a new conversation branch from the current log state.
    pub fn create_branch(&mut self, name: &str) -> usize {
        self.branches.push((name.to_string(), self.log.clone()));
        self.branches.len() // 1-indexed for display
    }

    /// Switch to a branch by index (1-indexed). 0 = stay on current.
    pub fn switch_branch(&mut self, idx: usize) -> bool {
        if idx == 0 || idx > self.branches.len() {
            return false;
        }
        // Save current log to the previously active branch slot
        if self.active_branch > 0 && self.active_branch <= self.branches.len() {
            self.branches[self.active_branch - 1].1 = self.log.clone();
        }
        // Restore the target branch
        self.log = self.branches[idx - 1].1.clone();
        self.active_branch = idx;
        true
    }

    /// List all branches with names.
    pub fn list_branches(&self) -> Vec<(usize, &str, bool)> {
        self.branches
            .iter()
            .enumerate()
            .map(|(i, (name, _))| (i + 1, name.as_str(), i + 1 == self.active_branch))
            .collect()
    }
}

// ─── Completion popup ─────────────────────────────────────────────────────────

/// Tracks the state of the slash-command completion popup.
#[derive(Debug, Default)]
pub(crate) struct CompletionPopup {
    pub visible: bool,
    pub matches: Vec<CompletionItem>,
    pub selected: usize,
}

#[derive(Debug, Clone)]
pub(crate) struct CompletionItem {
    pub insert: String,
    pub hint: String,
}

// ─── Spinner frames ───────────────────────────────────────────────────────────

pub(super) const THINK_FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
